mod api;
mod app_log;
mod autostart;
mod builtin_remote_rules;
mod commands;
mod config;
mod conn_journal;
mod core;
mod domain;
mod error;
mod log_retention;
mod portable;
mod proxy;
mod remote_rule_auto;
mod rule_apply;
mod runtime;
mod services;
mod smart_switch;
mod srs;
mod state;
mod storage;
mod subscription;
mod subscription_auto;
mod tray;
mod url_scheme;
mod window_ctrl;

use state::AppState;
use tauri::{Emitter, Manager};

const MAX_DEEP_LINK_URLS: usize = 8;
const MAX_DEEP_LINK_URL_LEN: usize = 8 * 1024;

fn show_startup_failure(
    app: &tauri::App,
    error: impl std::fmt::Display,
    data_dir: Option<&std::path::Path>,
) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    let handle = app.handle().clone();
    let location = data_dir
        .map(|path| {
            format!(
                "\n\n数据目录：{}\n日志目录：{}",
                path.display(),
                path.join("logs").display()
            )
        })
        .unwrap_or_default();
    let message = format!(
        "Satelite 无法加载本地数据，已停止启动以避免覆盖现有配置。\n\n错误：{error}{location}"
    );
    let dialog = app
        .dialog()
        .message(message)
        .title("Satelite 启动失败")
        .kind(MessageDialogKind::Error);
    if let Some(path) = data_dir.map(std::path::Path::to_path_buf) {
        dialog
            .buttons(MessageDialogButtons::OkCancelCustom(
                "打开数据目录".into(),
                "退出".into(),
            ))
            .show(move |open| {
                if open {
                    let _ = tauri_plugin_opener::open_path(path, None::<&str>);
                }
                handle.exit(1);
            });
    } else {
        dialog.show(move |_| handle.exit(1));
    }
}

fn bounded_deep_link_urls(urls: impl IntoIterator<Item = String>) -> Vec<String> {
    urls.into_iter()
        .filter(|url| url.len() <= MAX_DEEP_LINK_URL_LEN)
        .take(MAX_DEEP_LINK_URLS)
        .collect()
}

pub use domain::{
    AppSettings, ParseResult as SubscriptionParseResult, Protocol, ProtocolConfig, ProxyNode,
    SkippedProxy, Subscription, SubscriptionFormat, SubscriptionSource, SubscriptionView,
    TlsConfig, Transport,
};
pub use subscription::parse_subscription;

