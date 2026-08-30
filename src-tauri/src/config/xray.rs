//! Build Xray JSON from normalized [`ProxyNode`]s — the Xray counterpart of
//! `config/builder.rs` (sing-box). The two generators are fully independent
//! (same pattern as v2rayN's `CoreConfigSingboxService` vs
//! `CoreConfigV2rayService`) and only share the input domain model.
//!
//! Key semantic differences vs sing-box, by design:
//! - No `selector` outbound: Xray has no runtime node-switch API. The main
//!   target is the selected node's own outbound; switching nodes regenerates
//!   the config and restarts the core.
//! - `auto_select=kernel` maps to a `routing.balancers` entry with the
//!   `leastPing` strategy plus a top-level `observatory` (v2rayN scheme).
//! - Builtin remote rule sets are expressed as `geosite:` / `geoip:`
//!   matchers (requires geosite.dat / geoip.dat via XRAY_LOCATION_ASSET).
//!   User-added remote `.srs` sets are sing-box-only and skipped here.
//! - Per-connection observability does not exist; traffic totals come from
//!   the `metrics` module (`/debug/vars`) configured below.

use crate::config::builder::{
    effective_route_rules, filter_pool_tags, outbound_tag, resolve_selected_tag,
    rule_set_is_empty_for_config, smart_pool_tags, BuildOptions, BuiltConfig,
};
use crate::config::punycode::to_ascii_domain;
use crate::core::kind::CoreKind;
use crate::domain::{
    DnsAction, DnsRule, DomainMatcher, OutboundMode, ProtocolConfig, ProxyNode, RuleSet,
    RuleSetStrategy, RuleTarget, RuleType, Transport, DOMESTIC_DNS_POOL, REMOTE_DNS_POOL,
};
use crate::error::{AppError, AppResult};
use serde_json::{json, Map, Value};

/// Tag of the leastPing balancer used when `auto_select=kernel`.
const BALANCER_TAG: &str = "proxy-balancer";
/// `dns.tag` — inboundTag carried by queries of untagged DNS servers.
const DNS_MODULE_TAG: &str = "dns-module";
/// Tag of the direct domestic resolver server (matched via inboundTag).
const DIRECT_DNS_TAG: &str = "direct-dns";
/// All node outbound tags share this prefix (balancer/observatory selectors
/// match by prefix).
const NODE_TAG_PREFIX: &str = "node-";

pub fn build_xray_config(nodes: &[ProxyNode], opts: &BuildOptions) -> AppResult<BuiltConfig> {
    let mut supported: Vec<ProxyNode> = nodes
        .iter()
        .filter(|n| CoreKind::Xray.supports(n.protocol))
        .cloned()
        .collect();
    if supported.is_empty() {
        return Err(AppError::Config(
            "no Xray-compatible nodes (supports vmess/vless/shadowsocks/trojan/hysteria2(no obfs)/socks5/http/wireguard)".into(),
        ));
    }

    // Same tag space as sing-box (`node-<id[..16]>`): a stored id collision
    // would emit duplicate outbound tags and Xray refuses the config.
    let renamed = ProxyNode::ensure_unique_ids(supported.iter_mut());
    if renamed > 0 {
        crate::app_log::warn(
            "xray_config",
            format!("{renamed} 个节点 id 重复，已在生成时改写 tag 以避免校验失败"),
        );
    }

    let mut tags = Vec::new();
    let mut outbounds = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for node in &supported {
        match node_to_xray_outbound(node) {
            Ok(outbound) => {
                tags.push(outbound_tag(node));
                outbounds.push(outbound);
            }
            Err(e) => skipped.push(format!("{}: {e}", node.name)),
        }
    }
    if outbounds.is_empty() {
        return Err(AppError::Config(format!(
            "failed to map any node to an Xray outbound: {}",
            skipped.join("; ")
        )));
    }
    for reason in &skipped {
        crate::app_log::warn("xray_config", format!("skipped node: {reason}"));
    }

    let selected_tag = resolve_selected_tag(&supported, &tags, opts.current_node_id.as_deref());
    // kernel auto-select → balancer; otherwise the selected node's outbound.
    let main_target = if opts.auto_select.is_kernel() {
        BALANCER_TAG.to_string()
    } else {
        selected_tag.clone()
    };

    // Filter-strategy sets route through a keyword-filtered node pool. Xray
    // has no selector outbound, so each pool becomes a balancer over the
    // exact tags of its member nodes (leastPing; the shared observatory
    // probes the node- prefix so members have latency data). Empty pools
    // fall back to the main target.
    let mut filter_balancers = Vec::new();
    let mut filter_balancer_tags: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for set in opts
        .rule_sets
        .iter()
        .filter(|s| s.enabled && s.remote.is_none() && s.strategy == RuleSetStrategy::Filter)
    {
        if rule_set_is_empty_for_config(set) {
            continue;
        }
        let pool = filter_pool_tags(&set.smart_include, &set.smart_exclude, &supported, &tags);
        if pool.is_empty() {
            continue;
        }
        let balancer_tag = format!("filter-{}", &set.id[..set.id.len().min(12)]);
        filter_balancers.push(json!({
            "tag": balancer_tag,
            "selector": pool,
            "strategy": { "type": "leastPing" },
        }));
        filter_balancer_tags.insert(set.id.clone(), balancer_tag);
    }

    let effective_rules = effective_route_rules(&opts.rule_sets, &opts.rules);

    // Per-RULE smart pools (target=Smart with per-rule include/exclude
    // keywords): sing-box gives each a `smart-<id>` selector; under Xray each
    // becomes a leastPing balancer over the exact pool tags. Empty pools
    // route to the main target.
    let mut smart_balancers = Vec::new();
    let mut smart_balancer_tags: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for rule in effective_rules
        .iter()
        .filter(|r| r.enabled && r.target == RuleTarget::Smart && !r.payload.trim().is_empty())
    {
        let pool = smart_pool_tags(rule, &supported, &tags);
        if pool.is_empty() {
            continue;
        }
        let balancer_tag = rule.smart_outbound_tag();
        smart_balancers.push(json!({
            "tag": balancer_tag,
            "selector": pool,
            "strategy": { "type": "leastPing" },
        }));
        smart_balancer_tags.insert(rule.id.clone(), balancer_tag);
    }

    let (route_rules, dns_egress_rules) = build_routing(
        opts,
        &supported,
        &tags,
        &effective_rules,
        &main_target,
        &filter_balancer_tags,
        &smart_balancer_tags,
    );

    let dns = build_dns(opts, &opts.rule_sets, &effective_rules);
    let inbounds = build_inbounds(opts);

    outbounds.push(json!({ "tag": "direct", "protocol": "freedom" }));
    outbounds.push(json!({ "tag": "block", "protocol": "blackhole" }));
    if opts.tun_enabled {
        // Hand raw UDP/53 traffic arriving through the tun inbound to the
        // built-in DNS module (which applies the split above).
        outbounds.push(json!({ "tag": "dns-out", "protocol": "dns" }));
    }

    let mut config = Map::new();
    config.insert(
        "log".into(),
        json!({ "loglevel": xray_log_level(&opts.log_level) }),
    );
    config.insert("dns".into(), dns);
    config.insert("inbounds".into(), Value::Array(inbounds));
    config.insert("outbounds".into(), Value::Array(outbounds));
    let mut routing = Map::new();
    routing.insert("domainStrategy".into(), json!("IPIfNonMatch"));
    let mut rules = Vec::new();
    if opts.tun_enabled {
        rules.extend(tun_safety_rules());
        rules.push(json!({
            "type": "field", "inboundTag": ["tun-in"], "network": "udp", "port": "53",
            "outboundTag": "dns-out"
        }));
    }
    rules.extend(dns_egress_rules);
    if opts.block_quic {
        // Xray has no sniff-based QUIC rejection; blocking UDP/443 achieves
        // the same "make browsers fall back to TCP" effect.
        rules.push(json!({
            "type": "field", "network": "udp", "port": "443", "outboundTag": "block"
        }));
    }
    rules.extend(route_rules);
    if opts.bypass_lan && opts.outbound_mode == OutboundMode::Rule {
        rules.push(json!({
            "type": "field",
            "domain": ["domain:local", "domain:localhost"],
            "outboundTag": "direct"
        }));
        rules.push(json!({
            "type": "field", "ip": ["geoip:private"], "outboundTag": "direct"
        }));
    }
    // Final rule: Rule mode honors route_final; Global/Direct force the mode.
    let final_outbound = match opts.outbound_mode {
        OutboundMode::Rule => match opts.normalized_route_final() {
            "direct" => "direct".to_string(),
            "block" => "block".to_string(),
            _ => main_target.clone(),
        },
        OutboundMode::Global => main_target.clone(),
        OutboundMode::Direct => "direct".to_string(),
    };
    rules.push(json!({
        "type": "field", "network": "tcp,udp", "outboundTag": final_outbound
    }));

    // Under kernel auto-select the main target IS the balancer tag — which is
    // not an outbound. Every rule built above that resolved to the main
    // target (DNS egress, builtin remote sets, proxy-target user rules, the
    // final rule) must reference it via `balancerTag` or the dispatcher
    // rejects the connection ("non existing outTag"). One choke-point pass
    // fixes them all; `xray run -test` does NOT validate tag references, so
    // this class of bug is only caught here and by unit tests.
    if opts.auto_select.is_kernel() {
        for rule in rules.iter_mut() {
            if rule.get("outboundTag").and_then(Value::as_str) == Some(BALANCER_TAG) {
                if let Some(obj) = rule.as_object_mut() {
                    obj.remove("outboundTag");
                    obj.insert("balancerTag".into(), json!(BALANCER_TAG));
                }
            }
        }
    }
    routing.insert("rules".into(), Value::Array(rules));
    let mut balancers = Vec::new();
    if opts.auto_select.is_kernel() {
        balancers.push(json!({
            "tag": BALANCER_TAG,
            "selector": [NODE_TAG_PREFIX],
            "strategy": { "type": "leastPing" },
        }));
    }
    balancers.extend(filter_balancers);
    balancers.extend(smart_balancers);
    if !balancers.is_empty() {
        routing.insert("balancers".into(), Value::Array(balancers));
    }
    config.insert("routing".into(), Value::Object(routing));
    // leastPing needs probe data: keep the observatory up for kernel
    // auto-select and for any Filter-pool / per-rule smart-pool balancer.
    if opts.auto_select.is_kernel()
        || !filter_balancer_tags.is_empty()
        || !smart_balancer_tags.is_empty()
    {
        let probe_url = if opts.probe_url.trim().is_empty() {
            "https://www.gstatic.com/generate_204"
        } else {
            opts.probe_url.trim()
        };
        config.insert(
            "observatory".into(),
            json!({
                "subjectSelector": [NODE_TAG_PREFIX],
                "probeURL": probe_url,
                "probeInterval": "3m",
                "enableConcurrency": true,
            }),
        );
    }
    // Traffic stats: polled from /debug/vars by api::xray_metrics.
    config.insert("stats".into(), json!({}));
    config.insert(
        "policy".into(),
        json!({
            "system": {
                "statsOutboundUplink": true,
                "statsOutboundDownlink": true,
            }
        }),
    );
    config.insert(
        "metrics".into(),
        json!({ "listen": format!("127.0.0.1:{}", opts.api_port) }),
    );

    Ok(BuiltConfig {
        value: Value::Object(config),
        outbound_tags: tags,
        selected_tag,
    })
}

