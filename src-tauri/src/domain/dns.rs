//! DNS settings (PRD: docs/dns.md) — stored in AppStore, emitted into sing-box `dns`.

use serde::{Deserialize, Serialize};

/// Unified resolver pool shared by all three core generators (sing-box /
/// Xray / mihomo). One source of truth so switching cores never changes
/// which servers answer queries.
///
/// Remote pool: DoH over TCP, egressed through the proxy group in every
/// core (sing-box `detour:"proxy"`, Xray dns-module routing, mihomo
/// `#proxy`). TCP keeps it working through UDP-less nodes (e.g. socks5
/// without UDP ASSOCIATE) and encrypts the queries end to end.
///
/// Emission is per-core native: mihomo races the whole pool concurrently,
/// Xray uses ordered fallback within the pool, sing-box rules address one
/// server tag per rule (no racing) so only the first entry is emitted.
pub const REMOTE_DNS_POOL: [&str; 2] = ["https://1.1.1.1/dns-query", "https://8.8.8.8/dns-query"];

/// Domestic pool: plaintext UDP, always direct (plus the bootstrap role in
/// mihomo's `default-nameserver` / `proxy-server-nameserver`).
pub const DOMESTIC_DNS_POOL: [&str; 2] = ["223.5.5.5", "119.29.29.29"];

/// Legacy stored value. Resolution modes were removed in schema v3; this is
/// deserialized only so older stores can still be opened safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DnsMode {
    #[default]
    #[serde(alias = "system")]
    Local,
    #[serde(alias = "smart")]
    SmartLocal,
    SmartCn,
    #[serde(alias = "custom")]
    Rules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DomainMatcher {
    Domain,
    #[default]
    DomainSuffix,
    DomainKeyword,
}

/// Where a DNS rule sends the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DnsAction {
    /// Use local / system DNS.
    Local,
    /// Prefer domestic DNS.
    Domestic,
    /// Prefer remote DNS.
    Remote,
    /// Reject the DNS query.
    Block,
}

impl Default for DnsAction {
    fn default() -> Self {
        Self::Local
    }
}

impl<'de> Deserialize<'de> for DnsAction {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire {
            Local,
            Domestic,
            Remote,
            // Legacy variants — mapped onto the surviving set below.
            System,
            Block,
            FakeIp,
            #[allow(dead_code)]
            Server {
                server_id: String,
            },
        }

        match Wire::deserialize(deserializer)? {
            Wire::Local | Wire::System => Ok(Self::Local),
            Wire::Domestic => Ok(Self::Domestic),
            Wire::Remote => Ok(Self::Remote),
            Wire::Block => Ok(Self::Block),
            // Legacy fake_ip/server → safest non-blocking fallback is remote.
            Wire::FakeIp | Wire::Server { .. } => Ok(Self::Remote),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsRule {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub matcher: DomainMatcher,
    pub payload: String,
    #[serde(default)]
    pub action: DnsAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakeIpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_fakeip_v4")]
    pub inet4_range: String,
    #[serde(default)]
    pub inet6_enabled: bool,
    #[serde(default = "default_fakeip_v6")]
    pub inet6_range: String,
    /// Domain suffixes that must not use FakeIP (go system / real DNS).
    #[serde(default)]
    pub bypass: Vec<String>,
}

impl Default for FakeIpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            inet4_range: default_fakeip_v4(),
            inet6_enabled: false,
            inet6_range: default_fakeip_v6(),
            bypass: default_fakeip_bypass(),
        }
    }
}

/// One user-defined static domain→IP mapping (hosts entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostsEntry {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub domain: String,
    /// IPv4 or IPv6 literal (e.g. "10.0.0.1", "::1").
    pub addr: String,
}

