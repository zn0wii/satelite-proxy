mod assets;
mod download;
#[cfg(target_os = "windows")]
mod elevate;
#[cfg(target_os = "windows")]
mod job;
pub mod kind;
#[cfg(target_os = "macos")]
mod macos_auth;
#[cfg(target_os = "macos")]
pub mod macos_net;
#[cfg(target_os = "macos")]
pub use macos_auth::{core_has_setuid, ensure_core_setuid};
pub mod manager;
mod memory;
mod paths;

pub use assets::ensure_geodata;
#[cfg(target_os = "windows")]
pub use assets::ensure_wintun;
pub use assets::{download_missing_geodata, geodata_state};
pub use assets::{download_missing_mihomo_geodata, ensure_mihomo_geodata, mihomo_geodata_state};
pub use assets::prefetch_runtime_assets;
pub use kind::CoreKind;
pub use memory::read_process_mem_info;
pub use memory::ProcessMemInfo;

pub use download::{
    download_latest_core, download_latest_core_with_progress, fetch_latest_app_tag,
    fetch_latest_app_tag_via_redirect, fetch_latest_release_with_proxy, CoreDownloadProgress,
    CoreDownloadResult, LatestReleaseInfo,
};
#[cfg(test)]
pub use paths::find_bundled_core;
pub use paths::{
    active_core_version, bundled_core_version, detect_platform, inspect_core_bin, resolve_core_bin,
    CoreSource,
};