fn xray_log_level(level: &str) -> &'static str {
    match level.to_ascii_lowercase().as_str() {
        "trace" | "debug" => "debug",
        "info" | "" => "info",
        "warn" | "warning" => "warning",
        "error" | "fatal" | "panic" => "error",
        _ => "warning",
    }
}

/// Inbound tag prefix for sidecar per-node listeners: `in-sc-<port>`.
pub const SIDECAR_INBOUND_PREFIX: &str = "in-sc";

/// Minimal companion config for the Xray sidecar process
/// (`settings.xray_sidecar_*`, sing-box main mode only).
///
/// One loopback `mixed` inbound per delegated node + that node's normal
/// outbound, with a 1:1 `inboundTag → outboundTag` routing rule. Deliberately
/// emits NO Clash API, TUN, DNS module, stats/metrics, geosite/geoip matchers
/// or user rule sets — the sidecar is a dumb protocol translator behind
/// sing-box, so it starts without geodata and owns no ports besides its
/// per-node loopback inbounds. Outbound tags share the main config's
/// `node-<id[..16]>` space via [`outbound_tag`], and the sing-box side dials
/// each node through its dedicated inbound port (see
/// [`crate::config::builder::SidecarPlan`]).
///
/// Entries must already be Xray-supported (`CoreKind::Xray.supports_node`) —
/// the caller computes the delegation plan and falls back to native sing-box
/// outbounds for anything the sidecar can't speak.
pub fn build_xray_sidecar_config(entries: &[(ProxyNode, u16)]) -> AppResult<BuiltConfig> {
    if entries.is_empty() {
        return Err(AppError::Config(
            "xray sidecar plan is empty; nothing to delegate".into(),
        ));
    }

    let mut inbounds = Vec::new();
    let mut outbounds = Vec::new();
    let mut rules = Vec::new();
    let mut tags = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for (node, port) in entries {
        let outbound = match node_to_xray_outbound(node) {
            Ok(outbound) => outbound,
            Err(e) => {
                skipped.push(format!("{}: {e}", node.name));
                continue;
            }
        };
        let tag = outbound_tag(node);
        let inbound_tag = format!("{SIDECAR_INBOUND_PREFIX}-{port}");
        inbounds.push(json!({
            "tag": inbound_tag,
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "mixed",
            "settings": { "auth": "noauth", "udp": true },
            "sniffing": sniffing(false),
        }));
        outbounds.push(outbound);
        // 1:1 dispatch: whatever sing-box hands to this inbound egresses
        // through exactly this node's outbound.
        rules.push(json!({
            "type": "field",
            "inboundTag": [inbound_tag],
            "outboundTag": tag,
        }));
        tags.push(tag);
    }
    for reason in &skipped {
        crate::app_log::warn("xray_sidecar", format!("skipped node: {reason}"));
    }
    if tags.is_empty() {
        return Err(AppError::Config(format!(
            "failed to map any delegated node to an Xray outbound: {}",
            skipped.join("; ")
        )));
    }

    // Safety net for traffic that somehow misses a per-inbound rule (there
    // is no selector API, so the sidecar's "current node" is always the
    // first delegated entry).
    let selected_tag = tags[0].clone();
    rules.push(json!({
        "type": "field",
        "network": "tcp,udp",
        "outboundTag": selected_tag.clone(),
    }));

    let mut config = Map::new();
    config.insert("log".into(), json!({ "loglevel": "warning" }));
    config.insert("inbounds".into(), Value::Array(inbounds));
    config.insert("outbounds".into(), Value::Array(outbounds));
    let mut routing = Map::new();
    routing.insert("domainStrategy".into(), json!("AsIs"));
    routing.insert("rules".into(), Value::Array(rules));
    config.insert("routing".into(), Value::Object(routing));

    Ok(BuiltConfig {
        value: Value::Object(config),
        outbound_tags: tags,
        selected_tag,
    })
}

// —— inbounds ——

fn sniffing(fake_dns: bool) -> Value {
    let mut dest_override = vec!["http", "tls"];
    if fake_dns {
        dest_override.push("fakedns");
    }
    json!({
        "enabled": true,
        "destOverride": dest_override,
    })
}

fn build_inbounds(opts: &BuildOptions) -> Vec<Value> {
    let fake_dns = opts.tun_enabled && opts.dns.fake_ip.enabled;
    let mut inbounds = vec![json!({
        "tag": "mixed-in",
        "listen": if opts.allow_lan { "0.0.0.0" } else { "127.0.0.1" },
        "port": opts.mixed_port,
        "protocol": "mixed",
        "settings": { "auth": "noauth", "udp": true },
        "sniffing": sniffing(fake_dns),
    })];
    for inb in &opts.extra_inbounds {
        inbounds.push(json!({
            "tag": format!("in-mixed-{}", inb.port),
            "listen": if inb.allow_lan { "0.0.0.0" } else { "127.0.0.1" },
            "port": inb.port,
            "protocol": "mixed",
            "settings": { "auth": "noauth", "udp": true },
            "sniffing": sniffing(fake_dns),
        }));
    }
    if opts.tun_enabled {
        let mut gateway = vec!["172.18.0.1/30"];
        if opts.tun_ipv6 {
            gateway.push("fdfe:dcba:9876::1/126");
        }
        // macOS requires a name that parses as `utunN` (probed by the
        // caller); other platforms accept an arbitrary interface name.
        let tun_name = opts.tun_interface_name.as_deref().unwrap_or("satelite_tun");
        inbounds.push(json!({
            "tag": "tun-in",
            "protocol": "tun",
            "settings": {
                "name": tun_name,
                "MTU": 9000,
                "gateway": gateway,
                "autoSystemRoutingTable": ["0.0.0.0/0", "::/0"],
                "autoOutboundsInterface": "auto",
            },
            "sniffing": sniffing(true),
        }));
    }
    inbounds
}

/// Block NetBIOS/mDNS-style UDP noise and multicast when TUN grabs all
/// traffic (v2rayN `SampleTunRules`).
fn tun_safety_rules() -> Vec<Value> {
    vec![
        json!({
            "type": "field", "network": "udp", "port": "135,137-139,5353",
            "outboundTag": "block"
        }),
        json!({
            "type": "field",
            "ip": ["224.0.0.0/3", "ff00::/8"],
            "outboundTag": "block"
        }),
    ]
}

// —— routing ——

/// Xray domain-list entry for one of our rule matchers. Plain strings are
/// substring matches (keywords), `domain:` is suffix, `full:` exact.
fn domain_entry(rule_type: RuleType, payload: &str) -> Option<String> {
    match rule_type {
        RuleType::Domain => Some(format!("full:{}", to_ascii_domain(payload))),
        RuleType::DomainSuffix => Some(format!("domain:{}", to_ascii_domain(payload))),
        RuleType::DomainKeyword => Some(payload.to_string()),
        // Inline geoip was sing-box-legacy only; Geoip payloads already fold
        // into IpCidr-style matching upstream.
        RuleType::IpCidr | RuleType::Geoip => None,
        RuleType::Process => None,
    }
}

fn ip_entry(payload: &str) -> String {
    if payload.starts_with("geoip:") {
        payload.to_string()
    } else {
        format!("geoip:{payload}")
    }
}

/// Map one rule to an Xray field rule. `main_target` substitutes for the
/// sing-box `proxy` selector group.
fn rule_to_xray(
    rule: &crate::domain::Rule,
    nodes: &[ProxyNode],
    tags: &[String],
    main_target: &str,
    smart_balancer_tags: &std::collections::HashMap<String, String>,
) -> Option<Value> {
    use crate::domain::Rule;
    let payload = rule.payload.trim();
    if payload.is_empty() || matches!(rule.rule_type, RuleType::Geoip) {
        return None;
    }
    let Rule {
        target, node_id, ..
    } = rule;
    // Node pins point straight at the node outbound. Smart rules with a
    // non-empty keyword pool route through their per-rule balancer; empty
    // pools (and plain Proxy) fall back to the main target.
    let smart_balancer = match target {
        RuleTarget::Smart => smart_balancer_tags.get(&rule.id),
        _ => None,
    };
    let outbound = match target {
        RuleTarget::Direct => "direct".to_string(),
        RuleTarget::Block => "block".to_string(),
        // Xray has no `detour` chain concept — Chain routing is a
        // sing-box-only feature (see config/builder.rs). Degrade to the main
        // target, same as an empty Smart pool.
        RuleTarget::Proxy | RuleTarget::Smart | RuleTarget::Chain => main_target.to_string(),
        RuleTarget::Node => {
            let pinned = node_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|id| nodes.iter().find(|n| n.id == id))
                .map(outbound_tag)
                .filter(|tag| tags.iter().any(|t| t == tag));
            pinned.unwrap_or_else(|| main_target.to_string())
        }
    };
    let mut obj = Map::new();
    obj.insert("type".into(), json!("field"));
    match rule.rule_type {
        RuleType::Domain | RuleType::DomainSuffix | RuleType::DomainKeyword => {
            obj.insert(
                "domain".into(),
                json!([domain_entry(rule.rule_type, payload)?]),
            );
        }
        RuleType::IpCidr => {
            obj.insert("ip".into(), json!([payload]));
        }
        RuleType::Process => {
            obj.insert("process".into(), json!([payload]));
        }
        RuleType::Geoip => {
            obj.insert("ip".into(), json!([ip_entry(payload)]));
        }
    }
    if let Some(balancer) = smart_balancer {
        obj.insert("balancerTag".into(), json!(balancer));
    } else {
        obj.insert("outboundTag".into(), json!(outbound));
    }
    Some(Value::Object(obj))
}

