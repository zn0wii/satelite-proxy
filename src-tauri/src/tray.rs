//! System tray: open window, start/stop, quit (with cleanup).

use crate::domain::{CaptureMode, TrayIconStyle};
use crate::state::AppState;
use crate::window_ctrl;
#[cfg(not(target_os = "windows"))]
use std::io::Write;
#[cfg(not(target_os = "windows"))]
use std::process::{Command, Stdio};
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime as TauriRuntime,
};

/// Same shell line as Dashboard “复制环境变量”.
fn proxy_env_export(mixed_port: u16) -> String {
    let proxy_url = format!("http://127.0.0.1:{mixed_port}");
    if cfg!(target_os = "windows") {
        format!(r#"$env:ALL_PROXY = "{proxy_url}""#)
    } else {
        format!("export all_proxy={proxy_url}")
    }
}

#[cfg(test)]
mod tests {
    use super::proxy_env_export;

    #[test]
    fn proxy_env_matches_current_platform_shell() {
        let text = proxy_env_export(2080);
        if cfg!(target_os = "windows") {
            assert_eq!(text, r#"$env:ALL_PROXY = "http://127.0.0.1:2080""#);
        } else {
            assert_eq!(text, "export all_proxy=http://127.0.0.1:2080");
        }
    }
}

/// Round-trips text through the real clipboard to guard the unsafe
/// `SetClipboardData` plumbing end to end.
/// `#[ignore]` by default — it **replaces whatever the user has copied**.
/// Run with `cargo test --lib tray::clipboard_tests -- --ignored`.
#[cfg(all(test, target_os = "windows"))]
mod clipboard_tests {
    use super::set_clipboard_text;
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;

    #[test]
    #[ignore = "overwrites the user's clipboard"]
    fn clipboard_roundtrip_preserves_unicode() {
        let sample = "all_proxy=http://127.0.0.1:2080 代理";
        set_clipboard_text(sample).expect("set clipboard");

        let readback = unsafe {
            OpenClipboard(None).expect("open clipboard for readback");
            let handle = GetClipboardData(CF_UNICODETEXT).expect("get clipboard data");
            let global = HGLOBAL(handle.0);
            let ptr = GlobalLock(global).cast::<u16>();
            let len = GlobalSize(global) / 2;
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(global);
            let _ = CloseClipboard();
            text.trim_end_matches('\0').to_string()
        };
        assert_eq!(readback, sample);
    }
}

/// Put text on the clipboard through the Win32 API instead of piping into
/// `clip.exe`: a GUI-subsystem process spawning `cmd` flashes a console
/// window, and `clip.exe` round-trips stdin through the console codepage,
/// which can mangle non-ASCII text. CF_UNICODETEXT has neither problem.
#[cfg(target_os = "windows")]
fn set_clipboard_text(text: &str) -> Result<(), String> {
    use windows::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};

    // The clipboard is shared; a concurrent holder (clipboard manager,
    // another app mid-write) makes OpenClipboard fail — retry briefly
    // before reporting the copy as failed.
    let mut opened = false;
    for _ in 0..5 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !opened {
        return Err("OpenClipboard stayed busy".into());
    }
    let placed = unsafe { place_text_while_open(text) };
    unsafe {
        let _ = CloseClipboard();
    }
    placed
}

/// Caller must hold the open clipboard. On success the GlobalAlloc'd block
/// becomes the system's — it is freed only on the failure paths.
#[cfg(target_os = "windows")]
unsafe fn place_text_while_open(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    // Standard clipboard format ids (the crate only types these in the Ole
    // feature; pulling all of Ole for two constants isn't worth it).
    const CF_UNICODETEXT: u32 = 13;

    EmptyClipboard().map_err(|e| format!("EmptyClipboard: {e}"))?;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();
    let handle = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| format!("GlobalAlloc: {e}"))?;
    let dst = GlobalLock(handle);
    if dst.is_null() {
        let _ = GlobalFree(Some(handle));
        return Err("GlobalLock failed".into());
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), dst.cast::<u8>(), bytes);
    let _ = GlobalUnlock(handle);
    if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(handle.0))) {
        let _ = GlobalFree(Some(handle));
        return Err(format!("SetClipboardData: {e}"));
    }
    Ok(())
}

fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("pbcopy: {e}"))?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| "pbcopy stdin unavailable".to_string())?
            .write_all(text.as_bytes())
            .map_err(|e| format!("pbcopy write: {e}"))?;
        let status = child.wait().map_err(|e| format!("pbcopy wait: {e}"))?;
        if !status.success() {
            return Err("pbcopy failed".into());
        }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        return set_clipboard_text(text);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for (bin, args) in [
            ("wl-copy", &[][..]),
            ("xclip", &["-selection", "clipboard"][..]),
        ] {
            let Ok(mut child) = Command::new(bin).args(args).stdin(Stdio::piped()).spawn() else {
                continue;
            };
            if let Some(stdin) = child.stdin.as_mut() {
                if stdin.write_all(text.as_bytes()).is_err() {
                    continue;
                }
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return Ok(());
            }
        }
        return Err("no clipboard tool (wl-copy / xclip)".into());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let _ = text;
        Err("clipboard unsupported on this platform".into())
    }
}

fn copy_proxy_env(app: &AppHandle<impl TauriRuntime>) {
    let port = app
        .try_state::<AppState>()
        .and_then(|s| s.with_store(|st| Ok(st.settings.mixed_port)).ok())
        .unwrap_or(2080);
    let text = proxy_env_export(port);
    if let Err(e) = copy_text_to_clipboard(&text) {
        eprintln!("[satelite] tray copy env failed: {e}");
    }
}

const TRAY_ID: &str = "main";

/// Handles to tray items whose label/checked state depends on runtime
/// state, kept so `refresh_icon` can re-sync them (menu items expose no
/// getter back from `TrayIcon`, so we hold them ourselves via `app.manage`).
struct CaptureMenuHandles<R: TauriRuntime> {
    off: CheckMenuItem<R>,
    system: CheckMenuItem<R>,
    tun: CheckMenuItem<R>,
}

/// Single start/stop item: label (and action) flips with core run state
/// instead of showing both "启动代理" and "停止代理" at once.
struct ToggleMenuHandle<R: TauriRuntime>(MenuItem<R>);

/// "低内存模式" checkbox: mirrors `settings.unload_ui_on_tray`.
struct LowMemoryMenuHandle<R: TauriRuntime>(CheckMenuItem<R>);

const TOGGLE_START_LABEL: &str = "启动代理";
const TOGGLE_STOP_LABEL: &str = "停止代理";

fn current_capture_mode(app: &AppHandle<impl TauriRuntime>) -> CaptureMode {
    app.try_state::<AppState>()
        .and_then(|s| s.with_store(|st| Ok(st.settings.capture_mode)).ok())
        .unwrap_or_default()
}

/// Re-check the submenu item matching the current capture mode; uncheck the rest.
fn refresh_capture_menu<R: TauriRuntime>(app: &AppHandle<R>) {
    let Some(handles) = app.try_state::<CaptureMenuHandles<R>>() else {
        return;
    };
    let mode = current_capture_mode(app);
    let _ = handles.off.set_checked(mode == CaptureMode::Off);
    let _ = handles.system.set_checked(mode == CaptureMode::System);
    let _ = handles.tun.set_checked(mode == CaptureMode::Tun);
}

/// Re-label the start/stop toggle to match current core run state.
fn refresh_toggle_menu<R: TauriRuntime>(app: &AppHandle<R>, running: bool) {
    let Some(handle) = app.try_state::<ToggleMenuHandle<R>>() else {
        return;
    };
    let label = if running {
        TOGGLE_STOP_LABEL
    } else {
        TOGGLE_START_LABEL
    };
    let _ = handle.0.set_text(label);
}

