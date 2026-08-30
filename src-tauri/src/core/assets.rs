//! geosite.dat / geoip.dat asset management for the Xray core.
//!
//! Xray resolves `geosite:` / `geoip:` matchers (and `geoip:private`) through
//! plain `.dat` files located via `XRAY_LOCATION_ASSET` (set to the app-data
//! `bin/` dir by `CoreKind::spawn_env`). Sources, in order:
//! 1. Already staged in app data (`bin/geosite.dat`, `bin/geoip.dat`) —
//!    staged automatically when the bundled Xray is copied in
//!    (`paths::stage_bundled_core`) or when the core zip is downloaded
//!    (`download::extract_from_zip`).
//! 2. Bundled with the app (`resources/bin/<plat>/*.dat`).
//! 3. Network download from Loyalsoldier/v2ray-rules-dat (v2rayN's source).
//!
//! The mihomo core has its own geodata pair (`Country.mmdb` + `geosite.dat`
//! in MetaCubeX .mrs format) living in the mihomo home dir
//! `<app_data>/mihomo/` (see `paths::mihomo_home_dir`) — separate from
//! `bin/` because the two cores' `geosite.dat` files share a name but not a
//! format.

use crate::error::{AppError, AppResult};
use std::io::Read;
use std::path::Path;

const GEODATA_FILES: [&str; 2] = ["geosite.dat", "geoip.dat"];
const GEODATA_BASE_URL: &str =
    "https://github.com/Loyalsoldier/v2ray-rules-dat/releases/latest/download";
/// dat files are ~10-60 MB each; anything larger is a corrupt/hijacked body.
const MAX_DAT_BYTES: u64 = 128 * 1024 * 1024;

pub fn geodata_present(app_data_dir: &Path) -> bool {
    let bin = crate::core::paths::core_dir(app_data_dir);
    GEODATA_FILES.iter().all(|f| bin.join(f).is_file())
}

/// Stage missing dat files from the bundled resources. Returns true when all
/// files are present afterwards.
pub fn stage_bundled_geodata(app_data_dir: &Path, resource_dir: Option<&Path>) -> bool {
    let bin = crate::core::paths::core_dir(app_data_dir);
    let Some(resource_dir) = resource_dir else {
        return geodata_present(app_data_dir);
    };
    // Local layout uses the shared platform directory (sing-box naming).
    let platform = crate::core::paths::detect_platform()
        .map(|p| p.asset_suffix)
        .unwrap_or("windows-amd64");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bundled_dirs = [
        manifest.join("resources/bin").join(platform),
        resource_dir.join("resources/bin").join(platform),
        resource_dir.join("bin").join(platform),
        resource_dir.join(platform),
    ];
    for file in GEODATA_FILES {
        let dest = bin.join(file);
        if dest.is_file() {
            continue;
        }
        for dir in &bundled_dirs {
            let src = dir.join(file);
            if src.is_file() {
                let _ = std::fs::create_dir_all(&bin);
                if std::fs::copy(&src, &dest).is_ok() {
                    break;
                }
            }
        }
    }
    geodata_present(app_data_dir)
}

