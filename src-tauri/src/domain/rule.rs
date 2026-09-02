use super::dns::DnsRule;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleType {
    Domain,
    DomainSuffix,
    DomainKeyword,
    IpCidr,
    Process,
    /// Deprecated in sing-box 1.12+; kept for deserialize only.
    Geoip,
}

impl RuleType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::DomainSuffix => "domain_suffix",
            Self::DomainKeyword => "domain_keyword",
            Self::IpCidr => "ip_cidr",
            Self::Process => "process",
            Self::Geoip => "geoip",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTarget {
    Direct,
    Proxy,
    Block,
    /// Pin to a specific subscription node (`node_id` on [`Rule`]).
    Node,
    /// Smart pool: filter nodes by name keywords, then pick best via smart-switch probe.
    Smart,
    /// Route through a named multi-hop chain (`chain_id` on [`Rule`]).
    Chain,
}

impl RuleTarget {
    pub fn outbound_tag(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            // Node/Smart/Chain resolve to a dynamic tag via
            // `resolve_rule_outbound` in the config builder; "proxy" here is
            // only the static fallback used before that resolution runs.
            Self::Proxy | Self::Node | Self::Smart | Self::Chain => "proxy",
            Self::Block => "block",
        }
    }

    /// Clash-compatible third column (NODE/SMART/CHAIN export as PROXY).
    pub fn clash_token(self) -> &'static str {
        match self {
            Self::Direct => "DIRECT",
            Self::Proxy | Self::Node | Self::Smart | Self::Chain => "PROXY",
            Self::Block => "REJECT",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    /// Lower = higher priority (applied first).
    pub ord: i32,
    #[serde(rename = "type")]
    pub rule_type: RuleType,
    pub payload: String,
    pub target: RuleTarget,
    pub enabled: bool,
    /// When `target == Node`: stable node id to pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Snapshot of node display name at save time (for stale-node UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    /// When `target == Smart`: whitelist — name must contain any keyword (OR). Empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_include: Vec<String>,
    /// When `target == Smart`: blacklist — name containing any keyword is skipped (OR).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_exclude: Vec<String>,
    /// When `target == Chain`: stable id of the [`crate::domain::ProxyChain`] to route through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    /// Snapshot of chain display name at save time (for stale-chain UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_name: Option<String>,
}

impl Rule {
    pub fn new(rule_type: RuleType, payload: String, target: RuleTarget, ord: i32) -> Self {
        let payload = payload.trim().to_string();
        let id = Self::compute_id(rule_type, &payload, target, None, &[], &[], None);
        Self {
            id,
            ord,
            rule_type,
            payload,
            target,
            enabled: true,
            node_id: None,
            node_name: None,
            smart_include: Vec::new(),
            smart_exclude: Vec::new(),
            chain_id: None,
            chain_name: None,
        }
    }

    pub fn clash_type_token(&self) -> &'static str {
        match self.rule_type {
            RuleType::Domain => "DOMAIN",
            RuleType::DomainSuffix => "DOMAIN-SUFFIX",
            RuleType::DomainKeyword => "DOMAIN-KEYWORD",
            RuleType::IpCidr => "IP-CIDR",
            RuleType::Process => "PROCESS-NAME",
            RuleType::Geoip => "GEOIP",
        }
    }

    pub fn compute_id(
        rule_type: RuleType,
        payload: &str,
        target: RuleTarget,
        node_id: Option<&str>,
        smart_include: &[String],
        smart_exclude: &[String],
        chain_id: Option<&str>,
    ) -> String {
        let mut h = Sha256::new();
        h.update(rule_type.as_str().as_bytes());
        h.update(b"|");
        h.update(payload.trim().as_bytes());
        h.update(b"|");
        h.update(format!("{target:?}").as_bytes());
        if let Some(nid) = node_id.filter(|s| !s.is_empty()) {
            h.update(b"|");
            h.update(nid.as_bytes());
        }
        if matches!(target, RuleTarget::Smart) {
            for k in smart_include {
                h.update(b"|+");
                h.update(k.as_bytes());
            }
            for k in smart_exclude {
                h.update(b"|-");
                h.update(k.as_bytes());
            }
        }
        if matches!(target, RuleTarget::Chain) {
            if let Some(cid) = chain_id.filter(|s| !s.is_empty()) {
                h.update(b"|c");
                h.update(cid.as_bytes());
            }
        }
        hex::encode(&h.finalize()[..12])
    }

    /// Normalize keyword lists (trim, drop empty, de-dup case-insensitively, preserve order).
    pub fn normalize_keywords(raw: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for s in raw {
            let t = s.trim();
            if t.is_empty() {
                continue;
            }
            let lower = t.to_lowercase();
            if out.iter().any(|x: &String| x.to_lowercase() == lower) {
                continue;
            }
            out.push(t.to_string());
        }
        out
    }

    /// Whether a node display name matches this rule's smart include/exclude filters.
    pub fn smart_name_matches(&self, node_name: &str) -> bool {
        name_matches_keywords(node_name, &self.smart_include, &self.smart_exclude)
    }

    /// Selector outbound tag for a smart rule (stable, short).
    pub fn smart_outbound_tag(&self) -> String {
        format!("smart-{}", &self.id[..self.id.len().min(16)])
    }
}

