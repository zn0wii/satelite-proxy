use crate::config::{dump_rule_set_files, remove_rule_set_files};
use crate::domain::{
    Rule, RuleSet, RuleSetDnsStrategy, RuleSetStrategy, RuleSetSummary, RuleTarget, RuleType,
};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Deserialize)]
pub struct SaveRuleInput {
    pub set_id: Option<String>,
    pub id: Option<String>,
    pub rule_type: RuleType,
    pub payload: String,
    pub target: RuleTarget,
    pub ord: Option<i32>,
    pub enabled: Option<bool>,
    /// Required when `target == node`.
    pub node_id: Option<String>,
    /// When `target == smart`: name must contain each keyword.
    #[serde(default)]
    pub smart_include: Option<Vec<String>>,
    /// When `target == smart`: name must not contain any keyword.
    #[serde(default)]
    pub smart_exclude: Option<Vec<String>>,
    /// Required when `target == chain`.
    #[serde(default)]
    pub chain_id: Option<String>,
}

/// Persisting is done; queue one globally debounced restart and return.
fn apply_running(app: &AppHandle) {
    crate::rule_apply::request_restart(app.clone(), Vec::new());
}

/// Whether any rule set is enabled. Crossing zero enabled sets switches the
/// builder between grouped sets and the legacy flat rule list, which does
/// change the generated config even for empty sets.
fn any_rule_set_enabled(store: &crate::storage::AppStore) -> bool {
    store.rule_sets.iter().any(|set| set.enabled)
}

/// Priority order of enabled sets that actually contribute rules. Reordering
/// only reaches the kernel config when this sequence changes.
fn enabled_contributing_ids(state: &AppState) -> Vec<String> {
    state
        .with_store(|store| {
            Ok(store
                .enabled_rule_sets()
                .iter()
                .filter(|set| !crate::config::rule_set_is_empty_for_config(set))
                .map(|set| set.id.clone())
                .collect())
        })
        .unwrap_or_default()
}

/// Write Clash `.list` for a set under app data.
fn dump_set(state: &AppState, set_id: &str) {
    let set = state
        .with_store(|s| Ok(s.get_rule_set(set_id).cloned()))
        .ok()
        .flatten();
    if let Some(set) = set {
        if let Err(e) = dump_rule_set_files(&state.app_data_dir, &set) {
            eprintln!("[satelite] dump rule files {set_id}: {e}");
        }
    }
}

#[tauri::command]
pub fn list_rule_sets(state: State<'_, AppState>) -> Result<Vec<RuleSetSummary>, String> {
    state
        .with_store(|store| Ok(store.list_rule_set_summaries()))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_rule_set(state: State<'_, AppState>, id: String) -> Result<RuleSet, String> {
    state
        .with_store(|store| {
            store
                .get_rule_set(&id)
                .cloned()
                .ok_or_else(|| crate::error::AppError::NotFound(id))
        })
        .map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
pub struct RemoteRuleItem {
    pub index: u32,
    pub kind: String,
    pub summary: String,
    pub raw: String,
    pub raw_truncated: bool,
    pub complex: bool,
}

#[derive(Debug, Serialize)]
pub struct RemoteRulePage {
    pub total: u32,
    pub offset: u32,
    pub limit: u32,
    pub items: Vec<RemoteRuleItem>,
}

fn compact_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(values) => {
            values
                .iter()
                .take(5)
                .map(compact_value)
                .collect::<Vec<_>>()
                .join(", ")
                + if values.len() > 5 { " …" } else { "" }
        }
        serde_json::Value::Object(object) => format!("{{{} 个字段}}", object.len()),
        other => other.to_string(),
    }
}

fn describe_remote_rule(value: &serde_json::Value) -> (String, String) {
    let Some(object) = value.as_object() else {
        return ("UNKNOWN".into(), compact_value(value));
    };
    let kind = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(|value| value.to_ascii_uppercase())
        .or_else(|| object.keys().find(|key| key.as_str() != "invert").cloned())
        .unwrap_or_else(|| "UNKNOWN".into());
    let mut parts = object
        .iter()
        .filter(|(key, _)| key.as_str() != "type")
        .take(4)
        .map(|(key, value)| format!("{key}: {}", compact_value(value)))
        .collect::<Vec<_>>();
    if object.len() > parts.len() + usize::from(object.contains_key("type")) {
        parts.push("…".into());
    }
    (kind, parts.join(" · "))
}

fn capped_pretty_json(value: &serde_json::Value) -> (String, bool) {
    const MAX_CHARS: usize = 4_000;
    let raw = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let mut chars = raw.chars();
    let capped = chars.by_ref().take(MAX_CHARS).collect::<String>();
    let truncated = chars.next().is_some();
    (capped, truncated)
}

