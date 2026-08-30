import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  clearAppLogs,
  clearCoreLog,
  getCoreLogTail,
  getProxyStatus,
  listAppLogs,
  type AppLogEntry,
  type AppLogLevel,
} from "../api";
import { GlassButton } from "../components/GlassButton";
import { GlassSeg } from "../components/GlassSeg";
import { GlassSwitch } from "../components/GlassSwitch";
import { useVirtualRange } from "../hooks/useVirtualRange";
import { useVisibleInterval } from "../hooks/useVisibleInterval";
import { useI18n } from "../i18n";
import { ErrorModal } from "../components/ErrorModal";
import {
  CORE_LEVELS,
  coreLevelRank,
  coreLogLevel,
  type CoreLogLevel,
} from "../coreLog";

const LEVELS: AppLogLevel[] = ["error", "warn", "info", "debug", "trace"];

type LogsTab = "app" | "singbox" | "xray" | "mihomo";

const CORE_TAB_KINDS: { value: "singbox" | "xray" | "mihomo"; label: string }[] = [
  { value: "singbox", label: "sing-box" },
  { value: "xray", label: "Xray" },
  { value: "mihomo", label: "mihomo" },
];

/** Past this many lines the list renders only the visible window. */
const VIRTUALIZE_AFTER = 200;
/** Mirrors the fixed `.log-line` height in App.css. */
const ROW_H = 25;

function levelRank(l: AppLogLevel): number {
  switch (l) {
    case "trace":
      return 0;
    case "debug":
      return 1;
    case "info":
      return 2;
    case "warn":
      return 3;
    case "error":
      return 4;
  }
}

function fmtTs(ms: number) {
  try {
    const d = new Date(ms);
    return d.toLocaleTimeString(undefined, {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      fractionalSecondDigits: 3,
    } as Intl.DateTimeFormatOptions);
  } catch {
    return String(ms);
  }
}

