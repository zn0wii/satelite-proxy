use crate::domain::{
    ManualNodeDraft, ProxyNode, SubscriptionDetail, SubscriptionSource, SubscriptionView,
};
use crate::services::import::{
    canonical_subscription_url, import_from_file, import_from_file_with_id, import_from_node,
    import_from_singbox, import_from_text, import_from_url_with_id,
};
use crate::state::AppState;
use crate::subscription::node_to_draft;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::State;
use tokio::sync::watch;

#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    pub subscription: SubscriptionView,
    pub node_count: u32,
    pub skipped_count: u32,
}

#[derive(Debug, Serialize)]
pub struct SubscriptionUrlEntry {
    pub id: String,
    pub url: String,
}

type SharedRefreshResult = Result<ImportResult, String>;
type RefreshSender = watch::Sender<Option<SharedRefreshResult>>;

static REFRESH_FLIGHTS: OnceLock<Mutex<HashMap<String, RefreshSender>>> = OnceLock::new();

enum RefreshFlight {
    Leader(RefreshLeader),
    Follower(watch::Receiver<Option<SharedRefreshResult>>),
}

struct RefreshLeader {
    id: String,
    sender: RefreshSender,
    finished: bool,
}

impl RefreshLeader {
    fn finish(mut self, result: SharedRefreshResult) -> SharedRefreshResult {
        self.sender.send_replace(Some(result.clone()));
        remove_refresh_flight(&self.id, &self.sender);
        self.finished = true;
        result
    }
}

impl Drop for RefreshLeader {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.sender
            .send_replace(Some(Err("订阅更新任务已取消".into())));
        remove_refresh_flight(&self.id, &self.sender);
    }
}

fn refresh_flights() -> &'static Mutex<HashMap<String, RefreshSender>> {
    REFRESH_FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn begin_refresh_flight(id: &str) -> Result<RefreshFlight, String> {
    let mut flights = refresh_flights()
        .lock()
        .map_err(|_| "subscription refresh lock poisoned".to_string())?;
    if let Some(sender) = flights.get(id) {
        return Ok(RefreshFlight::Follower(sender.subscribe()));
    }
    let (sender, _) = watch::channel(None);
    flights.insert(id.to_string(), sender.clone());
    Ok(RefreshFlight::Leader(RefreshLeader {
        id: id.to_string(),
        sender,
        finished: false,
    }))
}

fn remove_refresh_flight(id: &str, sender: &RefreshSender) {
    let Ok(mut flights) = refresh_flights().lock() else {
        return;
    };
    if flights
        .get(id)
        .is_some_and(|current| current.same_channel(sender))
    {
        flights.remove(id);
    }
}

async fn wait_for_refresh(
    mut receiver: watch::Receiver<Option<SharedRefreshResult>>,
) -> SharedRefreshResult {
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result;
        }
        receiver
            .changed()
            .await
            .map_err(|_| "订阅更新任务已取消".to_string())?;
    }
}