/// Static hosts configuration. When enabled, entries become the highest-priority
/// DNS answers (a sing-box `predefined` server + a domain rule prepended to all
/// other DNS rules).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostsConfig {
    /// Master switch for the whole hosts feature.
    #[serde(default)]
    pub enabled: bool,
    /// When true, the OS hosts file is read at config-build time and merged in.
    #[serde(default)]
    pub include_system: bool,
    #[serde(default)]
    pub entries: Vec<HostsEntry>,
}

impl Default for HostsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            include_system: false,
            entries: Vec::new(),
        }
    }
}

pub const BUILTIN_DNS_SET_ID: &str = "builtin-dns";
pub const SYSTEM_HOSTS_SET_ID: &str = "system-hosts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsRuleSetKind {
    Dns,
    Hosts,
}

/// One independently enabled DNS-page rule set. Sets are evaluated in stored
/// order. The system-hosts set is built in and read-only; its entries are read
/// from the OS at config-build time rather than persisted here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsRuleSet {
    pub id: String,
    pub name: String,
    pub kind: DnsRuleSetKind,
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub dns_rules: Vec<DnsRule>,
    #[serde(default)]
    pub hosts: Vec<HostsEntry>,
}

impl DnsRuleSet {
    fn builtin_dns(rules: Vec<DnsRule>, enabled: bool) -> Self {
        Self {
            id: BUILTIN_DNS_SET_ID.into(),
            name: "内置 DNS 规则".into(),
            kind: DnsRuleSetKind::Dns,
            builtin: true,
            read_only: false,
            enabled,
            dns_rules: rules,
            hosts: Vec::new(),
        }
    }