/// Whole-set rule for a builtin remote set expressed via geodata matchers.
fn builtin_remote_xray_rule(set: &RuleSet, main_target: &str) -> Option<Value> {
    let matcher = match set.id.as_str() {
        "system-geosite-cn" => json!({ "domain": ["geosite:cn"] }),
        "system-geoip-cn" => json!({ "ip": ["geoip:cn"] }),
        "system-geolocation-not-cn" => json!({ "domain": ["geosite:geolocation-!cn"] }),
        _ => return None,
    };
    let outbound = match set.strategy {
        RuleSetStrategy::Direct => "direct",
        RuleSetStrategy::Block => "block",
        _ => main_target,
    };
    let mut obj = matcher.as_object().cloned()?;
    obj.insert("type".into(), json!("field"));
    obj.insert("outboundTag".into(), json!(outbound));
    Some(Value::Object(obj))
}

/// Build (route rules, dns egress routing rules) for the Xray config.
/// `filter_balancer_tags` maps a Filter set id to its pool balancer tag —
/// rules of that set route via `balancerTag` instead of `outboundTag`.
#[allow(clippy::too_many_arguments)]
fn build_routing(
    opts: &BuildOptions,
    nodes: &[ProxyNode],
    tags: &[String],
    effective_rules: &[crate::domain::Rule],
    main_target: &str,
    filter_balancer_tags: &std::collections::HashMap<String, String>,
    smart_balancer_tags: &std::collections::HashMap<String, String>,
) -> (Vec<Value>, Vec<Value>) {
    let mut route_rules = Vec::new();
    match opts.outbound_mode {
        OutboundMode::Rule => {
            if opts.rule_sets.is_empty() {
                for rule in effective_rules {
                    if let Some(value) =
                        rule_to_xray(rule, nodes, tags, main_target, smart_balancer_tags)
                    {
                        route_rules.push(value);
                    }
                }
            } else {
                for set in opts.rule_sets.iter().filter(|s| s.enabled) {
                    if set.remote.is_some() {
                        if let Some(rule) = builtin_remote_xray_rule(set, main_target) {
                            route_rules.push(rule);
                        } else {
                            crate::app_log::warn(
                                "xray_config",
                                format!(
                                    "remote rule set '{}' uses the sing-box .srs format and is skipped under Xray",
                                    set.name
                                ),
                            );
                        }
                    } else {
                        // Local set: route per rule (same clamping the sing-box
                        // path applies via effective_route_rules). Whole-set
                        // Node/Filter pins clamp onto each rule — without
                        // this, a Node-strategy set would silently route to
                        // the main target instead of its pinned node.
                        let mut rules: Vec<_> = set
                            .rules
                            .iter()
                            .filter(|r| r.enabled && !r.payload.trim().is_empty())
                            .cloned()
                            .collect();
                        rules.sort_by_key(|r| r.ord);
                        // Filter sets with a keyword pool route the whole set
                        // through its balancer.
                        let set_balancer = if set.strategy == RuleSetStrategy::Filter {
                            filter_balancer_tags.get(&set.id)
                        } else {
                            None
                        };
                        for rule in rules {
                            let mut rule = rule;
                            if let Some(target) = set.strategy.route_target() {
                                rule.target = target;
                                rule.node_id = None;
                                rule.node_name = None;
                                rule.smart_include.clear();
                                rule.smart_exclude.clear();
                            } else {
                                crate::config::builder::clamp_rule_pin_to_set(set, &mut rule);
                            }
                            if let Some(mut value) =
                                rule_to_xray(&rule, nodes, tags, main_target, smart_balancer_tags)
                            {
                                if let Some(balancer) = set_balancer {
                                    if let Some(obj) = value.as_object_mut() {
                                        obj.remove("outboundTag");
                                        obj.insert("balancerTag".into(), json!(balancer));
                                    }
                                }
                                route_rules.push(value);
                            }
                        }
                    }
                }
            }
        }
        // Global/Direct ignore user rules entirely (final rule decides).
        OutboundMode::Global | OutboundMode::Direct => {}
    }

    // DNS egress split: block-listed domains, then the tagged direct
    // resolver, then module queries via the main target.
    let mut dns_egress = Vec::new();
    let blocked_dns: Vec<String> = opts
        .dns
        .enabled_dns_rules()
        .into_iter()
        .filter(|r| r.enabled && matches!(r.action, DnsAction::Block))
        .filter_map(|r| dns_domain_entry(r.matcher, r.payload.trim()))
        .filter(|p| !p.is_empty())
        .collect();
    if !blocked_dns.is_empty() {
        dns_egress.push(json!({
            "type": "field",
            "domain": blocked_dns,
            "inboundTag": [DNS_MODULE_TAG],
            "outboundTag": "block",
        }));
    }
    dns_egress.push(json!({
        "type": "field",
        "inboundTag": [DIRECT_DNS_TAG],
        "outboundTag": "direct",
    }));
    dns_egress.push(json!({
        "type": "field",
        "inboundTag": [DNS_MODULE_TAG],
        "outboundTag": if opts.outbound_mode == OutboundMode::Direct {
            "direct"
        } else {
            main_target
        },
    }));
    (route_rules, dns_egress)
}

fn dns_domain_entry(matcher: DomainMatcher, payload: &str) -> Option<String> {
    if payload.is_empty() {
        return None;
    }
    Some(match matcher {
        DomainMatcher::Domain => format!("full:{}", to_ascii_domain(payload)),
        DomainMatcher::DomainSuffix => format!("domain:{}", to_ascii_domain(payload)),
        DomainMatcher::DomainKeyword => payload.to_string(),
    })
}

// —— dns ——

fn build_dns(
    opts: &BuildOptions,
    sets: &[RuleSet],
    effective_rules: &[crate::domain::Rule],
) -> Value {
    // Classify domains per resolver: remote (untagged server → via proxy) and
    // domestic (tagged direct server → direct egress).
    let mut remote_domains: Vec<String> = Vec::new();
    let mut domestic_domains: Vec<String> = Vec::new();
    let mut local_domains: Vec<String> = Vec::new();

    let mut push_dns_rule = |rule: &DnsRule| {
        let Some(entry) = dns_domain_entry(rule.matcher, rule.payload.trim()) else {
            return;
        };
        match rule.action {
            DnsAction::Remote => remote_domains.push(entry),
            DnsAction::Domestic => domestic_domains.push(entry),
            DnsAction::Local => local_domains.push(entry),
            DnsAction::Block => {}
        }
    };
    for rule in opts
        .dns
        .enabled_dns_rules()
        .into_iter()
        .filter(|r| r.enabled)
    {
        push_dns_rule(&rule);
    }

    // Rule-set level DNS strategy: local sets classify their domain rules;
    // builtin remote sets map onto the equivalent geosite category.
    let effective_ids: std::collections::HashSet<&str> =
        effective_rules.iter().map(|r| r.id.as_str()).collect();
    for set in sets.iter().filter(|s| s.enabled) {
        if set.remote.is_some() {
            let geosite = match set.id.as_str() {
                "system-geosite-cn" => Some("geosite:cn"),
                "system-geoip-cn" => None, // ip-only set: no DNS classification
                "system-geolocation-not-cn" => Some("geosite:geolocation-!cn"),
                _ => None,
            };
            if let Some(geosite) = geosite {
                match set.dns_strategy {
                    crate::domain::RuleSetDnsStrategy::Domestic => {
                        domestic_domains.push(geosite.into())
                    }
                    crate::domain::RuleSetDnsStrategy::Local => local_domains.push(geosite.into()),
                    crate::domain::RuleSetDnsStrategy::Remote => {
                        remote_domains.push(geosite.into())
                    }
                }
            }
            continue;
        }
        for rule in set.rules.iter().filter(|r| r.enabled) {
            // Only rules that actually reach routing carry DNS meaning.
            // Filter sets are excluded from effective_rules upstream (they
            // route via their pool balancer here) but still classify DNS.
            if set.strategy != RuleSetStrategy::Filter && !effective_ids.contains(rule.id.as_str())
            {
                continue;
            }
            let entry = match rule.rule_type {
                RuleType::Domain | RuleType::DomainSuffix | RuleType::DomainKeyword => {
                    match domain_entry(rule.rule_type, rule.payload.trim()) {
                        Some(entry) if !rule.payload.trim().is_empty() => entry,
                        _ => continue,
                    }
                }
                _ => continue,
            };
            match set.dns_strategy {
                crate::domain::RuleSetDnsStrategy::Domestic => domestic_domains.push(entry),
                crate::domain::RuleSetDnsStrategy::Local => local_domains.push(entry),
                crate::domain::RuleSetDnsStrategy::Remote => remote_domains.push(entry),
            }
        }
    }

    // Static hosts (highest priority answers), gated by the master switch.
    let mut hosts = Map::new();
    let effective_hosts = opts.dns.effective_hosts();
    if effective_hosts.enabled {
        for entry in &effective_hosts.entries {
            if entry.enabled && !entry.domain.trim().is_empty() && !entry.addr.trim().is_empty() {
                hosts.insert(
                    to_ascii_domain(entry.domain.trim()).to_string(),
                    json!(entry.addr.trim()),
                );
            }
        }
    }

    let use_fakeip = opts.tun_enabled && opts.dns.fake_ip.enabled;
    let mut servers: Vec<Value> = Vec::new();

    let dns_final = opts.dns.normalize_dns_final();

    // Unified pools (domain::REMOTE/DOMESTIC_DNS_POOL) — the same addresses
    // sing-box and mihomo resolve from. Remote is DoH over TCP: UDP-less
    // nodes (socks5 without UDP ASSOCIATE — ssh -D tunnels, cheap relays)
    // can't carry plaintext-UDP DNS through the proxy, and a dead remote
    // resolver makes Xray hand unresolved domains to the exit server (read
    // as a DNS leak by test sites). DoH rides the proxy over TCP, encrypted
    // end to end; the IP-literal host avoids a bootstrap lookup.
    //
    // dns_final is the ONLY fallback: the primary server (index 0) is the
    // dns_final pool, the second remote entry is in-pool redundancy
    // (ordered fallback), and every other pool carries skipFallback so it
    // answers only its own classified domains — never a silent cross-pool
    // fallback, matching what the DNS-page default resolver promises.
    //
    // Remote servers stay untagged so their queries carry dns.tag and
    // egress through the main outbound; the domestic server is tagged so
    // its queries egress direct.
    let mut remote = Map::new();
    remote.insert("address".into(), json!(REMOTE_DNS_POOL[0]));
    if !remote_domains.is_empty() {
        remote.insert("domains".into(), json!(remote_domains));
    }
    // Second remote entry: no domains list — pure in-pool fallback for the
    // primary remote server. Also untagged → also egresses via proxy.
    let mut remote_backup = Map::new();
    remote_backup.insert("address".into(), json!(REMOTE_DNS_POOL[1]));
    // Domestic resolver (tagged → queries route direct).
    let mut domestic = Map::new();
    domestic.insert("address".into(), json!(DOMESTIC_DNS_POOL[0]));
    domestic.insert("tag".into(), json!(DIRECT_DNS_TAG));
    if !domestic_domains.is_empty() {
        domestic.insert("domains".into(), json!(domestic_domains));
    }
    // System resolver for explicitly local-classified domains.
    let mut local = Map::new();
    local.insert("address".into(), json!("localhost"));
    if !local_domains.is_empty() {
        local.insert("domains".into(), json!(local_domains));
    }

    // Classification-only pools never act as fallback targets.
    let remote_is_primary = dns_final != "domestic" && dns_final != "local";
    if !remote_is_primary {
        remote.insert("skipFallback".into(), json!(true));
    }
    if dns_final != "domestic" {
        domestic.insert("skipFallback".into(), json!(true));
    }
    if dns_final != "local" {
        local.insert("skipFallback".into(), json!(true));
    }

    // Primary (index 0) = the dns_final pool. Every other pool degrades to
    // classification-only (skipFallback set above) and is emitted solely
    // when it has domains to classify.
    match dns_final {
        "domestic" => {
            servers.push(Value::Object(domestic));
            if !local_domains.is_empty() {
                servers.push(Value::Object(local));
            }
            if !remote_domains.is_empty() {
                servers.push(Value::Object(remote));
            }
        }
        "local" => {
            servers.push(Value::Object(local));
            if !remote_domains.is_empty() {
                servers.push(Value::Object(remote));
            }
            if !domestic_domains.is_empty() {
                servers.push(Value::Object(domestic));
            }
        }
        // remote (default): primary + in-pool redundancy, then the other
        // pools as classification-only entries.
        _ => {
            servers.push(Value::Object(remote));
            servers.push(Value::Object(remote_backup));
            if !local_domains.is_empty() {
                servers.push(Value::Object(local));
            }
            if !domestic_domains.is_empty() {
                servers.push(Value::Object(domestic));
            }
        }
    }
    // Note: no localhost/system entry is ever appended — `dns_final` is the
    // sole fallback in every core (the former leak_protect switch and its
    // system-resolver escape hatch were removed by design).
    if use_fakeip {
        servers.push(json!("fakedns"));
    }

    let mut dns = Map::new();
    if !hosts.is_empty() {
        dns.insert("hosts".into(), Value::Object(hosts));
    }
    dns.insert("servers".into(), Value::Array(servers));
    dns.insert("tag".into(), json!(DNS_MODULE_TAG));
    if use_fakeip {
        dns.insert(
            "fakedns".into(),
            json!([{ "ipPool": opts.dns.fake_ip.inet4_range, "poolSize": 65535 }]),
        );
    }
    Value::Object(dns)
}

