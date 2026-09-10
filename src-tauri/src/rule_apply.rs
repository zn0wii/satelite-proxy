//! Debounced, globally serialized apply-and-restart for rule-set changes.
//!
//! Rule-set toggles and downloaded remote-rule updates share one worker.
//! Changes are collected until the queue has been quiet for 500ms, then the
//! final store state is applied with one core restart. Changes arriving during
//! that restart form at most one next batch instead of creating overlapping
//! restart workers.

use crate::state::AppState;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const EVENT: &str = "rule-set-apply-status";
const CONFIG_EVENT: &str = "config-apply-status";
const DEBOUNCE: Duration = Duration::from_millis(500);
const CORE_BUSY_RETRY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(crate) struct RuleApplyQueue {
    pending: HashMap<String, bool>,
    cleanup_after_apply: Vec<PathBuf>,
    restart_requested: bool,
    /// Watchdog-set: the queued restart may revive a core that died
    /// unexpectedly, not just reload a running one. Sticky until consumed.
    force_restart: bool,
    generic_change: bool,
    revision: u64,
    worker_running: bool,
}

#[derive(Debug)]
struct ApplyBatch {
    toggles: HashMap<String, bool>,
    cleanup_after_apply: Vec<PathBuf>,
    generic_change: bool,
    force_restart: bool,
}

impl RuleApplyQueue {
    fn is_running(&self) -> bool {
        self.worker_running
    }
    /// Returns true only for the request that must start the singleton worker.
    fn enqueue_toggle(&mut self, id: String, enabled: bool) -> bool {
        self.pending.insert(id, enabled);
        self.restart_requested = true;
        self.revision = self.revision.wrapping_add(1);
        if self.worker_running {
            false
        } else {
            self.worker_running = true;
            true
        }
    }