/// Download missing dat files (sync; ureq — safe inside blocking workers).
/// `proxy_url` mirrors the core-download routing (local mixed port when up).
/// `force` re-downloads even when the file already exists (manual refresh).
pub fn download_missing_geodata(
    app_data_dir: &Path,
    proxy_url: Option<&str>,
    force: bool,
) -> AppResult<()> {
    let bin = crate::core::paths::core_dir(app_data_dir);
    std::fs::create_dir_all(&bin)?;
    for file in GEODATA_FILES {
        let dest = bin.join(file);
        if dest.is_file() && !force {
            continue;
        }
        let url = format!("{GEODATA_BASE_URL}/{file}");
        let mut builder = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(120));
        if let Some(proxy) = proxy_url {
            let proxy = ureq::Proxy::new(proxy)
                .map_err(|e| AppError::Core(format!("geodata proxy: {e}")))?;
            builder = builder.proxy(proxy);
        }
        let agent = builder.build();
        let resp = agent
            .get(&url)
            .call()
            .map_err(|e| AppError::Core(format!("geodata download {file}: {e}")))?;
        let len = resp
            .header("content-length")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        if len > MAX_DAT_BYTES {
            return Err(AppError::Core(format!(
                "geodata {file} exceeds {MAX_DAT_BYTES} bytes"
            )));
        }
        let mut bytes = Vec::new();
        resp.into_reader()
            .take(MAX_DAT_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| AppError::Core(format!("geodata read {file}: {e}")))?;
        // Sanity: dat files are protobuf containers, never tiny.
        if bytes.len() < 1024 {
            return Err(AppError::Core(format!(
                "geodata {file} too small ({} bytes), likely failed",
                bytes.len()
            )));
        }
        let staged = dest.with_extension("dat.part");
        std::fs::write(&staged, &bytes)?;
        std::fs::rename(&staged, &dest)?;
        crate::app_log::info(
            "geodata",
            format!("downloaded {file} ({} bytes)", bytes.len()),
        );
    }
    Ok(())
}

/// Full ensure chain used before starting Xray: staged → bundled → network.
pub fn ensure_geodata(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    proxy_url: Option<&str>,
) -> AppResult<()> {
    if geodata_present(app_data_dir) {
        return Ok(());
    }
    if stage_bundled_geodata(app_data_dir, resource_dir) {
        return Ok(());
    }
    download_missing_geodata(app_data_dir, proxy_url, false)?;
    if !geodata_present(app_data_dir) {
        return Err(AppError::Core(
            "geosite.dat / geoip.dat missing — geosite:/geoip: rules cannot load".into(),
        ));
    }
    Ok(())
}

/// Current state of one staged geodata file, for the Rules UI card.
#[derive(Debug, Clone, Copy)]
pub struct GeodataFileState {
    pub present: bool,
    pub bytes: u64,
    /// Unix seconds of the file's last modification.
    pub modified_at: Option<u64>,
}

pub fn geodata_state(app_data_dir: &Path) -> [(&'static str, GeodataFileState); 2] {
    let bin = crate::core::paths::core_dir(app_data_dir);
    let mut out = Vec::new();
    for file in GEODATA_FILES {
        let path = bin.join(file);
        let state = match std::fs::metadata(&path) {
            Ok(meta) => GeodataFileState {
                present: true,
                bytes: meta.len(),
                modified_at: meta.modified().ok().and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs())
                }),
            },
            Err(_) => GeodataFileState {
                present: false,
                bytes: 0,
                modified_at: None,
            },
        };
        out.push((file, state));
    }
    match out.try_into() {
        Ok(arr) => arr,
        Err(_) => unreachable!("GEODATA_FILES has exactly 2 entries"),
    }
}

// ---------------------------------------------------------------------------
// mihomo geodata: Country.mmdb (MaxMind) + GeoSite.dat (MetaCubeX mrs —
// note the exact casing, macOS is case-sensitive) in <data>/mihomo/.
// ---------------------------------------------------------------------------

const MIHOMO_GEODATA_FILES: [&str; 2] = ["Country.mmdb", "GeoSite.dat"];
/// Same source mihomo itself downloads from (its auto-update default).
const MIHOMO_GEODATA_BASE_URL: &str =
    "https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download";

pub fn mihomo_home(app_data_dir: &Path) -> std::path::PathBuf {
    crate::core::paths::mihomo_home_dir(app_data_dir)
}

pub fn mihomo_geodata_present(app_data_dir: &Path) -> bool {
    let home = mihomo_home(app_data_dir);
    MIHOMO_GEODATA_FILES.iter().all(|f| home.join(f).is_file())
}

