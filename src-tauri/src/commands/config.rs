use crate::config::{
    active_config_path, active_yaml_config_path, build_mihomo_config, build_singbox_config,
    build_xray_config, generate_api_secret, write_active_config, write_active_yaml_config,
    BuildOptions,
};
use crate::domain::{AppSettings, ProxyNode, RuntimeSource, SubscriptionSource};
use crate::error::AppError;
use crate::state::AppState;
use crate::subscription::parse_singbox_json;
use serde::Serialize;
use std::collections::HashMap;
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Serialize)]
pub struct GenerateConfigResult {
    pub path: String,
    pub selected_tag: String,
    pub outbound_count: usize,
    pub mixed_port: u16,
    pub api_port: u16,
    /// Pretty JSON for UI preview (may be large).
    pub preview: String,
}

/// Node list item for UI: ProxyNode fields + owning subscription (mix mode label).
#[derive(Debug, Serialize)]
pub struct ListedNode {
    #[serde(flatten)]
    pub node: ProxyNode,
    pub subscription_id: String,
    pub subscription_name: String,
}

#[derive(Debug, Serialize)]
pub struct NodePage {
    pub nodes: Vec<ListedNode>,
    pub total: usize,
    pub offset: usize,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    state
        .with_store(|store| Ok(store.settings.clone()))
        .map_err(|e| e.to_string())
}

/// Rotate the clash_api secret (user-triggered from Settings → Ports).
/// Restarts a running core so the new secret takes effect immediately.
#[tauri::command]
pub async fn regenerate_api_secret(app: AppHandle) -> Result<AppSettings, String> {
    let resource_dir = app.path().resource_dir().ok();
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .regenerate_api_secret(resource_dir.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("regenerate secret task: {e}"))?
}