    fn system_hosts(enabled: bool) -> Self {
        Self {
            id: SYSTEM_HOSTS_SET_ID.into(),
            name: "系统 Hosts".into(),
            kind: DnsRuleSetKind::Hosts,
            builtin: true,
            read_only: true,
            enabled,
            dns_rules: Vec::new(),
            hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsSettings {
    /// Master switch kept for stored-config compatibility.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing)]
    pub mode: DnsMode,
    /// Compatibility switch for legacy DNS-page rules.
    #[serde(default)]
    pub rules_enabled: bool,
    /// Legacy DNS rules retained for migration and backward compatibility.
    #[serde(default)]
    pub rules: Vec<DnsRule>,
    #[serde(default)]
    pub fake_ip: FakeIpConfig,
    /// Static hosts mapping (highest DNS priority when enabled).
    #[serde(default)]
    pub hosts: HostsConfig,
    /// Ordered DNS/Hosts rule sets. Empty means a pre-rule-set store that must
    /// be migrated from `rules` / `hosts` above.
    #[serde(default)]
    pub rule_sets: Vec<DnsRuleSet>,
    /// DNS matcher sets were moved into the unified routing rule-set model.
    #[serde(default)]
    pub unified_rules: bool,
    /// Inject route `hijack-dns` (always on with TUN; optional otherwise).
    #[serde(default = "default_true")]
    pub hijack: bool,
    /// independent_cache in sing-box DNS.
    #[serde(default = "default_true")]
    pub cache: bool,
    /// Default resolver for domains that match no rule set — the sole
    /// fallback in every core (the former `leak_protect` switch was removed:
    /// no core ever silently falls back past `dns_final` anymore).
    #[serde(default = "default_dns_final")]
    pub dns_final: String,
}

impl Default for DnsSettings {
    fn default() -> Self {
        let rules = default_rules();
        Self {
            enabled: true,
            mode: DnsMode::Local,
            rules_enabled: false,
            rules: rules.clone(),
            fake_ip: FakeIpConfig::default(),
            hosts: HostsConfig::default(),
            rule_sets: Vec::new(),
            unified_rules: false,
            hijack: true,
            cache: true,
            dns_final: default_dns_final(),
        }
    }
}

impl DnsSettings {
    fn migrate_legacy_rules_mode(&mut self) {
        if self.mode == DnsMode::Rules {
            self.mode = DnsMode::Local;
            self.rules_enabled = true;
        }
    }

    /// Populate the ordered rule-set model from legacy flat DNS/Hosts fields,
    /// then ensure the two factory sets always exist.
    pub fn ensure_rule_sets(&mut self) {
        self.migrate_legacy_rules_mode();
        if self.unified_rules {
            self.rule_sets
                .retain(|set| set.kind == DnsRuleSetKind::Hosts);
            if !self
                .rule_sets
                .iter()
                .any(|set| set.id == SYSTEM_HOSTS_SET_ID)
            {
                self.rule_sets.insert(0, DnsRuleSet::system_hosts(false));
            }
            return;
        }
        if self.rule_sets.is_empty() {
            self.rule_sets.push(DnsRuleSet::builtin_dns(
                self.rules.clone(),
                self.rules_enabled,
            ));
            self.rule_sets.push(DnsRuleSet::system_hosts(
                self.hosts.enabled && self.hosts.include_system,
            ));
            if !self.hosts.entries.is_empty() {
                self.rule_sets.push(DnsRuleSet {
                    id: "migrated-hosts".into(),
                    name: "自定义 Hosts".into(),
                    kind: DnsRuleSetKind::Hosts,
                    builtin: false,
                    read_only: false,
                    enabled: self.hosts.enabled,
                    dns_rules: Vec::new(),
                    hosts: self.hosts.entries.clone(),
                });
            }
        }
        if !self
            .rule_sets
            .iter()
            .any(|set| set.id == BUILTIN_DNS_SET_ID)
        {
            self.rule_sets.insert(
                0,
                DnsRuleSet::builtin_dns(Self::factory_rules_base(), false),
            );
        }
        if !self
            .rule_sets
            .iter()
            .any(|set| set.id == SYSTEM_HOSTS_SET_ID)
        {
            let insert_at = usize::from(!self.rule_sets.is_empty());
            self.rule_sets
                .insert(insert_at, DnsRuleSet::system_hosts(false));
        }
        if let Some(set) = self
            .rule_sets
            .iter_mut()
            .find(|set| set.id == BUILTIN_DNS_SET_ID)
        {
            set.name = "内置 DNS 规则".into();
            set.kind = DnsRuleSetKind::Dns;
            set.builtin = true;
            set.read_only = false;
            set.hosts.clear();
        }
        if let Some(set) = self
            .rule_sets
            .iter_mut()
            .find(|set| set.id == SYSTEM_HOSTS_SET_ID)
        {
            let enabled = set.enabled;
            *set = DnsRuleSet::system_hosts(enabled);
        }
    }

    pub fn enabled_dns_rules(&self) -> Vec<DnsRule> {
        if self.rule_sets.is_empty() {
            return self
                .effective_rules_enabled()
                .then(|| self.rules.clone())
                .unwrap_or_default();
        }
        self.rule_sets
            .iter()
            .filter(|set| set.enabled && set.kind == DnsRuleSetKind::Dns)
            .flat_map(|set| set.dns_rules.iter().cloned())
            .collect()
    }

    pub fn has_enabled_dns_sets(&self) -> bool {
        if self.rule_sets.is_empty() {
            return self.effective_rules_enabled();
        }
        self.rule_sets.iter().any(|set| {
            set.enabled
                && set.kind == DnsRuleSetKind::Dns
                && set.dns_rules.iter().any(|rule| rule.enabled)
        })
    }

    pub fn effective_hosts(&self) -> HostsConfig {
        if self.rule_sets.is_empty() {
            return self.hosts.clone();
        }
        let mut entries = Vec::new();
        for set in self
            .rule_sets
            .iter()
            .filter(|set| set.enabled && set.kind == DnsRuleSetKind::Hosts)
        {
            if set.id == SYSTEM_HOSTS_SET_ID {
                entries.extend(read_system_hosts_pairs().into_iter().enumerate().map(
                    |(index, (domain, addr))| HostsEntry {
                        id: format!("system-{index}"),
                        enabled: true,
                        domain,
                        addr,
                    },
                ));
            } else {
                entries.extend(set.hosts.iter().cloned());
            }
        }
        HostsConfig {
            enabled: !entries.is_empty(),
            include_system: false,
            entries,
        }
    }

    pub fn reset_builtin_dns_set(&mut self) {
        let enabled = self
            .rule_sets
            .iter()
            .find(|set| set.id == BUILTIN_DNS_SET_ID)
            .map(|set| set.enabled)
            .unwrap_or(true);
        let replacement = DnsRuleSet::builtin_dns(Self::factory_rules_base(), enabled);
        if let Some(set) = self
            .rule_sets
            .iter_mut()
            .find(|set| set.id == BUILTIN_DNS_SET_ID)
        {
            *set = replacement;
        } else {
            self.rule_sets.insert(0, replacement);
        }
    }

    /// Defensive compatibility for settings that have not been persisted yet.
    pub fn effective_rules_enabled(&self) -> bool {
        self.rules_enabled
    }

    /// Normalize `dns_final` to a known value: `local` | `domestic` | `remote`.
    /// Unknown/empty falls back to `remote` (safest for uncovered foreign sites).
    pub fn normalize_dns_final(&self) -> &str {
        match self.dns_final.trim().to_ascii_lowercase().as_str() {
            "local" => "local",
            "domestic" | "cn" => "domestic",
            _ => "remote",
        }
    }

    /// Built-in DNS rules loaded from `resources/dns/builtin-dns-rules.list`.
    /// Falls back to a hardcoded minimum if the file is missing.
    pub fn factory_rules_base() -> Vec<DnsRule> {
        default_rules()
    }
}

fn default_true() -> bool {
    true
}

fn default_dns_final() -> String {
    "remote".into()
}

fn default_fakeip_v4() -> String {
    "198.18.0.0/15".into()
}

fn default_fakeip_v6() -> String {
    "fc00::/18".into()
}

fn default_fakeip_bypass() -> Vec<String> {
    vec![
        "local".into(),
        "lan".into(),
        "internal".into(),
        "corp".into(),
        "localhost".into(),
    ]
}

/// Hardcoded fallback used when `resources/dns/builtin-dns-rules.list` is unavailable.
fn hardcoded_default_rules() -> Vec<DnsRule> {
    ["local", "lan", "internal", "corp"]
        .into_iter()
        .map(|s| DnsRule {
            id: format!("bypass-{s}"),
            enabled: true,
            matcher: DomainMatcher::DomainSuffix,
            payload: s.into(),
            action: DnsAction::Local,
        })
        .collect()
}

/// Built-in DNS rules. Loaded from `resources/dns/builtin-dns-rules.list`
/// (resolved relative to `CARGO_MANIFEST_DIR`); falls back to
/// [`hardcoded_default_rules`] if the file cannot be read.
fn default_rules() -> Vec<DnsRule> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("dns")
        .join("builtin-dns-rules.list");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed = parse_dns_whitelist_text(&text, "builtin");
            if parsed.is_empty() {
                hardcoded_default_rules()
            } else {
                parsed
            }
        }
        Err(_) => hardcoded_default_rules(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_rules_mode_enables_legacy_rules_layer() {
        let mut settings: DnsSettings = serde_json::from_value(serde_json::json!({
            "mode": "rules"
        }))
        .unwrap();
        assert_eq!(settings.mode, DnsMode::Rules);
        assert!(!settings.rules_enabled);

        settings.migrate_legacy_rules_mode();

        assert_eq!(settings.mode, DnsMode::Local);
        assert!(settings.rules_enabled);
    }

    #[test]
    fn legacy_flat_rules_and_hosts_migrate_into_typed_sets() {
        let mut settings = DnsSettings {
            rules_enabled: true,
            hosts: HostsConfig {
                enabled: true,
                include_system: true,
                entries: vec![HostsEntry {
                    id: "legacy-host".into(),
                    enabled: true,
                    domain: "legacy.example".into(),
                    addr: "10.0.0.8".into(),
                }],
            },
            ..DnsSettings::default()
        };

        settings.ensure_rule_sets();

        let builtin = settings
            .rule_sets
            .iter()
            .find(|set| set.id == BUILTIN_DNS_SET_ID)
            .unwrap();
        assert!(builtin.enabled);
        assert_eq!(builtin.kind, DnsRuleSetKind::Dns);
        assert!(!builtin.dns_rules.is_empty());

        let system = settings
            .rule_sets
            .iter()
            .find(|set| set.id == SYSTEM_HOSTS_SET_ID)
            .unwrap();
        assert!(system.enabled);
        assert!(system.read_only);

        let custom = settings
            .rule_sets
            .iter()
            .find(|set| set.id == "migrated-hosts")
            .unwrap();
        assert_eq!(custom.kind, DnsRuleSetKind::Hosts);
        assert_eq!(custom.hosts[0].domain, "legacy.example");
    }

    #[test]
    fn enabled_typed_sets_are_flattened_in_set_order() {
        let mut settings = DnsSettings::default();
        settings.rule_sets = vec![
            DnsRuleSet {
                id: "first".into(),
                name: "First".into(),
                kind: DnsRuleSetKind::Dns,
                builtin: false,
                read_only: false,
                enabled: true,
                dns_rules: vec![DnsRule {
                    id: "r1".into(),
                    enabled: true,
                    matcher: DomainMatcher::Domain,
                    payload: "first.example".into(),
                    action: DnsAction::Local,
                }],
                hosts: Vec::new(),
            },
            DnsRuleSet {
                id: "second".into(),
                name: "Second".into(),
                kind: DnsRuleSetKind::Dns,
                builtin: false,
                read_only: false,
                enabled: true,
                dns_rules: vec![DnsRule {
                    id: "r2".into(),
                    enabled: true,
                    matcher: DomainMatcher::Domain,
                    payload: "second.example".into(),
                    action: DnsAction::Remote,
                }],
                hosts: Vec::new(),
            },
        ];

        let rules = settings.enabled_dns_rules();
        assert_eq!(rules[0].payload, "first.example");
        assert_eq!(rules[1].payload, "second.example");
    }
}

/// Path to the OS hosts file.
fn system_hosts_path() -> std::path::PathBuf {
    if cfg!(target_os = "windows") {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        std::path::PathBuf::from(root)
            .join("System32")
            .join("drivers")
            .join("etc")
            .join("hosts")
    } else {
        // macOS / Linux
        std::path::PathBuf::from("/etc/hosts")
    }
}

/// Parse an OS hosts file into `(domain, ip)` pairs.
///
/// Format: `IP  domain1  domain2 ...` (whitespace-separated); `#` starts a comment.
/// Only valid IP literals in the first column are kept.
fn parse_hosts_text(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_ascii_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let ip = parts[0].trim();
        // Accept IPv4 / IPv6 literals only.
        let is_ip = ip.parse::<std::net::IpAddr>().is_ok();
        if !is_ip {
            continue;
        }
        for domain in &parts[1..] {
            let d = domain.trim();
            if !d.is_empty() {
                out.push((d.to_ascii_lowercase(), ip.to_string()));
            }
        }
    }
    out
}

/// Read the OS hosts file and return `(domain, ip)` pairs. Fail-soft: empty on any error.
pub fn read_system_hosts_pairs() -> Vec<(String, String)> {
    match std::fs::read_to_string(system_hosts_path()) {
        Ok(text) => parse_hosts_text(&text),
        Err(_) => Vec::new(),
    }
}

/// Read the OS hosts file as [`HostsEntry`] list (for the UI command).
pub fn read_system_hosts_entries() -> Vec<HostsEntry> {
    read_system_hosts_pairs()
        .into_iter()
        .enumerate()
        .map(|(i, (domain, addr))| HostsEntry {
            id: format!("sys-{i}"),
            enabled: true,
            domain,
            addr,
        })
        .collect()
}

fn parse_dns_action(raw: &str) -> Option<DnsAction> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "LOCAL" | "SYSTEM" => Some(DnsAction::Local),
        "DOMESTIC" | "CN" => Some(DnsAction::Domestic),
        "REMOTE" | "PROXY" => Some(DnsAction::Remote),
        "BLOCK" | "REJECT" => Some(DnsAction::Block),
        _ => None,
    }
}

