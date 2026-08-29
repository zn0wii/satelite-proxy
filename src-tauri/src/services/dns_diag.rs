//! Core-level DNS diagnostics (DNS 设置页「诊断」).
//!
//! Two layers, deliberately split because neither core's API can provide
//! both:
//!
//! 1. **Runtime query** — sing-box / mihomo expose
//!    `GET /dns/query?name=&type=` (`ClashApi::dns_query`). sing-box routes
//!    the exchange through its full DNS rule chain (the same path real
//!    traffic takes) before falling back to `dns.final`; mihomo answers via
//!    its default resolver with nameserver-policy applied. Neither response
//!    names the upstream server, so this layer yields answers/TTL/rcode
//!    only. Xray has no such API at all.
//! 2. **Path derivation** — the app generates every config itself, so the
//!    decision chain is fully known and can be replayed locally per domain:
//!    rule sets (store order) → Hosts → DNS-page rules → FakeIP →
//!    `dns_final`, mirroring `config/builder.rs` / `dns_build.rs` /
//!    `xray.rs` / `mihomo.rs` exactly, including the per-core divergences
//!    documented in AGENTS.md §18. Remote `.srs` sets are matched from the
//!    local cache (Xray/mihomo actually match their own geodata copy, hence
//!    an `approx` flag).
//!
//! This module never mutates anything — it is a read-only diagnostic.

use crate::api::ClashApi;
use crate::config::lookup_hosts;
use crate::config::to_ascii_domain;
use crate::core::CoreKind;
use crate::domain::OutboundMode;
use crate::domain::{
    DnsAction, DnsRule, DnsSettings, DomainMatcher, FakeIpConfig, HostsConfig, RuleSet,
    RuleSetDnsStrategy, RuleSetStrategy, RuleType, DOMESTIC_DNS_POOL, REMOTE_DNS_POOL,
};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Per-domain DNS query-path strategy, as derived from the active config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsPathStrategy {
    Remote,
    Domestic,
    Local,
    Block,
    Hosts,
    FakeIp,
}

/// One resource record from the core's `/dns/query` answer.
#[derive(Debug, Clone, Serialize)]
pub struct DnsDiagAnswer {
    pub name: String,
    #[serde(rename = "type")]
    pub rr_type: i64,
    pub ttl: i64,
    pub data: String,
}

/// Derived query path for one domain (config replay; `None` for custom
/// configs whose DNS layout the app did not generate).
#[derive(Debug, Clone, Serialize)]
pub struct DnsDiagPath {
    pub strategy: DnsPathStrategy,
    /// Human-readable resolver lines, e.g. `https://1.1.1.1/dns-query（DoH · 经代理）`.
    pub servers: Vec<String>,
    pub via_proxy: bool,
    /// What matched, e.g. `规则集「海外网站」（后缀 google.com）` or `默认解析（remote）`.
    pub matched_by: String,
    /// Remote-set / geodata membership was evaluated from a local cache —
    /// the kernel's own data is authoritative.
    pub approx: bool,
    pub note: Option<String>,
}

/// Live `/dns/query` result through the running core.
#[derive(Debug, Clone, Serialize)]
pub struct DnsDiagQuery {
    pub ok: bool,
    pub status_code: i64,
    pub status_text: String,
    pub answers: Vec<DnsDiagAnswer>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DnsDiagDomainResult {
    pub domain: String,
    pub path: Option<DnsDiagPath>,
    pub query: Option<DnsDiagQuery>,
    /// Why no runtime query ran (core stopped / Xray / custom without API).
    pub query_note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DnsDiagReport {
    pub core_type: String,
    pub running: bool,
    /// `generated` | `custom`
    pub runtime_source: String,
    pub results: Vec<DnsDiagDomainResult>,
    pub notes: Vec<String>,
}

/// Everything the analyzer needs, snapshotted from the store.
pub struct DnsDiagInput {
    pub core_type: String,
    pub runtime_source: String,
    pub tun_enabled: bool,
    pub outbound_mode: OutboundMode,
    pub rule_sets: Vec<RuleSet>,
    pub dns: DnsSettings,
    /// App data dir (`<data>/remote-rule-sets` holds the .srs caches).
    pub data_dir: PathBuf,
}

/// Which item kind won a match (keyword needs special handling on mihomo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Exact,
    Suffix,
    Keyword,
}

/// Domain-relevant items of one rule set, in match-friendly form.
#[derive(Default)]
struct DomainItems {
    exact: HashSet<String>,
    suffix: HashSet<String>,
    keywords: Vec<String>,
}

impl DomainItems {
    fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.suffix.is_empty() && self.keywords.is_empty()
    }

    /// Match a lowercased wire-format domain (sing-box semantics: exact =
    /// equal; suffix = equal or any parent label; keyword = raw substring).
    fn match_domain(&self, domain: &str) -> Option<(ItemKind, String)> {
        if self.exact.contains(domain) {
            return Some((ItemKind::Exact, format!("精确 {domain}")));
        }
        let mut rest = domain;
        loop {
            if self.suffix.contains(rest) {
                return Some((ItemKind::Suffix, format!("后缀 {rest}")));
            }
            match rest.split_once('.') {
                Some((_, parent)) => rest = parent,
                None => break,
            }
        }
        for keyword in &self.keywords {
            if domain.contains(keyword.as_str()) {
                return Some((ItemKind::Keyword, format!("关键词 {keyword}")));
            }
        }
        None
    }
}

