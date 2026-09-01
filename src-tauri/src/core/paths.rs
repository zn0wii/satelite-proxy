use crate::core::kind::CoreKind;
use crate::error::{AppError, AppResult};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorePlatform {
    /// sing-box release asset suffix, e.g. darwin-arm64, windows-amd64
    pub asset_suffix: &'static str,
    /// Xray release asset suffix, e.g. macos-arm64-v8a, windows-64
    pub xray_asset_suffix: &'static str,
    /// mihomo release asset suffix — same short form as sing-box (e.g.
    /// darwin-arm64, windows-amd64), not a Rust target triple.
    pub mihomo_asset_suffix: &'static str,
    pub is_windows: bool,
}

impl CorePlatform {
    /// Asset suffix for a core kind's release naming scheme.
    pub fn asset_suffix_for(self, kind: CoreKind) -> &'static str {
        match kind {
            CoreKind::SingBox => self.asset_suffix,
            CoreKind::Xray => self.xray_asset_suffix,
            CoreKind::Mihomo => self.mihomo_asset_suffix,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreSource {
    /// User-downloaded under app data
    Downloaded,
    /// Bundled with the app package / repo resources
    Bundled,
    Missing,
}

pub fn detect_platform() -> AppResult<CorePlatform> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let (suffix, xray_suffix, is_windows) = match (os, arch) {
        ("macos", "aarch64") => ("darwin-arm64", "macos-arm64-v8a", false),
        ("macos", "x86_64") => ("darwin-amd64", "macos-64", false),
        ("linux", "aarch64") => ("linux-arm64", "linux-arm64-v8a", false),
        ("linux", "x86_64") => ("linux-amd64", "linux-64", false),
        ("windows", "x86_64") => ("windows-amd64", "windows-64", true),
        ("windows", "aarch64") => ("windows-arm64", "windows-arm64-v8a", true),
        _ => {
            return Err(AppError::Core(format!("unsupported platform: {os}/{arch}")));
        }
    };
    Ok(CorePlatform {
        asset_suffix: suffix,
        xray_asset_suffix: xray_suffix,
        // mihomo release assets use the same short suffix as sing-box.
        mihomo_asset_suffix: suffix,
        is_windows,
    })
}

pub fn core_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("bin")
}

/// mihomo home directory (`-d`): hosts its `Country.mmdb` + `geosite.dat`
/// geodata. Kept separate from `bin/` because mihomo's `geosite.dat` is
/// MetaCubeX .mrs format and would collide with Xray's v2ray-format
/// `bin/geosite.dat` of the same name.
pub fn mihomo_home_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("mihomo")
}

/// User-managed binary path (download / update target).
pub fn core_bin_path(app_data_dir: &Path, kind: CoreKind) -> PathBuf {
    core_dir(app_data_dir).join(kind.binary_name())
}

pub fn version_file_path(app_data_dir: &Path, kind: CoreKind) -> PathBuf {
    core_dir(app_data_dir).join(kind.version_file_name())
}

/// Absolute path candidates for the built-in binary (dev + packaging).
/// The local layout uses one platform directory per OS/arch (the sing-box
/// asset naming, e.g. `windows-amd64`) shared by both cores — the per-kind
/// `asset_suffix_for` naming only applies to GitHub release assets.
pub fn bundled_core_candidates(resource_dir: Option<&Path>, kind: CoreKind) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let bin = kind.binary_name();
    let plat = detect_platform()
        .map(|p| p.asset_suffix)
        .unwrap_or("darwin-arm64");

    // Dev source tree first: running from target/debug/resources can be SIGKILL'd on macOS.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    out.push(manifest.join("resources/bin").join(plat).join(bin));

    if let Some(res) = resource_dir {
        // Tauri resource root layouts (varies by OS / config)
        out.push(res.join("resources/bin").join(plat).join(bin));
        out.push(res.join("bin").join(plat).join(bin));
        out.push(res.join(plat).join(bin));
        out.push(res.join(bin));
    }

    out
}

/// Paths under `target/{debug,release}/…` — same bytes can get SIGKILL when executed from there.
fn is_cargo_target_path(p: &Path) -> bool {
    let mut comps = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    while let Some(c) = comps.next() {
        if c == "target" {
            if let Some(profile) = comps.next() {
                if profile == "debug" || profile == "release" {
                    return true;
                }
            }
        }
    }
    false
}