/// Re-check the "低内存模式" item to match `settings.unload_ui_on_tray`.
fn refresh_low_memory_menu<R: TauriRuntime>(app: &AppHandle<R>) {
    let Some(handle) = app.try_state::<LowMemoryMenuHandle<R>>() else {
        return;
    };
    let enabled = app
        .try_state::<AppState>()
        .and_then(|s| s.with_store(|st| Ok(st.settings.unload_ui_on_tray)).ok())
        .unwrap_or(false);
    let _ = handle.0.set_checked(enabled);
}

fn tray_png(style: TrayIconStyle, running: bool) -> (&'static [u8], bool) {
    match (style, running) {
        (TrayIconStyle::Badge, true) => (include_bytes!("../icons/tray/badge-on.png"), false),
        (TrayIconStyle::Badge, false) => (include_bytes!("../icons/tray/badge-off.png"), false),
        (TrayIconStyle::Mark, true) => (include_bytes!("../icons/tray/mark-on.png"), false),
        // Transparent flat mark: black silhouette, macOS tints for the menu bar.
        (TrayIconStyle::Mark, false) => (include_bytes!("../icons/tray/mark-off.png"), true),
        (TrayIconStyle::Ghost, true) => (include_bytes!("../icons/tray/ghost-on.png"), false),
        (TrayIconStyle::Ghost, false) => (include_bytes!("../icons/tray/ghost-off.png"), false),
        (TrayIconStyle::Buddy, true) => (include_bytes!("../icons/tray/buddy-on.png"), false),
        (TrayIconStyle::Buddy, false) => (include_bytes!("../icons/tray/buddy-off.png"), false),
        (TrayIconStyle::Danger, true) => (include_bytes!("../icons/tray/danger-on.png"), false),
        (TrayIconStyle::Danger, false) => (include_bytes!("../icons/tray/danger-off.png"), false),
        (TrayIconStyle::Danger2, true) => (include_bytes!("../icons/tray/danger2-on.png"), false),
        (TrayIconStyle::Danger2, false) => (include_bytes!("../icons/tray/danger2-off.png"), false),
        (TrayIconStyle::Ghost2, true) => (include_bytes!("../icons/tray/ghost2-on.png"), false),
        (TrayIconStyle::Ghost2, false) => (include_bytes!("../icons/tray/ghost2-off.png"), false),
        (TrayIconStyle::Faceid, true) => (include_bytes!("../icons/tray/faceid-on.png"), false),
        // Black frown silhouette: macOS template tint keeps it visible in
        // both light and dark menu bars.
        (TrayIconStyle::Faceid, false) => (include_bytes!("../icons/tray/faceid-off.png"), true),
    }
}

fn current_style(app: &AppHandle<impl TauriRuntime>) -> TrayIconStyle {
    app.try_state::<AppState>()
        .and_then(|s| s.with_store(|st| Ok(st.settings.tray_icon)).ok())
        .unwrap_or_default()
}

/// Re-select and apply the tray icon based on current core run state.
/// Safe to call frequently; `is_core_running()` falls back to a cached snapshot.
pub fn refresh_icon<R: TauriRuntime>(app: &AppHandle<R>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let running = app
        .try_state::<AppState>()
        .map(|s| s.is_core_running())
        .unwrap_or(false);
    let (bytes, as_template) = tray_png(current_style(app), running);
    let Ok(icon) = Image::from_bytes(bytes) else {
        return;
    };
    let _ = tray.set_icon_with_as_template(Some(icon), as_template);
    refresh_capture_menu(app);
    refresh_toggle_menu(app, running);
    refresh_low_memory_menu(app);
}