enum RemoteRuleView<'a> {
    Whole(&'a serde_json::Value),
    Scalar {
        field: &'a str,
        value: &'a serde_json::Value,
        invert: Option<&'a serde_json::Value>,
    },
}

fn expand_remote_rules(rules: &[serde_json::Value]) -> Vec<RemoteRuleView<'_>> {
    let mut expanded = Vec::new();
    for rule in rules {
        let Some(object) = rule.as_object() else {
            expanded.push(RemoteRuleView::Whole(rule));
            continue;
        };
        // Logical/nested rules must remain grouped. Ordinary matcher objects
        // are flattened field by field for read-only display only.
        if crate::domain::remote_rule_is_complex(rule) {
            expanded.push(RemoteRuleView::Whole(rule));
            continue;
        }
        let invert = object.get("invert");
        let mut added = false;
        for (field, value) in object.iter().filter(|(field, _)| *field != "invert") {
            if let Some(values) = value.as_array() {
                for value in values {
                    expanded.push(RemoteRuleView::Scalar {
                        field,
                        value,
                        invert,
                    });
                    added = true;
                }
            } else {
                expanded.push(RemoteRuleView::Scalar {
                    field,
                    value,
                    invert,
                });
                added = true;
            }
        }
        if !added {
            expanded.push(RemoteRuleView::Whole(rule));
        }
    }
    expanded
}

fn remote_view_matches(view: &RemoteRuleView<'_>, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    match view {
        RemoteRuleView::Whole(value) => value.to_string().to_lowercase().contains(query),
        RemoteRuleView::Scalar {
            field,
            value,
            invert,
        } => {
            field.to_lowercase().contains(query)
                || compact_value(value).to_lowercase().contains(query)
                || invert.is_some_and(|value| compact_value(value).to_lowercase().contains(query))
        }
    }
}

fn remote_view_item(index: usize, view: RemoteRuleView<'_>) -> RemoteRuleItem {
    let (kind, summary, raw_value, complex) = match view {
        RemoteRuleView::Whole(value) => {
            let (kind, summary) = describe_remote_rule(value);
            (kind, summary, value.clone(), true)
        }
        RemoteRuleView::Scalar {
            field,
            value,
            invert,
        } => {
            let mut object = serde_json::Map::new();
            object.insert(field.to_string(), value.clone());
            let mut summary = compact_value(value);
            if let Some(invert) = invert {
                object.insert("invert".into(), invert.clone());
                summary.push_str(&format!(" · invert: {}", compact_value(invert)));
            }
            (
                field.to_string(),
                summary,
                serde_json::Value::Object(object),
                false,
            )
        }
    };
    let (raw, raw_truncated) = capped_pretty_json(&raw_value);
    RemoteRuleItem {
        index: u32::try_from(index + 1).unwrap_or(u32::MAX),
        kind,
        summary,
        raw,
        raw_truncated,
        complex,
    }
}