fn parse_dns_matcher(kind: &str) -> Option<DomainMatcher> {
    match kind.trim().to_ascii_uppercase().as_str() {
        "DOMAIN" => Some(DomainMatcher::Domain),
        "DOMAIN-SUFFIX" | "SUFFIX" => Some(DomainMatcher::DomainSuffix),
        "DOMAIN-KEYWORD" | "KEYWORD" => Some(DomainMatcher::DomainKeyword),
        _ => None,
    }
}

/// Parse one DNS whitelist list file.
///
/// Lines:
/// - `example.com` → domain_suffix + local
/// - `DOMAIN-SUFFIX,example.com,LOCAL`
/// - `DOMAIN,api.example.com,SYSTEM`
/// - `DOMAIN-KEYWORD,corp,DOMESTIC`
pub fn parse_dns_whitelist_text(text: &str, file_stem: &str) -> Vec<DnsRule> {
    let mut out = Vec::new();
    let mut idx = 0u32;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (matcher, payload, action) = if line.contains(',') {
            let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if parts.len() < 2 {
                continue;
            }
            let Some(matcher) = parse_dns_matcher(parts[0]) else {
                continue;
            };
            let payload = parts[1].trim();
            if payload.is_empty() {
                continue;
            }
            let action = if parts.len() >= 3 {
                parse_dns_action(parts[2]).unwrap_or(DnsAction::Local)
            } else {
                DnsAction::Local
            };
            (matcher, payload.to_string(), action)
        } else {
            // bare domain → domain_suffix + local
            if line.contains(char::is_whitespace) {
                continue;
            }
            (
                DomainMatcher::DomainSuffix,
                line.to_string(),
                DnsAction::Local,
            )
        };
        idx += 1;
        out.push(DnsRule {
            id: format!("bundled-{file_stem}-{idx}"),
            enabled: true,
            matcher,
            payload,
            action,
        });
    }
    out
}