/// One rule set prepared for path decisions.
struct SetInfo {
    name: String,
    items: DomainItems,
    /// sing-box rejects DNS for Block-strategy sets; mihomo/Xray only block
    /// at the routing layer (their generators never emit DNS rejects).
    block_route: bool,
    dns_strategy: RuleSetDnsStrategy,
    /// Matched against the locally cached copy — kernel data is authoritative
    /// (always true for the builtin sets under Xray/mihomo, whose geodata is
    /// compiled from the same upstream source but not byte-identical).
    approx: bool,
}

/// Mirrors `config::builder::inline_rule_is_effective` (private there).
fn inline_rule_is_effective(rule: &crate::domain::Rule) -> bool {
    rule.enabled && !rule.payload.trim().is_empty() && rule.rule_type != RuleType::Geoip
}

/// Domain items of a local (editable) rule set, mirroring
/// `build_headless_rules` normalization: enabled + non-empty payload,
/// `*`/leading-`.` stripped, Domain/Suffix punycode'd, keyword kept raw.
/// IP/process items never match a DNS query and are dropped.
fn local_set_items(set: &RuleSet) -> DomainItems {
    let mut items = DomainItems::default();
    for rule in set.rules.iter().filter(|r| inline_rule_is_effective(r)) {
        let payload = rule.payload.trim();
        let normalized = match rule.rule_type {
            RuleType::Domain | RuleType::DomainSuffix => payload.trim_start_matches(['*', '.']),
            _ => payload,
        };
        let value = match rule.rule_type {
            RuleType::Domain | RuleType::DomainSuffix => to_ascii_domain(normalized),
            _ => normalized.to_string(),
        };
        if value.is_empty() {
            continue;
        }
        match rule.rule_type {
            RuleType::Domain => {
                items.exact.insert(value.to_ascii_lowercase());
            }
            RuleType::DomainSuffix => {
                items.suffix.insert(value.to_ascii_lowercase());
            }
            RuleType::DomainKeyword => {
                // Kept even under mihomo (whose nameserver-policy has no
                // keyword form): `decide` needs to see the hit to report the
                // fallback instead of silently ignoring the rule.
                items.keywords.push(value);
            }
            _ => {} // ip_cidr / process: cannot match a DNS query
        }
    }
    items
}

/// Collect domain items from one source/binary rule-set rule (JSON shape).
/// `approx` is set when the rule contains items we cannot evaluate exactly
/// (regex / logical / inverted / adguard). `query_type`-restricted rules are
/// skipped entirely when they cannot match an A query.
fn collect_remote_rule_items(rule: &serde_json::Value, items: &mut DomainItems, approx: &mut bool) {
    let Some(object) = rule.as_object() else {
        return;
    };
    if object.get("type").is_some() {
        // Logical (and/or) rule — membership can't be replayed exactly.
        *approx = true;
    }
    if object
        .get("invert")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        *approx = true;
    }
    if let Some(query_types) = object.get("query_type").and_then(|v| v.as_array()) {
        // A = 1. A rule that can never match an A query is dead for us.
        if !query_types.iter().any(|v| v.as_i64() == Some(1)) {
            return;
        }
    }
    for (field, value) in object {
        let list: Vec<String> = value
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        match field.as_str() {
            "domain" => items
                .exact
                .extend(list.iter().map(|s| s.to_ascii_lowercase())),
            "domain_suffix" => items
                .suffix
                .extend(list.iter().map(|s| s.to_ascii_lowercase())),
            "domain_keyword" => items.keywords.extend(list),
            "domain_regex" => *approx = true,
            "ad_guard_domain" => {
                // `||domain^` lines behave as suffix matches.
                for line in list {
                    let domain = line
                        .trim_start_matches("||")
                        .trim_start_matches('|')
                        .trim_end_matches('^')
                        .trim()
                        .to_ascii_lowercase();
                    if !domain.is_empty() {
                        items.suffix.insert(domain);
                    }
                }
                *approx = true;
            }
            _ => {}
        }
    }
}

