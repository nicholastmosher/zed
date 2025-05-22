use std::sync::Arc;

use anyhow::Context as _;
use db::kvp::KEY_VALUE_STORE;
use gpui::{prelude::FluentBuilder as _, *};
use serde::{Deserialize, Serialize};
use util::ResultExt as _;
use willow_25::{AuthorisationToken25, NamespaceId25, PayloadDigest25, SubspaceId25};
use workspace::{
    dock::{DockPosition, PanelEvent},
    ui::{v_flex, ContextMenu, IconName, ListItem},
    AppState, Panel, Workspace,
};

mod p2p_panel_settings;

#[rustfmt::skip]
actions!(
    workspace,
    [
        ToggleFocus,
        CreateDocument,
        CreateIdentity,
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
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    people: Vec<String>,
    documents: Vec<String>,

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
    store: willow_store_simple_sled::StoreSimpleSled<
        1024,
        1024,
        1024,
        NamespaceId25,
        SubspaceId25,
        PayloadDigest25,
        AuthorisationToken25,
    >,
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
                context_menu: None,
                people: vec!["Person 1", "Person 2"]
                    .into_iter()
                    .map(ToString::to_string)
                    .collect(),
                documents: vec!["Document 1", "Document 2"]
                    .into_iter()
                    .map(ToString::to_string)
                    .collect(),

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
                store: {
                    let db = sled::open("my_db").unwrap();
                    let namespace = NamespaceId25::new_communal();
                    willow_store_simple_sled::StoreSimpleSled::new(&namespace, db).unwrap()
                },
            };

            // this.subscriptions.push(cx.subscribe(a, b));

            this
        })
    }

    fn render_people(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .flex_grow()
            .bg(rgba(0xffec3777))
            .py_2()
            .children(self.people.iter().enumerate().map(|(i, it)| {
                ListItem::new(SharedString::from(format!("people-{i}-{it}")))
                    .child(SharedString::from(it))
            }))
    }

    pub fn render_documents(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .on_action(cx.listener(Self::create_document))
            .on_action(cx.listener(Self::create_identity))
            .flex_grow()
            .bg(rgba(0x4ae43277))
            .py_2()
            .children(self.documents.iter().enumerate().map(|(i, it)| {
                ListItem::new(SharedString::from(format!("documents-{i}-{it}")))
                    .child(SharedString::from(it))
                    .on_secondary_mouse_down(cx.listener(
                        |this, event: &MouseDownEvent, window, cx| {
                            // Stop propagation to prevent the catch-all context menu for the project
                            // panel from being deployed.
                            cx.stop_propagation();
                            this.deploy_context_menu(event.position, window, cx);
                        },
                    ))
            }))
    }

    pub fn create_document(
        &mut self,
        action: &CreateDocument,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.documents
            .push(format!("Document {}", self.documents.len()));
        cx.notify();
    }

    pub fn create_identity(
        &mut self,
        action: &CreateIdentity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.people.push(format!("Identity {}", self.people.len()));
        cx.notify();
    }

    fn deploy_context_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let context_menu = ContextMenu::build(window, cx, |menu, _, _| {
            menu.context(self.focus_handle.clone()).map(|menu| {
                menu.action("Create Document", Box::new(CreateDocument))
                    .action("Create Identity", Box::new(CreateIdentity))
            })
        });

        window.focus(&context_menu.focus_handle(cx));
        let subscription = cx.subscribe(&context_menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu.take();
            cx.notify();
        });
        self.context_menu = Some((context_menu, position, subscription));

        cx.notify();
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

    fn set_size(&mut self, size: Option<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.width = size;
    }

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
        v_flex()
            .on_action(cx.listener(Self::create_document))
            .size_full()
            .child(self.render_people(window, cx))
            .child(div().border_1())
            .child(self.render_documents(window, cx))
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui::Corner::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
    }
}
