//! Download cores from GitHub releases (SagerNet/sing-box, XTLS/Xray-core).

use crate::core::kind::CoreKind;
use crate::core::paths::{
    core_bin_path, core_dir, detect_platform, normalize_version, read_version_of_binary,
    write_version_file, CorePlatform,
};
use crate::error::{AppError, AppResult};
use flate2::read::GzDecoder;
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tar::Archive;

const APP_GITHUB_LATEST: &str =
    "https://api.github.com/repos/zn0wii/satelite-proxy/releases/latest";
const APP_RELEASES_PAGE: &str = "https://github.com/zn0wii/satelite-proxy/releases/latest";
const MAX_CORE_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

fn github_latest_url(kind: CoreKind) -> String {
    format!(
        "https://api.github.com/repos/{}/releases/latest",
        kind.repo()
    )
}

fn github_tag_url(kind: CoreKind) -> String {
    format!(
        "https://api.github.com/repos/{}/releases/tags/",
        kind.repo()
    )
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CoreDownloadResult {
    pub kind: String,
    pub version: String,
    pub path: String,
    pub asset_name: String,
    pub platform: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CoreDownloadProgress {
    pub kind: String,
    pub stage: &'static str,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub percent: Option<u8>,
    pub via_proxy: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LatestReleaseInfo {
    pub kind: String,
    pub version: String,
    pub asset_name: String,
    pub download_url: String,
    pub size: u64,
    pub platform: String,
}

pub async fn fetch_latest_release_with_proxy(
    kind: CoreKind,
    proxy_url: Option<&str>,
) -> AppResult<LatestReleaseInfo> {
    let platform = detect_platform()?;
    match fetch_release_json(&github_latest_url(kind), proxy_url).await {
        Ok(release) => pick_asset(kind, release, platform),
        Err(api_err) => {
            // API blocked/unreachable → direct asset URL with pinned fallback version
            let _ = api_err;
            Ok(synthetic_release_info(
                kind,
                kind.fallback_version(),
                platform,
            ))
        }
    }
}

async fn fetch_release_by_tag_with_proxy(
    kind: CoreKind,
    tag: &str,
    proxy_url: Option<&str>,
) -> AppResult<LatestReleaseInfo> {
    let platform = detect_platform()?;
    let tag = normalize_version(tag);
    let url = format!("{}{tag}", github_tag_url(kind));
    match fetch_release_json(&url, proxy_url).await {
        Ok(release) => pick_asset(kind, release, platform),
        Err(_) => Ok(synthetic_release_info(kind, &tag, platform)),
    }
}

async fn fetch_release_json(url: &str, proxy_url: Option<&str>) -> AppResult<GhRelease> {
    let client = http_client(proxy_url)?;
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| AppError::Core(format!("github api: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Core(format!(
            "github api status {status} for {url}: {}",
            body.chars().take(200).collect::<String>()
        )));
    }
    resp.json::<GhRelease>()
        .await
        .map_err(|e| AppError::Core(format!("parse github release: {e}")))
}

/// Latest release tag of the app itself (zn0wii/satelite-proxy), used by the
/// Settings version tab to flag app updates. Tag only — no asset picking,
/// and unlike the core check there is no pinned fallback: if the API is
/// unreachable the caller surfaces the error instead of guessing.
pub async fn fetch_latest_app_tag(proxy_url: Option<&str>) -> AppResult<String> {
    #[derive(Deserialize)]
    struct TagOnly {
        tag_name: String,
    }
    let client = http_client(proxy_url)?;
    let resp = client
        .get(APP_GITHUB_LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| AppError::Core(format!("github api: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Core(format!(
            "github api status {} for {APP_GITHUB_LATEST}",
            resp.status()
        )));
    }
    let release: TagOnly = resp
        .json()
        .await
        .map_err(|e| AppError::Core(format!("parse github release: {e}")))?;
    Ok(normalize_version(&release.tag_name))
}