    /// Queue a restart for a non-toggle rule change, such as a downloaded
    /// remote rule file. Returns true only when a worker must be started.
    /// `force` marks the batch as allowed to revive a dead core (watchdog).
    fn enqueue_restart(&mut self, cleanup_after_apply: Vec<PathBuf>, force: bool) -> bool {
        self.cleanup_after_apply.extend(cleanup_after_apply);
        self.restart_requested = true;
        self.force_restart |= force;
        self.generic_change = true;
        self.revision = self.revision.wrapping_add(1);
        if self.worker_running {
            false
        } else {
            self.worker_running = true;
            true
        }
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn take_if_unchanged(&mut self, observed_revision: u64) -> Option<ApplyBatch> {
        if self.revision != observed_revision || !self.restart_requested {
            return None;
        }
        self.restart_requested = false;
        Some(ApplyBatch {
            toggles: std::mem::take(&mut self.pending),
            cleanup_after_apply: std::mem::take(&mut self.cleanup_after_apply),
            generic_change: std::mem::take(&mut self.generic_change),
            force_restart: std::mem::take(&mut self.force_restart),
        })
    }

    /// Requeue an un-applied batch without overwriting newer values for an id.
    fn requeue_older_batch(&mut self, batch: ApplyBatch) {
        for (id, enabled) in batch.toggles {
            self.pending.entry(id).or_insert(enabled);
        }
        self.cleanup_after_apply.extend(batch.cleanup_after_apply);
        self.generic_change |= batch.generic_change;
        self.force_restart |= batch.force_restart;
        self.restart_requested = true;
        self.revision = self.revision.wrapping_add(1);
    }

    fn finish_if_empty(&mut self) -> bool {
        if !self.restart_requested {
            self.worker_running = false;
            true
        } else {
            false
        }
    }

    fn terminal_subset(&self, batch: &HashMap<String, bool>) -> HashMap<String, bool> {
        batch
            .iter()
            .filter(|(id, _)| !self.pending.contains_key(*id))
            .map(|(id, enabled)| (id.clone(), *enabled))
            .collect()
    }
}

#[derive(Clone, Serialize)]
struct ApplyStatusEvent {
    id: String,
    enabled: bool,
    status: &'static str,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct ConfigApplyStatusEvent {
    status: &'static str,
    error: Option<String>,
}

fn emit_config(app: &AppHandle, status: &'static str, error: Option<String>) {
    let _ = app.emit(CONFIG_EVENT, ConfigApplyStatusEvent { status, error });
}

fn emit(app: &AppHandle, id: &str, enabled: bool, status: &'static str, error: Option<String>) {
    let _ = app.emit(
        EVENT,
        ApplyStatusEvent {
            id: id.to_string(),
            enabled,
            status,
            error,
        },
    );
}

fn emit_batch(
    app: &AppHandle,
    batch: &HashMap<String, bool>,
    status: &'static str,
    error: Option<String>,
) {
    for (id, enabled) in batch {
        emit(app, id, *enabled, status, error.clone());
    }
}

/// Persisting has already completed when this is called. Queue the final value
/// and return immediately; all ids share one background restart worker.
pub fn request_apply(app: AppHandle, id: String, enabled: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let start_worker = state.lock_rule_apply_queue().enqueue_toggle(id, enabled);
    if start_worker {
        spawn_worker(app);
    }
}

/// Acknowledge a persisted toggle that cannot change the generated config
/// (e.g. toggling an effectively-empty rule set): close the UI busy state
/// with a terminal `ready` event without queueing any core restart.
pub fn emit_ready_without_restart(app: &AppHandle, id: &str, enabled: bool) {
    emit(app, id, enabled, "ready", None);
}

/// Queue a non-toggle rule change for the same globally serialized restart.
/// This is used after one or more remote rule files have been downloaded.
pub fn request_restart(app: AppHandle, cleanup_after_apply: Vec<PathBuf>) {
    request_restart_inner(app, cleanup_after_apply, false);
}

/// Watchdog variant: same serialized queue, but the batch is allowed to
/// revive a core that died unexpectedly (regular restarts skip dead cores —
/// rule edits on a stopped core must not start anything).
pub fn request_forced_restart(app: AppHandle, cleanup_after_apply: Vec<PathBuf>) {
    request_restart_inner(app, cleanup_after_apply, true);
}

fn request_restart_inner(app: AppHandle, cleanup_after_apply: Vec<PathBuf>, force: bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let start_worker = state
        .lock_rule_apply_queue()
        .enqueue_restart(cleanup_after_apply, force);
    if start_worker {
        spawn_worker(app);
    }
}

pub fn is_pending(state: &AppState) -> bool {
    state.lock_rule_apply_queue().is_running()
}

fn spawn_worker(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || loop {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };

        // True debounce: require a complete quiet window, not merely 500ms
        // since the first click.
        let batch = loop {
            let revision = state.lock_rule_apply_queue().revision();
            std::thread::sleep(DEBOUNCE);
            let mut queue = state.lock_rule_apply_queue();
            if let Some(batch) = queue.take_if_unchanged(revision) {
                break batch;
            }
        };

        // Rule toggles and generic config edits both rebuild the same running
        // core. Drive the global navbar busy animation only when a restart is
        // actually expected; a stopped core only needs the persisted config.
        // A forced (watchdog) batch may start a dead core — announce that.
        let announce_core_restart = batch.force_restart || state.is_core_running();
        emit_batch(&app, &batch.toggles, "restarting", None);
        if announce_core_restart {
            emit_config(&app, "restarting", None);
        }
        let resource_dir = app.path().resource_dir().ok();
        let result = if batch.force_restart {
            state.restart_after_unexpected_exit(resource_dir.as_deref())
        } else {
            state.restart_if_running(resource_dir.as_deref())
        };

        match result {
            Ok(_) => {
                if !batch.cleanup_after_apply.is_empty() {
                    crate::app_log::info("rule_apply", "remote rule updates applied");
                } else if batch.generic_change {
                    crate::app_log::info("rule_apply", "queued config changes applied");
                }
                for path in &batch.cleanup_after_apply {
                    if let Err(error) = std::fs::remove_file(path) {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            crate::app_log::warn(
                                "remote_rules",
                                format!("failed to remove old cache {}: {error}", path.display()),
                            );
                        }
                    }
                }
                let terminal = state
                    .lock_rule_apply_queue()
                    .terminal_subset(&batch.toggles);
                emit_batch(&app, &terminal, "ready", None);
                if announce_core_restart {
                    emit_config(&app, "ready", None);
                }
            }
            Err(error) if error.to_string().contains("内核正在切换") => {
                // Close the current busy token before retrying. The next
                // attempt emits a fresh restarting event after the owner exits.
                if announce_core_restart {
                    emit_config(&app, "ready", None);
                }
                // Another legitimate operation (for example TUN switching) owns
                // the transition. Keep the latest values queued and retry later.
                state.lock_rule_apply_queue().requeue_older_batch(batch);
                std::thread::sleep(CORE_BUSY_RETRY);
                continue;
            }
            Err(error) => {
                crate::app_log::error(
                    "rule_apply",
                    format!("queued rule changes failed to restart core: {error}"),
                );
                let terminal = state
                    .lock_rule_apply_queue()
                    .terminal_subset(&batch.toggles);
                emit_batch(
                    &app,
                    &terminal,
                    "error",
                    Some(format!("已保存，但重启内核失败: {error}")),
                );
                if announce_core_restart {
                    emit_config(
                        &app,
                        "error",
                        Some(format!("配置已保存，但应用到内核失败: {error}")),
                    );
                }
            }
        }