// —— outbounds ——

fn node_to_xray_outbound(node: &ProxyNode) -> AppResult<Value> {
    let tag = outbound_tag(node);
    // REALITY supports raw (tcp) / grpc / xhttp transports only — fail the
    // node here with a clear reason instead of letting `xray run -test`
    // reject the whole config at startup.
    let reality = node
        .tls
        .as_ref()
        .is_some_and(|t| t.enabled && t.reality_public_key.is_some());
    if reality
        && !matches!(
            node.transport.as_ref(),
            None | Some(Transport::Tcp)
                | Some(Transport::Grpc { .. })
                | Some(Transport::Xhttp { .. })
        )
    {
        return Err(AppError::Config(
            "REALITY only supports tcp/grpc/xhttp transports under Xray".into(),
        ));
    }
    let (protocol, settings) = protocol_settings(node)?;
    let mut obj = Map::new();
    obj.insert("tag".into(), json!(tag));
    obj.insert("protocol".into(), json!(protocol));
    obj.insert("settings".into(), settings);
    if let Some(stream) = stream_settings(node) {
        obj.insert("streamSettings".into(), stream);
    }
    Ok(Value::Object(obj))
}

fn protocol_settings(node: &ProxyNode) -> AppResult<(&'static str, Value)> {
    Ok(match &node.config {
        ProtocolConfig::Vmess {
            uuid,
            alter_id,
            security,
        } => (
            "vmess",
            json!({
                "vnext": [{
                    "address": node.server,
                    "port": node.port,
                    "users": [{
                        "id": uuid,
                        "alterId": alter_id,
                        "security": security,
                        "email": "t@t.tt",
                    }],
                }],
            }),
        ),
        ProtocolConfig::Vless { uuid, flow, .. } => {
            let mut user = Map::new();
            user.insert("id".into(), json!(uuid));
            user.insert("encryption".into(), json!("none"));
            user.insert("email".into(), json!("t@t.tt"));
            // XTLS flow disables mux (Xray requirement; we never enable mux anyway).
            if let Some(flow) = flow.as_deref().filter(|f| !f.is_empty()) {
                user.insert("flow".into(), json!(flow));
            }
            (
                "vless",
                json!({
                    "vnext": [{
                        "address": node.server,
                        "port": node.port,
                        "users": [Value::Object(user)],
                    }],
                }),
            )
        }
        ProtocolConfig::Shadowsocks {
            method,
            password,
            plugin,
            ..
        } => {
            if plugin.as_deref().is_some_and(|p| !p.trim().is_empty()) {
                crate::app_log::warn(
                    "xray_config",
                    format!(
                        "node {}: SIP003 plugins are not supported under Xray; ignoring plugin",
                        node.name
                    ),
                );
            }
            (
                "shadowsocks",
                json!({
                    "servers": [{
                        "address": node.server,
                        "port": node.port,
                        "method": method,
                        "password": password,
                        "ota": false,
                        "level": 1,
                    }],
                }),
            )
        }
        ProtocolConfig::Hysteria2 { obfs, .. } => {
            // Xray's hysteria transport has no obfs field (only unrelated
            // masquerade options) — salamander-obfuscated nodes can't be
            // represented and must fail here rather than silently drop obfs.
            if obfs.as_deref().is_some_and(|o| !o.is_empty()) {
                return Err(AppError::Config(
                    "hysteria2 obfs is not supported by Xray".into(),
                ));
            }
            // HysteriaClientConfig.Build() rejects anything but version 2.
            (
                "hysteria",
                json!({
                    "version": 2,
                    "address": node.server,
                    "port": node.port,
                }),
            )
        }
        ProtocolConfig::Trojan { password } => (
            "trojan",
            json!({
                "servers": [{
                    "address": node.server,
                    "port": node.port,
                    "password": password,
                    "level": 1,
                }],
            }),
        ),
        ProtocolConfig::Socks5 { username, password } => {
            let mut server = Map::new();
            server.insert("address".into(), json!(node.server));
            server.insert("port".into(), json!(node.port));
            if let (Some(user), Some(pass)) = (username, password) {
                if !user.is_empty() && !pass.is_empty() {
                    server.insert(
                        "users".into(),
                        json!([{ "user": user, "pass": pass, "level": 1 }]),
                    );
                }
            }
            ("socks", json!({ "servers": [Value::Object(server)] }))
        }
        ProtocolConfig::Http {
            username, password, ..
        } => {
            let mut settings = Map::new();
            settings.insert("address".into(), json!(node.server));
            settings.insert("port".into(), json!(node.port));
            if let (Some(user), Some(pass)) = (username, password) {
                if !user.is_empty() && !pass.is_empty() {
                    settings.insert("user".into(), json!(user));
                    settings.insert("pass".into(), json!(pass));
                    settings.insert("level".into(), json!(1));
                }
            }
            ("http", Value::Object(settings))
        }
        ProtocolConfig::WireGuard {
            local_address,
            private_key,
            peer_public_key,
            pre_shared_key,
            reserved,
            mtu,
        } => {
            let mut peer = Map::new();
            peer.insert("publicKey".into(), json!(peer_public_key));
            if let Some(psk) = pre_shared_key.as_deref().filter(|s| !s.is_empty()) {
                peer.insert("preSharedKey".into(), json!(psk));
            }
            peer.insert(
                "endpoint".into(),
                json!(format!("{}:{}", node.server, node.port)),
            );
            let mut settings = Map::new();
            settings.insert("secretKey".into(), json!(private_key));
            settings.insert("address".into(), json!(local_address));
            settings.insert("peers".into(), json!([Value::Object(peer)]));
            if !reserved.is_empty() {
                settings.insert("reserved".into(), json!(reserved));
            }
            if let Some(mtu) = mtu {
                settings.insert("mtu".into(), json!(mtu));
            }
            ("wireguard", Value::Object(settings))
        }
        other => {
            return Err(AppError::Config(format!(
                "protocol {} is not supported by Xray",
                other_name(other)
            )))
        }
    })
}

fn other_name(config: &ProtocolConfig) -> &'static str {
    match config {
        ProtocolConfig::Hysteria2 { .. } => "hysteria2",
        ProtocolConfig::Hysteria { .. } => "hysteria",
        ProtocolConfig::Tuic { .. } => "tuic",
        ProtocolConfig::ShadowTls { .. } => "shadowtls",
        ProtocolConfig::Ssh { .. } => "ssh",
        ProtocolConfig::Naive { .. } => "naive",
        ProtocolConfig::Tor { .. } => "tor",
        ProtocolConfig::AnyTls { .. } => "anytls",
        ProtocolConfig::Snell { .. } => "snell",
        _ => "unknown",
    }
}