/// Load a downloaded remote rule set (`.srs` binary or source JSON) from the
/// cache dir. Mirrors the trust boundary of `list_remote_rule_items`: the
/// canonicalized file must live inside `<data>/remote-rule-sets`.
fn load_remote_items(
    cache_dir: &Path,
    remote: &crate::domain::RemoteRuleSetConfig,
) -> Option<(DomainItems, bool)> {
    let local_path = remote.local_path.as_deref()?.trim();
    if local_path.is_empty() {
        return None;
    }
    let file = std::fs::canonicalize(Path::new(local_path)).ok()?;
    let cache = std::fs::canonicalize(cache_dir).ok()?;
    if file.parent() != Some(cache.as_path()) {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    let source: serde_json::Value = if remote.format == "binary" {
        let parsed = crate::srs::parse_with_rules(&bytes).ok()?;
        parsed.display_source()
    } else {
        serde_json::from_slice(&bytes).ok()?
    };
    let rules = source.get("rules")?.as_array()?.clone();
    let mut items = DomainItems::default();
    let mut approx = false;
    for rule in &rules {
        collect_remote_rule_items(rule, &mut items, &mut approx);
    }
    Some((items, approx))
}

/// Replays the DNS decision chain of the active generator for one domain.
pub struct DnsPathAnalyzer {
    core: CoreKind,
    sets: Vec<SetInfo>,
    hosts: HostsConfig,
    dns_rules: Vec<DnsRule>,
    fake_ip: FakeIpConfig,
    /// mihomo forces fake-ip under TUN even when the toggle is off.
    fakeip_active: bool,
    outbound_direct: bool,
    dns_final: String,
    /// Names of sets whose membership is cache-derived (for report notes).
    approx_sets: Vec<String>,
}

impl DnsPathAnalyzer {
    pub fn build(input: &DnsDiagInput) -> Self {
        let core = CoreKind::parse(&input.core_type);
        let cache_dir = input.data_dir.join("remote-rule-sets");
        let mut sets = Vec::new();
        let mut approx_sets = Vec::new();

        for set in input.rule_sets.iter().filter(|set| set.enabled) {
            if let Some(remote) = &set.remote {
                // Mirrors the generators: a remote set without a usable cache
                // file is skipped everywhere (builder.rs drops the
                // definition and every rule referencing it).
                let has_cache = remote
                    .local_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|path| !path.is_empty() && Path::new(path).is_file())
                    .is_some();
                if !has_cache {
                    continue;
                }
                let builtin_geosite = matches!(
                    set.id.as_str(),
                    "system-geosite-cn" | "system-geolocation-not-cn"
                );
                match core {
                    CoreKind::SingBox => {
                        // Kernel loads the exact same cache file — exact match.
                        if let Some((items, approx)) = load_remote_items(&cache_dir, remote) {
                            if !items.is_empty() {
                                approx_sets.extend(approx.then(|| set.name.clone()));
                                sets.push(SetInfo {
                                    name: set.name.clone(),
                                    items,
                                    block_route: set.strategy == RuleSetStrategy::Block,
                                    dns_strategy: set.dns_strategy,
                                    approx,
                                });
                            }
                        }
                    }
                    CoreKind::Xray | CoreKind::Mihomo => {
                        // Only the two builtin geosite sets map onto geodata;
                        // user-built .srs sets are skipped by both generators.
                        if !builtin_geosite {
                            continue;
                        }
                        if let Some((items, _)) = load_remote_items(&cache_dir, remote) {
                            // The kernel matches its own geodata (compiled from
                            // the same upstream source, not byte-identical).
                            // Keyword items stay so `decide` can report the
                            // mihomo nameserver-policy keyword fallback.
                            if !items.is_empty() {
                                approx_sets.push(set.name.clone());
                                sets.push(SetInfo {
                                    name: set.name.clone(),
                                    items,
                                    block_route: false,
                                    dns_strategy: set.dns_strategy,
                                    approx: true,
                                });
                            }
                        }
                    }
                }
            } else {
                // Local set. All cores classify every enabled local set
                // (Filter sets included — they still classify DNS even though
                // their rules never reach `effective_route_rules`).
                let items = local_set_items(set);
                if items.is_empty() {
                    // Mirrors `build_headless_rules` → None: the kernel drops
                    // the definition together with its DNS rule.
                    continue;
                }
                sets.push(SetInfo {
                    name: set.name.clone(),
                    items,
                    block_route: set.strategy == RuleSetStrategy::Block,
                    dns_strategy: set.dns_strategy,
                    approx: false,
                });
            }
        }

        let hosts = input.dns.effective_hosts();
        let dns_rules = input.dns.enabled_dns_rules();
        let fakeip_active = match core {
            CoreKind::SingBox => input.dns.fake_ip.enabled,
            // mihomo: `tun_enabled || fake_ip.enabled` (mihomo.rs build_dns).
            CoreKind::Mihomo => input.tun_enabled || input.dns.fake_ip.enabled,
            // Xray: `tun_enabled && fake_ip.enabled` (xray.rs build_dns).
            CoreKind::Xray => input.tun_enabled && input.dns.fake_ip.enabled,
        };

        Self {
            core,
            sets,
            hosts,
            dns_rules,
            fake_ip: input.dns.fake_ip.clone(),
            fakeip_active,
            outbound_direct: input.outbound_mode == OutboundMode::Direct,
            dns_final: input.dns.normalize_dns_final().to_string(),
            approx_sets,
        }
    }

    /// Names of cache-derived sets (for the report's global notes).
    pub fn approx_set_names(&self) -> &[String] {
        &self.approx_sets
    }

    fn hosts_path(&self, domain: &str) -> Option<DnsDiagPath> {
        let addrs = lookup_hosts(&self.hosts, domain);
        if addrs.is_empty() {
            return None;
        }
        Some(DnsDiagPath {
            strategy: DnsPathStrategy::Hosts,
            servers: addrs.iter().map(|ip| format!("{ip}（静态映射）")).collect(),
            via_proxy: false,
            matched_by: "Hosts 静态映射（精确域名）".into(),
            approx: false,
            note: None,
        })
    }

    /// Resolver lines for a pool under the active core.
    fn pool_path(&self, pool: RuleSetDnsStrategy, matched_by: String, approx: bool) -> DnsDiagPath {
        match self.core {
            CoreKind::SingBox => match pool {
                RuleSetDnsStrategy::Remote => DnsDiagPath {
                    strategy: DnsPathStrategy::Remote,
                    servers: vec![format!("{}（DoH · 经代理出口）", REMOTE_DNS_POOL[0])],
                    via_proxy: true,
                    matched_by,
                    approx,
                    note: None,
                },
                RuleSetDnsStrategy::Domestic => DnsDiagPath {
                    strategy: DnsPathStrategy::Domestic,
                    servers: vec![format!("{}（UDP 明文 · 直连）", DOMESTIC_DNS_POOL[0])],
                    via_proxy: false,
                    matched_by,
                    approx,
                    note: None,
                },
                RuleSetDnsStrategy::Local => DnsDiagPath {
                    strategy: DnsPathStrategy::Local,
                    servers: vec!["系统解析（dns-local）".into()],
                    via_proxy: false,
                    matched_by,
                    approx,
                    note: None,
                },
            },
            CoreKind::Mihomo => {
                let via_proxy = pool == RuleSetDnsStrategy::Remote && !self.outbound_direct;
                match pool {
                    RuleSetDnsStrategy::Remote => DnsDiagPath {
                        strategy: DnsPathStrategy::Remote,
                        servers: REMOTE_DNS_POOL
                            .iter()
                            .map(|url| {
                                let egress = if self.outbound_direct {
                                    "直连"
                                } else {
                                    "经代理"
                                };
                                format!("{url}（DoH · {egress}）")
                            })
                            .collect(),
                        via_proxy,
                        matched_by,
                        approx,
                        note: Some("mihomo 并发竞速整池".into()),
                    },
                    RuleSetDnsStrategy::Domestic => DnsDiagPath {
                        strategy: DnsPathStrategy::Domestic,
                        servers: DOMESTIC_DNS_POOL
                            .iter()
                            .map(|ip| format!("{ip}（UDP 明文 · 直连）"))
                            .collect(),
                        via_proxy: false,
                        matched_by,
                        approx,
                        note: Some("mihomo 并发竞速整池".into()),
                    },
                    RuleSetDnsStrategy::Local => DnsDiagPath {
                        strategy: DnsPathStrategy::Local,
                        servers: vec!["system（系统解析）".into()],
                        via_proxy: false,
                        matched_by,
                        approx,
                        note: None,
                    },
                }
            }
            CoreKind::Xray => match pool {
                RuleSetDnsStrategy::Remote => DnsDiagPath {
                    strategy: DnsPathStrategy::Remote,
                    // Second pool entry is pure in-pool redundancy; the other
                    // pools carry skipFallback and answer their own domains
                    // only (dns_final stays the sole fallback).
                    servers: vec![format!("{}（DoH · 经主出站）", REMOTE_DNS_POOL[0])],
                    via_proxy: true,
                    matched_by,
                    approx,
                    note: Some("Xray 池内顺序回退（8.8.8.8 备援）".into()),
                },
                RuleSetDnsStrategy::Domestic => DnsDiagPath {
                    strategy: DnsPathStrategy::Domestic,
                    servers: vec![format!(
                        "{}（UDP 明文 · 直连 direct-dns）",
                        DOMESTIC_DNS_POOL[0]
                    )],
                    via_proxy: false,
                    matched_by,
                    approx,
                    note: None,
                },
                RuleSetDnsStrategy::Local => DnsDiagPath {
                    strategy: DnsPathStrategy::Local,
                    servers: vec!["localhost（系统解析）".into()],
                    via_proxy: false,
                    matched_by,
                    approx,
                    note: None,
                },
            },
        }
    }

    fn block_path(&self, matched_by: String) -> DnsDiagPath {
        DnsDiagPath {
            strategy: DnsPathStrategy::Block,
            servers: Vec::new(),
            via_proxy: false,
            matched_by,
            approx: false,
            note: None,
        }
    }

    fn fakeip_path(&self, matched_by: String, note: Option<String>) -> DnsDiagPath {
        DnsDiagPath {
            strategy: DnsPathStrategy::FakeIp,
            servers: vec![format!("FakeIP 池（{}）", self.fake_ip.inet4_range)],
            via_proxy: false,
            matched_by,
            approx: false,
            note,
        }
    }

    fn final_path(&self, extra_note: Option<String>) -> DnsDiagPath {
        let pool = match self.dns_final.as_str() {
            "local" => RuleSetDnsStrategy::Local,
            "domestic" => RuleSetDnsStrategy::Domestic,
            _ => RuleSetDnsStrategy::Remote,
        };
        let mut path = self.pool_path(
            pool,
            format!("默认解析（dns_final = {}）", self.dns_final),
            false,
        );
        // Xray answers unmatched domains with the primary (dns_final) pool —
        // or its in-pool backup; with fakedns enabled the fake pool may
        // answer A queries first (v2rayN-style semantics).
        if self.core == CoreKind::Xray && self.fakeip_active {
            let note = "TUN + FakeIP 启用：A 查询可能由 fakedns 池直接应答".to_string();
            path.note = Some(match path.note {
                Some(existing) => format!("{existing}；{note}"),
                None => note,
            });
        }
        if let Some(note) = extra_note {
            path.note = Some(match path.note {
                Some(existing) => format!("{existing}；{note}"),
                None => note,
            });
        }
        path
    }

    /// Whether any bypass suffix matches (FakeIP bypass / fake-ip-filter).
    fn bypass_hit(&self, domain: &str) -> Option<String> {
        let mut rest = domain;
        loop {
            let lowered = rest.to_ascii_lowercase();
            for suffix in &self.fake_ip.bypass {
                let normalized = suffix
                    .trim()
                    .trim_start_matches('*')
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                if !normalized.is_empty() && normalized == lowered {
                    return Some(normalized);
                }
            }
            match rest.split_once('.') {
                Some((_, parent)) => rest = parent,
                None => return None,
            }
        }
    }

    /// Replay the decision chain for one (already cleaned) domain.
    pub fn decide(&self, raw_domain: &str) -> DnsDiagPath {
        let domain = to_ascii_domain(raw_domain.trim().trim_end_matches('.')).to_ascii_lowercase();

        // Hosts: highest priority on mihomo/Xray (`dns.hosts` map). sing-box
        // checks rule-set DNS rules first (builder prepends them before the
        // hosts rule), so there it runs after the set loop below.
        if self.core != CoreKind::SingBox {
            if let Some(path) = self.hosts_path(&domain) {
                return path;
            }
        }

        // 1. Rule sets in store order.
        let mut mihomo_keyword_deferred: Option<String> = None;
        for set in &self.sets {
            let Some((kind, hit)) = set.items.match_domain(&domain) else {
                continue;
            };
            let matched_by = format!("规则集「{}」（{hit}）", set.name);
            if self.core == CoreKind::SingBox && set.block_route {
                return self.block_path(matched_by);
            }
            if self.core == CoreKind::Mihomo && kind == ItemKind::Keyword {
                // Keyword-classified domains have no nameserver-policy form
                // and fall back to `nameserver` (the dns_final pool).
                mihomo_keyword_deferred = Some(format!(
                    "关键词规则在 mihomo 下无 DNS 策略形式（{matched_by}），回落默认解析"
                ));
                continue;
            }
            return self.pool_path(set.dns_strategy, matched_by, set.approx);
        }

        // 2. Hosts (sing-box position: after rule sets).
        if self.core == CoreKind::SingBox {
            if let Some(path) = self.hosts_path(&domain) {
                return path;
            }
        }

        // 3. DNS-page rules in stored order (semantics mirror
        // `user_rule_to_json`: Domain/Suffix punycode'd, keyword raw).
        for rule in self.dns_rules.iter().filter(|r| r.enabled) {
            let Some((kind, hit)) = dns_rule_hit(rule, &domain) else {
                continue;
            };
            let matched_by = format!("DNS 规则（{hit}）");
            match rule.action {
                DnsAction::Block => {
                    // sing-box emits `action: reject`; mihomo/Xray drop the
                    // rule entirely (their generators filter Block out).
                    if self.core == CoreKind::SingBox {
                        return self.block_path(matched_by);
                    }
                    continue;
                }
                action => {
                    if self.core == CoreKind::Mihomo && kind == ItemKind::Keyword {
                        mihomo_keyword_deferred = Some(format!(
                            "关键词规则在 mihomo 下无 DNS 策略形式（{matched_by}），回落默认解析"
                        ));
                        continue;
                    }
                    let pool = match action {
                        DnsAction::Local => RuleSetDnsStrategy::Local,
                        DnsAction::Domestic => RuleSetDnsStrategy::Domestic,
                        DnsAction::Remote => RuleSetDnsStrategy::Remote,
                        DnsAction::Block => unreachable!(),
                    };
                    return self.pool_path(pool, matched_by, false);
                }
            }
        }

        // 4. FakeIP. sing-box: bypass suffixes → local, then A/AAAA → fakeip.
        // mihomo: fake-ip-filter suffixes keep real resolution and fall
        // through to the default resolver; everything else gets a fake IP.
        if self.fakeip_active {
            match self.core {
                CoreKind::SingBox => {
                    if let Some(suffix) = self.bypass_hit(&domain) {
                        return self.pool_path(
                            RuleSetDnsStrategy::Local,
                            format!("FakeIP 旁路后缀（{suffix}）"),
                            false,
                        );
                    }
                    return self.fakeip_path("A/AAAA 查询（FakeIP 启用）".into(), None);
                }
                CoreKind::Mihomo => {
                    if self.bypass_hit(&domain).is_none() {
                        return self.fakeip_path(
                            "A 查询（fake-ip 模式）".into(),
                            Some("fake-ip-filter 内的后缀走真实解析".into()),
                        );
                    }
                }
                CoreKind::Xray => {}
            }
        }

        // 5. dns_final fallback.
        self.final_path(mihomo_keyword_deferred)
    }
}