#[tauri::command(async)]
pub fn list_subscriptions(state: State<'_, AppState>) -> Result<Vec<SubscriptionView>, String> {
    state
        .with_store(|store| Ok(store.subscriptions.iter().map(|s| s.to_view()).collect()))
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn list_subscription_urls(
    state: State<'_, AppState>,
) -> Result<Vec<SubscriptionUrlEntry>, String> {
    state
        .with_store(|store| {
            Ok(store
                .subscriptions
                .iter()
                .filter_map(|subscription| match &subscription.source {
                    SubscriptionSource::Url { url } => Some(SubscriptionUrlEntry {
                        id: subscription.id.clone(),
                        url: url.clone(),
                    }),
                    _ => None,
                })
                .collect())
        })
        .map_err(|error| error.to_string())
}

#[tauri::command(async)]
pub fn get_subscription(
    state: State<'_, AppState>,
    id: String,
) -> Result<SubscriptionDetail, String> {
    state
        .with_store(|store| {
            let mut detail = store
                .get_subscription(&id)
                .map(|s| s.to_detail())
                .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))?;
            if detail.source_kind == "node" {
                detail.node = store
                    .nodes
                    .iter()
                    .find(|n| n.subscription_id == id)
                    .map(|n| node_to_draft(&n.node));
            }
            Ok(detail)
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_subscription_url(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: Option<String>,
    url: String,
    via_proxy: Option<bool>,
    auto_update: Option<bool>,
    auto_update_interval_min: Option<u32>,
) -> Result<ImportResult, String> {
    let via = via_proxy.unwrap_or(false);
    let canonical = canonical_subscription_url(&url);
    let existing_id = state
        .with_store(|store| {
            Ok(store
                .subscriptions
                .iter()
                .find_map(|subscription| match &subscription.source {
                    SubscriptionSource::Url { url: existing_url }
                        if canonical.is_some()
                            && canonical_subscription_url(existing_url) == canonical =>
                    {
                        Some(subscription.id.clone())
                    }
                    _ => None,
                }))
        })
        .map_err(|e| e.to_string())?;
    let mixed_port = state
        .with_store(|s| Ok(s.settings.mixed_port))
        .map_err(|e| e.to_string())?;
    let mut outcome = import_from_url_with_id(name, url, existing_id, via, Some(mixed_port))
        .await
        .map_err(|e| e.to_string())?;
    apply_auto_update_prefs(
        &mut outcome.subscription,
        auto_update.unwrap_or(false),
        auto_update_interval_min.unwrap_or(1440),
    );
    persist_import(&app, &state, outcome)
}

#[tauri::command]
pub async fn add_subscription_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: Option<String>,
    path: String,
    auto_update: Option<bool>,
    auto_update_interval_min: Option<u32>,
) -> Result<ImportResult, String> {
    let _ = (auto_update, auto_update_interval_min);
    let outcome = import_file_blocking(name, PathBuf::from(path), None).await?;
    persist_import(&app, &state, outcome)
}

#[tauri::command]
pub async fn add_subscription_text(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: Option<String>,
    content: String,
) -> Result<ImportResult, String> {
    let outcome = import_text_blocking(name, content, None).await?;
    persist_import(&app, &state, outcome)
}

#[tauri::command]
pub async fn add_subscription_node(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: Option<String>,
    uri: Option<String>,
    node: Option<ManualNodeDraft>,
) -> Result<ImportResult, String> {
    let outcome = import_node_blocking(name, uri, node, None).await?;
    persist_import(&app, &state, outcome)
}

#[tauri::command]
pub async fn add_subscription_singbox(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: Option<String>,
    content: Option<String>,
    path: Option<String>,
) -> Result<ImportResult, String> {
    let body = load_inline_body(content, path).await?;
    let outcome = import_singbox_blocking(name, body, None).await?;
    persist_import(&app, &state, outcome)
}

#[tauri::command(async)]
pub fn read_import_file(path: String) -> Result<String, String> {
    let path = PathBuf::from(path.trim());
    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.len() as usize > 8 * 1024 * 1024 {
        return Err("file too large (max 8 MB)".into());
    }
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// Update existing subscription. Keeps stable id. Re-imports nodes.
#[tauri::command]
pub async fn update_subscription(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    name: Option<String>,
    kind: String,
    url: Option<String>,
    path: Option<String>,
    content: Option<String>,
    uri: Option<String>,
    node: Option<ManualNodeDraft>,
    via_proxy: Option<bool>,
    auto_update: Option<bool>,
    auto_update_interval_min: Option<u32>,
) -> Result<ImportResult, String> {
    let existing = state
        .with_store(|store| {
            store
                .get_subscription(&id)
                .cloned()
                .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))
        })
        .map_err(|e| e.to_string())?;

    let display_name = name
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| existing.name.clone());

    let via = via_proxy.unwrap_or(existing.via_proxy);
    let mixed_port = state
        .with_store(|s| Ok(s.settings.mixed_port))
        .map_err(|e| e.to_string())?;

    let kind = kind.to_ascii_lowercase();
    let (outcome, replaced_id, replaced_enabled) = match kind.as_str() {
        "url" => {
            let url = url
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .ok_or_else(|| "url is required".to_string())?;
            let duplicate = state
                .with_store(|store| {
                    Ok(store.subscriptions.iter().find_map(|subscription| {
                        if subscription.id == id {
                            return None;
                        }
                        match &subscription.source {
                            SubscriptionSource::Url { url: existing_url }
                                if canonical_subscription_url(existing_url)
                                    == canonical_subscription_url(&url) =>
                            {
                                Some((subscription.id.clone(), subscription.enabled))
                            }
                            _ => None,
                        }
                    }))
                })
                .map_err(|e| e.to_string())?;
            let target_id = duplicate
                .as_ref()
                .map(|(duplicate_id, _)| duplicate_id.clone())
                .unwrap_or_else(|| id.clone());
            let outcome = import_from_url_with_id(
                Some(display_name),
                url,
                Some(target_id),
                via,
                Some(mixed_port),
            )
            .await
            .map_err(|e| e.to_string())?;
            let replaced_enabled = duplicate.as_ref().is_some_and(|(_, enabled)| *enabled);
            (outcome, duplicate.map(|_| id.clone()), replaced_enabled)
        }
        "file" => {
            let path = path
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .ok_or_else(|| "path is required".to_string())?;
            let mut o =
                import_file_blocking(Some(display_name), PathBuf::from(path), Some(id.clone()))
                    .await?;
            o.subscription.via_proxy = false;
            (o, None, false)
        }
        "text" => {
            let content = content
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .ok_or_else(|| "content is required".to_string())?;
            let mut o = import_text_blocking(Some(display_name), content, Some(id.clone())).await?;
            o.subscription.via_proxy = false;
            (o, None, false)
        }
        "node" => {
            let mut o =
                import_node_blocking(Some(display_name), uri, node, Some(id.clone())).await?;
            o.subscription.via_proxy = false;
            (o, None, false)
        }
        "singbox" => {
            let body = load_inline_body(content, path).await?;
            let mut o = import_singbox_blocking(Some(display_name), body, Some(id.clone())).await?;
            o.subscription.via_proxy = false;
            (o, None, false)
        }
        _ => return Err("kind must be url, file, text, node, or singbox".into()),
    };

    let mut outcome = outcome;
    outcome.subscription.enabled = existing.enabled || replaced_enabled;
    if outcome.subscription.source.is_remote() {
        apply_auto_update_prefs(
            &mut outcome.subscription,
            auto_update.unwrap_or(existing.auto_update),
            auto_update_interval_min.unwrap_or(existing.auto_update_interval_min),
        );
    } else {
        outcome.subscription.auto_update = false;
    }

    persist_import_replacing(&app, &state, outcome, replaced_id.as_deref())
}