/// streamSettings: transport (network) + security (tls / reality). Returns
/// `None` for a plain TCP node without TLS (nothing meaningful to configure).
fn stream_settings(node: &ProxyNode) -> Option<Value> {
    let transport = node.transport.as_ref();
    let tls = node.tls.as_ref().filter(|t| t.enabled);
    let is_hysteria2 = node.protocol == crate::domain::Protocol::Hysteria2;
    if !is_hysteria2 && matches!(transport, None | Some(Transport::Tcp)) && tls.is_none() {
        return None;
    }
    let network = if is_hysteria2 {
        "hysteria"
    } else {
        match transport {
            None | Some(Transport::Tcp) => "tcp",
            Some(Transport::Ws { .. }) => "ws",
            Some(Transport::Grpc { .. }) => "grpc",
            Some(Transport::Http { .. }) => "http",
            Some(Transport::HttpUpgrade { .. }) => "httpupgrade",
            Some(Transport::Xhttp { .. }) => "xhttp",
        }
    };

    let is_reality = tls.is_some_and(|t| t.reality_public_key.is_some());

    let mut stream = Map::new();
    stream.insert("network".into(), json!(network));

    if is_hysteria2 {
        if let crate::domain::ProtocolConfig::Hysteria2 { password, .. } = &node.config {
            // HysteriaConfig.Build() (streamSettings.hysteriaSettings) also
            // requires version: 2, same as the outbound settings above.
            stream.insert(
                "hysteriaSettings".into(),
                json!({ "version": 2, "auth": password }),
            );
        }
    }

    match transport {
        Some(Transport::Ws {
            path,
            headers,
            max_early_data,
        }) => {
            let mut ws = Map::new();
            if let Some(host) = headers
                .as_ref()
                .and_then(|h| h.get("Host").or_else(|| h.get("host")))
            {
                ws.insert("host".into(), json!(host));
            }
            if let Some(path) = path.as_deref().filter(|p| !p.is_empty()) {
                ws.insert("path".into(), json!(path));
            }
            if let Some(ed) = max_early_data.filter(|v| *v > 0) {
                ws.insert("maxEarlyData".into(), json!(ed));
            }
            stream.insert("wsSettings".into(), Value::Object(ws));
        }
        Some(Transport::Grpc { service_name }) => {
            let mut grpc = Map::new();
            if let Some(name) = service_name.as_deref().filter(|s| !s.is_empty()) {
                grpc.insert("serviceName".into(), json!(name));
            }
            stream.insert("grpcSettings".into(), Value::Object(grpc));
        }
        Some(Transport::Http { path, host }) => {
            let mut http = Map::new();
            if let Some(hosts) = host {
                if !hosts.is_empty() {
                    http.insert("host".into(), json!(hosts));
                }
            }
            if let Some(path) = path.as_deref().filter(|p| !p.is_empty()) {
                http.insert("path".into(), json!(path));
            }
            stream.insert("httpSettings".into(), Value::Object(http));
        }
        Some(Transport::HttpUpgrade { path, host }) => {
            let mut hu = Map::new();
            if let Some(host) = host.as_deref().filter(|s| !s.is_empty()) {
                hu.insert("host".into(), json!(host));
            }
            if let Some(path) = path.as_deref().filter(|p| !p.is_empty()) {
                hu.insert("path".into(), json!(path));
            }
            stream.insert("httpupgradeSettings".into(), Value::Object(hu));
        }
        Some(Transport::Xhttp { path, host, mode }) => {
            let mut xh = Map::new();
            if let Some(host) = host.as_deref().filter(|s| !s.is_empty()) {
                xh.insert("host".into(), json!(host));
            }
            if let Some(path) = path.as_deref().filter(|p| !p.is_empty()) {
                xh.insert("path".into(), json!(path));
            }
            // Xray defaults to "auto"; only emit an explicit mode when set.
            if let Some(mode) = mode.as_deref().filter(|m| !m.is_empty()) {
                xh.insert("mode".into(), json!(mode));
            }
            stream.insert("xhttpSettings".into(), Value::Object(xh));
        }
        _ => {}
    }

    if let Some(tls) = tls {
        let fingerprint = tls.utls_fingerprint.as_deref().filter(|f| !f.is_empty());
        if is_reality {
            stream.insert("security".into(), json!("reality"));
            let mut reality = Map::new();
            reality.insert("fingerprint".into(), json!(fingerprint.unwrap_or("chrome")));
            if let Some(sni) = tls.server_name.as_deref().filter(|s| !s.is_empty()) {
                reality.insert("serverName".into(), json!(sni));
            }
            if let Some(pk) = tls.reality_public_key.as_deref() {
                reality.insert("publicKey".into(), json!(pk));
            }
            if let Some(sid) = tls.reality_short_id.as_deref().filter(|s| !s.is_empty()) {
                reality.insert("shortId".into(), json!(sid));
            }
            reality.insert("show".into(), json!(false));
            stream.insert("realitySettings".into(), Value::Object(reality));
        } else {
            stream.insert("security".into(), json!("tls"));
            let mut tls_settings = Map::new();
            if let Some(sni) = tls.server_name.as_deref().filter(|s| !s.is_empty()) {
                tls_settings.insert("serverName".into(), json!(sni));
            }
            if let Some(alpn) = tls.alpn.as_ref().filter(|a| !a.is_empty()) {
                tls_settings.insert("alpn".into(), json!(alpn));
            }
            if let Some(fp) = fingerprint {
                tls_settings.insert("fingerprint".into(), json!(fp));
            }
            if tls.insecure == Some(true) {
                // Xray ≥ 26 removed `allowInsecure` (rejected at config load,
                // "migrated to pinnedPeerCertSha256" which needs a cert hash
                // we don't have). Certificate verification stays ON; nodes
                // with self-signed certs fail per-connection, not the start.
                crate::app_log::warn(
                    "xray_config",
                    format!(
                        "node {}: skip-cert-verify is ignored under Xray (cert verification stays on)",
                        node.name
                    ),
                );
            }
            stream.insert("tlsSettings".into(), Value::Object(tls_settings));
        }
    }

    Some(Value::Object(stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        DnsSettings, OutboundMode, Protocol, ProtocolConfig, ProxyNode, Rule, RuleType, TlsConfig,
        Transport,
    };

    fn vless_node(name: &str, flow: Option<&str>) -> ProxyNode {
        ProxyNode {
            id: String::new(),
            name: name.into(),
            protocol: Protocol::Vless,
            server: "example.com".into(),
            port: 443,
            tls: Some(TlsConfig {
                enabled: true,
                server_name: Some("sni.example.com".into()),
                insecure: None,
                alpn: None,
                utls_fingerprint: Some("chrome".into()),
                reality_public_key: Some("pbk".into()),
                reality_short_id: Some("abcd0123".into()),
            }),
            // REALITY supports tcp/grpc only; the default node uses plain TCP.
            transport: Some(Transport::Tcp),
            udp: None,
            config: ProtocolConfig::Vless {
                uuid: "uuid-1".into(),
                flow: flow.map(str::to_string),
                packet_encoding: "xudp".into(),
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        }
        .with_computed_id()
    }

    fn default_opts() -> BuildOptions {
        BuildOptions {
            mixed_port: 2080,
            allow_lan: false,
            api_port: 19090,
            extra_inbounds: Vec::new(),
            api_secret: String::new(),
            current_node_id: None,
            log_level: "info".into(),
            rules: Vec::new(),
            rule_sets: Vec::new(),
            pools: Vec::new(),
            chains: Vec::new(),
            tun_enabled: false,
            tun_stack: "mixed".into(),
            dns: DnsSettings::default(),
            outbound_mode: OutboundMode::Rule,
            route_final: "proxy".into(),
            auto_select: crate::domain::AutoSelectMode::Off,
            probe_url: String::new(),
            find_process: true,
            tun_ipv6: false,
            block_quic: false,
            bypass_lan: true,
            tun_interface_name: None,
            sidecar: None,
        }
    }

    #[test]
    fn builds_minimal_config_with_stats() {
        let nodes = vec![vless_node("n1", None)];
        let built = build_xray_config(&nodes, &default_opts()).expect("build");
        let v = &built.value;
        assert_eq!(v["log"]["loglevel"], "info");
        assert_eq!(v["inbounds"][0]["protocol"], "mixed");
        assert_eq!(v["inbounds"][0]["port"], 2080);
        assert_eq!(v["metrics"]["listen"], "127.0.0.1:19090");
        assert_eq!(v["policy"]["system"]["statsOutboundUplink"], true);
        let outbounds = v["outbounds"].as_array().unwrap();
        assert_eq!(outbounds.len(), 3); // node + direct + block
        assert_eq!(outbounds[1]["tag"], "direct");
        assert_eq!(outbounds[1]["protocol"], "freedom");
        assert_eq!(outbounds[2]["protocol"], "blackhole");
        // final rule routes to the selected node (no selector in Xray)
        let rules = v["routing"]["rules"].as_array().unwrap();
        let last = rules.last().unwrap();
        assert_eq!(last["outboundTag"], built.selected_tag);
    }

    #[test]
    fn vless_outbound_reality_and_flow() {
        let nodes = vec![vless_node("vision", Some("xtls-rprx-vision"))];
        let built = build_xray_config(&nodes, &default_opts()).expect("build");
        let outbound = &built.value["outbounds"][0];
        assert_eq!(outbound["protocol"], "vless");
        assert_eq!(outbound["settings"]["vnext"][0]["address"], "example.com");
        let user = &outbound["settings"]["vnext"][0]["users"][0];
        assert_eq!(user["id"], "uuid-1");
        assert_eq!(user["encryption"], "none");
        assert_eq!(user["flow"], "xtls-rprx-vision");
        let stream = &outbound["streamSettings"];
        assert_eq!(stream["network"], "tcp");
        assert_eq!(stream["security"], "reality");
        assert_eq!(stream["realitySettings"]["publicKey"], "pbk");
        assert_eq!(stream["realitySettings"]["shortId"], "abcd0123");
        assert_eq!(stream["realitySettings"]["fingerprint"], "chrome");
    }

    #[test]
    fn ws_tls_stream_shape() {
        let mut node = vless_node("ws", None);
        node.tls = Some(TlsConfig {
            enabled: true,
            server_name: Some("sni.example.com".into()),
            insecure: Some(true),
            alpn: Some(vec!["h2".into(), "http/1.1".into()]),
            utls_fingerprint: None,
            reality_public_key: None,
            reality_short_id: None,
        });
        node.transport = Some(Transport::Ws {
            path: Some("/ws".into()),
            headers: Some(
                [("Host".to_string(), "cdn.example.com".to_string())]
                    .into_iter()
                    .collect(),
            ),
            max_early_data: Some(2048),
        });
        let built = build_xray_config(&[node], &default_opts()).expect("build");
        let stream = &built.value["outbounds"][0]["streamSettings"];
        assert_eq!(stream["network"], "ws");
        assert_eq!(stream["wsSettings"]["path"], "/ws");
        assert_eq!(stream["wsSettings"]["host"], "cdn.example.com");
        assert_eq!(stream["wsSettings"]["maxEarlyData"], 2048);
        assert_eq!(stream["security"], "tls");
        assert_eq!(stream["tlsSettings"]["serverName"], "sni.example.com");
        // Xray ≥ 26 rejects `allowInsecure` at config load — insecure nodes
        // keep verification on and never emit the field.
        assert!(stream["tlsSettings"].get("allowInsecure").is_none());
        assert_eq!(stream["tlsSettings"]["alpn"], json!(["h2", "http/1.1"]));
    }

    #[test]
    fn reality_with_ws_transport_is_rejected() {
        let mut node = vless_node("bad", None);
        node.transport = Some(Transport::Ws {
            path: Some("/ws".into()),
            headers: None,
            max_early_data: None,
        });
        // The invalid node is skipped; with no other nodes the build fails.
        assert!(build_xray_config(&[node], &default_opts()).is_err());
    }

    #[test]
    fn xhttp_stream_shape_and_reality_combo() {
        // Plain xhttp: path/host land in xhttpSettings, explicit mode kept.
        let mut node = vless_node("xh", None);
        node.transport = Some(Transport::Xhttp {
            path: Some("/upload".into()),
            host: Some("cdn.example.com".into()),
            mode: Some("stream-up".into()),
        });
        let built = build_xray_config(&[node.clone()], &default_opts()).expect("build");
        let stream = &built.value["outbounds"][0]["streamSettings"];
        assert_eq!(stream["network"], "xhttp");
        assert_eq!(stream["xhttpSettings"]["path"], "/upload");
        assert_eq!(stream["xhttpSettings"]["host"], "cdn.example.com");
        assert_eq!(stream["xhttpSettings"]["mode"], "stream-up");

        // REALITY + xhttp is a supported Xray combination.
        node.tls = Some(TlsConfig {
            enabled: true,
            server_name: Some("sni.example.com".into()),
            insecure: None,
            alpn: None,
            utls_fingerprint: Some("chrome".into()),
            reality_public_key: Some("a".repeat(43)),
            reality_short_id: Some("abcd0123".into()),
        });
        let built = build_xray_config(&[node], &default_opts()).expect("build");
        let stream = &built.value["outbounds"][0]["streamSettings"];
        assert_eq!(stream["network"], "xhttp");
        assert_eq!(stream["security"], "reality");
    }

    #[test]
    fn mihomo_supports_node_rejects_xhttp_but_xray_accepts() {
        let mut node = vless_node("xh", None);
        node.transport = Some(Transport::Xhttp {
            path: Some("/x".into()),
            host: None,
            mode: None,
        });
        assert!(CoreKind::Xray.supports_node(&node));
        assert!(!CoreKind::Mihomo.supports_node(&node));
        // sing-box keeps the node listed (delegation decision happens at
        // plan/build time) — the supports set itself is protocol-level.
        assert!(CoreKind::SingBox.supports_node(&node));
    }

    #[test]
    fn vmess_outbound_shape() {
        let mut node = vless_node("vm", None);
        node.protocol = Protocol::Vmess;
        node.config = ProtocolConfig::Vmess {
            uuid: "u2".into(),
            alter_id: 0,
            security: "auto".into(),
        };
        node.tls = None;
        node.transport = None;
        let built = build_xray_config(&[node], &default_opts()).expect("build");
        let outbound = &built.value["outbounds"][0];
        assert_eq!(outbound["protocol"], "vmess");
        assert_eq!(outbound["settings"]["vnext"][0]["users"][0]["alterId"], 0);
        assert!(outbound.get("streamSettings").is_none());
    }

    #[test]
    fn skips_unsupported_protocols() {
        let mut bad = vless_node("tuic", None);
        bad.protocol = Protocol::Tuic;
        bad.config = ProtocolConfig::Tuic {
            uuid: "u".into(),
            password: "p".into(),
            congestion_control: None,
            udp_relay_mode: None,
            zero_rtt_handshake: false,
        };
        let good = vless_node("ok", None);
        let built = build_xray_config(&[bad, good], &default_opts()).expect("build");
        assert_eq!(built.value["outbounds"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn hysteria2_outbound_shape() {
        let mut node = vless_node("hy2", None);
        node.protocol = Protocol::Hysteria2;
        node.transport = None;
        node.tls = Some(TlsConfig {
            enabled: true,
            server_name: Some("sni.example.com".into()),
            insecure: None,
            alpn: None,
            utls_fingerprint: None,
            reality_public_key: None,
            reality_short_id: None,
        });
        node.config = ProtocolConfig::Hysteria2 {
            password: "secret".into(),
            up_mbps: None,
            down_mbps: None,
            obfs: None,
            obfs_password: None,
        };
        let built = build_xray_config(&[node], &default_opts()).expect("build");
        let outbound = &built.value["outbounds"][0];
        assert_eq!(outbound["protocol"], "hysteria");
        assert_eq!(outbound["settings"]["version"], 2);
        assert_eq!(outbound["streamSettings"]["network"], "hysteria");
        assert_eq!(outbound["streamSettings"]["hysteriaSettings"]["version"], 2);
        assert_eq!(
            outbound["streamSettings"]["hysteriaSettings"]["auth"],
            "secret"
        );
        assert_eq!(outbound["streamSettings"]["security"], "tls");
        assert_eq!(
            outbound["streamSettings"]["tlsSettings"]["serverName"],
            "sni.example.com"
        );
    }

    #[test]
    fn hysteria2_with_obfs_is_skipped() {
        let mut node = vless_node("hy2-obfs", None);
        node.protocol = Protocol::Hysteria2;
        node.transport = None;
        node.config = ProtocolConfig::Hysteria2 {
            password: "secret".into(),
            up_mbps: None,
            down_mbps: None,
            obfs: Some("salamander".into()),
            obfs_password: Some("obfspw".into()),
        };
        let good = vless_node("ok", None);
        let built = build_xray_config(&[node, good], &default_opts()).expect("build");
        // hy2-obfs skipped; only "ok" + direct + block remain.
        assert_eq!(built.value["outbounds"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn fails_when_no_supported_nodes() {
        let mut bad = vless_node("tuic", None);
        bad.protocol = Protocol::Tuic;
        bad.config = ProtocolConfig::Tuic {
            uuid: "u".into(),
            password: "p".into(),
            congestion_control: None,
            udp_relay_mode: None,
            zero_rtt_handshake: false,
        };
        assert!(build_xray_config(&[bad], &default_opts()).is_err());
    }

    #[test]
    fn domain_matcher_prefixes() {
        use crate::domain::Rule;
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.rules = vec![
            Rule::new(RuleType::Domain, "a.com".into(), RuleTarget::Direct, 10),
            Rule::new(
                RuleType::DomainSuffix,
                "b.com".into(),
                RuleTarget::Proxy,
                20,
            ),
            Rule::new(
                RuleType::DomainKeyword,
                "keyword".into(),
                RuleTarget::Block,
                30,
            ),
            Rule::new(
                RuleType::Process,
                "chrome.exe".into(),
                RuleTarget::Direct,
                40,
            ),
        ];
        let built = build_xray_config(&nodes, &opts).expect("build");
        let rules = built.value["routing"]["rules"].as_array().unwrap();
        let find = |needle: &str| {
            rules
                .iter()
                .find(|r| r.to_string().contains(needle))
                .unwrap_or_else(|| panic!("rule containing {needle} not found in {rules:?}"))
                .clone()
        };
        let exact = find("full:a.com");
        assert_eq!(exact["outboundTag"], "direct");
        let suffix = find("domain:b.com");
        assert_eq!(suffix["outboundTag"], built.selected_tag);
        let kw = find("\"keyword\"");
        assert_eq!(kw["outboundTag"], "block");
        let process = find("chrome.exe");
        assert_eq!(process["process"], json!(["chrome.exe"]));
    }

    #[test]
    fn kernel_autoselect_uses_balancer() {
        let nodes = vec![vless_node("a", None), vless_node("b", None)];
        let mut opts = default_opts();
        opts.auto_select = crate::domain::AutoSelectMode::Kernel;
        let built = build_xray_config(&nodes, &opts).expect("build");
        let routing = &built.value["routing"];
        assert_eq!(routing["balancers"][0]["tag"], "proxy-balancer");
        assert_eq!(routing["balancers"][0]["strategy"]["type"], "leastPing");
        assert_eq!(routing["balancers"][0]["selector"], json!(["node-"]));
        let last = routing["rules"].as_array().unwrap().last().unwrap();
        assert_eq!(last["balancerTag"], "proxy-balancer");
        assert!(last.get("outboundTag").is_none());
        assert_eq!(
            built.value["observatory"]["subjectSelector"],
            json!(["node-"])
        );
    }

    /// Regression: under kernel auto-select every path that resolves to the
    /// main target (DNS egress, builtin remote proxy sets, proxy-target user
    /// rules, final) used to emit `outboundTag: "proxy-balancer"` — a tag
    /// that is a BALANCER, not an outbound. The dispatcher then failed every
    /// such connection ("non existing outTag"). `xray run -test` does not
    /// validate tag references, so only this guard catches the class.
    #[test]
    fn kernel_mode_never_references_balancer_as_outbound() {
        let nodes = vec![vless_node("a", None), vless_node("b", None)];
        let mut opts = default_opts();
        opts.auto_select = crate::domain::AutoSelectMode::Kernel;
        // User proxy-target rule + builtin remote proxy set + DNS egress —
        // all three previously leaked the bare balancer tag.
        let set = RuleSet::new_user(
            "代理",
            vec![Rule::new(
                RuleType::DomainSuffix,
                "youtube.com".into(),
                RuleTarget::Proxy,
                10,
            )],
        );
        let mut builtin = crate::domain::build_builtin_remote_set(
            crate::domain::builtin_remote_spec("system-geolocation-not-cn").unwrap(),
        );
        builtin.strategy = RuleSetStrategy::Proxy;
        opts.rule_sets = vec![set, builtin];
        let built = build_xray_config(&nodes, &opts).expect("build");
        let routing = &built.value["routing"];
        assert!(routing["balancers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["tag"] == "proxy-balancer"));

        let rules = routing["rules"].as_array().unwrap();
        assert!(
            rules
                .iter()
                .all(|r| r.get("outboundTag").and_then(Value::as_str) != Some("proxy-balancer")),
            "a rule still references the balancer via outboundTag: {rules:?}"
        );
        // And the proxy-resolved rules actually reach the balancer.
        let yt = rules
            .iter()
            .find(|r| r.to_string().contains("youtube.com"))
            .expect("youtube rule");
        assert_eq!(yt["balancerTag"], "proxy-balancer");
        let overseas = rules
            .iter()
            .find(|r| r.to_string().contains("geolocation-!cn"))
            .expect("builtin overseas rule");
        assert_eq!(overseas["balancerTag"], "proxy-balancer");
        let dns_egress = rules
            .iter()
            .find(|r| r["inboundTag"] == json!(["dns-module"]))
            .expect("dns egress rule");
        assert_eq!(dns_egress["balancerTag"], "proxy-balancer");
    }

    #[test]
    fn dns_split_and_tags() {
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.dns.rules_enabled = true;
        opts.dns.rules.push(DnsRule {
            id: "dd1".into(),
            enabled: true,
            matcher: DomainMatcher::DomainSuffix,
            payload: "cn.example".into(),
            action: DnsAction::Domestic,
        });
        let built = build_xray_config(&nodes, &opts).expect("build");
        let dns = &built.value["dns"];
        assert_eq!(dns["tag"], "dns-module");
        let servers = dns["servers"].as_array().unwrap();
        // default dns_final=remote → remote DoH primary (both pool entries,
        // second is in-pool redundancy, untagged so it also egresses via
        // proxy), domestic tagged skipFallback (classification-only).
        assert_eq!(servers[0]["address"], REMOTE_DNS_POOL[0]);
        assert_eq!(servers[1]["address"], REMOTE_DNS_POOL[1]);
        assert!(servers[1].get("skipFallback").is_none());
        // domestic classification-only: tagged direct, skipFallback, only
        // carrying the classified domains (builtin LAN suffixes keep the
        // local server present by default, so look it up instead of
        // assuming an index).
        let domestic = servers
            .iter()
            .find(|s| s["tag"] == json!("direct-dns"))
            .expect("domestic classification entry");
        assert_eq!(domestic["address"], DOMESTIC_DNS_POOL[0]);
        assert_eq!(domestic["skipFallback"], true);
        assert_eq!(domestic["domains"], json!(["domain:cn.example"]));
        // routing: direct-dns → direct, dns-module → main target
        let rules = built.value["routing"]["rules"].as_array().unwrap();
        let direct_dns = rules
            .iter()
            .find(|r| r["inboundTag"] == json!(["direct-dns"]))
            .unwrap();
        assert_eq!(direct_dns["outboundTag"], "direct");
    }

    /// dns_final is the ONLY fallback: with final=domestic the domestic
    /// server is primary and the remote pool degrades to classification-only
    /// (skipFallback) instead of acting as a cross-pool fallback.
    #[test]
    fn dns_final_domestic_makes_remote_classification_only() {
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.dns.dns_final = "domestic".into();
        let built = build_xray_config(&nodes, &opts).expect("build");
        let servers = built.value["dns"]["servers"].as_array().unwrap();
        assert_eq!(servers[0]["address"], DOMESTIC_DNS_POOL[0]);
        assert_eq!(servers[0]["tag"], "direct-dns");
        assert!(servers[0].get("skipFallback").is_none());
        // Remote classification-only when a rule classifies domains to it;
        // with nothing to classify it is omitted entirely.
        assert!(
            servers
                .iter()
                .all(|s| s["address"] != json!(REMOTE_DNS_POOL[0])),
            "unclassified remote pool must not be present as fallback"
        );

        let mut opts = opts;
        opts.dns.rules_enabled = true;
        opts.dns.rules.push(DnsRule {
            id: "dr1".into(),
            enabled: true,
            matcher: DomainMatcher::DomainSuffix,
            payload: "foreign.example".into(),
            action: DnsAction::Remote,
        });
        let built = build_xray_config(&nodes, &opts).expect("build");
        let servers = built.value["dns"]["servers"].as_array().unwrap();
        let remote = servers
            .iter()
            .find(|s| s["address"] == json!(REMOTE_DNS_POOL[0]))
            .expect("remote classification entry");
        assert_eq!(remote["skipFallback"], true);
        assert_eq!(remote["domains"], json!(["domain:foreign.example"]));
    }

    /// Local-classified domains get the system resolver under any dns_final
    /// (regression guard: they used to be silently dropped unless the final
    /// itself was local).
    #[test]
    fn local_dns_rules_emit_system_resolver_under_remote_final() {
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.dns.rules_enabled = true;
        opts.dns.rules.push(DnsRule {
            id: "dl1".into(),
            enabled: true,
            matcher: DomainMatcher::DomainSuffix,
            payload: "corp.internal".into(),
            action: DnsAction::Local,
        });
        let built = build_xray_config(&nodes, &opts).expect("build");
        let servers = built.value["dns"]["servers"].as_array().unwrap();
        let local = servers
            .iter()
            .find(|s| s["address"] == json!("localhost"))
            .expect("system resolver present for local-classified domains");
        assert_eq!(local["skipFallback"], true);
        // Builtin LAN suffixes ride along; the user rule must be in the list.
        let domains = local["domains"].as_array().unwrap();
        assert!(domains.contains(&json!("domain:corp.internal")));
    }

    #[test]
    fn tun_adds_hijack_and_safety_rules() {
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.tun_enabled = true;
        let built = build_xray_config(&nodes, &opts).expect("build");
        let text = built.value.to_string();
        assert!(text.contains("\"protocol\":\"tun\""));
        assert!(text.contains("autoSystemRoutingTable"));
        assert!(text.contains("\"outboundTag\":\"dns-out\""));
        // dns-out outbound exists
        assert!(built.value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["tag"] == "dns-out"));
        // safety: block udp 135,137-139,5353 + multicast
        assert!(text.contains("135,137-139,5353"));
        assert!(text.contains("224.0.0.0/3"));
    }

    #[test]
    fn outbound_mode_direct_forces_direct_final() {
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.outbound_mode = OutboundMode::Direct;
        let built = build_xray_config(&nodes, &opts).expect("build");
        let last = built.value["routing"]["rules"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last["outboundTag"], "direct");
    }

    fn node_tag_of(node: &ProxyNode) -> String {
        crate::config::outbound_tag(node)
    }

    #[test]
    fn per_rule_node_pin_routes_to_that_node() {
        let a = vless_node("nodeA", None);
        let b = vless_node("nodeB", None);
        let mut rule = Rule::new(
            RuleType::DomainSuffix,
            "aa.com".into(),
            RuleTarget::Node,
            10,
        );
        rule.node_id = Some(a.id.clone());
        let mut set = RuleSet::new_user("custom", vec![rule]);
        set.strategy = RuleSetStrategy::Smart; // per-rule decisions preserved
        let mut opts = default_opts();
        opts.rule_sets = vec![set];
        let built = build_xray_config(&[a.clone(), b], &opts).expect("build");
        let text = built.value.to_string();
        let needle = format!(
            "\"domain\":[\"domain:aa.com\"],\"outboundTag\":\"{}\"",
            node_tag_of(&a)
        );
        assert!(
            text.contains(&needle),
            "expected aa.com pinned to nodeA; rules: {text}"
        );
    }

    #[test]
    fn whole_set_node_strategy_pins_all_rules() {
        let a = vless_node("nodeA", None);
        let b = vless_node("nodeB", None);
        // Real-store shape: batch_set_rule_targets rewrites every rule in a
        // Node-strategy set to target=Node with the set-level pin.
        let mut rule = Rule::new(
            RuleType::DomainSuffix,
            "aa.com".into(),
            RuleTarget::Node,
            10,
        );
        rule.node_id = Some(b.id.clone());
        let mut set = RuleSet::new_user("pinned", vec![rule]);
        set.strategy = RuleSetStrategy::Node;
        set.node_id = Some(b.id.clone());
        let mut opts = default_opts();
        opts.rule_sets = vec![set];
        let built = build_xray_config(&[a, b.clone()], &opts).expect("build");
        let text = built.value.to_string();
        let needle = format!(
            "\"domain\":[\"domain:aa.com\"],\"outboundTag\":\"{}\"",
            node_tag_of(&b)
        );
        assert!(
            text.contains(&needle),
            "expected aa.com pinned to the set's node (B); rules: {text}"
        );
    }

    #[test]
    fn legacy_flat_rule_node_pin_routes_to_that_node() {
        let a = vless_node("nodeA", None);
        let b = vless_node("nodeB", None);
        let mut rule = Rule::new(RuleType::Domain, "aa.com".into(), RuleTarget::Node, 10);
        rule.node_id = Some(a.id.clone());
        let mut opts = default_opts();
        opts.rules = vec![rule]; // no rule_sets → legacy flat path
        let built = build_xray_config(&[a.clone(), b], &opts).expect("build");
        let text = built.value.to_string();
        let needle = format!(
            "\"domain\":[\"full:aa.com\"],\"outboundTag\":\"{}\"",
            node_tag_of(&a)
        );
        assert!(
            text.contains(&needle),
            "expected aa.com pinned to nodeA; rules: {text}"
        );
    }

    #[test]
    fn filter_set_routes_through_keyword_pool_balancer() {
        let a = vless_node("香港-01", None);
        let b = vless_node("美国-01", None);
        let rule = Rule::new(
            RuleType::DomainSuffix,
            "stream.tv".into(),
            RuleTarget::Smart,
            10,
        );
        let mut set = RuleSet::new_user("filter", vec![rule]);
        set.strategy = RuleSetStrategy::Filter;
        set.smart_include = vec!["香港".into()];
        let mut opts = default_opts();
        opts.rule_sets = vec![set];
        let built = build_xray_config(&[a, b], &opts).expect("build");

        // The rule references the pool balancer, not an outbound tag.
        let rules = built.value["routing"]["rules"].as_array().unwrap();
        let stream_rule = rules
            .iter()
            .find(|r| r.to_string().contains("stream.tv"))
            .expect("stream.tv rule");
        assert!(stream_rule.get("outboundTag").is_none());
        let balancer_tag = stream_rule["balancerTag"].as_str().unwrap().to_string();

        // The balancer's selector holds exactly the keyword-matched node(s).
        let balancers = built.value["routing"]["balancers"].as_array().unwrap();
        let balancer = balancers
            .iter()
            .find(|b| b["tag"] == json!(balancer_tag))
            .expect("pool balancer");
        assert_eq!(balancer["selector"].as_array().unwrap().len(), 1);
        assert_eq!(balancer["selector"][0], json!(built.outbound_tags[0]));
        assert_eq!(balancer["strategy"]["type"], "leastPing");

        // leastPing needs probes: observatory present even with auto_select off.
        assert!(built.value["observatory"].is_object());
    }

    #[test]
    fn filter_set_with_empty_pool_falls_back_to_main_target() {
        let a = vless_node("nodeA", None);
        let rule = Rule::new(
            RuleType::DomainSuffix,
            "stream.tv".into(),
            RuleTarget::Smart,
            10,
        );
        let mut set = RuleSet::new_user("filter", vec![rule]);
        set.strategy = RuleSetStrategy::Filter;
        set.smart_include = vec!["不存在的关键词".into()];
        let mut opts = default_opts();
        opts.rule_sets = vec![set];
        let built = build_xray_config(&[a], &opts).expect("build");
        let text = built.value.to_string();
        assert!(text.contains("stream.tv"));
        // No pool balancer: the rule keeps a plain outboundTag (main target).
        assert!(!text.contains("filter-"));
        let rules = built.value["routing"]["rules"].as_array().unwrap();
        let stream_rule = rules
            .iter()
            .find(|r| r.to_string().contains("stream.tv"))
            .unwrap();
        assert_eq!(stream_rule["outboundTag"], built.selected_tag);
        assert!(built.value.get("observatory").is_none());
    }

    #[test]
    fn per_rule_smart_keyword_pool_routes_through_balancer() {
        // The user's real shape: a Smart-strategy set holding rules with
        // target=Smart and per-rule include keywords (e.g. chatgpt.com→新加坡).
        let sg = vless_node("新加坡-01", None);
        let hk = vless_node("香港-01", None);
        let us = vless_node("美国-01", None);
        let mut rule = Rule::new(
            RuleType::DomainSuffix,
            "chatgpt.com".into(),
            RuleTarget::Smart,
            10,
        );
        rule.smart_include = vec!["新加坡".into()];
        let mut set = RuleSet::new_user("AI · 智能", vec![rule]);
        set.strategy = RuleSetStrategy::Smart;
        let mut opts = default_opts();
        opts.rule_sets = vec![set];
        let built = build_xray_config(&[sg, hk, us], &opts).expect("build");

        let rules = built.value["routing"]["rules"].as_array().unwrap();
        let chatgpt = rules
            .iter()
            .find(|r| r.to_string().contains("chatgpt.com"))
            .expect("chatgpt rule");
        assert!(chatgpt.get("outboundTag").is_none());
        let balancer_tag = chatgpt["balancerTag"].as_str().unwrap().to_string();
        assert!(balancer_tag.starts_with("smart-"));

        let balancers = built.value["routing"]["balancers"].as_array().unwrap();
        let balancer = balancers
            .iter()
            .find(|b| b["tag"] == json!(balancer_tag))
            .expect("smart pool balancer");
        // Pool holds exactly the keyword-matched node(s).
        assert_eq!(balancer["selector"].as_array().unwrap().len(), 1);
        assert_eq!(balancer["selector"][0], json!(built.outbound_tags[0]));
        assert_eq!(balancer["strategy"]["type"], "leastPing");
        // Observatory up (auto_select off) so leastPing has probe data.
        assert!(built.value["observatory"].is_object());
    }

    #[test]
    fn per_rule_smart_empty_pool_falls_back_to_main_target() {
        let a = vless_node("nodeA", None);
        let mut rule = Rule::new(
            RuleType::DomainSuffix,
            "wtf.com".into(),
            RuleTarget::Smart,
            10,
        );
        rule.smart_include = vec!["不存在的关键词".into()];
        let mut set = RuleSet::new_user("smart", vec![rule]);
        set.strategy = RuleSetStrategy::Smart;
        let mut opts = default_opts();
        opts.rule_sets = vec![set];
        let built = build_xray_config(&[a], &opts).expect("build");
        let rules = built.value["routing"]["rules"].as_array().unwrap();
        let wtf = rules
            .iter()
            .find(|r| r.to_string().contains("wtf.com"))
            .unwrap();
        assert!(wtf.get("balancerTag").is_none());
        assert_eq!(wtf["outboundTag"], built.selected_tag);
        assert!(built.value.get("observatory").is_none());
    }

    #[test]
    fn hosts_map_into_dns() {
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.dns.hosts.enabled = true;
        opts.dns.hosts.entries = vec![crate::domain::HostsEntry {
            id: "h1".into(),
            enabled: true,
            domain: "local.test".into(),
            addr: "10.0.0.8".into(),
        }];
        let built = build_xray_config(&nodes, &opts).expect("build");
        assert_eq!(built.value["dns"]["hosts"]["local.test"], "10.0.0.8");
    }

    #[test]
    fn bypass_lan_in_rule_mode_only() {
        let nodes = vec![vless_node("n", None)];
        let mut opts = default_opts();
        opts.bypass_lan = true;
        let built = build_xray_config(&nodes, &opts).expect("build");
        assert!(built.value.to_string().contains("geoip:private"));
        let mut opts_global = opts.clone();
        opts_global.outbound_mode = OutboundMode::Global;
        let built_g = build_xray_config(&nodes, &opts_global).expect("build");
        assert!(!built_g.value.to_string().contains("geoip:private"));
    }

    /// Live validation against a real xray binary (`xray run -test -c`):
    /// proves the generated document is accepted by the actual core, not just
    /// by our own JSON expectations. Ignored by default — requires the
    /// dev-tree bundled binary (run `scripts/fetch-bundled-xray-*` first):
    /// `cargo test --lib config::xray::tests::live_config_validates -- --ignored`
    #[test]
    #[ignore = "needs the bundled dev xray binary"]
    fn live_config_validates() {
        let bin = crate::core::find_bundled_core(None, CoreKind::Xray)
            .expect("bundled xray binary — run the fetch-bundled-xray script");
        let mut node = vless_node("live", Some("xtls-rprx-vision"));
        node.server = "127.0.0.1".into();
        node.port = 443;
        node.tls = Some(TlsConfig {
            enabled: true,
            server_name: Some("sni.example.com".into()),
            insecure: None,
            alpn: None,
            utls_fingerprint: Some("chrome".into()),
            // Valid-format x25519 key + hex shortId so reality config parses.
            reality_public_key: Some("a".repeat(43)),
            reality_short_id: Some("abcd0123".into()),
        });
        node.transport = Some(Transport::Grpc {
            service_name: Some("svc".into()),
        });
        // Second node: plain TLS + ws + skip-cert-verify — the combination
        // that once emitted the removed `allowInsecure` field and was
        // rejected at config load (regression guard).
        let mut tls_node = vless_node("live-ws", None);
        tls_node.server = "127.0.0.1".into();
        tls_node.port = 8443;
        tls_node.tls = Some(TlsConfig {
            enabled: true,
            server_name: Some("cdn.example.com".into()),
            insecure: Some(true),
            alpn: Some(vec!["http/1.1".into()]),
            utls_fingerprint: Some("chrome".into()),
            reality_public_key: None,
            reality_short_id: None,
        });
        tls_node.transport = Some(Transport::Ws {
            path: Some("/ws".into()),
            headers: None,
            max_early_data: None,
        });
        let nodes = vec![node, tls_node];
        let mut opts = default_opts();
        // Include a Filter (keyword-pool) set: the real binary must accept
        // the exact-tag selector balancer + balancerTag rule + observatory.
        let filter_rule = Rule::new(
            RuleType::DomainSuffix,
            "stream.tv".into(),
            RuleTarget::Smart,
            10,
        );
        let mut filter_set = RuleSet::new_user("pool", vec![filter_rule]);
        filter_set.strategy = RuleSetStrategy::Filter;
        filter_set.smart_include = vec!["live".into()];
        opts.rule_sets = vec![filter_set];
        let built = build_xray_config(&nodes, &opts).expect("build");

        let tmp = std::env::temp_dir().join(format!(
            "satelite-xray-live-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&tmp, serde_json::to_vec(&built.value).unwrap()).unwrap();
        let output = std::process::Command::new(&bin)
            .args(["run", "-test", "-c"])
            .arg(&tmp)
            .output()
            .expect("spawn xray");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            output.status.success(),
            "xray run -test rejected the generated config:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    // ---- Xray sidecar companion config ------------------------------------

    #[test]
    fn sidecar_config_maps_each_inbound_to_its_node() {
        let mut a = vless_node("a", None);
        a.id = "aaaa".into();
        let mut b = vless_node("b", None);
        b.id = "bbbb".into();
        let entries = vec![(a, 20890u16), (b, 20891)];
        let built = build_xray_sidecar_config(&entries).expect("build");
        let v = &built.value;

        // One loopback mixed inbound per node, 1:1 with the plan ports.
        let inbounds = v["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[0]["listen"], "127.0.0.1");
        assert_eq!(inbounds[0]["port"], 20890);
        assert_eq!(inbounds[0]["tag"], "in-sc-20890");
        assert_eq!(inbounds[1]["port"], 20891);

        // Per-inbound dispatch rule + a final fallback to the first node.
        let rules = v["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0]["inboundTag"], json!(["in-sc-20890"]));
        assert_eq!(rules[0]["outboundTag"], built.outbound_tags[0]);
        assert_eq!(rules[2]["outboundTag"], built.selected_tag);

        // Minimal document: no metrics/dns module, no geodata references —
        // the sidecar must start without the geosite/geoip assets.
        assert!(v.get("metrics").is_none());
        assert!(v.get("dns").is_none());
        assert!(!v.to_string().contains("geosite"));
        assert!(!v.to_string().contains("geoip"));
    }

    #[test]
    fn sidecar_config_empty_entries_error() {
        assert!(build_xray_sidecar_config(&[]).is_err());
    }

    /// Live validation of the sidecar companion config (same harness as
    /// `live_config_validates`). Needs the bundled dev xray binary:
    /// `cargo test --lib config::xray::tests::live_sidecar_config_validates -- --ignored`
    #[test]
    #[ignore = "needs the bundled dev xray binary"]
    fn live_sidecar_config_validates() {
        let bin = crate::core::find_bundled_core(None, CoreKind::Xray)
            .expect("bundled xray binary — run the fetch-bundled-xray script");
        // Valid-format x25519 key (43 chars base64url) so the REALITY parser
        // accepts it — same fixture discipline as live_config_validates.
        let mut node = vless_node("live", None);
        node.tls = Some(TlsConfig {
            enabled: true,
            server_name: Some("sni.example.com".into()),
            insecure: None,
            alpn: None,
            utls_fingerprint: Some("chrome".into()),
            reality_public_key: Some("a".repeat(43)),
            reality_short_id: Some("abcd0123".into()),
        });
        let entries = vec![(node, 20890u16)];
        let built = build_xray_sidecar_config(&entries).expect("build");

        let tmp = std::env::temp_dir().join(format!(
            "satelite-xray-sidecar-live-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&tmp, serde_json::to_vec(&built.value).unwrap()).unwrap();
        let output = std::process::Command::new(&bin)
            .args(["run", "-test", "-c"])
            .arg(&tmp)
            .output()
            .expect("spawn xray");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            output.status.success(),
            "xray run -test rejected the sidecar config:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