/// Match one DNS-page rule, mirroring `user_rule_to_json` normalization.
fn dns_rule_hit(rule: &DnsRule, domain: &str) -> Option<(ItemKind, String)> {
    let payload = rule.payload.trim();
    if payload.is_empty() {
        return None;
    }
    let payload = match rule.matcher {
        DomainMatcher::DomainSuffix => payload
            .trim()
            .trim_start_matches('*')
            .trim_start_matches('.')
            .to_string(),
        _ => payload.trim_start_matches('.').to_string(),
    };
    if payload.is_empty() {
        return None;
    }
    match rule.matcher {
        DomainMatcher::Domain => {
            let normalized = to_ascii_domain(&payload).to_ascii_lowercase();
            (normalized == domain).then(|| (ItemKind::Exact, format!("精确 {normalized}")))
        }
        DomainMatcher::DomainSuffix => {
            let normalized = to_ascii_domain(&payload).to_ascii_lowercase();
            if normalized == domain {
                return Some((ItemKind::Suffix, format!("后缀 {normalized}")));
            }
            domain
                .ends_with(&format!(".{normalized}"))
                .then(|| (ItemKind::Suffix, format!("后缀 {normalized}")))
        }
        DomainMatcher::DomainKeyword => domain
            .contains(payload.as_str())
            .then(|| (ItemKind::Keyword, format!("关键词 {payload}"))),
    }
}

