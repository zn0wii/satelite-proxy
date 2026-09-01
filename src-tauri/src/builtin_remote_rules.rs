//! Seed the bundled remote rule sets: copy the packaged `.srs` files into
//! the app-managed `remote-rule-sets/` cache and heal their store entries,
//! so sing-box only ever loads local files and first launch works offline.

use crate::domain::{
    build_builtin_remote_set, BuiltinRemoteRuleSpec, RuleSet, BUILTIN_REMOTE_RULE_SETS,
};
use std::path::{Path, PathBuf};

/// Directory holding downloaded / seeded remote rule-set caches.
pub fn cache_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("remote-rule-sets")
}

/// Stable (timestamp-free) cache path for one bundled rule set.
pub fn stable_cache_path(app_data_dir: &Path, spec: &BuiltinRemoteRuleSpec) -> PathBuf {
    cache_dir(app_data_dir).join(spec.file)
}

/// Candidate directories for the bundled `.srs` copies. The packaged
/// resource dir wins when present; the dev source tree (via
/// CARGO_MANIFEST_DIR) is the fallback for `pnpm tauri dev`.
fn bundled_dir_candidates(resource_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(res) = resource_dir {
        out.push(res.join("resources/rule-sets"));
        out.push(res.join("rule-sets"));
    }
    out.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/rule-sets"));
    out
}

/// Read the bundled copy of one rule set, rejecting anything that is not a
/// structurally valid binary SRS (a truncated or HTML error page must never
/// reach the kernel).
fn read_bundled(resource_dir: Option<&Path>, spec: &BuiltinRemoteRuleSpec) -> Option<Vec<u8>> {
    for dir in bundled_dir_candidates(resource_dir) {
        let Ok(bytes) = std::fs::read(dir.join(spec.file)) else {
            continue;
        };
        if !bytes.starts_with(b"SRS") {
            crate::app_log::warn(
                "builtin_rules",
                format!("bundled {} is not a binary SRS; skipping", spec.file),
            );
            return None;
        }
        return match crate::srs::parse(&bytes) {
            Ok(_) => Some(bytes),
            Err(error) => {
                crate::app_log::warn(
                    "builtin_rules",
                    format!("bundled {} failed SRS validation: {error}", spec.file),
                );
                None
            }
        };
    }
    None
}

/// (Re)copy the bundled file for one rule set to its stable cache path.
/// Reset uses this to restore the factory payload; returns the path on
/// success (`None` leaves the set on the URL-download fallback).
pub fn copy_bundled_to_cache(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    spec: &BuiltinRemoteRuleSpec,
) -> Option<PathBuf> {
    let bytes = read_bundled(resource_dir, spec)?;
    let dir = cache_dir(app_data_dir);
    std::fs::create_dir_all(&dir).ok()?;
    let path = stable_cache_path(app_data_dir, spec);
    match std::fs::write(&path, bytes) {
        Ok(()) => Some(path),
        Err(error) => {
            crate::app_log::warn(
                "builtin_rules",
                format!("copy bundled {} failed: {error}", spec.file),
            );
            None
        }
    }
}

fn local_path_is_usable(set: &RuleSet) -> bool {
    set.remote
        .as_ref()
        .and_then(|remote| remote.local_path.as_deref())
        .map(|path| !path.trim().is_empty() && Path::new(path).is_file())
        .unwrap_or(false)
}

/// Factory names used by earlier (unreleased) builds of the builtin sets.
/// Seeding renames lingering entries to the current spec name; anything not
/// in this list is a deliberate user rename and stays untouched.
const FORMER_FACTORY_NAMES: [&str; 6] = [
    "geolocation-!cn · 海外网站",
    "geoip-cn · 国内 IP",
    "geosite-cn · 国内站点",
    "内置 海外网站",
    "内置 国内ip",
    "内置 国内站点",
];