/// Latest app tag via the `releases/latest` page redirect: github.com 302s
/// to `…/releases/tag/<tag>`. Preferred over the REST API because it draws
/// on the website's budget instead of api.github.com's 60 req/h per IP for
/// unauthenticated callers — an easy 403 behind shared NAT/proxy exits.
pub async fn fetch_latest_app_tag_via_redirect(proxy_url: Option<&str>) -> AppResult<String> {
    let client = http_client_with_redirect(proxy_url, reqwest::redirect::Policy::none())?;
    let resp = client
        .get(APP_RELEASES_PAGE)
        .send()
        .await
        .map_err(|e| AppError::Core(format!("github releases page: {e}")))?;
    if !resp.status().is_redirection() {
        return Err(AppError::Core(format!(
            "github releases page status {} (expected redirect)",
            resp.status()
        )));
    }
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Core("github releases redirect missing location".into()))?;
    extract_tag_from_release_url(location)
}

/// `…/releases/tag/<tag>` (absolute or relative) → normalized tag.
fn extract_tag_from_release_url(url: &str) -> AppResult<String> {
    let path = url.split('?').next().unwrap_or(url);
    let tag = path
        .split("/releases/tag/")
        .last()
        .unwrap_or("")
        .trim_end_matches('/');
    if tag.is_empty() || tag == path {
        return Err(AppError::Core(format!("unexpected release url: {url}")));
    }
    Ok(normalize_version(tag))
}

/// Fallback when GitHub API is blocked: build asset URL from known version tag.
fn synthetic_release_info(kind: CoreKind, tag: &str, platform: CorePlatform) -> LatestReleaseInfo {
    let version = normalize_version(tag);
    let suffix = platform.asset_suffix_for(kind);
    let asset_name = kind.asset_name(&version, suffix, platform.is_windows);
    let download_url = format!(
        "https://github.com/{}/releases/download/{version}/{asset_name}",
        kind.repo()
    );
    LatestReleaseInfo {
        kind: kind.as_str().into(),
        version,
        asset_name,
        download_url,
        size: 0,
        platform: suffix.to_string(),
    }
}

fn pick_asset(
    kind: CoreKind,
    release: GhRelease,
    platform: CorePlatform,
) -> AppResult<LatestReleaseInfo> {
    let version = normalize_version(&release.tag_name);
    let suffix = platform.asset_suffix_for(kind);
    let expected = kind.asset_name(&version, suffix, platform.is_windows);
    let ext = if platform.is_windows { "zip" } else { "tar.gz" };
    // sing-box assets embed the version (`sing-box-1.13.15-darwin-arm64.tar.gz`);
    // Xray assets don't (`Xray-macos-arm64-v8a.zip`); mihomo embeds it too
    // (`mihomo-darwin-arm64-v1.19.30.gz`).
    let prefix = match kind {
        CoreKind::SingBox => format!("sing-box-{}", version.trim_start_matches('v')),
        CoreKind::Xray => "Xray-".to_string(),
        CoreKind::Mihomo => format!("mihomo-"),
    };

    let asset = release
        .assets
        .iter()
        .find(|a| a.name == expected)
        .or_else(|| {
            // fallback: kind prefix + platform suffix + correct extension
            release.assets.iter().find(|a| {
                a.name.starts_with(&prefix)
                    && a.name.contains(suffix)
                    && a.name.ends_with(ext)
                    && !a.name.contains("legacy")
            })
        })
        .ok_or_else(|| {
            AppError::Core(format!(
                "no asset for platform {suffix} (expected {expected})"
            ))
        })?;

    Ok(LatestReleaseInfo {
        kind: kind.as_str().into(),
        version,
        asset_name: asset.name.clone(),
        download_url: asset.browser_download_url.clone(),
        size: asset.size,
        platform: suffix.to_string(),
    })
}

fn http_client(proxy_url: Option<&str>) -> AppResult<reqwest::Client> {
    http_client_with_redirect(proxy_url, reqwest::redirect::Policy::default())
}

