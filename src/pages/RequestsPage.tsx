import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { clearRequestHistory, listRequests } from "../api";
import { GlassButton } from "../components/GlassButton";
import { GlassSeg } from "../components/GlassSeg";
import { useVirtualRange } from "../hooks/useVirtualRange";
import { useVisibleInterval } from "../hooks/useVisibleInterval";
import { useI18n } from "../i18n";
import { ErrorModal } from "../components/ErrorModal";
import type { ConnectionView } from "../types";
import { scopeFilter, type TrafficScope } from "../trafficFilter";

/** Past this many rows the table renders only the visible window. */
const VIRTUALIZE_AFTER = 200;
/** Mirrors the fixed `.conn-table tbody tr` height in App.css. */
const ROW_H = 35;

function fmtBytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

function fmtTime(ms?: number | null) {
  if (!ms) return "—";
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

interface Props {
  /** When true, omit page chrome (used under Traffic tabs). */
  embedded?: boolean;
}

export function RequestsPage({ embedded = false }: Props) {
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
      const batch = await listRequests(query.trim() || null, 800);
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
      const batch = await listRequests(
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

  // History UI can refresh slower; journal keeps filling in Rust.
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

  const scoped = useMemo(() => scopeFilter(rows, scope), [rows, scope]);
  const virtualized = scoped.length > VIRTUALIZE_AFTER;
  const range = useVirtualRange({
    itemCount: scoped.length,
    itemSize: ROW_H,
    enabled: virtualized,
  });
  const visibleRows = virtualized ? scoped.slice(range.start, range.end) : scoped;
  const scopeOpts = useMemo(
    () => [
      { value: "all", label: t("traffic.scopeAll") },
      { value: "direct", label: t("traffic.scopeDirect") },
      { value: "proxy", label: t("traffic.scopeProxy") },
    ],
    [t],
  );

  const toolbar = (
    <div className={`traffic-toolbar ${embedded ? "" : "page-header"}`}>
      {!embedded && (
        <div>
          <h1>{t("req.title")}</h1>
          <p className="page-desc">{t("req.desc")}</p>
        </div>
      )}
      <div className="header-actions traffic-toolbar-actions">
        <input
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          className="search"
          placeholder={t("req.filter")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <GlassSeg
          value={scope}
          ariaLabel={t("traffic.scopeLabel")}
          onChange={(v) => setScope(v as TrafficScope)}
          options={scopeOpts}
        />
        <GlassButton icon="↻" onClick={() => void reload()}>
          {t("common.refresh")}
        </GlassButton>
        <GlassButton variant="danger" icon="⌫" onClick={() => void onClear()}>
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
        {t("req.count", { n: scoped.length })}
        {query.trim() ? t("req.filterLabel", { q: query.trim() }) : ""}
      </div>

      {loading ? (
        <div className="empty">{t("common.loading")}</div>
      ) : scoped.length === 0 ? (
        <div className="empty card muted">{t("req.empty")}</div>
      ) : (
        <div className="card table-wrap">
          <table className="conn-table">
            <thead>
              <tr>
                <th className="conn-th-time">{t("req.time")}</th>
                <th>{t("conn.dest")}</th>
                <th className="conn-th-node">{t("conn.node")}</th>
                <th className="conn-th-rule">{t("conn.rule")}</th>
                <th className="conn-th-process">{t("conn.process")}</th>
                <th className="conn-th-traffic">{t("conn.traffic")}</th>
              </tr>
            </thead>
            <tbody ref={range.containerRef as React.RefObject<HTMLTableSectionElement>}>
              {range.paddingTop > 0 && (
                <tr aria-hidden className="virt-pad">
                  <td colSpan={6} style={{ height: range.paddingTop }} />
                </tr>
              )}
              {visibleRows.map((r) => (
                <tr key={`${r.id}-${r.last_seen ?? 0}`}>
                  <td className="conn-time">
                    <div
                      className="conn-cell"
                      title={`${fmtTime(r.closed_at ?? r.last_seen)}${
                        r.first_seen && r.first_seen !== (r.closed_at ?? r.last_seen)
                          ? ` · ${t("req.first", { t: fmtTime(r.first_seen) })}`
                          : ""
                      }`}
                    >
                      {fmtTime(r.closed_at ?? r.last_seen)}
                    </div>
                  </td>
                  <td>
                    <div
                      className="conn-cell conn-dest"
                      title={`${r.destination}${r.host || r.source ? ` · ${r.host || r.source}` : ""}`}
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
                  <td className="conn-traffic">
                    <span title={`↑${fmtBytes(r.upload)} ↓${fmtBytes(r.download)}`}>
                      <span className="tr-dir up">↑</span>{fmtBytes(r.upload)}{" "}
                      <span className="tr-dir down">↓</span>{fmtBytes(r.download)}
                    </span>
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

  if (embedded) {
    return (
      <div className="traffic-embed">
        {toolbar}
        {body}
      </div>
    );
  }

  return (
    <div className="page">
      {toolbar}
      {body}
    </div>
  );
}