/// Startup seeding. For every bundled spec whose store entry exists:
///
/// 1. copy the packaged file to its stable cache path when the entry has no
///    usable cache file (deleted entries are skipped so the orphan cleaner
///    does not enter a copy/remove loop with them);
/// 2. heal the entry (`local_path` / `format` / `ready` / `rule_count`)
///    when it has no usable cache file but the stable path has one.
///
/// User-touched fields (name, target, enabled, update interval) and entries
/// pointing at a healthy downloaded file are never modified. Insertion of
/// the entries themselves is migration-only (`migrate_builtin_remote_rule_sets`).
pub fn seed(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    store: &mut crate::storage::AppStore,
) {
    for spec in BUILTIN_REMOTE_RULE_SETS.iter() {
        let Some(set) = store.rule_sets.iter_mut().find(|s| s.id == spec.id) else {
            continue;
        };
        if set.name != spec.name && FORMER_FACTORY_NAMES.contains(&set.name.as_str()) {
            set.name = spec.name.into();
        }
        if local_path_is_usable(set) {
            continue;
        }
        let stable = stable_cache_path(app_data_dir, spec);
        if !stable.is_file() {
            copy_bundled_to_cache(app_data_dir, resource_dir, spec);
        }
        let Ok(bytes) = std::fs::read(&stable) else {
            continue;
        };
        // `parse_with_rules` (not plain `parse`): the IP scan reads
        // `parsed.rules`, which only the rules-collecting variant fills in.
        let Ok(parsed) = crate::srs::parse_with_rules(&bytes) else {
            crate::app_log::warn(
                "builtin_rules",
                format!("stable cache {} is not valid SRS", stable.display()),
            );
            continue;
        };
        let contains_ip = parsed_srs_contains_ip(&parsed);
        mark_updated(set, &stable, parsed.display_count, contains_ip);
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether a parsed `.srs` carries an `ip_cidr` condition. AdGuard sets are
/// domain-only by construction (`ad_guard_domain` lines) and never scanned.
///
/// Only meaningful on a `parse_with_rules` result: plain `srs::parse` never
/// fills `rules` in, so this would read `None` as "no ip conditions" and
/// mislabel an IP-only set as domain-only.
pub(crate) fn parsed_srs_contains_ip(parsed: &crate::srs::ParsedSrs) -> bool {
    !parsed.has_adguard
        && parsed
            .rules
            .as_deref()
            .is_some_and(crate::domain::rules_contain_ip_cidr)
}

/// Mark a seeded/restored cache as a completed update so the auto scheduler
/// counts the 24h interval from seeding instead of refreshing immediately
/// (`due_ids` treats "never attempted" as due).
fn mark_updated(set: &mut RuleSet, path: &Path, count: u32, contains_ip: bool) {
    let Some(remote) = set.remote.as_mut() else {
        return;
    };
    let now = now_secs();
    remote.local_path = Some(path.to_string_lossy().to_string());
    remote.format = "binary".into();
    remote.download_status = "ready".into();
    remote.download_error = None;
    remote.last_update = Some(now);
    remote.last_attempt = Some(now);
    remote.rule_count = Some(count);
    remote.contains_ip = Some(contains_ip);
}
/// Build the factory entry for one bundled rule set, (re)copying the
/// packaged file so the restored set works offline immediately. Used by the
/// Reset paths; when no bundled copy is available the entry stays without a
/// cache and the remote scheduler falls back to downloading from the URL.
pub fn restore_set(
    app_data_dir: &Path,
    resource_dir: Option<&Path>,
    spec: &BuiltinRemoteRuleSpec,
) -> RuleSet {
    let mut set = build_builtin_remote_set(spec);
    let Some(path) = copy_bundled_to_cache(app_data_dir, resource_dir, spec) else {
        return set;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return set;
    };
    // `parse_with_rules`: the IP scan reads the collected rules (see
    // `parsed_srs_contains_ip`).
    let Ok(parsed) = crate::srs::parse_with_rules(&bytes) else {
        return set;
    };
    let contains_ip = parsed_srs_contains_ip(&parsed);
    mark_updated(&mut set, &path, parsed.display_count, contains_ip);
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::build_builtin_remote_set;

    /// `sing-box rule-set compile` output of
    /// `{"version":3,"rules":[{"ip_cidr":["1.0.1.0/24","1.0.2.0/23"]}]}`.
    const IP_ONLY_SRS: &[u8] = &[
        0x53, 0x52, 0x53, 0x02, 0x78, 0xda, 0x62, 0x64, 0x60, 0x63, 0x64, 0x80, 0x00, 0x46, 0x16,
        0x46, 0x06, 0x46, 0x06, 0x16, 0x46, 0x06, 0xe6, 0xff, 0xff, 0x19, 0x00, 0x01, 0x00, 0x00,
        0xff, 0xff, 0x06, 0x43, 0x02, 0x16,
    ];

    // Minimal valid binary rule-set produced by `sing-box rule-set compile`
    // (one domain rule); the same blob is used in `remote_rule_auto` tests.
    const VALID_SRS: &[u8] = &[
        0x53, 0x52, 0x53, 0x02, 0x78, 0xda, 0x62, 0x64, 0x60, 0x62, 0x60, 0x64, 0x00, 0x03, 0x01,
        0x08, 0x83, 0x71, 0xd5, 0xaa, 0x55, 0x3c, 0xb9, 0xf9, 0xc9, 0x7a, 0xa9, 0x39, 0x05, 0xb9,
        0x89, 0x15, 0xa9, 0x5c, 0xff, 0x19, 0x00, 0x01, 0x00, 0x00, 0xff, 0xff, 0x4d, 0xcc, 0x07,
        0x83,
    ];

    struct Sandbox {
        app_data: PathBuf,
        resources: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let base = std::env::temp_dir().join(format!(
                "satelite-builtin-rules-{tag}-{}-{nonce}",
                std::process::id()
            ));
            let sandbox = Self {
                app_data: base.join("appdata"),
                resources: base.join("resources/rule-sets"),
            };
            std::fs::create_dir_all(&sandbox.resources).unwrap();
            sandbox
        }

        fn bundle(&self, spec: &BuiltinRemoteRuleSpec, bytes: &[u8]) {
            std::fs::write(self.resources.join(spec.file), bytes).unwrap();
        }

        fn cleanup(&self) {
            let _ = std::fs::remove_dir_all(self.app_data.parent().unwrap());
        }
    }

    #[test]
    fn seed_copies_bundled_file_and_heals_entry() {
        let sandbox = Sandbox::new("heal");
        let spec = &BUILTIN_REMOTE_RULE_SETS[2];
        sandbox.bundle(spec, VALID_SRS);
        let mut store = crate::storage::AppStore::default();
        store.rule_sets.push(build_builtin_remote_set(spec));

        seed(
            &sandbox.app_data,
            Some(sandbox.resources.parent().unwrap()),
            &mut store,
        );

        let set = store.rule_sets.iter().find(|s| s.id == spec.id).unwrap();
        let remote = set.remote.as_ref().unwrap();
        let healed = stable_cache_path(&sandbox.app_data, spec);
        assert!(healed.is_file());
        assert_eq!(remote.local_path.as_deref(), Some(healed.to_str().unwrap()));
        assert_eq!(remote.format, "binary");
        assert_eq!(remote.download_status, "ready");
        assert!(remote.rule_count.is_some());
        // The scheduler counts the 24h interval from seeding; a fresh
        // install must not hit the network on its first tick.
        assert!(remote.last_attempt.is_some());
        assert!(remote.last_update.is_some());

        sandbox.cleanup();
    }

    #[test]
    fn seed_scans_ip_only_cache_with_collected_rules() {
        // Regression (sing-box 1.14, 2026-09): `parsed_srs_contains_ip` reads
        // `parsed.rules`, which only `parse_with_rules` fills in — seeding
        // must mark the IP-only geoip set `Some(true)`, or the config builder
        // emits a DNS-side reference sing-box 1.14 rejects (Legacy Address
        // Filter Fields) whenever a fakeip rule exists.
        let sandbox = Sandbox::new("ip-only");
        let spec = &BUILTIN_REMOTE_RULE_SETS[1]; // system-geoip-cn (ip_only)
        sandbox.bundle(spec, IP_ONLY_SRS);
        let mut store = crate::storage::AppStore::default();
        store.rule_sets.push(build_builtin_remote_set(spec));

        seed(
            &sandbox.app_data,
            Some(sandbox.resources.parent().unwrap()),
            &mut store,
        );

        let set = store.rule_sets.iter().find(|s| s.id == spec.id).unwrap();
        assert_eq!(set.remote.as_ref().unwrap().contains_ip, Some(true));

        sandbox.cleanup();
    }

    #[test]
    fn seed_skips_deleted_entries_and_healthy_entries() {
        let sandbox = Sandbox::new("skip");
        let spec = &BUILTIN_REMOTE_RULE_SETS[0];
        sandbox.bundle(spec, VALID_SRS);
        let mut store = crate::storage::AppStore::default();
        // Deleted: no entry at all → nothing copied.
        seed(
            &sandbox.app_data,
            Some(sandbox.resources.parent().unwrap()),
            &mut store,
        );
        assert!(!stable_cache_path(&sandbox.app_data, spec).is_file());
        assert!(store.rule_sets.is_empty());

        // Healthy: entry points at an existing downloaded file → untouched.
        let downloaded = sandbox.app_data.join("remote-rule-sets/downloaded.srs");
        std::fs::create_dir_all(downloaded.parent().unwrap()).unwrap();
        std::fs::write(&downloaded, VALID_SRS).unwrap();
        let mut set = build_builtin_remote_set(spec);
        set.remote.as_mut().unwrap().local_path = Some(downloaded.to_string_lossy().to_string());
        store.rule_sets.push(set);
        seed(
            &sandbox.app_data,
            Some(sandbox.resources.parent().unwrap()),
            &mut store,
        );
        let remote = store.rule_sets[0].remote.as_ref().unwrap();
        assert_eq!(
            remote.local_path.as_deref(),
            Some(downloaded.to_str().unwrap())
        );
        assert!(!stable_cache_path(&sandbox.app_data, spec).is_file());

        sandbox.cleanup();
    }

    #[test]
    fn seed_rejects_non_srs_bundled_payload() {
        let sandbox = Sandbox::new("reject");
        let spec = &BUILTIN_REMOTE_RULE_SETS[1];
        sandbox.bundle(spec, b"<html>error page</html>");
        let mut store = crate::storage::AppStore::default();
        store.rule_sets.push(build_builtin_remote_set(spec));

        seed(
            &sandbox.app_data,
            Some(sandbox.resources.parent().unwrap()),
            &mut store,
        );

        assert!(!stable_cache_path(&sandbox.app_data, spec).is_file());
        let remote = store.rule_sets[0].remote.as_ref().unwrap();
        assert_eq!(remote.local_path, None);

        sandbox.cleanup();
    }

    #[test]
    fn seed_renames_former_factory_names_but_keeps_user_renames() {
        let sandbox = Sandbox::new("rename");
        let spec = &BUILTIN_REMOTE_RULE_SETS[0];
        let mut store = crate::storage::AppStore::default();
        // Dev-store entry still on a former factory name → refreshed.
        let mut stale_named = build_builtin_remote_set(spec);
        stale_named.name = "内置 海外网站".into();
        store.rule_sets.push(stale_named);
        // Deliberate user rename → untouched.
        let mut renamed = build_builtin_remote_set(&BUILTIN_REMOTE_RULE_SETS[1]);
        renamed.name = "我的直连列表".into();
        store.rule_sets.push(renamed);

        seed(&sandbox.app_data, None, &mut store);

        assert_eq!(store.rule_sets[0].name, spec.name);
        assert_eq!(store.rule_sets[1].name, "我的直连列表");

        sandbox.cleanup();
    }

    #[test]
    fn copy_bundled_overwrites_stale_stable_file() {
        let sandbox = Sandbox::new("restore");
        let spec = &BUILTIN_REMOTE_RULE_SETS[0];
        sandbox.bundle(spec, VALID_SRS);
        let stable = stable_cache_path(&sandbox.app_data, spec);
        std::fs::create_dir_all(stable.parent().unwrap()).unwrap();
        std::fs::write(&stable, b"stale").unwrap();

        let copied = copy_bundled_to_cache(
            &sandbox.app_data,
            Some(sandbox.resources.parent().unwrap()),
            spec,
        );
        assert_eq!(copied.as_deref(), Some(stable.as_path()));
        assert_eq!(std::fs::read(&stable).unwrap(), VALID_SRS);

        sandbox.cleanup();
    }
}
