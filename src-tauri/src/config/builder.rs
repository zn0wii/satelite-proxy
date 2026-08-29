//! Build sing-box JSON from normalized [`ProxyNode`]s.

use crate::config::dns_build::{build_dns_section, build_hosts_route_rules};
use crate::config::punycode::to_ascii_domain;
use crate::domain::{
    AutoSelectMode, DnsSettings, ExtraInbound, OutboundMode, Protocol, ProtocolConfig, ProxyNode,
    Rule, RuleSet, RuleSetStrategy, RuleTarget, RuleType, TlsConfig, Transport,
};
use crate::error::{AppError, AppResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Loopback-only mixed inbound through which `diagnose_chain` fetches echo
/// services with a per-chain selector (see the "Diagnostics egress" comment
/// in `build_singbox_config`). Uncommon port; a conflict fails core startup
/// with a port-in-use error naming it.
pub const DIAG_INBOUND_PORT: u16 = 26486;
pub const DIAG_INBOUND_TAG: &str = "diag-in";
pub const DIAG_SELECTOR_TAG: &str = "chain-diag";

#[derive(Clone)]
pub struct BuildOptions {
    pub mixed_port: u16,
    /// Main mixed inbound listens on 0.0.0.0 (LAN) instead of 127.0.0.1.
    pub allow_lan: bool,
    pub api_port: u16,
    /// Additional mixed/http listeners (settings-managed).
    pub extra_inbounds: Vec<ExtraInbound>,
    pub api_secret: String,
    /// Preferred node id; falls back to first node.
    pub current_node_id: Option<String>,
    pub log_level: String,
    pub rules: Vec<Rule>,
    /// Enabled unified sets in match-priority order.
    pub rule_sets: Vec<RuleSet>,
    /// Reusable node pools, referenced by chain hops (and, in the future,
    /// directly by rules). Always passed in full — pools don't have an
    /// enabled/disabled flag, unlike rule sets.
    pub pools: Vec<crate::domain::NodePool>,
    /// Named multi-hop chains, built into sing-box `detour` chains.
    pub chains: Vec<crate::domain::ProxyChain>,
    /// Enable TUN inbound (global capture).
    pub tun_enabled: bool,
    /// system | gvisor | mixed
    pub tun_stack: String,
    /// DNS module settings (always applied).
    pub dns: DnsSettings,
    /// Rule / Global / Direct.
    pub outbound_mode: OutboundMode,
    /// `route.final` in Rule mode: proxy | direct | block.
    pub route_final: String,
    /// off/smart → selector; kernel → urltest.
    pub auto_select: AutoSelectMode,
    /// URL for kernel urltest (and shared probe default).
    pub probe_url: String,
    /// Resolve the originating process per connection (sing-box
    /// `find_process_mode`: on → always, off → off).
    pub find_process: bool,
    /// Include an IPv6 address on the TUN interface. Off by default: most
    /// budget VPS nodes have no IPv6 egress, and an IPv6-addressed tun makes
    /// the OS treat the machine as dual-stack — apps (notably Chrome) then
    /// prefer AAAA/v6 and black-hole against a node with no v6 route out.
    pub tun_ipv6: bool,
    /// Reject sniffed QUIC (UDP/443) traffic so browsers fall back to TCP.
    /// See `AppSettings::block_quic` doc for the congestion-control rationale.
    pub block_quic: bool,
    /// Bypass localhost and LAN segments with built-in direct rules appended
    /// after the rule sets (a safety net ahead of `route.final`). Rule mode
    /// only; Global proxies everything by explicit user choice.
    pub bypass_lan: bool,
    /// macOS-only, Xray-only: the `utunN` device name to bind the TUN
    /// inbound to. Xray's darwin backend rejects any name that does not
    /// parse as `utun<digits>` (unlike sing-box, which lets the OS assign
    /// one), so the caller must probe a free index before building the
    /// config. `None` on other platforms/cores, where Xray accepts an
    /// arbitrary interface name.
    pub tun_interface_name: Option<String>,
}

impl BuildOptions {
    pub fn normalized_tun_stack(&self) -> &str {
        match self.tun_stack.to_ascii_lowercase().as_str() {
            "system" => "system",
            "gvisor" => "gvisor",
            _ => "mixed",
        }
    }

    pub fn normalized_route_final(&self) -> &str {
        match self.route_final.to_ascii_lowercase().as_str() {
            "direct" => "direct",
            "block" => "block",
            _ => "proxy",
        }
    }
}

#[derive(Debug)]
pub struct BuiltConfig {
    pub value: Value,
    pub outbound_tags: Vec<String>,
    pub selected_tag: String,
}

/// Convert nodes into a complete sing-box config document.
pub fn build_singbox_config(nodes: &[ProxyNode], opts: &BuildOptions) -> AppResult<BuiltConfig> {
    if nodes.is_empty() {
        return Err(AppError::Config(
            "no nodes available; import a subscription first".into(),
        ));
    }

    // Last-line defense: stored nodes normally keep unique ids (import and
    // store-load both normalize), but any surviving collision would emit two
    // outbounds with the same `node-<id[..16]>` tag and sing-box rejects the
    // whole config. Work on a normalized copy so every tag reference inside
    // this build (selector members, route pins, smart pools) stays consistent.
    let mut owned_nodes = nodes.to_vec();
    let renamed = ProxyNode::ensure_unique_ids(owned_nodes.iter_mut());
    if renamed > 0 {
        crate::app_log::warn(
            "singbox_config",
            format!("{renamed} 个节点 id 重复，已在生成时改写 tag 以避免 sing-box 校验失败"),
        );
    }
    let nodes: &[ProxyNode] = &owned_nodes;

    let mut node_outbounds = Vec::new();
    let mut node_endpoints = Vec::new();
    let mut tags = Vec::new();
    let mut errors = Vec::new();

    for node in nodes {
        match node_to_outbound(node) {
            Ok((tag, outbound, extra_outbounds)) => {
                tags.push(tag);
                // Detour outbounds (e.g. shadowtls) aren't user-selectable
                // nodes, so they're appended to the outbounds list but kept
                // out of `tags`. They must precede the outbound that
                // references them via `detour`.
                node_outbounds.extend(extra_outbounds);
                if matches!(node.protocol, Protocol::WireGuard) {
                    node_endpoints.push(outbound);
                } else {
                    node_outbounds.push(outbound);
                }
            }
            Err(e) => errors.push(format!("{}: {e}", node.name)),
        }
    }

    if node_outbounds.is_empty() && node_endpoints.is_empty() {
        return Err(AppError::Config(format!(
            "failed to map any node to outbound: {}",
            errors.join("; ")
        )));
    }

    let selected_tag = resolve_selected_tag(nodes, &tags, opts.current_node_id.as_deref());
    let effective_rules = effective_route_rules(&opts.rule_sets, &opts.rules);

    let mut outbounds = Vec::new();
    // Main group: selector (manual / app smart) vs urltest (kernel auto).
    if opts.auto_select.is_kernel() {
        let url = if opts.probe_url.trim().is_empty() {
            "https://www.gstatic.com/generate_204".to_string()
        } else {
            opts.probe_url.trim().to_string()
        };
        // urltest only lists real nodes (never "direct" — would win on latency).
        outbounds.push(json!({
            "type": "urltest",
            "tag": "proxy",
            "outbounds": tags.clone(),
            "url": url,
            "interval": "1m",
            "tolerance": 50,
            "idle_timeout": "30m",
            "interrupt_exist_connections": false,
        }));
    } else {
        let mut selector_outbounds = tags.clone();
        selector_outbounds.push("direct".into());
        outbounds.push(json!({
            "type": "selector",
            "tag": "proxy",
            "outbounds": selector_outbounds,
            "default": selected_tag,
        }));
    }
    // Per-rule smart selectors (keyword-filtered node pools).
    outbounds.extend(build_smart_rule_selectors(&effective_rules, nodes, &tags));
    // Whole-set keyword pools for Filter-strategy sets (local + remote).
    outbounds.extend(build_filter_set_selectors(&opts.rule_sets, nodes, &tags));
    // Named node pools (Proxy Chain feature) — selectors must precede the
    // chain outbounds below, which detour into them by tag.
    outbounds.extend(build_pool_selectors(&opts.pools, nodes, &tags));
    // Named multi-hop chains: per-hop outbounds wired together via `detour`.
    let (chain_outbounds, chain_entry_tags) =
        build_chain_outbounds(&opts.chains, &opts.pools, nodes, &tags);
    outbounds.extend(chain_outbounds);
    // Diagnostics egress (Proxy Chain feature): a loopback-only mixed inbound
    // plus an app-owned selector over every chain's exit tag. `diagnose_chain`
    // switches this selector through the Clash API and fetches an echo
    // service via the inbound — real exit-IP verification with no user rule
    // and no core restart. Emitted only when at least one chain resolves.
    let diag_members: Vec<String> = opts
        .chains
        .iter()
        .filter_map(|c| chain_entry_tags.get(&c.id).cloned())
        .collect();
    let has_diag = !diag_members.is_empty();
    if has_diag {
        let mut members = diag_members.clone();
        members.push("direct".into());
        let default = members[0].clone();
        outbounds.push(json!({
            "type": "selector",
            "tag": DIAG_SELECTOR_TAG,
            "outbounds": members,
            "default": default,
        }));
    }
    outbounds.extend(node_outbounds);
    outbounds.push(json!({ "type": "direct", "tag": "direct" }));
    outbounds.push(json!({ "type": "block", "tag": "block" }));

    // Clash-style modes:
    // - Rule: user rules + configurable final (proxy|direct|block)
    // - Global: no user rules, final proxy
    // - Direct: no user rules, final direct
    let (apply_user_rules, route_final) = match opts.outbound_mode {
        OutboundMode::Rule => (true, opts.normalized_route_final()),
        OutboundMode::Global => (false, "proxy"),
        OutboundMode::Direct => (false, "direct"),
    };

    // DNS `final` is configured independently on the DNS page (local/domestic/
    // remote) and no longer follows the routing `final`.
    let mut built_dns = build_dns_section(&opts.dns, opts.tun_enabled, &effective_rules);
    let (rule_set_defs, grouped_route_rules, grouped_dns_rules) =
        build_grouped_rule_sets(&opts.rule_sets, nodes, &tags, &chain_entry_tags);
    if let Some(dns_rules) = built_dns.dns.get_mut("rules").and_then(Value::as_array_mut) {
        for rule in grouped_dns_rules.into_iter().rev() {
            dns_rules.insert(0, rule);
        }
    }

    let mut route_rules = Vec::new();
    // Sniff helps domain-based route / DNS on mixed + TUN
    route_rules.push(json!({ "action": "sniff" }));
    if has_diag {
        // Diagnostics traffic wins over every user rule: the diag inbound is
        // app-owned and its whole purpose is per-selector egress, regardless
        // of what the user's rules would do with the echo-service domain.
        route_rules.push(json!({
            "inbound": [DIAG_INBOUND_TAG],
            "outbound": DIAG_SELECTOR_TAG
        }));
    }
    if opts.block_quic {
        // QUIC relayed through XUDP-in-TCP carries two independent congestion
        // controllers (inner QUIC, outer TCP); on a mediocre link that fights
        // itself and stutters video. Rejecting sniffed QUIC makes browsers
        // fall back to TCP-based HTTP, which the proxy handles natively.
        // Must run after sniff (needs the detected protocol) and is safe
        // ahead of the DNS hijack rule below since DNS is a distinct protocol.
        route_rules.push(json!({ "protocol": "quic", "action": "reject" }));
    }
    if built_dns.want_hijack || opts.tun_enabled {
        route_rules.push(json!({ "protocol": "dns", "action": "hijack-dns" }));
    }
    // Hosts must also apply to mixed/system-proxy connections, which can pass a
    // domain directly to the outbound without performing a DNS query.
    route_rules.extend(build_hosts_route_rules(&opts.dns.effective_hosts()));
    if apply_user_rules {
        if opts.rule_sets.is_empty() {
            route_rules.extend(build_route_rules(
                &opts.rules,
                nodes,
                &tags,
                &chain_entry_tags,
            ));
        } else {
            route_rules.extend(grouped_route_rules);
        }
        if opts.bypass_lan {
            // localhost + LAN safety net before route.final. Emitted after the
            // rule sets so explicit user rules keep winning, and domain
            // classification (geosite sets) stays ahead of IP matching.
            // Pre-1.12 kernels resolved FQDNs for bare IP rules (breaking the
            // DNS split); 1.12+ only resolve via an explicit `resolve` action
            // (which we never emit) — IP rules now simply don't match domain
            // destinations, so domain rules must classify first or proxied
            // traffic falls through to the IP/final tier unresolved.
            // Verified empirically on 1.13.15.
            route_rules.push(json!({
                "domain_suffix": ["local", "localhost"],
                "action": "route",
                "outbound": "direct"
            }));
            // ip_is_private covers loopback, RFC1918, link-local and their
            // IPv6 counterparts in one matcher.
            route_rules.push(json!({
                "ip_is_private": true,
                "action": "route",
                "outbound": "direct"
            }));
        }
    }

    let mut inbounds = vec![json!({
        "type": "mixed",
        "tag": "mixed-in",
        "listen": if opts.allow_lan { "0.0.0.0" } else { "127.0.0.1" },
        "listen_port": opts.mixed_port
    })];

    for inb in &opts.extra_inbounds {
        inbounds.push(json!({
            "type": inb.kind,
            "tag": format!("in-{}-{}", inb.kind, inb.port),
            "listen": if inb.allow_lan { "0.0.0.0" } else { "127.0.0.1" },
            "listen_port": inb.port
        }));
    }

    if has_diag {
        // Loopback-only egress for chain diagnostics (see the selector note
        // above) — never exposed to the LAN.
        inbounds.push(json!({
            "type": "mixed",
            "tag": DIAG_INBOUND_TAG,
            "listen": "127.0.0.1",
            "listen_port": DIAG_INBOUND_PORT
        }));
    }

    if opts.tun_enabled {
        // strict_route drops packets that don't match auto_route's rules —
        // on macOS this used to be Windows-only over concern it could block
        // host → 127.0.0.1 (clash_api / mixed) while TUN is up. In practice
        // `route_exclude_address` below already carves out the loopback
        // range, so that conflict doesn't materialize; leaving strict_route
        // off on macOS/Linux instead lets traffic silently bypass the tunnel
        // (e.g. via a same-subnet route to the LAN gateway — see the DNS
        // pollution writeup). Enable it on every platform.
        inbounds.push(json!({
            "type": "tun",
            "tag": "tun-in",
            "address": tun_addresses(opts.tun_ipv6),
            "mtu": 9000,
            "auto_route": true,
            "strict_route": true,
            "route_exclude_address": ["127.0.0.0/8", "::1/128"],
            "stack": opts.normalized_tun_stack()
        }));
    }

    let mut value = json!({
        "log": {
            "level": opts.log_level,
            "timestamp": true
        },
        "dns": built_dns.dns,
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": {
            "rule_set": rule_set_defs,
            "rules": route_rules,
            "final": route_final,
            "auto_detect_interface": true,
            "default_domain_resolver": built_dns.default_resolver,
            // Resolve the originating process for each connection so the
            // Clash API connections list (and our traffic page) shows a real
            // process name. 1.13 uses route.find_process (bool).
            "find_process": opts.find_process
        },
        "experimental": {
            "clash_api": {
                "external_controller": format!("127.0.0.1:{}", opts.api_port),
                "secret": opts.api_secret,
                "default_mode": opts.outbound_mode.as_str()
            },
            // Persist the fakeip mapping table across core restarts. Without
            // this, every restart resets 198.18.x.x ⇄ domain mappings to
            // empty, and OS/app-level caches of the old fakeip briefly point
            // nowhere ("works after a moment"). `cache.db` is a relative path
            // — the core's cwd is anchored to the config directory (always
            // writable) by `CoreManager::start_with_ports`.
            "cache_file": {
                "enabled": true,
                "path": "cache.db",
                "store_fakeip": true
            }
        }
    });
    if !node_endpoints.is_empty() {
        value["endpoints"] = json!(node_endpoints);
    }

    // Post-build sanity scan: sing-box requires outbound + endpoint tags to
    // be globally unique. Collisions should be impossible after the entry
    // normalization; if one ever slips through (e.g. a non-node group tag),
    // leave a precise trace — the core's `check -c` still rejects it with
    // the authoritative error.
    if let Some(outbounds) = value.get("outbounds").and_then(Value::as_array) {
        let endpoint_tags = value
            .get("endpoints")
            .and_then(Value::as_array)
            .map(|eps| {
                eps.iter()
                    .filter_map(|e| e.get("tag").and_then(Value::as_str))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let dups: Vec<&str> = outbounds
            .iter()
            .filter_map(|o| o.get("tag").and_then(Value::as_str))
            .chain(endpoint_tags.iter().copied())
            .filter(|t| !seen.insert(t.to_string()))
            .collect();
        if !dups.is_empty() {
            crate::app_log::error(
                "singbox_config",
                format!(
                    "生成结果存在重复 tag（将由内核校验拦截）: {}",
                    dups.join(", ")
                ),
            );
        }
    }

    Ok(BuiltConfig {
        value,
        outbound_tags: tags,
        selected_tag,
    })
}

/// TUN interface addresses. IPv4 is always present; IPv6 is opt-in (see
/// `BuildOptions::tun_ipv6` doc) since most nodes have no v6 egress and an
/// IPv6-addressed tun makes the OS (and Chrome specifically) prefer AAAA/v6,
/// black-holing every connection.
fn tun_addresses(ipv6: bool) -> Vec<&'static str> {
    let mut addrs = vec!["172.19.0.1/30"];
    if ipv6 {
        addrs.push("fdfe:dcba:9876::1/126");
    }
    addrs
}

pub(crate) fn effective_route_rules(sets: &[RuleSet], fallback: &[Rule]) -> Vec<Rule> {
    if sets.is_empty() {
        return fallback.to_vec();
    }
    let mut out = Vec::new();
    let mut global_ord = 10;
    for set in sets
        .iter()
        .filter(|set| set.enabled && set.remote.is_none())
    {
        // Filter sets route through one whole-set selector; letting their
        // rules through here would spawn per-rule selectors that nothing
        // references (dead outbounds).
        if set.strategy == RuleSetStrategy::Filter {
            continue;
        }
        let mut rules = set.rules.clone();
        rules.sort_by_key(|rule| rule.ord);
        for mut rule in rules {
            if let Some(target) = set.strategy.route_target() {
                rule.target = target;
                rule.node_id = None;
                rule.node_name = None;
                rule.smart_include.clear();
                rule.smart_exclude.clear();
            } else {
                clamp_rule_pin_to_set(set, &mut rule);
            }
            rule.ord = global_ord;
            global_ord += 10;
            out.push(rule);
        }
    }
    out
}

/// Clamp a rule's node/smart pin to the whole-set strategy. Plain sets
/// (proxy/direct/block) collapse pins to the strategy target; Node sets pin
/// to the set-level node; Filter sets rewrite the keywords to the set-level
/// filters. Smart (Mixed) sets keep per-rule decisions untouched.
pub(crate) fn clamp_rule_pin_to_set(set: &RuleSet, rule: &mut Rule) {
    if set.strategy == RuleSetStrategy::Smart
        || matches!(
            rule.target,
            RuleTarget::Proxy | RuleTarget::Direct | RuleTarget::Block
        )
    {
        return;
    }
    match set.strategy {
        RuleSetStrategy::Direct => rule.target = RuleTarget::Direct,
        RuleSetStrategy::Block => rule.target = RuleTarget::Block,
        RuleSetStrategy::Node => {
            // Set-level pin is authoritative; a stale id falls back to the
            // main `proxy` group inside resolve_rule_outbound.
            rule.target = RuleTarget::Node;
            rule.node_id = set.node_id.clone();
            rule.node_name = set.node_name.clone();
        }
        RuleSetStrategy::Filter => {
            rule.target = RuleTarget::Smart;
            rule.smart_include = set.smart_include.clone();
            rule.smart_exclude = set.smart_exclude.clone();
        }
        _ => rule.target = RuleTarget::Proxy,
    }
}

/// Register every enabled logical set as a sing-box rule-set, then reference
/// its tag once from route and once from DNS. Smart route sets are the only
/// exception: their per-item destinations are partitioned into internal child
/// rule-sets, while DNS still references the single logical parent tag.
fn build_grouped_rule_sets(
    sets: &[RuleSet],
    nodes: &[ProxyNode],
    tags: &[String],
    chain_entry_tags: &std::collections::HashMap<String, String>,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let mut definitions = Vec::new();
    let mut route_rules = Vec::new();
    let mut dns_rules = Vec::new();

    for set in sets.iter().filter(|set| set.enabled) {
        if let Some(remote) = &set.remote {
            let Some(path) = remote
                .local_path
                .as_deref()
                .map(str::trim)
                .filter(|path| !path.is_empty())
            else {
                continue;
            };
            if !std::path::Path::new(path).is_file() {
                continue;
            }
            definitions.push(json!({
                "tag": set.id,
                "type": "local",
                "format": remote.format,
                "path": path,
            }));
        } else {
            // sing-box rejects an inline rule-set whose body is empty, so an
            // enabled-but-empty set is dropped together with every route/DNS
            // rule that would reference it.
            let Some(headless) = build_headless_rules(&set.rules) else {
                continue;
            };
            definitions.push(json!({
                "type": "inline",
                "tag": set.id,
                "rules": headless,
            }));
        }

        // Local sets route per rule: each rule's own target wins. Under a
        // plain strategy the per-rule choice is proxy/direct/block only —
        // node/smart pins (e.g. left over from an earlier smart phase) are
        // clamped to the set strategy, Node/Filter sets clamp them to the
        // set-level pin / keyword pool. `set_rule_set_strategy` and the batch
        // path retarget all rules on flip, so a plain set stays uniform unless
        // the user deliberately mixes per-rule routes. Remote sets have no
        // local rules and keep their single set-level route.
        if set.remote.is_none() {
            route_local_set_grouped(
                set,
                nodes,
                tags,
                chain_entry_tags,
                &mut definitions,
                &mut route_rules,
            );
        } else {
            route_rules.push(remote_set_route_rule(set, nodes, tags, chain_entry_tags));
        }

        if set.strategy == RuleSetStrategy::Block {
            dns_rules.push(json!({ "rule_set": [set.id], "action": "reject" }));
        } else {
            dns_rules.push(json!({
                "rule_set": [set.id],
                "action": "route",
                "server": set.dns_strategy.server_tag(),
            }));
        }
    }

    (definitions, route_rules, dns_rules)
}

/// Whole-set route rule for a remote set. Node pins and Filter pools fall
/// back to the main `proxy` group when the pin is stale / the keyword pool is
/// empty (no selector is emitted in that case either — see
/// `build_filter_set_selectors`).
fn remote_set_route_rule(
    set: &RuleSet,
    nodes: &[ProxyNode],
    tags: &[String],
    chain_entry_tags: &std::collections::HashMap<String, String>,
) -> Value {
    if set.strategy == RuleSetStrategy::Block {
        return json!({ "rule_set": [set.id], "action": "reject" });
    }
    let outbound = match set.strategy {
        RuleSetStrategy::Direct => "direct".to_string(),
        RuleSetStrategy::Node => node_pin_outbound(set.node_id.as_deref(), nodes, tags),
        RuleSetStrategy::Filter => {
            let pool = filter_pool_tags(&set.smart_include, &set.smart_exclude, nodes, tags);
            if pool.is_empty() {
                "proxy".to_string()
            } else {
                set.smart_set_outbound_tag()
            }
        }
        RuleSetStrategy::Chain => set
            .chain_id
            .as_deref()
            .and_then(|id| chain_entry_tags.get(id))
            .cloned()
            .unwrap_or_else(|| "proxy".to_string()),
        _ => "proxy".to_string(),
    };
    json!({ "rule_set": [set.id], "action": "route", "outbound": outbound })
}

/// Outbound tag of a pinned node, or the main `proxy` group when the id is
/// missing / stale / not part of the generated outbounds.
fn node_pin_outbound(node_id: Option<&str>, nodes: &[ProxyNode], tags: &[String]) -> String {
    if let Some(id) = node_id.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(node) = nodes.iter().find(|n| n.id == id) {
            let tag = outbound_tag(node);
            if tags.iter().any(|t| t == &tag) {
                return tag;
            }
        }
    }
    "proxy".into()
}

