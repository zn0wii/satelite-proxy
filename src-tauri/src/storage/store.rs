use crate::domain::{
    build_builtin_remote_set, builtin_remote_spec, default_rules, is_builtin_remote_id,
    is_factory_set_id, sanitize_rules, AppSettings, DnsAction, DnsRuleSetKind, DnsSettings,
    DomainMatcher, ProxyNode, Rule, RuleSet, RuleSetDnsStrategy, RuleSetOwnership, RuleSetStrategy,
    RuleSetSummary, RuleTarget, RuleType, Subscription, BUILTIN_REMOTE_RULE_SETS, BUILTIN_SET_ID,
    BUILTIN_SET_NAME, GENERAL_SET_ID, GENERAL_SET_NAME, LEGACY_BUILTIN_REMOTE_IDS,
};
use crate::error::{AppError, AppResult};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STORE_BACKUP_NAME: &str = "store.backup.json";
const MAX_CORRUPT_SNAPSHOTS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppStore {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
    #[serde(default)]
    pub nodes: Vec<StoredNode>,
    #[serde(default)]
    pub settings: AppSettings,
    /// DNS module (docs/dns.md).
    #[serde(default)]
    pub dns: DnsSettings,
    /// Legacy flat rules (migrated into a user rule set once).
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub rule_sets: Vec<RuleSet>,
    /// Reusable, named node pools (keyword filter or explicit node list).
    /// Referenced by [`ProxyChain`] hops and by `Rule`/`RuleSet` chain targets.
    #[serde(default)]
    pub pools: Vec<crate::domain::NodePool>,
    /// Named, ordered multi-hop chains (built into sing-box `detour` chains).
    #[serde(default)]
    pub chains: Vec<crate::domain::ProxyChain>,
    /// Legacy single-active field; migrated into `RuleSet.enabled`.
    #[serde(default)]
    pub active_rule_set_id: Option<String>,
    /// User-assigned node names, keyed by `identity|parsed-name`.
    #[serde(default)]
    pub node_aliases: std::collections::BTreeMap<String, String>,
    /// Items this build could not parse. Kept so save() writes them back
    /// instead of dropping newer-schema data.
    #[serde(skip)]
    retained_subscriptions: Vec<Value>,
    #[serde(skip)]
    retained_nodes: Vec<Value>,
    #[serde(skip)]
    retained_rules: Vec<Value>,
    #[serde(skip)]
    retained_rule_sets: Vec<Value>,
    #[serde(skip)]
    retained_pools: Vec<Value>,
    #[serde(skip)]
    retained_chains: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredNode {
    pub subscription_id: String,
    #[serde(flatten)]
    pub node: ProxyNode,
}

impl AppStore {
    pub fn load(path: &Path, resource_dir: Option<&Path>) -> AppResult<Self> {
        let (mut store, source_raw) = Self::load_with_recovery(path, resource_dir)?;
        let schema_before = store.schema_version;
        store.settings.migrate_auto_select();
        store.settings.migrate_capture_mode();
        store.settings.migrate_api_secret_enabled();
        store.dns.ensure_rule_sets();
        store.migrate_unified_rule_sets();
        store.ensure_rule_sets();
        store.migrate_redundant_general_rule_set();
        store.migrate_remote_update_policy();
        store.migrate_plain_set_rule_targets();
        store.migrate_builtin_remote_rule_sets();
        store.migrate_remove_general_rule_set();
        store.migrate_system_rule_set_ids();
        store.migrate_file_sources_to_copied_text();
        store.migrate_chain_feature();
        store.ensure_subscription_enable_policy();
        // Self-heal legacy stores that already contain colliding node ids
        // (same name/server/port/protocol, different credentials) — they
        // produce `duplicate outbound/endpoint tag` at config generation.
        // Idempotent, so no schema-version gate is needed.
        let renamed_ids = ProxyNode::ensure_unique_ids(store.nodes.iter_mut().map(|n| &mut n.node));
        if renamed_ids > 0 {
            crate::app_log::warn(
                "storage",
                format!("检测到 {renamed_ids} 个重复节点 id，已自动改写以避免 tag 冲突"),
            );
        }
        if schema_before < 5 && source_raw.is_some() {
            let backup = path.with_file_name("store.pre-v5.backup.json");
            if !backup.exists() {
                let _ = fs::write(backup, source_raw.as_deref().unwrap_or_default());
            }
        }
        if schema_before < 6 && source_raw.is_some() {
            let backup = path.with_file_name("store.pre-v6.backup.json");
            if !backup.exists() {
                let _ = fs::write(backup, source_raw.as_deref().unwrap_or_default());
            }
        }
        if schema_before < 7 && source_raw.is_some() {
            let backup = path.with_file_name("store.pre-v7.backup.json");
            if !backup.exists() {
                let _ = fs::write(backup, source_raw.as_deref().unwrap_or_default());
            }
        }
        if schema_before < 8 && source_raw.is_some() {
            let backup = path.with_file_name("store.pre-v8.backup.json");
            if !backup.exists() {
                let _ = fs::write(backup, source_raw.as_deref().unwrap_or_default());
            }
        }
        if schema_before < 9 && source_raw.is_some() {
            let backup = path.with_file_name("store.pre-v9.backup.json");
            if !backup.exists() {
                let _ = fs::write(backup, source_raw.as_deref().unwrap_or_default());
            }
        }
        // Persist migrations (new rule files) so they survive read-only sessions.
        let _ = store.save(path);
        Ok(store)
    }

    fn load_with_recovery(
        path: &Path,
        resource_dir: Option<&Path>,
    ) -> AppResult<(Self, Option<String>)> {
        match fs::read_to_string(path) {
            Ok(raw) => match parse_store(&raw) {
                Ok(store) => {
                    if store.subscriptions.is_empty() {
                        if let Some((richer, snapshot_raw, origin)) = load_richer_snapshot(path, 0)
                        {
                            crate::app_log::warn(
                                "storage",
                                format!(
                                    "store.json had no profiles; restored {} subscriptions from {}",
                                    richer.subscriptions.len(),
                                    origin.display()
                                ),
                            );
                            return Ok((richer, Some(snapshot_raw)));
                        }
                    }
                    Ok((store, Some(raw)))
                }
                Err(primary_error) => {
                    quarantine_corrupt_store(path, &raw)?;
                    if let Some((store, snapshot_raw, origin)) = load_valid_snapshot(path) {
                        crate::app_log::warn(
                            "storage",
                            format!(
                                "store.json was invalid ({primary_error}); restored {}",
                                origin.display()
                            ),
                        );
                        Ok((store, Some(snapshot_raw)))
                    } else {
                        crate::app_log::error(
                            "storage",
                            format!(
                                "store.json was invalid ({primary_error}) and no valid backup was available; starting with defaults"
                            ),
                        );
                        Ok((Self::with_builtin_sets(resource_dir), None))
                    }
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if let Some((store, snapshot_raw, origin)) = load_valid_snapshot(path) {
                    crate::app_log::warn(
                        "storage",
                        format!("store.json was missing; restored {}", origin.display()),
                    );
                    Ok((store, Some(snapshot_raw)))
                } else {
                    Ok((Self::with_builtin_sets(resource_dir), None))
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    fn with_builtin_sets(_resource_dir: Option<&Path>) -> Self {
        let mut s = Self::default();
        s.dns.ensure_rule_sets();
        s.migrate_unified_rule_sets();
        s.ensure_rule_sets();
        s.migrate_redundant_general_rule_set();
        s.migrate_remote_update_policy();
        s
    }

    /// Ensure the rule-set bookkeeping migrations (legacy renames, general
    /// dedup, flat-rule funnel, enabled fallback). Factory content is never
    /// loaded from disk here — see the comment inside.
    pub fn ensure_rule_sets(&mut self) {
        // Migrate old id `builtin-shadowrocket` → `builtin-ruleset`
        const OLD_BUILTIN_ID: &str = "builtin-shadowrocket";
        if let Some(set) = self.rule_sets.iter_mut().find(|s| s.id == OLD_BUILTIN_ID) {
            set.id = BUILTIN_SET_ID.into();
            set.name = BUILTIN_SET_NAME.into();
            set.builtin = true;
        }
        if self.active_rule_set_id.as_deref() == Some(OLD_BUILTIN_ID) {
            self.active_rule_set_id = Some(BUILTIN_SET_ID.into());
        }

        // Rename migrated-legacy / 自定义 → 通用规则 (before factory insert)
        for set in self.rule_sets.iter_mut() {
            if set.id == "migrated-legacy" || set.name == "我的规则（迁移）" || set.name == "自定义"
            {
                set.id = GENERAL_SET_ID.into();
                set.name = GENERAL_SET_NAME.into();
                set.builtin = false;
            }
        }
        let mut seen_general = false;
        self.rule_sets.retain(|s| {
            if s.id == GENERAL_SET_ID {
                if seen_general {
                    return false;
                }
                seen_general = true;
                // general is factory but not "builtin" label
            }
            true
        });
        if let Some(g) = self.rule_sets.iter_mut().find(|s| s.id == GENERAL_SET_ID) {
            g.builtin = false;
            g.name = GENERAL_SET_NAME.into();
        }

        // Factory content is no longer loaded from disk at all (stale
        // `resources/rules/*.list` copies in dev/packaged builds used to
        // resurrect deleted legacy sets on every restart). The bundled
        // `system-*` remote sets are inserted by migrations only.

        // Migrate legacy flat rules → 通用规则
        let legacy = sanitize_rules(&self.rules);
        if !legacy.is_empty() {
            if let Some(set) = self.rule_sets.iter_mut().find(|s| s.id == GENERAL_SET_ID) {
                if set.rules.is_empty() {
                    set.rules = legacy;
                } else {
                    set.rules.extend(legacy);
                }
            } else {
                self.rule_sets.push(RuleSet {
                    id: GENERAL_SET_ID.into(),
                    name: GENERAL_SET_NAME.into(),
                    builtin: false,
                    enabled: true,
                    ownership: RuleSetOwnership::User,
                    strategy: RuleSetStrategy::Smart,
                    node_id: None,
                    node_name: None,
                    smart_include: Vec::new(),
                    smart_exclude: Vec::new(),
                    chain_id: None,
                    chain_name: None,
                    dns_strategy: RuleSetDnsStrategy::Remote,
                    remote: None,
                    dns_rules: Vec::new(),
                    rules: legacy,
                });
            }
            self.rules.clear();
        }

        // Migrate single active_rule_set_id → RuleSet.enabled (multi)
        if let Some(id) = self.active_rule_set_id.take() {
            let any_enabled = self.rule_sets.iter().any(|s| s.enabled);
            if !any_enabled {
                for s in self.rule_sets.iter_mut() {
                    s.enabled = s.id == id || is_factory_set_id(&s.id);
                }
            } else if let Some(s) = self.rule_sets.iter_mut().find(|s| s.id == id) {
                s.enabled = true;
            }
        }

        // If nothing enabled, enable all factory sets
        if !self.rule_sets.iter().any(|s| s.enabled) {
            for s in self.rule_sets.iter_mut() {
                if is_factory_set_id(&s.id) {
                    s.enabled = true;
                }
            }
        }
    }

    /// Upgrade rule sets to the unified route + DNS policy model.
    pub fn migrate_unified_rule_sets(&mut self) {
        const VERSION: u32 = 3;
        if self.schema_version >= VERSION {
            return;
        }

        if self.schema_version < 2 {
            for set in &mut self.rule_sets {
                if set.id == "builtin-shadowrocket" {
                    set.id = BUILTIN_SET_ID.into();
                    set.name = BUILTIN_SET_NAME.into();
                    set.builtin = true;
                }
                if set.id == "migrated-legacy"
                    || set.name == "我的规则（迁移）"
                    || set.name == "自定义"
                {
                    set.id = GENERAL_SET_ID.into();
                    set.name = GENERAL_SET_NAME.into();
                }
            }

            let legacy = sanitize_rules(&self.rules);
            if !legacy.is_empty() {
                if let Some(general) = self
                    .rule_sets
                    .iter_mut()
                    .find(|set| set.id == GENERAL_SET_ID)
                {
                    general.rules.extend(legacy);
                } else {
                    let mut general = RuleSet::new_user(GENERAL_SET_NAME, legacy);
                    general.id = GENERAL_SET_ID.into();
                    self.rule_sets.push(general);
                }
                self.rules.clear();
            }

            let mut migrated = Vec::new();
            for mut set in std::mem::take(&mut self.rule_sets) {
                set.ownership = if set.builtin || is_factory_set_id(&set.id) {
                    RuleSetOwnership::Builtin
                } else {
                    RuleSetOwnership::User
                };
                if set.remote.is_some() {
                    if let Some(remote) = &set.remote {
                        set.strategy = RuleSetStrategy::from_target(remote.target);
                    }
                    migrated.push(set);
                    continue;
                }

                let mut buckets: Vec<(&'static str, Vec<Rule>)> = Vec::new();
                for rule in std::mem::take(&mut set.rules) {
                    let key = match rule.target {
                        RuleTarget::Proxy => "proxy",
                        RuleTarget::Direct => "direct",
                        RuleTarget::Block => "block",
                        // Chain didn't exist when this v2 data was written;
                        // grouped with Node/Smart for the same reason those are.
                        RuleTarget::Node | RuleTarget::Smart | RuleTarget::Chain => "smart",
                    };
                    if let Some((_, rules)) = buckets.iter_mut().find(|(bucket, _)| *bucket == key)
                    {
                        rules.push(rule);
                    } else {
                        buckets.push((key, vec![rule]));
                    }
                }
                if buckets.is_empty() {
                    migrated.push(set);
                    continue;
                }
                let mixed = buckets.len() > 1;
                for (key, rules) in buckets {
                    let mut sibling = set.clone();
                    sibling.rules = rules;
                    sibling.strategy = match key {
                        "proxy" => RuleSetStrategy::Proxy,
                        "direct" => RuleSetStrategy::Direct,
                        "block" => RuleSetStrategy::Block,
                        _ => RuleSetStrategy::Smart,
                    };
                    if mixed && !(set.id == GENERAL_SET_ID && key == "direct") {
                        let suffix = match sibling.strategy {
                            RuleSetStrategy::Proxy => "代理",
                            RuleSetStrategy::Direct => "直连",
                            RuleSetStrategy::Block => "拦截",
                            // Legacy v2 data never carries Node/Filter; the
                            // arm only keeps the match exhaustive.
                            _ => "智能",
                        };
                        sibling.id = format!("{}-{key}", set.id);
                        sibling.name = format!("{} · {suffix}", set.name);
                    }
                    migrated.push(sibling);
                }
            }

            for dns_set in self
                .dns
                .rule_sets
                .iter()
                .filter(|set| set.kind == DnsRuleSetKind::Dns)
            {
                let mut buckets: Vec<(&'static str, Vec<_>)> = Vec::new();
                for rule in &dns_set.dns_rules {
                    let key = match rule.action {
                        DnsAction::Local => "direct",
                        DnsAction::Remote => "proxy",
                        DnsAction::Domestic => "smart",
                        DnsAction::Block => "block",
                    };
                    if let Some((_, rules)) = buckets.iter_mut().find(|(bucket, _)| *bucket == key)
                    {
                        rules.push(rule.clone());
                    } else {
                        buckets.push((key, vec![rule.clone()]));
                    }
                }
                for (key, dns_rules) in buckets {
                    let strategy = match key {
                        "direct" => RuleSetStrategy::Direct,
                        "proxy" => RuleSetStrategy::Proxy,
                        "block" => RuleSetStrategy::Block,
                        _ => RuleSetStrategy::Smart,
                    };
                    let suffix = match strategy {
                        RuleSetStrategy::Direct => "直连",
                        RuleSetStrategy::Proxy => "代理",
                        _ => "智能",
                    };
                    migrated.push(RuleSet {
                        id: format!("dns-{}-{key}", dns_set.id),
                        name: format!("{} · {suffix}", dns_set.name),
                        builtin: dns_set.builtin,
                        enabled: dns_set.enabled,
                        ownership: if dns_set.builtin {
                            RuleSetOwnership::Builtin
                        } else {
                            RuleSetOwnership::User
                        },
                        strategy,
                        node_id: None,
                        node_name: None,
                        smart_include: Vec::new(),
                        smart_exclude: Vec::new(),
                        chain_id: None,
                        chain_name: None,
                        dns_strategy: match key {
                            "direct" => RuleSetDnsStrategy::Local,
                            "smart" => RuleSetDnsStrategy::Domestic,
                            _ => RuleSetDnsStrategy::Remote,
                        },
                        remote: None,
                        dns_rules,
                        rules: Vec::new(),
                    });
                }
            }

            self.rule_sets = migrated;
            self.dns.unified_rules = true;
            self.dns
                .rule_sets
                .retain(|set| set.kind == DnsRuleSetKind::Hosts);
            self.dns.ensure_rule_sets();
            self.schema_version = 2;
        }

        // v3: one matcher list is shared by route and DNS. v2 briefly stored
        // DNS matchers separately; fold those entries back without losing data.
        if self.schema_version < 3 {
            for set in &mut self.rule_sets {
                set.dns_strategy = set
                    .dns_rules
                    .first()
                    .map(|rule| match rule.action {
                        DnsAction::Local => RuleSetDnsStrategy::Local,
                        DnsAction::Domestic => RuleSetDnsStrategy::Domestic,
                        DnsAction::Remote | DnsAction::Block => RuleSetDnsStrategy::Remote,
                    })
                    .unwrap_or_else(|| match set.strategy {
                        RuleSetStrategy::Direct => RuleSetDnsStrategy::Local,
                        // Legacy v2 data predates Node/Filter/Chain; group them
                        // with the proxy-like strategies for DNS pairing.
                        RuleSetStrategy::Proxy
                        | RuleSetStrategy::Block
                        | RuleSetStrategy::Node
                        | RuleSetStrategy::Filter
                        | RuleSetStrategy::Chain
                        | RuleSetStrategy::Smart => RuleSetDnsStrategy::Remote,
                    });

                let dns_rules = std::mem::take(&mut set.dns_rules);
                let mut next_ord = set.rules.iter().map(|rule| rule.ord).max().unwrap_or(0) + 10;
                for dns_rule in dns_rules {
                    let rule_type = match dns_rule.matcher {
                        DomainMatcher::Domain => RuleType::Domain,
                        DomainMatcher::DomainSuffix => RuleType::DomainSuffix,
                        DomainMatcher::DomainKeyword => RuleType::DomainKeyword,
                    };
                    if set.rules.iter().any(|rule| {
                        rule.rule_type == rule_type
                            && rule.payload.eq_ignore_ascii_case(&dns_rule.payload)
                    }) {
                        continue;
                    }
                    set.rules.push(Rule {
                        id: dns_rule.id,
                        ord: next_ord,
                        rule_type,
                        payload: dns_rule.payload,
                        target: set.strategy.route_target().unwrap_or(RuleTarget::Direct),
                        enabled: dns_rule.enabled,
                        node_id: None,
                        node_name: None,
                        smart_include: Vec::new(),
                        smart_exclude: Vec::new(),
                        chain_id: None,
                        chain_name: None,
                    });
                    next_ord += 10;
                }
            }
            self.schema_version = VERSION;
        }
    }

    /// v4 removes the old factory "通用规则" because all seven entries are
    /// already present in the built-in direct set. Preserve edited copies as a
    /// normal user set; only the untouched factory payload is redundant.
    pub fn migrate_redundant_general_rule_set(&mut self) {
        const VERSION: u32 = 4;
        if self.schema_version >= VERSION {
            return;
        }

        if let Some(index) = self
            .rule_sets
            .iter()
            .position(|set| set.id == GENERAL_SET_ID)
        {
            if same_rules_ignoring_storage_fields(&self.rule_sets[index].rules, &default_rules()) {
                self.rule_sets.remove(index);
            } else {
                let set = &mut self.rule_sets[index];
                set.builtin = false;
                set.ownership = RuleSetOwnership::User;
            }
        }
        self.schema_version = VERSION;
    }

    /// v6: rule targets became per-rule under plain strategies (previously
    /// the set strategy always won). Normalize every non-smart local set to
    /// the semantics users observed so far — all rules retargeted to the set
    /// strategy — so the upgrade changes no routing. Per-rule choices made
    /// after the migration are kept (this runs once).
    pub fn migrate_plain_set_rule_targets(&mut self) {
        const VERSION: u32 = 6;
        if self.schema_version >= VERSION {
            return;
        }
        use crate::domain::{RuleSetStrategy, RuleTarget};
        for set in &mut self.rule_sets {
            if set.remote.is_some() || set.strategy == RuleSetStrategy::Smart {
                continue;
            }
            let target = match set.strategy {
                RuleSetStrategy::Direct => RuleTarget::Direct,
                RuleSetStrategy::Block => RuleTarget::Block,
                _ => RuleTarget::Proxy,
            };
            for rule in &mut set.rules {
                rule.target = target.clone();
            }
        }
        self.schema_version = VERSION;
    }

    /// v5: remote updates used to run hourly without an explicit user choice.
    /// Upgrade existing remote sets to opt-in scheduling; newly created sets
    /// persist the user's selected interval and are already on schema v5.
    pub fn migrate_remote_update_policy(&mut self) {
        const VERSION: u32 = 5;
        if self.schema_version >= VERSION {
            return;
        }
        for set in &mut self.rule_sets {
            if let Some(remote) = set.remote.as_mut() {
                remote.update_interval = "disabled".into();
            }
        }
        self.schema_version = VERSION;
    }

    /// v7: the factory rule sets became the three bundled remote rule sets
    /// (geo cn / geoip cn / geolocation-!cn). Insert any missing one at the
    /// front in match-priority order. Runs once — afterwards deleting a
    /// builtin remote set sticks until Reset restores it. Legacy `builtin-*`
    /// list sets (e.g. `builtin-ruleset`) are deliberately left untouched:
    /// still recognized, deletable, never resurrected by Reset.
    pub fn migrate_builtin_remote_rule_sets(&mut self) {
        const VERSION: u32 = 7;
        if self.schema_version >= VERSION {
            return;
        }
        let mut insert_at = 0;
        for spec in BUILTIN_REMOTE_RULE_SETS.iter() {
            if self.rule_sets.iter().any(|set| set.id == spec.id) {
                continue;
            }
            self.rule_sets
                .insert(insert_at, build_builtin_remote_set(spec));
            insert_at += 1;
        }
        self.schema_version = VERSION;
    }

    /// v8: the 通用规则 set is gone — LAN/localhost bypass became a routing
    /// setting (`AppSettings::bypass_lan`) and the builtin remote rule sets
    /// cover geo routing. Remove any remaining general set (including
    /// user-edited copies preserved by v4) and legacy flat rule leftovers.
    pub fn migrate_remove_general_rule_set(&mut self) {
        const VERSION: u32 = 8;
        if self.schema_version >= VERSION {
            return;
        }
        self.rule_sets.retain(|set| set.id != GENERAL_SET_ID);
        self.rules.clear();
        self.schema_version = VERSION;
    }

    /// v9: the bundled remote rule sets moved to the `system-` id prefix so
    /// legacy `builtin-*` list sets can never be conflated with them, and
    /// those legacy sets are downgraded to plain user sets (no 内置 badge,
    /// plainly deletable). Runs once.
    pub fn migrate_system_rule_set_ids(&mut self) {
        const VERSION: u32 = 9;
        if self.schema_version >= VERSION {
            return;
        }
        for (old, new) in LEGACY_BUILTIN_REMOTE_IDS {
            let has_new = self.rule_sets.iter().any(|set| set.id == new);
            if !has_new {
                if let Some(set) = self.rule_sets.iter_mut().find(|set| set.id == old) {
                    set.id = new.to_string();
                    // The stable cache file is named after the id; drop the
                    // stale path so seeding re-copies under the new name.
                    if let Some(remote) = set.remote.as_mut() {
                        remote.local_path = None;
                    }
                }
            } else {
                // Both present (reset raced a rename): the legacy twin is a
                // duplicate and goes away.
                self.rule_sets.retain(|set| set.id != old);
            }
        }
        for set in self.rule_sets.iter_mut() {
            // After the rename above nothing owned by the app starts with
            // `builtin-` anymore; anything left is a legacy list set.
            if set.id.starts_with("builtin-") {
                set.builtin = false;
                set.ownership = RuleSetOwnership::User;
            }
        }
        self.schema_version = VERSION;
    }

    /// v10: introduces `pools`/`chains` (Proxy Chain feature). No data to
    /// transform — both default to empty on stores created before this
    /// version — this migration only bumps the schema version so future
    /// migrations can gate on "chain feature exists".
    pub fn migrate_chain_feature(&mut self) {
        const VERSION: u32 = 10;
        if self.schema_version >= VERSION {
            return;
        }
        self.schema_version = VERSION;
    }

    pub fn save(&self, path: &Path) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = serialize_store(self)
            .map_err(|e| AppError::Storage(format!("serialize store: {e}")))?;
        if let Ok(previous_raw) = fs::read_to_string(path) {
            if parse_store(&previous_raw).is_ok() {
                replace_file(&backup_path(path), previous_raw.as_bytes())?;
            }
        }
        replace_file(path, raw.as_bytes())?;
        Ok(())
    }

    pub fn upsert_subscription(
        &mut self,
        sub: Subscription,
        nodes: Vec<ProxyNode>,
    ) -> AppResult<()> {
        let id = sub.id.clone();
        self.nodes.retain(|n| n.subscription_id != id);
        if let Some(existing) = self.subscriptions.iter_mut().find(|s| s.id == id) {
            *existing = sub;
        } else {
            self.subscriptions.push(sub);
        }
        for mut node in nodes {
            self.apply_node_alias(&mut node);
            self.nodes.push(StoredNode {
                subscription_id: id.clone(),
                node,
            });
        }
        Ok(())
    }

    pub fn remove_subscription(&mut self, id: &str) -> AppResult<()> {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| s.id != id);
        if self.subscriptions.len() == before {
            return Err(AppError::NotFound(id.to_string()));
        }
        self.nodes.retain(|n| n.subscription_id != id);
        if self.settings.runtime_source().singbox_id() == Some(id) {
            self.settings
                .set_runtime_source(crate::domain::RuntimeSource::Generated);
        }
        // If removed was the only enabled, enable first remaining.
        if !self.subscriptions.iter().any(|s| s.enabled) {
            if let Some(first) = self.subscriptions.first_mut() {
                first.enabled = true;
            }
        }
        self.ensure_current_node_valid();
        Ok(())
    }

    pub fn get_subscription(&self, id: &str) -> Option<&Subscription> {
        self.subscriptions.iter().find(|s| s.id == id)
    }

    /// Copy leftover path-based file profiles into stored text so we no longer
    /// depend on an external path.
    pub fn migrate_file_sources_to_copied_text(&mut self) {
        for sub in &mut self.subscriptions {
            let crate::domain::SubscriptionSource::File { path } = &sub.source else {
                continue;
            };
            if path.is_empty() || path.starts_with("satelite:") {
                continue;
            }
            match std::fs::read_to_string(path) {
                Ok(content) if !content.is_empty() => {
                    sub.source = crate::domain::SubscriptionSource::Text { content };
                    sub.auto_update = false;
                }
                _ => {
                    sub.auto_update = false;
                }
            }
        }
    }

    pub fn enabled_nodes(&self) -> Vec<ProxyNode> {
        let enabled: std::collections::HashSet<_> = self
            .subscriptions
            .iter()
            .filter(|s| s.enabled && s.source.contributes_nodes())
            .map(|s| s.id.as_str())
            .collect();
        self.nodes
            .iter()
            .filter(|n| enabled.contains(n.subscription_id.as_str()))
            .map(|n| n.node.clone())
            .collect()
    }

    /// Sorted ids of the nodes the generated config would include (same filter
    /// as [`Self::enabled_nodes`]). Subscription imports compare this before
    /// and after to decide whether the running core needs a rebuild — node ids
    /// are content hashes, so a renamed or rotated node silently changes the
    /// id set the running core was built from.
    pub fn enabled_node_ids_sorted(&self) -> Vec<String> {
        let enabled: std::collections::HashSet<_> = self
            .subscriptions
            .iter()
            .filter(|s| s.enabled && s.source.contributes_nodes())
            .map(|s| s.id.as_str())
            .collect();
        let mut ids: Vec<String> = self
            .nodes
            .iter()
            .filter(|n| enabled.contains(n.subscription_id.as_str()))
            .map(|n| n.node.id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Exclusive (default): only one subscription enabled.
    /// Mix: multiple can be enabled.
    pub fn ensure_subscription_enable_policy(&mut self) {
        let generated: Vec<String> = self
            .subscriptions
            .iter()
            .filter(|s| s.source.contributes_nodes())
            .map(|s| s.id.clone())
            .collect();
        if generated.is_empty() {
            return;
        }
        if !self.settings.mix_mode {
            let enabled: Vec<String> = self
                .subscriptions
                .iter()
                .filter(|s| s.enabled && s.source.contributes_nodes())
                .map(|s| s.id.clone())
                .collect();
            if enabled.len() > 1 {
                let keep = enabled[0].clone();
                for s in &mut self.subscriptions {
                    if s.source.contributes_nodes() {
                        s.enabled = s.id == keep;
                    }
                }
            } else if enabled.is_empty() {
                if let Some(first) = generated.first() {
                    for s in &mut self.subscriptions {
                        if s.source.contributes_nodes() {
                            s.enabled = s.id == *first;
                        }
                    }
                }
            }
        } else if !self
            .subscriptions
            .iter()
            .any(|s| s.enabled && s.source.contributes_nodes())
        {
            if let Some(first) = generated.first() {
                for s in &mut self.subscriptions {
                    if s.id == *first {
                        s.enabled = true;
                    }
                }
            }
        }
        self.ensure_current_node_valid();
    }

    /// Homepage ··· menu: `generated` or a stored complete sing-box archive.
    pub fn set_runtime_source(&mut self, source: crate::domain::RuntimeSource) -> AppResult<()> {
        if let crate::domain::RuntimeSource::Singbox { id } = &source {
            let ok = self.subscriptions.iter().any(|s| {
                s.id == *id && matches!(s.source, crate::domain::SubscriptionSource::Singbox { .. })
            });
            if !ok {
                return Err(AppError::NotFound(id.clone()));
            }
        }
        self.settings.set_runtime_source(source);
        Ok(())
    }

    /// Click card: exclusive → enable only this; Mix → toggle this.
    /// Does not change which file the kernel launches.
    pub fn activate_subscription(&mut self, id: &str) -> AppResult<()> {
        let contributes = self
            .subscriptions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.source.contributes_nodes())
            .ok_or_else(|| AppError::NotFound(id.to_string()))?;
        if !contributes {
            return Ok(());
        }
        if self.settings.mix_mode {
            let currently = self
                .subscriptions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.enabled)
                .unwrap_or(false);
            // Don't allow disabling the last enabled subscription.
            if currently {
                let enabled_count = self
                    .subscriptions
                    .iter()
                    .filter(|s| s.enabled && s.source.contributes_nodes())
                    .count();
                if enabled_count <= 1 {
                    return Ok(());
                }
                if let Some(s) = self.subscriptions.iter_mut().find(|s| s.id == id) {
                    s.enabled = false;
                }
            } else if let Some(s) = self.subscriptions.iter_mut().find(|s| s.id == id) {
                s.enabled = true;
            }
        } else {
            for s in &mut self.subscriptions {
                s.enabled = s.id == id;
            }
        }
        self.ensure_current_node_valid();
        Ok(())
    }

    pub fn set_mix_mode(&mut self, mix: bool) -> AppResult<()> {
        self.settings.mix_mode = mix;
        self.ensure_subscription_enable_policy();
        Ok(())
    }

    /// Drop current_node if it is not in any enabled subscription.
    pub fn ensure_current_node_valid(&mut self) {
        if let Some(ref cur) = self.settings.current_node_id {
            let still = self.nodes.iter().any(|n| {
                &n.node.id == cur && {
                    self.subscriptions
                        .iter()
                        .any(|s| s.enabled && s.id == n.subscription_id)
                }
            });
            if !still {
                self.settings.current_node_id = self.enabled_nodes().first().map(|n| n.id.clone());
            }
        }
    }

    /// New subscription: enable only when no other is enabled (or none exist).
    pub fn prepare_new_subscription_enabled(&self, sub: &mut Subscription) {
        if !sub.source.contributes_nodes() {
            sub.enabled = false;
            return;
        }
        let any_enabled = self
            .subscriptions
            .iter()
            .any(|s| s.enabled && s.id != sub.id && s.source.contributes_nodes());
        if any_enabled {
            sub.enabled = false;
        } else {
            sub.enabled = true;
        }
    }

    pub fn find_node(&self, id: &str) -> Option<&ProxyNode> {
        self.nodes.iter().find(|n| n.node.id == id).map(|n| &n.node)
    }

    pub fn node_alias_key(node: &ProxyNode) -> String {
        node.instance_key()
    }

    fn apply_node_alias(&self, node: &mut ProxyNode) {
        if let Some(alias) = self.node_aliases.get(&Self::node_alias_key(node)) {
            let alias = alias.trim();
            if !alias.is_empty() {
                node.name = alias.to_string();
            }
        }
    }

    pub fn rename_node(&mut self, id: &str, name: String) -> AppResult<ProxyNode> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::InvalidProxy {
                name: id.to_string(),
                reason: "name is required".into(),
            });
        }
        let node = self
            .nodes
            .iter_mut()
            .find(|n| n.node.id == id)
            .ok_or_else(|| AppError::NotFound(id.to_string()))?;
        let identity = node.node.identity_key();
        let old_name = node.node.name.clone();
        let parsed_key = format!("{identity}|{old_name}");
        let prefix = format!("{identity}|");
        let source_key = self
            .node_aliases
            .iter()
            .find_map(|(key, value)| {
                if key.starts_with(&prefix) && value == &old_name {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .unwrap_or(parsed_key);
        node.node.name = name.clone();
        self.node_aliases.insert(source_key, name);
        Ok(node.node.clone())
    }

    pub fn update_node_latency(
        &mut self,
        id: &str,
        latency_ms: Option<u32>,
        latency_at: i64,
    ) -> bool {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.node.id == id) {
            n.node.latency_ms = latency_ms;
            n.node.latency_at = Some(latency_at);
            true
        } else {
            false
        }
    }

    /// Merge rules from all **enabled** rule sets (set order, then rule.ord).
    pub fn enabled_rules_sorted(&self) -> Vec<Rule> {
        let mut out = Vec::new();
        for set in &self.rule_sets {
            if !set.enabled {
                continue;
            }
            let mut rules: Vec<_> = set
                .rules
                .iter()
                .filter(|r| r.enabled)
                .filter(|r| !matches!(r.rule_type, crate::domain::RuleType::Geoip))
                .cloned()
                .collect();
            rules.sort_by_key(|r| r.ord);
            out.extend(rules);
        }
        // No implicit fallback list anymore: localhost/LAN bypass comes from
        // the `bypass_lan` setting (builder-level), geo defaults from the
        // builtin remote rule sets.
        out
    }

    pub fn list_rule_set_summaries(&self) -> Vec<RuleSetSummary> {
        self.rule_sets
            .iter()
            .map(|s| RuleSetSummary {
                id: s.id.clone(),
                name: s.name.clone(),
                builtin: s.builtin,
                rule_count: s
                    .remote
                    .as_ref()
                    .and_then(|remote| remote.rule_count)
                    .unwrap_or(s.rules.len() as u32),
                enabled: s.enabled,
                ownership: s.ownership,
                strategy: s.strategy,
                node_id: s.node_id.clone(),
                node_name: s.node_name.clone(),
                smart_include: s.smart_include.clone(),
                smart_exclude: s.smart_exclude.clone(),
                chain_id: s.chain_id.clone(),
                chain_name: s.chain_name.clone(),
                dns_strategy: s.dns_strategy,
                resettable: is_builtin_remote_id(&s.id),
                remote: s.remote.clone(),
            })
            .collect()
    }

    /// Enable/disable a rule set for routing (multiple can be enabled).
    pub fn set_rule_set_enabled(&mut self, id: &str, enabled: bool) -> AppResult<()> {
        let set = self
            .rule_sets
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| AppError::NotFound(id.to_string()))?;
        // Enabling an empty set would change nothing in the generated config
        // (no matchable local rules / no downloaded remote cache).
        if enabled && crate::config::rule_set_is_empty_for_config(set) {
            return Err(crate::error::AppError::Config(
                "规则集暂无可生效的规则，无法启用".into(),
            ));
        }
        set.enabled = enabled;
        Ok(())
    }

    pub fn get_rule_set(&self, id: &str) -> Option<&RuleSet> {
        self.rule_sets.iter().find(|s| s.id == id)
    }

    pub fn upsert_rule_in_set(&mut self, set_id: &str, rule: Rule) -> AppResult<Rule> {
        let set = self
            .rule_sets
            .iter_mut()
            .find(|s| s.id == set_id)
            .ok_or_else(|| AppError::NotFound(set_id.to_string()))?;
        if let Some(existing) = set.rules.iter_mut().find(|r| r.id == rule.id) {
            *existing = rule.clone();
        } else {
            set.rules.push(rule.clone());
        }
        Ok(rule)
    }

    pub fn remove_rule_from_set(&mut self, set_id: &str, rule_id: &str) -> AppResult<()> {
        let set = self
            .rule_sets
            .iter_mut()
            .find(|s| s.id == set_id)
            .ok_or_else(|| AppError::NotFound(set_id.to_string()))?;
        let before = set.rules.len();
        set.rules.retain(|r| r.id != rule_id);
        if set.rules.len() == before {
            return Err(AppError::NotFound(rule_id.to_string()));
        }
        Ok(())
    }

    /// Validate + normalize whole-set route parameters: a node pin for
    /// `node` targets (must exist), normalized keyword filters for `smart`
    /// targets (no include/exclude overlap). Returns (pin, include, exclude).
    fn resolve_set_route_params(
        &self,
        target: crate::domain::RuleTarget,
        node_id: Option<String>,
        smart_include: Vec<String>,
        smart_exclude: Vec<String>,
        chain_id: Option<String>,
    ) -> AppResult<(
        Option<(String, String)>,
        Vec<String>,
        Vec<String>,
        Option<(String, String)>,
    )> {
        use crate::domain::{keyword_list_overlap, Rule, RuleTarget};
        let pin = if target == RuleTarget::Node {
            let nid = node_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| crate::error::AppError::Config("请选择节点".into()))?;
            let name = self
                .nodes
                .iter()
                .find(|stored| stored.node.id == nid)
                .map(|stored| stored.node.name.clone())
                .ok_or_else(|| crate::error::AppError::Config("指定的节点不存在".into()))?;
            Some((nid.to_string(), name))
        } else {
            None
        };
        let include = Rule::normalize_keywords(&smart_include);
        let exclude = Rule::normalize_keywords(&smart_exclude);
        if let Some(k) = keyword_list_overlap(&include, &exclude).first() {
            return Err(crate::error::AppError::Config(format!(
                "关键词不能同时出现在白名单和黑名单中：{k}"
            )));
        }
        let chain_pin = if target == RuleTarget::Chain {
            let cid = chain_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| crate::error::AppError::Config("请选择链路".into()))?;
            let name = self
                .chains
                .iter()
                .find(|chain| chain.id == cid)
                .map(|chain| chain.name.clone())
                .ok_or_else(|| crate::error::AppError::Config("指定的链路不存在".into()))?;
            Some((cid.to_string(), name))
        } else {
            None
        };
        Ok((pin, include, exclude, chain_pin))
    }

    /// Apply one whole-set route target + parameters: strategy flip, set-level
    /// pin/keyword fields, and the recommended DNS pairing. Shared by the
    /// batch path and both create paths. `node` → Node, `smart` → Filter,
    /// `chain` → Chain.
    fn apply_set_route(
        set: &mut RuleSet,
        target: crate::domain::RuleTarget,
        pin: &Option<(String, String)>,
        include: &[String],
        exclude: &[String],
        chain_pin: &Option<(String, String)>,
    ) {
        use crate::domain::{RuleSetStrategy, RuleTarget};
        set.strategy = RuleSetStrategy::from_target(target);
        set.node_id = pin.as_ref().map(|(id, _)| id.clone());
        set.node_name = pin.as_ref().map(|(_, name)| name.clone());
        set.smart_include = if target == RuleTarget::Smart {
            include.to_vec()
        } else {
            Vec::new()
        };
        set.smart_exclude = if target == RuleTarget::Smart {
            exclude.to_vec()
        } else {
            Vec::new()
        };
        set.chain_id = chain_pin.as_ref().map(|(id, _)| id.clone());
        set.chain_name = chain_pin.as_ref().map(|(_, name)| name.clone());
        if let Some(dns) = set.strategy.recommended_dns_strategy() {
            set.dns_strategy = dns;
        }
    }

    /// Apply one target to EVERY rule of a set (batch set-routes). Works for
    /// local sets (rewrites each rule) and remote sets (whole-set target —
    /// node pins and keyword filters live on the set level there).
    /// proxy/direct/block collapse to a plain strategy; node/smart become the
    /// whole-set Node/Filter strategies. Returns (set, needs_core_restart).
    pub fn batch_set_rule_targets(
        &mut self,
        id: &str,
        target: crate::domain::RuleTarget,
        node_id: Option<String>,
        smart_include: Vec<String>,
        smart_exclude: Vec<String>,
        chain_id: Option<String>,
    ) -> AppResult<(RuleSet, bool)> {
        use crate::domain::RuleTarget;
        let (pin, include, exclude, chain_pin) =
            self.resolve_set_route_params(target, node_id, smart_include, smart_exclude, chain_id)?;
        let set = self
            .rule_sets
            .iter_mut()
            .find(|set| set.id == id)
            .ok_or_else(|| crate::error::AppError::NotFound(id.to_string()))?;
        Self::apply_set_route(set, target, &pin, &include, &exclude, &chain_pin);
        if let Some(remote) = set.remote.as_mut() {
            remote.target = target;
        }
        for rule in &mut set.rules {
            rule.target = target;
            rule.node_id = pin.as_ref().map(|(id, _)| id.clone());
            rule.node_name = pin.as_ref().map(|(_, name)| name.clone());
            rule.smart_include = if target == RuleTarget::Smart {
                include.clone()
            } else {
                Vec::new()
            };
            rule.smart_exclude = if target == RuleTarget::Smart {
                exclude.clone()
            } else {
                Vec::new()
            };
            rule.chain_id = chain_pin.as_ref().map(|(id, _)| id.clone());
            rule.chain_name = chain_pin.as_ref().map(|(_, name)| name.clone());
        }
        let needs_restart = !crate::config::rule_set_is_empty_for_config(set);
        Ok((set.clone(), needs_restart))
    }

    /// Create a local user set with an initial whole-set route (the new-set
    /// dialog's 路由 choice, mirroring the remote flow). DNS strategy follows
    /// the recommended pairing via the same helper the flip path uses.
    /// `node`/`smart`/`chain` targets carry the set-level pin / keyword filters / chain ref.
    pub fn create_local_rule_set(
        &mut self,
        name: &str,
        target: crate::domain::RuleTarget,
        node_id: Option<String>,
        smart_include: Vec<String>,
        smart_exclude: Vec<String>,
        chain_id: Option<String>,
    ) -> AppResult<RuleSet> {
        let (pin, include, exclude, chain_pin) =
            self.resolve_set_route_params(target, node_id, smart_include, smart_exclude, chain_id)?;
        let mut set = RuleSet::new_user(name, vec![]);
        Self::apply_set_route(&mut set, target, &pin, &include, &exclude, &chain_pin);
        // New sets start disabled — enable once they hold effective rules.
        set.enabled = false;
        self.rule_sets.insert(0, set.clone());
        Ok(set)
    }

    pub fn create_remote_rule_set(
        &mut self,
        name: &str,
        url: &str,
        target: crate::domain::RuleTarget,
        update_interval: &str,
        node_id: Option<String>,
        smart_include: Vec<String>,
        smart_exclude: Vec<String>,
        chain_id: Option<String>,
    ) -> AppResult<RuleSet> {
        let (pin, include, exclude, chain_pin) =
            self.resolve_set_route_params(target, node_id, smart_include, smart_exclude, chain_id)?;
        let mut set = RuleSet::new_remote(name, url, target);
        if let Some(remote) = set.remote.as_mut() {
            remote.update_interval = update_interval.to_string();
        }
        Self::apply_set_route(&mut set, target, &pin, &include, &exclude, &chain_pin);
        // New sets start disabled — enable after the first successful
        // download produces a cached rule file.
        set.enabled = false;
        self.rule_sets.insert(0, set.clone());
        Ok(set)
    }

    pub fn enabled_rule_sets(&self) -> Vec<RuleSet> {
        self.rule_sets
            .iter()
            .filter(|set| set.enabled)
            .cloned()
            .collect()
    }

    // ---- Node pools -----------------------------------------------------

    pub fn create_pool(
        &mut self,
        name: &str,
        mode: crate::domain::PoolMode,
    ) -> AppResult<crate::domain::NodePool> {
        let n = name.trim();
        if n.is_empty() {
            return Err(AppError::Config("节点池名称不能为空".into()));
        }
        if n.chars().count() > 64 {
            return Err(AppError::Config("节点池名称过长（最多 64 字）".into()));
        }
        if self.pools.iter().any(|p| p.name.eq_ignore_ascii_case(n)) {
            return Err(AppError::Config(format!("已存在同名节点池「{n}」")));
        }
        Self::validate_pool_mode(&mode, &self.nodes)?;
        let pool = crate::domain::NodePool::new(n, mode);
        self.pools.push(pool.clone());
        Ok(pool)
    }

    pub fn update_pool(
        &mut self,
        id: &str,
        name: &str,
        mode: crate::domain::PoolMode,
    ) -> AppResult<crate::domain::NodePool> {
        let n = name.trim();
        if n.is_empty() {
            return Err(AppError::Config("节点池名称不能为空".into()));
        }
        if n.chars().count() > 64 {
            return Err(AppError::Config("节点池名称过长（最多 64 字）".into()));
        }
        if self
            .pools
            .iter()
            .any(|p| p.id != id && p.name.eq_ignore_ascii_case(n))
        {
            return Err(AppError::Config(format!("已存在同名节点池「{n}」")));
        }
        Self::validate_pool_mode(&mode, &self.nodes)?;
        let pool = self
            .pools
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::NotFound(id.to_string()))?;
        pool.name = n.to_string();
        pool.mode = mode;
        Ok(pool.clone())
    }

    pub fn delete_pool(&mut self, id: &str) -> AppResult<()> {
        let referencing_chains: Vec<String> = self
            .chains
            .iter()
            .filter(|c| {
                c.hops.iter().any(
                    |h| matches!(h, crate::domain::ChainHop::Pool { pool_id } if pool_id == id),
                )
            })
            .map(|c| c.name.clone())
            .collect();
        if !referencing_chains.is_empty() {
            return Err(AppError::Config(format!(
                "节点池被以下链路引用，无法删除：{}",
                referencing_chains.join("、")
            )));
        }
        let referencing_rules = self.pool_reference_names(id);
        if !referencing_rules.is_empty() {
            return Err(AppError::Config(format!(
                "节点池被以下规则/规则集引用，无法删除：{}",
                referencing_rules.join("、")
            )));
        }
        let before = self.pools.len();
        self.pools.retain(|p| p.id != id);
        if self.pools.len() == before {
            return Err(AppError::NotFound(id.to_string()));
        }
        Ok(())
    }

    fn validate_pool_mode(mode: &crate::domain::PoolMode, nodes: &[StoredNode]) -> AppResult<()> {
        use crate::domain::PoolMode;
        match mode {
            PoolMode::Explicit { node_ids } => {
                if node_ids.is_empty() {
                    return Err(AppError::Config("显式节点池至少需要选择一个节点".into()));
                }
                for nid in node_ids {
                    if !nodes.iter().any(|s| s.node.id == *nid) {
                        return Err(AppError::Config(format!(
                            "节点池引用了不存在的节点 id：{nid}"
                        )));
                    }
                }
            }
            PoolMode::Keyword { include, exclude } => {
                let inc = crate::domain::Rule::normalize_keywords(include);
                let exc = crate::domain::Rule::normalize_keywords(exclude);
                if let Some(k) = crate::domain::keyword_list_overlap(&inc, &exc).first() {
                    return Err(AppError::Config(format!(
                        "关键词不能同时出现在白名单和黑名单中：{k}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Pools/chains do not yet participate in Rule/RuleSet as a direct
    /// `RuleTarget::Pool` (v1 only exposes pools as chain hops), so this is
    /// currently always empty — kept as a seam for that future target so
    /// `delete_pool` doesn't need reshaping when it lands.
    fn pool_reference_names(&self, _pool_id: &str) -> Vec<String> {
        Vec::new()
    }

    // ---- Proxy chains -----------------------------------------------------

    pub fn create_chain(
        &mut self,
        name: &str,
        hops: Vec<crate::domain::ChainHop>,
    ) -> AppResult<crate::domain::ProxyChain> {
        let n = name.trim();
        if n.is_empty() {
            return Err(AppError::Config("链路名称不能为空".into()));
        }
        if n.chars().count() > 64 {
            return Err(AppError::Config("链路名称过长（最多 64 字）".into()));
        }
        if self.chains.iter().any(|c| c.name.eq_ignore_ascii_case(n)) {
            return Err(AppError::Config(format!("已存在同名链路「{n}」")));
        }
        self.validate_chain_hops(&hops)?;
        let chain = crate::domain::ProxyChain::new(n, hops);
        self.chains.push(chain.clone());
        Ok(chain)
    }

    pub fn update_chain(
        &mut self,
        id: &str,
        name: &str,
        hops: Vec<crate::domain::ChainHop>,
    ) -> AppResult<crate::domain::ProxyChain> {
        let n = name.trim();
        if n.is_empty() {
            return Err(AppError::Config("链路名称不能为空".into()));
        }
        if n.chars().count() > 64 {
            return Err(AppError::Config("链路名称过长（最多 64 字）".into()));
        }
        if self
            .chains
            .iter()
            .any(|c| c.id != id && c.name.eq_ignore_ascii_case(n))
        {
            return Err(AppError::Config(format!("已存在同名链路「{n}」")));
        }
        self.validate_chain_hops(&hops)?;
        let chain = self
            .chains
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| AppError::NotFound(id.to_string()))?;
        chain.name = n.to_string();
        chain.hops = hops;
        Ok(chain.clone())
    }

    /// Distinct rule-set names referencing each chain — the same reference
    /// detection `delete_chain`'s guard uses (set-level pin OR any single
    /// rule), deduped per set. Powers the chain list page's "used by N rule
    /// sets" hint so users can see deletion impact up front.
    pub fn chain_rule_usage(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut usage: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for set in &self.rule_sets {
            let mut referenced_ids: Vec<&str> = Vec::new();
            if let Some(id) = set.chain_id.as_deref() {
                referenced_ids.push(id);
            }
            referenced_ids.extend(set.rules.iter().filter_map(|r| r.chain_id.as_deref()));
            for id in referenced_ids {
                let names = usage.entry(id.to_string()).or_default();
                if !names.iter().any(|n| n == &set.name) {
                    names.push(set.name.clone());
                }
            }
        }
        usage
    }

    pub fn delete_chain(&mut self, id: &str) -> AppResult<()> {
        let referencing_rules: Vec<String> = self
            .rule_sets
            .iter()
            .flat_map(|set| {
                let mut names: Vec<String> = Vec::new();
                if set.chain_id.as_deref() == Some(id) {
                    names.push(set.name.clone());
                }
                names.extend(
                    set.rules
                        .iter()
                        .filter(|r| r.chain_id.as_deref() == Some(id))
                        .map(|_| set.name.clone()),
                );
                names
            })
            .collect();
        if !referencing_rules.is_empty() {
            let mut uniq = referencing_rules;
            uniq.sort();
            uniq.dedup();
            return Err(AppError::Config(format!(
                "链路被以下规则集引用，无法删除：{}",
                uniq.join("、")
            )));
        }
        let before = self.chains.len();
        self.chains.retain(|c| c.id != id);
        if self.chains.len() == before {
            return Err(AppError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// Structural validation: at least 2 hops (single-hop chains are just a
    /// node/pool pin — use `RuleTarget::Node`/keyword `Filter` directly), and
    /// every referenced node/pool id must exist.
    fn validate_chain_hops(&self, hops: &[crate::domain::ChainHop]) -> AppResult<()> {
        use crate::domain::ChainHop;
        if hops.len() < 2 {
            return Err(AppError::Config(
                "链路至少需要 2 跳；单跳链路请直接用节点/关键字池路由".into(),
            ));
        }
        for hop in hops {
            match hop {
                ChainHop::Node { node_id } => {
                    if !self.nodes.iter().any(|s| s.node.id == *node_id) {
                        return Err(AppError::Config(format!(
                            "链路引用了不存在的节点 id：{node_id}"
                        )));
                    }
                }
                ChainHop::Pool { pool_id } => {
                    if !self.pools.iter().any(|p| p.id == *pool_id) {
                        return Err(AppError::Config(format!(
                            "链路引用了不存在的节点池 id：{pool_id}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Reorder rule sets by id list. Unknown ids ignored; missing ids appended at end.
    /// List order = match priority (first set matched first).
    pub fn reorder_rule_sets(&mut self, ordered_ids: &[String]) -> AppResult<()> {
        if ordered_ids.is_empty() {
            return Err(AppError::Config("ordered ids empty".into()));
        }
        let mut by_id: std::collections::HashMap<String, RuleSet> = self
            .rule_sets
            .drain(..)
            .map(|s| (s.id.clone(), s))
            .collect();
        let mut next = Vec::with_capacity(by_id.len());
        for id in ordered_ids {
            if let Some(s) = by_id.remove(id) {
                next.push(s);
            }
        }
        // Keep any sets not mentioned (shouldn't happen) at the end
        for (_, s) in by_id {
            next.push(s);
        }
        if next.is_empty() {
            return Err(AppError::Config("no rule sets after reorder".into()));
        }
        self.rule_sets = next;
        Ok(())
    }

    pub fn delete_rule_set(&mut self, id: &str) -> AppResult<()> {
        let before = self.rule_sets.len();
        self.rule_sets.retain(|set| set.id != id);
        if self.rule_sets.len() == before {
            return Err(AppError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// Restore **one** factory set to defaults. Only the bundled `system-*`
    /// remote rule sets are restorable — factory content is never loaded
    /// from disk, so legacy `builtin-*` list ids always error here.
    /// Returns the restored set plus stale cache files to delete after the
    /// core restart (the running core may still be reading them).
    pub fn reset_rule_set(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        set_id: &str,
    ) -> AppResult<(RuleSet, Vec<PathBuf>)> {
        let spec = builtin_remote_spec(set_id)
            .ok_or_else(|| AppError::Config("只能重置内置规则集".into()))?;
        self.reset_builtin_remote_set(app_data_dir, resource_dir, spec)
    }

    /// Restore one bundled remote rule set to factory defaults (name, url,
    /// target, interval) while keeping the user's `enabled` choice. The
    /// packaged `.srs` is re-copied to the stable cache path.
    fn reset_builtin_remote_set(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
        spec: &crate::domain::BuiltinRemoteRuleSpec,
    ) -> AppResult<(RuleSet, Vec<PathBuf>)> {
        let cache_dir = crate::builtin_remote_rules::cache_dir(app_data_dir);
        let mut stale = self.stale_cache_paths(spec.id, &cache_dir);
        let mut restored =
            crate::builtin_remote_rules::restore_set(app_data_dir, resource_dir, spec);
        if let Some(index) = self.rule_sets.iter().position(|x| x.id == spec.id) {
            restored.enabled = self.rule_sets[index].enabled;
            self.rule_sets[index] = restored.clone();
        } else {
            self.rule_sets.insert(0, restored.clone());
        }
        // The stable path is live again; only superseded caches are stale.
        let stable = crate::builtin_remote_rules::stable_cache_path(app_data_dir, spec);
        stale.retain(|path| *path != stable);
        Ok((restored, stale))
    }

    /// local_path of one set inside the cache dir, if it has one.
    fn stale_cache_paths(&self, set_id: &str, cache_dir: &Path) -> Vec<PathBuf> {
        self.rule_sets
            .iter()
            .find(|set| set.id == set_id)
            .and_then(|set| set.remote.as_ref())
            .and_then(|remote| remote.local_path.as_ref())
            .map(PathBuf::from)
            .filter(|path| path.parent() == Some(cache_dir))
            .into_iter()
            .collect()
    }

    /// Reset the three bundled remote rule sets to factory defaults. Legacy
    /// `builtin-*` list sets are intentionally untouched — recognized but no
    /// longer restored. Returns (restored sets, stale cache files, ids whose
    /// `.list` exports should be removed).
    pub fn reset_all_builtin_rule_sets(
        &mut self,
        app_data_dir: &Path,
        resource_dir: Option<&Path>,
    ) -> (Vec<RuleSet>, Vec<PathBuf>, Vec<String>) {
        let cache_dir = crate::builtin_remote_rules::cache_dir(app_data_dir);
        let mut stale = Vec::new();
        for spec in BUILTIN_REMOTE_RULE_SETS.iter() {
            stale.extend(self.stale_cache_paths(spec.id, &cache_dir));
        }
        let export_ids: Vec<String> = self
            .rule_sets
            .iter()
            .filter(|set| is_builtin_remote_id(&set.id))
            .map(|set| set.id.clone())
            .collect();
        self.rule_sets.retain(|set| !is_builtin_remote_id(&set.id));
        let mut restored = Vec::new();
        for (index, spec) in BUILTIN_REMOTE_RULE_SETS.iter().enumerate() {
            let set = crate::builtin_remote_rules::restore_set(app_data_dir, resource_dir, spec);
            self.rule_sets.insert(index, set.clone());
            restored.push(set);
        }
        let stable: std::collections::HashSet<PathBuf> = BUILTIN_REMOTE_RULE_SETS
            .iter()
            .map(|spec| crate::builtin_remote_rules::stable_cache_path(app_data_dir, spec))
            .collect();
        stale.retain(|path| !stable.contains(path));
        (restored, stale, export_ids)
    }
}

fn same_rules_ignoring_storage_fields(left: &[Rule], right: &[Rule]) -> bool {
    fn canonical(rules: &[Rule]) -> Vec<String> {
        let mut out: Vec<String> = rules
            .iter()
            .cloned()
            .map(|mut rule| {
                rule.id.clear();
                rule.ord = 0;
                serde_json::to_string(&rule).unwrap_or_default()
            })
            .collect();
        out.sort();
        out
    }
    canonical(left) == canonical(right)
}

fn parse_store(raw: &str) -> Result<AppStore, serde_json::Error> {
    let value: Value = serde_json::from_str(raw)?;
    Ok(store_from_json(value))
}

fn store_from_json(value: Value) -> AppStore {
    let mut store = AppStore::default();
    let Some(obj) = value.as_object() else {
        crate::app_log::warn(
            "storage",
            "store root is not an object; using defaults for missing fields",
        );
        return store;
    };

    if let Some(v) = obj.get("schema_version").and_then(Value::as_u64) {
        store.schema_version = v as u32;
    }

    let (subs, retained_subs) = split_known_items::<Subscription>(obj.get("subscriptions"));
    store.subscriptions = subs;
    store.retained_subscriptions = retained_subs;

    let (nodes, retained_nodes) = split_known_items::<StoredNode>(obj.get("nodes"));
    store.nodes = nodes;
    store.retained_nodes = retained_nodes;

    let (rules, retained_rules) = split_known_items::<Rule>(obj.get("rules"));
    store.rules = rules;
    store.retained_rules = retained_rules;

    let (rule_sets, retained_sets) = split_known_items::<RuleSet>(obj.get("rule_sets"));
    store.rule_sets = rule_sets;
    store.retained_rule_sets = retained_sets;

    // Pools/chains/node_aliases: MUST be read here — this hand-rolled loader
    // is the only load path (the serde derives alone don't run for it), and
    // any field it skips loads as empty and is then wiped from disk by the
    // next save.
    let (pools, retained_pools) = split_known_items::<crate::domain::NodePool>(obj.get("pools"));
    store.pools = pools;
    store.retained_pools = retained_pools;

    let (chains, retained_chains) =
        split_known_items::<crate::domain::ProxyChain>(obj.get("chains"));
    store.chains = chains;
    store.retained_chains = retained_chains;

    if let Some(aliases) = obj.get("node_aliases") {
        match serde_json::from_value::<std::collections::BTreeMap<String, String>>(aliases.clone())
        {
            Ok(parsed) => store.node_aliases = parsed,
            Err(error) => crate::app_log::warn(
                "storage",
                format!("ignored unreadable node_aliases object ({error}); keeping defaults"),
            ),
        }
    }

    if let Some(settings) = obj.get("settings") {
        match serde_json::from_value::<AppSettings>(settings.clone()) {
            Ok(parsed) => store.settings = parsed,
            Err(error) => crate::app_log::warn(
                "storage",
                format!("ignored unreadable settings object ({error}); keeping defaults"),
            ),
        }
    }
    if let Some(dns) = obj.get("dns") {
        match serde_json::from_value::<DnsSettings>(dns.clone()) {
            Ok(parsed) => store.dns = parsed,
            Err(error) => crate::app_log::warn(
                "storage",
                format!("ignored unreadable dns object ({error}); keeping defaults"),
            ),
        }
    }
    store.active_rule_set_id = obj
        .get("active_rule_set_id")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    store
}

fn split_known_items<T: DeserializeOwned>(value: Option<&Value>) -> (Vec<T>, Vec<Value>) {
    let Some(Value::Array(items)) = value else {
        return (Vec::new(), Vec::new());
    };
    let mut known = Vec::with_capacity(items.len());
    let mut retained = Vec::new();
    for item in items {
        match serde_json::from_value::<T>(item.clone()) {
            Ok(parsed) => known.push(parsed),
            Err(error) => {
                crate::app_log::warn(
                    "storage",
                    format!("ignored unrecognized store item ({error}); keeping it on disk"),
                );
                retained.push(item.clone());
            }
        }
    }
    (known, retained)
}

fn serialize_store(store: &AppStore) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(store)?;
    if let Some(obj) = value.as_object_mut() {
        merge_retained(obj, "subscriptions", &store.retained_subscriptions);
        merge_retained(obj, "nodes", &store.retained_nodes);
        merge_retained(obj, "rules", &store.retained_rules);
        merge_retained(obj, "rule_sets", &store.retained_rule_sets);
        merge_retained(obj, "pools", &store.retained_pools);
        merge_retained(obj, "chains", &store.retained_chains);
    }
    serde_json::to_string_pretty(&value)
}

fn merge_retained(obj: &mut Map<String, Value>, key: &str, extra: &[Value]) {
    if extra.is_empty() {
        return;
    }
    let slot = obj.entry(key).or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(items) = slot {
        items.extend(extra.iter().cloned());
    }
}

fn snapshot_candidates(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![backup_path(path)];
    if let Some(directory) = path.parent() {
        if let Ok(entries) = fs::read_dir(directory) {
            let mut snapshots: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|entry| {
                    entry
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("store.corrupt-"))
                })
                .collect();
            snapshots.sort_by_key(|entry| {
                std::cmp::Reverse(
                    entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .ok(),
                )
            });
            out.extend(snapshots);
        }
    }
    out
}

fn load_valid_snapshot(path: &Path) -> Option<(AppStore, String, PathBuf)> {
    load_richer_snapshot(path, 0).or_else(|| {
        for candidate in snapshot_candidates(path) {
            let Ok(raw) = fs::read_to_string(&candidate) else {
                continue;
            };
            if let Ok(store) = parse_store(&raw) {
                return Some((store, raw, candidate));
            }
        }
        None
    })
}

fn load_richer_snapshot(path: &Path, min_subs: usize) -> Option<(AppStore, String, PathBuf)> {
    for candidate in snapshot_candidates(path) {
        let Ok(raw) = fs::read_to_string(&candidate) else {
            continue;
        };
        let Ok(store) = parse_store(&raw) else {
            continue;
        };
        if store.subscriptions.len() > min_subs {
            return Some((store, raw, candidate));
        }
    }
    None
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_file_name(STORE_BACKUP_NAME)
}

fn quarantine_corrupt_store(path: &Path, raw: &str) -> AppResult<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let quarantine = path.with_file_name(format!(
        "store.corrupt-{}-{timestamp}.json",
        std::process::id()
    ));
    fs::write(&quarantine, raw)?;
    if let Some(directory) = path.parent() {
        let _ = prune_corrupt_snapshots(directory, MAX_CORRUPT_SNAPSHOTS);
    }
    Ok(quarantine)
}

fn prune_corrupt_snapshots(directory: &Path, keep: usize) -> std::io::Result<()> {
    let mut snapshots: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("store.corrupt-"))
        })
        .collect();
    snapshots.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    let remove_count = snapshots.len().saturating_sub(keep);
    for entry in snapshots.into_iter().take(remove_count) {
        fs::remove_file(entry.path())?;
    }
    Ok(())
}

fn replace_file(path: &Path, raw: &[u8]) -> AppResult<()> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("json");
    let tmp = path.with_extension(format!("{extension}.tmp"));
    fs::write(&tmp, raw)?;
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

pub fn default_store_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("data").join("store.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RuleType;

    fn test_store_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "satelite-store-{name}-{}-{nonce}",
                std::process::id()
            ))
            .join("store.json")
    }

    fn corrupt_snapshots(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|entry| {
                entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("store.corrupt-"))
            })
            .collect()
    }

    #[test]
    fn load_self_heals_duplicate_node_ids() {
        use crate::domain::{Protocol, ProtocolConfig, ProxyNode};
        let mk = |id: &str, password: &str| StoredNode {
            subscription_id: "sub-1".into(),
            node: ProxyNode {
                id: id.into(),
                name: "香港 01".into(),
                protocol: Protocol::Shadowsocks,
                server: "example.com".into(),
                port: 8388,
                tls: None,
                transport: None,
                udp: None,
                config: ProtocolConfig::Shadowsocks {
                    method: "aes-128-gcm".into(),
                    password: password.into(),
                    plugin: None,
                    plugin_opts: None,
                    shadow_tls: None,
                },
                source: None,
                latency_ms: None,
                latency_at: None,
            },
        };
        // Legacy collision: same name/server/port/protocol, different creds.
        let base = ProxyNode::compute_id("香港 01", "example.com", 8388, Protocol::Shadowsocks);
        let path = test_store_path("dup-ids");
        let mut store = AppStore::default();
        store.nodes.push(mk(&base, "pass-a"));
        store.nodes.push(mk(&base, "pass-b"));
        store.save(&path).unwrap();

        let loaded = AppStore::load(&path, None).unwrap();
        assert_eq!(loaded.nodes.len(), 2);
        assert_ne!(loaded.nodes[0].node.id, loaded.nodes[1].node.id);
        assert_ne!(loaded.nodes[0].node.id[..16], loaded.nodes[1].node.id[..16]);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn batch_set_rule_targets_node_and_smart_set_whole_set_strategies() {
        use crate::domain::{
            Protocol, ProtocolConfig, ProxyNode, Rule, RuleSet, RuleSetStrategy, RuleTarget,
            RuleType,
        };
        let mut store = AppStore::default();
        let node = ProxyNode {
            id: "node-1".into(),
            name: "东京 01".into(),
            protocol: Protocol::Shadowsocks,
            server: "example.com".into(),
            port: 8388,
            tls: None,
            transport: None,
            udp: None,
            config: ProtocolConfig::Shadowsocks {
                method: "aes-256-gcm".into(),
                password: "x".into(),
                plugin: None,
                plugin_opts: None,
                shadow_tls: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        };
        store.nodes.push(StoredNode {
            subscription_id: "sub".into(),
            node,
        });
        let set = RuleSet::new_user(
            "批量集",
            vec![
                Rule::new(RuleType::DomainSuffix, "a.com".into(), RuleTarget::Proxy, 1),
                Rule::new(
                    RuleType::DomainSuffix,
                    "b.com".into(),
                    RuleTarget::Direct,
                    2,
                ),
            ],
        );
        store.rule_sets = vec![set];
        let id = store.rule_sets[0].id.clone();

        let (updated, _) = store
            .batch_set_rule_targets(
                &id,
                RuleTarget::Node,
                Some("node-1".into()),
                vec![],
                vec![],
                None,
            )
            .unwrap();
        // Batch node → whole-set Node strategy + set-level pin; every local
        // rule carries the same pin for per-row display.
        assert_eq!(updated.strategy, RuleSetStrategy::Node);
        assert_eq!(updated.node_id.as_deref(), Some("node-1"));
        assert_eq!(updated.node_name.as_deref(), Some("东京 01"));
        assert!(updated.rules.iter().all(|r| {
            r.target == RuleTarget::Node
                && r.node_id.as_deref() == Some("node-1")
                && r.node_name.as_deref() == Some("东京 01")
        }));

        // Batch keywords → whole-set Filter strategy with set-level filters.
        let (updated, _) = store
            .batch_set_rule_targets(
                &id,
                RuleTarget::Smart,
                None,
                vec!["东京".into(), "东京 ".into()],
                vec!["香港".into()],
                None,
            )
            .unwrap();
        assert_eq!(updated.strategy, RuleSetStrategy::Filter);
        assert_eq!(updated.smart_include, vec!["东京".to_string()]);
        assert_eq!(updated.smart_exclude, vec!["香港".to_string()]);
        assert!(updated.node_id.is_none());
        assert!(updated.rules.iter().all(|r| {
            r.target == RuleTarget::Smart
                && r.smart_include == vec!["东京".to_string()]
                && r.smart_exclude == vec!["香港".to_string()]
        }));

        // Include/exclude overlap is rejected.
        assert!(store
            .batch_set_rule_targets(
                &id,
                RuleTarget::Smart,
                None,
                vec!["东京".into()],
                vec!["东京".into()],
                None,
            )
            .is_err());

        // Batch to direct collapses back to a plain uniform strategy and
        // clears the set-level pin / filters.
        let (updated, _) = store
            .batch_set_rule_targets(&id, RuleTarget::Direct, None, vec![], vec![], None)
            .unwrap();
        assert_eq!(updated.strategy, RuleSetStrategy::Direct);
        assert!(updated.node_id.is_none());
        assert!(updated.smart_include.is_empty());
        assert!(updated
            .rules
            .iter()
            .all(|r| r.target == RuleTarget::Direct && r.node_id.is_none()));
    }

    #[test]
    fn batch_set_rule_targets_routes_remote_whole_set() {
        use crate::domain::{RuleSetStrategy, RuleTarget};
        let mut store = AppStore::default();
        let set = store
            .create_remote_rule_set(
                "远程集",
                "https://example.com/rules.json",
                RuleTarget::Proxy,
                "1h",
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();

        let (updated, _) = store
            .batch_set_rule_targets(&set.id, RuleTarget::Direct, None, vec![], vec![], None)
            .unwrap();
        assert_eq!(updated.strategy, RuleSetStrategy::Direct);
        assert_eq!(
            updated.remote.expect("remote config").target,
            RuleTarget::Direct
        );

        // Remote sets support the whole-set node pin / keyword filters too.
        let node_pin = crate::domain::ProxyNode {
            id: "n1".into(),
            name: "东京 01".into(),
            protocol: crate::domain::Protocol::Shadowsocks,
            server: "example.com".into(),
            port: 8388,
            tls: None,
            transport: None,
            udp: None,
            config: crate::domain::ProtocolConfig::Shadowsocks {
                method: "aes-256-gcm".into(),
                password: "x".into(),
                plugin: None,
                plugin_opts: None,
                shadow_tls: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        };
        store.nodes.push(StoredNode {
            subscription_id: "sub".into(),
            node: node_pin,
        });
        let (updated, _) = store
            .batch_set_rule_targets(
                &set.id,
                RuleTarget::Node,
                Some("n1".into()),
                vec![],
                vec![],
                None,
            )
            .unwrap();
        assert_eq!(updated.strategy, RuleSetStrategy::Node);
        assert_eq!(updated.node_id.as_deref(), Some("n1"));
        assert_eq!(
            updated.remote.expect("remote config").target,
            RuleTarget::Node
        );

        let (updated, _) = store
            .batch_set_rule_targets(
                &set.id,
                RuleTarget::Smart,
                None,
                vec!["东京".into()],
                vec![],
                None,
            )
            .unwrap();
        assert_eq!(updated.strategy, RuleSetStrategy::Filter);
        assert_eq!(updated.smart_include, vec!["东京".to_string()]);
        assert_eq!(
            updated.remote.expect("remote config").target,
            RuleTarget::Smart
        );
    }

    #[test]
    fn create_local_rule_set_applies_initial_strategy() {
        use crate::domain::{RuleSetStrategy, RuleTarget};
        let mut store = AppStore::default();
        let set = store
            .create_local_rule_set("本地直连集", RuleTarget::Direct, None, vec![], vec![], None)
            .unwrap();
        assert_eq!(set.strategy, RuleSetStrategy::Direct);
        // DNS pairing follows the same recommendation as a strategy flip.
        assert_eq!(
            set.dns_strategy,
            RuleSetStrategy::Direct.recommended_dns_strategy().unwrap()
        );
        assert!(store.rule_sets[0].id == set.id, "new set lands on top");
    }

    #[test]
    fn create_local_rule_set_smart_pairs_remote_dns() {
        use crate::domain::{RuleSetStrategy, RuleTarget};
        let mut store = AppStore::default();
        // Keyword target creates a whole-set Filter (per-rule pools stay a
        // Mixed-only concern).
        let set = store
            .create_local_rule_set(
                "过滤集",
                RuleTarget::Smart,
                None,
                vec!["东京".into()],
                vec![],
                None,
            )
            .unwrap();
        assert_eq!(set.strategy, RuleSetStrategy::Filter);
        assert_eq!(set.smart_include, vec!["东京".to_string()]);
        assert_eq!(
            set.dns_strategy,
            RuleSetStrategy::Filter.recommended_dns_strategy().unwrap()
        );
    }

    #[test]
    fn new_sets_start_disabled_and_empty_sets_cannot_be_enabled() {
        use crate::domain::{Rule, RuleTarget, RuleType};
        let mut store = AppStore::default();
        let local = store
            .create_local_rule_set("新本地", RuleTarget::Proxy, None, vec![], vec![], None)
            .unwrap();
        assert!(!local.enabled, "new local sets start disabled");
        assert!(
            store.set_rule_set_enabled(&local.id, true).is_err(),
            "empty local set cannot be enabled"
        );

        // One effective rule unlocks enabling.
        store
            .upsert_rule_in_set(
                &local.id,
                Rule::new(RuleType::DomainSuffix, "a.com".into(), RuleTarget::Proxy, 1),
            )
            .unwrap();
        store.set_rule_set_enabled(&local.id, true).unwrap();
        assert!(store.get_rule_set(&local.id).unwrap().enabled);
        // Disabling a populated set stays allowed.
        store.set_rule_set_enabled(&local.id, false).unwrap();

        let remote = store
            .create_remote_rule_set(
                "新远程",
                "https://example.com/r.json",
                RuleTarget::Proxy,
                "1h",
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        assert!(!remote.enabled, "new remote sets start disabled");
        // No downloaded cache file yet → still empty for config purposes.
        assert!(store.set_rule_set_enabled(&remote.id, true).is_err());
    }

    #[test]
    fn v6_load_writes_pre_v6_backup_and_migrates() {
        use crate::domain::{Rule, RuleSet, RuleSetStrategy, RuleTarget, RuleType};
        let path = test_store_path("pre-v6");
        let mut store = AppStore::default();
        store.schema_version = 5;
        let mut set = RuleSet::new_user(
            "v5集",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "a.com".into(),
                RuleTarget::Direct,
                1,
            )],
        );
        set.strategy = RuleSetStrategy::Proxy;
        store.rule_sets = vec![set];
        store.save(&path).unwrap();

        let loaded = AppStore::load(&path, None).unwrap();
        assert_eq!(loaded.schema_version, 10);
        let pre_v6 = path.with_file_name("store.pre-v6.backup.json");
        assert!(
            pre_v6.exists(),
            "pre-v6 snapshot must be written on upgrade"
        );
        let snap = parse_store(&fs::read_to_string(&pre_v6).unwrap()).unwrap();
        assert_eq!(snap.schema_version, 5);
        // Reloading the migrated store must not resurrect the backup logic.
        fs::remove_file(&pre_v6).unwrap();
        let again = AppStore::load(&path, None).unwrap();
        assert_eq!(again.schema_version, 10);
        assert!(!pre_v6.exists(), "v6 store skips the backup on reload");

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_preserves_the_previous_valid_store() {
        let path = test_store_path("backup");
        let mut store = AppStore::default();
        store.settings.mixed_port = 2101;
        store.save(&path).unwrap();
        store.settings.mixed_port = 2102;
        store.save(&path).unwrap();

        let current = parse_store(&fs::read_to_string(&path).unwrap()).unwrap();
        let backup = parse_store(&fs::read_to_string(backup_path(&path)).unwrap()).unwrap();
        assert_eq!(current.settings.mixed_port, 2102);
        assert_eq!(backup.settings.mixed_port, 2101);

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn load_recovers_a_corrupt_store_from_backup() {
        let path = test_store_path("recover");
        let mut store = AppStore::default();
        store.settings.mixed_port = 2201;
        store.save(&path).unwrap();
        store.settings.mixed_port = 2202;
        store.save(&path).unwrap();
        fs::write(&path, "{broken").unwrap();

        let recovered = AppStore::load(&path, None).unwrap();
        assert_eq!(recovered.settings.mixed_port, 2201);
        assert_eq!(corrupt_snapshots(&path).len(), 1);
        assert!(parse_store(&fs::read_to_string(&path).unwrap()).is_ok());

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn pools_chains_and_aliases_survive_a_save_load_roundtrip() {
        // Regression: the hand-rolled loader in `store_from_json` used to skip
        // pools/chains/node_aliases entirely — they loaded as empty and the
        // next save wiped them from disk (chains "disappeared" every restart).
        use crate::domain::{ChainHop, PoolMode};
        let path = test_store_path("chain-roundtrip");
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "A"));
        store.nodes.push(mk_stored_node("n2", "B"));
        let pool = store
            .create_pool(
                "池",
                PoolMode::Explicit {
                    node_ids: vec!["n1".into()],
                },
            )
            .unwrap();
        let chain = store
            .create_chain(
                "链",
                vec![
                    ChainHop::Node {
                        node_id: "n1".into(),
                    },
                    ChainHop::Pool {
                        pool_id: pool.id.clone(),
                    },
                ],
            )
            .unwrap();
        store
            .node_aliases
            .insert("identity|原名".into(), "别名".into());

        store.save(&path).unwrap();
        let reloaded = AppStore::load(&path, None).unwrap();

        assert_eq!(reloaded.pools.len(), 1, "pools must survive reload");
        assert_eq!(reloaded.pools[0].id, pool.id);
        assert_eq!(reloaded.chains.len(), 1, "chains must survive reload");
        assert_eq!(reloaded.chains[0].id, chain.id);
        assert_eq!(
            reloaded
                .node_aliases
                .get("identity|原名")
                .map(String::as_str),
            Some("别名"),
            "node aliases must survive reload"
        );

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn sample_url_sub(name: &str) -> Subscription {
        Subscription {
            id: format!("id-{name}"),
            name: name.into(),
            source: crate::domain::SubscriptionSource::Url {
                url: "https://example.com/sub".into(),
            },
            last_update: 1,
            node_count: 0,
            enabled: true,
            format: None,
            skipped_count: 0,
            via_proxy: false,
            auto_update: false,
            auto_update_interval_min: 1440,
            traffic: None,
        }
    }

    fn stored_node(sub_id: &str, node_id: &str) -> StoredNode {
        StoredNode {
            subscription_id: sub_id.into(),
            node: crate::domain::ProxyNode {
                id: node_id.into(),
                name: node_id.into(),
                protocol: crate::domain::Protocol::Trojan,
                server: "example.com".into(),
                port: 443,
                tls: None,
                transport: None,
                udp: None,
                config: crate::domain::ProtocolConfig::Trojan {
                    password: "x".into(),
                },
                source: None,
                latency_ms: None,
                latency_at: None,
            },
        }
    }

    #[test]
    fn enabled_node_ids_sorted_tracks_node_and_enable_changes() {
        let mut store = AppStore::default();
        let sub = sample_url_sub("a");
        let sub_id = sub.id.clone();
        store.subscriptions.push(sub);
        store.nodes.push(stored_node(&sub_id, "node-a"));

        let before = store.enabled_node_ids_sorted();
        assert_eq!(before, vec!["node-a".to_string()]);

        // Identical refresh (same ids) keeps the fingerprint stable — no
        // rebuild would be queued.
        store.nodes.clear();
        store.nodes.push(stored_node(&sub_id, "node-a"));
        assert_eq!(store.enabled_node_ids_sorted(), before);

        // Renamed node → new content-hash id → fingerprint changes.
        store.nodes.clear();
        store.nodes.push(stored_node(&sub_id, "node-b"));
        assert_ne!(store.enabled_node_ids_sorted(), before);

        // Disabling the subscription removes its nodes from the projection.
        let with_node = store.enabled_node_ids_sorted();
        store.subscriptions[0].enabled = false;
        assert!(store.enabled_node_ids_sorted().is_empty());
        assert_ne!(store.enabled_node_ids_sorted(), with_node);

        // Singbox (custom config) subscriptions never contribute nodes.
        store.subscriptions[0].enabled = true;
        store.subscriptions[0].source = crate::domain::SubscriptionSource::Singbox {
            content: "{}".into(),
        };
        assert!(store.enabled_node_ids_sorted().is_empty());
    }

    #[test]
    fn load_quarantines_a_corrupt_store_without_backup() {
        let path = test_store_path("defaults");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not-json").unwrap();

        let recovered = AppStore::load(&path, None).unwrap();
        assert_eq!(
            recovered.settings.mixed_port,
            AppStore::default().settings.mixed_port
        );
        assert_eq!(corrupt_snapshots(&path).len(), 1);
        assert!(parse_store(&fs::read_to_string(&path).unwrap()).is_ok());

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn skips_unknown_subscription_objects() {
        let raw = r#"{
          "schema_version": 5,
          "subscriptions": [
            {
              "id": "keep",
              "name": "ok",
              "source": {"kind":"url","url":"https://example.com"},
              "last_update": 1,
              "node_count": 0,
              "enabled": true,
              "skipped_count": 0
            },
            12
          ],
          "nodes": []
        }"#;
        let store = parse_store(raw).unwrap();
        assert_eq!(store.subscriptions.len(), 1);
        assert_eq!(store.subscriptions[0].id, "keep");
        assert_eq!(store.retained_subscriptions.len(), 1);
    }

    #[test]
    fn unknown_source_kind_is_ignored_and_written_back() {
        let raw = r#"{
          "schema_version": 5,
          "subscriptions": [
            {
              "id": "keep",
              "name": "ok",
              "source": {"kind":"url","url":"https://example.com"},
              "last_update": 1,
              "node_count": 0,
              "enabled": true,
              "skipped_count": 0
            },
            {
              "id": "future",
              "name": "next-gen",
              "source": {"kind":"quantum","payload":"x"},
              "last_update": 1,
              "node_count": 0,
              "enabled": true,
              "skipped_count": 0
            }
          ],
          "nodes": []
        }"#;
        let store = parse_store(raw).unwrap();
        assert_eq!(store.subscriptions.len(), 1);
        assert_eq!(store.subscriptions[0].id, "keep");
        assert_eq!(store.retained_subscriptions.len(), 1);

        let path = test_store_path("retain-unknown");
        store.save(&path).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let kinds: Vec<&str> = written["subscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["source"]["kind"].as_str())
            .collect();
        assert!(kinds.contains(&"url"));
        assert!(kinds.contains(&"quantum"));

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn valid_json_with_unknown_kind_does_not_reset_or_quarantine() {
        let path = test_store_path("no-wipe");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
              "schema_version": 5,
              "settings": {"mixed_port": 2345, "api_port": 19090},
              "subscriptions": [
                {
                  "id": "keep",
                  "name": "ok",
                  "source": {"kind":"url","url":"https://example.com"},
                  "last_update": 1,
                  "node_count": 0,
                  "enabled": true,
                  "skipped_count": 0
                },
                {
                  "id": "future",
                  "name": "next-gen",
                  "source": {"kind":"quantum"},
                  "last_update": 1,
                  "node_count": 0,
                  "enabled": true,
                  "skipped_count": 0
                }
              ],
              "nodes": []
            }"#,
        )
        .unwrap();

        let loaded = AppStore::load(&path, None).unwrap();
        assert_eq!(loaded.subscriptions.len(), 1);
        assert_eq!(loaded.subscriptions[0].name, "ok");
        assert_eq!(loaded.settings.mixed_port, 2345);
        assert!(corrupt_snapshots(&path).is_empty());

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn empty_store_recovers_from_newer_corrupt_snapshot() {
        let path = test_store_path("empty-recover");
        AppStore::default().save(&path).unwrap();
        let mut rich = AppStore::default();
        rich.subscriptions.push(sample_url_sub("keep-me"));
        let snapshot = path.with_file_name("store.corrupt-999.json");
        fs::write(&snapshot, serde_json::to_string(&rich).unwrap()).unwrap();

        let recovered = AppStore::load(&path, None).unwrap();
        assert_eq!(recovered.subscriptions.len(), 1);
        assert_eq!(recovered.subscriptions[0].name, "keep-me");

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn corrupt_store_snapshots_are_bounded() {
        let path = test_store_path("prune");
        let directory = path.parent().unwrap();
        fs::create_dir_all(directory).unwrap();
        for index in 0..5 {
            fs::write(directory.join(format!("store.corrupt-{index}.json")), "bad").unwrap();
        }

        prune_corrupt_snapshots(directory, 3).unwrap();
        assert_eq!(corrupt_snapshots(&path).len(), 3);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unified_migration_splits_mixed_sets_once() {
        let mut store = AppStore::default();
        store.rule_sets.push(RuleSet::new_user(
            "混合",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "a.test".into(),
                    RuleTarget::Proxy,
                    10,
                ),
                Rule::new(
                    RuleType::DomainSuffix,
                    "b.test".into(),
                    RuleTarget::Direct,
                    20,
                ),
                Rule::new(RuleType::Domain, "c.test".into(), RuleTarget::Smart, 30),
            ],
        ));
        store.migrate_unified_rule_sets();

        assert_eq!(store.schema_version, 3);
        assert!(store
            .rule_sets
            .iter()
            .any(|set| set.strategy == RuleSetStrategy::Proxy));
        assert!(store
            .rule_sets
            .iter()
            .any(|set| set.strategy == RuleSetStrategy::Direct));
        assert!(store
            .rule_sets
            .iter()
            .any(|set| set.strategy == RuleSetStrategy::Smart));
        assert_eq!(
            store
                .rule_sets
                .iter()
                .map(|set| set.rules.len())
                .sum::<usize>(),
            3
        );
        let once = serde_json::to_string(&store.rule_sets).unwrap();
        store.migrate_unified_rule_sets();
        assert_eq!(once, serde_json::to_string(&store.rule_sets).unwrap());
    }

    #[test]
    fn v6_migration_normalizes_plain_set_rule_targets_once() {
        use crate::domain::{Rule, RuleSet, RuleSetStrategy, RuleTarget, RuleType};
        let mut set = RuleSet::new_user(
            "遗留混合",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "a.com".into(),
                    RuleTarget::Direct,
                    1,
                ),
                Rule::new(RuleType::DomainSuffix, "b.com".into(), RuleTarget::Block, 2),
            ],
        );
        set.strategy = RuleSetStrategy::Proxy;
        let mut smart = RuleSet::new_user(
            "智能集",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "c.com".into(),
                RuleTarget::Node,
                1,
            )],
        );
        smart.strategy = RuleSetStrategy::Smart;
        let mut store = AppStore {
            schema_version: 5,
            rule_sets: vec![set, smart],
            ..AppStore::with_builtin_sets(None)
        };
        store.migrate_plain_set_rule_targets();
        // Plain set normalized to its strategy; smart set untouched.
        assert!(store.rule_sets[0]
            .rules
            .iter()
            .all(|r| r.target == RuleTarget::Proxy));
        assert!(store.rule_sets[1].rules[0].target == RuleTarget::Node);
        assert_eq!(store.schema_version, 6);
        // Idempotent: a post-migration per-rule choice survives reload.
        store.rule_sets[0].rules[0].target = RuleTarget::Direct;
        store.migrate_plain_set_rule_targets();
        assert!(store.rule_sets[0].rules[0].target == RuleTarget::Direct);
    }

    #[test]
    fn v3_migration_folds_dns_matchers_into_shared_rules() {
        let mut store = AppStore {
            schema_version: 2,
            ..AppStore::default()
        };
        let mut set = RuleSet::new_user("国内解析", Vec::new());
        set.dns_rules.push(crate::domain::DnsRule {
            id: "dns-cn".into(),
            enabled: true,
            matcher: DomainMatcher::DomainSuffix,
            payload: "example.cn".into(),
            action: DnsAction::Domestic,
        });
        store.rule_sets.push(set);

        store.migrate_unified_rule_sets();

        assert_eq!(store.schema_version, 3);
        assert_eq!(
            store.rule_sets[0].dns_strategy,
            RuleSetDnsStrategy::Domestic
        );
        assert!(store.rule_sets[0].dns_rules.is_empty());
        assert_eq!(store.rule_sets[0].rules.len(), 1);
        assert_eq!(store.rule_sets[0].rules[0].payload, "example.cn");
    }

    #[test]
    fn v4_removes_untouched_redundant_general_set() {
        let mut store = AppStore {
            schema_version: 3,
            ..AppStore::default()
        };
        let mut general = RuleSet::new_user(GENERAL_SET_NAME, default_rules());
        general.id = GENERAL_SET_ID.into();
        general.ownership = RuleSetOwnership::Builtin;
        general.strategy = RuleSetStrategy::Direct;
        store.rule_sets.push(general);

        store.migrate_redundant_general_rule_set();

        assert_eq!(store.schema_version, 4);
        assert!(!store.rule_sets.iter().any(|set| set.id == GENERAL_SET_ID));
    }

    #[test]
    fn v4_preserves_edited_general_set_as_user_owned() {
        let mut store = AppStore {
            schema_version: 3,
            ..AppStore::default()
        };
        let mut rules = default_rules();
        rules.push(Rule::new(
            RuleType::DomainSuffix,
            "user.example".into(),
            RuleTarget::Direct,
            100,
        ));
        let mut general = RuleSet::new_user(GENERAL_SET_NAME, rules);
        general.id = GENERAL_SET_ID.into();
        general.ownership = RuleSetOwnership::Builtin;
        general.strategy = RuleSetStrategy::Direct;
        store.rule_sets.push(general);

        store.migrate_redundant_general_rule_set();

        let preserved = store
            .get_rule_set(GENERAL_SET_ID)
            .expect("preserved general");
        assert_eq!(preserved.ownership, RuleSetOwnership::User);
        assert!(!preserved.builtin);
        store.delete_rule_set(GENERAL_SET_ID).unwrap();
        assert!(store.get_rule_set(GENERAL_SET_ID).is_none());
    }

    #[test]
    fn v5_disables_implicit_remote_auto_updates_once() {
        let mut store = AppStore {
            schema_version: 4,
            ..AppStore::default()
        };
        let mut remote = RuleSet::new_remote(
            "旧远程规则",
            "https://example.com/rules.json",
            RuleTarget::Proxy,
        );
        remote.remote.as_mut().unwrap().update_interval = "1h".into();
        store.rule_sets.push(remote);

        store.migrate_remote_update_policy();
        assert_eq!(store.schema_version, 5);
        assert_eq!(
            store.rule_sets[0].remote.as_ref().unwrap().update_interval,
            "disabled"
        );

        store.rule_sets[0].remote.as_mut().unwrap().update_interval = "12h".into();
        store.migrate_remote_update_policy();
        assert_eq!(
            store.rule_sets[0].remote.as_ref().unwrap().update_interval,
            "12h"
        );
    }

    #[test]
    fn deleted_builtin_sets_are_not_resurrected() {
        let mut store = AppStore::default();
        let mut legacy = RuleSet::new_user("旧内置", Vec::new());
        legacy.id = BUILTIN_SET_ID.into();
        legacy.builtin = true;
        legacy.ownership = RuleSetOwnership::Builtin;
        store.rule_sets.push(legacy);
        store.migrate_builtin_remote_rule_sets();
        let builtin_remote_id = BUILTIN_REMOTE_RULE_SETS[0].id.to_string();
        assert!(store.get_rule_set(&builtin_remote_id).is_some());

        // Deleting either kind sticks: factory content is never loaded from
        // disk (stale `resources/rules/*.list` copies cannot resurrect it)
        // and the v7 migration never runs again (schema guard).
        store.delete_rule_set(&builtin_remote_id).unwrap();
        store.delete_rule_set(BUILTIN_SET_ID).unwrap();
        store.ensure_rule_sets();
        store.migrate_builtin_remote_rule_sets();
        assert!(store.get_rule_set(&builtin_remote_id).is_none());
        assert!(store.get_rule_set(BUILTIN_SET_ID).is_none());
    }

    #[test]
    fn stale_rule_list_resources_never_resurrect_legacy_builtins() {
        let dir = std::env::temp_dir().join(format!(
            "satelite-stale-res-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Simulate the stale copy shipped in older dev/packaged builds:
        // `target/{debug,release}/resources/rules/builtin-ruleset.list`.
        let stale_rules_dir = dir.join("res/resources/rules");
        fs::create_dir_all(&stale_rules_dir).unwrap();
        fs::write(
            stale_rules_dir.join("builtin-ruleset.list"),
            "# name: 内置规则集\nDOMAIN-SUFFIX,example.com,PROXY\n",
        )
        .unwrap();

        let path = dir.join("data/store.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut store = AppStore {
            schema_version: 8,
            ..AppStore::default()
        };
        store
            .rule_sets
            .push(build_builtin_remote_set(&BUILTIN_REMOTE_RULE_SETS[0]));
        let mut legacy = RuleSet::new_user("旧内置", Vec::new());
        legacy.id = BUILTIN_SET_ID.into();
        legacy.builtin = true;
        legacy.ownership = RuleSetOwnership::Builtin;
        store.rule_sets.push(legacy.clone());
        store.save(&path).unwrap();
        // The user deleted the legacy set before this launch.
        store.delete_rule_set(BUILTIN_SET_ID).unwrap();
        store.save(&path).unwrap();

        let loaded = AppStore::load(&path, Some(&dir.join("res"))).unwrap();
        assert!(
            !loaded.rule_sets.iter().any(|set| set.id == BUILTIN_SET_ID),
            "stale .list resources must not resurrect a deleted legacy set"
        );
        assert!(
            loaded
                .rule_sets
                .iter()
                .any(|set| set.id == BUILTIN_REMOTE_RULE_SETS[0].id),
            "system set intact"
        );
        assert_eq!(loaded.schema_version, 10);

        // Even a legacy set that still exists loses its 内置 badge.
        let mut store = AppStore {
            schema_version: 8,
            ..AppStore::default()
        };
        store.rule_sets.push(legacy.clone());
        store.save(&path).unwrap();
        let loaded = AppStore::load(&path, Some(&dir.join("res"))).unwrap();
        let set = loaded.get_rule_set(BUILTIN_SET_ID).unwrap();
        assert!(!set.builtin);
        assert_eq!(set.ownership, RuleSetOwnership::User);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v9_renames_first_iteration_ids_and_downgrades_legacy_builtins() {
        let mut store = AppStore {
            schema_version: 8,
            ..AppStore::default()
        };
        // Entry from the first (unreleased) iteration of the system sets.
        let (old, new) = LEGACY_BUILTIN_REMOTE_IDS[0];
        let mut first_gen = build_builtin_remote_set(&BUILTIN_REMOTE_RULE_SETS[0]);
        first_gen.id = old.into();
        first_gen.remote.as_mut().unwrap().local_path = Some("/tmp/old.srs".into());
        store.rule_sets.push(first_gen);
        // Legacy list sets keep existing but stop being 内置.
        let mut legacy = RuleSet::new_user("旧内置", Vec::new());
        legacy.id = BUILTIN_SET_ID.into();
        legacy.builtin = true;
        legacy.ownership = RuleSetOwnership::Builtin;
        store.rule_sets.push(legacy);

        store.migrate_system_rule_set_ids();

        assert_eq!(store.schema_version, 9);
        assert!(
            store.rule_sets.iter().any(|set| set.id == new),
            "renamed to the system id"
        );
        assert!(
            !store.rule_sets.iter().any(|set| set.id == old),
            "old id gone"
        );
        let renamed = store.rule_sets.iter().find(|set| set.id == new).unwrap();
        assert_eq!(renamed.remote.as_ref().unwrap().local_path, None);
        let legacy = store.get_rule_set(BUILTIN_SET_ID).unwrap();
        assert!(!legacy.builtin);
        assert_eq!(legacy.ownership, RuleSetOwnership::User);

        // A pre-existing system entry wins over a stale first-gen duplicate.
        let mut dup = build_builtin_remote_set(&BUILTIN_REMOTE_RULE_SETS[1]);
        let (dup_old, dup_new) = LEGACY_BUILTIN_REMOTE_IDS[1];
        dup.id = dup_old.into();
        let mut store = AppStore {
            schema_version: 8,
            ..AppStore::default()
        };
        store
            .rule_sets
            .push(build_builtin_remote_set(&BUILTIN_REMOTE_RULE_SETS[1]));
        store.rule_sets.push(dup);
        store.migrate_system_rule_set_ids();
        let count = store
            .rule_sets
            .iter()
            .filter(|set| set.id == dup_new || set.id == dup_old)
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn v7_migration_runs_once_and_orders_specs_first() {
        let mut store = AppStore {
            schema_version: 6,
            ..AppStore::default()
        };
        store
            .rule_sets
            .push(RuleSet::new_user("已有规则", Vec::new()));

        store.migrate_builtin_remote_rule_sets();
        assert_eq!(store.schema_version, 7);
        let ids: Vec<&str> = store.rule_sets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids[..3],
            BUILTIN_REMOTE_RULE_SETS
                .iter()
                .map(|spec| spec.id)
                .collect::<Vec<_>>()[..]
        );
        assert_eq!(store.rule_sets.len(), 4);

        // Idempotent: a user deletion after v7 is never undone by reload.
        store
            .delete_rule_set(BUILTIN_REMOTE_RULE_SETS[1].id)
            .unwrap();
        store.migrate_builtin_remote_rule_sets();
        assert_eq!(store.rule_sets.len(), 3);
    }

    #[test]
    fn v8_removes_general_rule_set_and_legacy_flat_rules() {
        let mut store = AppStore {
            schema_version: 7,
            ..AppStore::default()
        };
        let mut general = RuleSet::new_user(GENERAL_SET_NAME, default_rules());
        general.id = GENERAL_SET_ID.into();
        store.rule_sets.push(general);
        let user = RuleSet::new_user("用户集", Vec::new());
        store.rule_sets.push(user);
        // Legacy flat leftovers that ensure_rule_sets would fold into the
        // general set before v8 runs — both must disappear together.
        store.rules.push(Rule::new(
            RuleType::DomainSuffix,
            "old.com".into(),
            RuleTarget::Direct,
            10,
        ));

        store.migrate_remove_general_rule_set();

        assert_eq!(store.schema_version, 8);
        assert!(
            !store.rule_sets.iter().any(|set| set.id == GENERAL_SET_ID),
            "general set removed"
        );
        assert!(
            store.rule_sets.iter().any(|set| set.name == "用户集"),
            "user sets survive"
        );
        assert!(store.rules.is_empty(), "legacy flat rules cleared");
        // Idempotent: schema guard keeps later user choices intact.
        store.migrate_remove_general_rule_set();
        assert_eq!(store.rule_sets.len(), 1);
    }

    #[test]
    fn v7_load_writes_pre_v7_backup() {
        let path = test_store_path("pre-v7");
        let mut store = AppStore {
            schema_version: 6,
            ..AppStore::default()
        };
        store.rule_sets.push(RuleSet::new_user("v6集", Vec::new()));
        store.save(&path).unwrap();

        let loaded = AppStore::load(&path, None).unwrap();
        assert_eq!(loaded.schema_version, 10);
        let pre_v7 = path.with_file_name("store.pre-v7.backup.json");
        assert!(
            pre_v7.exists(),
            "pre-v7 snapshot must be written on upgrade"
        );
        let snap = parse_store(&fs::read_to_string(&pre_v7).unwrap()).unwrap();
        assert_eq!(snap.schema_version, 6);

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn reset_all_builtin_restores_only_the_three_remote_sets() {
        let dir = std::env::temp_dir().join(format!(
            "satelite-reset-all-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = AppStore::default();
        let user = RuleSet::new_remote(
            "用户远程",
            "https://example.com/rules.json",
            RuleTarget::Proxy,
        );
        let user_id = user.id.clone();
        store.rule_sets.push(user);
        let mut legacy = RuleSet::new_user("旧内置", Vec::new());
        legacy.id = "builtin-ruleset".into();
        legacy.builtin = true;
        legacy.ownership = RuleSetOwnership::Builtin;
        store.rule_sets.push(legacy);
        store.migrate_builtin_remote_rule_sets();

        let (restored, _stale, export_ids) = store.reset_all_builtin_rule_sets(&dir, None);

        assert_eq!(restored.len(), 3);
        let ids: Vec<&str> = store.rule_sets.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids.len(), 5);
        assert!(ids[..3].iter().all(|id| is_builtin_remote_id(id)));
        // User set and the legacy builtin list set both survive the reset.
        assert!(store.get_rule_set(&user_id).is_some());
        assert!(store.get_rule_set("builtin-ruleset").is_some());
        assert!(!export_ids.iter().any(|id| id == "builtin-ruleset"));
        // With bundled copies available the sets reference their stable
        // cache file; without them they stay on the URL fallback.
        for set in &restored {
            let spec = crate::domain::builtin_remote_spec(&set.id).unwrap();
            match set.remote.as_ref().unwrap().local_path.as_deref() {
                Some(path) => {
                    let stable = crate::builtin_remote_rules::stable_cache_path(&dir, spec);
                    assert_eq!(path, stable.to_str().unwrap());
                    assert!(stable.is_file());
                }
                None => {}
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_single_builtin_remote_restores_defaults_and_keeps_enabled() {
        let dir = std::env::temp_dir().join(format!(
            "satelite-reset-one-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut store = AppStore::default();
        store.migrate_builtin_remote_rule_sets();
        let spec = &BUILTIN_REMOTE_RULE_SETS[2];
        {
            let set = store
                .rule_sets
                .iter_mut()
                .find(|s| s.id == spec.id)
                .unwrap();
            set.name = "改名".into();
            set.enabled = false;
            set.remote.as_mut().unwrap().update_interval = "disabled".into();
        }

        let (restored, _stale) = store
            .reset_rule_set(&dir, None, spec.id)
            .expect("reset builtin remote set");

        assert_eq!(restored.name, spec.name);
        assert!(!restored.enabled, "user's disabled state survives reset");
        assert_eq!(restored.remote.as_ref().unwrap().update_interval, "24h");
        assert_eq!(
            store
                .rule_sets
                .iter()
                .find(|s| s.id == spec.id)
                .unwrap()
                .name,
            spec.name
        );

        // Legacy builtin list ids are no longer resettable at all.
        assert!(store.reset_rule_set(&dir, None, BUILTIN_SET_ID).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_local_and_remote_sets_are_inserted_at_highest_priority() {
        let mut store = AppStore::default();
        store
            .rule_sets
            .push(RuleSet::new_user("已有规则", Vec::new()));

        let local = store
            .create_local_rule_set("新本地", RuleTarget::Proxy, None, vec![], vec![], None)
            .unwrap();
        assert_eq!(store.rule_sets[0].id, local.id);

        let remote = store
            .create_remote_rule_set(
                "新远程",
                "https://example.com/rules.json",
                RuleTarget::Proxy,
                "1h",
                None,
                vec![],
                vec![],
                None,
            )
            .unwrap();
        assert_eq!(store.rule_sets[0].id, remote.id);
        assert_eq!(store.rule_sets[1].id, local.id);
    }

    fn sample_hy2(id: &str, name: &str) -> ProxyNode {
        use crate::domain::{Protocol, ProtocolConfig};
        ProxyNode {
            id: id.into(),
            name: name.into(),
            protocol: Protocol::Hysteria2,
            server: "203.10.98.188".into(),
            port: 443,
            tls: None,
            transport: None,
            udp: None,
            config: ProtocolConfig::Hysteria2 {
                password: "same".into(),
                up_mbps: None,
                down_mbps: None,
                obfs: None,
                obfs_password: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        }
    }

    #[test]
    fn rename_does_not_alias_sibling_nodes_on_same_backend() {
        let mut store = AppStore::default();
        store
            .upsert_subscription(
                sample_url_sub("s"),
                vec![sample_hy2("a", "HK-01"), sample_hy2("b", "HK-02")],
            )
            .unwrap();
        store.rename_node("a", "Hong Kong".into()).unwrap();
        assert_eq!(store.find_node("a").unwrap().name, "Hong Kong");
        assert_eq!(store.find_node("b").unwrap().name, "HK-02");

        store
            .upsert_subscription(
                sample_url_sub("s"),
                vec![sample_hy2("a", "HK-01"), sample_hy2("b", "HK-02")],
            )
            .unwrap();
        assert_eq!(store.find_node("a").unwrap().name, "Hong Kong");
        assert_eq!(store.find_node("b").unwrap().name, "HK-02");
    }

    #[test]
    fn set_runtime_source_selects_custom_and_falls_back_on_delete() {
        let mut store = AppStore::default();
        let mut sub = sample_url_sub("s");
        sub.id = "sb1".into();
        sub.source = crate::domain::SubscriptionSource::Singbox {
            content:
                r#"{"inbounds":[{"type":"mixed","listen_port":1}],"outbounds":[{"type":"direct"}]}"#
                    .into(),
        };
        store.upsert_subscription(sub, Vec::new()).unwrap();
        store
            .set_runtime_source(crate::domain::RuntimeSource::Singbox { id: "sb1".into() })
            .unwrap();
        assert_eq!(store.settings.runtime_source, "singbox:sb1");
        store.remove_subscription("sb1").unwrap();
        assert_eq!(store.settings.runtime_source, "generated");
    }

    // ---- Proxy Chain: pool/chain CRUD -------------------------------------

    fn mk_stored_node(id: &str, name: &str) -> StoredNode {
        use crate::domain::{Protocol, ProtocolConfig, ProxyNode};
        StoredNode {
            subscription_id: "sub-1".into(),
            node: ProxyNode {
                id: id.into(),
                name: name.into(),
                protocol: Protocol::Shadowsocks,
                server: "example.com".into(),
                port: 8388,
                tls: None,
                transport: None,
                udp: None,
                config: ProtocolConfig::Shadowsocks {
                    method: "aes-128-gcm".into(),
                    password: "secret".into(),
                    plugin: None,
                    plugin_opts: None,
                    shadow_tls: None,
                },
                source: None,
                latency_ms: None,
                latency_at: None,
            },
        }
    }

    #[test]
    fn create_pool_rejects_duplicate_name_case_insensitively() {
        use crate::domain::PoolMode;
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "HK-1"));
        store
            .create_pool(
                "香港",
                PoolMode::Explicit {
                    node_ids: vec!["n1".into()],
                },
            )
            .unwrap();
        let err = store
            .create_pool(
                "香港",
                PoolMode::Explicit {
                    node_ids: vec!["n1".into()],
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("已存在同名"));
    }

    #[test]
    fn create_pool_explicit_mode_rejects_unknown_node_id() {
        // A dangling node id in an Explicit pool would silently drop that
        // member at build time (filter_pool_tags-style code only emits tags
        // present in `nodes`) — better to reject it up front than let the
        // user believe a node is in the pool when it never resolves.
        use crate::domain::PoolMode;
        let mut store = AppStore::default();
        let err = store
            .create_pool(
                "坏池",
                PoolMode::Explicit {
                    node_ids: vec!["does-not-exist".into()],
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("不存在的节点"));
    }

    #[test]
    fn create_pool_explicit_mode_requires_at_least_one_node() {
        use crate::domain::PoolMode;
        let mut store = AppStore::default();
        let err = store
            .create_pool("空池", PoolMode::Explicit { node_ids: vec![] })
            .unwrap_err();
        assert!(err.to_string().contains("至少需要选择一个节点"));
    }

    #[test]
    fn create_pool_keyword_mode_rejects_include_exclude_overlap() {
        use crate::domain::PoolMode;
        let mut store = AppStore::default();
        let err = store
            .create_pool(
                "冲突池",
                PoolMode::Keyword {
                    include: vec!["香港".into()],
                    exclude: vec!["香港".into()],
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("不能同时出现在白名单和黑名单"));
    }

    #[test]
    fn delete_pool_blocked_while_a_chain_references_it() {
        // Deleting a pool a chain depends on would leave that chain with a
        // dangling hop — the config builder degrades that to "chain absent"
        // (see build_chain_outbounds_for), silently breaking a route the
        // user thinks is still configured. Block the delete instead.
        use crate::domain::{ChainHop, PoolMode};
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "HK-1"));
        store.nodes.push(mk_stored_node("n2", "Exit"));
        let pool = store
            .create_pool(
                "落地池",
                PoolMode::Explicit {
                    node_ids: vec!["n1".into()],
                },
            )
            .unwrap();
        store
            .create_chain(
                "链A",
                vec![
                    ChainHop::Pool {
                        pool_id: pool.id.clone(),
                    },
                    ChainHop::Node {
                        node_id: "n2".into(),
                    },
                ],
            )
            .unwrap();
        let err = store.delete_pool(&pool.id).unwrap_err();
        assert!(err.to_string().contains("链路引用"));
        assert_eq!(
            store.pools.len(),
            1,
            "blocked delete must not remove the pool"
        );
    }

    #[test]
    fn create_chain_requires_at_least_two_hops() {
        // A single-hop "chain" is just a node/pool pin — RuleTarget::Node or
        // a keyword Filter pool already cover that; allowing a 1-hop chain
        // would be a redundant, confusing second way to say the same thing.
        use crate::domain::ChainHop;
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "Only"));
        let err = store
            .create_chain(
                "单跳",
                vec![ChainHop::Node {
                    node_id: "n1".into(),
                }],
            )
            .unwrap_err();
        assert!(err.to_string().contains("至少需要 2 跳"));
    }

    #[test]
    fn create_chain_rejects_unknown_node_and_pool_ids() {
        use crate::domain::ChainHop;
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "Entry"));
        let err = store
            .create_chain(
                "坏链",
                vec![
                    ChainHop::Node {
                        node_id: "n1".into(),
                    },
                    ChainHop::Node {
                        node_id: "does-not-exist".into(),
                    },
                ],
            )
            .unwrap_err();
        assert!(err.to_string().contains("不存在的节点"));

        let err = store
            .create_chain(
                "坏链2",
                vec![
                    ChainHop::Node {
                        node_id: "n1".into(),
                    },
                    ChainHop::Pool {
                        pool_id: "pool-nope".into(),
                    },
                ],
            )
            .unwrap_err();
        assert!(err.to_string().contains("不存在的节点池"));
    }

    #[test]
    fn delete_chain_blocked_while_a_rule_set_references_it() {
        use crate::domain::{ChainHop, RuleSet, RuleSetStrategy};
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "A"));
        store.nodes.push(mk_stored_node("n2", "B"));
        let chain = store
            .create_chain(
                "被引用链",
                vec![
                    ChainHop::Node {
                        node_id: "n1".into(),
                    },
                    ChainHop::Node {
                        node_id: "n2".into(),
                    },
                ],
            )
            .unwrap();
        let mut set = RuleSet::new_user("规则集", vec![]);
        set.strategy = RuleSetStrategy::Chain;
        set.chain_id = Some(chain.id.clone());
        store.rule_sets.push(set);

        let err = store.delete_chain(&chain.id).unwrap_err();
        assert!(err.to_string().contains("规则集引用"));
        assert_eq!(
            store.chains.len(),
            1,
            "blocked delete must not remove the chain"
        );
    }

    #[test]
    fn chain_rule_usage_dedups_set_level_and_per_rule_references() {
        use crate::domain::{ChainHop, Rule, RuleSet, RuleSetStrategy, RuleTarget, RuleType};
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "A"));
        store.nodes.push(mk_stored_node("n2", "B"));
        let chain = store
            .create_chain(
                "被引用链",
                vec![
                    ChainHop::Node {
                        node_id: "n1".into(),
                    },
                    ChainHop::Node {
                        node_id: "n2".into(),
                    },
                ],
            )
            .unwrap();

        // Set A: whole-set chain pin AND a per-rule chain target — must count
        // this set exactly once.
        let mut set_a = RuleSet::new_user("集合A", vec![]);
        set_a.strategy = RuleSetStrategy::Chain;
        set_a.chain_id = Some(chain.id.clone());
        let mut rule_a = Rule::new(
            RuleType::DomainSuffix,
            "example.com".into(),
            RuleTarget::Chain,
            0,
        );
        rule_a.chain_id = Some(chain.id.clone());
        set_a.rules.push(rule_a);
        // Set B: only a per-rule reference.
        let mut set_b = RuleSet::new_user("集合B", vec![]);
        let mut rule_b = Rule::new(
            RuleType::DomainSuffix,
            "other.com".into(),
            RuleTarget::Chain,
            0,
        );
        rule_b.chain_id = Some(chain.id.clone());
        set_b.rules.push(rule_b);
        // Set C: unrelated set, must not appear.
        let set_c = RuleSet::new_user("集合C", vec![]);
        store.rule_sets.push(set_a);
        store.rule_sets.push(set_b);
        store.rule_sets.push(set_c);

        let usage = store.chain_rule_usage();
        let names = &usage[&chain.id];
        assert_eq!(
            names.len(),
            2,
            "set A counted once despite two reference levels"
        );
        assert!(names.contains(&"集合A".to_string()));
        assert!(names.contains(&"集合B".to_string()));
        assert!(!names.contains(&"集合C".to_string()));
    }

    #[test]
    fn update_chain_revalidates_hops_against_current_nodes() {
        // A chain edited to reference a node that no longer exists must be
        // rejected the same way create_chain would reject it — update isn't
        // a looser code path than create.
        use crate::domain::ChainHop;
        let mut store = AppStore::default();
        store.nodes.push(mk_stored_node("n1", "A"));
        store.nodes.push(mk_stored_node("n2", "B"));
        let chain = store
            .create_chain(
                "链",
                vec![
                    ChainHop::Node {
                        node_id: "n1".into(),
                    },
                    ChainHop::Node {
                        node_id: "n2".into(),
                    },
                ],
            )
            .unwrap();
        let err = store
            .update_chain(
                &chain.id,
                "链",
                vec![
                    ChainHop::Node {
                        node_id: "n1".into(),
                    },
                    ChainHop::Node {
                        node_id: "gone".into(),
                    },
                ],
            )
            .unwrap_err();
        assert!(err.to_string().contains("不存在的节点"));
        // Original hops must survive a rejected update.
        assert_eq!(store.chains[0].hops.len(), 2);
    }
}
