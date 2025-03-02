// Disable command line from opening on release mode
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod reliability;
mod zed;

use agent::GlobalIsEval;
use anyhow::{Context as _, Result};
use clap::{Parser, command};
use cli::FORCE_CLI_MODE_ENV_VAR_NAME;
use client::{Client, GlobalClient, GlobalUserStore, ProxySettings, UserStore, parse_zed_link};
use collab_ui::channel_view::ChannelView;
use collections::HashMap;
use db::kvp::{GLOBAL_KEY_VALUE_STORE, KEY_VALUE_STORE};
use editor::Editor;
use extension_host::ExtensionStore;
use fs::{Fs, GlobalFs, GlobalGitBinaryPath, RealFs};
use futures::{StreamExt, channel::oneshot, future};
use git::GitHostingProviderRegistry;
use gpui::{
    App, AppContext as _, Application, AsyncApp, Global, Plugin, ReadGlobal, SemanticVersion,
};

use gpui_tokio::Tokio;
use http_client::read_proxy_from_env;
use language::{GlobalLanguageRegistry, LanguageRegistry};
use prompt_store::{GlobalPromptBuilder, PromptBuilder};
use reqwest_client::ReqwestClient;

use assets::Assets;
use node_runtime::{
    GlobalNodeOptionsRx, GlobalNodeOptionsTx, GlobalNodeRuntime, NodeBinaryOptions, NodeRuntime,
};
use parking_lot::Mutex;
use project::project_settings::ProjectSettings;
use recent_projects::{SshSettings, open_ssh_project};
use release_channel::{AppCommitSha, AppVersion, ReleaseChannel, ReleaseChannelPlugin};
use session::{AppSession, Session};
use settings::{Settings, SettingsStore, watch_config_file};
use std::{
    env,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process,
    sync::Arc,
};
use theme::{
    ActiveTheme, IconThemeNotFoundError, SystemAppearance, ThemeNotFoundError, ThemePlugin,
    ThemeRegistry, ThemeSettings,
};
use url::Url;
use util::{ConnectionResult, ResultExt, TryFutureExt, maybe};
use uuid::Uuid;
use welcome::{BaseKeymap, FIRST_OPEN, show_welcome_view};
use workspace::{
    AppState, GlobalWorkspaceStore, SerializedWorkspaceLocation, WorkspaceSettings, WorkspaceStore,
};
use zed::{
    KeymapFileChangesPlugin, OpenListenerRx, OpenListenerTx, OpenRequest, SettingsFilePlugin,
    app_menus, build_window_options, derive_paths_with_position, handle_cli_connection,
    handle_settings_changed, initialize_workspace, inline_completion_registry, open_listener,
    open_paths_with_positions,
};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn files_not_created_on_launch(errors: HashMap<io::ErrorKind, Vec<&Path>>) {
    let message = "Zed failed to launch";
    let error_details = errors
        .into_iter()
        .flat_map(|(kind, paths)| {
            #[allow(unused_mut)] // for non-unix platforms
            let mut error_kind_details = match paths.len() {
                0 => return None,
                1 => format!(
                    "{kind} when creating directory {:?}",
                    paths.first().expect("match arm checks for a single entry")
                ),
                _many => format!("{kind} when creating directories {paths:?}"),
            };

            #[cfg(unix)]
            {
                match kind {
                    io::ErrorKind::PermissionDenied => {
                        error_kind_details.push_str("\n\nConsider using chown and chmod tools for altering the directories permissions if your user has corresponding rights.\
                            \nFor example, `sudo chown $(whoami):staff ~/.config` and `chmod +uwrx ~/.config`");
                    }
                    _ => {}
                }
            }

            Some(error_kind_details)
        })
        .collect::<Vec<_>>().join("\n\n");

    eprintln!("{message}: {error_details}");
    Application::new()
        .add_plugins(FailToOpenWindowPlugin::new(message, error_details))
        .run();
}

struct FailToOpenWindowPlugin {
    message: String,
    error_details: String,
}

impl FailToOpenWindowPlugin {
    pub fn new(message: impl Into<String>, error_details: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error_details: error_details.into(),
        }
    }
}

impl Plugin for FailToOpenWindowPlugin {
    fn build(&self, cx: &mut App) {
        let message = &self.message;
        let error_details = &self.error_details;

        if let Ok(window) = cx.open_window(gpui::WindowOptions::default(), |_, cx| {
            cx.new(|_| gpui::Empty)
        }) {
            window
                .update(cx, |_, window, cx| {
                    let response = window.prompt(
                        gpui::PromptLevel::Critical,
                        message,
                        Some(error_details),
                        &["Exit"],
                        cx,
                    );

                    cx.spawn_in(window, async move |_, cx| {
                        response.await?;
                        cx.update(|_, cx| cx.quit())
                    })
                    .detach_and_log_err(cx);
                })
                .log_err();
        } else {
            fail_to_open_window(anyhow::anyhow!("{message}: {error_details}"), cx)
        }
    }
}

fn fail_to_open_window_async(e: anyhow::Error, cx: &mut AsyncApp) {
    cx.update(|cx| fail_to_open_window(e, cx)).log_err();
}