/// Whole-set selectors for Filter-strategy sets (local + remote): one
/// keyword-filtered pool per set, tagged like a per-rule smart selector
/// (`smart-<id prefix>` — set ids never collide with rule hash ids) so
/// smart_switch can probe/switch it through a stand-in rule with the same id.
fn build_filter_set_selectors(
    sets: &[RuleSet],
    nodes: &[ProxyNode],
    tags: &[String],
) -> Vec<Value> {
    let mut out = Vec::new();
    for set in sets
        .iter()
        .filter(|set| set.enabled && set.strategy == RuleSetStrategy::Filter)
    {
        // Sets that contribute nothing to the config never reach a route
        // rule; a selector for them would be a dead outbound.
        if rule_set_is_empty_for_config(set) {
            continue;
        }
        let pool = filter_pool_tags(&set.smart_include, &set.smart_exclude, nodes, tags);
        if pool.is_empty() {
            continue;
        }
        let default = pool.first().cloned().unwrap_or_else(|| "direct".into());
        out.push(json!({
            "type": "selector",
            "tag": set.smart_set_outbound_tag(),
            "outbounds": pool,
            "default": default,
        }));
    }
    out
}

/// Per-rule routing for a local set: group effective rules by resolved
/// outbound and emit one child inline rule-set per group (the parent set
/// definition stays registered for DNS). Shared by every local strategy —
/// Smart honors node/smart targets; plain strategies clamp them; Node/Filter
/// sets clamp to the set-level pin / keyword pool.
fn route_local_set_grouped(
    set: &RuleSet,
    nodes: &[ProxyNode],
    tags: &[String],
    chain_entry_tags: &std::collections::HashMap<String, String>,
    definitions: &mut Vec<Value>,
    route_rules: &mut Vec<Value>,
) {
    // Filter sets route every smart-pool rule through one whole-set selector
    // (falling back to `proxy` on an empty pool) instead of per-rule tags.
    let filter_key = if set.strategy == RuleSetStrategy::Filter {
        let pool = filter_pool_tags(&set.smart_include, &set.smart_exclude, nodes, tags);
        format!(
            "route:{}",
            if pool.is_empty() {
                "proxy".to_string()
            } else {
                set.smart_set_outbound_tag()
            }
        )
    } else {
        String::new()
    };
    let mut groups: Vec<(String, Vec<Rule>)> = Vec::new();
    let mut sorted: Vec<Rule> = set
        .rules
        .iter()
        .filter(|rule| inline_rule_is_effective(rule))
        .cloned()
        .collect();
    sorted.sort_by_key(|rule| rule.ord);
    for mut rule in sorted {
        clamp_rule_pin_to_set(set, &mut rule);
        let key = if rule.target == RuleTarget::Block {
            "reject".to_string()
        } else if set.strategy == RuleSetStrategy::Filter && rule.target == RuleTarget::Smart {
            filter_key.clone()
        } else {
            format!(
                "route:{}",
                resolve_rule_outbound(&rule, nodes, tags, chain_entry_tags)
            )
        };
        if let Some((_, rules)) = groups.iter_mut().find(|(group, _)| group == &key) {
            rules.push(rule);
        } else {
            groups.push((key, vec![rule]));
        }
    }
    // A uniform set keeps its classic shape: the parent definition itself is
    // referenced by route (and DNS) — no child sets. Only genuinely mixed
    // per-rule routing pays for child rule-sets.
    if groups.len() == 1 {
        let (key, _) = groups.into_iter().next().unwrap();
        if key == "reject" {
            route_rules.push(json!({ "rule_set": [set.id.clone()], "action": "reject" }));
        } else {
            route_rules.push(json!({
                "rule_set": [set.id.clone()],
                "action": "route",
                "outbound": key.trim_start_matches("route:"),
            }));
        }
        return;
    }
    for (index, (key, rules)) in groups.into_iter().enumerate() {
        let tag = format!("{}-route-{index}", set.id);
        let Some(headless) = build_headless_rules(&rules) else {
            continue;
        };
        definitions.push(json!({
            "type": "inline",
            "tag": tag,
            "rules": headless,
        }));
        if key == "reject" {
            route_rules.push(json!({ "rule_set": [tag], "action": "reject" }));
        } else {
            route_rules.push(json!({
                "rule_set": [tag],
                "action": "route",
                "outbound": key.trim_start_matches("route:"),
            }));
        }
    }
}

/// Whether a set contributes nothing to the generated sing-box config: no
/// matchable local rules, or a remote set whose cache file is missing. Must
/// stay in sync with what `build_grouped_rule_sets` registers — callers use
/// this to skip core restarts for edits that cannot change the config.
pub fn rule_set_is_empty_for_config(set: &RuleSet) -> bool {
    match &set.remote {
        Some(remote) => match remote
            .local_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            Some(path) => !std::path::Path::new(path).is_file(),
            None => true,
        },
        None => build_headless_rules(&set.rules).is_none(),
    }
}

/// Whether a rule contributes at least one matchable payload to an inline
/// rule-set (enabled, non-empty payload, non-deprecated type).
fn inline_rule_is_effective(rule: &Rule) -> bool {
    rule.enabled && !rule.payload.trim().is_empty() && rule.rule_type != RuleType::Geoip
}

/// Headless rule bodies for one inline rule-set. `None` when nothing is
/// matchable — sing-box rejects an empty rule body ("missing condition"), so
/// the caller must drop the definition and everything referencing it.
fn build_headless_rules(rules: &[Rule]) -> Option<Vec<Value>> {
    let mut buckets: [Vec<String>; 5] = Default::default();
    for rule in rules.iter().filter(|rule| inline_rule_is_effective(rule)) {
        let payload = rule.payload.trim();
        let index = match rule.rule_type {
            RuleType::Domain => 0,
            RuleType::DomainSuffix => 1,
            RuleType::DomainKeyword => 2,
            RuleType::IpCidr => 3,
            RuleType::Process => 4,
            RuleType::Geoip => continue,
        };
        let normalized = match rule.rule_type {
            RuleType::Domain | RuleType::DomainSuffix => payload.trim_start_matches(['*', '.']),
            _ => payload,
        };
        let value = match rule.rule_type {
            // sing-box matches wire-format QNAME/SNI, which is always ASCII.
            // domain_keyword is a substring match — Punycode-encoding it
            // would break that semantic, so it's left as-is.
            RuleType::Domain | RuleType::DomainSuffix => to_ascii_domain(normalized),
            _ => normalized.to_string(),
        };
        // Payloads like "*" or "." normalize to nothing and would produce an
        // empty condition entry, which the kernel also rejects.
        if value.is_empty() {
            continue;
        }
        buckets[index].push(value);
    }
    let keys = [
        "domain",
        "domain_suffix",
        "domain_keyword",
        "ip_cidr",
        "process_name",
    ];
    let headless: Vec<Value> = keys
        .iter()
        .zip(buckets)
        .filter_map(|(key, values)| (!values.is_empty()).then(|| json!({ (*key): values })))
        .collect();
    (!headless.is_empty()).then_some(headless)
}