pub fn find_bundled_core(resource_dir: Option<&Path>, kind: CoreKind) -> Option<PathBuf> {
    let cands: Vec<PathBuf> = bundled_core_candidates(resource_dir, kind)
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    // Prefer non-target paths (src-tauri/resources, app bundle, …)
    cands
        .iter()
        .find(|p| !is_cargo_target_path(p))
        .cloned()
        .or_else(|| cands.into_iter().next())
}

/// Copy bundled core into app data `bin/` so we always execute from a stable path.
fn stage_bundled_core(app_data_dir: &Path, bundled: &Path, kind: CoreKind) -> AppResult<PathBuf> {
    let dest = core_bin_path(app_data_dir, kind);
    if dest.is_file() {
        // Same size → assume OK; re-copy if source is newer/different size.
        // Preserve setuid binaries (TUN auth) when content length matches.
        let same = std::fs::metadata(&dest)
            .ok()
            .zip(std::fs::metadata(bundled).ok())
            .map(|(a, b)| a.len() == b.len())
            .unwrap_or(false);
        if same {
            return Ok(dest);
        }
        // Root-owned setuid binary cannot be overwritten by the user without elevation.
        #[cfg(target_os = "macos")]
        {
            if let Err(e) = super::macos_auth::remove_setuid_core_if_needed(&dest) {
                crate::app_log::warn("core", format!("could not replace setuid core: {e}"));
            }
        }
    }
    let dir = core_dir(app_data_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(bundled, &dest).map_err(|e| {
        AppError::Core(format!(
            "copy {} to {}: {e}",
            kind.display_name(),
            dest.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
    }
    // Best-effort clear quarantine on macOS
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("xattr").args(["-cr"]).arg(&dest).output();
    }
    // Keep version file next to staged binary when available
    if let Some(parent) = bundled.parent() {
        let vf = parent.join(kind.version_file_name());
        if vf.is_file() {
            if let Ok(v) = std::fs::read_to_string(&vf) {
                let _ = write_version_file(app_data_dir, kind, v.trim());
            }
        }
    }
    // Xray: stage geosite/geoip data files shipped alongside the binary so
    // geosite:/geoip: routing works from the app-data asset location, plus
    // wintun.dll (Windows tun adapter driver, not in the Xray release zip).
    if kind == CoreKind::Xray {
        #[cfg(target_os = "windows")]
        let extra_files: [&str; 3] = ["geosite.dat", "geoip.dat", "wintun.dll"];
        #[cfg(not(target_os = "windows"))]
        let extra_files: [&str; 2] = ["geosite.dat", "geoip.dat"];
        if let Some(parent) = bundled.parent() {
            for file in extra_files {
                let src = parent.join(file);
                if src.is_file() {
                    let target = core_dir(app_data_dir).join(file);
                    if !target.is_file() {
                        let _ = std::fs::copy(&src, &target);
                    }
                }
            }
        }
    }
    // mihomo: stage the bundled geodata pair
    // (`mihomo-geodata/Country.mmdb` + `geosite.dat`) into the mihomo home
    // dir. wintun.dll (Windows tun) is shared with the Xray staging —
    // mihomo looks it up next to its exe in `bin/`.
    if kind == CoreKind::Mihomo {
        if let Some(parent) = bundled.parent() {
            let _ = super::assets::stage_bundled_mihomo_geodata_from(
                app_data_dir,
                &parent.join("mihomo-geodata"),
            );
        }
    }
    Ok(dest)
}

/// Prefer staged/downloaded core under app data; stage bundled on first use.
pub fn resolve_core_bin(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    kind: CoreKind,
) -> (Option<PathBuf>, CoreSource) {
    let downloaded = core_bin_path(app_data_dir, kind);
    if downloaded.is_file() {
        return (Some(downloaded), CoreSource::Downloaded);
    }
    if let Some(bundled) = find_bundled_core(resource_dir, kind) {
        match stage_bundled_core(app_data_dir, &bundled, kind) {
            Ok(staged) => return (Some(staged), CoreSource::Bundled),
            Err(_) => {
                // Fall back to direct path if not under cargo target
                if !is_cargo_target_path(&bundled) {
                    return (Some(bundled), CoreSource::Bundled);
                }
            }
        }
    }
    (None, CoreSource::Missing)
}

/// Inspect the available core without copying or executing anything.
/// UI status paths must use this instead of `resolve_core_bin`, which stages
/// a bundled binary and may perform additional platform-specific filesystem work.
pub fn inspect_core_bin(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    kind: CoreKind,
) -> (Option<PathBuf>, CoreSource) {
    let downloaded = core_bin_path(app_data_dir, kind);
    if downloaded.is_file() {
        return (Some(downloaded), CoreSource::Downloaded);
    }
    match find_bundled_core(resource_dir, kind) {
        Some(bundled) => (Some(bundled), CoreSource::Bundled),
        None => (None, CoreSource::Missing),
    }
}

/// Remove a user-downloaded core so resolution falls back to the bundled
/// copy ("restore factory core"). The bundled binary is verified to exist
/// up front — the downloaded one is never dropped without a replacement.
/// The next start stages the bundled binary back into `bin/` via the
/// regular first-run path (`resolve_core_bin` → `stage_bundled_core`).
///
/// A running Windows core holds its image locked against deletion (but not
/// against rename): when the direct delete fails, the binary is renamed
/// aside (`<stem>.previous.exe`, same convention as download swaps) so
/// `inspect_core_bin` reports the bundled copy immediately; the running
/// process keeps executing the renamed image until its next (re)start.
pub fn reset_core_to_bundled(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    kind: CoreKind,
) -> AppResult<()> {
    if find_bundled_core(resource_dir, kind).is_none() {
        return Err(AppError::Core(format!(
            "no bundled {} in this installation",
            kind.display_name()
        )));
    }
    let dest = core_bin_path(app_data_dir, kind);
    if dest.is_file() {
        if std::fs::remove_file(&dest).is_err() {
            let previous = super::download::previous_core_path(kind, &dest);
            let _ = std::fs::remove_file(&previous);
            std::fs::rename(&dest, &previous).map_err(|e| {
                AppError::Core(format!(
                    "retire downloaded {} binary: {e}",
                    kind.display_name()
                ))
            })?;
        }
    }
    // The version file belongs to the downloaded install; the bundled source
    // reads its own copy next to the resource binary.
    let _ = std::fs::remove_file(version_file_path(app_data_dir, kind));
    Ok(())
}

pub fn installed_core_version(app_data_dir: &Path, kind: CoreKind) -> Option<String> {
    let vf = version_file_path(app_data_dir, kind);
    if let Ok(s) = std::fs::read_to_string(vf) {
        let t = s.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

/// Read bundled version from the kind's version file only (no process spawn —
/// keeps UI instant).
pub fn bundled_core_version(resource_dir: Option<&Path>, kind: CoreKind) -> Option<String> {
    if let Some(bin) = find_bundled_core(resource_dir, kind) {
        if let Some(parent) = bin.parent() {
            let vf = parent.join(kind.version_file_name());
            if let Ok(s) = std::fs::read_to_string(&vf) {
                let t = s.trim().to_string();
                if !t.is_empty() {
                    return Some(normalize_version(&t));
                }
            }
        }
    }
    // Also try fixed relative layout next to candidates (dev)
    for cand in bundled_core_candidates(resource_dir, kind) {
        if let Some(parent) = cand.parent() {
            let vf = parent.join(kind.version_file_name());
            if let Ok(s) = std::fs::read_to_string(&vf) {
                let t = s.trim().to_string();
                if !t.is_empty() {
                    return Some(normalize_version(&t));
                }
            }
        }
    }
    None
}

/// Resolve version for whatever core is active (file metadata only; no spawn).
pub fn active_core_version(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    kind: CoreKind,
) -> Option<String> {
    let (_path, source) = inspect_core_bin(app_data_dir, resource_dir, kind);
    match source {
        CoreSource::Downloaded => installed_core_version(app_data_dir, kind),
        CoreSource::Bundled => bundled_core_version(resource_dir, kind),
        CoreSource::Missing => None,
    }
}

pub fn read_version_of_binary(kind: CoreKind, bin: &Path) -> AppResult<String> {
    if !bin.exists() {
        return Err(AppError::Core(format!(
            "{} binary not found",
            kind.display_name()
        )));
    }
    let mut cmd = Command::new(bin);
    cmd.args(kind.version_args());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd
        .output()
        .map_err(|e| AppError::Core(format!("run version failed: {e}")))?;
    if !out.status.success() {
        return Err(AppError::Core(format!(
            "version exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let version = kind
        .parse_version_output(&text)
        .ok_or_else(|| AppError::Core("empty version output".into()))?;
    Ok(normalize_version(&version))
}

pub fn normalize_version(v: &str) -> String {
    let v = v.trim();
    if v.starts_with('v') {
        v.to_string()
    } else {
        format!("v{v}")
    }
}

pub fn write_version_file(app_data_dir: &Path, kind: CoreKind, version: &str) -> AppResult<()> {
    let dir = core_dir(app_data_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        version_file_path(app_data_dir, kind),
        normalize_version(version),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn inspection_never_stages_bundled_core() {
        let root = std::env::temp_dir().join(format!(
            "satelite-core-inspect-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let app_data = root.join("app-data");
        let resources = root.join("resources-root");
        let platform = detect_platform().expect("supported test platform");
        for kind in [CoreKind::SingBox, CoreKind::Xray, CoreKind::Mihomo] {
            // Bundled layout uses the shared sing-box-style platform directory
            // for every core kind (`bundled_core_candidates`); the per-kind
            // `asset_suffix_for` naming only applies to GitHub release assets.
            let bundled_dir = resources.join("bin").join(platform.asset_suffix);
            std::fs::create_dir_all(&bundled_dir).expect("create fake resource directory");
            std::fs::write(bundled_dir.join(kind.binary_name()), b"fake-core")
                .expect("write fake bundled core");
            std::fs::write(bundled_dir.join(kind.version_file_name()), b"v-test")
                .expect("write fake version");

            let (path, source) = inspect_core_bin(&app_data, Some(&resources), kind);
            assert!(path.is_some());
            assert_eq!(source, CoreSource::Bundled);
            assert!(!core_bin_path(&app_data, kind).exists());

            let _ = active_core_version(&app_data, Some(&resources), kind);
            assert!(!core_bin_path(&app_data, kind).exists());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn xray_platform_suffixes_differ_from_singbox() {
        let p = detect_platform().expect("platform");
        assert_ne!(
            p.asset_suffix_for(CoreKind::SingBox),
            p.asset_suffix_for(CoreKind::Xray)
        );
        assert!(
            p.asset_suffix_for(CoreKind::Xray).starts_with("windows")
                || p.asset_suffix_for(CoreKind::Xray).starts_with("macos")
                || p.asset_suffix_for(CoreKind::Xray).starts_with("linux")
        );
    }

    #[test]
    fn reset_retires_downloaded_core_and_falls_back_to_bundled() {
        let root = std::env::temp_dir().join(format!(
            "satelite-core-reset-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let app_data = root.join("app-data");
        let resources = root.join("resources-root");
        let platform = detect_platform().expect("supported test platform");
        let bundled_dir = resources.join("bin").join(platform.asset_suffix);
        std::fs::create_dir_all(&bundled_dir).expect("create bundled dir");
        std::fs::write(
            bundled_dir.join(CoreKind::SingBox.binary_name()),
            b"bundled",
        )
        .expect("write bundled core");
        std::fs::create_dir_all(core_dir(&app_data)).expect("create bin dir");
        std::fs::write(
            core_bin_path(&app_data, CoreKind::SingBox),
            b"user download",
        )
        .expect("write downloaded core");
        write_version_file(&app_data, CoreKind::SingBox, "1.14.0").expect("write version");

        // Reset → downloaded binary + its version file are gone; inspection
        // reports the bundled copy, and resolution stages it back on next use.
        // (The no-bundled guard branch is untestable here: the dev source
        // tree itself is a bundled candidate and carries the real binary.)
        reset_core_to_bundled(&app_data, Some(&resources), CoreKind::SingBox).expect("reset");
        let (path, source) = inspect_core_bin(&app_data, Some(&resources), CoreKind::SingBox);
        assert_eq!(source, CoreSource::Bundled);
        assert!(path.is_some());
        assert!(!core_bin_path(&app_data, CoreKind::SingBox).exists());
        assert!(!version_file_path(&app_data, CoreKind::SingBox).exists());
        let (staged, source) = resolve_core_bin(&app_data, Some(&resources), CoreKind::SingBox);
        assert_eq!(source, CoreSource::Bundled);
        assert_eq!(staged, Some(core_bin_path(&app_data, CoreKind::SingBox)));

        // Resetting again retires the staged copy too — the data-dir binary
        // always loses — but resolution just re-stages it, so the core stays
        // usable either way.
        reset_core_to_bundled(&app_data, Some(&resources), CoreKind::SingBox)
            .expect("reset already-bundled");
        assert!(!core_bin_path(&app_data, CoreKind::SingBox).exists());
        let (staged, source) = resolve_core_bin(&app_data, Some(&resources), CoreKind::SingBox);
        assert_eq!(source, CoreSource::Bundled);
        assert!(staged.is_some());

        let _ = std::fs::remove_dir_all(root);
    }
}