        if state.lock_rule_apply_queue().finish_if_empty() {
            break;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_rule_sets_share_one_worker_and_one_batch() {
        let mut queue = RuleApplyQueue::default();
        assert!(queue.enqueue_toggle("a".into(), true));
        assert!(!queue.enqueue_toggle("b".into(), false));
        assert!(!queue.enqueue_toggle("c".into(), true));
        let revision = queue.revision();
        let batch = queue.take_if_unchanged(revision).unwrap();
        assert_eq!(batch.toggles.len(), 3);
        assert!(queue.worker_running);
        assert!(queue.finish_if_empty());
        assert!(!queue.worker_running);
    }

    #[test]
    fn latest_value_wins_before_restart() {
        let mut queue = RuleApplyQueue::default();
        queue.enqueue_toggle("same".into(), true);
        let stale_revision = queue.revision();
        queue.enqueue_toggle("same".into(), false);
        assert!(queue.take_if_unchanged(stale_revision).is_none());
        let batch = queue.take_if_unchanged(queue.revision()).unwrap();
        assert_eq!(batch.toggles.get("same"), Some(&false));
    }

    #[test]
    fn change_during_restart_forms_only_next_batch() {
        let mut queue = RuleApplyQueue::default();
        queue.enqueue_toggle("a".into(), true);
        let first = queue.take_if_unchanged(queue.revision()).unwrap();
        assert_eq!(first.toggles.len(), 1);
        assert!(!queue.enqueue_toggle("b".into(), true));
        assert!(!queue.finish_if_empty());
        let second = queue.take_if_unchanged(queue.revision()).unwrap();
        assert_eq!(
            second.toggles.keys().collect::<Vec<_>>(),
            vec![&"b".to_string()]
        );
    }

    #[test]
    fn busy_retry_preserves_newer_value() {
        let mut queue = RuleApplyQueue::default();
        queue.enqueue_toggle("a".into(), true);
        let old = queue.take_if_unchanged(queue.revision()).unwrap();
        queue.enqueue_toggle("a".into(), false);
        queue.requeue_older_batch(old);
        let next = queue.take_if_unchanged(queue.revision()).unwrap();
        assert_eq!(next.toggles.get("a"), Some(&false));
    }

    #[test]
    fn stale_terminal_event_is_suppressed_for_changed_id() {
        let mut queue = RuleApplyQueue::default();
        queue.enqueue_toggle("a".into(), true);
        queue.enqueue_toggle("b".into(), true);
        let first = queue.take_if_unchanged(queue.revision()).unwrap();
        queue.enqueue_toggle("a".into(), false);
        let terminal = queue.terminal_subset(&first.toggles);
        assert!(!terminal.contains_key("a"));
        assert_eq!(terminal.get("b"), Some(&true));
    }

    #[test]
    fn remote_updates_and_toggles_share_one_batch() {
        let mut queue = RuleApplyQueue::default();
        assert!(queue.enqueue_restart(Vec::new(), false));
        assert!(!queue.enqueue_restart(Vec::new(), false));
        assert!(!queue.enqueue_toggle("a".into(), true));

        let batch = queue.take_if_unchanged(queue.revision()).unwrap();
        assert_eq!(batch.toggles.get("a"), Some(&true));
        assert!(queue.finish_if_empty());
    }

    #[test]
    fn config_sources_share_one_worker_and_debounced_batch() {
        let mut queue = RuleApplyQueue::default();
        // Rule edit, DNS save and settings save all request the same generic
        // restart while a toggle and remote cleanup arrive in the same window.
        assert!(queue.enqueue_restart(Vec::new(), false));
        assert!(!queue.enqueue_restart(Vec::new(), false));
        assert!(!queue.enqueue_restart(Vec::new(), false));
        assert!(!queue.enqueue_toggle("rule-set".into(), true));
        let old_cache = PathBuf::from("old-remote.json");
        assert!(!queue.enqueue_restart(vec![old_cache.clone()], false));

        let batch = queue.take_if_unchanged(queue.revision()).unwrap();
        assert_eq!(batch.toggles.get("rule-set"), Some(&true));
        assert_eq!(batch.cleanup_after_apply, vec![old_cache]);
        assert!(batch.generic_change);
        assert!(queue.finish_if_empty());
    }

    #[test]
    fn remote_update_during_restart_forms_one_next_batch() {
        let mut queue = RuleApplyQueue::default();
        queue.enqueue_restart(Vec::new(), false);
        let first = queue.take_if_unchanged(queue.revision()).unwrap();
        assert!(first.toggles.is_empty());

        assert!(!queue.enqueue_restart(Vec::new(), false));
        assert!(!queue.enqueue_restart(Vec::new(), false));
        assert!(!queue.finish_if_empty());
        let second = queue.take_if_unchanged(queue.revision()).unwrap();
        assert!(second.toggles.is_empty());
        assert!(queue.finish_if_empty());
    }

    #[test]
    fn busy_retry_keeps_remote_cache_cleanup() {
        let mut queue = RuleApplyQueue::default();
        let old = PathBuf::from("old-rule.json");
        queue.enqueue_restart(vec![old.clone()], false);
        let batch = queue.take_if_unchanged(queue.revision()).unwrap();
        queue.requeue_older_batch(batch);

        let retried = queue.take_if_unchanged(queue.revision()).unwrap();
        assert_eq!(retried.cleanup_after_apply, vec![old]);
    }

    #[test]
    fn forced_restart_flag_flows_to_the_batch_and_survives_requeue() {
        let mut queue = RuleApplyQueue::default();
        // A regular queued restart stays non-forced (must never start a
        // stopped core just to apply config).
        assert!(queue.enqueue_restart(Vec::new(), false));
        let plain = queue.take_if_unchanged(queue.revision()).unwrap();
        assert!(!plain.force_restart);
        assert!(queue.finish_if_empty());

        // A watchdog batch carries the force flag...
        assert!(queue.enqueue_restart(Vec::new(), true));
        let batch = queue.take_if_unchanged(queue.revision()).unwrap();
        assert!(batch.force_restart);

        // ...and keeps it when the worker hits a busy core and requeues.
        queue.requeue_older_batch(batch);
        let retried = queue.take_if_unchanged(queue.revision()).unwrap();
        assert!(retried.force_restart);
    }
}