/// Parse a downloaded sing-box source or binary rule set for read-only display.
#[tauri::command]
pub async fn list_remote_rule_items(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    offset: u32,
    limit: u32,
    query: Option<String>,
) -> Result<RemoteRulePage, String> {
    let (local_path, format) =
        state
            .with_store(|store| {
                let set = store
                    .get_rule_set(&id)
                    .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))?;
                let remote = set.remote.as_ref().ok_or_else(|| {
                    crate::error::AppError::Config("该规则集不是远程规则集".into())
                })?;
                let path = remote.local_path.clone().ok_or_else(|| {
                    crate::error::AppError::Config("远程规则集尚未下载完成".into())
                })?;
                Ok((path, remote.format.clone()))
            })
            .map_err(|error| error.to_string())?;

    let cache_dir = crate::portable::resolve_app_data_dir(&app)
        .map_err(|error| error.to_string())?
        .join("remote-rule-sets")
        .canonicalize()
        .map_err(|error| format!("远程规则缓存目录不可用: {error}"))?;
    let path = std::path::PathBuf::from(local_path)
        .canonicalize()
        .map_err(|error| format!("远程规则缓存不可用: {error}"))?;
    if path.parent() != Some(cache_dir.as_path()) {
        return Err("远程规则缓存路径无效".into());
    }
    let query = query.unwrap_or_default();
    let persist_count = query.trim().is_empty();
    let core = if format == "binary" {
        let resource_dir = app.path().resource_dir().ok();
        crate::core::resolve_core_bin(
            &state.app_data_dir,
            resource_dir.as_deref(),
            crate::core::CoreKind::SingBox,
        )
        .0
    } else {
        None
    };
    let page = tauri::async_runtime::spawn_blocking(move || {
        let bytes = if format == "binary" {
            let raw = std::fs::read(&path).map_err(|error| error.to_string())?;
            let scan = crate::srs::parse(&raw).map_err(|error| format!("SRS 解析失败: {error}"))?;
            if scan.has_adguard {
                // AdGuard rule-sets cannot be decompiled by sing-box; serve
                // the rules rebuilt from the binary structure instead.
                let parsed = crate::srs::parse_with_rules(&raw)
                    .map_err(|error| format!("SRS 解析失败: {error}"))?;
                serde_json::to_vec(&parsed.display_source()).map_err(|error| error.to_string())?
            } else {
                let core = core.ok_or_else(|| "无法查看 SRS：sing-box 内核不可用".to_string())?;
                crate::remote_rule_auto::decompile_srs(&core, &path)?
            }
        } else {
            std::fs::read(&path).map_err(|error| error.to_string())?
        };
        parse_remote_rule_bytes(&bytes, offset, limit, &query)
    })
    .await
    .map_err(|error| error.to_string())??;
    if persist_count {
        let needs_update = state
            .with_store(|store| {
                Ok(store
                    .rule_sets
                    .iter()
                    .find(|set| set.id == id)
                    .and_then(|set| set.remote.as_ref())
                    .is_some_and(|remote| remote.rule_count != Some(page.total)))
            })
            .map_err(|error| error.to_string())?;
        if needs_update {
            state
                .with_store_mut(|store| {
                    if let Some(remote) = store
                        .rule_sets
                        .iter_mut()
                        .find(|set| set.id == id)
                        .and_then(|set| set.remote.as_mut())
                    {
                        remote.rule_count = Some(page.total);
                    }
                    Ok(())
                })
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(page)
}

fn parse_remote_rule_bytes(
    bytes: &[u8],
    offset: u32,
    limit: u32,
    query: &str,
) -> Result<RemoteRulePage, String> {
    let source: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| format!("无法解析远程规则缓存: {error}"))?;
    let rules = source
        .get("rules")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "远程规则缓存缺少 rules 数组".to_string())?;
    let query = query.trim().to_lowercase();
    let filtered = expand_remote_rules(rules)
        .into_iter()
        .enumerate()
        .filter(|(_, view)| remote_view_matches(view, &query));
    let limit = limit.clamp(1, 100);
    let matched = filtered.collect::<Vec<_>>();
    let total = u32::try_from(matched.len()).unwrap_or(u32::MAX);
    let items = matched
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|(index, view)| remote_view_item(index, view))
        .collect();
    Ok(RemoteRulePage {
        total,
        offset,
        limit,
        items,
    })
}

#[cfg(test)]
mod remote_rule_view_tests {
    use super::*;

    #[test]
    fn splits_single_scalar_array_into_rows() {
        let rules = vec![serde_json::json!({
            "domain": ["one.example", "two.example"],
            "invert": true
        })];
        let views = expand_remote_rules(&rules);
        assert_eq!(views.len(), 2);
        let first = remote_view_item(0, views.into_iter().next().unwrap());
        assert_eq!(first.kind, "domain");
        assert_eq!(first.summary, "one.example · invert: true");
        assert!(!first.complex);
    }

    #[test]
    fn splits_multiple_scalar_fields_but_keeps_nested_rules_grouped() {
        let rules = vec![
            serde_json::json!({"domain": ["example.com"], "domain_suffix": ["example.org", "example.net"]}),
            serde_json::json!({"type": "logical", "rules": [{"domain": ["example.org"]}]}),
        ];
        let mut views = expand_remote_rules(&rules);
        assert_eq!(views.len(), 4);
        let logical_view = views.pop().unwrap();
        let kinds = views
            .into_iter()
            .take(3)
            .enumerate()
            .map(|(index, view)| remote_view_item(index, view).kind)
            .collect::<Vec<_>>();
        assert_eq!(kinds, ["domain", "domain_suffix", "domain_suffix"]);
        let logical = remote_view_item(3, logical_view);
        assert!(logical.complex);
    }
}

