import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  activateSubscription,
  addSubscriptionFile,
  addSubscriptionNode,
  addSubscriptionSingbox,
  addSubscriptionText,
  addSubscriptionUrl,
  getSettings,
  getSubscription,
  listSubscriptions,
  listSubscriptionUrls,
  peekSettings,
  refreshSubscription,
  removeSubscription,
  setMixMode,
  updateSubscription,
} from "../api";
import {
  AddConfigModal,
  type ConfigFormValues,
} from "../components/AddConfigModal";
import { EditLocalNodesModal } from "../components/EditLocalNodesModal";
import { GlassButton } from "../components/GlassButton";
import { GlassSwitch } from "../components/GlassSwitch";
import { useImportIntent } from "../ImportIntentContext";
import { getVersion } from "@tauri-apps/api/app";
import { useI18n } from "../i18n";
import { ErrorModal } from "../components/ErrorModal";
import type {
  ImportResult,
  SubscriptionTraffic,
  SubscriptionUrlEntry,
  SubscriptionView,
} from "../types";


const REFRESH_ALL_CONCURRENCY = 4;

/** One subscription's skipped-node report entry (import / manual refresh). */
interface SkippedReport {
  name: string;
  source: string;
  format: string | null;
  skipped: { name?: string | null; reason: string }[];
}

function skippedEntry(result: ImportResult): SkippedReport {
  return {
    name: result.subscription.name,
    source: result.subscription.source_display,
    format: result.subscription.format ?? null,
    skipped: result.skipped ?? [],
  };
}

async function settleWithConcurrency<T, R>(
  values: readonly T[],
  concurrency: number,
  task: (value: T) => Promise<R>,
): Promise<PromiseSettledResult<R>[]> {
  const results = new Array<PromiseSettledResult<R>>(values.length);
  let nextIndex = 0;
  async function worker() {
    while (nextIndex < values.length) {
      const index = nextIndex++;
      try {
        results[index] = {
          status: "fulfilled",
          value: await task(values[index] as T),
        };
      } catch (reason) {
        results[index] = { status: "rejected", reason };
      }
    }
  }
  const workerCount = Math.min(Math.max(1, concurrency), values.length);
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  return results;
}

function formatTime(ts: number) {
  if (!ts) return "—";
  try {
    return new Date(ts * 1000).toLocaleString();
  } catch {
    return String(ts);
  }
}

/** Relative time for "Last Update" (e.g. 5 minutes ago). */
function formatRelative(
  ts: number,
  t: (key: import("../i18n").MessageKey, vars?: Record<string, string | number>) => string,
) {
  if (!ts) return "—";
  const sec = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  if (sec < 60) return t("common.justNow");
  if (sec < 3600) return t("common.minutesAgo", { n: Math.floor(sec / 60) });
  if (sec < 86400) return t("common.hoursAgo", { n: Math.floor(sec / 3600) });
  if (sec < 86400 * 30) return t("common.daysAgo", { n: Math.floor(sec / 86400) });
  return formatTime(ts);
}

function formatExpireDate(ts: number) {
  try {
    return new Date(ts * 1000).toLocaleDateString(undefined, {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    });
  } catch {
    return String(ts);
  }
}