/// Stage missing mihomo geodata from an explicit bundled dir
/// (`resources/bin/<plat>/mihomo-geodata`). Returns true when all files are
/// present afterwards.
pub fn stage_bundled_mihomo_geodata_from(app_data_dir: &Path, bundled_dir: &Path) -> bool {
    let home = mihomo_home(app_data_dir);
    for file in MIHOMO_GEODATA_FILES {
        let dest = home.join(file);
        if dest.is_file() {
            continue;
        }
        let src = bundled_dir.join(file);
        if src.is_file() {
            let _ = std::fs::create_dir_all(&home);
            if std::fs::copy(&src, &dest).is_ok() {
                continue;
            }
        }
        return false;
    }
    true
}

/// Stage missing mihomo geodata from the bundled resources layout.
pub fn stage_bundled_mihomo_geodata(app_data_dir: &Path, resource_dir: Option<&Path>) -> bool {
    if mihomo_geodata_present(app_data_dir) {
        return true;
    }
    let Some(resource_dir) = resource_dir else {
        return false;
    };
    let platform = crate::core::paths::detect_platform()
        .map(|p| p.asset_suffix)
        .unwrap_or("windows-amd64");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in [
        manifest.join("resources/bin").join(platform),
        resource_dir.join("resources/bin").join(platform),
        resource_dir.join("bin").join(platform),
        resource_dir.join(platform),
    ] {
        if stage_bundled_mihomo_geodata_from(app_data_dir, &dir.join("mihomo-geodata")) {
            return true;
        }
    }
    false
}

/// Download missing mihomo geodata (sync; ureq). `force` re-downloads for the
/// manual refresh action in the Rules UI.
pub fn download_missing_mihomo_geodata(
    app_data_dir: &Path,
    proxy_url: Option<&str>,
    force: bool,
) -> AppResult<()> {
    let home = mihomo_home(app_data_dir);
    std::fs::create_dir_all(&home)?;
    for file in MIHOMO_GEODATA_FILES {
        let dest = home.join(file);
        if dest.is_file() && !force {
            continue;
        }
        let url = format!("{MIHOMO_GEODATA_BASE_URL}/{file}");
        // 30s: this download sits inside the core-start path holding the
        // store/runtime locks. 120s × 2 files let a blocked GitHub route
        // (CN direct) stall the restart for minutes with the UI busy.
        // The files are ~8MB/~4MB — 30s is ample on any working route.
        let mut builder = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30));
        if let Some(proxy) = proxy_url {
            let proxy = ureq::Proxy::new(proxy)
                .map_err(|e| AppError::Core(format!("mihomo geodata proxy: {e}")))?;
            builder = builder.proxy(proxy);
        }
        let resp = builder
            .build()
            .get(&url)
            .call()
            .map_err(|e| AppError::Core(format!("mihomo geodata download {file}: {e}")))?;
        let len = resp
            .header("content-length")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        if len > MAX_DAT_BYTES {
            return Err(AppError::Core(format!(
                "mihomo geodata {file} exceeds {MAX_DAT_BYTES} bytes"
            )));
        }
        let mut bytes = Vec::new();
        resp.into_reader()
            .take(MAX_DAT_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| AppError::Core(format!("mihomo geodata read {file}: {e}")))?;
        // Sanity: mmdb is ~8 MB, mrs geosite ~4 MB; never tiny.
        if bytes.len() < 1024 {
            return Err(AppError::Core(format!(
                "mihomo geodata {file} too small ({} bytes), likely failed",
                bytes.len()
            )));
        }
        let staged = dest.with_extension("part");
        std::fs::write(&staged, &bytes)?;
        std::fs::rename(&staged, &dest)?;
        crate::app_log::info(
            "geodata",
            format!("mihomo: downloaded {file} ({} bytes)", bytes.len()),
        );
    }
    Ok(())
}

/// Full ensure chain used before starting mihomo: staged → bundled → network.
/// Missing geodata is a hard start failure in mihomo (GEOIP/GEOSITE rules make
/// the process exit), so unlike Xray this returns an error naming the files.
pub fn ensure_mihomo_geodata(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    proxy_url: Option<&str>,
) -> AppResult<()> {
    if mihomo_geodata_present(app_data_dir) {
        return Ok(());
    }
    if stage_bundled_mihomo_geodata(app_data_dir, resource_dir) {
        return Ok(());
    }
    download_missing_mihomo_geodata(app_data_dir, proxy_url, false)?;
    if !mihomo_geodata_present(app_data_dir) {
        return Err(AppError::Core(
            "mihomo geodata (Country.mmdb / geosite.dat) missing — GEOIP/GEOSITE rules cannot load"
                .into(),
        ));
    }
    Ok(())
}