#[tauri::command]
pub fn set_active_rule_set(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // Back-compat: enable this set (does not disable others).
    // An effectively-empty set never reaches the kernel config, so enabling
    // it needs no restart unless the zero-enabled-sets boundary is crossed.
    let skip_restart = state
        .with_store_mut(|store| {
            let boundary_before = any_rule_set_enabled(store);
            store.set_rule_set_enabled(&id, true)?;
            Ok(any_rule_set_enabled(store) == boundary_before
                && store
                    .get_rule_set(&id)
                    .is_some_and(crate::config::rule_set_is_empty_for_config))
        })
        .map_err(|e| e.to_string())?;
    if !skip_restart {
        apply_running(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn set_rule_set_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    // Empty sets never reach the kernel config; skip the restart (and just
    // ack the UI) unless this toggle crosses the zero-enabled-sets boundary,
    // where the builder would fall back to the legacy flat rule list.
    let skip_restart = state
        .with_store_mut(|store| {
            let boundary_before = any_rule_set_enabled(store);
            store.set_rule_set_enabled(&id, enabled)?;
            Ok(any_rule_set_enabled(store) == boundary_before
                && store
                    .get_rule_set(&id)
                    .is_some_and(crate::config::rule_set_is_empty_for_config))
        })
        .map_err(|e| e.to_string())?;
    // Persist resolves immediately; restart runs in the background and
    // reports via the `rule-set-apply-status` event (see `rule_apply`).
    if skip_restart {
        crate::rule_apply::emit_ready_without_restart(&app, &id, enabled);
    } else {
        crate::rule_apply::request_apply(app, id, enabled);
    }
    Ok(())
}

#[tauri::command]
pub fn set_rule_set_strategy(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    strategy: RuleSetStrategy,
) -> Result<RuleSet, String> {
    let (set, needs_restart) = state
        .with_store_mut(|store| {
            let set = store
                .rule_sets
                .iter_mut()
                .find(|set| set.id == id)
                .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))?;
            if set.remote.is_some() && strategy == RuleSetStrategy::Smart {
                return Err(crate::error::AppError::Config(
                    "远程规则集没有单条规则，无法转为混合策略".into(),
                ));
            }
            // Node/Filter carry whole-set parameters (node pin / keywords)
            // that this command cannot accept — the batch path owns them.
            if matches!(strategy, RuleSetStrategy::Node | RuleSetStrategy::Filter) {
                return Err(crate::error::AppError::Config(
                    "指定 / 过滤策略需要节点或关键词参数，请在「批量设置路由」中修改".into(),
                ));
            }
            set.strategy = strategy;
            // Plain strategies own every rule's target: retarget all local
            // rules so flipping a set keeps its one-knob meaning (stale
            // node/smart pins from an earlier smart phase must not linger).
            // Flipping TO smart keeps current targets for per-rule editing.
            if set.remote.is_none() && strategy != RuleSetStrategy::Smart {
                let fallback = match strategy {
                    RuleSetStrategy::Direct => RuleTarget::Direct,
                    RuleSetStrategy::Block => RuleTarget::Block,
                    _ => RuleTarget::Proxy,
                };
                for rule in set.rules.iter_mut() {
                    rule.target = fallback.clone();
                }
            }
            if let Some(dns_strategy) = strategy.recommended_dns_strategy() {
                set.dns_strategy = dns_strategy;
            }
            if let Some(remote) = set.remote.as_mut() {
                if let Some(target) = strategy.route_target() {
                    remote.target = target;
                }
            }
            // The strategy of an effectively-empty set reaches nothing in the
            // generated config — no restart needed for those.
            let needs_restart = !crate::config::rule_set_is_empty_for_config(set);
            Ok((set.clone(), needs_restart))
        })
        .map_err(|e| e.to_string())?;
    if needs_restart {
        apply_running(&app);
    }
    Ok(set)
}

#[tauri::command]
pub fn batch_set_rule_targets(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    target: RuleTarget,
    node_id: Option<String>,
    smart_include: Option<Vec<String>>,
    smart_exclude: Option<Vec<String>>,
    chain_id: Option<String>,
) -> Result<RuleSet, String> {
    let (set, needs_restart) = state
        .with_store_mut(|store| {
            store.batch_set_rule_targets(
                &id,
                target,
                node_id,
                smart_include.unwrap_or_default(),
                smart_exclude.unwrap_or_default(),
                chain_id,
            )
        })
        .map_err(|e| e.to_string())?;
    if needs_restart {
        apply_running(&app);
    }
    Ok(set)
}

#[tauri::command]
pub fn set_rule_set_dns_strategy(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    strategy: RuleSetDnsStrategy,
) -> Result<RuleSet, String> {
    let (set, needs_restart) = state
        .with_store_mut(|store| {
            let set = store
                .rule_sets
                .iter_mut()
                .find(|set| set.id == id)
                .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))?;
            set.dns_strategy = strategy;
            // DNS policy of an effectively-empty set emits no grouped DNS
            // rule, so the kernel config is unchanged — skip the restart.
            let needs_restart = !crate::config::rule_set_is_empty_for_config(set);
            Ok((set.clone(), needs_restart))
        })
        .map_err(|e| e.to_string())?;
    if needs_restart {
        apply_running(&app);
    }
    Ok(set)
}

