use std::sync::Arc;

use anyhow::Context as _;
use db::kvp::KEY_VALUE_STORE;
use gpui::*;
use serde::{Deserialize, Serialize};
use util::ResultExt as _;
use workspace::{
    dock::{DockPosition, PanelEvent},
    ui::{h_flex, IconName},
    AppState, Panel, Workspace,
};

mod p2p_panel_settings;

#[rustfmt::skip]
actions!(
    workspace,
    [
        ToggleFocus,
    ]
);

pub fn init(app_state: &Arc<AppState>, cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _cx: &mut Context<Workspace>| {
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<P2pPanel>(window, cx);
            });
        },
    )
    .detach();
}

const P2P_PANEL_KEY: &str = "P2pPanel";

#[derive(Serialize, Deserialize)]
struct SerializedP2pPanel {
    width: Option<Pixels>,
}

// pub struct ChatPanel {
pub struct P2pPanel {
    message_list: ListState,
    width: Option<Pixels>,
    active: bool,
    pending_serialization: Task<Option<()>>,
    subscriptions: Vec<gpui::Subscription>,
    is_scrolled_to_bottom: bool,
    focus_handle: FocusHandle,
    open_context_menu: Option<(u64, Subscription)>,
    highlighted_message: Option<(u64, Task<()>)>,
    last_acknowledged_message_id: Option<u64>,
}

impl P2pPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        let serialized_panel = cx
            .background_executor()
            .spawn(async move { KEY_VALUE_STORE.read_kvp(P2P_PANEL_KEY) })
            .await
            .context("loading p2p panel")
            .log_err()
            .flatten()
            .map(|panel| serde_json::from_str::<SerializedP2pPanel>(&panel))
            .transpose()
            .log_err()
            .flatten();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let panel = Self::new(workspace, window, cx);
            if let Some(serialized_panel) = serialized_panel {
                panel.update(cx, |panel, cx| {
                    panel.width = serialized_panel.width.map(|px| px.round());
                    cx.notify();
                });
            }
            panel
        })
    }

    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let user_store = workspace.app_state().user_store.clone();

        cx.new(|cx| {
            let entity = cx.entity().downgrade();

            // let message_list = ListState::new(
            //     0,
            //     gpui::ListAlignment::Bottom,
            //     px(1000.),
            //     move |ix, window, cx| {
            //         if let Some(entity) = entity.upgrade() {
            //             entity.update(cx, |this: &mut Self, cx| {
            //                 this.render_message(ix, window, cx).into_any_element()
            //             })
            //         } else {
            //             div().into_any()
            //         }
            //     },
            // );

            // message_list.set_scroll_handler(cx.listener(|this, event: &ListScrollEvent, _, cx| {
            //     if event.visible_range.start < MESSAGE_LOADING_THRESHOLD {
            //         this.load_more_messages(cx);
            //     }
            //     this.is_scrolled_to_bottom = !event.is_scrolled;
            // }));

            let mut this = Self {
                pending_serialization: Task::ready(None),
                subscriptions: Vec::new(),
                is_scrolled_to_bottom: true,
                active: false,
                width: None,
                focus_handle: cx.focus_handle(),
                open_context_menu: None,
                highlighted_message: None,
                last_acknowledged_message_id: None,
                message_list: ListState::new(
                    0,
                    ListAlignment::Top,
                    px(1000.),
                    move |ix, window, cx| div().into_any(),
                ),
            };

            // this.subscriptions.push(cx.subscribe(a, b));

            this
        })
    }
}

impl Focusable for P2pPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for P2pPanel {}

impl Panel for P2pPanel {
    fn persistent_name() -> &'static str {
        "P2p"
    }

    fn position(&self, window: &Window, cx: &App) -> DockPosition {
        DockPosition::Left
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, workspace::dock::DockPosition::Left)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
    }

    fn size(&self, window: &Window, cx: &App) -> Pixels {
        self.width.unwrap_or(px(300.))
    }

    fn set_size(&mut self, size: Option<Pixels>, window: &mut Window, cx: &mut Context<Self>) {}

    fn icon(&self, window: &Window, cx: &App) -> Option<workspace::ui::IconName> {
        Some(IconName::DatabaseZap)
    }

    fn icon_tooltip(&self, window: &Window, cx: &App) -> Option<&'static str> {
        Some("P2p")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        0
    }
}

impl Render for P2pPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        h_flex().size_full().child("Hello, world!")
    }
}