fn fail_to_open_window(e: anyhow::Error, _cx: &mut App) {
    eprintln!(
        "Zed failed to open a window: {e:?}. See https://zed.dev/docs/linux for troubleshooting steps."
    );
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        process::exit(1);
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        use ashpd::desktop::notification::{Notification, NotificationProxy, Priority};
        _cx.spawn(async move |_cx| {
            let Ok(proxy) = NotificationProxy::new().await else {
                process::exit(1);
            };

            let notification_id = "dev.zed.Oops";
            proxy
                .add_notification(
                    notification_id,
                    Notification::new("Zed failed to launch")
                        .body(Some(
                            format!(
                                "{e:?}. See https://zed.dev/docs/linux for troubleshooting steps."
                            )
                            .as_str(),
                        ))
                        .priority(Priority::High)
                        .icon(ashpd::desktop::Icon::with_names(&[
                            "dialog-question-symbolic",
                        ])),
                )
                .await
                .ok();

            process::exit(1);
        })
        .detach();
    }
}

fn main() {
    Application::new().add_plugins(init).run();
}

#[derive(Clone)]
pub struct SystemInfo {
    pub(crate) system_id: Option<IdType>,
    pub(crate) installation_id: Option<IdType>,
    pub session_id: String,
    pub session: Arc<Session>,
    pub app_version: SemanticVersion,
    pub app_commit_sha: Option<AppCommitSha>,
}
impl Global for SystemInfo {}

struct GlobalArgs(pub Arc<Args>);
impl Global for GlobalArgs {}

pub struct GlobalShellEnvLoadedRx(Option<oneshot::Receiver<()>>);
impl Global for GlobalShellEnvLoadedRx {}