/// Reorder rule sets. `ids` is full preferred order (first = highest priority).
#[tauri::command]
pub fn reorder_rule_sets(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<Vec<RuleSetSummary>, String> {
    if ids.is_empty() {
        return Err("ids is empty".into());
    }
    // Empty sets are skipped by the config builder, so only a changed order
    // of contributing sets can affect the kernel.
    let contributing_before = enabled_contributing_ids(&state);
    state
        .with_store_mut(|store| store.reorder_rule_sets(&ids))
        .map_err(|e| e.to_string())?;
    if enabled_contributing_ids(&state) != contributing_before {
        // Order is already saved; restart failure must not revert UI order.
        apply_running(&app);
    }
    state
        .with_store(|store| Ok(store.list_rule_set_summaries()))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_rule_set(
    state: State<'_, AppState>,
    name: String,
    remote_url: Option<String>,
    target: Option<RuleTarget>,
    update_interval: Option<String>,
    node_id: Option<String>,
    smart_include: Option<Vec<String>>,
    smart_exclude: Option<Vec<String>>,
    chain_id: Option<String>,
) -> Result<RuleSet, String> {
    let set = state
        .with_store_mut(|store| {
            let n = name.trim();
            if n.is_empty() {
                return Err(crate::error::AppError::Config("规则集名称不能为空".into()));
            }
            if n.chars().count() > 64 {
                return Err(crate::error::AppError::Config(
                    "规则集名称过长（最多 64 字）".into(),
                ));
            }
            // Avoid duplicate names (case-insensitive)
            if store
                .rule_sets
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(n))
            {
                return Err(crate::error::AppError::Config(format!(
                    "已存在同名规则集「{n}」"
                )));
            }
            let target = target.unwrap_or(RuleTarget::Proxy);
            let node_id = node_id.filter(|v| !v.trim().is_empty());
            let smart_include = smart_include.unwrap_or_default();
            let smart_exclude = smart_exclude.unwrap_or_default();
            if let Some(url) = remote_url
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(crate::error::AppError::Config(
                        "远程规则集 URL 必须以 http:// 或 https:// 开头".into(),
                    ));
                }
                let update_interval = update_interval.as_deref().unwrap_or("disabled");
                let update_interval = crate::domain::normalize_remote_update_interval(
                    update_interval,
                )
                .ok_or_else(|| {
                    crate::error::AppError::Config("自动更新周期必须是 disabled/1h/12h/24h".into())
                })?;
                store.create_remote_rule_set(
                    n,
                    url,
                    target,
                    update_interval,
                    node_id,
                    smart_include,
                    smart_exclude,
                    chain_id,
                )
            } else {
                // Local set: an optional initial whole-set route from the
                // new-set dialog (node/smart carry the set-level pin /
                // keyword filters; Mixed stays an emergent per-rule state).
                store.create_local_rule_set(
                    n,
                    target,
                    node_id,
                    smart_include,
                    smart_exclude,
                    chain_id,
                )
            }
        })
        .map_err(|e| e.to_string())?;
    dump_set(&state, &set.id);
    Ok(set)
}

#[tauri::command]
pub fn update_rule_set(
    state: State<'_, AppState>,
    id: String,
    name: String,
    remote_url: Option<String>,
    update_interval: Option<String>,
) -> Result<RuleSet, String> {
    let set = state
        .with_store_mut(|store| {
            let name = name.trim();
            if name.is_empty() {
                return Err(crate::error::AppError::Config("规则集名称不能为空".into()));
            }
            if name.chars().count() > 64 {
                return Err(crate::error::AppError::Config(
                    "规则集名称过长（最多 64 字）".into(),
                ));
            }
            if store
                .rule_sets
                .iter()
                .any(|set| set.id != id && set.name.eq_ignore_ascii_case(name))
            {
                return Err(crate::error::AppError::Config(format!(
                    "已存在同名规则集「{name}」"
                )));
            }
            let set = store
                .rule_sets
                .iter_mut()
                .find(|set| set.id == id)
                .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))?;
            set.name = name.to_string();
            if let Some(remote) = set.remote.as_mut() {
                let url = remote_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        crate::error::AppError::Config("远程规则集 URL 不能为空".into())
                    })?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(crate::error::AppError::Config(
                        "远程规则集 URL 必须以 http:// 或 https:// 开头".into(),
                    ));
                }
                if crate::domain::is_builtin_remote_id(&id) && url != remote.url {
                    return Err(crate::error::AppError::Config(
                        "内置规则集的 URL 不能修改".into(),
                    ));
                }
                let interval = update_interval.as_deref().unwrap_or("disabled");
                let interval = crate::domain::normalize_remote_update_interval(interval)
                    .ok_or_else(|| {
                        crate::error::AppError::Config(
                            "自动更新周期必须是 disabled/1h/12h/24h".into(),
                        )
                    })?;
                remote.url = url.to_string();
                remote.update_interval = interval.to_string();
            }
            Ok(set.clone())
        })
        .map_err(|error| error.to_string())?;
    dump_set(&state, &id);
    Ok(set)
}