export function LogsPage() {
  const { t } = useI18n();
  const [tab, setTab] = useState<LogsTab>("app");
  const [minLevel, setMinLevel] = useState<AppLogLevel>("info");
  const [coreMinLevel, setCoreMinLevel] = useState<CoreLogLevel>("info");
  const [query, setQuery] = useState("");
  const [rows, setRows] = useState<AppLogEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [autoScroll, setAutoScroll] = useState(true);
  // Which kernels currently have a live process — drives the tab dots.
  const [runningKinds, setRunningKinds] = useState<Set<string>>(new Set());
  const listRef = useRef<HTMLDivElement>(null);
  const cursorRef = useRef(0);
  const generationRef = useRef(0);
  const fullReloadGenerationRef = useRef<number | null>(null);

  const isCoreTab = tab !== "app";
  const coreKind = isCoreTab ? tab : null;

  // —— app log (structured entries) ——
  const reload = useCallback(async () => {
    const generation = ++generationRef.current;
    fullReloadGenerationRef.current = generation;
    try {
      const batch = await listAppLogs({
        minLevel,
        limit: 800,
        query: query.trim() || null,
      });
      if (generation !== generationRef.current) return;
      cursorRef.current = batch.cursor;
      setRows(batch.entries);
      setError(null);
    } catch (e) {
      if (generation !== generationRef.current) return;
      setError(typeof e === "string" ? e : String(e));
    } finally {
      if (fullReloadGenerationRef.current === generation) {
        fullReloadGenerationRef.current = null;
      }
    }
  }, [minLevel, query]);

  const loadIncremental = useCallback(async () => {
    const generation = generationRef.current;
    if (fullReloadGenerationRef.current === generation) return;
    try {
      const batch = await listAppLogs({
        minLevel,
        limit: 800,
        query: query.trim() || null,
        afterId: cursorRef.current,
      });
      if (generation !== generationRef.current) return;
      cursorRef.current = batch.cursor;
      if (batch.entries.length > 0) {
        setRows((current) => [...current, ...batch.entries].slice(-800));
      }
      setError(null);
    } catch (e) {
      if (generation !== generationRef.current) return;
      setError(typeof e === "string" ? e : String(e));
    }
  }, [minLevel, query]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // —— kernel log (raw tail, per tab kind) ——
  const [coreLines, setCoreLines] = useState<string[]>([]);
  const [corePath, setCorePath] = useState<string | null>(null);
  const coreReload = useCallback(async () => {
    if (!coreKind) return;
    try {
      const tail = await getCoreLogTail(400, coreKind);
      setCoreLines(tail.lines);
      setCorePath(tail.path);
    } catch {
      /* transient — keep the last view */
    }
  }, [coreKind]);

  useEffect(() => {
    if (!coreKind) return;
    setCoreLines([]);
    setCorePath(null);
    void coreReload();
  }, [coreReload]);

  // Poll: app log increments while on the app tab; kernel tail refreshes on
  // kernel tabs; proxy status always feeds the running dots.
  useVisibleInterval(
    () => {
      const jobs: Promise<unknown>[] = [getProxyStatus().then((s) => {
        const next = new Set<string>();
        if (s.running && s.core_type) next.add(s.core_type);
        if (s.sidecar_running) next.add("xray");
        setRunningKinds(next);
      }).catch(() => setRunningKinds(new Set()))];
      if (tab === "app") jobs.push(loadIncremental());
      else jobs.push(coreReload());
      return Promise.all(jobs);
    },
    1200,
  );

  // Auto-scroll: app log pins to the bottom (append), kernel log pins to the
  // top (newest-first).
  useEffect(() => {
    if (!autoScroll || !listRef.current) return;
    listRef.current.scrollTop = tab === "app"
      ? listRef.current.scrollHeight
      : 0;
  }, [tab, autoScroll, rows, coreLines]);

  async function onClear() {
    try {
      if (tab === "app") {
        await clearAppLogs();
        generationRef.current += 1;
        cursorRef.current = 0;
        setRows([]);
      } else {
        await clearCoreLog(tab);
        setCoreLines([]);
        setCorePath(null);
        void coreReload();
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // —— shared derived state ——
  const appCountLabel = useMemo(() => `${rows.length}`, [rows.length]);

  const appVirtualized = rows.length > VIRTUALIZE_AFTER;
  const appRange = useVirtualRange({
    itemCount: rows.length,
    itemSize: ROW_H,
    enabled: appVirtualized && tab === "app",
    // The log list scrolls inside its own panel, not the app shell.
    scrollerSelector: ".logs-panel",
  });
  const appVisibleRows = appVirtualized
    ? rows.slice(appRange.start, appRange.end)
    : rows;

  const coreRows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return coreLines
      .slice()
      .reverse()
      .map((line, i) => ({ id: i, level: coreLogLevel(line), msg: line }))
      .filter((r) => coreLevelRank(r.level) >= coreLevelRank(coreMinLevel))
      .filter((r) => !q || r.msg.toLowerCase().includes(q));
  }, [coreLines, coreMinLevel, query]);

  const countLabel = tab === "app" ? appCountLabel : `${coreRows.length}`;

  const tabOptions = [
    { value: "app" as const, label: t("logs.tabApp") },
    ...CORE_TAB_KINDS.map((k) => ({
      value: k.value,
      label: (
        <>
          <span
            className={`seg-dot${runningKinds.has(k.value) ? " on" : ""}`}
            aria-hidden
          />
          {k.label}
        </>
      ),
    })),
  ];

  return (
    <div className="page logs-page">
      <div className="page-header traffic-header">
        <div>
          <h1>{t("logs.title")}</h1>
          <p className="page-desc">
            {tab === "app" ? t("logs.desc") : t("logs.coreDesc")}
          </p>
        </div>
        <div className="header-actions traffic-toolbar-actions">
          <span className="muted mono" style={{ fontSize: 12 }}>
            {countLabel}
          </span>
          <GlassSwitch
            checked={autoScroll}
            onChange={setAutoScroll}
            label={t("logs.autoScroll")}
            title={t("logs.autoScroll")}
            capsule
            size="sm"
          />
          <GlassButton
            icon="↻"
            onClick={() => void (tab === "app" ? reload() : coreReload())}
            title={t("common.refresh")}
          >
            {t("common.refresh")}
          </GlassButton>
          <GlassButton
            variant="danger"
            icon="⌫"
            onClick={() => void onClear()}
            title={t("common.clear")}
          >
            {t("common.clear")}
          </GlassButton>
        </div>
      </div>

      <div className="logs-toolbar">
        <GlassSeg
          value={tab}
          ariaLabel={t("logs.title")}
          onChange={(v) => setTab(v as LogsTab)}
          options={tabOptions}
        />
        {tab === "app" ? (
          <GlassSeg
            value={minLevel}
            ariaLabel={t("logs.level")}
            onChange={(v) => setMinLevel(v as AppLogLevel)}
            titles={Object.fromEntries(
              LEVELS.map((lv) => [lv, `${t("logs.minLevel")}: ${lv}`]),
            )}
            options={LEVELS.map((lv) => ({ value: lv, label: lv }))}
          />
        ) : (
          <GlassSeg
            value={coreMinLevel}
            ariaLabel={t("logs.level")}
            onChange={(v) => setCoreMinLevel(v as CoreLogLevel)}
            titles={Object.fromEntries(
              CORE_LEVELS.map((lv) => [lv, `${t("logs.minLevel")}: ${lv}`]),
            )}
            options={CORE_LEVELS.map((lv) => ({ value: lv, label: lv }))}
          />
        )}
        <input
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          className="search"
          placeholder={t("logs.filter")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      {error && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      {tab === "app" ? (
        <div className="logs-panel card glass" ref={listRef}>
          {rows.length === 0 ? (
            <p className="muted logs-empty">{t("logs.empty")}</p>
          ) : (
            <ul
              className="logs-list mono"
              ref={appRange.containerRef as React.RefObject<HTMLUListElement>}
            >
              {appRange.paddingTop > 0 && (
                <li aria-hidden className="virt-pad" style={{ height: appRange.paddingTop }} />
              )}
              {appVisibleRows.map((e) => (
                <li
                  key={e.id}
                  className={`log-line log-${e.level}`}
                  data-level={e.level}
                  title={e.message}
                  style={{
                    opacity: levelRank(e.level) < levelRank(minLevel) ? 0.5 : 1,
                  }}
                >
                  <span className="log-ts">{fmtTs(e.ts_ms)}</span>
                  <span className={`log-lvl log-lvl-${e.level}`}>{e.level}</span>
                  <span className="log-target">{e.target}</span>
                  <span className="log-msg">{e.message}</span>
                </li>
              ))}
              {appRange.paddingBottom > 0 && (
                <li aria-hidden className="virt-pad" style={{ height: appRange.paddingBottom }} />
              )}
            </ul>
          )}
        </div>
      ) : (
        <>
          <div className="logs-panel card glass" ref={listRef}>
            {coreRows.length === 0 ? (
              <p className="muted logs-empty">{t("logs.coreEmpty")}</p>
            ) : (
              <ul className="logs-list mono">
                {coreRows.map((r) => (
                  <li
                    key={r.id}
                    className={`log-line core-log-line log-${r.level}`}
                    data-level={r.level}
                    title={r.msg}
                  >
                    <span className={`log-lvl log-lvl-${r.level}`}>
                      {r.level}
                    </span>
                    <span className="log-msg">{r.msg}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          {corePath && <p className="logs-core-path muted mono">{corePath}</p>}
        </>
      )}
    </div>
  );
}