#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    mixed_port: Option<u16>,
    allow_lan: Option<bool>,
    api_port: Option<u16>,
    api_secret_enabled: Option<bool>,
    extra_inbounds: Option<Vec<crate::domain::ExtraInbound>>,
    probe_url: Option<String>,
    tun_enabled: Option<bool>,
    tun_stack: Option<String>,
    tun_ipv6_enabled: Option<bool>,
    block_quic: Option<bool>,
    bypass_lan: Option<bool>,
    close_to_tray: Option<bool>,
    launch_at_login: Option<bool>,
    silent_start: Option<bool>,
    auto_start_proxy: Option<bool>,
    close_connections_on_switch: Option<bool>,
    locale: Option<String>,
    theme: Option<String>,
    accent: Option<String>,
    glow_color: Option<String>,
    home_background: Option<String>,
    hero_style: Option<String>,
    glass_frost: Option<bool>,
    tray_icon: Option<String>,
    unload_ui_on_tray: Option<bool>,
    smart_switch: Option<bool>,
    auto_select: Option<String>, // off | smart | kernel
    route_final: Option<String>, // proxy | direct | block (Rule mode)
    find_process: Option<bool>,
) -> Result<AppSettings, String> {
    let mut launch_changed: Option<bool> = None;
    let mut auto_select_changed: Option<(
        crate::domain::AutoSelectMode,
        crate::domain::AutoSelectMode,
    )> = None;
    let mut route_final_changed = false;
    let mut find_process_changed = false;
    let mut bypass_lan_changed = false;
    let settings = state
        .with_store_mut(|store| {
            if let Some(p) = mixed_port {
                store.settings.mixed_port = p;
            }
            if let Some(v) = allow_lan {
                store.settings.allow_lan = v;
            }
            if let Some(p) = api_port {
                store.settings.api_port = p;
            }
            if let Some(v) = api_secret_enabled {
                store.settings.api_secret_enabled = v;
                if !v {
                    store.settings.clash_api_secret = None;
                }
            }
            if let Some(list) = &extra_inbounds {
                if list.len() > 10 {
                    return Err(AppError::Config("额外入站监听最多 10 个".into()));
                }
                let mut seen = std::collections::HashSet::new();
                seen.insert(store.settings.mixed_port);
                seen.insert(store.settings.api_port);
                for inb in list {
                    if inb.kind != "mixed" && inb.kind != "http" {
                        return Err(AppError::Config(format!(
                            "入站类型无效：{kind}（仅支持 mixed / http）",
                            kind = inb.kind
                        )));
                    }
                    if inb.port == 0 {
                        return Err(AppError::Config("入站端口无效".into()));
                    }
                    if !seen.insert(inb.port) {
                        return Err(AppError::Config(format!(
                            "端口 {} 与其他监听端口冲突",
                            inb.port
                        )));
                    }
                }
                store.settings.extra_inbounds = list.clone();
            }
            if let Some(u) = probe_url {
                if !u.trim().is_empty() {
                    store.settings.probe_url = u;
                }
            }
            if let Some(t) = tun_enabled {
                store.settings.tun_enabled = t;
                if t {
                    store.settings.capture_mode = crate::domain::CaptureMode::Tun;
                } else if store.settings.capture_mode == crate::domain::CaptureMode::Tun {
                    store.settings.capture_mode = crate::domain::CaptureMode::Off;
                }
            }
            if let Some(s) = tun_stack {
                let s = s.trim().to_ascii_lowercase();
                if matches!(s.as_str(), "system" | "gvisor" | "mixed") {
                    store.settings.tun_stack = s;
                }
            }
            if let Some(v) = tun_ipv6_enabled {
                store.settings.tun_ipv6_enabled = v;
            }
            if let Some(v) = block_quic {
                store.settings.block_quic = v;
            }
            if let Some(v) = bypass_lan {
                if store.settings.bypass_lan != v {
                    bypass_lan_changed = true;
                    store.settings.bypass_lan = v;
                }
            }
            if let Some(v) = close_to_tray {
                store.settings.close_to_tray = v;
            }
            if let Some(v) = launch_at_login {
                if store.settings.launch_at_login != v {
                    launch_changed = Some(v);
                }
                store.settings.launch_at_login = v;
            }
            if let Some(v) = silent_start {
                store.settings.silent_start = v;
            }
            if let Some(v) = auto_start_proxy {
                store.settings.auto_start_proxy = v;
            }
            if let Some(v) = close_connections_on_switch {
                store.settings.close_connections_on_switch = v;
            }
            if let Some(loc) = locale {
                let loc = loc.trim().to_ascii_lowercase();
                if matches!(loc.as_str(), "zh" | "en") {
                    store.settings.locale = loc;
                }
            }
            if let Some(th) = theme {
                let th = th.trim().to_ascii_lowercase();
                if matches!(th.as_str(), "aerospace" | "day") {
                    store.settings.theme = th;
                }
            }
            if let Some(ac) = accent {
                let ac = ac.trim().to_ascii_lowercase();
                // Preset ids, or a custom accent picked in the color picker
                // (stored verbatim as `#rrggbb`).
                let is_hex = ac.len() == 7
                    && ac.starts_with('#')
                    && ac[1..].chars().all(|c| c.is_ascii_hexdigit());
                if matches!(
                    ac.as_str(),
                    "green" | "blue" | "purple" | "pink" | "orange" | "cyan"
                ) || is_hex
                {
                    store.settings.accent = ac;
                }
            }
            if let Some(gl) = glow_color {
                let gl = gl.trim().to_ascii_lowercase();
                // "accent" tracks the UI accent; otherwise preset ids or a
                // custom `#rrggbb` picked in the glow color picker.
                let is_hex = gl.len() == 7
                    && gl.starts_with('#')
                    && gl[1..].chars().all(|c| c.is_ascii_hexdigit());
                if gl == "accent"
                    || matches!(
                        gl.as_str(),
                        "green" | "blue" | "purple" | "pink" | "orange" | "cyan"
                    )
                    || is_hex
                {
                    store.settings.glow_color = gl;
                }
            }
            if let Some(bg) = home_background {
                let bg = bg.trim().to_ascii_lowercase();
                if matches!(bg.as_str(), "starfield" | "ocean") {
                    store.settings.home_background = bg;
                }
            }
            if let Some(hs) = hero_style {
                let hs = hs.trim().to_ascii_lowercase();
                if matches!(hs.as_str(), "particle" | "classic" | "smiley") {
                    store.settings.hero_style = hs;
                }
            }
            if let Some(v) = glass_frost {
                store.settings.glass_frost = v;
            }
            if let Some(raw) = tray_icon {
                if let Some(style) = crate::domain::TrayIconStyle::parse(&raw) {
                    store.settings.tray_icon = style;
                }
            }
            if let Some(v) = unload_ui_on_tray {
                store.settings.unload_ui_on_tray = v;
            }
            if let Some(rf) = route_final {
                let rf = rf.trim().to_ascii_lowercase();
                if matches!(rf.as_str(), "proxy" | "direct" | "block") {
                    if store.settings.route_final != rf {
                        route_final_changed = true;
                        store.settings.route_final = rf;
                    }
                }
            }
            // Prefer explicit auto_select; legacy smart_switch maps to off/smart.
            if let Some(v) = find_process {
                if store.settings.find_process != v {
                    find_process_changed = true;
                    store.settings.find_process = v;
                }
            }
            if let Some(raw) = auto_select {
                if let Some(mode) = crate::domain::AutoSelectMode::parse(&raw) {
                    let prev = store.settings.auto_select;
                    if prev != mode {
                        auto_select_changed = Some((prev, mode));
                        store.settings.auto_select = mode;
                        store.settings.smart_switch = mode.is_smart();
                        crate::app_log::info(
                            "settings",
                            format!("auto_select {} → {}", prev.as_str(), mode.as_str()),
                        );
                    }
                }
            } else if let Some(v) = smart_switch {
                let mode = if v {
                    crate::domain::AutoSelectMode::Smart
                } else {
                    crate::domain::AutoSelectMode::Off
                };
                let prev = store.settings.auto_select;
                // Don't clobber kernel via legacy bool unless turning smart on/off from non-kernel.
                if prev.is_kernel() && !v {
                    // off from UI that still sends smartSwitch:false while on kernel → treat as off
                    auto_select_changed = Some((prev, crate::domain::AutoSelectMode::Off));
                    store.settings.auto_select = crate::domain::AutoSelectMode::Off;
                    store.settings.smart_switch = false;
                } else if prev != mode && !prev.is_kernel() {
                    auto_select_changed = Some((prev, mode));
                    store.settings.auto_select = mode;
                    store.settings.smart_switch = mode.is_smart();
                } else if prev.is_kernel() && v {
                    auto_select_changed = Some((prev, crate::domain::AutoSelectMode::Smart));
                    store.settings.auto_select = crate::domain::AutoSelectMode::Smart;
                    store.settings.smart_switch = true;
                }
                crate::app_log::info(
                    "settings",
                    format!(
                        "smart_switch legacy → auto_select {}",
                        store.settings.auto_select.as_str()
                    ),
                );
            }
            Ok(store.settings.clone())
        })
        .map_err(|e| e.to_string())?;

    if let Some(enabled) = launch_changed {
        crate::autostart::set_launch_at_login(enabled).map_err(|e| e.to_string())?;
    }
    crate::tray::refresh_icon(&app);

    // route.final must restart: sing-box Clash PUT /configs often returns OK without
    // re-applying route.final (file updates, process keeps old final).
    // selector ↔ urltest also needs a full restart (outbound type changes).
    let need_restart = route_final_changed
        || find_process_changed
        || bypass_lan_changed
        || auto_select_changed
            .map(|(prev, next)| prev.is_kernel() != next.is_kernel())
            .unwrap_or(false);
    if need_restart {
        crate::rule_apply::request_restart(app, Vec::new());
    }

    Ok(settings)
}