pub fn setup_tray<R: TauriRuntime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let show_i = MenuItem::with_id(app, "show", "打开主界面", true, None::<&str>)?;
    let running_now = app
        .try_state::<AppState>()
        .map(|s| s.is_core_running())
        .unwrap_or(false);
    let toggle_label = if running_now {
        TOGGLE_STOP_LABEL
    } else {
        TOGGLE_START_LABEL
    };
    let toggle_i = MenuItem::with_id(app, "toggle", toggle_label, true, None::<&str>)?;
    let restart_i = MenuItem::with_id(app, "restart", "重启内核", true, None::<&str>)?;
    let copy_env_i = MenuItem::with_id(app, "copy_env", "复制环境变量", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    app.manage(ToggleMenuHandle::<R>(toggle_i.clone()));

    let low_memory_now = app
        .try_state::<AppState>()
        .and_then(|s| s.with_store(|st| Ok(st.settings.unload_ui_on_tray)).ok())
        .unwrap_or(false);
    let low_memory_i = CheckMenuItem::with_id(
        app,
        "low_memory",
        "低内存模式",
        true,
        low_memory_now,
        None::<&str>,
    )?;
    app.manage(LowMemoryMenuHandle::<R>(low_memory_i.clone()));

    let mode = current_capture_mode(app);
    let capture_off_i = CheckMenuItem::with_id(
        app,
        "capture_off",
        "关闭",
        true,
        mode == CaptureMode::Off,
        None::<&str>,
    )?;
    let capture_system_i = CheckMenuItem::with_id(
        app,
        "capture_system",
        "系统代理",
        true,
        mode == CaptureMode::System,
        None::<&str>,
    )?;
    let capture_tun_i = CheckMenuItem::with_id(
        app,
        "capture_tun",
        "TUN（全局）",
        true,
        mode == CaptureMode::Tun,
        None::<&str>,
    )?;
    let capture_menu = Submenu::with_id_and_items(
        app,
        "capture",
        "流量接管",
        true,
        &[&capture_off_i, &capture_system_i, &capture_tun_i],
    )?;
    app.manage(CaptureMenuHandles::<R> {
        off: capture_off_i,
        system: capture_system_i,
        tun: capture_tun_i,
    });

    let menu = Menu::with_items(
        app,
        &[
            &show_i,
            &sep,
            &toggle_i,
            &restart_i,
            &capture_menu,
            &copy_env_i,
            &sep,
            &low_memory_i,
            &sep,
            &quit_i,
        ],
    )?;

    // Prefer app icon; fall back to default tray without custom image if load fails.
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Satelite")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                window_ctrl::show_main(app);
            }
            "toggle" => {
                if let Some(state) = app.try_state::<AppState>() {
                    if state.is_core_running() {
                        let _ = state.stop_proxy();
                    } else {
                        let res = app.path().resource_dir().ok();
                        let _ = state.start_proxy(res.as_deref(), true);
                    }
                }
                refresh_icon(app);
            }
            "restart" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let res = app.path().resource_dir().ok();
                    let _ = state.restart_proxy(res.as_deref());
                }
                refresh_icon(app);
            }
            "capture_off" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let res = app.path().resource_dir().ok();
                    let _ = state.set_capture_mode("off", res.as_deref());
                }
                refresh_icon(app);
            }
            "capture_system" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let res = app.path().resource_dir().ok();
                    let _ = state.set_capture_mode("system", res.as_deref());
                }
                refresh_icon(app);
            }
            "capture_tun" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let res = app.path().resource_dir().ok();
                    let _ = state.set_capture_mode("tun", res.as_deref());
                }
                refresh_icon(app);
            }
            "copy_env" => {
                copy_proxy_env(app);
            }
            "low_memory" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let _ = state.with_store_mut(|store| {
                        store.settings.unload_ui_on_tray = !store.settings.unload_ui_on_tray;
                        Ok(())
                    });
                }
                refresh_low_memory_menu(app);
            }
            "quit" => {
                window_ctrl::quit_app(app);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                window_ctrl::show_main(tray.app_handle());
            }
        });

    // Placeholder; refresh_icon applies the user's chosen set + run state.
    let (bytes, as_template) = tray_png(current_style(app), false);
    if let Ok(icon) = Image::from_bytes(bytes) {
        builder = builder.icon(icon).icon_as_template(as_template);
    } else if let Ok(icon) = Image::from_bytes(include_bytes!("../icons/tray-icon.png")) {
        builder = builder.icon(icon);
    } else if let Ok(icon) = Image::from_bytes(include_bytes!("../icons/32x32.png")) {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    refresh_icon(app);
    Ok(())
}