pub fn mihomo_geodata_state(app_data_dir: &Path) -> [(&'static str, GeodataFileState); 2] {
    let home = mihomo_home(app_data_dir);
    let mut out = Vec::new();
    for file in MIHOMO_GEODATA_FILES {
        let path = home.join(file);
        let state = match std::fs::metadata(&path) {
            Ok(meta) => GeodataFileState {
                present: true,
                bytes: meta.len(),
                modified_at: meta.modified().ok().and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs())
                }),
            },
            Err(_) => GeodataFileState {
                present: false,
                bytes: 0,
                modified_at: None,
            },
        };
        out.push((file, state));
    }
    match out.try_into() {
        Ok(arr) => arr,
        Err(_) => unreachable!("MIHOMO_GEODATA_FILES has exactly 2 entries"),
    }
}

// ---------------------------------------------------------------------------
// Post-download prefetch: fetch a core's runtime assets right after the core
// itself is installed (settings → core download/update), so the first start
// doesn't discover-and-download them while holding the store/runtime locks
// (AGENTS.md §9.22).
// ---------------------------------------------------------------------------

/// Fetch whatever network assets `kind` needs at start time. Best-effort:
/// returns one warning string per failed asset; the startup `ensure_*`
/// calls remain as the fallback when something fails here.
pub fn prefetch_runtime_assets(
    kind: crate::core::CoreKind,
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    proxy_url: Option<&str>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    match kind {
        crate::core::CoreKind::Xray => {
            if let Err(error) = ensure_geodata(app_data_dir, resource_dir, proxy_url) {
                warnings.push(format!("xray geodata prefetch failed: {error}"));
            }
        }
        crate::core::CoreKind::Mihomo => {
            if let Err(error) = download_missing_mihomo_geodata(app_data_dir, proxy_url, false) {
                warnings.push(format!("mihomo geodata prefetch failed: {error}"));
            }
        }
        crate::core::CoreKind::SingBox => {}
    }
    // wintun.dll (Windows TUN) is shared by Xray and mihomo — prefetch it for
    // both so enabling TUN never triggers a download on core start.
    #[cfg(target_os = "windows")]
    if matches!(
        kind,
        crate::core::CoreKind::Xray | crate::core::CoreKind::Mihomo
    ) {
        if let Err(error) = ensure_wintun(app_data_dir, resource_dir, proxy_url) {
            warnings.push(format!("wintun prefetch failed: {error}"));
        }
    }
    warnings
}

/// Xray's native tun inbound loads wintun.dll on Windows (not shipped in the
/// Xray release zip). Ensure it sits next to the core binary in `bin/`.
#[cfg(target_os = "windows")]
pub fn ensure_wintun(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    proxy_url: Option<&str>,
) -> AppResult<()> {
    const WINTUN_URL: &str = "https://www.wintun.net/builds/wintun-0.14.1.zip";
    let bin = crate::core::paths::core_dir(app_data_dir);
    let dest = bin.join("wintun.dll");
    if dest.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(&bin)?;

    // 1. staged alongside a staged xray.exe (paths::stage_bundled_core) —
    //    nothing to do; 2. bundled resources dir; 3. network download.
    if let Some(resource_dir) = resource_dir {
        // Local layout uses the shared platform directory (sing-box naming).
        let suffix = crate::core::paths::detect_platform()
            .map(|p| p.asset_suffix)
            .unwrap_or("windows-amd64");
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for dir in [
            manifest.join("resources/bin").join(suffix),
            resource_dir.join("resources/bin").join(suffix),
            resource_dir.join("bin").join(suffix),
            resource_dir.join(suffix),
        ] {
            let src = dir.join("wintun.dll");
            if src.is_file() && std::fs::copy(&src, &dest).is_ok() {
                return Ok(());
            }
        }
    }

    let mut builder = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(120));
    if let Some(proxy) = proxy_url {
        let proxy =
            ureq::Proxy::new(proxy).map_err(|e| AppError::Core(format!("wintun proxy: {e}")))?;
        builder = builder.proxy(proxy);
    }
    let resp = builder
        .build()
        .get(WINTUN_URL)
        .call()
        .map_err(|e| AppError::Core(format!("wintun download: {e}")))?;
    let mut zip_bytes = Vec::new();
    resp.into_reader()
        .take(MAX_DAT_BYTES)
        .read_to_end(&mut zip_bytes)
        .map_err(|e| AppError::Core(format!("wintun read: {e}")))?;
    let staged = dest.with_extension("part");
    extract_wintun_amd64(&zip_bytes, &staged)?;
    std::fs::rename(&staged, &dest)?;
    Ok(())
}