pub async fn download_core_to(
    app_data_dir: &std::path::Path,
    tag: Option<String>,
) -> Result<core::CoreDownloadResult, String> {
    core::download_latest_core(core::CoreKind::SingBox, app_data_dir, tag)
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    if let Some(code) = core::manager::try_run_elevated_log_helper() {
        std::process::exit(code);
    }
    // Portable mode: when `portable.flag` sits next to the exe, disable the
    // config-window auto-create so setup can rebuild the window with the
    // WebView2 profile redirected next to the exe (see portable.rs).
    let mut context = tauri::generate_context!();
    portable::patch_context(&mut context);
    let mut builder = tauri::Builder::default();

    // Single instance + deep-link: second launch (e.g. click clash:// while running)
    // forwards argv to the first process on Windows/Linux.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            window_ctrl::show_main(app);
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            // Portable: config windows were disabled pre-build; recreate them
            // here with the WebView2 profile next to the exe. Must precede the
            // silent-start hide and everything that expects the "main" window.
            if portable::is_portable() {
                portable::build_main_window(app);
            }
            let dir = match portable::resolve_app_data_dir(app.handle()) {
                Ok(dir) => dir,
                Err(error) => {
                    show_startup_failure(app, error, None);
                    return Ok(());
                }
            };
            if let Err(error) = std::fs::create_dir_all(&dir) {
                show_startup_failure(app, error, Some(&dir));
                return Ok(());
            }
            app_log::init(dir.join("logs"));
            app_log::install_panic_hook();
            if portable::is_portable() {
                app_log::info(
                    "app",
                    format!("portable mode active, data dir: {}", dir.display()),
                );
            }
            let resource_dir = app.path().resource_dir().ok();
            let app_state = match AppState::load(dir.clone(), resource_dir) {
                Ok(state) => state,
                Err(error) => {
                    app_log::error("startup", format!("load app store failed: {error}"));
                    show_startup_failure(app, error, Some(&dir));
                    return Ok(());
                }
            };

            // Snapshot app prefs before move into managed state
            let silent = app_state
                .with_store(|s| Ok(s.settings.silent_start))
                .unwrap_or(false);
            let auto_proxy = app_state
                .with_store(|s| Ok(s.settings.auto_start_proxy))
                .unwrap_or(false);
            let keep_system_proxy_for_auto_start = auto_proxy
                && app_state
                    .with_store(|s| {
                        Ok(s.settings.capture_mode == crate::domain::CaptureMode::System)
                    })
                    .unwrap_or(false);
            // Keep LaunchAgent in sync with stored preference
            let launch = app_state
                .with_store(|s| Ok(s.settings.launch_at_login))
                .unwrap_or(false);
            let _ = autostart::set_launch_at_login(launch);

            app.manage(app_state);
            app_log::info("app", "Satelite started");

            // Seed bundled remote rule sets: copy the packaged `.srs` files
            // into the remote cache and heal their store entries. Must run
            // before `remote_rule_auto::spawn` so the copies are referenced
            // before its orphan cleanup wakes up.
            if let Some(state) = app.try_state::<AppState>() {
                let app_data_dir = state.app_data_dir.clone();
                let resource_dir = state.resource_dir.clone();
                if let Err(error) = state.with_store_mut(|store| {
                    crate::builtin_remote_rules::seed(
                        &app_data_dir,
                        resource_dir.as_deref(),
                        store,
                    );
                    Ok(())
                }) {
                    app_log::warn(
                        "builtin_rules",
                        format!("seed bundled rule sets failed: {error}"),
                    );
                }
            }

            // Build reqwest blocking client on a plain OS thread so its internal
            // Tokio runtime is never created/dropped on a tauri async worker.
            std::thread::spawn(|| {
                crate::api::warmup_blocking_client();
            });

            if let Err(e) = tray::setup_tray(app.handle()) {
                app_log::error("tray", format!("setup failed: {e}"));
            }

            // Connection journal: WebSocket snapshots @100ms + ring history.
            // Clash API only yields live sockets; low-interval stream reduces misses.
            conn_journal::spawn_connection_journal(app.handle().clone());

            // Profile auto-update (per-subscription interval, default 1440 min).
            subscription_auto::spawn(app.handle().clone());

            // Remote rule sets are fetched by the app and cached locally so
            // sing-box startup never blocks on remote downloads.
            remote_rule_auto::spawn(app.handle().clone());

            // Smart node switch (docs/auto.md): passive + on-demand probe.
            smart_switch::spawn(app.handle().clone());

            // Core watchdog: a core that dies without a user stop is
            // auto-restarted, bounded by
            // an attempt budget so config-error loops cannot spin.
            state::spawn_core_watchdog(app.handle().clone());

            // Deep links (clash:// · sing-box://): show UI; frontend opens add form.
            // Pending URLs live in AppState until the user closes the modal (then cleared).
            let mut launched_via_deep_link = false;
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let queue_import = |handle: &tauri::AppHandle, urls: Vec<String>| {
                    let urls = bounded_deep_link_urls(urls);
                    if urls.is_empty() {
                        return;
                    }
                    app_log::info("deep-link", format!("queued {} URL(s)", urls.len()));
                    if let Some(state) = handle.try_state::<AppState>() {
                        state.set_pending_import_urls(urls.clone());
                    }
                    window_ctrl::show_main(handle);
                    let _ = handle.emit("deep-link-urls", urls);
                };

                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    let list = bounded_deep_link_urls(urls.iter().map(|url| url.to_string()));
                    if !list.is_empty() {
                        launched_via_deep_link = true;
                        app_log::info("deep-link", format!("queued {} startup URL(s)", list.len()));
                        // Store immediately; re-emit after UI boot if listener wasn't ready.
                        if let Some(state) = app.try_state::<AppState>() {
                            state.set_pending_import_urls(list.clone());
                        }
                        let handle = app.handle().clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            window_ctrl::show_main(&handle);
                            let _ = handle.emit("deep-link-urls", list);
                        });
                    }
                }
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> = event.urls().iter().map(|u| u.to_string()).collect();
                    queue_import(&handle, urls);
                });
                // Dev / Linux / Windows: register schemes for the current executable.
                #[cfg(any(windows, target_os = "linux"))]
                {
                    if let Err(e) = app.deep_link().register_all() {
                        app_log::error("deep-link", format!("register_all failed: {e}"));
                    }
                }
            }

            // Multiple clients share clash:// · sing-box:// — claim default so
            // browser "one-click import" opens Satelite (not Sparkle / Verge / …).
            url_scheme::claim_subscription_schemes();

            // Silent start: hide only (do not destroy at launch — that can exit the app).
            // Skip when opened via one-click subscribe so the add form is visible.
            if silent && !launched_via_deep_link {
                window_ctrl::soft_hide_main(app.handle());
            }

            // Reconcile a proxy left by an unclean exit and auto-start in one
            // worker, so cleanup can never race with re-enabling system mode.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                // slight delay so tray / window settle
                std::thread::sleep(std::time::Duration::from_millis(400));
                let Some(state) = handle.try_state::<AppState>() else {
                    return;
                };
                if !keep_system_proxy_for_auto_start {
                    match state.cleanup_stale_system_proxy() {
                        Ok(true) => app_log::warn(
                            "system_proxy",
                            "cleared stale owned proxy during startup",
                        ),
                        Ok(false) => {}
                        Err(error) => app_log::error(
                            "system_proxy",
                            format!("startup reconciliation failed: {error}"),
                        ),
                    }
                }
                if !auto_proxy {
                    return;
                }
                let res = handle.path().resource_dir().ok();
                if let Err(error) = state.start_proxy(res.as_deref(), false) {
                    app_log::error("app", format!("auto_start_proxy failed: {error}"));
                    if keep_system_proxy_for_auto_start {
                        match state.cleanup_stale_system_proxy() {
                            Ok(true) => app_log::warn(
                                "system_proxy",
                                "auto-start failed; cleared stale owned proxy",
                            ),
                            Ok(false) => {}
                            Err(cleanup_error) => app_log::error(
                                "system_proxy",
                                format!("auto-start cleanup failed: {cleanup_error}"),
                            ),
                        }
                    }
                } else {
                    app_log::info("app", "auto_start_proxy ok");
                    tray::refresh_icon(&handle);
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    let close_to_tray = window
                        .app_handle()
                        .try_state::<AppState>()
                        .and_then(|s| s.with_store(|st| Ok(st.settings.close_to_tray)).ok())
                        .unwrap_or(true);
                    if close_to_tray {
                        // Keep Rust + tray + core; optionally destroy WebView for memory.
                        api.prevent_close();
                        window_ctrl::hide_main_to_tray(window.app_handle());
                    } else {
                        // Real quit from window close
                        api.prevent_close();
                        window_ctrl::quit_app(window.app_handle());
                    }
                }
                tauri::WindowEvent::Focused(true) => {
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        state.set_ui_visible(true);
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_subscriptions,
            commands::list_subscription_urls,
            commands::get_subscription,
            commands::add_subscription_url,
            commands::add_subscription_file,
            commands::add_subscription_text,
            commands::add_subscription_node,
            commands::add_subscription_singbox,
            commands::read_import_file,
            commands::update_subscription,
            commands::refresh_subscription,
            commands::activate_subscription,
            commands::set_runtime_source,
            commands::set_mix_mode,
            commands::remove_subscription,
            commands::list_subscription_nodes,
            commands::list_all_nodes,
            commands::list_nodes_page,
            commands::list_node_ids,
            commands::list_custom_config_nodes,
            commands::get_settings,
            commands::update_settings,
            commands::diagnose_network,
            commands::regenerate_api_secret,
            commands::set_current_node,
            commands::rename_node,
            commands::generate_singbox_config,
            commands::preview_singbox_config,
            commands::get_active_config_path,
            commands::get_core_info,
            commands::get_lan_ip,
            commands::check_core_update,
            commands::check_app_update,
            commands::get_app_install_path,
            commands::download_core,
            commands::fetch_core_latest,
            commands::refresh_geodata,
            commands::set_core_type,
            commands::test_nodes_latency,
            commands::test_custom_nodes_latency,
            commands::get_proxy_status,
            commands::start_proxy,
            commands::stop_proxy,
            commands::restart_proxy,
            commands::set_system_proxy,
            commands::set_tun_enabled,
            commands::set_capture_mode,
            commands::set_outbound_mode,
            commands::get_dns_settings,
            commands::update_dns_settings,
            commands::reset_dns_defaults,
            commands::diagnose_dns,
            commands::read_system_hosts,
            commands::set_current_node_live,
            commands::smart_switch_now,
            commands::list_rule_sets,
            commands::get_rule_set,
            commands::list_remote_rule_items,
            commands::set_active_rule_set,
            commands::set_rule_set_enabled,
            commands::set_rule_set_strategy,
            commands::set_rule_set_dns_strategy,
            commands::batch_set_rule_targets,
            commands::create_rule_set,
            commands::update_rule_set,
            commands::refresh_remote_rule_set,
            commands::reorder_rule_sets,
            commands::delete_rule_set,
            commands::reset_rule_set,
            commands::reset_builtin_rule_set,
            commands::list_rules,
            commands::save_rule,
            commands::remove_rule,
            commands::set_rule_enabled,
            commands::list_pools,
            commands::create_pool,
            commands::update_pool,
            commands::delete_pool,
            commands::list_chains,
            commands::list_chain_usage,
            commands::create_chain,
            commands::update_chain,
            commands::delete_chain,
            commands::diagnose_chain,
            commands::list_connections,
            commands::list_connection_changes,
            commands::list_requests,
            commands::list_request_failures,
            commands::clear_request_history,
            commands::list_app_logs,
            commands::clear_app_logs,
            commands::get_core_log_tail,
            parse_subscription_text,
            set_ui_mode_pref,
            peek_pending_import_urls,
            clear_pending_import_urls,
        ])
        .build(context)
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            match event {
                // Destroying the last WebView triggers ExitRequested. Stay in tray
                // unless the user explicitly quit (exit_allowed).
                tauri::RunEvent::ExitRequested { api, .. } => {
                    let allow = app_handle
                        .try_state::<AppState>()
                        .map(|s| s.is_exit_allowed())
                        .unwrap_or(false);
                    if !allow {
                        api.prevent_exit();
                        return;
                    }
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        state.shutdown_runtime();
                    }
                    app_log::flush();
                }
                // Process is exiting regardless (Cmd+Q / terminate: goes straight here,
                // bypassing ExitRequested and exit_allowed). Always clean up.
                tauri::RunEvent::Exit => {
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        state.shutdown_runtime();
                    }
                    app_log::flush();
                }
                // macOS Dock / “reopen”: user clicked the app icon while no visible window
                // (UI destroyed or hidden to tray). Tray already calls show_main; Dock did not.
                // Reopen is a macOS-only RunEvent variant.
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen {
                    has_visible_windows,
                    ..
                } => {
                    if !has_visible_windows {
                        window_ctrl::show_main(app_handle);
                    } else {
                        // Still focus main if it exists but is not key window.
                        window_ctrl::show_main(app_handle);
                    }
                }
                _ => {}
            }
        });
}