#[cfg(test)]
mod whitelist_tests {
    use super::*;

    #[test]
    fn parse_bare_and_explicit() {
        let text = r#"
# comment
xiaojukeji.com
DOMAIN-SUFFIX,didichuxing.com,SYSTEM
DOMAIN,api.example.com,DOMESTIC
DOMAIN-KEYWORD,corp,REMOTE
"#;
        let rules = parse_dns_whitelist_text(text, "test");
        assert_eq!(rules.len(), 4);
        assert!(matches!(rules[0].matcher, DomainMatcher::DomainSuffix));
        assert_eq!(rules[0].payload, "xiaojukeji.com");
        assert!(matches!(rules[0].action, DnsAction::Local));
        assert!(matches!(rules[2].matcher, DomainMatcher::Domain));
        assert!(matches!(rules[2].action, DnsAction::Domestic));
        assert!(matches!(rules[3].action, DnsAction::Remote));
    }

    #[test]
    fn default_rules_load_builtin_file() {
        let rules = default_rules();
        // The builtin file seeds local/lan/internal/corp/localhost at minimum.
        assert!(rules.iter().any(|r| r.payload == "local"));
        assert!(rules.iter().any(|r| r.payload == "localhost"));
        assert!(rules.iter().all(|r| matches!(r.action, DnsAction::Local)));
    }
}