fn http_client_with_redirect(
    proxy_url: Option<&str>,
    policy: reqwest::redirect::Policy,
) -> AppResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent("SateliteProxy/0.1 (core-downloader)")
        .redirect(policy);
    if let Some(proxy_url) = proxy_url {
        builder = builder.proxy(
            reqwest::Proxy::all(proxy_url)
                .map_err(|error| AppError::Core(format!("download proxy: {error}")))?,
        );
    }
    builder.build().map_err(|e| AppError::Core(e.to_string()))
}

#[cfg(test)]
mod app_update_tests {
    use super::extract_tag_from_release_url;

    #[test]
    fn extracts_tag_from_absolute_url() {
        assert_eq!(
            extract_tag_from_release_url(
                "https://github.com/zn0wii/satelite-proxy/releases/tag/1.0.9"
            )
            .unwrap(),
            "v1.0.9"
        );
    }

    #[test]
    fn extracts_tag_from_relative_url() {
        assert_eq!(
            extract_tag_from_release_url("/zn0wii/satelite-proxy/releases/tag/v1.1.0").unwrap(),
            "v1.1.0"
        );
    }

    #[test]
    fn strips_query_string() {
        assert_eq!(
            extract_tag_from_release_url(
                "https://github.com/zn0wii/satelite-proxy/releases/tag/1.2.0?foo=bar"
            )
            .unwrap(),
            "v1.2.0"
        );
    }

    #[test]
    fn rejects_urls_without_a_tag_segment() {
        assert!(extract_tag_from_release_url("https://github.com/zn0wii/satelite-proxy").is_err());
    }
}

/// Download latest (or given tag) and install into `{app_data}/bin/<core>`.
pub async fn download_latest_core(
    kind: CoreKind,
    app_data_dir: &Path,
    tag: Option<String>,
) -> AppResult<CoreDownloadResult> {
    download_latest_core_with_progress(kind, app_data_dir, tag, None, |_| {}).await
}

pub async fn download_latest_core_with_progress(
    kind: CoreKind,
    app_data_dir: &Path,
    tag: Option<String>,
    proxy_url: Option<String>,
    progress: impl Fn(CoreDownloadProgress) + Send + Sync + 'static,
) -> AppResult<CoreDownloadResult> {
    let info = if let Some(t) = tag {
        fetch_release_by_tag_with_proxy(kind, &t, proxy_url.as_deref()).await?
    } else {
        fetch_latest_release_with_proxy(kind, proxy_url.as_deref()).await?
    };
    download_and_install(kind, app_data_dir, &info, proxy_url.as_deref(), progress).await
}