/// Download a remote source through Rust and atomically switch sing-box to the
/// resulting local cache file. Network and restart work run off the UI thread.
#[tauri::command]
pub async fn refresh_remote_rule_set(app: AppHandle, id: String) -> Result<RuleSet, String> {
    crate::remote_rule_auto::refresh(app, id).await
}

#[tauri::command]
pub fn delete_rule_set(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let (cached_path, was_contributing, boundary_before) = state
        .with_store(|store| {
            let set = store.get_rule_set(&id);
            Ok((
                set.and_then(|set| set.remote.as_ref())
                    .and_then(|remote| remote.local_path.clone()),
                set.is_some_and(|set| !crate::config::rule_set_is_empty_for_config(set)),
                any_rule_set_enabled(store),
            ))
        })
        .map_err(|e| e.to_string())?;
    state
        .with_store_mut(|store| store.delete_rule_set(&id))
        .map_err(|e| e.to_string())?;
    remove_rule_set_files(&state.app_data_dir, &id);
    if let Some(path) = cached_path.map(std::path::PathBuf::from) {
        let cache_dir = state.app_data_dir.join("remote-rule-sets");
        if path.parent() == Some(cache_dir.as_path()) {
            let _ = std::fs::remove_file(path);
        }
    }
    // Deleting an effectively-empty set changes nothing the kernel sees,
    // unless it was the last enabled set (legacy fallback boundary).
    let boundary_after = state
        .with_store(|store| Ok(any_rule_set_enabled(store)))
        .unwrap_or(true);
    if was_contributing || boundary_before != boundary_after {
        apply_running(&app);
    }
    Ok(())
}