#[tauri::command]
pub async fn set_current_node(app: AppHandle, node_id: String) -> Result<AppSettings, String> {
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        let (settings, was_kernel, _) = state
            .select_current_node_serialized(&node_id, true, true)
            .map_err(|e| e.to_string())?;
        if was_kernel {
            crate::rule_apply::request_restart(worker_app.clone(), Vec::new());
        }
        Ok(settings)
    })
    .await
    .map_err(|e| format!("select node task: {e}"))?
}

#[tauri::command]
pub fn rename_node(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<ProxyNode, String> {
    state
        .with_store_mut(|store| store.rename_node(&id, name))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_all_nodes(state: State<'_, AppState>) -> Result<Vec<ListedNode>, String> {
    state
        .with_store(|store| {
            let names: HashMap<&str, &str> = store
                .subscriptions
                .iter()
                .map(|s| (s.id.as_str(), s.name.as_str()))
                .collect();
            let enabled: std::collections::HashSet<&str> = store
                .subscriptions
                .iter()
                .filter(|s| s.enabled)
                .map(|s| s.id.as_str())
                .collect();
            // Under a core that cannot serve a protocol, such nodes are
            // hidden from listings entirely (they reappear after switching).
            let core_kind = crate::core::CoreKind::parse(&store.settings.core_type);
            Ok(store
                .nodes
                .iter()
                .filter(|n| enabled.contains(n.subscription_id.as_str()))
                .filter(|n| core_kind.supports_node(&n.node))
                .map(|n| ListedNode {
                    node: n.node.clone(),
                    subscription_id: n.subscription_id.clone(),
                    subscription_name: names
                        .get(n.subscription_id.as_str())
                        .copied()
                        .unwrap_or("")
                        .to_string(),
                })
                .collect())
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_nodes_page(
    state: State<'_, AppState>,
    query: Option<String>,
    sort_mode: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<NodePage, String> {
    state
        .with_store(|store| {
            let names: HashMap<&str, &str> = store
                .subscriptions
                .iter()
                .map(|s| (s.id.as_str(), s.name.as_str()))
                .collect();
            let enabled: std::collections::HashSet<&str> = store
                .subscriptions
                .iter()
                .filter(|s| s.enabled)
                .map(|s| s.id.as_str())
                .collect();
            let query = query.unwrap_or_default().trim().to_lowercase();
            // Hide protocols the active core cannot serve (see list_all_nodes).
            let core_kind = crate::core::CoreKind::parse(&store.settings.core_type);
            let mut nodes: Vec<ListedNode> = store
                .nodes
                .iter()
                .filter(|n| enabled.contains(n.subscription_id.as_str()))
                .filter(|n| core_kind.supports_node(&n.node))
                .filter(|n| {
                    query.is_empty()
                        || n.node.name.to_lowercase().contains(&query)
                        || n.node.server.to_lowercase().contains(&query)
                        || n.node.protocol.as_str().to_lowercase().contains(&query)
                        || names
                            .get(n.subscription_id.as_str())
                            .is_some_and(|name| name.to_lowercase().contains(&query))
                })
                .map(|n| ListedNode {
                    node: n.node.clone(),
                    subscription_id: n.subscription_id.clone(),
                    subscription_name: names
                        .get(n.subscription_id.as_str())
                        .copied()
                        .unwrap_or("")
                        .to_string(),
                })
                .collect();
            match sort_mode.as_deref() {
                Some("name") => nodes.sort_by_cached_key(|n| n.node.name.to_lowercase()),
                Some("latency") => nodes.sort_by(|a, b| {
                    let score = |n: &ListedNode| match n.node.latency_ms {
                        Some(ms) => (0u8, ms as u64),
                        None if n.node.latency_at.is_some() => (1, 0),
                        None => (2, 0),
                    };
                    score(a)
                        .cmp(&score(b))
                        .then_with(|| a.node.name.to_lowercase().cmp(&b.node.name.to_lowercase()))
                }),
                _ => {}
            }
            let total = nodes.len();
            let offset = offset.unwrap_or(0).min(total);
            let limit = limit.unwrap_or(200).clamp(1, 500);
            let nodes = nodes.into_iter().skip(offset).take(limit).collect();
            Ok(NodePage {
                nodes,
                total,
                offset,
            })
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_node_ids(
    state: State<'_, AppState>,
    query: Option<String>,
) -> Result<Vec<String>, String> {
    state
        .with_store(|store| {
            let enabled: std::collections::HashSet<&str> = store
                .subscriptions
                .iter()
                .filter(|s| s.enabled)
                .map(|s| s.id.as_str())
                .collect();
            let names: HashMap<&str, &str> = store
                .subscriptions
                .iter()
                .map(|s| (s.id.as_str(), s.name.as_str()))
                .collect();
            let query = query.unwrap_or_default().trim().to_lowercase();
            // Hide protocols the active core cannot serve (see list_all_nodes).
            let core_kind = crate::core::CoreKind::parse(&store.settings.core_type);
            Ok(store
                .nodes
                .iter()
                .filter(|n| enabled.contains(n.subscription_id.as_str()))
                .filter(|n| core_kind.supports_node(&n.node))
                .filter(|n| {
                    query.is_empty()
                        || n.node.name.to_lowercase().contains(&query)
                        || n.node.server.to_lowercase().contains(&query)
                        || n.node.protocol.as_str().to_lowercase().contains(&query)
                        || names
                            .get(n.subscription_id.as_str())
                            .is_some_and(|name| name.to_lowercase().contains(&query))
                })
                .map(|n| n.node.id.clone())
                .collect())
        })
        .map_err(|e| e.to_string())
}

/// Nodes extracted read-only from a stored custom sing-box config body.
/// A config whose outbounds are all groups (selector / urltest / direct / …)
/// is valid but has nothing to show — returns an empty list, not an error.
fn extract_custom_nodes(
    content: &str,
    sub_id: &str,
    sub_name: &str,
) -> Result<Vec<ListedNode>, String> {
    match parse_singbox_json(content) {
        Ok(parsed) => Ok(parsed
            .nodes
            .into_iter()
            .map(|node| ListedNode {
                node,
                subscription_id: sub_id.to_string(),
                subscription_name: sub_name.to_string(),
            })
            .collect()),
        Err(AppError::NoProxies) => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Read-only node list extracted from the selected custom sing-box config.
/// Custom profiles never feed the node store, so the stored config body is
/// parsed on demand. Empty when not in custom runtime mode.
#[tauri::command]
pub fn list_custom_config_nodes(state: State<'_, AppState>) -> Result<Vec<ListedNode>, String> {
    custom_config_nodes(&state)
}

/// Shared body of [`list_custom_config_nodes`]; also feeds the custom-mode
/// latency probe (`test_custom_nodes_latency`).
pub(crate) fn custom_config_nodes(state: &AppState) -> Result<Vec<ListedNode>, String> {
    // Copy only what parsing needs while the store lock is held; the JSON
    // pass runs outside so a large config cannot stall other commands.
    let (custom, tag_names) = state
        .with_store(|store| {
            let id = match store.settings.runtime_source() {
                RuntimeSource::Singbox { id } => id,
                RuntimeSource::Generated => return Ok((None, HashMap::new())),
            };
            // Display names for tags emitted by app-generated configs
            // (see restore_generated_tag_names).
            let tag_names: HashMap<String, String> = store
                .nodes
                .iter()
                .map(|n| {
                    (
                        n.node.id[..n.node.id.len().min(16)].to_string(),
                        n.node.name.clone(),
                    )
                })
                .collect();
            Ok((
                store
                    .subscriptions
                    .iter()
                    .find(|s| s.id == id)
                    .and_then(|s| match &s.source {
                        SubscriptionSource::Singbox { content } => {
                            Some((s.id.clone(), s.name.clone(), content.clone()))
                        }
                        _ => None,
                    }),
                tag_names,
            ))
        })
        .map_err(|e| e.to_string())?;

    match custom {
        Some((id, name, content)) => {
            let mut nodes = extract_custom_nodes(&content, &id, &name)?;
            restore_generated_tag_names(&mut nodes, &tag_names);
            Ok(nodes)
        }
        None => Ok(Vec::new()),
    }
}

/// Outbound tags in app-generated sing-box configs are internal
/// `node-{id[..16]}` hashes (`config::builder::outbound_tag`), not display
/// names. When such a config is re-imported as a custom profile the display
/// names only exist in the node store — recover them by matching the id
/// prefix embedded in the tag. Ids are left untouched so latency results
/// stay stable across calls.
fn restore_generated_tag_names(nodes: &mut [ListedNode], prefix_names: &HashMap<String, String>) {
    for listed in nodes.iter_mut() {
        let Some(suffix) = listed.node.name.strip_prefix("node-") else {
            continue;
        };
        if suffix.len() != 16 || !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if let Some(display) = prefix_names.get(suffix) {
            listed.node.name = display.clone();
        }
    }
}

#[tauri::command]
pub async fn generate_singbox_config(
    state: State<'_, AppState>,
) -> Result<GenerateConfigResult, String> {
    let app_data_dir = state.app_data_dir.clone();

    let (nodes, settings, rules, remote_rule_sets, dns, pools, chains) = state
        .with_store(|store| {
            Ok((
                store.enabled_nodes(),
                store.settings.clone(),
                store.enabled_rules_sorted(),
                store.enabled_rule_sets(),
                store.dns.clone(),
                store.pools.clone(),
                store.chains.clone(),
            ))
        })
        .map_err(|e| e.to_string())?;

    // Reuse the persisted secret (user rotates it explicitly); generate only
    // for old stores that predate the clash_api_secret field.
    let secret = settings
        .clash_api_secret
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(generate_api_secret);
    let core_type = settings.core_type.clone();
    let worker_secret = secret.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let opts = BuildOptions {
            mixed_port: settings.mixed_port,
            allow_lan: settings.allow_lan,
            api_port: settings.api_port,
            extra_inbounds: settings.extra_inbounds.clone(),
            api_secret: worker_secret,
            current_node_id: settings.current_node_id.clone(),
            log_level: "info".into(),
            rules,
            rule_sets: remote_rule_sets,
            pools,
            chains,
            tun_enabled: settings.tun_enabled,
            tun_stack: settings.tun_stack.clone(),
            dns,
            outbound_mode: settings.outbound_mode,
            route_final: settings.route_final.clone(),
            auto_select: settings.auto_select,
            probe_url: settings.probe_url.clone(),
            find_process: settings.find_process,
            tun_ipv6: settings.tun_ipv6_enabled,
            block_quic: settings.block_quic,
            bypass_lan: settings.bypass_lan,
            tun_interface_name: None,
        };
        let result = match crate::core::CoreKind::parse(&core_type) {
            crate::core::CoreKind::Mihomo => {
                let built = build_mihomo_config(&nodes, &opts).map_err(|e| e.to_string())?;
                let path = write_active_yaml_config(&app_data_dir, &built.yaml)
                    .map_err(|e| e.to_string())?;
                GenerateConfigResult {
                    path: path.display().to_string(),
                    selected_tag: built.selected_tag,
                    outbound_count: built.outbound_tags.len(),
                    mixed_port: settings.mixed_port,
                    api_port: settings.api_port,
                    preview: built.yaml,
                }
            }
            kind => {
                let built = if kind == crate::core::CoreKind::Xray {
                    build_xray_config(&nodes, &opts)
                } else {
                    build_singbox_config(&nodes, &opts)
                }
                .map_err(|e| e.to_string())?;
                let path = write_active_config(&app_data_dir, &built).map_err(|e| e.to_string())?;
                let preview = serde_json::to_string_pretty(&built.value).unwrap_or_default();
                GenerateConfigResult {
                    path: path.display().to_string(),
                    selected_tag: built.selected_tag,
                    outbound_count: built.outbound_tags.len(),
                    mixed_port: settings.mixed_port,
                    api_port: settings.api_port,
                    preview,
                }
            }
        };
        Ok::<_, String>(result)
    })
    .await
    .map_err(|e| format!("generate config task: {e}"))??;

    // persist secret + ensure current node set if missing
    state
        .with_store_mut(|store| {
            store.settings.clash_api_secret = Some(secret);
            if store.settings.current_node_id.is_none() {
                if let Some(first) = store.enabled_nodes().first() {
                    store.settings.current_node_id = Some(first.id.clone());
                }
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    Ok(result)
}

#[tauri::command]
pub fn get_active_config_path(state: State<'_, AppState>) -> Result<Option<String>, String> {
    // mihomo keeps its Clash YAML in active.yaml; JSON cores share active.json.
    let core_type = state
        .with_store(|store| Ok(store.settings.core_type.clone()))
        .map_err(|e| e.to_string())?;
    let path = match crate::core::CoreKind::parse(&core_type) {
        crate::core::CoreKind::Mihomo => active_yaml_config_path(&state.app_data_dir),
        _ => active_config_path(&state.app_data_dir),
    };
    if path.exists() {
        Ok(Some(path.display().to_string()))
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn preview_singbox_config(
    state: State<'_, AppState>,
) -> Result<GenerateConfigResult, String> {
    let (nodes, settings, rules, remote_rule_sets, dns, pools, chains) = state
        .with_store(|store| {
            Ok((
                store.enabled_nodes(),
                store.settings.clone(),
                store.enabled_rules_sorted(),
                store.enabled_rule_sets(),
                store.dns.clone(),
                store.pools.clone(),
                store.chains.clone(),
            ))
        })
        .map_err(|e| e.to_string())?;

    let secret = settings
        .clash_api_secret
        .clone()
        .unwrap_or_else(generate_api_secret);
    let core_type = settings.core_type.clone();

    let path = match crate::core::CoreKind::parse(&core_type) {
        crate::core::CoreKind::Mihomo => active_yaml_config_path(&state.app_data_dir),
        _ => active_config_path(&state.app_data_dir),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let opts = BuildOptions {
            mixed_port: settings.mixed_port,
            allow_lan: settings.allow_lan,
            api_port: settings.api_port,
            extra_inbounds: settings.extra_inbounds.clone(),
            api_secret: secret,
            current_node_id: settings.current_node_id.clone(),
            log_level: "info".into(),
            rules,
            rule_sets: remote_rule_sets,
            pools,
            chains,
            tun_enabled: settings.tun_enabled,
            tun_stack: settings.tun_stack.clone(),
            dns,
            outbound_mode: settings.outbound_mode,
            route_final: settings.route_final.clone(),
            auto_select: settings.auto_select,
            probe_url: settings.probe_url.clone(),
            find_process: settings.find_process,
            tun_ipv6: settings.tun_ipv6_enabled,
            block_quic: settings.block_quic,
            bypass_lan: settings.bypass_lan,
            tun_interface_name: None,
        };
        let result = match crate::core::CoreKind::parse(&core_type) {
            crate::core::CoreKind::Mihomo => {
                let built = build_mihomo_config(&nodes, &opts).map_err(|e| e.to_string())?;
                GenerateConfigResult {
                    path: path.display().to_string(),
                    selected_tag: built.selected_tag,
                    outbound_count: built.outbound_tags.len(),
                    mixed_port: settings.mixed_port,
                    api_port: settings.api_port,
                    preview: built.yaml,
                }
            }
            kind => {
                let built = if kind == crate::core::CoreKind::Xray {
                    build_xray_config(&nodes, &opts)
                } else {
                    build_singbox_config(&nodes, &opts)
                }
                .map_err(|e| e.to_string())?;
                let preview = serde_json::to_string_pretty(&built.value).unwrap_or_default();
                GenerateConfigResult {
                    path: path.display().to_string(),
                    selected_tag: built.selected_tag,
                    outbound_count: built.outbound_tags.len(),
                    mixed_port: settings.mixed_port,
                    api_port: settings.api_port,
                    preview,
                }
            }
        };
        Ok::<_, String>(result)
    })
    .await
    .map_err(|e| format!("preview config task: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "inbounds": [{"type": "mixed", "listen_port": 7890}],
        "outbounds": [
            {"type": "selector", "tag": "route", "outbounds": ["a", "b", "direct"]},
            {"type": "shadowsocks", "tag": "a", "server": "a.example.com", "server_port": 8388,
             "method": "aes-128-gcm", "password": "pw"},
            {"type": "trojan", "tag": "b", "server": "b.example.com", "server_port": 443,
             "password": "pw"},
            {"type": "direct", "tag": "direct"}
        ]
    }"#;

    #[test]
    fn extract_custom_nodes_maps_outbounds() {
        let nodes = extract_custom_nodes(SAMPLE, "sub1", "My Config").unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|n| n.subscription_id == "sub1"));
        assert!(nodes.iter().all(|n| n.subscription_name == "My Config"));
        let names: Vec<&str> = nodes.iter().map(|n| n.node.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn extract_custom_nodes_all_groups_is_empty() {
        let content = r#"{
            "inbounds": [{"type": "mixed", "listen_port": 7890}],
            "outbounds": [
                {"type": "selector", "tag": "route", "outbounds": ["direct"]},
                {"type": "direct", "tag": "direct"}
            ]
        }"#;
        assert!(extract_custom_nodes(content, "sub1", "x")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn extract_custom_nodes_invalid_json_errors() {
        assert!(extract_custom_nodes("{ not json", "sub1", "x").is_err());
    }

    #[test]
    fn restore_generated_tag_names_recovers_display_names() {
        // Round-trip: a generated-style config carries internal `node-{id16}`
        // tags; the display name is recovered via the embedded id prefix.
        use crate::domain::Protocol;
        let id = ProxyNode::compute_id("香港 01", "a.example.com", 8388, Protocol::Shadowsocks);
        let tag = format!("node-{}", &id[..16]);
        let content = format!(
            r#"{{
                "inbounds": [{{"type": "mixed", "listen_port": 7890}}],
                "outbounds": [
                    {{"type": "shadowsocks", "tag": "{tag}", "server": "a.example.com",
                      "server_port": 8388, "method": "aes-128-gcm", "password": "pw"}}
                ]
            }}"#
        );
        let mut nodes = extract_custom_nodes(&content, "sub1", "My Config").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node.name, tag);

        let mut map = HashMap::new();
        map.insert(id[..16].to_string(), "香港 01".to_string());
        restore_generated_tag_names(&mut nodes, &map);
        assert_eq!(nodes[0].node.name, "香港 01");
    }

    #[test]
    fn restore_generated_tag_names_only_matches_id_prefixes() {
        let content = r#"{
            "inbounds": [{"type": "mixed", "listen_port": 7890}],
            "outbounds": [
                {"type": "shadowsocks", "tag": "node-short", "server": "s1.example.com",
                 "server_port": 8388, "method": "aes-128-gcm", "password": "pw"},
                {"type": "shadowsocks", "tag": "node-aabbccddeeff0011", "server": "s2.example.com",
                 "server_port": 8388, "method": "aes-128-gcm", "password": "pw"}
            ]
        }"#;
        let mut nodes = extract_custom_nodes(content, "sub1", "x").unwrap();
        let mut map = HashMap::new();
        map.insert("0011aabbccddeeff".to_string(), "其他节点".to_string());
        restore_generated_tag_names(&mut nodes, &map);
        let names: Vec<&str> = nodes.iter().map(|n| n.node.name.as_str()).collect();
        // Wrong shape ("short") and unknown 16-hex prefix stay untouched.
        assert_eq!(names, ["node-short", "node-aabbccddeeff0011"]);
    }
}