/// Strip URL decoration from user input (`https://a.com/x:80` → `a.com`).
fn clean_domain(raw: &str) -> Option<String> {
    let domain = raw
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(raw.trim())
        .split(':')
        .next()
        .unwrap_or(raw.trim())
        .trim()
        .to_string();
    (!domain.is_empty() && domain != ".").then_some(domain)
}

/// Map a DNS RCODE to its mnemonic.
fn rcode_text(code: i64) -> String {
    match code {
        0 => "NOERROR".into(),
        1 => "FORMERR".into(),
        2 => "SERVFAIL".into(),
        3 => "NXDOMAIN".into(),
        4 => "NOTIMP".into(),
        5 => "REFUSED".into(),
        other => format!("RCODE {other}"),
    }
}

/// Upper bound on diagnosed domains per run. The frontend allows 5 presets
/// + 32 persisted customs (37 max); 64 leaves headroom while still capping
/// a hostile invoke. Queries run in parallel with a per-domain timeout.
const MAX_DOMAINS: usize = 64;
/// Per-domain runtime query timeout.
const QUERY_TIMEOUT: Duration = Duration::from_secs(6);

/// Run the full diagnosis: local path replay for every domain plus live
/// `/dns/query` calls through the running core (parallel, one blocking task
/// per domain — ureq must not run on the async runtime).
pub async fn run(
    input: DnsDiagInput,
    domains: Vec<String>,
    running: bool,
    api: Option<ClashApi>,
) -> DnsDiagReport {
    let core = CoreKind::parse(&input.core_type);
    let core_type = input.core_type.clone();
    let runtime_source = input.runtime_source.clone();

    // Dedup + cap, preserving order.
    let mut cleaned: Vec<String> = Vec::new();
    for raw in domains {
        if let Some(domain) = clean_domain(&raw) {
            if !cleaned.iter().any(|d| d.eq_ignore_ascii_case(&domain)) {
                cleaned.push(domain);
            }
        }
    }
    cleaned.truncate(MAX_DOMAINS);

    let analyzer = {
        // .srs cache parsing is blocking IO — off the async runtime.
        tauri::async_runtime::spawn_blocking(move || DnsPathAnalyzer::build(&input))
            .await
            .map_err(|e| format!("dns diag analyzer: {e}"))
            .ok()
    };

    let mut notes: Vec<String> = Vec::new();
    if let Some(analyzer) = analyzer.as_ref() {
        if !analyzer.approx_set_names().is_empty() {
            let names = analyzer.approx_set_names().join("、");
            notes.push(format!(
                "规则集 {names} 的成员按本地缓存近似判定，内核以自身数据为准"
            ));
        }
    }
    let path_available = runtime_source != "custom";
    if !path_available {
        notes.push("自定义配置：查询路径推演不可用（配置非应用生成）".into());
    }

    // Live queries: sing-box / mihomo only, core running, API reachable.
    // Custom sing-box configs work too when they enable clash_api (`api` is
    // Some only in that case).
    let can_query =
        running && api.is_some() && matches!(core, CoreKind::SingBox | CoreKind::Mihomo);
    let query_note: Option<String> = if can_query {
        None
    } else if !running {
        Some("内核未运行，仅展示配置路径".into())
    } else if core == CoreKind::Xray {
        Some("Xray 内核无 DNS 查询 API，仅展示配置路径".into())
    } else {
        Some("当前配置未启用 Clash API，无法实时查询".into())
    };

    let mut handles = Vec::new();
    if let Some(api) = api.filter(|_| can_query) {
        for domain in &cleaned {
            let api = api.clone();
            let domain = domain.clone();
            handles.push(tauri::async_runtime::spawn_blocking(move || {
                let start = Instant::now();
                let result = api.dns_query(&domain, "A", QUERY_TIMEOUT);
                let elapsed_ms = start.elapsed().as_millis() as u64;
                (domain, result, elapsed_ms)
            }));
        }
    }

    let mut queries: std::collections::HashMap<String, DnsDiagQuery> =
        std::collections::HashMap::new();
    for handle in handles {
        let Ok((domain, result, elapsed_ms)) = handle.await else {
            continue;
        };
        let query = match result {
            Ok(response) => DnsDiagQuery {
                ok: response.status == 0,
                status_code: response.status,
                status_text: rcode_text(response.status),
                answers: response
                    .answers
                    .into_iter()
                    .map(|answer| DnsDiagAnswer {
                        name: answer.name,
                        rr_type: answer.rr_type,
                        ttl: answer.ttl,
                        data: answer.data,
                    })
                    .collect(),
                elapsed_ms,
                error: None,
            },
            Err(error) => DnsDiagQuery {
                ok: false,
                status_code: -1,
                status_text: String::new(),
                answers: Vec::new(),
                elapsed_ms,
                error: Some(error.to_string()),
            },
        };
        queries.insert(domain, query);
    }

    let results = cleaned
        .into_iter()
        .map(|domain| {
            let path = analyzer
                .as_ref()
                .filter(|_| path_available)
                .map(|analyzer| analyzer.decide(&domain));
            let query = queries.remove(&domain);
            let query_note = if query.is_none() {
                query_note.clone()
            } else {
                None
            };
            DnsDiagDomainResult {
                domain,
                path,
                query,
                query_note,
            }
        })
        .collect();

    DnsDiagReport {
        core_type,
        running,
        runtime_source,
        results,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DnsSettings, HostsConfig, HostsEntry, Rule, RuleSet, RuleTarget};

    fn input(core: &str, sets: Vec<RuleSet>, dns: DnsSettings) -> DnsDiagInput {
        DnsDiagInput {
            core_type: core.into(),
            runtime_source: "generated".into(),
            tun_enabled: false,
            outbound_mode: OutboundMode::Rule,
            rule_sets: sets,
            dns,
            data_dir: std::env::temp_dir(),
        }
    }

    fn local_set(
        id: &str,
        name: &str,
        rules: Vec<Rule>,
        dns_strategy: RuleSetDnsStrategy,
    ) -> RuleSet {
        RuleSet {
            id: id.into(),
            name: name.into(),
            builtin: false,
            enabled: true,
            ownership: crate::domain::RuleSetOwnership::User,
            strategy: crate::domain::RuleSetStrategy::Proxy,
            node_id: None,
            node_name: None,
            smart_include: Vec::new(),
            smart_exclude: Vec::new(),
            chain_id: None,
            chain_name: None,
            dns_strategy,
            remote: None,
            dns_rules: Vec::new(),
            rules,
        }
    }

    #[test]
    fn singbox_chain_set_beats_hosts_rules_and_final() {
        let sets = vec![local_set(
            "s1",
            "海外",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "google.com".into(),
                RuleTarget::Proxy,
                10,
            )],
            RuleSetDnsStrategy::Remote,
        )];
        let mut dns = DnsSettings::default();
        dns.fake_ip.enabled = false;
        // A hosts entry and a DNS rule for the same domain must NOT win.
        dns.hosts = HostsConfig {
            enabled: true,
            include_system: false,
            entries: vec![HostsEntry {
                id: "h".into(),
                enabled: true,
                domain: "google.com".into(),
                addr: "10.0.0.1".into(),
            }],
        };
        dns.rule_sets = Vec::new(); // effective_hosts falls back to flat hosts
        let analyzer = DnsPathAnalyzer::build(&input("singbox", sets, dns));
        let path = analyzer.decide("www.google.com");
        assert_eq!(path.strategy, DnsPathStrategy::Remote);
        assert!(path.matched_by.contains("海外"));
        assert!(path.matched_by.contains("后缀 google.com"));
        assert!(path.via_proxy);
        assert!(path.servers[0].contains(REMOTE_DNS_POOL[0]));
    }

    #[test]
    fn singbox_hosts_beats_dns_rules_and_fakeip() {
        let mut dns = DnsSettings::default();
        dns.fake_ip.enabled = false;
        dns.hosts = HostsConfig {
            enabled: true,
            include_system: false,
            entries: vec![HostsEntry {
                id: "h".into(),
                enabled: true,
                domain: "pinned.example".into(),
                addr: "10.0.0.9".into(),
            }],
        };
        dns.rule_sets = Vec::new();
        let analyzer = DnsPathAnalyzer::build(&input("singbox", vec![], dns));
        let path = analyzer.decide("pinned.example");
        assert_eq!(path.strategy, DnsPathStrategy::Hosts);
        assert!(path.servers[0].contains("10.0.0.9"));
    }

    #[test]
    fn singbox_block_set_rejects_but_mihomo_xray_keep_resolving() {
        let mut block_set = local_set(
            "ads",
            "广告",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "ads.example".into(),
                RuleTarget::Block,
                10,
            )],
            RuleSetDnsStrategy::Remote,
        );
        block_set.strategy = crate::domain::RuleSetStrategy::Block;
        let mut dns = DnsSettings::default();
        dns.fake_ip.enabled = false;

        let singbox =
            DnsPathAnalyzer::build(&input("singbox", vec![block_set.clone()], dns.clone()));
        assert_eq!(
            singbox.decide("ads.example").strategy,
            DnsPathStrategy::Block
        );

        // mihomo/Xray never emit a DNS reject for Block sets — traffic is
        // blocked at routing, DNS still resolves via the set's dns_strategy.
        let mihomo = DnsPathAnalyzer::build(&input("mihomo", vec![block_set.clone()], dns.clone()));
        assert_eq!(
            mihomo.decide("ads.example").strategy,
            DnsPathStrategy::Remote
        );

        let xray = DnsPathAnalyzer::build(&input("xray", vec![block_set], dns));
        assert_eq!(xray.decide("ads.example").strategy, DnsPathStrategy::Remote);
    }

    #[test]
    fn mihomo_keyword_falls_back_to_final_with_note() {
        let sets = vec![local_set(
            "kw",
            "关键词集",
            vec![Rule::new(
                RuleType::DomainKeyword,
                "google".into(),
                RuleTarget::Proxy,
                10,
            )],
            RuleSetDnsStrategy::Remote,
        )];
        let mut dns = DnsSettings::default();
        dns.fake_ip.enabled = false;
        dns.dns_final = "domestic".into();

        let mihomo = DnsPathAnalyzer::build(&input("mihomo", sets.clone(), dns.clone()));
        let path = mihomo.decide("google.com");
        assert_eq!(path.strategy, DnsPathStrategy::Domestic); // dns_final pool
        assert!(path.note.as_deref().unwrap_or_default().contains("mihomo"));
        assert!(path.note.as_deref().unwrap_or_default().contains("关键词"));

        // sing-box keywords work normally.
        let singbox = DnsPathAnalyzer::build(&input("singbox", sets, dns));
        let path = singbox.decide("google.com");
        assert_eq!(path.strategy, DnsPathStrategy::Remote);
        assert!(path.matched_by.contains("关键词集"));
    }

    #[test]
    fn fakeip_activation_matches_each_core() {
        let mut dns = DnsSettings::default();
        dns.fake_ip.enabled = false;
        dns.dns_final = "remote".into();

        // mihomo forces fake-ip under TUN even with the toggle off.
        let mut tun_input = input("mihomo", vec![], dns.clone());
        tun_input.tun_enabled = true;
        let path = DnsPathAnalyzer::build(&tun_input).decide("youtube.com");
        assert_eq!(path.strategy, DnsPathStrategy::FakeIp);

        // sing-box does not (fake_ip.enabled stays authoritative).
        let path =
            DnsPathAnalyzer::build(&input("singbox", vec![], dns.clone())).decide("youtube.com");
        assert_eq!(path.strategy, DnsPathStrategy::Remote);

        // sing-box with FakeIP on: bypass suffix falls back to local.
        let mut dns_bypass = dns.clone();
        dns_bypass.fake_ip.enabled = true;
        dns_bypass.fake_ip.bypass = vec!["lan".into()];
        let analyzer = DnsPathAnalyzer::build(&input("singbox", vec![], dns_bypass));
        assert_eq!(
            analyzer.decide("printer.lan").strategy,
            DnsPathStrategy::Local
        );
        assert_eq!(
            analyzer.decide("youtube.com").strategy,
            DnsPathStrategy::FakeIp
        );
    }

    #[test]
    fn xray_skips_user_remote_sets_but_singbox_uses_cache() {
        // Source-format cache under a temp remote-rule-sets dir.
        let dir = std::env::temp_dir().join(format!(
            "satelite-dnsdiag-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache_dir = dir.join("remote-rule-sets");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join("user-set.json");
        std::fs::write(
            &cache_file,
            serde_json::json!({
                "version": 3,
                "rules": [
                    { "domain_suffix": ["openai.com"] },
                    { "domain_keyword": ["huggingface"] }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let mut remote_set = local_set(
            "rs-user",
            "用户远程集",
            vec![],
            RuleSetDnsStrategy::Domestic,
        );
        remote_set.remote = Some(crate::domain::RemoteRuleSetConfig {
            url: "https://example.com/set.json".into(),
            format: "source".into(),
            update_interval: "disabled".into(),
            target: RuleTarget::Proxy,
            local_path: Some(cache_file.to_string_lossy().into_owned()),
            download_status: "ready".into(),
            download_error: None,
            last_update: None,
            last_attempt: None,
            rule_count: Some(2),
        });

        let mut dns = DnsSettings::default();
        dns.fake_ip.enabled = false;
        let mut singbox_input = input("singbox", vec![remote_set.clone()], dns.clone());
        singbox_input.data_dir = dir.clone();
        let analyzer = DnsPathAnalyzer::build(&singbox_input);
        let path = analyzer.decide("chat.openai.com");
        assert_eq!(path.strategy, DnsPathStrategy::Domestic);
        assert!(path.matched_by.contains("用户远程集"));
        assert!(path.matched_by.contains("后缀 openai.com"));
        // keyword items from remote caches classify too
        assert_eq!(
            analyzer.decide("hf.huggingface.co").strategy,
            DnsPathStrategy::Domestic
        );

        // Xray skips user remote sets entirely.
        let mut xray_input = input("xray", vec![remote_set], dns);
        xray_input.data_dir = dir.clone();
        let analyzer = DnsPathAnalyzer::build(&xray_input);
        assert_eq!(
            analyzer.decide("chat.openai.com").strategy,
            DnsPathStrategy::Remote
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_geosite_sets_approximate_under_xray_with_bypass_keyword() {
        // Same cache trick for a builtin geolocation-!cn set.
        let dir = std::env::temp_dir().join(format!(
            "satelite-dnsdiag-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache_dir = dir.join("remote-rule-sets");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join("system-geolocation-not-cn.srs");
        // The loader dispatches on `remote.format`; "source" JSON keeps this
        // test independent of the binary encoder (srs.rs covers that parser).
        std::fs::write(
            &cache_file,
            serde_json::json!({ "version": 3, "rules": [ { "domain_suffix": ["google.com"] } ] })
                .to_string(),
        )
        .unwrap();

        let mut builtin =
            RuleSet::new_remote("海外网站", "https://example.com/x.srs", RuleTarget::Proxy);
        builtin.id = "system-geolocation-not-cn".into();
        builtin.dns_strategy = RuleSetDnsStrategy::Remote;
        if let Some(remote) = builtin.remote.as_mut() {
            remote.format = "source".into();
            remote.local_path = Some(cache_file.to_string_lossy().into_owned());
        }

        let mut dns = DnsSettings::default();
        dns.fake_ip.enabled = false;
        let mut xray_input = input("xray", vec![builtin], dns);
        xray_input.data_dir = dir.clone();
        let analyzer = DnsPathAnalyzer::build(&xray_input);
        let path = analyzer.decide("google.com");
        assert_eq!(path.strategy, DnsPathStrategy::Remote);
        assert!(
            path.approx,
            "builtin sets are cache-approximated under Xray"
        );
        assert!(!analyzer.approx_set_names().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_domain_strips_url_decoration() {
        assert_eq!(
            clean_domain("https://www.google.com/search?q=1").as_deref(),
            Some("www.google.com")
        );
        assert_eq!(clean_domain("  x.com:443 ").as_deref(), Some("x.com"));
        assert_eq!(clean_domain("youtube.com").as_deref(), Some("youtube.com"));
        assert_eq!(
            clean_domain("https://a.com:8080/path").as_deref(),
            Some("a.com")
        );
        assert_eq!(clean_domain(""), None);
    }
}
