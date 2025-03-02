use crate::notification_window_options;
use crate::notifications::collab_notification::CollabNotification;
use call::{ActiveCall, IncomingCall};
use futures::StreamExt;
use gpui::{App, WindowHandle, prelude::*};

use std::sync::Arc;
use ui::{Button, Label, prelude::*};
use util::ResultExt;
use workspace::GlobalAppState;

pub fn init(cx: &mut App) {
    let mut incoming_call = ActiveCall::global(cx).read(cx).incoming();
    cx.spawn(async move |cx| {
        let mut notification_windows: Vec<WindowHandle<IncomingCallNotification>> = Vec::new();
        while let Some(incoming_call) = incoming_call.next().await {
            for window in notification_windows.drain(..) {
                window
                    .update(cx, |_, window, _| {
                        window.remove_window();
                    })
                    .log_err();
            }

            let Some(incoming_call) = incoming_call else {
                return;
            };

            let unique_screens = cx.update(|cx| cx.displays()).unwrap();
            let window_size = gpui::Size {
                width: px(400.),
                height: px(72.),
            };

            for screen in unique_screens {
                if let Some(options) = cx
                    .update(|cx| notification_window_options(screen, window_size, cx))
                    .log_err()
                {
                    let window = cx
                        .open_window(options, |_, cx| {
                            cx.new(|_| IncomingCallNotification::new(incoming_call.clone()))
                        })
                        .unwrap();
                    notification_windows.push(window);
                }
            }
        }
    })
    .detach();
}

struct IncomingCallNotificationState {
    call: IncomingCall,
}

pub struct IncomingCallNotification {
    state: Arc<IncomingCallNotificationState>,
}
impl IncomingCallNotificationState {
    pub fn new(call: IncomingCall) -> Self {
        Self { call }
    }

    fn respond(&self, accept: bool, cx: &mut App) {
        let active_call = ActiveCall::global(cx);
        if accept {
            let join = active_call.update(cx, |active_call, cx| active_call.accept_incoming(cx));
            let caller_user_id = self.call.calling_user.id;
            let initial_project_id = self.call.initial_project.as_ref().map(|project| project.id);
            let cx: &mut App = cx;
            cx.spawn(async move |cx| {
                join.await?;
                let Some(project_id) = initial_project_id else {
                    return anyhow::Ok(());
                };

                cx.update(|cx| {
                    let app_state = cx.global::<GlobalAppState>().0.clone();
                    workspace::join_in_room_project(project_id, caller_user_id, app_state, cx)
                        .detach_and_log_err(cx);
                })
                .log_err();

                anyhow::Ok(())
            })
            .detach_and_log_err(cx);
        } else {
            active_call.update(cx, |active_call, cx| {
                active_call.decline_incoming(cx).log_err();
            });
        }
    }
}

impl IncomingCallNotification {
    pub fn new(call: IncomingCall) -> Self {
        Self {
            state: Arc::new(IncomingCallNotificationState::new(call)),
        }
    }
}

impl Render for IncomingCallNotification {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = theme::setup_ui_font(window, cx);

        div().size_full().font(ui_font).child(
            CollabNotification::new(
                self.state.call.calling_user.avatar_uri.clone(),
                Button::new("accept", "Accept").on_click({
                    let state = self.state.clone();
                    move |_, _, cx| state.respond(true, cx)
                }),
                Button::new("decline", "Decline").on_click({
                    let state = self.state.clone();
                    move |_, _, cx| state.respond(false, cx)
                }),
            )
            .child(v_flex().overflow_hidden().child(Label::new(format!(
                "{} is sharing a project in Zed",
                self.state.call.calling_user.github_login
            )))),
        )
    }
}