#[tauri::command]
pub async fn refresh_subscription(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
    via_proxy: Option<bool>,
) -> Result<ImportResult, String> {
    refresh_subscription_inner(&app, &state, id, via_proxy).await
}

fn apply_auto_update_prefs(
    sub: &mut crate::domain::Subscription,
    auto_update: bool,
    interval_min: u32,
) {
    sub.auto_update = auto_update;
    sub.auto_update_interval_min = interval_min.max(1);
}

/// Internal refresh used by the auto-update scheduler (no Tauri State).
pub async fn refresh_subscription_by_id(
    app: &tauri::AppHandle,
    state: &AppState,
    id: &str,
) -> Result<ImportResult, String> {
    refresh_subscription_inner(app, state, id.to_string(), None).await
}

async fn refresh_subscription_inner(
    app: &tauri::AppHandle,
    state: &AppState,
    id: String,
    via_proxy: Option<bool>,
) -> Result<ImportResult, String> {
    match begin_refresh_flight(&id)? {
        RefreshFlight::Follower(receiver) => wait_for_refresh(receiver).await,
        RefreshFlight::Leader(leader) => {
            let result = refresh_subscription_once(app, state, id, via_proxy).await;
            leader.finish(result)
        }
    }
}

async fn refresh_subscription_once(
    app: &tauri::AppHandle,
    state: &AppState,
    id: String,
    via_proxy: Option<bool>,
) -> Result<ImportResult, String> {
    let existing = state
        .with_store(|store| {
            store
                .get_subscription(&id)
                .cloned()
                .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))
        })
        .map_err(|e| e.to_string())?;

    let via = via_proxy.unwrap_or(existing.via_proxy);
    let mixed_port = state
        .with_store(|s| Ok(s.settings.mixed_port))
        .map_err(|e| e.to_string())?;

    let mut outcome = match &existing.source {
        crate::domain::SubscriptionSource::Url { url } => import_from_url_with_id(
            Some(existing.name.clone()),
            url.clone(),
            Some(id.clone()),
            via,
            Some(mixed_port),
        )
        .await
        .map_err(|e| e.to_string())?,
        crate::domain::SubscriptionSource::File { path } => {
            import_file_blocking(
                Some(existing.name.clone()),
                PathBuf::from(path),
                Some(id.clone()),
            )
            .await?
        }
        crate::domain::SubscriptionSource::Text { content } => {
            import_text_blocking(
                Some(existing.name.clone()),
                content.clone(),
                Some(id.clone()),
            )
            .await?
        }
        crate::domain::SubscriptionSource::Node { uri } => {
            let draft = if uri.is_none() {
                state
                    .with_store(|store| {
                        Ok(store
                            .nodes
                            .iter()
                            .find(|n| n.subscription_id == id)
                            .map(|n| node_to_draft(&n.node)))
                    })
                    .map_err(|e| e.to_string())?
            } else {
                None
            };
            import_node_blocking(
                Some(existing.name.clone()),
                uri.clone(),
                draft,
                Some(id.clone()),
            )
            .await?
        }
        crate::domain::SubscriptionSource::Singbox { content } => {
            import_singbox_blocking(
                Some(existing.name.clone()),
                content.clone(),
                Some(id.clone()),
            )
            .await?
        }
    };
    let latest = state
        .with_store(|store| {
            store
                .get_subscription(&id)
                .cloned()
                .ok_or_else(|| crate::error::AppError::NotFound(id.clone()))
        })
        .map_err(|error| error.to_string())?;
    if latest.source != existing.source {
        return Err("订阅地址或文件已在更新期间改变，已丢弃旧结果".into());
    }
    outcome.subscription.name = latest.name;
    outcome.subscription.enabled = latest.enabled;
    outcome.subscription.via_proxy = via_proxy.unwrap_or(latest.via_proxy);
    outcome.subscription.id = id;
    apply_auto_update_prefs(
        &mut outcome.subscription,
        latest.auto_update,
        latest.auto_update_interval_min,
    );
    persist_import(app, state, outcome)
}