async fn download_and_install<F>(
    kind: CoreKind,
    app_data_dir: &Path,
    info: &LatestReleaseInfo,
    proxy_url: Option<&str>,
    progress: F,
) -> AppResult<CoreDownloadResult>
where
    F: Fn(CoreDownloadProgress) + Send + Sync + 'static,
{
    validate_archive_size_hint(info.size)?;
    let via_proxy = proxy_url.is_some();
    let progress = Arc::new(progress);

    let client = http_client(proxy_url)?;
    let resp = client
        .get(&info.download_url)
        .send()
        .await
        .map_err(|e| AppError::Core(format!("download: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Core(format!("download status {}", resp.status())));
    }
    let declared_total = (info.size > 0).then_some(info.size);
    let mut last_percent = None;
    let download_progress = Arc::clone(&progress);
    let bytes = crate::services::http_body::read_limited_with_progress(
        resp,
        MAX_CORE_ARCHIVE_BYTES,
        "core archive exceeds 256 MB".into(),
        move |downloaded, response_total| {
            let total = response_total.or(declared_total);
            let percent = total
                .filter(|total| *total > 0)
                .map(|total| ((downloaded.saturating_mul(100) / total).min(100)) as u8);
            if downloaded == 0 || percent != last_percent {
                last_percent = percent;
                download_progress(CoreDownloadProgress {
                    kind: kind.as_str().into(),
                    stage: "downloading",
                    downloaded,
                    total,
                    percent,
                    via_proxy,
                });
            }
        },
    )
    .await
    .map_err(|e| AppError::Core(format!("download body: {e}")))?;
    if bytes.len() < 1024 {
        return Err(AppError::Core("download too small, likely failed".into()));
    }

    let app_data_dir = app_data_dir.to_path_buf();
    let info = info.clone();
    let downloaded = bytes.len() as u64;
    progress(CoreDownloadProgress {
        kind: kind.as_str().into(),
        stage: "installing",
        downloaded,
        total: Some(downloaded),
        percent: Some(100),
        via_proxy,
    });
    let result = tokio::task::spawn_blocking(move || {
        install_downloaded_archive(kind, &app_data_dir, &info, bytes)
    })
    .await
    .map_err(|error| AppError::Core(format!("install core task: {error}")))??;
    progress(CoreDownloadProgress {
        kind: kind.as_str().into(),
        stage: "done",
        downloaded,
        total: Some(downloaded),
        percent: Some(100),
        via_proxy,
    });
    Ok(result)
}

fn validate_archive_size_hint(size: u64) -> AppResult<()> {
    if size > MAX_CORE_ARCHIVE_BYTES as u64 {
        return Err(AppError::Core("core archive exceeds 256 MB".into()));
    }
    Ok(())
}

fn install_downloaded_archive(
    kind: CoreKind,
    app_data_dir: &Path,
    info: &LatestReleaseInfo,
    bytes: Vec<u8>,
) -> AppResult<CoreDownloadResult> {
    let bin_dir = core_dir(app_data_dir);
    fs::create_dir_all(&bin_dir)?;
    let archive_path = bin_dir.join(&info.asset_name);
    {
        let mut f = File::create(&archive_path)
            .map_err(|e| AppError::Core(format!("write archive: {e}")))?;
        f.write_all(&bytes)
            .map_err(|e| AppError::Core(format!("write archive: {e}")))?;
    }

    let dest = core_bin_path(app_data_dir, kind);
    let staged = staged_core_path(kind, &dest);
    let previous = previous_core_path(kind, &dest);
    let _ = fs::remove_file(&staged);
    let install_result = (|| {
        if info.asset_name.ends_with(".tar.gz") || info.asset_name.ends_with(".tgz") {
            extract_binary_from_tar_gz(kind, &archive_path, &staged)?;
        } else if info.asset_name.ends_with(".gz") {
            // mihomo darwin/linux assets are a bare gzipped binary.
            let file =
                File::open(&archive_path).map_err(|e| AppError::Core(format!("open gz: {e}")))?;
            let mut dec = GzDecoder::new(file);
            let mut out =
                File::create(&staged).map_err(|e| AppError::Core(format!("create binary: {e}")))?;
            io::copy(&mut dec, &mut out)
                .map_err(|e| AppError::Core(format!("extract binary: {e}")))?;
        } else if info.asset_name.ends_with(".zip") {
            extract_from_zip(kind, &archive_path, &staged, &bin_dir)?;
        } else {
            return Err(AppError::Core(format!(
                "unsupported archive: {}",
                info.asset_name
            )));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&staged)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&staged, perms)?;
        }

        let actual_version = read_version_of_binary(kind, &staged)?;
        if !versions_match(&actual_version, &info.version) {
            return Err(AppError::Core(format!(
                "downloaded core version mismatch: expected {}, got {actual_version}",
                info.version
            )));
        }

        let had_previous = replace_installed_core(kind, &staged, &dest, &previous)?;
        if let Err(error) = write_version_file(app_data_dir, kind, &actual_version) {
            let _ = fs::remove_file(&dest);
            if had_previous {
                let _ = fs::rename(&previous, &dest);
            }
            return Err(error);
        }
        if had_previous {
            let _ = fs::remove_file(&previous);
        }

        Ok(CoreDownloadResult {
            kind: kind.as_str().into(),
            version: actual_version,
            path: dest.display().to_string(),
            asset_name: info.asset_name.clone(),
            platform: info.platform.clone(),
            bytes: bytes.len() as u64,
        })
    })();

    let _ = fs::remove_file(&archive_path);
    if install_result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    install_result
}