/// Reset one factory rule set (the bundled `system-*` remote sets) from its
/// packaged `.srs` copy. Anything else — including legacy `builtin-*` ids —
/// is not resettable.
#[tauri::command]
pub fn reset_rule_set(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<RuleSet, String> {
    let (set, stale_cache) = state
        .with_store_mut(|store| {
            store.reset_rule_set(&state.app_data_dir, state.resource_dir.as_deref(), &id)
        })
        .map_err(|e| e.to_string())?;
    remove_rule_set_files(&state.app_data_dir, &set.id);
    // Superseded cache files are deleted only after the core restart
    // (the running core may still be reading them).
    crate::rule_apply::request_restart(app, stale_cache);
    Ok(set)
}

/// Reset the three bundled remote rule sets to factory defaults. Legacy
/// `builtin-*` list sets stay untouched — recognized but never restored.
#[tauri::command]
pub fn reset_builtin_rule_set(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RuleSet, String> {
    let (restored, stale_cache) = state
        .with_store_mut(|store| {
            let (sets, stale, export_ids) = store
                .reset_all_builtin_rule_sets(&state.app_data_dir, state.resource_dir.as_deref());
            for id in &export_ids {
                remove_rule_set_files(&state.app_data_dir, id);
            }
            sets.into_iter()
                .next()
                .map(|set| (set, stale))
                .ok_or_else(|| crate::error::AppError::NotFound("builtin remote rule sets".into()))
        })
        .map_err(|e| e.to_string())?;
    crate::rule_apply::request_restart(app, stale_cache);
    Ok(restored)
}

/// List rules of a set (default: active set).
#[tauri::command]
pub fn list_rules(state: State<'_, AppState>, set_id: Option<String>) -> Result<Vec<Rule>, String> {
    state
        .with_store(|store| {
            let id = set_id.unwrap_or_else(|| {
                store
                    .rule_sets
                    .iter()
                    .find(|s| s.enabled)
                    .map(|s| s.id.clone())
                    .unwrap_or_else(|| crate::domain::BUILTIN_SET_ID.into())
            });
            let set = store
                .get_rule_set(&id)
                .ok_or_else(|| crate::error::AppError::NotFound(id))?;
            let mut rules = set.rules.clone();
            rules.sort_by_key(|r| r.ord);
            Ok(rules)
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_rule(
    app: AppHandle,
    state: State<'_, AppState>,
    input: SaveRuleInput,
) -> Result<Rule, String> {
    let (rule, needs_restart) = state
        .with_store_mut(|store| {
            if matches!(input.rule_type, RuleType::Geoip) {
                return Err(crate::error::AppError::Config(
                    "GEOIP 规则已不被 sing-box 1.12+ 支持，请改用 DOMAIN-SUFFIX / IP-CIDR".into(),
                ));
            }
            let payload = input.payload.trim().to_string();
            if payload.is_empty() {
                return Err(crate::error::AppError::Config("payload empty".into()));
            }
            let set_id = input.set_id.clone().unwrap_or_else(|| {
                store
                    .rule_sets
                    .iter()
                    .find(|s| s.enabled && s.remote.is_none())
                    .map(|s| s.id.clone())
                    .unwrap_or_default()
            });

            let set = store
                .get_rule_set(&set_id)
                .ok_or_else(|| crate::error::AppError::NotFound(set_id.clone()))?;
            if set.remote.is_some() {
                return Err(crate::error::AppError::Config(
                    "远程规则集不能编辑单项".into(),
                ));
            }
            let set_empty_before = crate::config::rule_set_is_empty_for_config(set);
            let effective_target = set.strategy.route_target().unwrap_or(input.target);

            let ord = input
                .ord
                .unwrap_or_else(|| set.rules.iter().map(|r| r.ord).max().unwrap_or(0) + 10);

            // Resolve pin fields for target=node (snapshot name for stale UI).
            let (node_id, node_name) = if matches!(effective_target, RuleTarget::Node) {
                let nid = input
                    .node_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        crate::error::AppError::Config("指定节点出口需要选择一个节点".into())
                    })?;
                let stored = store
                    .nodes
                    .iter()
                    .find(|n| n.node.id == nid)
                    .ok_or_else(|| {
                        crate::error::AppError::Config(
                            "指定的节点不存在或已从订阅中移除，请重新选择".into(),
                        )
                    })?;
                (Some(stored.node.id.clone()), Some(stored.node.name.clone()))
            } else {
                (None, None)
            };

            let (smart_include, smart_exclude) = if matches!(effective_target, RuleTarget::Smart) {
                let include =
                    Rule::normalize_keywords(input.smart_include.as_deref().unwrap_or(&[]));
                let exclude =
                    Rule::normalize_keywords(input.smart_exclude.as_deref().unwrap_or(&[]));
                let overlap = crate::domain::keyword_list_overlap(&include, &exclude);
                if !overlap.is_empty() {
                    return Err(crate::error::AppError::Config(format!(
                        "智能模式：关键字不能同时出现在白名单与黑名单中：{}",
                        overlap.join("、")
                    )));
                }
                let match_count = store
                    .enabled_nodes()
                    .iter()
                    .filter(|n| crate::domain::name_matches_keywords(&n.name, &include, &exclude))
                    .count();
                if match_count == 0 {
                    return Err(crate::error::AppError::Config(
                        "智能模式：当前没有符合关键字条件的节点，请调整白名单/黑名单或先导入订阅"
                            .into(),
                    ));
                }
                (include, exclude)
            } else {
                (Vec::new(), Vec::new())
            };

            let (chain_id, chain_name) = if matches!(effective_target, RuleTarget::Chain) {
                let cid = input
                    .chain_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        crate::error::AppError::Config("链路出口需要选择一个链路".into())
                    })?;
                let chain = store.chains.iter().find(|c| c.id == cid).ok_or_else(|| {
                    crate::error::AppError::Config("指定的链路不存在，请重新选择".into())
                })?;
                (Some(chain.id.clone()), Some(chain.name.clone()))
            } else {
                (None, None)
            };

            let rule = if let Some(id) = input.id.clone() {
                if let Some(existing) = set.rules.iter().find(|r| r.id == id) {
                    let mut r = existing.clone();
                    r.rule_type = input.rule_type;
                    r.payload = payload;
                    r.target = effective_target;
                    r.ord = ord;
                    r.node_id = node_id;
                    r.node_name = node_name;
                    r.smart_include = smart_include;
                    r.smart_exclude = smart_exclude;
                    r.chain_id = chain_id;
                    r.chain_name = chain_name;
                    if let Some(en) = input.enabled {
                        r.enabled = en;
                    }
                    r
                } else {
                    let mut r = Rule::new(input.rule_type, payload, effective_target, ord);
                    r.id = id;
                    r.node_id = node_id;
                    r.node_name = node_name;
                    r.smart_include = smart_include;
                    r.smart_exclude = smart_exclude;
                    r.chain_id = chain_id;
                    r.chain_name = chain_name;
                    if let Some(en) = input.enabled {
                        r.enabled = en;
                    }
                    r
                }
            } else {
                let mut r = Rule::new(input.rule_type, payload, effective_target, ord);
                r.node_id = node_id;
                r.node_name = node_name;
                r.smart_include = smart_include;
                r.smart_exclude = smart_exclude;
                r.chain_id = chain_id;
                r.chain_name = chain_name;
                if matches!(input.target, RuleTarget::Smart | RuleTarget::Chain) {
                    r.id = Rule::compute_id(
                        r.rule_type,
                        &r.payload,
                        r.target,
                        None,
                        &r.smart_include,
                        &r.smart_exclude,
                        r.chain_id.as_deref(),
                    );
                }
                if let Some(en) = input.enabled {
                    r.enabled = en;
                }
                r
            };

            let rule = store.upsert_rule_in_set(&set_id, rule)?;
            // Edits that keep the set effectively empty (all rules disabled
            // or unmatchable) cannot change the kernel config.
            let set_empty_after = store
                .get_rule_set(&set_id)
                .is_some_and(crate::config::rule_set_is_empty_for_config);
            Ok((rule, !(set_empty_before && set_empty_after)))
        })
        .map_err(|e| e.to_string())?;
    // Dual files: Clash route list + optional SYSTEM DNS sidecar.
    if let Some(sid) = rule_set_id_of(&state, &rule) {
        dump_set(&state, &sid);
    } else if let Some(sid) = input.set_id.as_deref() {
        dump_set(&state, sid);
    }
    if needs_restart {
        apply_running(&app);
    }
    // Best-effort: pick best node for new/updated smart rule after core restarts.
    if matches!(rule.target, RuleTarget::Smart) && rule.enabled {
        let r = rule.clone();
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app2.try_state::<AppState>() {
                // Wait for the shared config-apply queue (including any
                // follow-up batch) before selecting the smart-rule outbound.
                while crate::rule_apply::is_pending(&state) {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                let _ = crate::smart_switch::refresh_smart_rule_now(&state, &r).await;
            }
        });
    }
    Ok(rule)
}