/// Whitelist (`include`): empty = allow all; otherwise name must contain **any** keyword (OR).
/// Blacklist (`exclude`): name must contain **none** of the keywords (any hit skips).
/// Matching is case-insensitive substring on the display name.
pub fn name_matches_keywords(node_name: &str, include: &[String], exclude: &[String]) -> bool {
    let name = node_name.to_lowercase();

    // Blacklist first: any hit → skip
    for k in exclude {
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        if name.contains(&k.to_lowercase()) {
            return false;
        }
    }

    let include_keys: Vec<&str> = include
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if include_keys.is_empty() {
        return true;
    }
    // Whitelist: any keyword match → allow
    include_keys
        .into_iter()
        .any(|k| name.contains(&k.to_lowercase()))
}

/// Keywords that appear in both include and exclude (case-insensitive). Empty if no conflict.
pub fn keyword_list_overlap(include: &[String], exclude: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for a in include {
        let al = a.trim().to_lowercase();
        if al.is_empty() {
            continue;
        }
        if exclude.iter().any(|b| b.trim().to_lowercase() == al)
            && !out.iter().any(|x: &String| x.to_lowercase() == al)
        {
            out.push(a.trim().to_string());
        }
    }
    out
}

/// Named rule set (built-in or user). Multiple sets can be enabled at once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSet {
    pub id: String,
    pub name: String,
    /// Marks sets bundled with the application.
    pub builtin: bool,
    /// When true, rules in this set are merged into the active routing config.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub ownership: RuleSetOwnership,
    /// Ordinary groups apply one strategy to every item. `Node`/`Filter` are
    /// whole-set pins (parameters below); `Smart` (Mixed) preserves the
    /// legacy per-item target / node-pool settings.
    #[serde(default)]
    pub strategy: RuleSetStrategy,
    /// When `strategy == Node`: stable node id the whole set is pinned to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Snapshot of node display name at pin time (for stale-node UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    /// When `strategy == Filter`: whitelist — name must contain any keyword (OR). Empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_include: Vec<String>,
    /// When `strategy == Filter`: blacklist — name containing any keyword is skipped (OR).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_exclude: Vec<String>,
    /// When `strategy == Chain`: stable id of the [`crate::domain::ProxyChain`] the whole set routes through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    /// Snapshot of chain display name at pin time (for stale-chain UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_name: Option<String>,
    /// Whole-set DNS resolver policy, independent from the route strategy.
    #[serde(default)]
    pub dns_strategy: RuleSetDnsStrategy,
    /// Remote sing-box rule-set source. `None` means an editable local set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteRuleSetConfig>,
    /// Transitional v2 field. v3 folds these matchers into `rules` and no
    /// longer exposes a second per-set DNS rule list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns_rules: Vec<DnsRule>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetOwnership {
    Builtin,
    #[default]
    User,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetStrategy {
    #[default]
    Proxy,
    Direct,
    Block,
    /// Whole set pinned to one node (`node_id` on [`RuleSet`]).
    Node,
    /// Whole set routed to a keyword-filtered node pool
    /// (`smart_include`/`smart_exclude` on [`RuleSet`]).
    Filter,
    /// Whole set routed through a named multi-hop chain (`chain_id` on [`RuleSet`]).
    Chain,
    /// Per-item route/DNS decisions (emergent "Mixed" tag).
    Smart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetDnsStrategy {
    Local,
    Domestic,
    #[default]
    Remote,
}

impl RuleSetDnsStrategy {
    pub fn server_tag(self) -> &'static str {
        match self {
            Self::Local => "dns-local",
            Self::Domestic => "dns-cn",
            Self::Remote => "dns-remote",
        }
    }
}