async fn import_file_blocking(
    name: Option<String>,
    path: PathBuf,
    existing_id: Option<String>,
) -> Result<crate::services::import::ImportOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if existing_id.is_some() {
            import_from_file_with_id(name, &path, existing_id)
        } else {
            import_from_file(name, &path)
        }
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("subscription file task: {error}"))?
}

async fn import_text_blocking(
    name: Option<String>,
    content: String,
    existing_id: Option<String>,
) -> Result<crate::services::import::ImportOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        import_from_text(name, content, existing_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("subscription text task: {error}"))?
}

async fn import_node_blocking(
    name: Option<String>,
    uri: Option<String>,
    node: Option<ManualNodeDraft>,
    existing_id: Option<String>,
) -> Result<crate::services::import::ImportOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        import_from_node(name, uri, node, existing_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("subscription node task: {error}"))?
}

async fn import_singbox_blocking(
    name: Option<String>,
    content: String,
    existing_id: Option<String>,
) -> Result<crate::services::import::ImportOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        import_from_singbox(name, content, existing_id).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("subscription singbox task: {error}"))?
}

async fn load_inline_body(content: Option<String>, path: Option<String>) -> Result<String, String> {
    if let Some(content) = content.filter(|s| !s.trim().is_empty()) {
        return Ok(content);
    }
    let path = path
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "content or path is required".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(path);
        if !path.exists() {
            return Err(format!("file not found: {}", path.display()));
        }
        std::fs::read_to_string(&path).map_err(|e| e.to_string())
    })
    .await
    .map_err(|error| format!("read file task: {error}"))?
}

#[tauri::command(async)]
pub fn remove_subscription(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let before = enabled_flags(&state);
    state
        .with_store_mut(|store| store.remove_subscription(&id))
        .map_err(|e| e.to_string())?;
    crate::config::remove_custom_config(&state.app_data_dir, &id);
    queue_rebuild_if_enabled_set_changed(&app, &state, &before, "remove");
    Ok(())
}