fn init(cx: &mut App) {
    // Check if there is a pending installer
    // If there is, run the installer and exit
    // And we don't want to run the installer if we are not the first instance
    #[cfg(target_os = "windows")]
    let is_first_instance = crate::zed::windows_only_instance::is_first_instance();
    #[cfg(target_os = "windows")]
    if is_first_instance && auto_update::check_pending_installation() {
        return;
    }
    cx.with_assets(Assets);

    let args = Arc::new(Args::parse());
    cx.set_global(GlobalArgs(args.clone()));

    if let Some(socket) = &args.askpass {
        askpass::main(socket);
        return;
    }

    // Set custom data directory.
    if let Some(dir) = &cx.global::<GlobalArgs>().0.user_data_dir {
        paths::set_custom_data_dir(dir);
    }

    #[cfg(all(not(debug_assertions), target_os = "windows"))]
    unsafe {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

        if args.foreground {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }

    cx.add_plugins(menu::init);
    cx.add_plugins(zed_actions::init);

    {
        fn init_file_errors(_: &mut App) {
            let file_errors = init_paths();
            if !file_errors.is_empty() {
                files_not_created_on_launch(file_errors);
                return;
            }
        }
        cx.add_plugins(init_file_errors);
    }

    {
        fn init_log(_: &mut App) {
            zlog::init();
            if stdout_is_a_pty() {
                zlog::init_output_stdout();
            } else {
                let result = zlog::init_output_file(paths::log_file(), Some(paths::old_log_file()));
                if let Err(err) = result {
                    eprintln!("Could not open log file: {}... Defaulting to stdout", err);
                    zlog::init_output_stdout();
                };
            }
        }
        cx.add_plugins(init_log);
    }

    log::info!("========== starting zed ==========");

    {
        let system_id = cx.background_executor().block(system_id()).ok();
        let installation_id = cx.background_executor().block(installation_id()).ok();
        let session_id = Uuid::new_v4().to_string();
        let session = Arc::new(cx.background_executor().block(Session::new()));
        let app_version = AppVersion::load(env!("CARGO_PKG_VERSION"));
        let app_commit_sha = option_env!("ZED_COMMIT_SHA")
            .map(|commit_sha| AppCommitSha::new(commit_sha.to_string()));

        let system_info = SystemInfo {
            system_id,
            installation_id,
            session_id,
            session,
            app_version,
            app_commit_sha,
        };
        cx.set_global(system_info);
    };

    let app_version = AppVersion::load(env!("CARGO_PKG_VERSION"));
    let app_commit_sha =
        option_env!("ZED_COMMIT_SHA").map(|commit_sha| AppCommitSha::new(commit_sha.to_string()));

    if let Some(app_commit_sha) = &app_commit_sha {
        AppCommitSha::set_global(app_commit_sha.clone(), cx);
    }

    cx.add_plugins(reliability::init_panic_hook);

    if cx.global::<GlobalArgs>().0.system_specs {
        let system_specs = feedback::system_specs::SystemSpecs::new_stateless(
            app_version,
            app_commit_sha.clone(),
            *release_channel::RELEASE_CHANNEL,
        );
        println!("Zed System Specs (from CLI):\n{}", system_specs);
        return;
    }

    log::info!("========== starting zed ==========");

    cx.add_plugins(open_listener::init);

    let failed_single_instance_check =
        if *db::ZED_STATELESS || *release_channel::RELEASE_CHANNEL == ReleaseChannel::Dev {
            false
        } else {
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            {
                crate::zed::listen_for_cli_connections(cx).is_err()
            }

            #[cfg(target_os = "windows")]
            {
                !crate::zed::windows_only_instance::handle_single_instance(
                    // open_listener.clone(),
                    cx.global::<OpenListenerTx>().clone(),
                    &args,
                    is_first_instance,
                )
            }

            #[cfg(target_os = "macos")]
            {
                use zed::mac_only_instance::*;
                ensure_only_instance() != IsOnlyInstance::Yes
            }
        };
    if failed_single_instance_check {
        println!("zed is already running");
        return;
    }

    let git_binary_path =
        if cfg!(target_os = "macos") && option_env!("ZED_BUNDLE").as_deref() == Some("true") {
            cx.path_for_auxiliary_executable("git")
                .context("could not find git binary path")
                .log_err()
        } else {
            None
        };
    cx.set_global(GlobalGitBinaryPath(git_binary_path.clone()));
    log::info!("Using git binary path: {:?}", git_binary_path);

    let fs = Arc::new(RealFs::new(
        git_binary_path,
        cx.background_executor().clone(),
    ));
    cx.set_global(GlobalFs(fs.clone()));

    let user_settings_file_rx = watch_config_file(
        &cx.background_executor(),
        fs.clone(),
        paths::settings_file().clone(),
    );
    let global_settings_file_rx = watch_config_file(
        cx.background_executor(),
        fs.clone(),
        paths::global_settings_file().clone(),
    );
    let user_keymap_file_rx = watch_config_file(
        &cx.background_executor(),
        fs.clone(),
        paths::keymap_file().clone(),
    );

    {
        let (shell_env_loaded_tx, shell_env_loaded_rx) = oneshot::channel();
        if !stdout_is_a_pty() {
            cx.background_executor()
                .spawn(async {
                    #[cfg(unix)]
                    util::load_login_shell_environment().log_err();
                    shell_env_loaded_tx.send(()).ok();
                })
                .detach()
        } else {
            drop(shell_env_loaded_tx)
        }
        cx.set_global(GlobalShellEnvLoadedRx(Some(shell_env_loaded_rx)));
    };

    cx.add_plugins(|cx: &mut App| {
        cx.on_open_urls({
            let open_listener = cx.global::<OpenListenerTx>().clone();
            move |urls| open_listener.open_urls(urls)
        });
    });
    cx.on_reopen(move |cx| {
        if let Some(app_state) = AppState::try_global(cx) {
            cx.spawn({
                let app_state = app_state.clone();
                async move |mut cx| {
                    if let Err(e) = restore_or_create_workspace(app_state, &mut cx).await {
                        fail_to_open_window_async(e, &mut cx)
                    }
                }
            })
            .detach();
        }
    });

    // app.run(move |cx| { ...

    cx.add_plugins(ReleaseChannelPlugin::new(app_version, app_commit_sha));
    cx.add_plugins(gpui_tokio::init);
    cx.add_plugins(settings::init);
    cx.add_plugins(zlog_settings::init);
    cx.add_plugins(SettingsFilePlugin::new(
        user_settings_file_rx,
        global_settings_file_rx,
        handle_settings_changed,
    ));
    cx.add_plugins(KeymapFileChangesPlugin::new(user_keymap_file_rx));
    cx.add_plugins(client::init_settings);

    {
        fn init_http_client(cx: &mut App) {
            let user_agent = format!(
                "Zed/{} ({}; {})",
                AppVersion::global(cx),
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            let proxy_str = ProxySettings::get_global(cx).proxy.to_owned();
            let proxy_url = proxy_str
                .as_ref()
                .and_then(|input| {
                    input
                        .parse::<Url>()
                        .inspect_err(|e| log::error!("Error parsing proxy settings: {}", e))
                        .ok()
                })
                .or_else(read_proxy_from_env);
            let http = {
                let _guard = Tokio::handle(cx).enter();

                ReqwestClient::proxy_and_user_agent(proxy_url, &user_agent)
                    .expect("could not start HTTP client")
            };
            cx.set_http_client(Arc::new(http));
        }
        cx.add_plugins(init_http_client);
    }

    cx.add_plugins(fs::init);

    {
        let git_hosting_provider_registry = Arc::new(GitHostingProviderRegistry::new());
        GitHostingProviderRegistry::set_global(git_hosting_provider_registry, cx);
        cx.add_plugins(git_hosting_providers::init);
    }

    cx.add_plugins(extension::init);

    cx.add_plugins(|cx: &mut App| {
        let client = Client::production(cx);
        cx.set_http_client(client.http_client());
        cx.set_global(GlobalClient(client));
    });

    cx.add_plugins(|cx: &mut App| {
        let mut languages = LanguageRegistry::new(cx.background_executor().clone());
        languages.set_language_server_download_dir(paths::languages_dir().clone());
        cx.set_global(GlobalLanguageRegistry(Arc::new(languages)));
    });

    cx.add_plugins(|cx: &mut App| {
        let (tx, rx) = async_watch::channel(None);
        cx.set_global(GlobalNodeOptionsTx(tx));
        cx.set_global(GlobalNodeOptionsRx(rx));

        cx.observe_global::<SettingsStore>(move |cx| {
            let settings = &ProjectSettings::get_global(cx).node;
            let options = NodeBinaryOptions {
                allow_path_lookup: !settings.ignore_system_version,
                // TODO: Expose this setting
                allow_binary_download: true,
                use_paths: settings.path.as_ref().map(|node_path| {
                    let node_path = PathBuf::from(shellexpand::tilde(node_path).as_ref());
                    let npm_path = settings
                        .npm_path
                        .as_ref()
                        .map(|path| PathBuf::from(shellexpand::tilde(&path).as_ref()));
                    (
                        node_path.clone(),
                        npm_path.unwrap_or_else(|| {
                            let base_path = PathBuf::new();
                            node_path.parent().unwrap_or(&base_path).join("npm")
                        }),
                    )
                }),
            };
            cx.global::<GlobalNodeOptionsTx>()
                .0
                .send(Some(options))
                .log_err();
        })
        .detach();
    });

    cx.add_plugins(move |cx: &mut App| {
        let client = cx.global::<GlobalClient>().0.clone();
        let rx = cx.global::<GlobalNodeOptionsRx>().0.clone();
        let shell_env_loaded_rx = cx.global_mut::<GlobalShellEnvLoadedRx>().0.take();
        let node_runtime = NodeRuntime::new(client.http_client(), shell_env_loaded_rx, rx);
        cx.set_global(GlobalNodeRuntime(node_runtime));
    });

    cx.add_plugins(debug_adapter_extension::init);
    cx.add_plugins(language::init);
    cx.add_plugins(language_extension::init);
    cx.add_plugins(languages::init);

    cx.add_plugins(move |cx: &mut App| {
        let user_store = cx.new(|cx| UserStore::new(cx));
        cx.set_global(GlobalUserStore(user_store));
        let workspace_store = cx.new(|cx| WorkspaceStore::new(cx));
        cx.set_global(GlobalWorkspaceStore(workspace_store));
    });

    cx.add_plugins(zed::init);
    cx.add_plugins(project::Project::init);
    cx.add_plugins(debugger_ui::init);
    cx.add_plugins(debugger_tools::init);
    cx.add_plugins(client::init);

    cx.add_plugins(move |cx: &mut App| {
        let system_info = cx.global::<SystemInfo>();
        let system_id = system_info.system_id.clone();
        let installation_id = system_info.installation_id.clone();
        let session_id = system_info.session_id.clone();
        let client = cx.global::<GlobalClient>().0.clone();
        let telemetry = client.telemetry();
        telemetry.start(
            system_id.as_ref().map(|id| id.to_string()),
            installation_id.as_ref().map(|id| id.to_string()),
            session_id.clone(),
            cx,
        );
    });

    cx.add_plugins(|cx: &mut App| {
        let system_info = cx.global::<SystemInfo>();
        let system_id = system_info.system_id.clone();
        let installation_id = system_info.installation_id.clone();

        // We should rename these in the future to `first app open`, `first app open for release channel`, and `app open`
        if let (Some(system_id), Some(installation_id)) = (&system_id, &installation_id) {
            match (&system_id, &installation_id) {
                (IdType::New(_), IdType::New(_)) => {
                    telemetry::event!("App First Opened");
                    telemetry::event!("App First Opened For Release Channel");
                }
                (IdType::Existing(_), IdType::New(_)) => {
                    telemetry::event!("App First Opened For Release Channel");
                }
                (_, IdType::Existing(_)) => {
                    telemetry::event!("App Opened");
                }
            }
        }
    });

    cx.add_plugins(move |cx: &mut App| {
        let session = cx.background_executor().block(Session::new());
        let app_session = cx.new(|cx| AppSession::new(session, cx));

        let app_state = Arc::new(AppState {
            languages: cx.global::<GlobalLanguageRegistry>().0.clone(),
            client: cx.global::<GlobalClient>().0.clone(),
            user_store: cx.global::<GlobalUserStore>().0.clone(),
            fs: cx.global::<GlobalFs>().0.clone(),
            build_window_options,
            workspace_store: cx.global::<GlobalWorkspaceStore>().0.clone(),
            node_runtime: cx.global::<GlobalNodeRuntime>().0.clone(),
            session: app_session,
        });
        AppState::set_global(app_state, cx);
    });

    cx.add_plugins(auto_update::init);
    cx.add_plugins(dap_adapters::init);
    cx.add_plugins(auto_update_ui::init);
    cx.add_plugins(reliability::init);

    cx.add_plugins(SystemAppearance::init);
    cx.add_plugins(ThemePlugin::new(theme::LoadThemes::All(Box::new(Assets))));
    cx.add_plugins(theme_extension::init);
    cx.add_plugins(command_palette::init);
    cx.add_plugins(copilot::init);
    cx.add_plugins(supermaven::init);
    cx.add_plugins(language_model::init);
    cx.add_plugins(language_models::init);
    cx.add_plugins(snippet_provider::init);
    cx.add_plugins(inline_completion_registry::init);

    cx.add_plugins(move |cx: &mut App| {
        let prompt_builder =
            PromptBuilder::load(AppState::global(cx).fs.clone(), stdout_is_a_pty(), cx);
        cx.set_global(GlobalPromptBuilder(prompt_builder));
    });

    cx.add_plugins((
        |cx: &mut App| cx.set_global(GlobalIsEval(false)),
        agent::init,
    ));
    cx.add_plugins(assistant_tools::init);
    cx.add_plugins(repl::init);
    cx.add_plugins(extension_host::init);
    cx.add_plugins(recent_projects::init);
    cx.add_plugins(load_embedded_fonts);

    cx.add_plugins(move |cx: &mut App| {
        AppState::global(cx).languages.set_theme(cx.theme().clone());
    });
    cx.add_plugins(editor::init);
    cx.add_plugins(image_viewer::init);
    cx.add_plugins(repl::notebook::init);
    cx.add_plugins(diagnostics::init);

    cx.add_plugins(move |cx: &mut App| {
        audio::init(Assets, cx);
    });
    cx.add_plugins(workspace::init);
    cx.add_plugins(ui_prompt::init);
    cx.add_plugins(go_to_line::init);
    cx.add_plugins(file_finder::init);
    cx.add_plugins(tab_switcher::init);
    cx.add_plugins(outline::init);
    cx.add_plugins(project_symbols::init);
    cx.add_plugins(project_panel::init);
    cx.add_plugins(outline_panel::init);
    cx.add_plugins(tasks_ui::init);
    cx.add_plugins(snippets_ui::init);
    cx.add_plugins(channel::init);
    cx.add_plugins(search::init);
    cx.add_plugins(vim::init);
    cx.add_plugins(terminal_view::init);
    cx.add_plugins(journal::init);
    cx.add_plugins(language_selector::init);
    cx.add_plugins(toolchain_selector::init);
    cx.add_plugins(theme_selector::init);
    cx.add_plugins(language_tools::init);
    cx.add_plugins(call::init);
    cx.add_plugins(notifications::init);
    cx.add_plugins(collab_ui::init);
    cx.add_plugins(git_ui::init);
    cx.add_plugins(feedback::init);
    cx.add_plugins(markdown_preview::init);
    cx.add_plugins(welcome::init);
    cx.add_plugins(settings_ui::init);
    cx.add_plugins(extensions_ui::init);
    cx.add_plugins(zeta::init);
    cx.add_plugins(inspector_ui::init);

    cx.add_plugins(move |cx: &mut App| {
        cx.observe_global::<SettingsStore>({
            let fs = fs.clone();
            let languages = AppState::global(cx).languages.clone();
            let http = AppState::global(cx).client.http_client();
            let client = AppState::global(cx).client.clone();
            move |cx| {
                for &mut window in cx.windows().iter_mut() {
                    let background_appearance = cx.theme().window_background_appearance();
                    window
                        .update(cx, |_, window, _| {
                            window.set_background_appearance(background_appearance)
                        })
                        .ok();
                }

                eager_load_active_theme_and_icon_theme(fs.clone(), cx);

                languages.set_theme(cx.theme().clone());
                let new_host = &client::ClientSettings::get_global(cx).server_url;
                if &http.base_url() != new_host {
                    http.set_base_url(new_host);
                    if client.status().borrow().is_connected() {
                        client.reconnect(&cx.to_async());
                    }
                }
            }
        })
        .detach();
    });

    cx.add_plugins(move |cx: &mut App| {
        telemetry::event!(
            "Settings Changed",
            setting = "theme",
            value = cx.theme().name.to_string()
        );
        telemetry::event!(
            "Settings Changed",
            setting = "keymap",
            value = BaseKeymap::get_global(cx).to_string()
        );
        cx.global::<GlobalClient>()
            .0
            .telemetry()
            .flush_events()
            .detach();
    });

    cx.add_plugins(load_user_themes_in_background);
    cx.add_plugins(watch_themes);
    cx.add_plugins(watch_languages);
    cx.add_plugins(|cx: &mut App| {
        cx.set_menus(app_menus());
    });
    cx.add_plugins(initialize_workspace);
    cx.add_plugins(|cx: &mut App| cx.activate(true));
    cx.add_plugins(|cx: &mut App| {
        cx.spawn({
            let client = cx.global::<GlobalClient>().0.clone();
            async move |cx| match authenticate(client, &cx).await {
                ConnectionResult::Timeout => log::error!("Timeout during initial auth"),
                ConnectionResult::ConnectionReset => {
                    log::error!("Connection reset during initial auth")
                }
                ConnectionResult::Result(r) => {
                    r.log_err();
                }
            }
        })
        .detach();
    });
    cx.add_plugins(crate::zed::component_preview::init);
    cx.add_plugins(|cx: &mut App| {
        let urls: Vec<_> = cx
            .global::<GlobalArgs>()
            .0
            .paths_or_urls
            .iter()
            .filter_map(|arg| parse_url_arg(arg, cx).log_err())
            .collect();

        if !urls.is_empty() {
            OpenListenerTx::global(cx).open_urls(urls);
        }

        // 11

        let mut open_rx = cx.global_mut::<OpenListenerRx>().0.take().unwrap();

        // Check for initial open request
        let request = open_rx
            .try_next()
            .ok()
            .flatten()
            .and_then(|urls| OpenRequest::parse(urls, cx).log_err());
        match request {
            Some(request) => {
                handle_open_request(request, cx);
            }
            None => {
                cx.spawn({
                    let app_state = AppState::global(cx).clone();
                    async move |mut cx| {
                        if let Err(e) = restore_or_create_workspace(app_state, &mut cx).await {
                            fail_to_open_window_async(e, &mut cx)
                        }
                    }
                })
                .detach();
            }
        }

        // Pass open handler to background task for subsequent open requests
        cx.spawn(async move |cx| {
            while let Some(urls) = open_rx.next().await {
                cx.update(|cx| {
                    if let Some(request) = OpenRequest::parse(urls, cx).log_err() {
                        handle_open_request(request, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    });
}

fn handle_open_request(request: OpenRequest, cx: &mut App) {
    if let Some(connection) = request.cli_connection {
        let app_state = AppState::global(cx);
        cx.spawn(async move |cx| handle_cli_connection(connection, app_state, cx).await)
            .detach();
        return;
    }

    if let Some(action_index) = request.dock_menu_action {
        cx.perform_dock_menu_action(action_index);
        return;
    }

    if let Some(connection_options) = request.ssh_connection {
        let app_state = AppState::global(cx);
        cx.spawn(async move |mut cx| {
            let paths_with_position =
                derive_paths_with_position(app_state.fs.as_ref(), request.open_paths).await;
            open_ssh_project(
                connection_options,
                paths_with_position.into_iter().map(|p| p.path).collect(),
                app_state,
                workspace::OpenOptions::default(),
                &mut cx,
            )
            .await
        })
        .detach_and_log_err(cx);
        return;
    }

    let mut task = None;
    if !request.open_paths.is_empty() {
        let app_state = AppState::global(cx);
        task = Some(cx.spawn(async move |mut cx| {
            let paths_with_position =
                derive_paths_with_position(app_state.fs.as_ref(), request.open_paths).await;
            let (_window, results) = open_paths_with_positions(
                &paths_with_position,
                app_state,
                workspace::OpenOptions::default(),
                &mut cx,
            )
            .await?;
            for result in results.into_iter().flatten() {
                if let Err(err) = result {
                    log::error!("Error opening path: {err}",);
                }
            }
            anyhow::Ok(())
        }));
    }

    if !request.open_channel_notes.is_empty() || request.join_channel.is_some() {
        let app_state = AppState::global(cx);
        cx.spawn(async move |mut cx| {
            let result = maybe!(async {
                if let Some(task) = task {
                    task.await?;
                }
                let client = app_state.client.clone();
                // we continue even if authentication fails as join_channel/ open channel notes will
                // show a visible error message.
                match authenticate(client, &cx).await {
                    ConnectionResult::Timeout => {
                        log::error!("Timeout during open request handling")
                    }
                    ConnectionResult::ConnectionReset => {
                        log::error!("Connection reset during open request handling")
                    }
                    ConnectionResult::Result(r) => r?,
                };

                if let Some(channel_id) = request.join_channel {
                    cx.update(|cx| {
                        workspace::join_channel(
                            client::ChannelId(channel_id),
                            app_state.clone(),
                            None,
                            cx,
                        )
                    })?
                    .await?;
                }

                let workspace_window =
                    workspace::get_any_active_workspace(app_state, cx.clone()).await?;
                let workspace = workspace_window.entity(cx)?;

                let mut promises = Vec::new();
                for (channel_id, heading) in request.open_channel_notes {
                    promises.push(cx.update_window(workspace_window.into(), |_, window, cx| {
                        ChannelView::open(
                            client::ChannelId(channel_id),
                            heading,
                            workspace.clone(),
                            window,
                            cx,
                        )
                        .log_err()
                    })?)
                }
                future::join_all(promises).await;
                anyhow::Ok(())
            })
            .await;
            if let Err(err) = result {
                fail_to_open_window_async(err, &mut cx);
            }
        })
        .detach()
    } else if let Some(task) = task {
        cx.spawn(async move |mut cx| {
            if let Err(err) = task.await {
                fail_to_open_window_async(err, &mut cx);
            }
        })
        .detach();
    }
}

async fn authenticate(client: Arc<Client>, cx: &AsyncApp) -> ConnectionResult<()> {
    if stdout_is_a_pty() {
        if client::IMPERSONATE_LOGIN.is_some() {
            return client.authenticate_and_connect(false, cx).await;
        } else if client.has_credentials(cx).await {
            return client.authenticate_and_connect(true, cx).await;
        }
    } else if client.has_credentials(cx).await {
        return client.authenticate_and_connect(true, cx).await;
    }

    ConnectionResult::Result(Ok(()))
}

async fn system_id() -> Result<IdType> {
    let key_name = "system_id".to_string();

    if let Ok(Some(system_id)) = GLOBAL_KEY_VALUE_STORE.read_kvp(&key_name) {
        return Ok(IdType::Existing(system_id));
    }

    let system_id = Uuid::new_v4().to_string();

    GLOBAL_KEY_VALUE_STORE
        .write_kvp(key_name, system_id.clone())
        .await?;

    Ok(IdType::New(system_id))
}

async fn installation_id() -> Result<IdType> {
    let legacy_key_name = "device_id".to_string();
    let key_name = "installation_id".to_string();

    // Migrate legacy key to new key
    if let Ok(Some(installation_id)) = KEY_VALUE_STORE.read_kvp(&legacy_key_name) {
        KEY_VALUE_STORE
            .write_kvp(key_name, installation_id.clone())
            .await?;
        KEY_VALUE_STORE.delete_kvp(legacy_key_name).await?;
        return Ok(IdType::Existing(installation_id));
    }

    if let Ok(Some(installation_id)) = KEY_VALUE_STORE.read_kvp(&key_name) {
        return Ok(IdType::Existing(installation_id));
    }

    let installation_id = Uuid::new_v4().to_string();

    KEY_VALUE_STORE
        .write_kvp(key_name, installation_id.clone())
        .await?;

    Ok(IdType::New(installation_id))
}

async fn restore_or_create_workspace(app_state: Arc<AppState>, cx: &mut AsyncApp) -> Result<()> {
    if let Some(locations) = restorable_workspace_locations(cx, &app_state).await {
        for location in locations {
            match location {
                SerializedWorkspaceLocation::Local(location, _) => {
                    let task = cx.update(|cx| {
                        workspace::open_paths(
                            location.paths().as_ref(),
                            app_state.clone(),
                            workspace::OpenOptions::default(),
                            cx,
                        )
                    })?;
                    task.await?;
                }
                SerializedWorkspaceLocation::Ssh(ssh) => {
                    let connection_options = cx.update(|cx| {
                        SshSettings::get_global(cx)
                            .connection_options_for(ssh.host, ssh.port, ssh.user)
                    })?;
                    let app_state = app_state.clone();
                    cx.spawn(async move |cx| {
                        recent_projects::open_ssh_project(
                            connection_options,
                            ssh.paths.into_iter().map(PathBuf::from).collect(),
                            app_state,
                            workspace::OpenOptions::default(),
                            cx,
                        )
                        .await
                        .log_err();
                    })
                    .detach();
                }
            }
        }
    } else if matches!(KEY_VALUE_STORE.read_kvp(FIRST_OPEN), Ok(None)) {
        cx.update(|cx| show_welcome_view(app_state, cx))?.await?;
    } else {
        cx.update(|cx| {
            workspace::open_new(
                Default::default(),
                app_state,
                cx,
                |workspace, window, cx| {
                    Editor::new_file(workspace, &Default::default(), window, cx)
                },
            )
        })?
        .await?;
    }

    Ok(())
}

pub(crate) async fn restorable_workspace_locations(
    cx: &mut AsyncApp,
    app_state: &Arc<AppState>,
) -> Option<Vec<SerializedWorkspaceLocation>> {
    let mut restore_behavior = cx
        .update(|cx| WorkspaceSettings::get(None, cx).restore_on_startup)
        .ok()?;

    let session_handle = app_state.session.clone();
    let (last_session_id, last_session_window_stack) = cx
        .update(|cx| {
            let session = session_handle.read(cx);

            (
                session.last_session_id().map(|id| id.to_string()),
                session.last_session_window_stack(),
            )
        })
        .ok()?;

    if last_session_id.is_none()
        && matches!(
            restore_behavior,
            workspace::RestoreOnStartupBehavior::LastSession
        )
    {
        restore_behavior = workspace::RestoreOnStartupBehavior::LastWorkspace;
    }

    match restore_behavior {
        workspace::RestoreOnStartupBehavior::LastWorkspace => {
            workspace::last_opened_workspace_location()
                .await
                .map(|location| vec![location])
        }
        workspace::RestoreOnStartupBehavior::LastSession => {
            if let Some(last_session_id) = last_session_id {
                let ordered = last_session_window_stack.is_some();

                let mut locations = workspace::last_session_workspace_locations(
                    &last_session_id,
                    last_session_window_stack,
                )
                .filter(|locations| !locations.is_empty());

                // Since last_session_window_order returns the windows ordered front-to-back
                // we need to open the window that was frontmost last.
                if ordered {
                    if let Some(locations) = locations.as_mut() {
                        locations.reverse();
                    }
                }

                locations
            } else {
                None
            }
        }
        _ => None,
    }
}

fn init_paths() -> HashMap<io::ErrorKind, Vec<&'static Path>> {
    [
        paths::config_dir(),
        paths::extensions_dir(),
        paths::languages_dir(),
        paths::database_dir(),
        paths::logs_dir(),
        paths::temp_dir(),
    ]
    .into_iter()
    .fold(HashMap::default(), |mut errors, path| {
        if let Err(e) = std::fs::create_dir_all(path) {
            errors.entry(e.kind()).or_insert_with(Vec::new).push(path);
        }
        errors
    })
}

fn stdout_is_a_pty() -> bool {
    std::env::var(FORCE_CLI_MODE_ENV_VAR_NAME).ok().is_none() && io::stdout().is_terminal()
}

#[derive(Parser, Debug)]
#[command(name = "zed", disable_version_flag = true)]
struct Args {
    /// A sequence of space-separated paths or urls that you want to open.
    ///
    /// Use `path:line:row` syntax to open a file at a specific location.
    /// Non-existing paths and directories will ignore `:line:row` suffix.
    ///
    /// URLs can either be `file://` or `zed://` scheme, or relative to <https://zed.dev>.
    paths_or_urls: Vec<String>,

    /// Sets a custom directory for all user data (e.g., database, extensions, logs).
    /// This overrides the default platform-specific data directory location.
    /// On macOS, the default is `~/Library/Application Support/Zed`.
    /// On Linux/FreeBSD, the default is `$XDG_DATA_HOME/zed`.
    /// On Windows, the default is `%LOCALAPPDATA%\Zed`.
    #[arg(long, value_name = "DIR")]
    user_data_dir: Option<String>,

    /// Instructs zed to run as a dev server on this machine. (not implemented)
    #[arg(long)]
    dev_server_token: Option<String>,

    /// Prints system specs. Useful for submitting issues on GitHub when encountering a bug
    /// that prevents Zed from starting, so you can't run `zed: copy system specs to clipboard`
    #[arg(long)]
    system_specs: bool,

    /// Used for SSH/Git password authentication, to remove the need for netcat as a dependency,
    /// by having Zed act like netcat communicating over a Unix socket.
    #[arg(long, hide = true)]
    askpass: Option<String>,

    /// Run zed in the foreground, only used on Windows, to match the behavior of the behavior on macOS.
    #[arg(long)]
    #[cfg(target_os = "windows")]
    #[arg(hide = true)]
    foreground: bool,

    /// The dock action to perform. This is used on Windows only.
    #[arg(long)]
    #[cfg(target_os = "windows")]
    #[arg(hide = true)]
    dock_action: Option<usize>,
}

#[derive(Clone, Debug)]
enum IdType {
    New(String),
    Existing(String),
}

impl ToString for IdType {
    fn to_string(&self) -> String {
        match self {
            IdType::New(id) | IdType::Existing(id) => id.clone(),
        }
    }
}

fn parse_url_arg(arg: &str, cx: &App) -> Result<String> {
    match std::fs::canonicalize(Path::new(&arg)) {
        Ok(path) => Ok(format!("file://{}", path.display())),
        Err(error) => {
            if arg.starts_with("file://")
                || arg.starts_with("zed-cli://")
                || arg.starts_with("ssh://")
                || parse_zed_link(arg, cx).is_some()
            {
                Ok(arg.into())
            } else {
                anyhow::bail!("error parsing path argument: {error}")
            }
        }
    }
}

fn load_embedded_fonts(cx: &mut App) {
    let asset_source = cx.asset_source();
    let font_paths = asset_source.list("fonts").unwrap();
    let embedded_fonts = Mutex::new(Vec::new());
    let executor = cx.background_executor();

    executor.block(executor.scoped(|scope| {
        for font_path in &font_paths {
            if !font_path.ends_with(".ttf") {
                continue;
            }

            scope.spawn(async {
                let font_bytes = asset_source.load(font_path).unwrap().unwrap();
                embedded_fonts.lock().push(font_bytes);
            });
        }
    }));

    cx.text_system()
        .add_fonts(embedded_fonts.into_inner())
        .unwrap();
}

/// Eagerly loads the active theme and icon theme based on the selections in the
/// theme settings.
///
/// This fast path exists to load these themes as soon as possible so the user
/// doesn't see the default themes while waiting on extensions to load.
fn eager_load_active_theme_and_icon_theme(fs: Arc<dyn Fs>, cx: &App) {
    let extension_store = ExtensionStore::global(cx);
    let theme_registry = ThemeRegistry::global(cx);
    let theme_settings = ThemeSettings::get_global(cx);
    let appearance = SystemAppearance::global(cx).0;

    if let Some(theme_selection) = theme_settings.theme_selection.as_ref() {
        let theme_name = theme_selection.theme(appearance);
        if matches!(theme_registry.get(theme_name), Err(ThemeNotFoundError(_))) {
            if let Some(theme_path) = extension_store.read(cx).path_to_extension_theme(theme_name) {
                cx.spawn({
                    let theme_registry = theme_registry.clone();
                    let fs = fs.clone();
                    async move |cx| {
                        theme_registry.load_user_theme(&theme_path, fs).await?;

                        cx.update(|cx| {
                            ThemeSettings::reload_current_theme(cx);
                        })
                    }
                })
                .detach_and_log_err(cx);
            }
        }
    }

    if let Some(icon_theme_selection) = theme_settings.icon_theme_selection.as_ref() {
        let icon_theme_name = icon_theme_selection.icon_theme(appearance);
        if matches!(
            theme_registry.get_icon_theme(icon_theme_name),
            Err(IconThemeNotFoundError(_))
        ) {
            if let Some((icon_theme_path, icons_root_path)) = extension_store
                .read(cx)
                .path_to_extension_icon_theme(icon_theme_name)
            {
                cx.spawn({
                    let theme_registry = theme_registry.clone();
                    let fs = fs.clone();
                    async move |cx| {
                        theme_registry
                            .load_icon_theme(&icon_theme_path, &icons_root_path, fs)
                            .await?;

                        cx.update(|cx| {
                            ThemeSettings::reload_current_icon_theme(cx);
                        })
                    }
                })
                .detach_and_log_err(cx);
            }
        }
    }
}

/// Spawns a background task to load the user themes from the themes directory.
fn load_user_themes_in_background(cx: &mut App) {
    let fs = <dyn Fs>::global(cx);
    cx.spawn({
        let fs = fs.clone();
        async move |cx| {
            if let Some(theme_registry) =
                cx.update(|cx| ThemeRegistry::global(cx).clone()).log_err()
            {
                let themes_dir = paths::themes_dir().as_ref();
                match fs
                    .metadata(themes_dir)
                    .await
                    .ok()
                    .flatten()
                    .map(|m| m.is_dir)
                {
                    Some(is_dir) => {
                        anyhow::ensure!(is_dir, "Themes dir path {themes_dir:?} is not a directory")
                    }
                    None => {
                        fs.create_dir(themes_dir).await.with_context(|| {
                            format!("Failed to create themes dir at path {themes_dir:?}")
                        })?;
                    }
                }
                theme_registry.load_user_themes(themes_dir, fs).await?;
                cx.update(ThemeSettings::reload_current_theme)?;
            }
            anyhow::Ok(())
        }
    })
    .detach_and_log_err(cx);
}

/// Spawns a background task to watch the themes directory for changes.
fn watch_themes(cx: &mut App) {
    use std::time::Duration;
    let fs = <dyn Fs>::global(cx);
    cx.spawn(async move |cx| {
        let (mut events, _) = fs
            .watch(paths::themes_dir(), Duration::from_millis(100))
            .await;

        while let Some(paths) = events.next().await {
            for event in paths {
                if fs.metadata(&event.path).await.ok().flatten().is_some() {
                    if let Some(theme_registry) =
                        cx.update(|cx| ThemeRegistry::global(cx).clone()).log_err()
                    {
                        if let Some(()) = theme_registry
                            .load_user_theme(&event.path, fs.clone())
                            .await
                            .log_err()
                        {
                            cx.update(ThemeSettings::reload_current_theme).log_err();
                        }
                    }
                }
            }
        }
    })
    .detach()
}

#[cfg(debug_assertions)]
fn watch_languages(cx: &mut App) {
    use std::time::Duration;
    let fs = <dyn Fs>::global(cx);
    let languages = cx.global::<GlobalLanguageRegistry>().0.clone();

    let path = {
        let p = Path::new("crates/languages/src");
        let Ok(full_path) = p.canonicalize() else {
            return;
        };
        full_path
    };

    cx.spawn(async move |_| {
        let (mut events, _) = fs.watch(path.as_path(), Duration::from_millis(100)).await;
        while let Some(event) = events.next().await {
            let has_language_file = event.iter().any(|event| {
                event
                    .path
                    .extension()
                    .map(|ext| ext.to_string_lossy().as_ref() == "scm")
                    .unwrap_or(false)
            });
            if has_language_file {
                languages.reload();
            }
        }
    })
    .detach()
}

#[cfg(not(debug_assertions))]
fn watch_languages(_fs: Arc<dyn fs::Fs>, _languages: Arc<LanguageRegistry>, _cx: &mut App) {}