fn staged_core_path(kind: CoreKind, dest: &Path) -> PathBuf {
    let stem = kind.binary_name().trim_end_matches(".exe");
    #[cfg(target_os = "windows")]
    return dest.with_file_name(format!("{stem}.new.exe"));
    #[cfg(not(target_os = "windows"))]
    return dest.with_file_name(format!("{stem}.new"));
}

// pub(crate): `paths::reset_core_to_bundled` reuses the same aside-name
// convention when retiring a locked (running) downloaded binary.
pub(crate) fn previous_core_path(kind: CoreKind, dest: &Path) -> PathBuf {
    let stem = kind.binary_name().trim_end_matches(".exe");
    #[cfg(target_os = "windows")]
    return dest.with_file_name(format!("{stem}.previous.exe"));
    #[cfg(not(target_os = "windows"))]
    return dest.with_file_name(format!("{stem}.previous"));
}

fn versions_match(actual: &str, expected: &str) -> bool {
    normalize_version(actual) == normalize_version(expected)
}

fn replace_installed_core(
    kind: CoreKind,
    staged: &Path,
    dest: &Path,
    previous: &Path,
) -> AppResult<bool> {
    let _ = fs::remove_file(previous);
    #[cfg(target_os = "macos")]
    if dest.exists() {
        crate::core::macos_auth::remove_setuid_core_if_needed(dest)?;
    }
    let _ = kind;
    let had_previous = dest.exists();
    if had_previous {
        fs::rename(dest, previous)
            .map_err(|error| AppError::Core(format!("stage previous core: {error}")))?;
    }
    if let Err(error) = fs::rename(staged, dest) {
        if had_previous {
            let _ = fs::rename(previous, dest);
        }
        return Err(AppError::Core(format!("activate downloaded core: {error}")));
    }
    Ok(had_previous)
}

/// Binary file names accepted inside a sing-box tar.gz (historical layouts).
fn tar_binary_matches(kind: CoreKind, name: &str) -> bool {
    let bin = kind.binary_name();
    let base = bin.trim_end_matches(".exe");
    name == bin || name == base
}