#[tauri::command(async)]
pub fn list_subscription_nodes(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<ProxyNode>, String> {
    state
        .with_store(|store| {
            Ok(store
                .nodes
                .iter()
                .filter(|n| n.subscription_id == id)
                .map(|n| n.node.clone())
                .collect())
        })
        .map_err(|e| e.to_string())
}

fn persist_import(
    app: &tauri::AppHandle,
    state: &AppState,
    outcome: crate::services::import::ImportOutcome,
) -> Result<ImportResult, String> {
    persist_import_replacing(app, state, outcome, None)
}

fn persist_import_replacing(
    app: &tauri::AppHandle,
    state: &AppState,
    outcome: crate::services::import::ImportOutcome,
    remove_id: Option<&str>,
) -> Result<ImportResult, String> {
    if let crate::domain::SubscriptionSource::Singbox { content } = &outcome.subscription.source {
        crate::config::write_custom_config(&state.app_data_dir, &outcome.subscription.id, content)
            .map_err(|e| e.to_string())?;
    }
    let node_count = outcome.subscription.node_count;
    let skipped_count = outcome.subscription.skipped_count;
    let sub_id = outcome.subscription.id.clone();
    let (view, node_set_changed) = state
        .with_store_mut(|store| {
            let mut outcome = outcome;
            let node_ids_before = store.enabled_node_ids_sorted();
            if let Some(remove_id) = remove_id.filter(|remove_id| *remove_id != sub_id) {
                store
                    .subscriptions
                    .retain(|subscription| subscription.id != remove_id);
                store.nodes.retain(|node| node.subscription_id != remove_id);
            }
            let is_new = !store
                .subscriptions
                .iter()
                .any(|s| s.id == outcome.subscription.id);
            if is_new {
                store.prepare_new_subscription_enabled(&mut outcome.subscription);
            }
            store.upsert_subscription(outcome.subscription, outcome.nodes)?;
            store.ensure_subscription_enable_policy();
            store.ensure_current_node_valid();
            let node_ids_after = store.enabled_node_ids_sorted();
            let view = store
                .get_subscription(&sub_id)
                .map(|s| s.to_view())
                .ok_or_else(|| crate::error::AppError::NotFound(sub_id.clone()))?;
            Ok((view, node_ids_before != node_ids_after))
        })
        .map_err(|e| e.to_string())?;
    if node_set_changed {
        // Node ids are content hashes, so a refreshed subscription may rename
        // or rotate nodes. The running core still holds outbounds built from
        // the old ids: without a rebuild, traffic rows lose their display
        // names (raw node-… tags) and stale outbounds can dial servers the
        // provider has already retired. Same debounced queue rule edits use —
        // several subscriptions updating together produce one rebuild.
        crate::app_log::info(
            "subscription",
            format!("{sub_id}: enabled node set changed; queued core rebuild"),
        );
        crate::rule_apply::request_restart(app.clone(), Vec::new());
    }
    Ok(ImportResult {
        subscription: view,
        node_count,
        skipped_count,
    })
}

/// Homepage ··· → 指定配置. `source` is `generated` or `singbox:<id>`.
/// Restarts the core when it is already running so the new file takes effect.
#[tauri::command]
pub async fn set_runtime_source(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    source: String,
) -> Result<crate::domain::AppSettings, String> {
    use crate::domain::RuntimeSource;
    use tauri::Manager;

    let parsed = RuntimeSource::parse(&source);
    state
        .with_store_mut(|store| {
            store.set_runtime_source(parsed)?;
            Ok(store.settings.clone())
        })
        .map_err(|e| e.to_string())?;

    if state.is_core_running() {
        let resource_dir = app.path().resource_dir().ok();
        let worker = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let state = worker
                .try_state::<AppState>()
                .ok_or_else(|| "app state unavailable".to_string())?;
            state
                .restart_proxy(resource_dir.as_deref())
                .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| format!("switch config task: {e}"))??;
    }

    state
        .with_store(|store| Ok(store.settings.clone()))
        .map_err(|e| e.to_string())
}

