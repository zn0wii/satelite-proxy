// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    apply_webkitgtk_workarounds();
    satelite_proxy_lib::run()
}

// WebKitGTK ≥2.42 的 DMA-BUF 渲染器在部分 Wayland/Mesa/NVIDIA 栈上创建 surfaceless EGL
// display 会得到 EGL_BAD_ALLOC 并在 Web 进程里直接 abort（应用闪退、日志仅剩该行）。
// 默认退回传统渲染路径；用户已显式设置同名变量时不覆盖，留出手动调参出口。
#[cfg(target_os = "linux")]
fn apply_webkitgtk_workarounds() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}
