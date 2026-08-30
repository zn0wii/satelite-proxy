use crate::config::builder::BuiltConfig;
use crate::error::{AppError, AppResult};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn config_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("config")
}

pub fn active_config_path(app_data_dir: &Path) -> PathBuf {
    config_dir(app_data_dir).join("active.json")
}

pub fn custom_config_dir(app_data_dir: &Path) -> PathBuf {
    config_dir(app_data_dir).join("custom")
}

pub fn custom_config_path(app_data_dir: &Path, id: &str) -> PathBuf {
    custom_config_dir(app_data_dir).join(format!("{}.json", sanitize_profile_id(id)))
}

/// Persist a user sing-box document as-is. Never writes `active.json`.
pub fn write_custom_config(app_data_dir: &Path, id: &str, raw: &str) -> AppResult<PathBuf> {
    let dir = custom_config_dir(app_data_dir);
    fs::create_dir_all(&dir)?;
    let path = custom_config_path(app_data_dir, id);
    let tmp = dir.join(format!("{}.json.tmp", sanitize_profile_id(id)));
    fs::write(&tmp, raw.as_bytes())?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

pub fn remove_custom_config(app_data_dir: &Path, id: &str) {
    let path = custom_config_path(app_data_dir, id);
    let _ = fs::remove_file(path);
}

fn sanitize_profile_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "profile".into()
    } else {
        cleaned
    }
}

/// Write active.json and a timestamped backup. Returns active path.
pub fn write_active_config(app_data_dir: &Path, built: &BuiltConfig) -> AppResult<PathBuf> {
    let dir = config_dir(app_data_dir);
    let backup_dir = dir.join("backup");
    fs::create_dir_all(&backup_dir)?;

    let raw = serde_json::to_string_pretty(&built.value)
        .map_err(|e| AppError::Config(format!("serialize config: {e}")))?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = backup_dir.join(format!("{ts}.json"));
    fs::write(&backup, &raw)?;

    let active = active_config_path(app_data_dir);
    let tmp = dir.join("active.json.tmp");
    fs::write(&tmp, &raw)?;
    fs::rename(&tmp, &active)?;

    // Keep at most 20 backups
    prune_backups(&backup_dir, 20)?;

    Ok(active)
}

/// Active config path for the mihomo core (Clash YAML). The JSON cores
/// share `active.json`; mihomo keeps its own file — every start rewrites it
/// whole, so the two dialects never mix (same policy as `active.json`).
pub fn active_yaml_config_path(app_data_dir: &Path) -> PathBuf {
    config_dir(app_data_dir).join("active.yaml")
}

/// Config path for the Xray sidecar companion process. Never touches
/// `active.*` — the sidecar dialect is a strict subset generated whole on
/// every start (same rewrite policy as the other config files).
pub fn xray_sidecar_config_path(app_data_dir: &Path) -> PathBuf {
    config_dir(app_data_dir).join("xray-sidecar.json")
}

/// Write the Xray sidecar config (tmp+rename, no backup churn — the file is
/// fully derived from the main config's build plan and cheap to regenerate).
pub fn write_xray_sidecar_config(app_data_dir: &Path, built: &BuiltConfig) -> AppResult<PathBuf> {
    let dir = config_dir(app_data_dir);
    fs::create_dir_all(&dir)?;

    let raw = serde_json::to_string_pretty(&built.value)
        .map_err(|e| AppError::Config(format!("serialize sidecar config: {e}")))?;

    let path = xray_sidecar_config_path(app_data_dir);
    let tmp = dir.join("xray-sidecar.json.tmp");
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Write active.yaml and a timestamped backup (mirrors write_active_config).
pub fn write_active_yaml_config(app_data_dir: &Path, raw: &str) -> AppResult<PathBuf> {
    let dir = config_dir(app_data_dir);
    let backup_dir = dir.join("backup");
    fs::create_dir_all(&backup_dir)?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = backup_dir.join(format!("{ts}.yaml"));
    fs::write(&backup, raw)?;

    let active = active_yaml_config_path(app_data_dir);
    let tmp = dir.join("active.yaml.tmp");
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, &active)?;

    prune_backups_yaml(&backup_dir, 20)?;

    Ok(active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::builder::BuiltConfig;
    use serde_json::json;

    #[test]
    fn custom_write_does_not_touch_active_json() {
        let dir = std::env::temp_dir().join(format!(
            "satelite-custom-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let active_before = json!({"mark":"generated"});
        let built = BuiltConfig {
            value: active_before.clone(),
            outbound_tags: Vec::new(),
            selected_tag: "direct".into(),
        };
        write_active_config(&dir, &built).unwrap();
        let user =
            r#"{"inbounds":[{"type":"mixed","listen_port":1080}],"outbounds":[{"type":"direct"}]}"#;
        let custom = write_custom_config(&dir, "abc123", user).unwrap();
        assert!(custom.ends_with(std::path::Path::new("custom").join("abc123.json")));
        assert_eq!(fs::read_to_string(&custom).unwrap(), user);
        let active_after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(active_config_path(&dir)).unwrap()).unwrap();
        assert_eq!(active_after, active_before);
        let _ = fs::remove_dir_all(dir);
    }
}

fn prune_backups(dir: &Path, keep: usize) -> AppResult<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    for e in entries.into_iter().skip(keep) {
        let _ = fs::remove_file(e.path());
    }
    Ok(())
}

fn prune_backups_yaml(dir: &Path, keep: usize) -> AppResult<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
    for e in entries.into_iter().skip(keep) {
        let _ = fs::remove_file(e.path());
    }
    Ok(())
}