/// Pull `wintun/bin/amd64/wintun.dll` out of the official wintun.zip.
#[cfg(target_os = "windows")]
fn extract_wintun_amd64(zip_bytes: &[u8], dest: &Path) -> AppResult<()> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| AppError::Core(format!("wintun zip open: {e}")))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::Core(format!("wintun zip entry: {e}")))?;
        if entry.name().replace('\\', "/") != "wintun/bin/amd64/wintun.dll" {
            continue;
        }
        let mut out = std::fs::File::create(dest)
            .map_err(|e| AppError::Core(format!("create wintun.dll: {e}")))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| AppError::Core(format!("extract wintun.dll: {e}")))?;
        return Ok(());
    }
    Err(AppError::Core(
        "wintun/bin/amd64/wintun.dll not found in archive".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dir_is_not_geodata_present() {
        let dir = std::env::temp_dir().join(format!(
            "satelite-geodata-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!geodata_present(&dir));
        // staging from nothing changes nothing
        assert!(!stage_bundled_geodata(&dir, None));
        assert!(!geodata_present(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stage_from_fake_bundled_dir() {
        let root = std::env::temp_dir().join(format!(
            "satelite-geodata-stage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let app_data = root.join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        // Fake a bundled platform dir layout: resources/bin/<suffix> (shared
        // sing-box-style platform naming for both cores).
        let resource_root = root.join("res");
        let suffix = crate::core::paths::detect_platform().unwrap().asset_suffix;
        let bundled = resource_root.join("bin").join(suffix);
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join("geosite.dat"), b"fake-geosite").unwrap();
        std::fs::write(bundled.join("geoip.dat"), b"fake-geoip").unwrap();

        assert!(stage_bundled_geodata(&app_data, Some(&resource_root)));
        assert!(geodata_present(&app_data));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mihomo_geodata_stages_into_home_dir() {
        let root = std::env::temp_dir().join(format!(
            "satelite-mihomo-geodata-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let app_data = root.join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        assert!(!mihomo_geodata_present(&app_data));
        assert!(!stage_bundled_mihomo_geodata(&app_data, None));

        let resource_root = root.join("res");
        let suffix = crate::core::paths::detect_platform().unwrap().asset_suffix;
        let bundled = resource_root
            .join("bin")
            .join(suffix)
            .join("mihomo-geodata");
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join("Country.mmdb"), b"fake-mmdb").unwrap();
        std::fs::write(bundled.join("geosite.dat"), b"fake-mrs").unwrap();

        assert!(stage_bundled_mihomo_geodata(
            &app_data,
            Some(&resource_root)
        ));
        assert!(mihomo_geodata_present(&app_data));
        // Lives in the mihomo home dir, never in bin/ (name collision with
        // Xray's v2ray-format geosite.dat).
        let home = mihomo_home(&app_data);
        assert!(home.join("Country.mmdb").is_file());
        assert!(home.join("geosite.dat").is_file());
        assert!(!crate::core::paths::core_dir(&app_data)
            .join("Country.mmdb")
            .is_file());
        let _ = std::fs::remove_dir_all(root);
    }
}