function fmtBytes(n: number) {
  if (!Number.isFinite(n) || n < 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  const digits = i === 0 ? 0 : v >= 100 ? 0 : v >= 10 ? 1 : 2;
  return `${v.toFixed(digits)} ${units[i]}`;
}

type TrafficView = {
  used: number;
  total: number | null;
  remaining: number | null;
  ratio: number | null;
  expire: number | null;
  expireText: string | null;
};

/** used = upload + download; remaining = total - used (or explicit remaining). */
function trafficStats(t: SubscriptionTraffic | null | undefined): TrafficView | null {
  if (!t) return null;
  const upload = t.upload ?? 0;
  const download = t.download ?? 0;
  const usedFromParts = upload + download;
  const total = t.total && t.total > 0 ? t.total : null;
  const remainingExplicit =
    t.quota_remaining != null && t.quota_remaining >= 0
      ? t.quota_remaining
      : null;

  let used = usedFromParts;
  let remaining: number | null = remainingExplicit;

  if (total != null) {
    if (usedFromParts > 0) {
      remaining = Math.max(0, total - usedFromParts);
    } else if (remaining != null) {
      used = Math.max(0, total - remaining);
    } else {
      remaining = total;
      used = 0;
    }
  }

  let ratio: number | null = null;
  if (total != null && total > 0) {
    ratio = Math.min(1, Math.max(0, used / total));
  }

  const expire = t.expire && t.expire > 0 ? t.expire : null;
  const expireText = t.expire_text?.trim() || null;

  if (
    total == null &&
    remaining == null &&
    used === 0 &&
    expire == null &&
    !expireText
  ) {
    return null;
  }
  return { used, total, remaining, ratio, expire, expireText };
}

/** Compact FlClash-style traffic: thin bar + "used / total · expire". */
function TrafficBlock({ traffic }: { traffic?: SubscriptionTraffic | null }) {
  const { t } = useI18n();
  const tr = trafficStats(traffic);
  if (!tr) return null;

  const expireLabel = tr.expireText
    ? tr.expireText
    : tr.expire != null
      ? formatExpireDate(tr.expire)
      : null;

  // Full userinfo: progress = used/total (hide when total==0, same as FlClash)
  if (tr.total != null && tr.total > 0 && tr.ratio != null) {
    const barWidth = Math.min(100, Math.max(0, tr.ratio * 100));
    const level =
      tr.ratio >= 0.9 ? "critical" : tr.ratio >= 0.7 ? "warn" : "ok";
    const pct = Math.round(tr.ratio * 100);
    return (
      <div className="traffic-block">
        <div
          className="traffic-bar"
          role="progressbar"
          aria-valuenow={pct}
          aria-valuemin={0}
          aria-valuemax={100}
        >
          <div
            className={`traffic-bar-fill ${level}`}
            style={{ width: `${barWidth}%` }}
          />
        </div>
        <div className="traffic-line">
          <span>
            {fmtBytes(tr.used)} / {fmtBytes(tr.total)}
          </span>
          {expireLabel && <span className="dot-sep">·</span>}
          {expireLabel && (
            <span className="traffic-expire">
              {expireLabel}
            </span>
          )}
        </div>
      </div>
    );
  }

  // Remaining-only fallback
  if (tr.remaining != null || expireLabel) {
    return (
      <div className="traffic-block">
        <div className="traffic-line">
          {tr.remaining != null && (
            <span>{t("common.remaining", { n: fmtBytes(tr.remaining) })}</span>
          )}
          {tr.remaining != null && expireLabel && (
            <span className="dot-sep">·</span>
          )}
          {expireLabel && (
            <span className="traffic-expire">
              {expireLabel}
            </span>
          )}
        </div>
      </div>
    );
  }

  return null;
}

function ConfigGroup({
  title,
  empty,
  items,
  renderCard,
}: {
  title: string;
  empty: string;
  items: SubscriptionView[];
  renderCard: (item: SubscriptionView) => ReactNode;
}) {
  return (
    <section className="config-group">
      <h2 className="config-group-title">{title}</h2>
      {items.length === 0 ? (
        <p className="config-group-empty muted">{empty}</p>
      ) : (
        <div className="sub-grid">{items.map((item) => renderCard(item))}</div>
      )}
    </section>
  );
}

export function ConfigPage() {
  const { t } = useI18n();
  const { prefill, token, consume, dismiss } = useImportIntent();
  const [items, setItems] = useState<SubscriptionView[]>([]);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState<string | null>(null);
  const [skippedReport, setSkippedReport] = useState<SkippedReport[] | null>(null);
  const [skippedCopied, setSkippedCopied] = useState(false);
  // Seed from the cross-mount settings snapshot (see api.ts) so the header
  // mix switch paints its persisted position on re-mount without sliding.
  const [mixMode, setMixModeState] = useState(
    () => peekSettings()?.mix_mode ?? false,
  );
  const [runtimeSource, setRuntimeSource] = useState("generated");

  const [modalOpen, setModalOpen] = useState(false);
  const [editId, setEditId] = useState<string | null>(null);
  const [editInitial, setEditInitial] = useState<ConfigFormValues | null>(null);
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const [existingUrls, setExistingUrls] = useState<SubscriptionUrlEntry[]>([]);

  const [actionId, setActionId] = useState<string | null>(null);
  const [refreshingAll, setRefreshingAll] = useState(false);
  const [menuId, setMenuId] = useState<string | null>(null);
  const [renameProfile, setRenameProfile] = useState<SubscriptionView | null>(
    null,
  );

  const busy = refreshingAll || actionId != null;

  const reload = useCallback(async () => {
    setListError(null);
    try {
      const [list, settings] = await Promise.all([
        listSubscriptions(),
        getSettings(),
      ]);
      setItems(list);
      setMixModeState(!!settings.mix_mode);
      setRuntimeSource(settings.runtime_source || "generated");
    } catch (e) {
      setListError(typeof e === "string" ? e : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    if (!modalOpen) return;
    let cancelled = false;
    setExistingUrls([]);
    void listSubscriptionUrls()
      .then((entries) => {
        if (cancelled) return;
        setExistingUrls(entries);
      })
      .catch(() => {
        if (!cancelled) setExistingUrls([]);
      });
    return () => {
      cancelled = true;
    };
  }, [modalOpen, items.length]);

  // One-click subscribe deep link → open add modal with URL/name filled.
  useEffect(() => {
    if (!token || !prefill) return;
    setEditId(null);
    setImportError(null);
    setEditInitial({
      name: prefill.name ?? "",
      kind: "url",
      url: prefill.url,
      autoUpdate: true,
      autoUpdateIntervalMin: 1440,
    });
    setModalOpen(true);
    consume();
  }, [token, prefill, consume]);

  useEffect(() => {
    if (!menuId) return;
    function onDocPointerDown(e: PointerEvent) {
      const t = e.target as HTMLElement | null;
      if (t?.closest?.("[data-sub-menu]")) return;
      setMenuId(null);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setMenuId(null);
    }
    document.addEventListener("pointerdown", onDocPointerDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDocPointerDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuId]);

  async function onActivate(id: string) {
    if (busy) return;
    setListError(null);
    try {
      const list = await activateSubscription(id);
      setItems(list);
    } catch (e) {
      setListError(typeof e === "string" ? e : String(e));
    }
  }

  async function onToggleMix() {
    if (busy) return;
    setListError(null);
    try {
      const next = !mixMode;
      const settings = await setMixMode(next);
      setMixModeState(!!settings.mix_mode);
      // policy may collapse multi-enabled → reload list
      const list = await listSubscriptions();
      setItems(list);
    } catch (e) {
      setListError(typeof e === "string" ? e : String(e));
    }
  }

  function openAdd() {
    setEditId(null);
    setEditInitial(null);
    setImportError(null);
    setModalOpen(true);
  }

  async function openEdit(id: string) {
    setImportError(null);
    setActionId(id);
    try {
      const d = await getSubscription(id);
      setEditId(id);
      const kind =
        d.source_kind === "file" ||
        d.source_kind === "text" ||
        d.source_kind === "node" ||
        d.source_kind === "singbox"
          ? d.source_kind
          : "url";
      const isUriOnlyNode =
        kind === "node" && !!(d.uri && !(d.node && d.node.server));
      setEditInitial({
        name: d.name,
        kind: isUriOnlyNode ? "text" : kind,
        url: d.url ?? "",
        path: d.path ?? "",
        content: d.content ?? (isUriOnlyNode ? (d.uri ?? "") : ""),
        uri: d.uri ?? "",
        node: isUriOnlyNode ? undefined : (d.node ?? undefined),
        viaProxy: d.via_proxy,
        autoUpdate: !!d.auto_update,
        autoUpdateIntervalMin: d.auto_update_interval_min ?? 1440,
        userAgent: d.user_agent ?? "",
      });
      setModalOpen(true);
    } catch (e) {
      setListError(typeof e === "string" ? e : String(e));
    } finally {
      setActionId(null);
    }
  }

  async function handleSubmit(payload: ConfigFormValues) {
    setImporting(true);
    setImportError(null);
    let importResult: ImportResult | null = null;
    try {
      const name = payload.name || null;
      const autoUpdate = !!payload.autoUpdate;
      const autoUpdateIntervalMin = payload.autoUpdateIntervalMin ?? 1440;
      if (editId) {
        importResult = await updateSubscription({
          id: editId,
          name,
          kind: payload.kind,
          url: payload.url ?? null,
          path: payload.path ?? null,
          content: payload.content ?? null,
          uri: payload.uri ?? null,
          node: payload.node ?? null,
          viaProxy: payload.viaProxy ?? false,
          autoUpdate,
          autoUpdateIntervalMin,
          userAgent: payload.userAgent ?? null,
        });
      } else if (payload.kind === "url") {
        importResult = await addSubscriptionUrl(
          name,
          payload.url ?? "",
          !!payload.viaProxy,
          autoUpdate,
          autoUpdateIntervalMin,
          payload.userAgent ?? null,
        );
      } else if (payload.kind === "file") {
        importResult = await addSubscriptionFile(
          name,
          payload.path ?? "",
          autoUpdate,
          autoUpdateIntervalMin,
        );
      } else if (payload.kind === "text") {
        importResult = await addSubscriptionText(name, payload.content ?? "");
      } else if (payload.kind === "singbox") {
        importResult = await addSubscriptionSingbox(name, payload.content ?? "", null);
      } else {
        importResult = await addSubscriptionNode(
          name,
          payload.uri ?? null,
          payload.node ?? null,
        );
      }
      setModalOpen(false);
      setEditId(null);
      setEditInitial(null);
      dismiss();
      await reload();
      maybeReportSkipped(importResult);
    } catch (e) {
      setImportError(typeof e === "string" ? e : String(e));
    } finally {
      setImporting(false);
    }
  }

  /** Import/refresh produced skipped nodes → surface the report dialog. */
  function maybeReportSkipped(result: ImportResult | null) {
    if (result?.skipped?.length) setSkippedReport([skippedEntry(result)]);
  }

  async function copySkippedReport() {
    if (!skippedReport) return;
    let version = "";
    try {
      version = await getVersion();
    } catch {
      version = "?";
    }
    const total = skippedReport.reduce((n, r) => n + r.skipped.length, 0);
    const lines = [
      `Satelite 跳过节点报告（v${version}）`,
      `共 ${total} 个节点未能导入：`,
    ];
    for (const r of skippedReport) {
      lines.push(``, `订阅：${r.name}${r.format ? `（${r.format}）` : ""}`, `来源：${r.source}`);
      r.skipped.forEach((item, i) => {
        lines.push(`  ${i + 1}. ${item.name || "(未命名)"} — ${item.reason}`);
      });
    }
    const text = lines.join("\n");
    try {
      await navigator.clipboard.writeText(text);
      setSkippedCopied(true);
      window.setTimeout(() => setSkippedCopied(false), 1500);
    } catch {
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      setSkippedCopied(true);
      window.setTimeout(() => setSkippedCopied(false), 1500);
    }
  }

  async function onRefresh(id: string) {
    setActionId(id);
    setListError(null);
    try {
      const result = await refreshSubscription(id);
      await reload();
      maybeReportSkipped(result);
    } catch (e) {
      setListError(typeof e === "string" ? e : String(e));
    } finally {
      setActionId(null);
    }
  }

  /** Refresh subscriptions in a bounded pool to avoid request and parse bursts. */
  async function onRefreshAll() {
    const remotes = items.filter((item) => item.source_kind === "url");
    if (remotes.length === 0 || refreshingAll) return;
    setRefreshingAll(true);
    setListError(null);
    try {
      const results = await settleWithConcurrency(
        remotes,
        REFRESH_ALL_CONCURRENCY,
        (item) => refreshSubscription(item.id),
      );
      const failed: string[] = [];
      const skippedReports: SkippedReport[] = [];
      results.forEach((r, i) => {
        if (r.status === "rejected") {
          const name = remotes[i]?.name ?? remotes[i]?.id ?? "?";
          const reason =
            typeof r.reason === "string"
              ? r.reason
              : r.reason != null
                ? String(r.reason)
                : "unknown";
          failed.push(`${name}: ${reason}`);
        } else if (r.value?.skipped?.length) {
          skippedReports.push(skippedEntry(r.value));
        }
      });
      await reload();
      if (skippedReports.length > 0) setSkippedReport(skippedReports);
      if (failed.length > 0) {
        setListError(failed.slice(0, 5).join("；"));
      }
    } catch (e) {
      setListError(typeof e === "string" ? e : String(e));
    } finally {
      setRefreshingAll(false);
    }
  }

  async function onRemove(id: string) {
    if (!confirm(t("config.confirmDelete"))) return;
    setActionId(id);
    setListError(null);
    try {
      await removeSubscription(id);
      await reload();
    } catch (e) {
      setListError(typeof e === "string" ? e : String(e));
    } finally {
      setActionId(null);
    }
  }

  function renderCard(item: SubscriptionView) {
    const generated = item.source_kind !== "singbox";
    // Custom mode: switching the active profile is a runtime-source change
    // (homepage picker) — subscription / local cards become read-only.
    const clickable = generated && runtimeSource === "generated";
    const customActive =
      !generated && runtimeSource === `singbox:${item.id}`;
    const generatedActive = clickable && item.enabled;
    return (
      <article
        key={item.id}
        className={`sub-card${generatedActive || customActive ? " enabled" : ""}${
          clickable ? "" : " readonly"
        }`}
        role={clickable ? "button" : "article"}
        tabIndex={clickable ? 0 : undefined}
        onClick={clickable ? () => void onActivate(item.id) : undefined}
        onKeyDown={
          clickable
            ? (e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  void onActivate(item.id);
                }
              }
            : undefined
        }
      >
        <div className="sub-card-main">
          <div className="sub-card-top">
            {generated ? (
              <span
                className="node-dot"
                aria-label={
                  item.enabled ? t("common.enabled") : t("common.disabled")
                }
              >
                {item.enabled ? "●" : "○"}
              </span>
            ) : null}
            <h3>{item.name}</h3>
            <div className="sub-card-top-right">
              <span className="sub-card-updated muted">
                {formatRelative(item.last_update, t)}
              </span>
              <div
                className="sub-menu"
                data-sub-menu
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => e.stopPropagation()}
              >
                <button
                  type="button"
                  className="sub-menu-trigger"
                  aria-label={t("common.actions")}
                  aria-haspopup="menu"
                  aria-expanded={menuId === item.id}
                  disabled={busy && menuId !== item.id}
                  onClick={() =>
                    setMenuId((id) => (id === item.id ? null : item.id))
                  }
                >
                  {actionId === item.id ||
                  (refreshingAll && menuId === item.id)
                    ? "…"
                    : "⋮"}
                </button>
                {menuId === item.id && (
                  <div className="sub-menu-pop" role="menu">
                    <button
                      type="button"
                      role="menuitem"
                      className="sub-menu-item"
                      disabled={busy}
                      onClick={() => {
                        setMenuId(null);
                        void openEdit(item.id);
                      }}
                    >
                      {t("config.menuEdit")}
                    </button>
                    {item.source_kind === "url" && (
                      <button
                        type="button"
                        role="menuitem"
                        className="sub-menu-item"
                        disabled={busy}
                        onClick={() => {
                          setMenuId(null);
                          void onRefresh(item.id);
                        }}
                      >
                        {t("config.menuUpdate")}
                      </button>
                    )}
                    {item.source_kind !== "url" &&
                      item.source_kind !== "singbox" && (
                        <button
                          type="button"
                          role="menuitem"
                          className="sub-menu-item"
                          disabled={busy}
                          onClick={() => {
                            setMenuId(null);
                            setRenameProfile(item);
                          }}
                        >
                          {t("config.menuRenameNodes")}
                        </button>
                      )}
                    <button
                      type="button"
                      role="menuitem"
                      className="sub-menu-item danger"
                      disabled={busy}
                      onClick={() => {
                        setMenuId(null);
                        void onRemove(item.id);
                      }}
                    >
                      {t("config.menuDelete")}
                    </button>
                  </div>
                )}
              </div>
            </div>
          </div>
          <div className="sub-card-meta">
            {generated ? (
              <span>{t("config.nodes", { n: item.node_count })}</span>
            ) : (
              <span>{t("config.singboxReadonly")}</span>
            )}
            {item.skipped_count > 0 && (
              <span className="warn">
                {t("config.skipped", { n: item.skipped_count })}
              </span>
            )}
          </div>
          <TrafficBlock traffic={item.traffic} />
        </div>
      </article>
    );
  }

  return (
    <div className="page config-page">
      <header className="page-header">
        <div>
          <h1>{t("config.title")}</h1>
          <p className="page-desc">{t("config.desc")}</p>
        </div>
        <div className="header-actions">
          <GlassSwitch
            checked={mixMode}
            ready={!loading}
            onChange={() => void onToggleMix()}
            label={t("config.mix")}
            title={mixMode ? t("config.mixEnabled") : t("config.mixDisabled")}
            disabled={loading || busy || runtimeSource.startsWith("singbox:")}
            capsule
            size="sm"
          />
          <GlassButton
            icon="↻"
            disabled={busy || items.every((item) => item.source_kind !== "url")}
            onClick={() => void onRefreshAll()}
          >
            {refreshingAll ? t("config.refreshing") : t("config.refreshAll")}
          </GlassButton>
          <GlassButton icon="+" disabled={busy} onClick={openAdd}>
            {t("config.add")}
          </GlassButton>
        </div>
      </header>

      {listError && (
        <ErrorModal
          message={listError}
          onClose={() => setListError(null)}
        />
      )}

      {skippedReport && (
        <div className="modal-backdrop">
          <div className="modal skipped-nodes-modal">
            <header className="modal-header">
              <h2>
                {t("config.skippedTitle", {
                  n: skippedReport.reduce((sum, r) => sum + r.skipped.length, 0),
                })}
              </h2>
              <button
                type="button"
                className="icon-btn"
                onClick={() => setSkippedReport(null)}
              >
                ×
              </button>
            </header>
            <div className="modal-body">
              <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                {t("config.skippedHint")}
              </p>
              {skippedReport.map((r) => (
                <div key={r.name + r.source} className="skipped-report-block">
                  <div className="skipped-report-sub">
                    <strong>{r.name}</strong>
                    {r.format ? <span className="muted mono">{r.format}</span> : null}
                    <span className="muted skipped-report-source">{r.source}</span>
                  </div>
                  <ul className="skipped-report-list">
                    {r.skipped.map((item, i) => (
                      <li key={i} title={`${item.reason}`}>
                        <span className="skipped-item-name">{item.name || t("config.skippedUnnamed")}</span>
                        <span className="muted skipped-item-reason">{item.reason}</span>
                      </li>
                    ))}
                  </ul>
                </div>
              ))}
            </div>
            <footer className="modal-footer">
              <GlassButton onClick={() => setSkippedReport(null)}>
                {t("common.close")}
              </GlassButton>
              <GlassButton variant="primary" onClick={() => void copySkippedReport()}>
                {skippedCopied ? t("common.copied") : t("config.skippedCopy")}
              </GlassButton>
            </footer>
          </div>
        </div>
      )}

      {loading ? (
        <div className="empty">{t("common.loading")}</div>
      ) : items.length === 0 ? (
        <div className="empty card">
          <p>{t("config.empty")}</p>
          <p className="muted">{t("config.emptyHint")}</p>
          <GlassButton variant="primary" icon="+" onClick={openAdd}>
            {t("config.add")}
          </GlassButton>
        </div>
      ) : (
        <div className="config-groups">
          <ConfigGroup
            title={t("config.groupSubscription")}
            empty={t("config.groupSubscriptionEmpty")}
            items={items.filter((item) => item.source_kind === "url")}
            renderCard={(item) => renderCard(item)}
          />
          <ConfigGroup
            title={t("config.groupLocal")}
            empty={t("config.groupLocalEmpty")}
            items={items.filter(
              (item) =>
                item.source_kind !== "url" && item.source_kind !== "singbox",
            )}
            renderCard={(item) => renderCard(item)}
          />
          <ConfigGroup
            title={t("config.groupSingbox")}
            empty={t("config.groupSingboxEmpty")}
            items={items.filter((item) => item.source_kind === "singbox")}
            renderCard={(item) => renderCard(item)}
          />
        </div>
      )}

      <AddConfigModal
        open={modalOpen}
        busy={importing}
        error={importError}
        onDismissError={() => setImportError(null)}
        isEdit={!!editId}
        initial={editInitial}
        existingUrls={existingUrls
          .filter((item) => item.id !== editId)
          .map((item) => item.url)}
        onClose={() => {
          if (importing) return;
          setModalOpen(false);
          setEditId(null);
          setEditInitial(null);
          dismiss();
        }}
        onSubmit={(p) => void handleSubmit(p)}
      />
      <EditLocalNodesModal
        open={!!renameProfile}
        profileId={renameProfile?.id ?? null}
        profileName={renameProfile?.name ?? ""}
        onClose={() => setRenameProfile(null)}
      />
    </div>
  );
}