#[tauri::command]
fn parse_subscription_text(content: String) -> Result<domain::ParseResult, String> {
    parse_subscription(&content).map_err(|e| e.to_string())
}

/// Persist UI shell preference (pro | simple) for correct window size on recreate.
#[tauri::command]
fn set_ui_mode_pref(app: tauri::AppHandle, mode: String) -> Result<(), String> {
    let dir = portable::resolve_app_data_dir(&app).map_err(|e| e.to_string())?;
    window_ctrl::write_ui_mode(&dir, &mode);
    Ok(())
}

/// Deep-link import URLs still waiting for the add form (None after user closes it).
#[tauri::command]
fn peek_pending_import_urls(state: tauri::State<'_, AppState>) -> Option<Vec<String>> {
    state.peek_pending_import_urls()
}

/// User closed / finished the add-subscription dialog — do not re-open on next UI wake.
#[tauri::command]
fn clear_pending_import_urls(state: tauri::State<'_, AppState>) {
    state.clear_pending_import_urls();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_deep_link_url_count_and_length() {
        let mut urls: Vec<String> = (0..10)
            .map(|index| format!("clash://install-config?url={index}"))
            .collect();
        urls.insert(2, "x".repeat(MAX_DEEP_LINK_URL_LEN + 1));

        let bounded = bounded_deep_link_urls(urls);

        assert_eq!(bounded.len(), MAX_DEEP_LINK_URLS);
        assert!(bounded.iter().all(|url| url.len() <= MAX_DEEP_LINK_URL_LEN));
        assert_eq!(bounded[2], "clash://install-config?url=2");
    }
}