pub(crate) fn resolve_selected_tag(
    nodes: &[ProxyNode],
    tags: &[String],
    current_id: Option<&str>,
) -> String {
    if let Some(id) = current_id {
        if let Some(node) = nodes.iter().find(|n| n.id == id) {
            let tag = outbound_tag(node);
            if tags.iter().any(|t| t == &tag) {
                return tag;
            }
        }
    }
    tags.first().cloned().unwrap_or_else(|| "direct".into())
}

pub fn outbound_tag(node: &ProxyNode) -> String {
    format!("node-{}", &node.id[..node.id.len().min(16)])
}

fn build_route_rules(
    rules: &[Rule],
    nodes: &[ProxyNode],
    tags: &[String],
    chain_entry_tags: &std::collections::HashMap<String, String>,
) -> Vec<Value> {
    let mut sorted: Vec<&Rule> = rules.iter().filter(|r| r.enabled).collect();
    sorted.sort_by_key(|r| r.ord);

    sorted
        .into_iter()
        .filter_map(|r| {
            let payload = r.payload.trim();
            if payload.is_empty() {
                return None;
            }
            // sing-box 1.8+ deprecated / 1.12+ removed inline `geoip` — skip
            if matches!(r.rule_type, RuleType::Geoip) {
                return None;
            }
            let outbound = resolve_rule_outbound(r, nodes, tags, chain_entry_tags);
            // sing-box matches wire-format QNAME/SNI, which is always ASCII.
            // domain_keyword is a substring match — Punycode-encoding it
            // would break that semantic, so it's left as-is.
            let mut rule = match r.rule_type {
                RuleType::Domain => json!({ "domain": [to_ascii_domain(payload)] }),
                RuleType::DomainSuffix => json!({ "domain_suffix": [to_ascii_domain(payload)] }),
                RuleType::DomainKeyword => json!({ "domain_keyword": [payload] }),
                RuleType::IpCidr => json!({ "ip_cidr": [payload] }),
                RuleType::Process => json!({ "process_name": [payload] }),
                RuleType::Geoip => return None,
            };
            if r.target == RuleTarget::Block {
                rule.as_object_mut()?
                    .insert("action".into(), json!("reject"));
            } else {
                rule.as_object_mut()?
                    .insert("action".into(), json!("route"));
                rule.as_object_mut()?
                    .insert("outbound".into(), json!(outbound));
            }
            Some(rule)
        })
        .collect()
}

/// Map a rule to an outbound tag. Pinned node missing → fall back to main `proxy` selector.
fn resolve_rule_outbound(
    r: &Rule,
    nodes: &[ProxyNode],
    tags: &[String],
    chain_entry_tags: &std::collections::HashMap<String, String>,
) -> String {
    use crate::domain::RuleTarget;
    match r.target {
        RuleTarget::Direct | RuleTarget::Proxy | RuleTarget::Block => {
            r.target.outbound_tag().into()
        }
        // Stale pins (subscription updated / node removed / sub disabled) fall
        // back to the main `proxy` group inside node_pin_outbound.
        RuleTarget::Node => node_pin_outbound(r.node_id.as_deref(), nodes, tags),
        RuleTarget::Smart => {
            let pool = smart_pool_tags(r, nodes, tags);
            if pool.is_empty() {
                RuleTarget::Proxy.outbound_tag().into()
            } else {
                r.smart_outbound_tag()
            }
        }
        RuleTarget::Chain => r
            .chain_id
            .as_deref()
            .and_then(|id| chain_entry_tags.get(id))
            .cloned()
            .unwrap_or_else(|| RuleTarget::Proxy.outbound_tag().into()),
    }
}

/// Node outbound tags matching a smart rule's include/exclude name filters.
pub fn smart_pool_tags(r: &Rule, nodes: &[ProxyNode], tags: &[String]) -> Vec<String> {
    filter_pool_tags(&r.smart_include, &r.smart_exclude, nodes, tags)
}

/// Node outbound tags matching include/exclude keyword filters, preferring
/// historically better latency as the selector default.
pub fn filter_pool_tags(
    include: &[String],
    exclude: &[String],
    nodes: &[ProxyNode],
    tags: &[String],
) -> Vec<String> {
    let mut pool: Vec<(u32, String)> = nodes
        .iter()
        .filter(|n| crate::domain::name_matches_keywords(&n.name, include, exclude))
        .filter_map(|n| {
            let tag = outbound_tag(n);
            if tags.iter().any(|t| t == &tag) {
                Some((n.latency_ms.unwrap_or(u32::MAX / 4), tag))
            } else {
                None
            }
        })
        .collect();
    pool.sort_by_key(|(lat, _)| *lat);
    pool.into_iter().map(|(_, tag)| tag).collect()
}

/// Member outbound tags for one [`NodePool`], resolved against the current
/// node list. `Explicit` keeps its configured order; `Keyword` re-filters
/// every build (same semantics as `filter_pool_tags`, latency-sorted).
fn pool_member_tags(
    pool: &crate::domain::NodePool,
    nodes: &[ProxyNode],
    tags: &[String],
) -> Vec<String> {
    use crate::domain::PoolMode;
    match &pool.mode {
        PoolMode::Explicit { node_ids } => node_ids
            .iter()
            .filter_map(|nid| nodes.iter().find(|n| &n.id == nid))
            .map(outbound_tag)
            .filter(|tag| tags.iter().any(|t| t == tag))
            .collect(),
        PoolMode::Keyword { include, exclude } => filter_pool_tags(include, exclude, nodes, tags),
    }
}

/// One `selector` outbound per [`NodePool`]. Pools with no live members are
/// skipped (a selector with an empty `outbounds` list is invalid sing-box
/// config) — chain hops referencing an empty pool fall back to `direct` at
/// resolution time, mirroring the empty-Smart-pool fallback.
fn build_pool_selectors(
    pools: &[crate::domain::NodePool],
    nodes: &[ProxyNode],
    tags: &[String],
) -> Vec<Value> {
    let mut out = Vec::new();
    for pool in pools {
        let members = pool_member_tags(pool, nodes, tags);
        if members.is_empty() {
            continue;
        }
        let default = members.first().cloned().unwrap_or_else(|| "direct".into());
        out.push(json!({
            "type": "selector",
            "tag": pool.outbound_tag(),
            "outbounds": members,
            "default": default,
        }));
    }
    out
}

/// Member nodes for one [`NodePool`] (live in `tags`), mirroring
/// [`pool_member_tags`]'s ordering — `Explicit` keeps its configured order,
/// `Keyword` is latency-sorted. Used where the chain builder needs the nodes
/// themselves (to mint per-member clones), not just their tags.
fn pool_member_nodes<'a>(
    pool: &crate::domain::NodePool,
    nodes: &'a [ProxyNode],
    tags: &[String],
) -> Vec<&'a ProxyNode> {
    use crate::domain::PoolMode;
    let live = |n: &ProxyNode| tags.iter().any(|t| t == &outbound_tag(n));
    match &pool.mode {
        PoolMode::Explicit { node_ids } => node_ids
            .iter()
            .filter_map(|nid| nodes.iter().find(|n| &n.id == nid))
            .filter(|n| live(n))
            .collect(),
        PoolMode::Keyword { include, exclude } => {
            let mut members: Vec<&ProxyNode> = nodes
                .iter()
                .filter(|n| crate::domain::name_matches_keywords(&n.name, include, exclude))
                .filter(|n| live(n))
                .collect();
            members.sort_by_key(|n| n.latency_ms.unwrap_or(u32::MAX / 4));
            members
        }
    }
}

/// Per-chain-hop-local outbound tag for a `Node` hop. Deliberately distinct
/// from `outbound_tag(node)` — the chain rewrites this clone's `detour` to
/// the next hop, and that must never leak onto the tag the node is directly
/// selectable under elsewhere in the config.
pub fn chain_hop_outbound_tag(chain: &crate::domain::ProxyChain, hop_index: usize) -> String {
    format!(
        "{}-h{hop_index}",
        &chain.id[chain.id.len().saturating_sub(20)..]
    )
}

/// Build the `detour` chain for one [`ProxyChain`]. User-facing semantics:
/// traffic flows hop[0] → hop[1] → … → hop[N-1] → internet — hop[0] is the
/// client-side entry, hop[N-1] is the exit the internet sees.
///
/// In sing-box terms `A.detour = B` means "A's server connection is dialed
/// through B, and A remains the exit" — so the route must point at the LAST
/// hop's tag, and each hop[i] (i ≥ 1) carries `detour: hop[i-1]`. hop[0] has
/// no detour (the client dials it directly). Only `Node` hops need a minted
/// outbound (their own extra_outbounds, e.g. shadowtls, ride along).
///
/// `Pool` hops split by position. A pool at hop[0] is the client-side entry:
/// the client dials the selected member directly, so the shared pool
/// selector works as-is. A pool at i ≥ 1 is dialed through hop[i-1], which
/// the shared selector's detour-less members can't express — it's expanded
/// into per-member clones (each carrying `detour` → the previous hop's tag)
/// grouped under a chain-local selector, so whichever member is picked keeps
/// chaining.
///
/// sing-box requires a `detour` target to be defined before the outbound
/// that references it, so hops are emitted in forward order (each hop's
/// target — the previous hop — already precedes it). Returns `None` if any
/// hop is unreachable (stale id / empty pool / member that can't map to an
/// outbound) — a chain with a broken link can't route traffic, so the whole
/// chain is treated as absent rather than silently truncated.
fn build_chain_outbounds_for(
    chain: &crate::domain::ProxyChain,
    pools: &[crate::domain::NodePool],
    nodes: &[ProxyNode],
    tags: &[String],
    live_pool_ids: &std::collections::HashSet<&str>,
) -> Option<(Vec<Value>, String)> {
    use crate::domain::ChainHop;

    let last = chain.hops.len().saturating_sub(1);

    // Resolve every hop's tag first, bailing out on any dead hop before
    // emitting anything.
    let mut hop_tags: Vec<String> = Vec::with_capacity(chain.hops.len());
    for (i, hop) in chain.hops.iter().enumerate() {
        match hop {
            ChainHop::Node { node_id } => {
                // Must have actually produced a usable outbound, not just
                // exist in the node list — e.g. a node whose protocol config
                // failed to map (see `node_to_outbound`'s Err path) is present
                // in `nodes` but absent from `tags`.
                let live = nodes
                    .iter()
                    .find(|n| &n.id == node_id)
                    .is_some_and(|n| tags.iter().any(|t| t == &outbound_tag(n)));
                if !live {
                    return None;
                }
                hop_tags.push(chain_hop_outbound_tag(chain, i));
            }
            ChainHop::Pool { pool_id } => {
                if !live_pool_ids.contains(pool_id.as_str()) {
                    return None;
                }
                // hop[0] pool: client-side entry, shared selector is fine;
                // pools at i ≥ 1 get a chain-local selector over clones.
                let tag = if i == 0 {
                    crate::domain::pool_outbound_tag_for_id(pool_id)
                } else {
                    chain_hop_outbound_tag(chain, i)
                };
                hop_tags.push(tag);
            }
        }
    }

    let mut outbounds = Vec::with_capacity(chain.hops.len());
    // Emit in forward order so each hop's detour target (the previous hop)
    // already precedes it.
    for i in 0..chain.hops.len() {
        let prev_tag = if i == 0 {
            None
        } else {
            Some(hop_tags[i - 1].clone())
        };
        match &chain.hops[i] {
            ChainHop::Pool { pool_id } if i != 0 => {
                let pool = pools
                    .iter()
                    .find(|p| &p.id == pool_id)
                    .expect("liveness checked in the tag pass");
                let members = pool_member_nodes(pool, nodes, tags);
                let mut clone_tags = Vec::with_capacity(members.len());
                for (j, node) in members.into_iter().enumerate() {
                    let clone_tag = format!("{}-m{j}", hop_tags[i]);
                    let (_, mut ob, extra) = match node_to_outbound_tagged(node, Some(&clone_tag)) {
                        Ok(v) => v,
                        Err(_) => return None,
                    };
                    outbounds.extend(extra);
                    if let (Some(prev), Some(obj)) = (prev_tag.as_ref(), ob.as_object_mut()) {
                        obj.insert("detour".into(), json!(prev));
                    }
                    outbounds.push(ob);
                    clone_tags.push(clone_tag);
                }
                // A pool with zero clones was filtered by `live_pool_ids`
                // already; guard anyway — an empty selector is invalid.
                if clone_tags.is_empty() {
                    return None;
                }
                let default = clone_tags[0].clone();
                outbounds.push(json!({
                    "type": "selector",
                    "tag": hop_tags[i],
                    "outbounds": clone_tags,
                    "default": default,
                }));
            }
            // hop[0] pool: the shared pool selector is emitted by
            // `build_pool_selectors` and is dialed by the client directly.
            ChainHop::Pool { .. } => {}
            ChainHop::Node { node_id } => {
                let node = nodes
                    .iter()
                    .find(|n| &n.id == node_id)
                    .expect("presence checked above");
                let own_tag = hop_tags[i].clone();
                let (_, mut ob, extra) = match node_to_outbound_tagged(node, Some(&own_tag)) {
                    Ok(v) => v,
                    Err(_) => return None,
                };
                outbounds.extend(extra);
                if let (Some(prev), Some(obj)) = (prev_tag.as_ref(), ob.as_object_mut()) {
                    obj.insert("detour".into(), json!(prev));
                }
                outbounds.push(ob);
            }
        }
    }

    // Rules route to the LAST hop — it is the exit the internet sees.
    let entry_tag = hop_tags[last].clone();
    Some((outbounds, entry_tag))
}

/// One resolved chain's outbounds + its route-facing entry tag, or `None` if
/// the chain is currently broken (stale hop). Skips chains whose id doesn't
/// resolve to anything — same as an orphaned pool/node pin elsewhere.
fn build_chain_outbounds(
    chains: &[crate::domain::ProxyChain],
    pools: &[crate::domain::NodePool],
    nodes: &[ProxyNode],
    tags: &[String],
) -> (Vec<Value>, std::collections::HashMap<String, String>) {
    let live_pool_ids: std::collections::HashSet<&str> = pools
        .iter()
        .filter(|p| !pool_member_tags(p, nodes, tags).is_empty())
        .map(|p| p.id.as_str())
        .collect();
    let mut outbounds = Vec::new();
    let mut entry_tags = std::collections::HashMap::new();
    for chain in chains {
        if let Some((chain_outbounds, entry_tag)) =
            build_chain_outbounds_for(chain, pools, nodes, tags, &live_pool_ids)
        {
            outbounds.extend(chain_outbounds);
            entry_tags.insert(chain.id.clone(), entry_tag);
        }
    }
    (outbounds, entry_tags)
}

