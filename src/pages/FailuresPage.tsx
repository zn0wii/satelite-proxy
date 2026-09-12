import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import {
  clearRequestHistory,
  createRuleSet,
  listRequestFailures,
  listRuleSets,
  saveRule,
} from "../api";
import { GlassButton } from "../components/GlassButton";
import { GlassSeg } from "../components/GlassSeg";
import { SolidSelect } from "../components/SolidSelect";
import { useVirtualRange } from "../hooks/useVirtualRange";
import { useVisibleInterval } from "../hooks/useVisibleInterval";
import { useI18n } from "../i18n";
import { ErrorModal } from "../components/ErrorModal";
import type { ConnectionView, RuleSetSummary, RuleTarget } from "../types";
import { scopeFilter, type TrafficScope } from "../trafficFilter";

/** Past this many rows the table renders only the visible window. */
const VIRTUALIZE_AFTER = 200;
/** Mirrors the fixed `.conn-table tbody tr` height in App.css. */
const ROW_H = 35;

function fmtTime(ms?: number | null) {
  if (!ms) return "—";
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

/** Derive the host (no port) for a request row, preferring the SNI host. */
function rowHost(r: ConnectionView): string {
  const h = r.host.trim();
  if (h) return h;
  const dest = r.destination.trim();
  if (!dest || dest === "—") return "";
  // destination looks like "example.com:443" or "1.2.3.4:443"
  const i = dest.lastIndexOf(":");
  return i > 0 ? dest.slice(0, i) : dest;
}

/**
 * Extract a DOMAIN-SUFFIX payload from a request.
 *
 * Returns the last two labels of the host (so `cdn.api.example.com` →
 * `example.com`), which is what most DOMAIN-SUFFIX rule lists want. IP
 * literals and single-label hosts fall back to the whole value.
 */
export function extractDomainSuffix(host: string): string {
  const h = host.trim().toLowerCase();
  if (!h) return "";
  // IPv4 / IPv6 / single label → can't split meaningfully.
  const isIp = /^\[?[0-9a-f:.]+\]?$/i.test(h);
  if (isIp || !h.includes(".")) return h;
  const labels = h.split(".").filter(Boolean);
  if (labels.length <= 2) return h;
  return labels.slice(-2).join(".");
}

const LAST_SET_KEY = "traffic.lastRuleSetId";

interface Props {
  /** When true, omit page chrome (used under Traffic tabs). */
  embedded?: boolean;
}

export function FailuresPage({ embedded = false }: Props) {
  const { t } = useI18n();
  const [rows, setRows] = useState<ConnectionView[]>([]);
  const [query, setQuery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [scope, setScope] = useState<TrafficScope>("all");
  const cursorRef = useRef(0);
  const generationRef = useRef(0);
  const fullReloadGenerationRef = useRef<number | null>(null);

  const reload = useCallback(async () => {
    const generation = ++generationRef.current;
    fullReloadGenerationRef.current = generation;
    try {
      const batch = await listRequestFailures(query.trim() || null, 800);
      if (generation !== generationRef.current) return;
      cursorRef.current = batch.cursor;
      setRows(batch.entries);
      setError(null);
    } catch (e) {
      if (generation !== generationRef.current) return;
      setError(typeof e === "string" ? e : String(e));
    } finally {
      if (generation === generationRef.current) setLoading(false);
      if (fullReloadGenerationRef.current === generation) {
        fullReloadGenerationRef.current = null;
      }
    }
  }, [query]);

  const loadIncremental = useCallback(async () => {
    const generation = generationRef.current;
    if (fullReloadGenerationRef.current === generation) return;
    try {
      const batch = await listRequestFailures(
        query.trim() || null,
        800,
        cursorRef.current,
      );
      if (generation !== generationRef.current) return;
      cursorRef.current = batch.cursor;
      if (batch.entries.length > 0) {
        setRows((current) => [
          ...batch.entries.slice().reverse(),
          ...current,
        ].slice(0, 800));
      }
      setError(null);
    } catch (e) {
      if (generation !== generationRef.current) return;
      setError(typeof e === "string" ? e : String(e));
    }
  }, [query]);

  useEffect(() => {
    void reload();
  }, [reload]);

  useVisibleInterval(() => loadIncremental(), 2500);

  async function onClear() {
    if (!confirm(t("req.clearConfirm"))) return;
    try {
      await clearRequestHistory();
      generationRef.current += 1;
      cursorRef.current = 0;
      setRows([]);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // —— Quick add-rule modal state ——
  const [addOpen, setAddOpen] = useState(false);
  const [addBusy, setAddBusy] = useState(false);
  const [addHost, setAddHost] = useState("");
  const [payload, setPayload] = useState("");
  const [target, setTarget] = useState<RuleTarget>("proxy");
  const [sets, setSets] = useState<RuleSetSummary[]>([]);
  const [setId, setSetId] = useState<string>("");
  const [newSetName, setNewSetName] = useState("");
  const [createNewSet, setCreateNewSet] = useState(false);

  const targetOpts: { value: RuleTarget; label: string }[] = useMemo(
    () => [
      { value: "proxy", label: t("rules.targetProxy") },
      { value: "direct", label: t("rules.targetDirect") },
      { value: "block", label: t("rules.targetBlock") },
    ],
    [t],
  );

  const scoped = useMemo(() => scopeFilter(rows, scope), [rows, scope]);
  // Per-row host/suffix parsing memoized so the 2.5s poll re-renders don't
  // re-split/regex every row (up to 800) on each pass.
  const scopedMeta = useMemo(
    () =>
      scoped.map((r) => {
        const host = rowHost(r);
        return { row: r, host, suffix: extractDomainSuffix(host) };
      }),
    [scoped],
  );
  const virtualized = scopedMeta.length > VIRTUALIZE_AFTER;
  const range = useVirtualRange({
    itemCount: scopedMeta.length,
    itemSize: ROW_H,
    enabled: virtualized,
  });
  const visibleMeta = virtualized
    ? scopedMeta.slice(range.start, range.end)
    : scopedMeta;
  const scopeOpts = useMemo(
    () => [
      { value: "all", label: t("traffic.scopeAll") },
      { value: "direct", label: t("traffic.scopeDirect") },
      { value: "proxy", label: t("traffic.scopeProxy") },
    ],
    [t],
  );

  const setOptions = useMemo(
    () => sets.map((s) => ({ value: s.id, label: s.name })),
    [sets],
  );

  /** Load rule sets + resolve the default: last-used, else first enabled, else first. */
  const reloadSets = useCallback(async () => {
    const list = await listRuleSets();
    setSets(list);
    const lastUsed = localStorage.getItem(LAST_SET_KEY) ?? "";
    const exists = list.some((s) => s.id === lastUsed);
    if (exists) {
      setSetId(lastUsed);
      return;
    }
    const preferred =
      list.find((s) => s.enabled)?.id ?? list[0]?.id ?? "";
    setSetId(preferred);
  }, []);

  function openAddRule(r: ConnectionView) {
    setError(null);
    const host = rowHost(r);
    const suffix = extractDomainSuffix(host);
    setAddHost(host);
    setPayload(suffix);
    setTarget("proxy");
    setNewSetName("");
    setCreateNewSet(false);
    setAddOpen(true);
    void reloadSets();
  }

  async function onAddSave(e: FormEvent) {
    e.preventDefault();
    const body = payload.trim();
    if (!body) {
      setError(t("fails.needPayload"));
      return;
    }
    if (createNewSet && !newSetName.trim()) {
      setError(t("rules.needName"));
      return;
    }
    setAddBusy(true);
    setError(null);
    try {
      let targetSetId = setId;
      if (createNewSet) {
        const set = await createRuleSet(newSetName.trim());
        const list = await listRuleSets();
        setSets(list);
        targetSetId = set.id;
      }
      if (!targetSetId) {
        setError(t("fails.needSet"));
        setAddBusy(false);
        return;
      }
      await saveRule({
        setId: targetSetId,
        ruleType: "domain_suffix",
        payload: body,
        target,
        enabled: true,
      });
      localStorage.setItem(LAST_SET_KEY, targetSetId);
      setSetId(targetSetId);
      setAddOpen(false);
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setAddBusy(false);
    }
  }

  const toolbar = (
    <div className={`traffic-toolbar ${embedded ? "" : "page-header"}`}>
      {!embedded && (
        <div>
          <h1>{t("fails.title")}</h1>
          <p className="page-desc">{t("fails.desc")}</p>
        </div>
      )}
      <div className="header-actions traffic-toolbar-actions">
        <input
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          className="search"
          placeholder={t("fails.filter")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <GlassSeg
          value={scope}
          ariaLabel={t("traffic.scopeLabel")}
          onChange={(v) => setScope(v as TrafficScope)}
          options={scopeOpts}
        />
        <GlassButton
          icon="↻"
          onClick={() => void reload()}
        >
          {t("common.refresh")}
        </GlassButton>
        <GlassButton
          variant="danger"
          icon="⌫"
          onClick={() => void onClear()}
        >
          {t("common.clear")}
        </GlassButton>
      </div>
    </div>
  );

  const body = (
    <>
      {error && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      <div className="muted mono traffic-meta">
        {t("fails.count", { n: scoped.length })}
        {query.trim() ? t("req.filterLabel", { q: query.trim() }) : ""}
      </div>

      {loading ? (
        <div className="empty">{t("common.loading")}</div>
      ) : scoped.length === 0 ? (
        <div className="empty card muted">{t("fails.empty")}</div>
      ) : (
        <div className="card table-wrap">
          <table className="conn-table fails-table">
            <thead>
              <tr>
                <th className="conn-th-time">{t("req.time")}</th>
                <th>{t("conn.dest")}</th>
                <th className="conn-th-node">{t("conn.node")}</th>
                <th className="conn-th-rule">{t("conn.rule")}</th>
                <th className="conn-th-process">{t("conn.process")}</th>
                <th className="conn-th-actions">{t("common.actions")}</th>
              </tr>
            </thead>
            <tbody ref={range.containerRef as React.RefObject<HTMLTableSectionElement>}>
              {range.paddingTop > 0 && (
                <tr aria-hidden className="virt-pad">
                  <td colSpan={6} style={{ height: range.paddingTop }} />
                </tr>
              )}
              {visibleMeta.map(({ row: r, host, suffix }) => (
                  <tr key={`${r.id}-${r.closed_at ?? r.last_seen ?? 0}`}>
                    <td className="conn-time">
                      <div className="conn-cell" title={fmtTime(r.closed_at ?? r.last_seen)}>
                        {fmtTime(r.closed_at ?? r.last_seen)}
                      </div>
                    </td>
                    <td>
                      <div
                        className="conn-cell conn-dest"
                        title={`${r.destination}${host ? ` · ${host}` : ""}`}
                      >
                        {r.destination}
                      </div>
                    </td>
                    <td>
                      <div
                        className="conn-cell conn-node"
                        title={
                          r.subscription_name
                            ? `${r.subscription_name} · ${r.node_name}`
                            : r.node_name
                        }
                      >
                        {r.node_name || r.node_tag || "—"}
                      </div>
                    </td>
                    <td>
                      <div className="conn-cell conn-rule" title={r.rule}>
                        {r.rule || "—"}
                      </div>
                    </td>
                    <td>
                      <div className="conn-cell" title={r.process}>
                        {r.process || "—"}
                      </div>
                    </td>
                    <td className="conn-actions-cell">
                      <button
                        type="button"
                        className="compact"
                        disabled={!suffix}
                        title={suffix ? `DOMAIN-SUFFIX, ${suffix}` : t("fails.noSuffix")}
                        onClick={() => openAddRule(r)}
                      >
                        {t("fails.addRule")}
                      </button>
                    </td>
                  </tr>
                ))}
              {range.paddingBottom > 0 && (
                <tr aria-hidden className="virt-pad">
                  <td colSpan={6} style={{ height: range.paddingBottom }} />
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}
    </>
  );

  const modal = addOpen && (
    <div className="modal-backdrop">
      <div className="modal">
        <header className="modal-header">
          <h2>{t("fails.addRuleTitle")}</h2>
          <button
            type="button"
            className="icon-btn"
            disabled={addBusy}
            onClick={() => setAddOpen(false)}
          >
            ×
          </button>
        </header>
        <form className="modal-body" onSubmit={(e) => void onAddSave(e)}>
          <div className="field">
            <span>{t("fails.ruleSet")}</span>
            <div className="fails-set-row">
              <label className="inline">
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  type="radio"
                  checked={!createNewSet}
                  onChange={() => setCreateNewSet(false)}
                />
                <SolidSelect
                  value={setId}
                  options={setOptions}
                  onChange={setSetId}
                  disabled={createNewSet || setOptions.length === 0}
                  aria-label={t("fails.ruleSet")}
                  placeholder={setOptions.length === 0 ? t("fails.noSets") : undefined}
                />
              </label>
            </div>
            <label className="inline fails-new-set">
              <input
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                type="radio"
                checked={createNewSet}
                onChange={() => setCreateNewSet(true)}
              />
              <input
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                value={newSetName}
                onChange={(e) => setNewSetName(e.target.value)}
                placeholder={t("fails.newSetPh")}
                maxLength={64}
                disabled={!createNewSet}
              />
            </label>
          </div>

          <div className="field">
            <span>{t("rules.type")}</span>
            <code className="fails-type-tag">DOMAIN-SUFFIX</code>
            <span className="muted" style={{ fontSize: 12 }}>
              {t("fails.typeHint")}
            </span>
          </div>

          <label className="field">
            <span>{t("rules.payload")}</span>
            <input
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              value={payload}
              onChange={(e) => setPayload(e.target.value)}
              placeholder={t("fails.payloadPh")}
              autoFocus
            />
            {addHost && addHost !== payload && (
              <span className="muted" style={{ fontSize: 12 }}>
                {t("fails.fromHost", { host: addHost })}
              </span>
            )}
          </label>

          <div className="field">
            <span>{t("rules.outbound")}</span>
            <SolidSelect
              value={target}
              options={targetOpts}
              onChange={(v) => setTarget(v as RuleTarget)}
              aria-label={t("rules.outbound")}
            />
          </div>

          <footer className="modal-footer">
            <GlassButton
              disabled={addBusy}
              onClick={() => setAddOpen(false)}
            >
              {t("common.cancel")}
            </GlassButton>
            <GlassButton
              type="submit"
              variant="primary"
              disabled={
                addBusy ||
                !payload.trim() ||
                (createNewSet && !newSetName.trim())
              }
            >
              {addBusy ? t("common.saving") : t("common.save")}
            </GlassButton>
          </footer>
        </form>
      </div>
    </div>
  );

  if (embedded) {
    return (
      <div className="traffic-embed">
        {toolbar}
        {body}
        {modal}
      </div>
    );
  }

  return (
    <div className="page">
      {toolbar}
      {body}
      {modal}
    </div>
  );
}
