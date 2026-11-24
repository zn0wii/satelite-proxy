//! System HTTP(S) proxy control.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod stub;
#[cfg(target_os = "windows")]
mod windows;

use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const OWNER_FILE: &str = "system-proxy-owner.json";

#[derive(Debug, Clone)]
pub struct SystemProxySnapshot {
    /// Platform-specific opaque restore token (e.g. service name + previous flags).
    ///
    /// Part of the cross-platform snapshot contract; written by some backends
    /// and read by others, so on a given platform it may look unused.
    #[allow(dead_code)]
    pub detail: String,
}

pub trait SystemProxy: Send + Sync {
    fn enable(&self, host: &str, port: u16) -> AppResult<SystemProxySnapshot>;
    fn disable(&self, snapshot: Option<&SystemProxySnapshot>) -> AppResult<()>;
    /// Return a disable token only when the enabled OS proxy belongs entirely
    /// to this exact loopback endpoint. Mixed/foreign proxy settings are never claimed.
    fn detect_owned(&self, host: &str, port: u16) -> AppResult<Option<SystemProxySnapshot>>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProxyOwnership {
    host: String,
    port: u16,
    pid: u32,
    updated_at: u64,
}

fn owner_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("data").join(OWNER_FILE)
}

pub fn record_ownership(app_data_dir: &Path, host: &str, port: u16) -> AppResult<()> {
    let path = owner_path(app_data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let ownership = ProxyOwnership {
        host: host.to_string(),
        port,
        pid: std::process::id(),
        updated_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let raw = serde_json::to_vec_pretty(&ownership)
        .map_err(|error| AppError::Storage(format!("serialize proxy ownership: {error}")))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn clear_ownership(app_data_dir: &Path) -> AppResult<()> {
    let path = owner_path(app_data_dir);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn load_ownership(app_data_dir: &Path) -> Option<ProxyOwnership> {
    let raw = fs::read(owner_path(app_data_dir)).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn cleanup_targets(marker: Option<&ProxyOwnership>, current_port: u16) -> Vec<(String, u16)> {
    if let Some(owner) = marker {
        if owner.host == "127.0.0.1" || owner.host.eq_ignore_ascii_case("localhost") {
            return vec![(owner.host.clone(), owner.port)];
        }
        return Vec::new();
    }
    vec![("127.0.0.1".into(), current_port)]
}

/// Clean an OS proxy left behind by an unclean exit. The persisted marker is
/// authoritative; the current mixed port is also checked for pre-marker releases.
pub fn cleanup_stale_owned_proxy(app_data_dir: &Path, current_port: u16) -> AppResult<bool> {
    let backend = create_system_proxy();
    let marker_file_exists = owner_path(app_data_dir).exists();
    let marker = load_ownership(app_data_dir);
    let targets = cleanup_targets(marker.as_ref(), current_port);
    for (host, port) in targets {
        if let Some(snapshot) = backend.detect_owned(&host, port)? {
            backend.disable(Some(&snapshot))?;
            clear_ownership(app_data_dir)?;
            return Ok(true);
        }
    }
    // A marker with no matching live OS proxy is stale and no longer useful.
    if marker_file_exists {
        clear_ownership(app_data_dir)?;
    }
    Ok(false)
}

pub fn create_system_proxy() -> Box<dyn SystemProxy> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacSystemProxy::default())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsSystemProxy)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxSystemProxy)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Box::new(stub::StubSystemProxy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_data_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "satelite-proxy-owner-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn ownership_marker_round_trips_and_clears() {
        let directory = test_data_dir("roundtrip");
        record_ownership(&directory, "127.0.0.1", 2080).unwrap();
        let owner = load_ownership(&directory).expect("ownership marker");
        assert_eq!(owner.host, "127.0.0.1");
        assert_eq!(owner.port, 2080);

        clear_ownership(&directory).unwrap();
        assert!(load_ownership(&directory).is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn marker_port_is_authoritative_over_current_settings() {
        let marker = ProxyOwnership {
            host: "127.0.0.1".into(),
            port: 2080,
            pid: 1,
            updated_at: 1,
        };
        assert_eq!(
            cleanup_targets(Some(&marker), 3080),
            vec![("127.0.0.1".into(), 2080)]
        );
        assert_eq!(
            cleanup_targets(None, 3080),
            vec![("127.0.0.1".into(), 3080)]
        );
    }
}