impl RuleSetStrategy {
    pub fn from_target(target: RuleTarget) -> Self {
        match target {
            RuleTarget::Proxy => Self::Proxy,
            RuleTarget::Direct => Self::Direct,
            RuleTarget::Block => Self::Block,
            RuleTarget::Node => Self::Node,
            RuleTarget::Smart => Self::Filter,
            RuleTarget::Chain => Self::Chain,
        }
    }

    pub fn route_target(self) -> Option<RuleTarget> {
        match self {
            Self::Proxy => Some(RuleTarget::Proxy),
            Self::Direct => Some(RuleTarget::Direct),
            Self::Block => Some(RuleTarget::Block),
            Self::Node | Self::Filter | Self::Smart | Self::Chain => None,
        }
    }

    /// Recommended whole-set DNS policy when the route strategy changes.
    /// Block has no editable DNS policy because it always emits DNS reject.
    pub fn recommended_dns_strategy(self) -> Option<RuleSetDnsStrategy> {
        match self {
            Self::Proxy | Self::Node | Self::Filter | Self::Smart | Self::Chain => {
                Some(RuleSetDnsStrategy::Remote)
            }
            Self::Direct => Some(RuleSetDnsStrategy::Local),
            Self::Block => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRuleSetConfig {
    pub url: String,
    #[serde(default = "default_remote_format")]
    pub format: String,
    #[serde(default = "default_update_interval")]
    pub update_interval: String,
    /// Whole-set route strategy. `node`/`smart` pin the set via the set-level
    /// `node_id` / `smart_include`/`smart_exclude` fields on [`RuleSet`].
    pub target: RuleTarget,
    /// Rust-managed downloaded source JSON or binary SRS file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// idle | downloading | ready | error
    #[serde(default = "default_download_status")]
    pub download_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt: Option<i64>,
    /// Number of expanded display entries in the latest validated cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_count: Option<u32>,
    /// Whether the latest validated cache carries any `ip_cidr` condition.
    /// `None` until a download completes (never fetched yet). sing-box
    /// 1.14+ rejects a DNS rule that references a rule-set containing
    /// `ip_cidr` (Legacy Address Filter Fields) — the config builder uses
    /// `Some(true)` to skip the DNS-side reference for such sets instead of
    /// letting `sing-box check` fail. `None`/`Some(false)` both keep the
    /// DNS-side reference: unknown content is assumed domain-only (the
    /// common case, and the one the user actually wants remote-DNS
    /// resolution for) rather than pessimistically dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains_ip: Option<bool>,
}

fn default_remote_format() -> String {
    "source".into()
}
fn default_update_interval() -> String {
    "disabled".into()
}

pub fn normalize_remote_update_interval(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "disabled" => Some("disabled"),
        "1h" => Some("1h"),
        "12h" => Some("12h"),
        "24h" => Some("24h"),
        _ => None,
    }
}

pub fn remote_update_interval_secs(value: &str) -> Option<i64> {
    match normalize_remote_update_interval(value)? {
        "1h" => Some(60 * 60),
        "12h" => Some(12 * 60 * 60),
        "24h" => Some(24 * 60 * 60),
        _ => None,
    }
}
fn default_download_status() -> String {
    "idle".into()
}

fn default_true() -> bool {
    true
}

impl RuleSet {
    pub fn new_user(name: &str, rules: Vec<Rule>) -> Self {
        let id = {
            let mut h = Sha256::new();
            h.update(name.as_bytes());
            h.update(b"|");
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            h.update(nanos.to_le_bytes());
            // Extra entropy so rapid creates don't collide
            h.update(std::process::id().to_le_bytes());
            format!("rs-{}", hex::encode(&h.finalize()[..10]))
        };
        Self {
            id,
            name: name.trim().to_string(),
            builtin: false,
            enabled: true,
            ownership: RuleSetOwnership::User,
            strategy: RuleSetStrategy::Proxy,
            node_id: None,
            node_name: None,
            smart_include: Vec::new(),
            smart_exclude: Vec::new(),
            chain_id: None,
            chain_name: None,
            dns_strategy: RuleSetDnsStrategy::Remote,
            remote: None,
            dns_rules: Vec::new(),
            rules,
        }
    }

    pub fn new_remote(name: &str, url: &str, target: RuleTarget) -> Self {
        let mut set = Self::new_user(name, vec![]);
        set.remote = Some(RemoteRuleSetConfig {
            url: url.trim().to_string(),
            format: default_remote_format(),
            update_interval: default_update_interval(),
            target,
            local_path: None,
            download_status: default_download_status(),
            download_error: None,
            last_update: None,
            last_attempt: None,
            rule_count: None,
            contains_ip: None,
        });
        set.strategy = RuleSetStrategy::from_target(target);
        if let Some(dns_strategy) = set.strategy.recommended_dns_strategy() {
            set.dns_strategy = dns_strategy;
        }
        set
    }

    /// Selector outbound tag for a whole-set (Filter) keyword pool. Same shape
    /// as `Rule::smart_outbound_tag` — set ids (`rs-*` / `system-*`) never
    /// collide with rule hash ids, and smart_switch probes sets through a
    /// stand-in rule so both sides must agree on this tag.
    pub fn smart_set_outbound_tag(&self) -> String {
        format!("smart-{}", &self.id[..self.id.len().min(16)])
    }
}

#[cfg(test)]
mod remote_update_interval_tests {
    use super::*;

    #[test]
    fn accepts_only_supported_remote_update_intervals() {
        assert_eq!(
            normalize_remote_update_interval("disabled"),
            Some("disabled")
        );
        assert_eq!(normalize_remote_update_interval("1H"), Some("1h"));
        assert_eq!(normalize_remote_update_interval("12h"), Some("12h"));
        assert_eq!(normalize_remote_update_interval("24h"), Some("24h"));
        assert_eq!(normalize_remote_update_interval("6h"), None);
    }

    #[test]
    fn disabled_has_no_schedule_and_legacy_default_is_disabled() {
        assert_eq!(remote_update_interval_secs("disabled"), None);
        assert_eq!(remote_update_interval_secs("1h"), Some(3_600));
        assert_eq!(remote_update_interval_secs("12h"), Some(43_200));
        assert_eq!(remote_update_interval_secs("24h"), Some(86_400));

        let value = serde_json::json!({
            "url": "https://example.com/rules.json",
            "target": "proxy"
        });
        let remote: RemoteRuleSetConfig = serde_json::from_value(value).unwrap();
        assert_eq!(remote.update_interval, "disabled");
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSetSummary {
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub rule_count: u32,
    /// Enabled for routing (multiple sets can be true).
    pub enabled: bool,
    pub ownership: RuleSetOwnership,
    pub strategy: RuleSetStrategy,
    /// Set-level route parameters (strategy == Node / Filter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub smart_exclude: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_name: Option<String>,
    pub dns_strategy: RuleSetDnsStrategy,
    /// Restorable by Reset: only the bundled remote rule sets.
    #[serde(default)]
    pub resettable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteRuleSetConfig>,
}

pub const GENERAL_SET_ID: &str = "general-rules";
pub const GENERAL_SET_NAME: &str = "通用规则";

/// Legacy / known id for the large default list file `builtin-ruleset.list`.
pub const BUILTIN_SET_ID: &str = "builtin-ruleset";
pub const BUILTIN_SET_NAME: &str = "内置规则集";

/// One bundled remote rule set shipped in `resources/rule-sets/`.
#[derive(Debug)]
pub struct BuiltinRemoteRuleSpec {
    /// Stable store id (also the sing-box rule-set tag).
    pub id: &'static str,
    pub name: &'static str,
    /// Stable cache file name under `remote-rule-sets/`.
    pub file: &'static str,
    /// Remote source the set refreshes from; the bundled copy only seeds it.
    pub url: &'static str,
    /// Whole-set route strategy.
    pub target: RuleTarget,
    /// Content is known IP-only at compile time (geoip). Overrides whatever
    /// `remote.contains_ip` metadata says — sing-box 1.14 FATALs on a DNS rule
    /// referencing an `ip_cidr` rule-set once a fakeip rule exists (Legacy
    /// Address Filter Fields), so the builder must be able to skip the DNS
    /// side of the geoip set even on stores downloaded before that field
    /// existed. See `builtin_remote_ip_only`.
    pub ip_only: bool,
}

/// Factory remote rule sets, in match-priority order (store order wins).
/// Seeded from bundled `.srs` copies at startup; Reset restores these three.
/// Ids deliberately use the `system-` prefix: legacy `builtin-*` list sets
/// must never be conflated with them.
pub const BUILTIN_REMOTE_RULE_SETS: [BuiltinRemoteRuleSpec; 3] = [
    BuiltinRemoteRuleSpec {
        id: "system-geolocation-not-cn",
        name: "海外网站",
        file: "system-geolocation-not-cn.srs",
        url:
            "https://cdn.jsdelivr.net/gh/SagerNet/sing-geosite@rule-set/geosite-geolocation-!cn.srs",
        target: RuleTarget::Proxy,
        ip_only: false,
    },
    BuiltinRemoteRuleSpec {
        id: "system-geoip-cn",
        name: "国内ip",
        file: "system-geoip-cn.srs",
        url: "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@sing/geo/geoip/cn.srs",
        target: RuleTarget::Direct,
        ip_only: true,
    },
    BuiltinRemoteRuleSpec {
        id: "system-geosite-cn",
        name: "国内站点",
        file: "system-geosite-cn.srs",
        url: "https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@sing/geo/geosite/cn.srs",
        target: RuleTarget::Direct,
        ip_only: false,
    },
];

pub fn is_builtin_remote_id(id: &str) -> bool {
    BUILTIN_REMOTE_RULE_SETS.iter().any(|spec| spec.id == id)
}

/// Statically-known "IP-only content" verdict for a bundled set (`None` for
/// non-builtin ids). Takes precedence over the store's `remote.contains_ip`
/// metadata, which can be missing (sets downloaded before the field existed)
/// or mislabeled by older builds.
pub fn builtin_remote_ip_only(id: &str) -> Option<bool> {
    builtin_remote_spec(id).map(|spec| spec.ip_only)
}

pub fn builtin_remote_spec(id: &str) -> Option<&'static BuiltinRemoteRuleSpec> {
    BUILTIN_REMOTE_RULE_SETS.iter().find(|spec| spec.id == id)
}

/// Ids used by the first (unreleased) iteration of the system sets; the v9
/// migration renames lingering entries to the `system-` ids above.
pub const LEGACY_BUILTIN_REMOTE_IDS: [(&str, &str); 3] = [
    (
        "builtin-remote-geolocation-not-cn",
        "system-geolocation-not-cn",
    ),
    ("builtin-remote-geoip-cn", "system-geoip-cn"),
    ("builtin-remote-geosite-cn", "system-geosite-cn"),
];

/// Factory entry for one bundled remote rule set. `local_path` is filled in
/// by the startup seeding step once the bundled copy lands in the cache dir.
pub fn build_builtin_remote_set(spec: &BuiltinRemoteRuleSpec) -> RuleSet {
    let mut set = RuleSet::new_remote(spec.name, spec.url, spec.target);
    set.id = spec.id.into();
    set.builtin = true;
    set.ownership = RuleSetOwnership::Builtin;
    set.enabled = true;
    if let Some(remote) = set.remote.as_mut() {
        remote.format = "binary".into();
        remote.update_interval = "24h".into();
    }
    set
}

/// Whether a source rule must remain one logical row in the remote rule viewer.
pub fn remote_rule_is_complex(rule: &serde_json::Value) -> bool {
    let Some(object) = rule.as_object() else {
        return true;
    };
    object.contains_key("type")
        || object.iter().any(|(field, value)| {
            field != "invert"
                && (value.is_object()
                    || value.as_array().is_some_and(|values| {
                        values
                            .iter()
                            .any(|item| item.is_object() || item.is_array())
                    }))
        })
}

/// Whether any rule (at any nesting depth, e.g. inside a `logical` group)
/// carries an `ip_cidr` condition. sing-box 1.14+ rejects a DNS rule that
/// references a rule-set containing this field (Legacy Address Filter
/// Fields) — the config builder uses this to decide whether a remote
/// rule-set's DNS-side reference is safe to keep. Recurses structurally
/// instead of parsing headless-rule semantics, so nested `logical`/`rules`
/// groups are covered without modeling their shape.
pub fn rules_contain_ip_cidr(rules: &[serde_json::Value]) -> bool {
    fn value_contains_ip_cidr(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(object) => object
                .iter()
                .any(|(field, nested)| field == "ip_cidr" || value_contains_ip_cidr(nested)),
            serde_json::Value::Array(items) => items.iter().any(value_contains_ip_cidr),
            _ => false,
        }
    }
    rules.iter().any(value_contains_ip_cidr)
}

/// Count the rows produced when a source rule is expanded for display.
pub fn remote_rule_display_count(rule: &serde_json::Value) -> usize {
    if remote_rule_is_complex(rule) {
        return 1;
    }
    let count: usize = rule
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(field, _)| field.as_str() != "invert")
        .map(|(_, value)| value.as_array().map_or(1, Vec::len))
        .sum();
    count.max(1)
}

/// Factory sets currently shipped with the app: the bundled `system-*`
/// remote rule sets only. Nothing is ever loaded from disk anymore, so
/// legacy `builtin-*` list sets (or stale resource files) can never be
/// re-inserted as factory content.
pub fn is_factory_set_id(id: &str) -> bool {
    is_builtin_remote_id(id)
}

/// Historical general-set payload (used by the v4 migration to detect an
/// untouched 通用规则 set). No longer a routing fallback — LAN/localhost
/// bypass comes from `AppSettings::bypass_lan` in the config builder.
pub fn default_rules() -> Vec<Rule> {
    vec![
        Rule::new(
            RuleType::DomainSuffix,
            "local".into(),
            RuleTarget::Direct,
            10,
        ),
        Rule::new(
            RuleType::DomainSuffix,
            "localhost".into(),
            RuleTarget::Direct,
            20,
        ),
        Rule::new(
            RuleType::IpCidr,
            "10.0.0.0/8".into(),
            RuleTarget::Direct,
            30,
        ),
        Rule::new(
            RuleType::IpCidr,
            "172.16.0.0/12".into(),
            RuleTarget::Direct,
            31,
        ),
        Rule::new(
            RuleType::IpCidr,
            "192.168.0.0/16".into(),
            RuleTarget::Direct,
            32,
        ),
        Rule::new(
            RuleType::IpCidr,
            "127.0.0.0/8".into(),
            RuleTarget::Direct,
            33,
        ),
        Rule::new(RuleType::DomainSuffix, "cn".into(), RuleTarget::Direct, 50),
    ]
}

pub fn sanitize_rules(rules: &[Rule]) -> Vec<Rule> {
    rules
        .iter()
        .filter(|r| !matches!(r.rule_type, RuleType::Geoip))
        .cloned()
        .collect()
}

/// Serialize a rule set to Clash-style `.list` (routing only; no DNS columns).
pub fn format_clash_rules_list(set_name: &str, rules: &[Rule]) -> String {
    let mut lines = Vec::new();
    lines.push(format!("# name: {set_name}"));
    lines.push("# format: clash/shadowrocket DOMAIN-SUFFIX,host,DIRECT".into());
    lines.push(String::new());
    let mut sorted: Vec<&Rule> = rules
        .iter()
        .filter(|r| !matches!(r.rule_type, RuleType::Geoip))
        .collect();
    sorted.sort_by_key(|r| r.ord);
    for r in sorted {
        if !r.enabled {
            lines.push(format!(
                "# disabled: {},{},{}",
                r.clash_type_token(),
                r.payload.trim(),
                r.target.clash_token()
            ));
            continue;
        }
        let mut line = format!(
            "{},{},{}",
            r.clash_type_token(),
            r.payload.trim(),
            r.target.clash_token()
        );
        if matches!(r.rule_type, RuleType::IpCidr) {
            line.push_str(",no-resolve");
        }
        lines.push(line);
    }
    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_contain_ip_cidr_detects_top_level_field() {
        let rules =
            vec![serde_json::json!({"domain_suffix": ["a.com"], "ip_cidr": ["10.0.0.0/8"]})];
        assert!(rules_contain_ip_cidr(&rules));
    }

    #[test]
    fn rules_contain_ip_cidr_detects_nested_logical_rule() {
        let rules = vec![serde_json::json!({
            "type": "logical",
            "mode": "and",
            "rules": [
                {"domain_suffix": ["a.com"]},
                {"ip_cidr": ["10.0.0.0/8"]}
            ]
        })];
        assert!(rules_contain_ip_cidr(&rules));
    }

    #[test]
    fn rules_contain_ip_cidr_false_for_domain_only_rules() {
        let rules = vec![
            serde_json::json!({"domain_suffix": ["a.com"]}),
            serde_json::json!({
                "type": "logical",
                "mode": "or",
                "rules": [
                    {"domain": ["b.com"]},
                    {"domain_keyword": ["c"]}
                ]
            }),
        ];
        assert!(!rules_contain_ip_cidr(&rules));
    }

    #[test]
    fn smart_keywords_whitelist_or_blacklist_or() {
        let inc = vec!["新加坡".into(), "日本".into()];
        let exc = vec!["香港".into(), "台湾".into()];
        // Whitelist OR: either keyword ok
        assert!(name_matches_keywords("新加坡 01", &inc, &exc));
        assert!(name_matches_keywords("日本 东京", &inc, &exc));
        assert!(!name_matches_keywords("美国 01", &inc, &exc));
        // Blacklist OR: any hit skips (even if whitelist would pass)
        assert!(!name_matches_keywords("新加坡香港", &inc, &exc));
        assert!(!name_matches_keywords("香港 01", &inc, &exc));
        // Empty whitelist = all except blacklist
        assert!(name_matches_keywords("任意节点", &[], &exc));
        assert!(!name_matches_keywords("HK 香港专线", &[], &exc));
        assert!(!name_matches_keywords("台湾专线", &[], &exc));
    }

    #[test]
    fn smart_keywords_list_overlap() {
        let a = vec!["新加坡".into(), "香港".into()];
        let b = vec!["香港".into(), "日本".into()];
        let o = keyword_list_overlap(&a, &b);
        assert_eq!(o, vec!["香港".to_string()]);
        assert!(keyword_list_overlap(&a, &[]).is_empty());
    }

    #[test]
    fn format_clash_rules_list_basic() {
        let direct = Rule::new(
            RuleType::DomainSuffix,
            "corp.internal".into(),
            RuleTarget::Direct,
            10,
        );
        let proxy = Rule::new(
            RuleType::DomainSuffix,
            "openai.com".into(),
            RuleTarget::Proxy,
            20,
        );
        let clash = format_clash_rules_list("通用", &[direct, proxy]);
        assert!(clash.contains("DOMAIN-SUFFIX,corp.internal,DIRECT"));
        assert!(clash.contains("DOMAIN-SUFFIX,openai.com,PROXY"));
    }

    #[test]
    fn route_strategy_recommends_dns_policy() {
        assert_eq!(
            RuleSetStrategy::Proxy.recommended_dns_strategy(),
            Some(RuleSetDnsStrategy::Remote)
        );
        assert_eq!(
            RuleSetStrategy::Direct.recommended_dns_strategy(),
            Some(RuleSetDnsStrategy::Local)
        );
        assert_eq!(
            RuleSetStrategy::Smart.recommended_dns_strategy(),
            Some(RuleSetDnsStrategy::Remote)
        );
        assert_eq!(
            RuleSetStrategy::Node.recommended_dns_strategy(),
            Some(RuleSetDnsStrategy::Remote)
        );
        assert_eq!(
            RuleSetStrategy::Filter.recommended_dns_strategy(),
            Some(RuleSetDnsStrategy::Remote)
        );
        assert_eq!(RuleSetStrategy::Block.recommended_dns_strategy(), None);
    }

    #[test]
    fn whole_set_strategies_map_from_targets() {
        assert_eq!(
            RuleSetStrategy::from_target(RuleTarget::Node),
            RuleSetStrategy::Node
        );
        // RuleTarget::Smart is the keyword pool → whole-set Filter strategy.
        assert_eq!(
            RuleSetStrategy::from_target(RuleTarget::Smart),
            RuleSetStrategy::Filter
        );
        assert_eq!(RuleSetStrategy::Node.route_target(), None);
        assert_eq!(RuleSetStrategy::Filter.route_target(), None);
        assert_eq!(
            RuleSetStrategy::Proxy.route_target(),
            Some(RuleTarget::Proxy)
        );
    }

    #[test]
    fn smart_set_tag_is_stable_and_never_collides_with_rule_tags() {
        let mut set = RuleSet::new_user("过滤池", vec![]);
        set.id = "system-geolocation-not-cn".into();
        let tag = set.smart_set_outbound_tag();
        assert_eq!(tag, "smart-system-geolocati");
        // Rule ids are 24-char hex — the "rs-"/"system-" prefixes keep the
        // tag spaces disjoint.
        let rule = Rule::new(RuleType::DomainSuffix, "x.com".into(), RuleTarget::Smart, 1);
        assert_ne!(tag, rule.smart_outbound_tag());
    }

    #[test]
    fn new_remote_derives_dns_strategy_from_route_target() {
        let proxy =
            RuleSet::new_remote("Proxy", "https://example.com/proxy.srs", RuleTarget::Proxy);
        assert_eq!(proxy.strategy, RuleSetStrategy::Proxy);
        assert_eq!(proxy.dns_strategy, RuleSetDnsStrategy::Remote);

        let direct = RuleSet::new_remote(
            "Direct",
            "https://example.com/direct.srs",
            RuleTarget::Direct,
        );
        assert_eq!(direct.strategy, RuleSetStrategy::Direct);
        assert_eq!(direct.dns_strategy, RuleSetDnsStrategy::Local);
    }

    #[test]
    fn builtin_remote_specs_are_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for spec in BUILTIN_REMOTE_RULE_SETS.iter() {
            assert!(
                ids.insert(spec.id),
                "duplicate builtin remote id {}",
                spec.id
            );
            assert!(spec.id.starts_with("system-"));
            assert_eq!(spec.file, format!("{}.srs", spec.id));
            assert!(spec.url.starts_with("https://"));
            assert!(matches!(
                spec.target,
                RuleTarget::Proxy | RuleTarget::Direct
            ));
            assert!(is_builtin_remote_id(spec.id));
            assert!(is_factory_set_id(spec.id));
        }
        // System ids map 1:1 from their first-iteration ids.
        assert_eq!(
            LEGACY_BUILTIN_REMOTE_IDS.len(),
            BUILTIN_REMOTE_RULE_SETS.len()
        );
        for (old, new) in LEGACY_BUILTIN_REMOTE_IDS {
            assert_eq!(builtin_remote_spec(new).expect("mapped id exists").id, new);
            assert_ne!(old, new);
        }
        // Legacy list ids are recognizable but never factory or system.
        assert!(!is_factory_set_id(BUILTIN_SET_ID));
        assert!(!is_factory_set_id("builtin-ruleset-proxy"));
        assert!(!is_builtin_remote_id(BUILTIN_SET_ID));
        assert!(!is_builtin_remote_id(LEGACY_BUILTIN_REMOTE_IDS[0].0));
    }

    #[test]
    fn build_builtin_remote_set_shape() {
        let spec = &BUILTIN_REMOTE_RULE_SETS[0];
        let set = build_builtin_remote_set(spec);
        assert_eq!(set.id, spec.id);
        assert!(set.builtin);
        assert_eq!(set.ownership, RuleSetOwnership::Builtin);
        assert!(set.enabled);
        let remote = set.remote.expect("remote config");
        assert_eq!(remote.url, spec.url);
        assert_eq!(remote.format, "binary");
        assert_eq!(remote.update_interval, "24h");
        assert_eq!(remote.target, spec.target);
        assert_eq!(remote.local_path, None);
        assert_eq!(set.strategy, RuleSetStrategy::from_target(spec.target));
        // DNS pairing follows the route target.
        assert_eq!(
            set.dns_strategy,
            RuleSetStrategy::from_target(spec.target)
                .recommended_dns_strategy()
                .unwrap()
        );
    }
}
