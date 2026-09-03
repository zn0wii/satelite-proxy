//! Main window show / hide / destroy for tray memory management.
//!
//! Destroying the last WebView triggers Tauri `ExitRequested`. Callers must
//! keep `AppState::exit_allowed == false` so the run loop calls `prevent_exit`
//! and tray + sing-box stay alive.

use crate::state::AppState;
use std::fs;
use std::path::PathBuf;
use tauri::window::Color;
use tauri::{AppHandle, Manager, Runtime, Theme, WebviewUrl, WebviewWindowBuilder};

/// Matches frontend `windowLayout.ts` (logical px).
const PRO_SIZE: (f64, f64) = (960.0, 720.0);
const SIMPLE_SIZE: (f64, f64) = (420.0, 720.0);
/// Simple mode lets the user shrink the window; content scrolls below this.
const SIMPLE_MIN: (f64, f64) = (320.0, 480.0);
/// …but never grow past the default simple strip.
const SIMPLE_MAX: (f64, f64) = SIMPLE_SIZE;

fn ui_mode_file(app_data_dir: &std::path::Path) -> PathBuf {
    app_data_dir.join("data").join("ui_mode")
}

/// Persist UI mode so the next WebView recreate uses the correct window size.
pub fn write_ui_mode(app_data_dir: &std::path::Path, mode: &str) {
    let path = ui_mode_file(app_data_dir);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let v = match mode.trim().to_ascii_lowercase().as_str() {
        "simple" => "simple",
        _ => "pro",
    };
    let _ = fs::write(path, v);
}

pub fn read_ui_mode(app_data_dir: &std::path::Path) -> &'static str {
    let path = ui_mode_file(app_data_dir);
    match fs::read_to_string(path) {
        Ok(s) if s.trim().eq_ignore_ascii_case("simple") => "simple",
        _ => "pro",
    }
}

fn size_for_ui_mode(mode: &str) -> (f64, f64) {
    if mode == "simple" {
        SIMPLE_SIZE
    } else {
        PRO_SIZE
    }
}

/// Native window background per theme — covers the gap between WebView
/// creation and the first HTML/CSS paint on the recreate-from-tray path.
/// Values mirror App.css `--bg` per theme (kept in sync by hand).
const BG_AEROSPACE: (u8, u8, u8) = (0x11, 0x14, 0x1c);
const BG_DAY: (u8, u8, u8) = (0xee, 0xf0, 0xf4);

/// Accent presets mirrored from `src/theme/accents.ts` `ACCENTS` — (aerospace, day)
/// hex per preset id. Kept in sync by hand; only the id lookup matters here.
#[cfg(target_os = "windows")]
const ACCENT_PRESETS: &[(&str, (u8, u8, u8), (u8, u8, u8))] = &[
    ("green", (0x55, 0xc8, 0x9a), (0x1f, 0x9a, 0x72)),
    ("blue", (0x6b, 0xb6, 0xe8), (0x2e, 0x86, 0xc8)),
    ("purple", (0xb1, 0x9c, 0xd9), (0x8e, 0x5b, 0xb8)),
    ("pink", (0xf4, 0xa6, 0xb8), (0xd6, 0x5a, 0x7e)),
    ("orange", (0xf5, 0xb9, 0x7a), (0xd8, 0x8a, 0x3d)),
    ("cyan", (0x7a, 0xd7, 0xd7), (0x2f, 0xa9, 0xa9)),
];
#[cfg(target_os = "windows")]
const DEFAULT_ACCENT_RGB: (u8, u8, u8) = (0x55, 0xc8, 0x9a); // "green", aerospace shade

/// hero-glow alpha per theme — mirrors App.css `--hero-glow` (`rgba(var(--glow-rgb), a)`).
#[cfg(target_os = "windows")]
const HERO_GLOW_ALPHA_DARK: f64 = 0.12;
#[cfg(target_os = "windows")]
const HERO_GLOW_ALPHA_LIGHT: f64 = 0.10;

fn is_dark_theme<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<AppState>()
        .and_then(|s| {
            s.with_store(|st| Ok(st.settings.theme.trim().eq_ignore_ascii_case("aerospace")))
                .ok()
        })
        .unwrap_or(false)
}

fn theme_bg_color<R: Runtime>(app: &AppHandle<R>) -> Color {
    let (r, g, b) = if is_dark_theme(app) { BG_AEROSPACE } else { BG_DAY };
    Color(r, g, b, 255)
}