fn extract_binary_from_tar_gz(kind: CoreKind, archive: &Path, dest: &Path) -> AppResult<()> {
    let file = File::open(archive).map_err(|e| AppError::Core(format!("open tar.gz: {e}")))?;
    let dec = GzDecoder::new(file);
    let mut tar = Archive::new(dec);
    let mut found = false;

    for entry in tar
        .entries()
        .map_err(|e| AppError::Core(format!("tar entries: {e}")))?
    {
        let mut entry = entry.map_err(|e| AppError::Core(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| AppError::Core(format!("tar path: {e}")))?
            .to_path_buf();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if tar_binary_matches(kind, name) {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out =
                File::create(dest).map_err(|e| AppError::Core(format!("create binary: {e}")))?;
            io::copy(&mut entry, &mut out)
                .map_err(|e| AppError::Core(format!("extract binary: {e}")))?;
            found = true;
            break;
        }
    }

    if !found {
        return Err(AppError::Core(format!(
            "{} binary not found inside tar.gz",
            kind.display_name()
        )));
    }
    Ok(())
}

/// How confidently a zip entry name matches the core binary. Exact names
/// (historical layouts) outrank mihomo's platform-suffixed inner exe.
fn zip_binary_match_rank(kind: CoreKind, file_name: &str) -> Option<u8> {
    let want = kind.binary_name();
    if file_name == want
        || tar_binary_matches(kind, file_name)
        || file_name
            .strip_suffix(want.strip_suffix(".exe").unwrap_or(want))
            .is_some()
    {
        return Some(2);
    }
    // mihomo Windows zips carry the platform-suffixed exe name
    // (`mihomo-windows-amd64.exe`), not the plain `mihomo.exe`.
    let stem = want.strip_suffix(".exe").unwrap_or(want);
    if kind == CoreKind::Mihomo
        && file_name.starts_with(&format!("{stem}-"))
        && file_name.ends_with(".exe")
    {
        return Some(1);
    }
    None
}

/// Extract the core binary from a release zip. Xray zips additionally ship
/// `geosite.dat` / `geoip.dat` next to the binary — stage them into `bin_dir`
/// so `geosite:` / `geoip:` routing works after a user-initiated download
/// (bundled installs stage them via `paths::stage_bundled_core`).
fn extract_from_zip(kind: CoreKind, archive: &Path, dest: &Path, bin_dir: &Path) -> AppResult<()> {
    let file = File::open(archive).map_err(|e| AppError::Core(format!("open zip: {e}")))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| AppError::Core(format!("zip open: {e}")))?;
    let mut target_index: Option<(usize, u8)> = None;
    let mut dat_indexes = Vec::new();
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| AppError::Core(format!("zip entry: {e}")))?;
        let name = PathBuf::from(entry.name());
        let file_name = name
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if let Some(rank) = zip_binary_match_rank(kind, file_name) {
            let better = target_index
                .map(|(_, prev_rank)| rank >= prev_rank)
                .unwrap_or(true);
            if better {
                target_index = Some((i, rank));
            }
        } else if kind == CoreKind::Xray && (file_name == "geosite.dat" || file_name == "geoip.dat")
        {
            dat_indexes.push((i, file_name.to_string()));
        }
    }
    let idx = target_index
        .ok_or_else(|| {
            AppError::Core(format!(
                "{} binary not found inside zip",
                kind.display_name()
            ))
        })?
        .0;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut entry = zip
        .by_index(idx)
        .map_err(|e| AppError::Core(format!("zip entry: {e}")))?;
    let mut out = File::create(dest).map_err(|e| AppError::Core(format!("create binary: {e}")))?;
    io::copy(&mut entry, &mut out).map_err(|e| AppError::Core(format!("extract binary: {e}")))?;
    drop(entry);

    for (i, file_name) in dat_indexes {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| AppError::Core(format!("zip entry: {e}")))?;
        let target = bin_dir.join(&file_name);
        // Only overwrite when missing or changed in size — dat files can be
        // shared between installs and are large enough that blind copies hurt.
        let needs_copy = std::fs::metadata(&target)
            .map(|m| m.len() != entry.size())
            .unwrap_or(true);
        if needs_copy {
            let mut out = File::create(&target)
                .map_err(|e| AppError::Core(format!("create {file_name}: {e}")))?;
            io::copy(&mut entry, &mut out)
                .map_err(|e| AppError::Core(format!("extract {file_name}: {e}")))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replacement_test_dir(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "satelite-core-replace-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn platform_suffix_known() {
        let p = detect_platform().expect("platform");
        assert!(!p.asset_suffix.is_empty());
        assert!(!p.xray_asset_suffix.is_empty());
        assert!(!p.mihomo_asset_suffix.is_empty());
    }

    /// Regression: mihomo release assets use the sing-box short suffix
    /// (`darwin-arm64`), not a Rust target triple. A prior mihomo_asset_suffix
    /// mismatch made every mihomo download 404.
    #[test]
    fn mihomo_asset_suffix_matches_singbox_short_form() {
        let p = detect_platform().expect("platform");
        assert_eq!(p.mihomo_asset_suffix, p.asset_suffix);
    }

    #[test]
    fn core_archive_size_hint_is_bounded() {
        assert!(validate_archive_size_hint(0).is_ok());
        assert!(validate_archive_size_hint(MAX_CORE_ARCHIVE_BYTES as u64).is_ok());
        assert!(validate_archive_size_hint(MAX_CORE_ARCHIVE_BYTES as u64 + 1).is_err());
    }

    #[test]
    fn downloaded_core_version_must_match_release() {
        assert!(versions_match("1.13.15", "v1.13.15"));
        assert!(!versions_match("v1.13.14", "v1.13.15"));
    }

    #[test]
    fn failed_activation_restores_previous_core() {
        let directory = replacement_test_dir("rollback");
        fs::create_dir_all(&directory).unwrap();
        let dest = directory.join("sing-box");
        let staged = staged_core_path(CoreKind::SingBox, &dest);
        let previous = previous_core_path(CoreKind::SingBox, &dest);
        fs::write(&dest, b"old").unwrap();

        assert!(replace_installed_core(CoreKind::SingBox, &staged, &dest, &previous).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        assert!(!previous.exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn synthetic_xray_release_url_uses_xtls_repo() {
        let platform = detect_platform().unwrap();
        let info = synthetic_release_info(CoreKind::Xray, "v26.3.27", platform);
        assert_eq!(info.version, "v26.3.27");
        assert_eq!(
            info.download_url,
            format!(
                "https://github.com/XTLS/Xray-core/releases/download/v26.3.27/{}",
                info.asset_name
            )
        );
        assert!(info.asset_name.starts_with("Xray-"));
        assert!(info.asset_name.ends_with(".zip"));
    }

    /// Regression: mihomo Windows zips carry a platform-suffixed inner exe
    /// (`mihomo-windows-amd64.exe`), not the plain `mihomo.exe` — a prior
    /// exact-name-only matcher made every in-app mihomo download fail with
    /// "mihomo binary not found inside zip".
    #[test]
    fn zip_matcher_accepts_mihomo_platform_suffixed_exe() {
        assert_eq!(
            zip_binary_match_rank(CoreKind::Mihomo, "mihomo-windows-amd64.exe"),
            Some(1)
        );
        assert_eq!(
            zip_binary_match_rank(CoreKind::Mihomo, "mihomo-windows-amd64-v1.19.30.exe"),
            Some(1)
        );
        // Exact names still win (rank 2).
        assert_eq!(zip_binary_match_rank(CoreKind::Mihomo, "mihomo"), Some(2));
        assert_eq!(
            zip_binary_match_rank(CoreKind::Mihomo, "mihomo.exe"),
            Some(2)
        );
        assert_eq!(zip_binary_match_rank(CoreKind::Mihomo, "README.txt"), None);
        // Prefixed match stays mihomo-only: unrelated xray/sing-box entries
        // with dashes don't hit it.
        assert_eq!(
            zip_binary_match_rank(CoreKind::Xray, "xray-windows.exe"),
            None
        );
        assert_eq!(
            zip_binary_match_rank(CoreKind::SingBox, "sing-box-windows-amd64.exe"),
            None
        );
        assert_eq!(zip_binary_match_rank(CoreKind::Xray, "xray"), Some(2));
    }

    /// End-to-end over the real release artifact: extract the binary from an
    /// actual mihomo Windows zip. `#[ignore]` — needs network like the other
    /// live core tests; run with
    /// `cargo test --lib core::download::tests::live_extracts_mihomo_windows_zip -- --ignored`.
    #[test]
    #[ignore = "live network test"]
    fn live_extracts_mihomo_windows_zip() {
        let bytes = std::process::Command::new("curl")
            .args([
                "-sSL",
                "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.30/mihomo-windows-amd64-v1.19.30.zip",
            ])
            .output()
            .expect("curl mihomo zip");
        assert!(bytes.status.success(), "curl failed: {:?}", bytes.status);
        let directory = replacement_test_dir("mihomo-zip");
        fs::create_dir_all(&directory).unwrap();
        let archive = directory.join("mihomo.zip");
        fs::write(&archive, bytes.stdout).unwrap();
        let dest = directory.join("mihomo.exe");
        extract_from_zip(CoreKind::Mihomo, &archive, &dest, &directory)
            .expect("extract mihomo from real zip");
        assert!(dest.metadata().map(|m| m.len() > 1024).unwrap_or(false));
        fs::remove_dir_all(directory).unwrap();
    }
}