/// Queue a debounced core rebuild when a mutation changed the enabled
/// subscription set and the core is running. Without this, switching
/// subscriptions keeps the OLD node set live in the kernel while the UI
/// shows the new one — and through-kernel latency probes then fail
/// instantly for every node missing from the stale config.
fn queue_rebuild_if_enabled_set_changed(
    app: &tauri::AppHandle,
    state: &AppState,
    before: &[bool],
    reason: &str,
) {
    let changed = state
        .with_store(|store| {
            Ok(store
                .subscriptions
                .iter()
                .map(|s| s.enabled)
                .zip(before.iter().copied())
                .any(|(now, was)| now != was))
        })
        .unwrap_or(true);
    if changed && state.is_core_running() {
        crate::app_log::info(
            "subscription",
            format!("{reason}: enabled node set changed; queued core rebuild"),
        );
        crate::rule_apply::request_restart(app.clone(), Vec::new());
    }
}

fn enabled_flags(state: &AppState) -> Vec<bool> {
    state
        .with_store(|store| Ok(store.subscriptions.iter().map(|s| s.enabled).collect()))
        .unwrap_or_default()
}

/// Click a config card: exclusive enable (default) or Mix toggle.
#[tauri::command(async)]
pub fn activate_subscription(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<SubscriptionView>, String> {
    let before = enabled_flags(&state);
    let views = state
        .with_store_mut(|store| {
            store.activate_subscription(&id)?;
            Ok(store.subscriptions.iter().map(|s| s.to_view()).collect())
        })
        .map_err(|e| e.to_string())?;
    queue_rebuild_if_enabled_set_changed(&app, &state, &before, &id);
    Ok(views)
}

/// Toggle Mix mode (multi-subscription enable). Turning off keeps first enabled only.
#[tauri::command(async)]
pub fn set_mix_mode(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    mix: bool,
) -> Result<crate::domain::AppSettings, String> {
    let before = enabled_flags(&state);
    let settings = state
        .with_store_mut(|store| {
            store.set_mix_mode(mix)?;
            Ok(store.settings.clone())
        })
        .map_err(|e| e.to_string())?;
    queue_rebuild_if_enabled_set_changed(&app, &state, &before, "mix-mode");
    Ok(settings)
}

#[cfg(test)]
mod refresh_flight_tests {
    use super::*;

    fn unique_id(name: &str) -> String {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(
            "subscription-flight-{name}-{}-{}",
            std::process::id(),
            nonce
        )
    }

    #[tokio::test]
    async fn duplicate_refreshes_share_the_leader_result() {
        let id = unique_id("shared");
        let leader = match begin_refresh_flight(&id).unwrap() {
            RefreshFlight::Leader(leader) => leader,
            RefreshFlight::Follower(_) => panic!("first refresh must lead"),
        };
        let follower = match begin_refresh_flight(&id).unwrap() {
            RefreshFlight::Follower(receiver) => receiver,
            RefreshFlight::Leader(_) => panic!("duplicate refresh must follow"),
        };

        let result = leader.finish(Err("shared result".into()));
        assert_eq!(result.unwrap_err(), "shared result");
        assert_eq!(
            wait_for_refresh(follower).await.unwrap_err(),
            "shared result"
        );
        assert!(matches!(
            begin_refresh_flight(&id).unwrap(),
            RefreshFlight::Leader(_)
        ));
    }

    #[tokio::test]
    async fn cancelled_leader_releases_waiters_and_registry() {
        let id = unique_id("cancelled");
        let leader = match begin_refresh_flight(&id).unwrap() {
            RefreshFlight::Leader(leader) => leader,
            RefreshFlight::Follower(_) => panic!("first refresh must lead"),
        };
        let follower = match begin_refresh_flight(&id).unwrap() {
            RefreshFlight::Follower(receiver) => receiver,
            RefreshFlight::Leader(_) => panic!("duplicate refresh must follow"),
        };

        drop(leader);
        assert!(wait_for_refresh(follower)
            .await
            .unwrap_err()
            .contains("取消"));
        assert!(matches!(
            begin_refresh_flight(&id).unwrap(),
            RefreshFlight::Leader(_)
        ));
    }
}