/// Nodes matching smart filters (for probe / UI).
pub fn smart_pool_nodes(r: &Rule, nodes: &[ProxyNode]) -> Vec<ProxyNode> {
    nodes
        .iter()
        .filter(|n| r.smart_name_matches(&n.name))
        .cloned()
        .collect()
}

fn build_smart_rule_selectors(rules: &[Rule], nodes: &[ProxyNode], tags: &[String]) -> Vec<Value> {
    use crate::domain::RuleTarget;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in rules
        .iter()
        // Empty-payload rules never reach a route rule-set, so their selector
        // would be a dead outbound — skipping keeps "empty set ⇒ no config
        // output" true for restart-skipping decisions.
        .filter(|r| {
            r.enabled && !r.payload.trim().is_empty() && matches!(r.target, RuleTarget::Smart)
        })
    {
        let group = r.smart_outbound_tag();
        if !seen.insert(group.clone()) {
            continue;
        }
        let pool = smart_pool_tags(r, nodes, tags);
        if pool.is_empty() {
            continue;
        }
        let default = pool.first().cloned().unwrap_or_else(|| "direct".into());
        out.push(json!({
            "type": "selector",
            "tag": group,
            "outbounds": pool,
            "default": default,
        }));
    }
    out
}

fn node_to_outbound(node: &ProxyNode) -> AppResult<(String, Value, Vec<Value>)> {
    node_to_outbound_tagged(node, None)
}

/// Same as [`node_to_outbound`] but lets the caller pin the outbound's own
/// tag (and, transitively, any per-node extra outbound tag such as
/// shadowtls's detour target) to something other than `outbound_tag(node)`.
/// Used to mint chain-hop-local clones of a node's outbound so a chain's
/// `detour` edits never touch the tag the node is directly selectable under.
fn node_to_outbound_tagged(
    node: &ProxyNode,
    tag_override: Option<&str>,
) -> AppResult<(String, Value, Vec<Value>)> {
    let tag = tag_override
        .map(str::to_string)
        .unwrap_or_else(|| outbound_tag(node));
    let mut extra_outbounds = Vec::new();
    let mut ob = match (&node.protocol, &node.config) {
        (
            Protocol::Shadowsocks,
            ProtocolConfig::Shadowsocks {
                method,
                password,
                plugin,
                plugin_opts,
                shadow_tls,
            },
        ) => {
            let mut o = json!({
                "type": "shadowsocks",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "method": method,
                "password": password,
            });
            if let Some(p) = plugin {
                o["plugin"] = json!(p);
            }
            if let Some(opts) = plugin_opts {
                o["plugin_opts"] = json!(opts);
            }
            if let Some(st) = shadow_tls {
                // sing-box has no SIP003 arg-string form for shadow-tls: it's
                // a separate `shadowtls` outbound the ss outbound detours
                // through (mirrors xmdhs/clash2singbox's shadowTls()). The
                // shadowtls outbound is intentionally excluded from `tags`
                // (the selector's outbound list) since it isn't a node the
                // user can pick directly.
                let detour_tag = format!("{tag}-shadowtls");
                o["server"] = json!("");
                o["server_port"] = json!(0);
                o["detour"] = json!(detour_tag.clone());
                let mut tls = json!({
                    "enabled": true,
                    "server_name": st.host,
                });
                if let Some(fp) = &st.fingerprint {
                    tls["utls"] = json!({ "enabled": true, "fingerprint": fp });
                }
                extra_outbounds.push(json!({
                    "type": "shadowtls",
                    "tag": detour_tag,
                    "server": node.server,
                    "server_port": node.port,
                    "version": st.version,
                    "password": st.password,
                    "tls": tls,
                }));
            }
            o
        }
        (
            Protocol::Vmess,
            ProtocolConfig::Vmess {
                uuid,
                alter_id,
                security,
            },
        ) => {
            json!({
                "type": "vmess",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "uuid": uuid,
                "security": security,
                "alter_id": alter_id,
            })
        }
        (
            Protocol::Vless,
            ProtocolConfig::Vless {
                uuid,
                flow,
                packet_encoding,
            },
        ) => {
            let mut o = json!({
                "type": "vless",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "uuid": uuid,
                "packet_encoding": packet_encoding,
            });
            if let Some(f) = flow {
                if !f.is_empty() {
                    o["flow"] = json!(f);
                }
            }
            o
        }
        (Protocol::Trojan, ProtocolConfig::Trojan { password }) => {
            json!({
                "type": "trojan",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "password": password,
            })
        }
        (
            Protocol::Hysteria2,
            ProtocolConfig::Hysteria2 {
                password,
                up_mbps,
                down_mbps,
                obfs,
                obfs_password,
            },
        ) => {
            let mut o = json!({
                "type": "hysteria2",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "password": password,
            });
            if let Some(u) = up_mbps {
                o["up_mbps"] = json!(u);
            }
            if let Some(d) = down_mbps {
                o["down_mbps"] = json!(d);
            }
            if let Some(t) = obfs {
                let mut obfs_obj = json!({ "type": t });
                if let Some(p) = obfs_password {
                    obfs_obj["password"] = json!(p);
                }
                o["obfs"] = obfs_obj;
            }
            o
        }
        (
            Protocol::Tuic,
            ProtocolConfig::Tuic {
                uuid,
                password,
                congestion_control,
                udp_relay_mode,
                zero_rtt_handshake,
            },
        ) => {
            let mut o = json!({
                "type": "tuic",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "uuid": uuid,
                "password": password,
                "zero_rtt_handshake": zero_rtt_handshake,
            });
            if let Some(c) = congestion_control {
                o["congestion_control"] = json!(c);
            }
            if let Some(m) = udp_relay_mode {
                o["udp_relay_mode"] = json!(m);
            }
            o
        }
        (Protocol::Socks5, ProtocolConfig::Socks5 { username, password }) => {
            let mut o = json!({
                "type": "socks",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "version": "5",
            });
            if let Some(u) = username {
                o["username"] = json!(u);
            }
            if let Some(p) = password {
                o["password"] = json!(p);
            }
            o
        }
        (
            Protocol::Http,
            ProtocolConfig::Http {
                username,
                password,
                path,
            },
        ) => {
            let mut o = json!({ "type": "http", "tag": tag.clone(), "server": node.server, "server_port": node.port });
            if let Some(v) = username {
                o["username"] = json!(v);
            }
            if let Some(v) = password {
                o["password"] = json!(v);
            }
            if let Some(v) = path {
                o["path"] = json!(v);
            }
            o
        }
        (
            Protocol::Hysteria,
            ProtocolConfig::Hysteria {
                auth,
                auth_base64,
                up_mbps,
                down_mbps,
                obfs,
            },
        ) => {
            let mut o = json!({ "type": "hysteria", "tag": tag.clone(), "server": node.server, "server_port": node.port });
            if *auth_base64 {
                o["auth"] = json!(auth);
            } else {
                o["auth_str"] = json!(auth);
            }
            o["up_mbps"] = json!(up_mbps.unwrap_or(100));
            o["down_mbps"] = json!(down_mbps.unwrap_or(100));
            if let Some(v) = obfs {
                o["obfs"] = json!(v);
            }
            o
        }
        (Protocol::ShadowTls, ProtocolConfig::ShadowTls { version, password }) => {
            let mut o = json!({ "type": "shadowtls", "tag": tag.clone(), "server": node.server, "server_port": node.port, "version": version });
            if let Some(v) = password {
                o["password"] = json!(v);
            }
            o
        }
        (
            Protocol::Ssh,
            ProtocolConfig::Ssh {
                user,
                password,
                private_key,
                private_key_passphrase,
                host_key,
            },
        ) => {
            let mut o = json!({ "type": "ssh", "tag": tag.clone(), "server": node.server, "server_port": node.port, "user": user });
            if let Some(v) = password {
                o["password"] = json!(v);
            }
            if let Some(v) = private_key {
                o["private_key"] = json!(v);
            }
            if let Some(v) = private_key_passphrase {
                o["private_key_passphrase"] = json!(v);
            }
            if !host_key.is_empty() {
                o["host_key"] = json!(host_key);
            }
            o
        }
        (
            Protocol::Naive,
            ProtocolConfig::Naive {
                username,
                password,
                quic,
            },
        ) => {
            json!({ "type": "naive", "tag": tag.clone(), "server": node.server, "server_port": node.port, "username": username, "password": password, "quic": quic })
        }
        (
            Protocol::Tor,
            ProtocolConfig::Tor {
                executable_path,
                extra_args,
                data_directory,
            },
        ) => {
            let mut o =
                json!({ "type": "tor", "tag": tag.clone(), "executable_path": executable_path });
            if !extra_args.is_empty() {
                o["extra_args"] = json!(extra_args);
            }
            if let Some(v) = data_directory {
                o["data_directory"] = json!(v);
            }
            o
        }
        (
            Protocol::WireGuard,
            ProtocolConfig::WireGuard {
                local_address,
                private_key,
                peer_public_key,
                pre_shared_key,
                reserved,
                mtu,
            },
        ) => {
            let mut peer = json!({ "address": node.server, "port": node.port, "public_key": peer_public_key, "allowed_ips": ["0.0.0.0/0", "::/0"] });
            if let Some(v) = pre_shared_key {
                peer["pre_shared_key"] = json!(v);
            }
            if !reserved.is_empty() {
                peer["reserved"] = json!(reserved);
            }
            let mut o = json!({ "type": "wireguard", "tag": tag.clone(), "address": local_address, "private_key": private_key, "peers": [peer] });
            if let Some(v) = mtu {
                o["mtu"] = json!(v);
            }
            o
        }
        (Protocol::AnyTls, ProtocolConfig::AnyTls { password }) => {
            // sing-box ≥ 1.12; TLS is required on the outbound.
            json!({
                "type": "anytls",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "password": password,
            })
        }
        (
            Protocol::Snell,
            ProtocolConfig::Snell {
                psk,
                version,
                userkey,
                reuse,
                obfs_mode,
                obfs_host,
                mode,
            },
        ) => {
            // sing-box ≥ 1.14; accepts version 4 or 6 (v1–3/v5 may fail at core runtime).
            let ver = match *version {
                6 => 6,
                // v5 wire ≈ v4 per sing-box docs
                1 | 2 | 3 | 4 | 5 => 4,
                other => other,
            };
            let mut o = json!({
                "type": "snell",
                "tag": tag.clone(),
                "server": node.server,
                "server_port": node.port,
                "version": ver,
                "psk": psk,
            });
            if let Some(uk) = userkey {
                if !uk.is_empty() {
                    o["userkey"] = json!(uk);
                }
            }
            if let Some(true) = reuse {
                o["reuse"] = json!(true);
            }
            if ver == 6 {
                if let Some(m) = mode {
                    let m = m.replace('_', "-").to_ascii_lowercase();
                    if matches!(m.as_str(), "default" | "unshaped" | "unsafe-raw") {
                        o["mode"] = json!(m);
                    }
                }
            } else {
                // v4: HTTP obfs only (`none` | `http`). Clash also uses `tls` → map to none.
                if let Some(m) = obfs_mode {
                    let m = m.to_ascii_lowercase();
                    if m == "http" {
                        o["obfs_mode"] = json!("http");
                        if let Some(h) = obfs_host {
                            if !h.is_empty() {
                                o["obfs_host"] = json!(h);
                            }
                        }
                    }
                }
            }
            o
        }
        _ => {
            return Err(AppError::Config(format!(
                "protocol/config mismatch for {}",
                node.name
            )));
        }
    };

    if let Some(tls) = &node.tls {
        if let Some(tls_val) = tls_to_json(tls) {
            ob.as_object_mut()
                .ok_or_else(|| AppError::Config("outbound not object".into()))?
                .insert("tls".into(), tls_val);
        }
    }

    // AnyTLS requires a TLS block in sing-box.
    if matches!(
        node.protocol,
        Protocol::AnyTls | Protocol::ShadowTls | Protocol::Naive
    ) {
        let obj = ob
            .as_object_mut()
            .ok_or_else(|| AppError::Config("outbound not object".into()))?;
        if !obj.contains_key("tls") {
            obj.insert("tls".into(), json!({ "enabled": true }));
        }
    }

    if let Some(transport) = &node.transport {
        if let Some(t) = transport_to_json(transport) {
            ob.as_object_mut()
                .ok_or_else(|| AppError::Config("outbound not object".into()))?
                .insert("transport".into(), t);
        }
    }

    Ok((tag, ob, extra_outbounds))
}

/// Only emit known uTLS profile names (ignore hex pins / garbage from subscriptions).
fn normalize_utls_fingerprint(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    const VALID: &[&str] = &[
        "chrome",
        "firefox",
        "safari",
        "ios",
        "android",
        "edge",
        "360",
        "qq",
        "random",
        "chrome_psk",
        "chrome_psk_shuffle",
        "chrome_padding_psk_shuffle",
        "chrome_pq",
        "chrome_pq_psk",
    ];
    if VALID.contains(&s.as_str()) {
        Some(s)
    } else {
        None
    }
}

fn tls_to_json(tls: &TlsConfig) -> Option<Value> {
    if !tls.enabled && tls.reality_public_key.is_none() {
        return None;
    }
    let mut o = json!({ "enabled": true });
    if let Some(sni) = &tls.server_name {
        o["server_name"] = json!(sni);
    }
    if let Some(true) = tls.insecure {
        o["insecure"] = json!(true);
    }
    if let Some(alpn) = &tls.alpn {
        if !alpn.is_empty() {
            o["alpn"] = json!(alpn);
        }
    }
    let normalized_fp = normalize_utls_fingerprint(tls.utls_fingerprint.as_deref());
    // Reality requires uTLS; fall back to "chrome" when the subscription didn't
    // provide a valid fingerprint, otherwise sing-box rejects the outbound with
    // "uTLS is required by reality client".
    let fp_for_utls = if tls.reality_public_key.is_some() {
        Some(
            normalized_fp
                .clone()
                .unwrap_or_else(|| "chrome".to_string()),
        )
    } else {
        normalized_fp
    };
    if let Some(fp) = fp_for_utls {
        o["utls"] = json!({
            "enabled": true,
            "fingerprint": fp
        });
    }
    if let Some(pk) = &tls.reality_public_key {
        let mut reality = json!({
            "enabled": true,
            "public_key": pk
        });
        if let Some(sid) = &tls.reality_short_id {
            reality["short_id"] = json!(sid);
        }
        o["reality"] = reality;
    }
    Some(o)
}

fn transport_to_json(t: &Transport) -> Option<Value> {
    match t {
        Transport::Tcp => None,
        Transport::Ws {
            path,
            headers,
            max_early_data,
        } => {
            let mut o = json!({ "type": "ws" });
            if let Some(p) = path {
                o["path"] = json!(p);
            }
            if let Some(h) = headers {
                if !h.is_empty() {
                    o["headers"] = json!(h);
                }
            }
            if let Some(m) = max_early_data {
                o["max_early_data"] = json!(m);
            }
            Some(o)
        }
        Transport::Grpc { service_name } => {
            let mut o = json!({ "type": "grpc" });
            if let Some(s) = service_name {
                o["service_name"] = json!(s);
            }
            Some(o)
        }
        Transport::Http { path, host } => {
            let mut o = json!({ "type": "http" });
            if let Some(p) = path {
                o["path"] = json!(p);
            }
            if let Some(h) = host {
                o["host"] = json!(h);
            }
            Some(o)
        }
        Transport::HttpUpgrade { path, host } => {
            let mut o = json!({ "type": "httpupgrade" });
            if let Some(p) = path {
                o["path"] = json!(p);
            }
            if let Some(h) = host {
                o["host"] = json!(h);
            }
            Some(o)
        }
    }
}