/// Resolve a stored glow/accent id to its rgb for the given theme darkness.
/// Mirrors `resolveAccent()` in accents.ts: a `#rrggbb` custom hex is used
/// verbatim for both themes; otherwise look up the preset table and fall
/// back to the default ("green") shade.
#[cfg(target_os = "windows")]
fn resolve_glow_rgb(id: &str, dark: bool) -> (u8, u8, u8) {
    let id = id.trim();
    if id.len() == 7 && id.starts_with('#') {
        if let Ok(n) = u32::from_str_radix(&id[1..], 16) {
            return (((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8, (n & 0xff) as u8);
        }
    }
    ACCENT_PRESETS
        .iter()
        .find(|(pid, _, _)| *pid == id)
        .map(|(_, dark_rgb, light_rgb)| if dark { *dark_rgb } else { *light_rgb })
        .unwrap_or(DEFAULT_ACCENT_RGB)
}

/// Blend `bg` and `glow` at `alpha` (straight-alpha over), matching how
/// `--hero-glow`'s radial gradient reads at its center — the same tint that
/// washes the dashboard hero. Used as a flat Windows caption color so the
/// title bar reads as "hero region, sampled".
#[cfg(target_os = "windows")]
fn blend_over(bg: (u8, u8, u8), glow: (u8, u8, u8), alpha: f64) -> (u8, u8, u8) {
    let mix = |b: u8, g: u8| -> u8 {
        (b as f64 * (1.0 - alpha) + g as f64 * alpha).round() as u8
    };
    (mix(bg.0, glow.0), mix(bg.1, glow.1), mix(bg.2, glow.2))
}

/// Title bar tint: theme background blended with the active glow color at
/// the same alpha as the dashboard hero glow (see `--hero-glow` in App.css).
/// `glow_color` may be `"accent"` (follow the UI accent), a preset id, or a
/// custom `#rrggbb` — mirrors `applyGlowToDom()` in accents.ts.
#[cfg(target_os = "windows")]
fn titlebar_accent_color<R: Runtime>(app: &AppHandle<R>) -> (u8, u8, u8) {
    let dark = is_dark_theme(app);
    let (glow_id, accent_id) = app
        .try_state::<AppState>()
        .and_then(|s| {
            s.with_store(|st| {
                Ok((st.settings.glow_color.clone(), st.settings.accent.clone()))
            })
            .ok()
        })
        .unwrap_or_else(|| ("accent".to_string(), "green".to_string()));
    let effective_id = if glow_id.trim() == "accent" { accent_id } else { glow_id };
    let glow_rgb = resolve_glow_rgb(&effective_id, dark);
    let bg = if dark { BG_AEROSPACE } else { BG_DAY };
    let alpha = if dark { HERO_GLOW_ALPHA_DARK } else { HERO_GLOW_ALPHA_LIGHT };
    blend_over(bg, glow_rgb, alpha)
}

/// Windows 11 (build 22000+) only: tint the native title bar caption to match
/// the dashboard hero glow. `DWMWA_CAPTION_COLOR` is unsupported on Windows 10
/// and earlier Win11 builds — the call fails silently there and the title bar
/// keeps the OS default light/dark chrome (see `set_theme` above).
#[cfg(target_os = "windows")]
fn apply_titlebar_accent<R: Runtime>(app: &AppHandle<R>) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR};

    let Some(w) = app.get_webview_window("main") else { return };
    let Ok(hwnd) = w.hwnd() else { return };
    let (r, g, b) = titlebar_accent_color(app);
    let colorref = COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32);
    unsafe {
        let _ = DwmSetWindowAttribute(
            HWND(hwnd.0),
            DWMWA_CAPTION_COLOR,
            &colorref as *const _ as *const _,
            std::mem::size_of::<COLORREF>() as u32,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_titlebar_accent<R: Runtime>(_app: &AppHandle<R>) {}

/// Pin the native window chrome (macOS title bar, Windows caption) to the
/// app's own theme setting instead of letting it drift with the OS
/// light/dark mode. Without this, wry infers the window appearance from the
/// WebView's `color-scheme` at whatever point the CSS happens to apply,
/// which raced with WebView (re)creation and left the title bar
/// inconsistent between a fresh launch and a tray-recreated window.
pub fn apply_window_theme<R: Runtime>(app: &AppHandle<R>) {
    let theme = if is_dark_theme(app) {
        Theme::Dark
    } else {
        Theme::Light
    };
    if let Some(w) = app.get_webview_window("main") {
        if let Err(e) = w.set_theme(Some(theme)) {
            eprintln!("[satelite] set_theme failed: {e}");
        }
    }
    apply_titlebar_accent(app);
}

/// macOS: show Dock icon (foreground app). No-op on other platforms.
#[cfg(target_os = "macos")]
pub fn set_dock_visible<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    let policy = if visible {
        tauri::ActivationPolicy::Regular
    } else {
        // Accessory ≈ menu-bar / tray-only; Dock icon is hidden.
        tauri::ActivationPolicy::Accessory
    };
    if let Err(e) = app.set_activation_policy(policy) {
        eprintln!("[satelite] set_activation_policy failed: {e}");
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_visible<R: Runtime>(_app: &AppHandle<R>, _visible: bool) {}

/// Show main UI; recreate WebView if it was destroyed on tray.
///
/// Called from tray menu/click and from macOS Dock reopen (`RunEvent::Reopen`).
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    // Restore Dock icon before showing so the window can become key.
    set_dock_visible(app, true);

    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_theme(Some(if is_dark_theme(app) {
            Theme::Dark
        } else {
            Theme::Light
        }));
        apply_titlebar_accent(app);
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    } else {
        // Use last persisted UI mode so we don't flash pro (960) then shrink to simple.
        let mode = app
            .try_state::<AppState>()
            .map(|s| read_ui_mode(&s.app_data_dir).to_string())
            .unwrap_or_else(|| "pro".into());
        let (w, h) = size_for_ui_mode(&mode);
        let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("Satelite")
            .inner_size(w, h)
            .fullscreen(false)
            // Native底色 follows the stored theme so the recreated window
            // never flashes white before the inline CSS lands.
            .background_color(theme_bg_color(app))
            // Pin the title bar to the app theme instead of the OS
            // light/dark mode (see apply_window_theme's doc comment).
            .theme(Some(if is_dark_theme(app) {
                Theme::Dark
            } else {
                Theme::Light
            }))
            // Important on macOS: without activation policy / visible, Dock reopen
            // can recreate a window that never becomes key.
            .visible(true)
            .focused(true);
        // Portable: keep the WebView2 profile next to the exe — otherwise the
        // recreated webview silently spawns a second profile in %LOCALAPPDATA%.
        let builder = match crate::portable::webview_data_dir() {
            Some(dir) => builder.data_directory(dir),
            None => builder,
        };
        // Simple mode: user-resizable strip, shrink-only (frontend restores size).
        let builder = if mode == "simple" {
            builder
                .resizable(true)
                .min_inner_size(SIMPLE_MIN.0, SIMPLE_MIN.1)
                .max_inner_size(SIMPLE_MAX.0, SIMPLE_MAX.1)
        } else {
            builder.resizable(false)
        };
        match builder.build() {
            Ok(win) => {
                apply_titlebar_accent(app);
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
            Err(e) => eprintln!("[satelite] recreate main window failed: {e}"),
        }
    }
    if let Some(state) = app.try_state::<AppState>() {
        state.set_ui_visible(true);
    }
}

/// Soft-hide only (keep WebView process). Safe at app launch for silent_start.
pub fn soft_hide_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AppState>() {
        state.set_ui_visible(false);
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    // Silent / tray-only: hide Dock icon on macOS.
    set_dock_visible(app, false);
}

/// Hide to tray. Optionally destroy WebView (low-memory mode).
/// Default is hide-only; destroy is opt-in via `unload_ui_on_tray`.
/// Does **not** allow process exit — tray and core keep running.
pub fn hide_main_to_tray<R: Runtime>(app: &AppHandle<R>) {
    let unload = app
        .try_state::<AppState>()
        .map(|s| s.unload_ui_on_tray())
        .unwrap_or(false);

    if let Some(state) = app.try_state::<AppState>() {
        state.set_ui_visible(false);
        // Critical: destroy() may fire ExitRequested; stay alive unless tray Quit.
        // exit_allowed stays false.
    }

    // Hide Dock icon before (or with) hide — matches close-to-tray-and-dock.md.
    set_dock_visible(app, false);

    if let Some(w) = app.get_webview_window("main") {
        if unload {
            // hide first so user doesn't see a flash; then drop WKWebView
            let _ = w.hide();
            if let Err(e) = w.destroy() {
                eprintln!("[satelite] destroy main window: {e}");
                // fallback: already hidden
            }
        } else {
            let _ = w.hide();
        }
    }
}

/// Explicit full quit: allow exit, stop core, exit process.
pub fn quit_app<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AppState>() {
        state.allow_exit();
        state.shutdown_runtime();
    }
    app.exit(0);
}