fn rule_set_id_of(state: &AppState, rule: &Rule) -> Option<String> {
    state
        .with_store(|store| {
            Ok(store
                .rule_sets
                .iter()
                .find(|s| s.rules.iter().any(|r| r.id == rule.id))
                .map(|s| s.id.clone()))
        })
        .ok()
        .flatten()
}

#[tauri::command]
pub fn remove_rule(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    set_id: Option<String>,
) -> Result<(), String> {
    let sid = match set_id {
        Some(sid) => sid,
        None => state
            .with_store(|store| {
                store
                    .rule_sets
                    .iter()
                    .find(|set| set.rules.iter().any(|rule| rule.id == id))
                    .map(|set| set.id.clone())
                    .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))
            })
            .map_err(|e| e.to_string())?,
    };
    let needs_restart = state
        .with_store_mut(|store| {
            // Removing a rule from an already-empty set (no effective rules)
            // cannot change the kernel config.
            let empty_before = store
                .get_rule_set(&sid)
                .is_some_and(crate::config::rule_set_is_empty_for_config);
            store.remove_rule_from_set(&sid, &id)?;
            let empty_after = store
                .get_rule_set(&sid)
                .is_some_and(crate::config::rule_set_is_empty_for_config);
            Ok(!(empty_before && empty_after))
        })
        .map_err(|e| e.to_string())?;
    dump_set(&state, &sid);
    if needs_restart {
        apply_running(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn set_rule_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
    set_id: Option<String>,
) -> Result<Rule, String> {
    let sid = match set_id {
        Some(sid) => sid,
        None => state
            .with_store(|store| {
                store
                    .rule_sets
                    .iter()
                    .find(|set| set.rules.iter().any(|rule| rule.id == id))
                    .map(|set| set.id.clone())
                    .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))
            })
            .map_err(|e| e.to_string())?,
    };
    let (rule, needs_restart) = state
        .with_store_mut(|store| {
            let set = store
                .rule_sets
                .iter_mut()
                .find(|s| s.id == sid)
                .ok_or_else(|| crate::error::AppError::NotFound(sid.clone()))?;
            let empty_before = crate::config::rule_set_is_empty_for_config(set);
            let rule = set
                .rules
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or_else(|| crate::error::AppError::NotFound(id))?;
            rule.enabled = enabled;
            let rule = rule.clone();
            // Toggling rules inside an effectively-empty set never reaches
            // the kernel config; only emptiness transitions need a restart.
            let empty_after = crate::config::rule_set_is_empty_for_config(set);
            Ok((rule, !(empty_before && empty_after)))
        })
        .map_err(|e| e.to_string())?;
    dump_set(&state, &sid);
    if needs_restart {
        apply_running(&app);
    }
    Ok(rule)
}