pub fn generate_api_secret() -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{:?}", std::time::SystemTime::now()).as_bytes());
    hasher.update(std::process::id().to_string().as_bytes());
    hasher.update(b"satelite-proxy-clash-api");
    hex::encode(hasher.finalize())[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Protocol, ProtocolConfig, ProxyNode, TlsConfig, Transport};
    use std::collections::BTreeMap;

    fn sample_ss() -> ProxyNode {
        ProxyNode {
            id: "aabbccddeeff0011".into(),
            name: "SS-HK".into(),
            protocol: Protocol::Shadowsocks,
            server: "ss.example.com".into(),
            port: 8388,
            tls: None,
            transport: None,
            udp: Some(true),
            config: ProtocolConfig::Shadowsocks {
                method: "aes-256-gcm".into(),
                password: "secret".into(),
                plugin: None,
                plugin_opts: None,
                shadow_tls: None,
            },
            source: Some("ss".into()),
            latency_ms: None,
            latency_at: None,
        }
    }

    fn sample_wg(id: &str) -> ProxyNode {
        ProxyNode {
            id: id.into(),
            name: "WG-HK".into(),
            protocol: Protocol::WireGuard,
            server: "wg.example.com".into(),
            port: 51820,
            tls: None,
            transport: None,
            udp: None,
            config: ProtocolConfig::WireGuard {
                local_address: vec!["172.16.0.2/32".into()],
                private_key: "priv".into(),
                peer_public_key: "pub".into(),
                pre_shared_key: None,
                reserved: vec![],
                mtu: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        }
    }

    // Regression for `duplicate outbound/endpoint tag: node-xxx`: two nodes
    // with a colliding id (same name/server/port/protocol, different
    // credentials) must still build a valid config with distinct tags.
    #[test]
    fn colliding_node_ids_yield_unique_outbound_tags() {
        let opts = || BuildOptions {
            mixed_port: 2080,
            allow_lan: false,
            api_port: 19090,
            extra_inbounds: vec![],
            api_secret: "test".into(),
            current_node_id: None,
            log_level: "info".into(),
            rules: vec![],
            rule_sets: vec![],
            pools: vec![],
            chains: vec![],
            tun_enabled: false,
            tun_stack: "mixed".into(),
            dns: DnsSettings::default(),
            outbound_mode: OutboundMode::Rule,
            route_final: "proxy".into(),
            auto_select: crate::domain::AutoSelectMode::Off,
            probe_url: "https://www.gstatic.com/generate_204".into(),
            find_process: true,
            tun_ipv6: false,
            block_quic: false,
            bypass_lan: false,
            tun_interface_name: None,
        };

        // Both outbounds.
        let a = sample_ss();
        let mut b = sample_ss();
        if let ProtocolConfig::Shadowsocks { password, .. } = &mut b.config {
            *password = "other-secret".into();
        }
        assert_eq!(a.id, b.id, "precondition: ids collide");
        let built = build_singbox_config(&[a, b], &opts()).unwrap();
        assert_eq!(built.outbound_tags.len(), 2);
        assert_ne!(built.outbound_tags[0], built.outbound_tags[1]);
        let selector = built.value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == "proxy")
            .expect("proxy selector");
        let members = selector["outbounds"].as_array().unwrap();
        assert!(members.contains(&json!(built.outbound_tags[0])));
        assert!(members.contains(&json!(built.outbound_tags[1])));

        // WireGuard endpoint shares the tag namespace with outbounds — a
        // colliding id must be disambiguated across both lists too.
        let ss = sample_ss();
        let wg = sample_wg(&ss.id);
        let built = build_singbox_config(&[ss, wg], &opts()).unwrap();
        assert_eq!(built.outbound_tags.len(), 2);
        assert_ne!(built.outbound_tags[0], built.outbound_tags[1]);
        let endpoints = built.value["endpoints"].as_array().expect("endpoints");
        assert_eq!(endpoints.len(), 1);
        let endpoint_tag = endpoints[0]["tag"].as_str().unwrap();
        let outbound_tags: Vec<&str> = built.value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|o| o["tag"].as_str())
            .collect();
        assert!(
            !outbound_tags.contains(&endpoint_tag),
            "endpoint tag {endpoint_tag} duplicated in outbounds {outbound_tags:?}"
        );
    }

    #[test]
    fn shadow_tls_ss_node_produces_ss_outbound_detoured_through_shadowtls_outbound() {
        let mut node = sample_ss();
        node.config = ProtocolConfig::Shadowsocks {
            method: "aes-256-gcm".into(),
            password: "secret".into(),
            plugin: Some("shadow-tls".into()),
            plugin_opts: None,
            shadow_tls: Some(crate::domain::ShadowTlsOpts {
                host: "www.bing.com".into(),
                password: "tls-pass".into(),
                version: 3,
                fingerprint: Some("chrome".into()),
            }),
        };
        let (tag, ob, extra) = node_to_outbound(&node).unwrap();

        // The ss outbound no longer dials the real server directly; it
        // detours through the shadowtls outbound instead.
        assert_eq!(ob["type"], "shadowsocks");
        assert_eq!(ob["server"], "");
        assert_eq!(ob["server_port"], 0);
        let detour_tag = ob["detour"].as_str().unwrap().to_string();
        assert_eq!(detour_tag, format!("{tag}-shadowtls"));

        assert_eq!(extra.len(), 1);
        let sl = &extra[0];
        assert_eq!(sl["type"], "shadowtls");
        assert_eq!(sl["tag"], detour_tag);
        assert_eq!(sl["server"], node.server);
        assert_eq!(sl["server_port"], node.port);
        assert_eq!(sl["version"], 3);
        assert_eq!(sl["password"], "tls-pass");
        assert_eq!(sl["tls"]["enabled"], true);
        assert_eq!(sl["tls"]["server_name"], "www.bing.com");
        assert_eq!(sl["tls"]["utls"]["fingerprint"], "chrome");
    }

    // The shadowtls detour outbound isn't a user-selectable node, so it must
    // not appear in the selector's outbound tag list.
    #[test]
    fn shadow_tls_detour_outbound_is_excluded_from_selectable_tags() {
        let mut node = sample_ss();
        node.config = ProtocolConfig::Shadowsocks {
            method: "aes-256-gcm".into(),
            password: "secret".into(),
            plugin: Some("shadow-tls".into()),
            plugin_opts: None,
            shadow_tls: Some(crate::domain::ShadowTlsOpts {
                host: "www.bing.com".into(),
                password: "tls-pass".into(),
                version: 3,
                fingerprint: None,
            }),
        };
        let built = build_singbox_config(
            &[node],
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        let outbounds = built.value["outbounds"].as_array().unwrap();
        let types: Vec<&str> = outbounds
            .iter()
            .map(|o| o["type"].as_str().unwrap_or_default())
            .collect();
        assert!(types.contains(&"shadowtls"), "types: {types:?}");
        assert!(types.contains(&"shadowsocks"), "types: {types:?}");
        assert!(
            !built
                .outbound_tags
                .iter()
                .any(|t| t.ends_with("-shadowtls")),
            "outbound_tags: {:?}",
            built.outbound_tags
        );
    }

    #[test]
    fn remote_block_rejects_route_and_dns_as_a_whole_set() {
        let set = RuleSet::new_remote(
            "AdBlock",
            "https://example.com/adblock.json",
            RuleTarget::Block,
        );
        let mut set = set;
        let local_path = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .to_string();
        set.remote.as_mut().unwrap().local_path = Some(local_path.clone());
        let tag = set.id.clone();
        let (definitions, routes, dns) =
            build_grouped_rule_sets(&[set.clone()], &[], &[], &Default::default());

        assert_eq!(definitions[0]["tag"], tag);
        assert_eq!(definitions[0]["type"], "local");
        assert_eq!(definitions[0]["format"], "source");
        assert_eq!(definitions[0]["path"], local_path);
        assert!(definitions[0].get("url").is_none());
        assert_eq!(
            routes[0],
            json!({ "rule_set": [tag.clone()], "action": "reject" })
        );
        assert_eq!(dns[0], json!({ "rule_set": [tag], "action": "reject" }));

        set.remote.as_mut().unwrap().format = "binary".into();
        let (binary_definitions, _, _) =
            build_grouped_rule_sets(&[set], &[], &[], &Default::default());
        assert_eq!(binary_definitions[0]["format"], "binary");
    }

    #[test]
    fn remote_proxy_and_direct_sets_generate_group_route_and_dns_rules() {
        for (target, outbound, dns_strategy, dns_server) in [
            (
                RuleTarget::Proxy,
                "proxy",
                crate::domain::RuleSetDnsStrategy::Local,
                "dns-local",
            ),
            (
                RuleTarget::Direct,
                "direct",
                crate::domain::RuleSetDnsStrategy::Domestic,
                "dns-cn",
            ),
        ] {
            let mut set = RuleSet::new_remote("Remote", "https://example.com/rules.json", target);
            set.remote.as_mut().unwrap().local_path = Some(
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
            set.dns_strategy = dns_strategy;
            let tag = set.id.clone();
            let (_, routes, dns) = build_grouped_rule_sets(&[set], &[], &[], &Default::default());
            assert_eq!(
                routes[0],
                json!({ "rule_set": [tag.clone()], "action": "route", "outbound": outbound })
            );
            assert_eq!(
                dns[0],
                json!({ "rule_set": [tag], "action": "route", "server": dns_server })
            );
        }
    }

    #[test]
    fn plain_set_routes_uniform_targets_through_parent_tag() {
        // Since the v6 store migration (and rewrite-on-strategy-flip), every
        // non-smart local set is uniform unless the user mixes per-rule
        // routes on purpose. A uniform set keeps the classic single-group
        // shape: the parent definition is referenced by both route and DNS.
        let mut set = RuleSet::new_user(
            "整组代理",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "example.com".into(),
                    RuleTarget::Proxy,
                    10,
                ),
                Rule::new(
                    RuleType::DomainSuffix,
                    "example.org".into(),
                    RuleTarget::Proxy,
                    20,
                ),
            ],
        );
        set.strategy = RuleSetStrategy::Proxy;
        set.dns_strategy = crate::domain::RuleSetDnsStrategy::Local;

        let effective = effective_route_rules(&[set.clone()], &[]);
        assert_eq!(effective[0].target, RuleTarget::Proxy);
        assert!(effective
            .iter()
            .all(|rule| rule.target == RuleTarget::Proxy));

        let tag = set.id.clone();
        let (definitions, routes, dns) =
            build_grouped_rule_sets(&[set], &[], &[], &Default::default());
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0]["type"], "inline");
        assert_eq!(
            definitions[0]["rules"][0]["domain_suffix"],
            json!(["example.com", "example.org"])
        );
        assert_eq!(
            routes,
            vec![json!({ "rule_set": [tag.clone()], "action": "route", "outbound": "proxy" })]
        );
        assert_eq!(
            dns,
            vec![json!({ "rule_set": [tag], "action": "route", "server": "dns-local" })]
        );
    }

    #[test]
    fn empty_local_rule_sets_are_dropped_entirely() {
        let mut no_rules = RuleSet::new_user("空集", vec![]);
        no_rules.strategy = RuleSetStrategy::Proxy;

        let mut disabled_only = RuleSet::new_user(
            "全停用",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "example.com".into(),
                RuleTarget::Proxy,
                10,
            )],
        );
        disabled_only.rules[0].enabled = false;

        // Rule::new trims payloads, so both variants below store "".
        let blank_payload = RuleSet::new_user(
            "空载荷",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "  ".into(),
                RuleTarget::Proxy,
                10,
            )],
        );

        let wildcard_only = RuleSet::new_user(
            "仅通配符",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "*".into(),
                RuleTarget::Block,
                10,
            )],
        );

        for set in [no_rules, disabled_only, blank_payload, wildcard_only] {
            let (definitions, routes, dns) =
                build_grouped_rule_sets(&[set], &[], &[], &Default::default());
            assert!(definitions.is_empty(), "empty set must not be registered");
            assert!(routes.is_empty(), "empty set must not be routed");
            assert!(dns.is_empty(), "empty set must not get DNS rules");
        }
    }

    #[test]
    fn empty_smart_rule_sets_emit_no_child_definitions_or_dns_rules() {
        let mut set = RuleSet::new_user(
            "空智能集",
            vec![
                Rule::new(RuleType::DomainSuffix, "  ".into(), RuleTarget::Smart, 10),
                Rule::new(
                    RuleType::DomainKeyword,
                    "chrome".into(),
                    RuleTarget::Block,
                    20,
                ),
            ],
        );
        set.strategy = RuleSetStrategy::Smart;
        // Both rules are uneffective → the whole set must vanish.
        set.rules[1].enabled = false;

        let (definitions, routes, dns) =
            build_grouped_rule_sets(&[set], &[], &[], &Default::default());
        assert!(definitions.is_empty());
        assert!(routes.is_empty());
        assert!(dns.is_empty());
    }

    #[test]
    fn plain_strategy_set_honors_per_rule_routes() {
        let mut set = RuleSet::new_user(
            "混合路由",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "a.com".into(),
                    RuleTarget::Proxy,
                    10,
                ),
                Rule::new(
                    RuleType::DomainSuffix,
                    "b.com".into(),
                    RuleTarget::Direct,
                    20,
                ),
                Rule::new(
                    RuleType::DomainSuffix,
                    "c.com".into(),
                    RuleTarget::Block,
                    30,
                ),
            ],
        );
        set.strategy = RuleSetStrategy::Proxy;

        let (definitions, routes, _dns) =
            build_grouped_rule_sets(&[set], &[], &[], &Default::default());
        // Parent (DNS) + one child per distinct outbound: proxy, direct, reject.
        assert_eq!(definitions.len(), 4);
        let outbounds: Vec<(String, String)> = routes
            .iter()
            .map(|r| {
                (
                    r["action"].as_str().unwrap_or("").to_string(),
                    r["outbound"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        assert!(outbounds.contains(&("route".into(), "proxy".into())));
        assert!(outbounds.contains(&("route".into(), "direct".into())));
        assert!(outbounds.contains(&("reject".into(), String::new())));
        assert_eq!(routes.len(), 3);
    }

    #[test]
    fn plain_strategy_set_clamps_node_and_smart_pins() {
        let mut set = RuleSet::new_user(
            "钳制",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "node.com".into(),
                    RuleTarget::Node,
                    10,
                ),
                Rule::new(
                    RuleType::DomainSuffix,
                    "smart.com".into(),
                    RuleTarget::Smart,
                    20,
                ),
            ],
        );
        set.strategy = RuleSetStrategy::Direct;

        let (_definitions, routes, _dns) =
            build_grouped_rule_sets(&[set], &[], &[], &Default::default());
        // Both pins clamp to the set strategy: a single direct route group.
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["action"], "route");
        assert_eq!(routes[0]["outbound"], "direct");
    }

    fn node_tags(nodes: &[ProxyNode]) -> Vec<String> {
        nodes.iter().map(outbound_tag).collect()
    }

    #[test]
    fn remote_node_set_routes_whole_set_to_pinned_node() {
        let nodes = vec![sample_ss()];
        let tags = node_tags(&nodes);
        let mut set = RuleSet::new_remote(
            "指定节点集",
            "https://example.com/pin.json",
            RuleTarget::Node,
        );
        set.node_id = Some(nodes[0].id.clone());
        set.node_name = Some(nodes[0].name.clone());
        set.remote.as_mut().unwrap().local_path = Some(
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );

        let (_, routes, _) =
            build_grouped_rule_sets(&[set.clone()], &nodes, &tags, &Default::default());
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["outbound"], tags[0]);

        // Stale pin (node removed from the subscription) → main proxy group.
        let mut stale = set;
        stale.node_id = Some("gone".into());
        let (_, routes, _) = build_grouped_rule_sets(&[stale], &nodes, &tags, &Default::default());
        assert_eq!(routes[0]["outbound"], "proxy");
    }

    #[test]
    fn remote_filter_set_routes_whole_set_through_keyword_pool_selector() {
        let nodes = vec![sample_ss()];
        let tags = node_tags(&nodes);
        let mut set = RuleSet::new_remote(
            "过滤集",
            "https://example.com/filter.json",
            RuleTarget::Smart,
        );
        set.strategy = RuleSetStrategy::Filter;
        set.smart_include = vec!["HK".into()];
        set.remote.as_mut().unwrap().local_path = Some(
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );

        let group = set.smart_set_outbound_tag();
        let tag = set.id.clone();
        let (_, routes, _) =
            build_grouped_rule_sets(&[set.clone()], &nodes, &tags, &Default::default());
        assert_eq!(
            routes[0],
            json!({ "rule_set": [tag], "action": "route", "outbound": group })
        );

        let selectors = build_filter_set_selectors(&[set.clone()], &nodes, &tags);
        assert_eq!(selectors.len(), 1);
        assert_eq!(selectors[0]["tag"], group);
        assert_eq!(selectors[0]["outbounds"], json!(tags));
        assert_eq!(selectors[0]["default"], tags[0]);

        // Empty keyword pool (nothing matches) → fall back to the proxy group
        // and emit no dead selector.
        let mut empty = set;
        empty.smart_include = vec![" nonexistent ".into()];
        let (_, routes, _) =
            build_grouped_rule_sets(&[empty.clone()], &nodes, &tags, &Default::default());
        assert_eq!(routes[0]["outbound"], "proxy");
        assert!(build_filter_set_selectors(&[empty], &nodes, &tags).is_empty());
    }

    #[test]
    fn local_node_set_clamps_every_rule_to_set_level_pin() {
        let nodes = vec![sample_ss()];
        let tags = node_tags(&nodes);
        let mut set = RuleSet::new_user(
            "本地指定",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "node.com".into(),
                    RuleTarget::Node,
                    10,
                ),
                Rule::new(
                    RuleType::DomainSuffix,
                    "smart.com".into(),
                    RuleTarget::Smart,
                    20,
                ),
            ],
        );
        set.strategy = RuleSetStrategy::Node;
        set.node_id = Some(nodes[0].id.clone());
        set.node_name = Some(nodes[0].name.clone());

        // Uniform group keeps the classic parent-tag shape, routed to the pin.
        let tag = set.id.clone();
        let (_, routes, _) =
            build_grouped_rule_sets(&[set.clone()], &nodes, &tags, &Default::default());
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["rule_set"], json!([tag]));
        assert_eq!(routes[0]["outbound"], tags[0]);

        // Stale pin → whole set falls back to the proxy group.
        let mut stale = set;
        stale.node_id = Some("gone".into());
        let (_, routes, _) = build_grouped_rule_sets(&[stale], &nodes, &tags, &Default::default());
        assert_eq!(routes[0]["outbound"], "proxy");
    }

    #[test]
    fn local_filter_set_routes_through_one_whole_set_selector() {
        let nodes = vec![sample_ss()];
        let tags = node_tags(&nodes);
        let mut set = RuleSet::new_user(
            "本地过滤",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "a.com".into(),
                    RuleTarget::Smart,
                    10,
                ),
                Rule::new(RuleType::DomainSuffix, "b.com".into(), RuleTarget::Node, 20),
            ],
        );
        set.strategy = RuleSetStrategy::Filter;
        set.smart_include = vec!["HK".into()];

        // One group for every pool rule (node pins clamp into the pool too):
        // the parent tag carries the route, referencing the whole-set selector.
        let group = set.smart_set_outbound_tag();
        let tag = set.id.clone();
        let (definitions, routes, _) =
            build_grouped_rule_sets(&[set.clone()], &nodes, &tags, &Default::default());
        assert_eq!(definitions.len(), 1, "no child rule-sets for uniform pool");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["rule_set"], json!([tag]));
        assert_eq!(routes[0]["outbound"], group);

        // Selector is emitted exactly once, and per-rule smart selectors stay
        // out (effective_route_rules skips Filter sets entirely).
        assert_eq!(build_filter_set_selectors(&[set], &nodes, &tags).len(), 1);
        let effective = effective_route_rules(&[RuleSet::new_user("本地过滤", vec![])], &[]);
        assert!(effective.is_empty());
    }

    #[test]
    fn smart_set_partitions_only_effective_rules() {
        let mut set = RuleSet::new_user(
            "智能集",
            vec![
                Rule::new(
                    RuleType::DomainSuffix,
                    "openai.com".into(),
                    RuleTarget::Smart,
                    10,
                ),
                Rule::new(RuleType::DomainSuffix, "  ".into(), RuleTarget::Smart, 20),
            ],
        );
        set.strategy = RuleSetStrategy::Smart;

        let (definitions, routes, dns) =
            build_grouped_rule_sets(&[set], &[], &[], &Default::default());
        // Single outbound group: the parent definition itself carries the
        // route (classic shape) — no child rule-set needed.
        assert_eq!(definitions.len(), 1);
        assert!(!definitions[0]["rules"].as_array().unwrap().is_empty());
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["rule_set"], json!([definitions[0]["tag"]]));
        assert_eq!(dns.len(), 1);
    }

    #[test]
    fn empty_rule_set_keeps_generated_config_valid() {
        let nodes = vec![sample_ss()];
        let empty_set = RuleSet::new_user("空集", vec![]);
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![empty_set],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();

        let rule_sets = built.value["route"]["rule_set"].as_array().unwrap();
        assert!(
            rule_sets.is_empty(),
            "empty set must not reach the kernel config"
        );
        let route_rules = built.value["route"]["rules"].as_array().unwrap();
        assert!(route_rules
            .iter()
            .all(|rule| rule.get("rule_set").is_none()));
    }

    #[test]
    fn rule_set_is_empty_for_config_matches_builder_registration() {
        // Local sets: emptiness follows the inline headless body.
        let empty = RuleSet::new_user("空", vec![]);
        assert!(rule_set_is_empty_for_config(&empty));

        let mut disabled_only = RuleSet::new_user(
            "全停用",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "example.com".into(),
                RuleTarget::Proxy,
                10,
            )],
        );
        disabled_only.rules[0].enabled = false;
        assert!(rule_set_is_empty_for_config(&disabled_only));

        let contributing = RuleSet::new_user(
            "有规则",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "example.com".into(),
                RuleTarget::Proxy,
                10,
            )],
        );
        assert!(!rule_set_is_empty_for_config(&contributing));

        // Remote sets: emptiness follows the downloaded cache file.
        let mut undownloaded =
            RuleSet::new_remote("Remote", "https://example.com/r.json", RuleTarget::Proxy);
        assert!(rule_set_is_empty_for_config(&undownloaded));
        undownloaded.remote.as_mut().unwrap().local_path =
            Some("Z:/definitely/missing/file.srs".into());
        assert!(rule_set_is_empty_for_config(&undownloaded));
        undownloaded.remote.as_mut().unwrap().local_path = Some(
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );
        assert!(!rule_set_is_empty_for_config(&undownloaded));

        // The predicate must agree with what the builder actually registers.
        for set in [&empty, &disabled_only, &contributing] {
            let (definitions, routes, dns) =
                build_grouped_rule_sets(&[set.clone()], &[], &[], &Default::default());
            let registered = !definitions.is_empty() && !routes.is_empty() && !dns.is_empty();
            assert_eq!(
                registered,
                !rule_set_is_empty_for_config(set),
                "predicate and builder disagree for {}",
                set.name
            );
        }
    }

    #[test]
    fn builds_extra_inbounds_after_mixed_before_tun() {
        let nodes = vec![sample_ss()];
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![
                    crate::domain::ExtraInbound {
                        id: "i1".into(),
                        kind: "http".into(),
                        port: 2081,
                        allow_lan: false,
                    },
                    crate::domain::ExtraInbound {
                        id: "i2".into(),
                        kind: "mixed".into(),
                        port: 2082,
                        allow_lan: true,
                    },
                ],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: true,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        let inbounds = built.value["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 4);
        assert_eq!(inbounds[0]["tag"], "mixed-in");
        assert_eq!(inbounds[1]["type"], "http");
        assert_eq!(inbounds[1]["tag"], "in-http-2081");
        assert_eq!(inbounds[1]["listen"], "127.0.0.1");
        assert_eq!(inbounds[1]["listen_port"], 2081);
        assert_eq!(inbounds[2]["type"], "mixed");
        assert_eq!(inbounds[2]["tag"], "in-mixed-2082");
        assert_eq!(inbounds[2]["listen"], "0.0.0.0");
        assert_eq!(inbounds[2]["listen_port"], 2082);
        // TUN stays last even with extras present.
        assert_eq!(inbounds[3]["type"], "tun");
    }

    #[test]
    fn allow_lan_switches_main_mixed_listen_host() {
        let nodes = vec![sample_ss()];
        let base = || BuildOptions {
            mixed_port: 2080,
            allow_lan: false,
            api_port: 19090,
            extra_inbounds: vec![],
            api_secret: "test".into(),
            current_node_id: None,
            log_level: "info".into(),
            rules: vec![],
            rule_sets: vec![],
            pools: vec![],
            chains: vec![],
            tun_enabled: false,
            tun_stack: "mixed".into(),
            dns: DnsSettings::default(),
            outbound_mode: OutboundMode::Rule,
            route_final: "proxy".into(),
            auto_select: crate::domain::AutoSelectMode::Off,
            probe_url: "https://www.gstatic.com/generate_204".into(),
            find_process: true,
            tun_ipv6: false,
            block_quic: false,
            bypass_lan: false,
            tun_interface_name: None,
        };

        let localhost = build_singbox_config(&nodes, &base()).unwrap();
        assert_eq!(
            localhost.value["inbounds"][0]["listen"], "127.0.0.1",
            "main mixed inbound defaults to loopback"
        );

        let lan = build_singbox_config(
            &nodes,
            &BuildOptions {
                allow_lan: true,
                ..base()
            },
        )
        .unwrap();
        assert_eq!(
            lan.value["inbounds"][0]["listen"], "0.0.0.0",
            "allow_lan opens the main mixed inbound to the LAN"
        );
    }

    #[test]
    fn builds_selector() {
        let nodes = vec![sample_ss()];
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        assert_eq!(built.outbound_tags.len(), 1);
        assert_eq!(built.selected_tag, "node-aabbccddeeff0011");
        let inbounds = built.value["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 1);
        assert_eq!(inbounds[0]["type"], "mixed");
        assert!(built.value.get("dns").is_some());
        // Without TUN the system resolver stays the fastest choice.
        assert_eq!(built.value["route"]["default_domain_resolver"], "dns-local");
        assert_eq!(built.value["route"]["final"], "proxy");
        let proxy = built.value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == "proxy")
            .unwrap();
        assert_eq!(proxy["type"], "selector");
    }

    #[test]
    fn hosts_override_is_injected_before_user_route_rules() {
        use crate::domain::{HostsConfig, HostsEntry};

        let nodes = vec![sample_ss()];
        let dns = DnsSettings {
            hosts: HostsConfig {
                enabled: true,
                include_system: false,
                entries: vec![HostsEntry {
                    id: "baidu".into(),
                    enabled: true,
                    domain: "baidu.com".into(),
                    addr: "192.168.1.1".into(),
                }],
            },
            ..DnsSettings::default()
        };
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns,
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();

        let rules = built.value["route"]["rules"].as_array().unwrap();
        let host_rule = rules
            .iter()
            .find(|rule| rule["override_address"] == "192.168.1.1")
            .expect("hosts route override");
        assert_eq!(host_rule["domain"], json!(["baidu.com"]));
        assert_eq!(host_rule["action"], "route-options");
    }

    #[test]
    fn builds_urltest_when_kernel_auto_select() {
        let nodes = vec![sample_ss()];
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Kernel,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        let proxy = built.value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == "proxy")
            .unwrap();
        assert_eq!(proxy["type"], "urltest");
        assert_eq!(proxy["url"], "https://www.gstatic.com/generate_204");
        assert!(proxy["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t != "direct"));
    }

    #[test]
    fn builds_with_tun_inbound() {
        let nodes = vec![sample_ss()];
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: true,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        let inbounds = built.value["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[1]["type"], "tun");
        assert_eq!(inbounds[1]["auto_route"], true);
        assert_eq!(inbounds[1]["stack"], "mixed");
        // strict_route must be on on every platform now (problem 5): the
        // Windows-only carve-out is gone, and route_exclude_address below
        // already protects host → 127.0.0.1 (clash_api / mixed) on macOS.
        assert_eq!(inbounds[1]["strict_route"], true);
        // IPv6 off by default (problem 1): a dual-stack tun makes Chrome
        // prefer AAAA/v6 even when the node has no v6 egress.
        assert_eq!(inbounds[1]["address"], json!(["172.19.0.1/30"]));
        assert!(inbounds[1]["route_exclude_address"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "127.0.0.0/8")));
        assert!(built.value.get("dns").is_some());
        // TUN must not resolve outbound domains via the system resolver:
        // those queries get hijacked into the tunnel (loop), so the direct
        // UDP server is used instead.
        assert_eq!(built.value["route"]["default_domain_resolver"], "dns-cn");
        let rules = built.value["route"]["rules"].as_array().unwrap();
        assert!(rules
            .iter()
            .any(|r| r.get("action") == Some(&json!("sniff"))));
        assert!(rules
            .iter()
            .any(|r| r.get("action") == Some(&json!("hijack-dns"))));

        // fakeip persistence (problem 2): cache_file must be on with
        // store_fakeip so restarting the core doesn't reset the 198.18.x.x
        // mapping table (the "brief disconnect after restart" bug).
        let experimental = &built.value["experimental"];
        assert_eq!(experimental["cache_file"]["enabled"], true);
        assert_eq!(experimental["cache_file"]["store_fakeip"], true);
    }

    #[test]
    fn tun_ipv6_flag_adds_the_v6_tun_address() {
        let nodes = vec![sample_ss()];
        let base = |ipv6: bool| BuildOptions {
            mixed_port: 2080,
            allow_lan: false,
            api_port: 19090,
            extra_inbounds: vec![],
            api_secret: "test".into(),
            current_node_id: None,
            log_level: "info".into(),
            rules: vec![],
            rule_sets: vec![],
            pools: vec![],
            chains: vec![],
            tun_enabled: true,
            tun_stack: "mixed".into(),
            dns: DnsSettings::default(),
            outbound_mode: OutboundMode::Rule,
            route_final: "proxy".into(),
            auto_select: crate::domain::AutoSelectMode::Off,
            probe_url: "https://www.gstatic.com/generate_204".into(),
            find_process: true,
            tun_ipv6: ipv6,
            block_quic: false,
            bypass_lan: false,
            tun_interface_name: None,
        };

        let v4_only = build_singbox_config(&nodes, &base(false)).unwrap();
        let addrs = v4_only.value["inbounds"][1]["address"].as_array().unwrap();
        assert_eq!(addrs.len(), 1, "default must be IPv4-only: {addrs:?}");
        assert_eq!(addrs[0], "172.19.0.1/30");

        let dual_stack = build_singbox_config(&nodes, &base(true)).unwrap();
        let addrs = dual_stack.value["inbounds"][1]["address"]
            .as_array()
            .unwrap();
        assert_eq!(addrs.len(), 2, "opt-in must add the v6 address: {addrs:?}");
        assert!(addrs.iter().any(|a| a == "fdfe:dcba:9876::1/126"));
    }

    #[test]
    fn cache_file_persists_fakeip_even_without_tun() {
        // The fix targets sing-box restarts in general, not just TUN — a
        // non-TUN (system proxy / mixed-only) run also loses its fakeip table
        // on restart without cache_file.
        let nodes = vec![sample_ss()];
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        assert_eq!(built.value["experimental"]["cache_file"]["enabled"], true);
        assert_eq!(
            built.value["experimental"]["cache_file"]["store_fakeip"],
            true
        );
    }

    #[test]
    fn block_quic_injects_a_protocol_reject_rule_after_sniff() {
        let nodes = vec![sample_ss()];
        let base = |block_quic: bool| BuildOptions {
            mixed_port: 2080,
            allow_lan: false,
            api_port: 19090,
            extra_inbounds: vec![],
            api_secret: "test".into(),
            current_node_id: None,
            log_level: "info".into(),
            rules: vec![],
            rule_sets: vec![],
            pools: vec![],
            chains: vec![],
            tun_enabled: false,
            tun_stack: "mixed".into(),
            dns: DnsSettings::default(),
            outbound_mode: OutboundMode::Rule,
            route_final: "proxy".into(),
            auto_select: crate::domain::AutoSelectMode::Off,
            probe_url: "https://www.gstatic.com/generate_204".into(),
            find_process: true,
            tun_ipv6: false,
            block_quic,
            bypass_lan: false,
            tun_interface_name: None,
        };

        let off = build_singbox_config(&nodes, &base(false)).unwrap();
        let rules = off.value["route"]["rules"].as_array().unwrap();
        assert!(
            !rules
                .iter()
                .any(|r| r.get("protocol") == Some(&json!("quic"))),
            "block_quic off must not add a quic rule"
        );

        let on = build_singbox_config(&nodes, &base(true)).unwrap();
        let rules = on.value["route"]["rules"].as_array().unwrap();
        let sniff_idx = rules
            .iter()
            .position(|r| r.get("action") == Some(&json!("sniff")))
            .expect("sniff rule present");
        let quic_idx = rules
            .iter()
            .position(|r| {
                r.get("protocol") == Some(&json!("quic"))
                    && r.get("action") == Some(&json!("reject"))
            })
            .expect("quic reject rule present when block_quic is on");
        assert!(
            quic_idx > sniff_idx,
            "quic reject must come after sniff (needs the detected protocol)"
        );
    }

    #[test]
    fn bypass_lan_appends_direct_rules_after_rule_sets() {
        let nodes = vec![sample_ss()];
        let mut set = RuleSet::new_user(
            "直连集",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "example.com".into(),
                RuleTarget::Direct,
                10,
            )],
        );
        set.strategy = RuleSetStrategy::Direct;
        let base = |bypass_lan: bool| BuildOptions {
            mixed_port: 2080,
            allow_lan: false,
            api_port: 19090,
            extra_inbounds: vec![],
            api_secret: "test".into(),
            current_node_id: None,
            log_level: "info".into(),
            rules: vec![],
            rule_sets: vec![set.clone()],
            pools: vec![],
            chains: vec![],
            tun_enabled: false,
            tun_stack: "mixed".into(),
            dns: DnsSettings::default(),
            outbound_mode: OutboundMode::Rule,
            route_final: "proxy".into(),
            auto_select: crate::domain::AutoSelectMode::Off,
            probe_url: "https://www.gstatic.com/generate_204".into(),
            find_process: true,
            tun_ipv6: false,
            block_quic: false,
            bypass_lan,
            tun_interface_name: None,
        };

        let off = build_singbox_config(&nodes, &base(false)).unwrap();
        let rules = off.value["route"]["rules"].as_array().unwrap();
        assert!(
            !rules
                .iter()
                .any(|r| r.get("ip_is_private") == Some(&json!(true))),
            "bypass_lan off must not add LAN rules"
        );

        let on = build_singbox_config(&nodes, &base(true)).unwrap();
        let rules = on.value["route"]["rules"].as_array().unwrap();
        let set_idx = rules
            .iter()
            .position(|r| r.get("rule_set").is_some())
            .expect("rule set route present");
        let domain_idx = rules
            .iter()
            .position(|r| {
                r.get("domain_suffix")
                    .is_some_and(|v| v == &json!(["local", "localhost"]))
            })
            .expect("localhost bypass rule present");
        let private_idx = rules
            .iter()
            .position(|r| r.get("ip_is_private") == Some(&json!(true)))
            .expect("private CIDR bypass rule present");
        assert!(
            domain_idx > set_idx && private_idx > set_idx,
            "bypass rules append after the rule sets (DNS split + user rules win)"
        );
        for idx in [domain_idx, private_idx] {
            assert_eq!(rules[idx]["action"], json!("route"));
            assert_eq!(rules[idx]["outbound"], json!("direct"));
        }
        // Global mode proxies everything by choice: no bypass there.
        let mut global = base(true);
        global.outbound_mode = OutboundMode::Global;
        let built = build_singbox_config(&nodes, &global).unwrap();
        let rules = built.value["route"]["rules"].as_array().unwrap();
        assert!(!rules
            .iter()
            .any(|r| r.get("ip_is_private") == Some(&json!(true))));
    }

    #[test]
    fn maps_vmess_ws() {
        let mut headers = BTreeMap::new();
        headers.insert("Host".into(), "cdn.example.com".into());
        let node = ProxyNode {
            id: "vmessid000000001".into(),
            name: "VM".into(),
            protocol: Protocol::Vmess,
            server: "vm.example.com".into(),
            port: 443,
            tls: Some(TlsConfig {
                enabled: true,
                server_name: Some("cdn.example.com".into()),
                insecure: Some(true),
                alpn: None,
                utls_fingerprint: None,
                reality_public_key: None,
                reality_short_id: None,
            }),
            transport: Some(Transport::Ws {
                path: Some("/ray".into()),
                headers: Some(headers),
                max_early_data: None,
            }),
            udp: None,
            config: ProtocolConfig::Vmess {
                uuid: "11111111-1111-1111-1111-111111111111".into(),
                alter_id: 0,
                security: "auto".into(),
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        };
        let (_, ob, _) = node_to_outbound(&node).unwrap();
        assert_eq!(ob["type"], "vmess");
        assert_eq!(ob["transport"]["type"], "ws");
    }

    #[test]
    fn empty_nodes_err() {
        let err = build_singbox_config(
            &[],
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "x".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no nodes"));
    }

    #[test]
    fn outbound_mode_direct_final() {
        let nodes = vec![sample_ss()];
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Direct,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        assert_eq!(built.value["route"]["final"], "direct");
    }

    #[test]
    fn rule_mode_honors_route_final() {
        let nodes = vec![sample_ss()];
        for (rf, expect) in [("direct", "direct"), ("block", "block"), ("proxy", "proxy")] {
            let built = build_singbox_config(
                &nodes,
                &BuildOptions {
                    mixed_port: 2080,
                    allow_lan: false,
                    api_port: 19090,
                    extra_inbounds: vec![],
                    api_secret: "test".into(),
                    current_node_id: None,
                    log_level: "info".into(),
                    rules: vec![],
                    rule_sets: vec![],
                    pools: vec![],
                    chains: vec![],
                    tun_enabled: false,
                    tun_stack: "mixed".into(),
                    dns: DnsSettings::default(),
                    outbound_mode: OutboundMode::Rule,
                    route_final: rf.into(),
                    auto_select: crate::domain::AutoSelectMode::Off,
                    probe_url: "https://www.gstatic.com/generate_204".into(),
                    find_process: true,
                    tun_ipv6: false,
                    block_quic: false,
                    bypass_lan: false,
                    tun_interface_name: None,
                },
            )
            .unwrap();
            assert_eq!(built.value["route"]["final"], expect, "rf={rf}");
        }
    }

    #[test]
    fn outbound_mode_global_skips_user_rules() {
        use crate::domain::{Rule, RuleTarget, RuleType};
        let nodes = vec![sample_ss()];
        let built = build_singbox_config(
            &nodes,
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![Rule::new(
                    RuleType::DomainSuffix,
                    "example.com".into(),
                    RuleTarget::Direct,
                    10,
                )],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Global,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        assert_eq!(built.value["route"]["final"], "proxy");
        let rules = built.value["route"]["rules"].as_array().unwrap();
        // only sniff (+ maybe dns hijack from dns settings)
        assert!(!rules.iter().any(|r| r.get("domain_suffix").is_some()));
    }

    #[test]
    fn rule_pin_node_routes_to_node_tag() {
        use crate::domain::{Rule, RuleTarget, RuleType};
        let node = sample_ss();
        let tag = outbound_tag(&node);
        let mut rule = Rule::new(
            RuleType::DomainSuffix,
            "chatgpt.com".into(),
            RuleTarget::Node,
            10,
        );
        rule.node_id = Some(node.id.clone());
        rule.node_name = Some(node.name.clone());
        let built = build_singbox_config(
            &[node],
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![rule],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        let rules = built.value["route"]["rules"].as_array().unwrap();
        let pinned = rules
            .iter()
            .find(|r| r.get("domain_suffix").is_some())
            .expect("pin rule");
        assert_eq!(pinned["outbound"], tag);
    }

    #[test]
    fn rule_pin_stale_node_falls_back_to_proxy() {
        use crate::domain::{Rule, RuleTarget, RuleType};
        let node = sample_ss();
        let mut rule = Rule::new(
            RuleType::DomainSuffix,
            "openai.com".into(),
            RuleTarget::Node,
            10,
        );
        rule.node_id = Some("deadbeefdeadbeef".into());
        rule.node_name = Some("gone".into());
        let built = build_singbox_config(
            &[node],
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![rule],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        let rules = built.value["route"]["rules"].as_array().unwrap();
        let pinned = rules
            .iter()
            .find(|r| r.get("domain_suffix").is_some())
            .expect("stale pin rule");
        assert_eq!(pinned["outbound"], "proxy");
    }

    #[test]
    fn smart_rule_builds_filtered_selector() {
        use crate::domain::{Rule, RuleTarget, RuleType};
        let mut hk = sample_ss();
        hk.id = "aaaaaaaaaaaaaaaa".into();
        hk.name = "香港 01".into();
        let mut sg = sample_ss();
        sg.id = "bbbbbbbbbbbbbbbb".into();
        sg.name = "新加坡 01".into();
        let mut rule = Rule::new(
            RuleType::DomainSuffix,
            "chatgpt.com".into(),
            RuleTarget::Smart,
            10,
        );
        rule.smart_exclude = vec!["香港".into()];
        let built = build_singbox_config(
            &[hk, sg.clone()],
            &BuildOptions {
                mixed_port: 2080,
                allow_lan: false,
                api_port: 19090,
                extra_inbounds: vec![],
                api_secret: "test".into(),
                current_node_id: None,
                log_level: "info".into(),
                rules: vec![rule.clone()],
                rule_sets: vec![],
                pools: vec![],
                chains: vec![],
                tun_enabled: false,
                tun_stack: "mixed".into(),
                dns: DnsSettings::default(),
                outbound_mode: OutboundMode::Rule,
                route_final: "proxy".into(),
                auto_select: crate::domain::AutoSelectMode::Off,
                probe_url: "https://www.gstatic.com/generate_204".into(),
                find_process: true,
                tun_ipv6: false,
                block_quic: false,
                bypass_lan: false,
                tun_interface_name: None,
            },
        )
        .unwrap();
        let group = rule.smart_outbound_tag();
        let outs = built.value["outbounds"].as_array().unwrap();
        let sel = outs
            .iter()
            .find(|o| o.get("tag") == Some(&json!(group)))
            .expect("smart selector");
        let pool = sel["outbounds"].as_array().unwrap();
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0], json!(outbound_tag(&sg)));
        let rules = built.value["route"]["rules"].as_array().unwrap();
        let routed = rules
            .iter()
            .find(|r| r.get("domain_suffix").is_some())
            .expect("smart route");
        assert_eq!(routed["outbound"], group);
    }

    #[test]
    fn inline_rule_set_converts_domain_and_suffix_to_punycode_but_not_keyword() {
        let rules = vec![
            Rule::new(RuleType::Domain, "中文.com".into(), RuleTarget::Proxy, 0),
            Rule::new(
                RuleType::DomainSuffix,
                "中国.com".into(),
                RuleTarget::Proxy,
                1,
            ),
            Rule::new(RuleType::DomainKeyword, "中文".into(), RuleTarget::Proxy, 2),
        ];
        let headless = build_headless_rules(&rules).expect("non-empty headless body");
        let domain = headless
            .iter()
            .find(|r| r.get("domain").is_some())
            .expect("domain bucket");
        assert_eq!(domain["domain"], json!(["xn--fiq228c.com"]));
        let suffix = headless
            .iter()
            .find(|r| r.get("domain_suffix").is_some())
            .expect("domain_suffix bucket");
        assert_eq!(suffix["domain_suffix"], json!(["xn--fiqs8s.com"]));
        let keyword = headless
            .iter()
            .find(|r| r.get("domain_keyword").is_some())
            .expect("domain_keyword bucket");
        assert_eq!(keyword["domain_keyword"], json!(["中文"]));
    }

    #[test]
    fn legacy_route_rules_convert_domain_and_suffix_to_punycode_but_not_keyword() {
        let rules = vec![
            Rule::new(RuleType::Domain, "中文.com".into(), RuleTarget::Proxy, 0),
            Rule::new(RuleType::DomainKeyword, "中文".into(), RuleTarget::Proxy, 1),
        ];
        let out = build_route_rules(&rules, &[], &["direct".into()], &Default::default());
        assert_eq!(out[0]["domain"], json!(["xn--fiq228c.com"]));
        assert_eq!(out[1]["domain_keyword"], json!(["中文"]));
    }

    // ---- Proxy Chain: pool selectors + detour chains ---------------------

    fn sample_node(id: &str, name: &str) -> ProxyNode {
        let mut n = sample_ss();
        n.id = id.into();
        n.name = name.into();
        n
    }

    #[test]
    fn explicit_pool_selector_lists_members_in_configured_order() {
        use crate::domain::{NodePool, PoolMode};
        let nodes = vec![sample_node("n1", "HK-1"), sample_node("n2", "HK-2")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let pool = NodePool::new(
            "手动池",
            PoolMode::Explicit {
                node_ids: vec!["n2".into(), "n1".into()],
            },
        );
        let selectors = build_pool_selectors(&[pool.clone()], &nodes, &tags);
        assert_eq!(selectors.len(), 1);
        // Explicit mode is a deliberate user-picked order — must not get
        // silently re-sorted by latency like Keyword mode does.
        assert_eq!(
            selectors[0]["outbounds"],
            json!([outbound_tag(&nodes[1]), outbound_tag(&nodes[0])])
        );
        assert_eq!(selectors[0]["tag"], pool.outbound_tag());
    }

    #[test]
    fn keyword_pool_selector_sorts_by_latency_like_smart_rules() {
        use crate::domain::{NodePool, PoolMode};
        let mut fast = sample_node("n1", "JP-fast");
        fast.latency_ms = Some(50);
        let mut slow = sample_node("n2", "JP-slow");
        slow.latency_ms = Some(500);
        let nodes = vec![slow.clone(), fast.clone()];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let pool = NodePool::new(
            "日本",
            PoolMode::Keyword {
                include: vec!["JP".into()],
                exclude: vec![],
            },
        );
        let selectors = build_pool_selectors(&[pool], &nodes, &tags);
        assert_eq!(
            selectors[0]["outbounds"],
            json!([outbound_tag(&fast), outbound_tag(&slow)]),
            "lower latency must sort first, same as filter_pool_tags for Smart rules"
        );
        assert_eq!(selectors[0]["default"], json!(outbound_tag(&fast)));
    }

    #[test]
    fn pool_with_no_live_members_emits_no_selector() {
        // sing-box rejects a selector with an empty `outbounds` array — a
        // keyword pool matching nothing must vanish, not emit broken JSON.
        use crate::domain::{NodePool, PoolMode};
        let nodes = vec![sample_node("n1", "HK-1")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let pool = NodePool::new(
            "空池",
            PoolMode::Keyword {
                include: vec!["找不到".into()],
                exclude: vec![],
            },
        );
        assert!(build_pool_selectors(&[pool], &nodes, &tags).is_empty());
    }

    #[test]
    fn diag_inbound_selector_and_rule_emit_only_with_chains() {
        // The diagnostics egress (loopback mixed inbound + chain-diag
        // selector + priority inbound rule) is the rule-free way
        // `diagnose_chain` fetches a chosen chain's exit IP. It must exist
        // exactly when at least one chain resolves, and never leak to the
        // LAN.
        use crate::domain::{ChainHop, ProxyChain};
        let opts = |chains: Vec<ProxyChain>| BuildOptions {
            mixed_port: 2080,
            allow_lan: false,
            api_port: 19090,
            extra_inbounds: vec![],
            api_secret: "test".into(),
            current_node_id: None,
            log_level: "info".into(),
            rules: vec![],
            rule_sets: vec![],
            pools: vec![],
            chains,
            tun_enabled: false,
            tun_stack: "mixed".into(),
            dns: DnsSettings::default(),
            outbound_mode: OutboundMode::Rule,
            route_final: "proxy".into(),
            auto_select: crate::domain::AutoSelectMode::Off,
            probe_url: "https://www.gstatic.com/generate_204".into(),
            find_process: true,
            tun_ipv6: false,
            block_quic: false,
            bypass_lan: false,
            tun_interface_name: None,
        };
        let nodes = vec![sample_node("n1", "A"), sample_node("n2", "B")];
        let chain = ProxyChain::new(
            "诊断链",
            vec![
                ChainHop::Node {
                    node_id: "n1".into(),
                },
                ChainHop::Node {
                    node_id: "n2".into(),
                },
            ],
        );

        let with = build_singbox_config(&nodes, &opts(vec![chain])).unwrap();
        let diag_in = with.value["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["tag"] == json!("diag-in"))
            .expect("diag inbound emitted");
        assert_eq!(diag_in["listen"], json!("127.0.0.1"), "loopback only");
        assert_eq!(diag_in["listen_port"], json!(26486));
        let selector = with.value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == json!("chain-diag"))
            .expect("chain-diag selector");
        let members = selector["outbounds"].as_array().unwrap();
        assert!(members.len() >= 2, "chain exit tags + direct fallback");
        assert_eq!(members[members.len() - 1], json!("direct"));
        // Right after the sniff rule and ahead of every user rule.
        let first_rule = &with.value["route"]["rules"].as_array().unwrap()[1];
        assert_eq!(first_rule["inbound"], json!(["diag-in"]));
        assert_eq!(first_rule["outbound"], json!("chain-diag"));

        let without = build_singbox_config(&nodes, &opts(vec![])).unwrap();
        assert!(
            !without.value["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["tag"] == json!("diag-in")),
            "no chains -> no diag inbound"
        );
        assert!(
            !without.value["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["tag"] == json!("chain-diag")),
            "no chains -> no diag selector"
        );
    }

    #[test]
    fn chain_routes_to_the_exit_and_each_hop_detours_to_the_previous() {
        // sing-box's `A.detour = B` means "A's server connection is dialed
        // through B, and A remains the exit". For a user-facing chain
        // hop[0] → hop[1] → hop[2] → internet that means: rules point at the
        // LAST hop (the exit the internet sees), and hop[i] carries
        // `detour: hop[i-1]`; hop[0] is dialed directly by the client. The
        // original implementation had this exactly backwards — [US → HK]
        // chains egressed from the US.
        use crate::domain::{ChainHop, ProxyChain};
        let nodes = vec![
            sample_node("n1", "Entry"),
            sample_node("n2", "Middle"),
            sample_node("n3", "Exit"),
        ];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let chain = ProxyChain::new(
            "三跳链",
            vec![
                ChainHop::Node {
                    node_id: "n1".into(),
                },
                ChainHop::Node {
                    node_id: "n2".into(),
                },
                ChainHop::Node {
                    node_id: "n3".into(),
                },
            ],
        );
        let (outbounds, entry_tags) = build_chain_outbounds(&[chain.clone()], &[], &nodes, &tags);
        let entry_tag = entry_tags.get(&chain.id).cloned().expect("chain resolves");

        let by_tag = |t: &str| -> &Value {
            outbounds
                .iter()
                .find(|o| o["tag"] == json!(t))
                .unwrap_or_else(|| panic!("missing outbound {t}"))
        };
        // Route rules must point at the EXIT hop's chain-local tag — the
        // internet sees this node's IP.
        let hop2 = by_tag(&entry_tag);
        let hop2_detour = hop2["detour"]
            .as_str()
            .expect("exit hop detours")
            .to_string();
        let hop1 = by_tag(&hop2_detour);
        let hop1_detour = hop1["detour"]
            .as_str()
            .expect("middle hop detours")
            .to_string();
        let hop0 = by_tag(&hop1_detour);
        // The client-side entry dials out directly — no further detour.
        assert!(
            hop0.get("detour").is_none(),
            "the client-side entry must not detour anywhere — the client dials it directly"
        );
        // The detour chain must follow hop order: 2→1→0.
        assert_ne!(hop2_detour, hop1_detour);
    }

    #[test]
    fn chain_hop_outbounds_never_reuse_the_node_directly_selectable_tag() {
        // A node used inside a chain is often also directly selectable via
        // RuleTarget::Node elsewhere. If the chain clone reused
        // `outbound_tag(node)`, rewriting its `detour` would silently hijack
        // every other rule/pin that references the same node outside the
        // chain — the clone must live under its own chain-local tag.
        use crate::domain::{ChainHop, ProxyChain};
        let nodes = vec![sample_node("n1", "Shared"), sample_node("n2", "Exit")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let chain = ProxyChain::new(
            "共享节点链",
            vec![
                ChainHop::Node {
                    node_id: "n1".into(),
                },
                ChainHop::Node {
                    node_id: "n2".into(),
                },
            ],
        );
        let (outbounds, entry_tags) = build_chain_outbounds(&[chain.clone()], &[], &nodes, &tags);
        let entry_tag = entry_tags[&chain.id].clone();
        assert_ne!(
            entry_tag,
            outbound_tag(&nodes[0]),
            "chain hop clone must not collide with the node's directly-selectable tag"
        );
        // The plain node outbound (used when picked directly) is untouched —
        // no stray `detour` was smuggled onto it.
        let plain = outbounds
            .iter()
            .find(|o| o["tag"] == json!(outbound_tag(&nodes[0])));
        assert!(
            plain.is_none(),
            "build_chain_outbounds only mints chain-local clones, never the plain node outbound"
        );
    }

    #[test]
    fn chain_with_stale_hop_resolves_to_no_entry_tag() {
        // A node removed from a subscription after the chain was built must
        // not silently truncate the chain (which would leak traffic through
        // fewer hops than the user configured) — the whole chain goes dark
        // and callers fall back to the main proxy group instead.
        use crate::domain::{ChainHop, ProxyChain};
        let nodes = vec![sample_node("n1", "Entry")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let chain = ProxyChain::new(
            "断链",
            vec![
                ChainHop::Node {
                    node_id: "n1".into(),
                },
                ChainHop::Node {
                    node_id: "gone".into(),
                },
            ],
        );
        let (outbounds, entry_tags) = build_chain_outbounds(&[chain.clone()], &[], &nodes, &tags);
        assert!(outbounds.is_empty(), "a broken chain must emit nothing");
        assert!(
            !entry_tags.contains_key(&chain.id),
            "a broken chain must not resolve to a partial/misleading entry tag"
        );
    }

    #[test]
    fn chain_with_leading_pool_reuses_the_shared_selector_as_client_entry() {
        // A pool at hop[0] is the client-side entry — the client dials the
        // selected member directly, so the shared pool selector works as-is
        // and the exit node clone detours into it. No expansion needed.
        use crate::domain::{ChainHop, NodePool, PoolMode, ProxyChain};
        let nodes = vec![sample_node("n1", "PoolMember"), sample_node("n2", "Exit")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let pool = NodePool::new(
            "落地池",
            PoolMode::Explicit {
                node_ids: vec!["n1".into()],
            },
        );
        let chain = ProxyChain::new(
            "过池链",
            vec![
                ChainHop::Pool {
                    pool_id: pool.id.clone(),
                },
                ChainHop::Node {
                    node_id: "n2".into(),
                },
            ],
        );
        let (chain_outbounds, entry_tags) =
            build_chain_outbounds(&[chain.clone()], &[pool.clone()], &nodes, &tags);
        let entry = entry_tags[&chain.id].clone();
        assert_ne!(
            entry,
            pool.outbound_tag(),
            "route points at the exit node clone, not the entry pool selector"
        );

        let exit_ob = chain_outbounds
            .iter()
            .find(|o| o["tag"] == json!(entry))
            .expect("exit clone emitted");
        assert_eq!(
            exit_ob["detour"],
            json!(pool.outbound_tag()),
            "exit clone dials through the (client-side) pool selector"
        );
        assert!(
            chain_outbounds
                .iter()
                .all(|o| o["type"] != json!("selector")),
            "a hop[0] pool needs no chain-local selector"
        );
    }

    #[test]
    fn chain_with_trailing_pool_expands_members_into_detour_clones() {
        // A pool at i ≥ 1 is dialed through hop[i-1] — the shared selector's
        // detour-less members can't express that, so the chain expands the
        // pool into per-member clones (each detouring into the previous
        // hop) under a chain-local selector, and rules route to that
        // selector (its selected member is the exit).
        use crate::domain::{ChainHop, NodePool, PoolMode, ProxyChain};
        let nodes = vec![sample_node("n1", "Entry"), sample_node("n2", "PoolMember")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let pool = NodePool::new(
            "出口池",
            PoolMode::Explicit {
                node_ids: vec!["n2".into()],
            },
        );
        let chain = ProxyChain::new(
            "入口过池",
            vec![
                ChainHop::Node {
                    node_id: "n1".into(),
                },
                ChainHop::Pool {
                    pool_id: pool.id.clone(),
                },
            ],
        );
        let (chain_outbounds, entry_tags) =
            build_chain_outbounds(&[chain.clone()], &[pool.clone()], &nodes, &tags);
        let entry = entry_tags[&chain.id].clone();
        assert_ne!(
            entry,
            pool.outbound_tag(),
            "exit pool gets a chain-local selector"
        );

        let selector = chain_outbounds
            .iter()
            .find(|o| o["tag"] == json!(entry))
            .expect("chain-local pool selector emitted");
        assert_eq!(selector["type"], json!("selector"));
        let members = selector["outbounds"].as_array().unwrap();
        assert_eq!(members.len(), 1, "one clone per pool member");

        // The member clone detours into the entry hop, and is itself a
        // clone — the member's directly-selectable tag stays untouched.
        let member_tag = members[0].as_str().unwrap().to_string();
        assert_ne!(member_tag, outbound_tag(&nodes[1]));
        let member_ob = chain_outbounds
            .iter()
            .find(|o| o["tag"] == json!(member_tag))
            .expect("member clone emitted");
        let entry_tag_ref = member_ob["detour"]
            .as_str()
            .expect("member clone detours")
            .to_string();
        assert_ne!(
            entry_tag_ref,
            outbound_tag(&nodes[0]),
            "entry hop is a chain-local clone"
        );

        let entry_ob = chain_outbounds
            .iter()
            .find(|o| o["tag"] == json!(entry_tag_ref))
            .expect("entry hop emitted");
        assert_eq!(
            entry_ob.get("detour"),
            None,
            "the client-side entry dials out directly"
        );

        // sing-box requires detour targets to be defined before their
        // referrers — the entry clone must precede the member clone.
        let entry_pos = chain_outbounds
            .iter()
            .position(|o| o["tag"] == json!(entry_tag_ref))
            .unwrap();
        let member_pos = chain_outbounds
            .iter()
            .position(|o| o["tag"] == json!(member_tag))
            .unwrap();
        assert!(entry_pos < member_pos, "detour target precedes referrer");
    }

    #[test]
    fn rule_with_chain_target_resolves_to_chain_entry_tag() {
        use crate::domain::{ChainHop, ProxyChain};
        let nodes = vec![sample_node("n1", "A"), sample_node("n2", "B")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let chain = ProxyChain::new(
            "规则链",
            vec![
                ChainHop::Node {
                    node_id: "n1".into(),
                },
                ChainHop::Node {
                    node_id: "n2".into(),
                },
            ],
        );
        let (_, entry_tags) = build_chain_outbounds(&[chain.clone()], &[], &nodes, &tags);
        let mut rule = Rule::new(RuleType::Domain, "example.com".into(), RuleTarget::Chain, 0);
        rule.chain_id = Some(chain.id.clone());
        let resolved = resolve_rule_outbound(&rule, &nodes, &tags, &entry_tags);
        assert_eq!(resolved, entry_tags[&chain.id]);
    }

    #[test]
    fn rule_with_dangling_chain_id_falls_back_to_proxy() {
        // Chain deleted after the rule was saved — same stale-pin contract
        // as RuleTarget::Node with a removed node_id.
        let nodes = vec![sample_node("n1", "A")];
        let tags: Vec<String> = nodes.iter().map(outbound_tag).collect();
        let mut rule = Rule::new(RuleType::Domain, "example.com".into(), RuleTarget::Chain, 0);
        rule.chain_id = Some("chain-does-not-exist".into());
        let resolved =
            resolve_rule_outbound(&rule, &nodes, &tags, &std::collections::HashMap::new());
        assert_eq!(resolved, "proxy");
    }
}
