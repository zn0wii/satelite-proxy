import { useCallback, useEffect, useMemo, useState } from "react";
import {
  generateSingboxConfig,
  getProxyStatus,
  getSettings,
  listCustomConfigNodes,
  listNodeIds,
  listNodesPage,
  pingNodesLatency,
  setCurrentNode,
  testCustomNodesLatency,
  testNodesLatency,
} from "../api";
import { GlassButton } from "../components/GlassButton";
import { ErrorModal } from "../components/ErrorModal";
import { useI18n } from "../i18n";
import { groupNodes, type GroupBy } from "../nodeGroups";
import { GlassSeg } from "../components/GlassSeg";
import { waitForCoreRestart } from "../coreBusy";
import { useVirtualRange } from "../hooks/useVirtualRange";
import { filterCustomNodes, applyCustomLatency, type CustomLatencyMap } from "../customNodes";
import type { AutoSelectMode, ProxyNode, SortMode, ViewMode } from "../types";

const VIRTUALIZE_AFTER = 200;
const LIST_ROW_HEIGHT = 49;
const GRID_ROW_HEIGHT = 94;
const PAGE_SIZE = 200;

/** Slim group header band height (px). */
const NODE_GROUP_H = 30;
/** .node-grid row gap (0.65rem) — a spanning header row is followed by the
 *  gap before the next card row, so its pitch includes it. */
const GRID_GAP = 10.4;

/** Flat render items with per-item heights: the virtualizer runs in pixel
 *  space (itemSize=1) and a prefix-offset window maps px → items, which
 *  keeps slim headers + collapsible groups exact. */
type ListItem =
  | {
      type: "group";
      key: string;
      label: string;
      flag?: string;
      count: number;
      h: number;
    }
  | { type: "node"; n: ProxyNode; h: number };
type GridItem = ListItem;

/** Render latency cell: spinner / ms / timeout / needs-core / dash */
function LatencyDisplay({
  ms,
  latencyAt,
  testing,
  unsupported,
  unsupportedLabel,
}: {
  ms?: number | null;
  latencyAt?: number | null;
  testing: boolean;
  unsupported?: boolean;
  /** Overrides the default "start core" note — e.g. after a ping test the
      QUIC-only note applies instead (the core isn't involved at all). */
  unsupportedLabel?: string;
}) {
  const { t } = useI18n();
  if (testing) {
    return <span className="lat-spinner" aria-label="测试中" />;
  }
  if (unsupported) {
    const label = unsupportedLabel ?? t("nodes.latencyNeedsCore");
    return <span className="lat lat-none" title={label}>{label}</span>;
  }
  if (ms != null && ms >= 0) {
    return (
      <span className={`lat ${latencyClass(ms)}`}>{ms}ms</span>
    );
  }
  // tested but no value → timeout
  if (latencyAt != null) {
    return <span className="lat lat-timeout">timeout</span>;
  }
  return <span className="lat lat-none">—</span>;
}

function latencyClass(ms?: number | null) {
  if (ms == null || ms < 0) return "lat-none";
  if (ms < 200) return "lat-good";
  if (ms < 300) return "lat-ok";
  return "lat-slow";
}

export function NodesPage() {
  const { t, locale } = useI18n();
  const [nodes, setNodes] = useState<ProxyNode[]>([]);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [autoSelect, setAutoSelect] = useState<AutoSelectMode>("off");
  // Manual click in kernel-auto mode: urltest → selector rebuild restarts the core.
  const [switching, setSwitching] = useState(false);
  const [viewMode, setViewMode] = useState<ViewMode>(() => {
    return (localStorage.getItem("nodes.viewMode") as ViewMode) || "list";
  });
  const [sortMode, setSortMode] = useState<SortMode>(() => {
    return (localStorage.getItem("nodes.sortMode") as SortMode) || "default";
  });
  // Click-test mode: node clicks probe latency instead of selecting.
  const [clickTest, setClickTest] = useState<boolean>(
    () => localStorage.getItem("nodes.clickTest") === "1",
  );

  const [customRuntime, setCustomRuntime] = useState(false);
  // Session-only latency results for custom-mode nodes (not persisted backend-side).
  const [customLatency, setCustomLatency] = useState<CustomLatencyMap>(new Map());
  const [testing, setTesting] = useState(false);
  const [testingIds, setTestingIds] = useState<Set<string>>(new Set());
  // Which probe the current/last run used — "real" rides the kernel's proxy
  // path, "ping" is direct TCP; drives button labels and the unsupported note.
  const [testKind, setTestKind] = useState<"real" | "ping">("real");
  // Node ids whose last test used method "unsupported" (UDP-only protocol,
  // core not running) — shown as "start core to test" instead of "timeout".
  const [unsupportedIds, setUnsupportedIds] = useState<Set<string>>(new Set());
  // Protocols delegated to the companion Xray sidecar (from settings) —
  // surfaced as a small badge so the egress path is visible per node.
  const [delegatedProtocols, setDelegatedProtocols] = useState<Set<string>>(
    new Set(),
  );
  const reload = useCallback(async (append = false) => {
    setError(null);
    if (append) setLoadingMore(true);
    try {
      const settings = await getSettings();
      const custom = (settings.runtime_source ?? "generated").startsWith("singbox:");
      setCustomRuntime(custom);
      setCurrentId(settings.current_node_id ?? null);
      setAutoSelect((settings.auto_select as AutoSelectMode) ?? "off");
      setDelegatedProtocols(
        settings.multi_core_enabled
          ? new Set(
              (settings.protocol_cores ?? [])
                .filter((e) => e.core === "xray")
                .map((e) => e.protocol),
            )
          : new Set(),
      );
      const offset = append ? nodes.length : 0;
      if (custom) {
        // Custom mode: read-only nodes extracted from the sing-box config,
        // overlaid with this session's latency results.
        const all = applyCustomLatency(await listCustomConfigNodes(), customLatency);
        const filtered = filterCustomNodes(all, query, sortMode, offset, PAGE_SIZE);
        setNodes((prev) => (append ? [...prev, ...filtered.nodes] : filtered.nodes));
        setTotal(filtered.total);
      } else {
        const page = await listNodesPage(query, sortMode, offset, PAGE_SIZE);
        setNodes((prev) => (append ? [...prev, ...page.nodes] : page.nodes));
        setTotal(page.total);
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setLoading(false);
      setLoadingMore(false);
    }
  }, [nodes.length, query, sortMode, customLatency]);

  useEffect(() => {
    setLoading(true);
    const timer = window.setTimeout(() => void reload(false), 150);
    return () => window.clearTimeout(timer);
    // nodes.length changes as pages append and must not restart the first page.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, sortMode]);

  useEffect(() => {
    localStorage.setItem("nodes.viewMode", viewMode);
  }, [viewMode]);

  useEffect(() => {
    localStorage.setItem("nodes.sortMode", sortMode);
  }, [sortMode]);

  useEffect(() => {
    localStorage.setItem("nodes.clickTest", clickTest ? "1" : "0");
  }, [clickTest]);

  // Grouping: default (flat) / subscription / protocol / country, persisted
  // like viewMode. v2 key: the first iteration persisted "sub" as its
  // default — the feature is unreleased, so bump the key to let every
  // profile start on the new "default = flat" preference.
  const [groupBy, setGroupBy] = useState<GroupBy>(
    () =>
      (localStorage.getItem("nodes.groupBy.v2") as GroupBy | null) || "none",
  );
  useEffect(() => {
    localStorage.setItem("nodes.groupBy.v2", groupBy);
  }, [groupBy]);

  const displayed = nodes;

  // Flat render items: slim collapsible group headers interleave with
  // nodes; each item carries its own height (headers are slimmer than
  // rows) and the virtualizer runs in pixel space over prefix offsets.
  const groups = useMemo(
    () =>
      groupNodes(displayed, groupBy, locale, {
        other: t("nodes.groupOther"),
        noSub: t("nodes.groupNoSub"),
      }),
    [displayed, groupBy, locale, t],
  );

  // Collapsed group keys (session-only — collapse is a browsing gesture).
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(
    () => new Set(),
  );
  function toggleGroup(key: string) {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  const listItems = useMemo(() => {
    const out: ListItem[] = [];
    if (groups.length === 0) {
      for (const n of displayed) out.push({ type: "node", n, h: LIST_ROW_HEIGHT });
      return out;
    }
    for (const g of groups) {
      const open = !collapsedGroups.has(g.key);
      out.push({
        type: "group",
        key: g.key,
        label: g.label,
        flag: g.flag,
        count: g.nodes.length,
        h: NODE_GROUP_H,
      });
      if (open) {
        for (const n of g.nodes)
          out.push({ type: "node", n, h: LIST_ROW_HEIGHT });
      }
    }
    return out;
  }, [groups, displayed, collapsedGroups]);

  const gridItems = useMemo(() => {
    const out: GridItem[] = [];
    if (groups.length === 0) {
      for (const n of displayed)
        out.push({ type: "node", n, h: GRID_ROW_HEIGHT });
      return out;
    }
    for (const g of groups) {
      const open = !collapsedGroups.has(g.key);
      // Header pitch includes the row gap that follows the band.
      out.push({
        type: "group",
        key: g.key,
        label: g.label,
        flag: g.flag,
        count: g.nodes.length,
        h: NODE_GROUP_H + GRID_GAP,
      });
      if (open) {
        for (const n of g.nodes)
          out.push({ type: "node", n, h: GRID_ROW_HEIGHT });
      }
    }
    return out;
  }, [groups, displayed, collapsedGroups]);

  const virtualized = displayed.length > VIRTUALIZE_AFTER;

  // Pixel-space virtualization: itemSize=1 turns the hook into a px window
  // over the prefix-offset items (slim headers ≠ node rows, and collapse
  // changes counts dynamically — both need per-item offsets).
  function offsetsOf(items: { h: number }[]): number[] {
    const o = new Array<number>(items.length + 1);
    o[0] = 0;
    for (let i = 0; i < items.length; i++) o[i + 1] = o[i] + items[i].h;
    return o;
  }
  function visibleWindow<T extends { h: number }>(
    items: T[],
    offsets: number[],
    startPx: number,
    endPx: number,
  ) {
    let lo = 0;
    let hi = items.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (offsets[mid + 1] <= startPx) lo = mid + 1;
      else hi = mid;
    }
    const first = lo;
    let last = first;
    while (last < items.length && offsets[last] < endPx) last++;
    const total = offsets[items.length] ?? 0;
    const bottom = offsets[last] ?? total;
    return {
      first,
      last,
      top: offsets[first],
      bottom,
      bottomPad: Math.max(0, total - bottom),
    };
  }

  const listOffsets = useMemo(() => offsetsOf(listItems), [listItems]);
  const gridOffsets = useMemo(() => offsetsOf(gridItems), [gridItems]);
  const listPx = useVirtualRange({
    itemCount: Math.max(1, listOffsets[listOffsets.length - 1]),
    itemSize: 1,
    enabled: virtualized,
    overscanRows: 400,
  });
  const gridPx = useVirtualRange({
    itemCount: Math.max(1, gridOffsets[gridOffsets.length - 1]),
    itemSize: 1,
    enabled: virtualized,
    overscanRows: 400,
  });
  const listWin = useMemo(
    () =>
      visibleWindow(
        listItems,
        listOffsets,
        Math.max(0, listPx.start),
        Math.min(listPx.end, listOffsets[listOffsets.length - 1] ?? 0),
      ),
    [listItems, listOffsets, listPx],
  );
  const gridWin = useMemo(
    () =>
      visibleWindow(
        gridItems,
        gridOffsets,
        Math.max(0, gridPx.start),
        Math.min(gridPx.end, gridOffsets[gridOffsets.length - 1] ?? 0),
      ),
    [gridItems, gridOffsets, gridPx],
  );

  async function onSelect(id: string) {
    if (busyId || switching) return;
    setBusyId(id);
    setError(null);
    try {
      const leavingKernel = autoSelect === "kernel";
      await setCurrentNode(id);
      setCurrentId(id);
      setAutoSelect("off");
      // Running: Clash API hot-switch — UI selection is enough feedback.
      // Stopped: write active.json so next start uses the new node.
      const status = await getProxyStatus().catch(() => null);
      if (!status?.running) {
        await generateSingboxConfig();
      } else if (leavingKernel) {
        // Main group rebuilds urltest → selector: hold the busy feedback
        // until the core restart finishes.
        setSwitching(true);
        await waitForCoreRestart();
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setSwitching(false);
      setBusyId(null);
    }
  }

  async function onTest(kind: "real" | "ping") {
    if (testing || displayed.length === 0) return;
    setTesting(true);
    setTestKind(kind);
    setError(null);
    // no top banner / completion message
    // Custom mode probes the extracted (unsaved) nodes — ids come from the
    // loaded list because they are not in the node store.
    const ids = customRuntime ? nodes.map((n) => n.id) : await listNodeIds(query);
    const idSet = new Set(ids);
    setTestingIds(idSet);

    // clear prior latency so only spinner shows while testing
    setNodes((prev) =>
      prev.map((n) =>
        idSet.has(n.id)
          ? { ...n, latency_ms: undefined, latency_at: undefined }
          : n,
      ),
    );

    try {
      // Custom mode can't map into the running config, so both probes are
      // the same direct-TCP path there.
      const batch = customRuntime
        ? await testCustomNodesLatency(3000)
        : kind === "ping"
          ? await pingNodesLatency(ids, 3000)
          : await testNodesLatency(ids, 3000);
      const map = new Map(batch.results.map((r) => [r.id, r]));
      setUnsupportedIds(
        new Set(batch.results.filter((r) => r.method === "unsupported").map((r) => r.id)),
      );
      if (customRuntime) {
        // Session-only — remember results across filter / sort / page reloads.
        setCustomLatency((prev) => {
          const next = new Map(prev);
          for (const r of batch.results) {
            next.set(r.id, { ms: r.latency_ms ?? null, at: r.tested_at });
          }
          return next;
        });
      }
      setNodes((prev) =>
        prev.map((n) => {
          const r = map.get(n.id);
          if (!r) return n;
          return {
            ...n,
            // null = failed → show timeout; number = success
            latency_ms: r.latency_ms ?? null,
            latency_at: r.tested_at,
          };
        }),
      );
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
      if (!customRuntime) await reload();
    } finally {
      setTesting(false);
      setTestingIds(new Set());
      // Custom results are session-only — keep the merged values instead of
      // re-reading the latency-less extracted list.
      if (!customRuntime) await reload(false);
    }
  }

  // After a ping run, "unsupported" means QUIC-only (unpingable), not "core
  // stopped" — swap the cell note accordingly.
  const pingNote = testKind === "ping" ? t("nodes.pingUnsupported") : undefined;

  // Click-test mode: probe one node with the real-latency path (Clash delay
  // API through the core; TCP fallback when the core is stopped). The backend
  // persists the result, same as the batch run.
  async function onTestOne(id: string) {
    if (testing || testingIds.size > 0 || busyId || switching) return;
    setTestKind("real");
    setError(null);
    setTestingIds(new Set([id]));
    setNodes((prev) =>
      prev.map((n) =>
        n.id === id ? { ...n, latency_ms: undefined, latency_at: undefined } : n,
      ),
    );
    try {
      const batch = await testNodesLatency([id], 3000);
      const r = batch.results.find((x) => x.id === id);
      setUnsupportedIds((prev) => {
        const next = new Set(prev);
        if (r?.method === "unsupported") next.add(id);
        else next.delete(id);
        return next;
      });
      if (r) {
        setNodes((prev) =>
          prev.map((n) =>
            n.id === id
              ? { ...n, latency_ms: r.latency_ms ?? null, latency_at: r.tested_at }
              : n,
          ),
        );
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setTestingIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  }

  /** Slim collapsible group header row (list). */
  function renderGroupRow(item: Extract<ListItem, { type: "group" }>) {
    const open = !collapsedGroups.has(item.key);
    return (
      <tr
        key={item.key}
        className="node-group-row"
        onClick={() => toggleGroup(item.key)}
        title={t("nodes.groupToggleHint")}
      >
        <td colSpan={6}>
          <span className={`node-group-caret${open ? "" : " closed"}`}>
            ▾
          </span>
          <span className="node-group-label">
            {item.flag ? <span className="node-group-flag">{item.flag}</span> : null}
            {item.label}
          </span>
          <span className="node-group-count mono">{item.count}</span>
        </td>
      </tr>
    );
  }

  /** Slim collapsible group header band (grid), spans all columns. */
  function renderGroupHead(item: Extract<GridItem, { type: "group" }>) {
    const open = !collapsedGroups.has(item.key);
    return (
      <div
        key={item.key}
        className="node-group-head"
        style={{ height: NODE_GROUP_H }}
        onClick={() => toggleGroup(item.key)}
        title={t("nodes.groupToggleHint")}
      >
        <span className={`node-group-caret${open ? "" : " closed"}`}>
          ▾
        </span>
        <span className="node-group-label">
          {item.flag ? <span className="node-group-flag">{item.flag}</span> : null}
          {item.label}
        </span>
        <span className="node-group-count mono">{item.count}</span>
      </div>
    );
  }

  function renderNodeRow(n: ProxyNode) {
                const active = n.id === currentId;
                const isTesting = testingIds.has(n.id);
                return (
                  <tr
                    key={n.id}
                    className={`node-virtual-row ${active ? "row-active" : ""}`}
                    onClick={
                      customRuntime
                        ? undefined
                        : clickTest
                          ? () => void onTestOne(n.id)
                          : () => void onSelect(n.id)
                    }
                    style={{ cursor: customRuntime ? "default" : "pointer" }}
                    title={
                      !customRuntime && clickTest ? t("nodes.clickTestLatency") : undefined
                    }
                  >
                    <td>{active ? "●" : "○"}</td>
                    <td>
                      <div className="node-list-name">{n.name}</div>
                      {n.subscription_name ? (
                        <div className="node-sub-label" title={n.subscription_name}>
                          {n.subscription_name}
                        </div>
                      ) : null}
                    </td>
                    <td>
                      <code>{n.protocol}</code>
                      {delegatedProtocols.has(n.protocol) ? (
                        <span className="pill sidecar-tag">Xray</span>
                      ) : null}
                    </td>
                    <td>{n.server}</td>
                    <td>{n.port}</td>
                    <td className="node-list-latency">
                      <LatencyDisplay
                        ms={n.latency_ms}
                        latencyAt={n.latency_at}
                        testing={isTesting}
                        unsupported={unsupportedIds.has(n.id)}
                        unsupportedLabel={pingNote}
                      />
                    </td>
                  </tr>
                );
  }

  function renderNodeCard(n: ProxyNode) {
              const active = n.id === currentId;
              const isTesting = testingIds.has(n.id);
              return (
                <button
                  key={n.id}
                  type="button"
                  className={`node-card ${active ? "active" : ""}`}
                  onClick={() => void (clickTest ? onTestOne(n.id) : onSelect(n.id))}
                  disabled={customRuntime || busyId === n.id}
                  title={
                    !customRuntime && clickTest ? t("nodes.clickTestLatency") : undefined
                  }
                >
                  <div className="node-card-top">
                    <span className="node-dot">{active ? "●" : "○"}</span>
                    <div className="node-card-meta">
                      <code>{n.protocol}</code>
                      {delegatedProtocols.has(n.protocol) ? (
                        <span className="pill sidecar-tag">Xray</span>
                      ) : null}
                    </div>
                  </div>
                  <div className="node-card-name" title={n.name}>
                    {n.name}
                  </div>
                  <div className="node-card-footer">
                    <span className="node-sub-label" title={n.subscription_name ?? ""}>
                      {n.subscription_name}
                    </span>
                    <span className="node-card-latency">
                      <LatencyDisplay
                        ms={n.latency_ms}
                        latencyAt={n.latency_at}
                        testing={isTesting}
                        unsupported={unsupportedIds.has(n.id)}
                        unsupportedLabel={pingNote}
                      />
                    </span>
                  </div>
                </button>
              );
  }

  return (

    <div className="page nodes-page">
      {customRuntime && (
        <div className="banner" role="status">
          {t("nodes.customReadOnly")}
        </div>
      )}
      <header className="page-header">
        <div>
          <h1>{t("nodes.title")}</h1>
          <p className="page-desc">
            {t("nodes.desc")}
            {" · "}
            <span className="mono">
              {query.trim()
                ? t("nodes.countFiltered", {
                    shown: displayed.length,
                    total,
                  })
                : t("nodes.count", { n: total })}
            </span>
          </p>
        </div>
        <div className="header-actions nodes-toolbar">
          <input
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            className="search"
            placeholder={t("nodes.search")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />

          <GlassSeg
            value={sortMode}
            ariaLabel="sort"
            onChange={(v) => setSortMode(v as SortMode)}
            options={[
              { value: "default", label: t("nodes.sortDefault") },
              { value: "name", label: t("nodes.sortName") },
              { value: "latency", label: t("nodes.sortLatency") },
            ]}
          />

          {/* Monochrome text glyphs (same family as ↻ / + elsewhere) — they
              follow the button color instead of rendering as color emoji. */}
          <GlassButton
            icon="◉"
            disabled={testing || displayed.length === 0}
            onClick={() => void onTest("real")}
            title={t("nodes.testRealLatencyHint")}
          >
            {testing && testKind === "real" ? t("nodes.testing") : t("nodes.testRealLatency")}
          </GlassButton>
          {/* Hidden in custom mode — there both probes take the same
              direct-TCP path (extracted nodes have no kernel mapping). */}
          {!customRuntime && (
            <GlassButton
              icon="∿"
              disabled={testing || displayed.length === 0}
              onClick={() => void onTest("ping")}
              title={t("nodes.pingTestHint")}
            >
              {testing && testKind === "ping" ? t("nodes.pinging") : t("nodes.pingTest")}
            </GlassButton>
          )}
          {/* 单点测试 toggle: state reads from the LED dot alone — gray
              while off, green while armed (same LED language as the logs
              page kernel tabs). Label stays constant in both states.
              Meaningless in custom mode (rows are not clickable there) —
              hidden with ping. */}
          {!customRuntime && (
            <GlassButton
              icon={
                <span
                  className={`seg-dot${clickTest ? " on" : ""}`}
                  aria-hidden
                />
              }
              onClick={() => setClickTest((v) => !v)}
              title={t("nodes.clickTestHint")}
            >
              {t("nodes.clickTest")}
            </GlassButton>
          )}

          {/* Grouping + view segs glue together on one wrapped row. */}
          <div className="nodes-view-segs">
            <GlassSeg
              value={groupBy}
              ariaLabel={t("nodes.groupBy")}
              onChange={(v) => setGroupBy(v as GroupBy)}
              options={[
                { value: "none", label: t("nodes.groupDefault") },
                { value: "sub", label: t("nodes.groupSub") },
                { value: "proto", label: t("nodes.groupProto") },
                { value: "country", label: t("nodes.groupCountry") },
              ]}
            />
            <GlassSeg
              value={viewMode}
              ariaLabel="视图"
              onChange={(v) => setViewMode(v as ViewMode)}
              options={[
                { value: "list", label: "列表" },
                { value: "grid", label: "网格" },
              ]}
            />
          </div>
        </div>
      </header>

      {error && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      {switching && (
        <div className="banner busy" role="status">
          <span className="lat-spinner" aria-hidden />
          {t("nodes.switchingManual")}
        </div>
      )}

      {loading ? (
        <div className="empty">{t("common.loading")}</div>
      ) : displayed.length === 0 ? (
        <div className="empty card muted">
          {nodes.length === 0
            ? customRuntime
              ? t("nodes.customEmpty")
              : t("nodes.empty")
            : "—"}
        </div>
      ) : viewMode === "list" ? (
        <div className={`card table-wrap${clickTest ? " spot-armed" : ""}`}>
          <table>
            <thead>
              <tr>
                <th style={{ width: 40 }}></th>
                <th>{t("nodes.sortName")}</th>
                <th>proto</th>
                <th>host</th>
                <th>port</th>
                <th style={{ width: 90 }}>{t("nodes.sortLatency")}</th>
              </tr>
            </thead>
            <tbody ref={listPx.containerRef as React.RefObject<HTMLTableSectionElement>}>
              {listWin.top > 0 && (
                <tr className="node-virtual-spacer" aria-hidden="true">
                  <td colSpan={6} style={{ height: listWin.top }} />
                </tr>
              )}
              {listItems
                .slice(listWin.first, listWin.last)
                .map((item) =>
                  item.type === "group" ? (
                    renderGroupRow(item)
                  ) : (
                    renderNodeRow(item.n)
                  ),
                )}
              {listWin.bottom < (listOffsets[listOffsets.length - 1] ?? 0) && (
                <tr className="node-virtual-spacer" aria-hidden="true">
                  <td colSpan={6} style={{ height: listWin.bottomPad }} />
                </tr>
              )}
            </tbody>
          </table>
        </div>
      ) : (
        <div
          className={virtualized ? "node-grid-window" : undefined}
          ref={gridPx.containerRef as React.RefObject<HTMLDivElement>}
        >
          {gridWin.top > 0 && (
            <div style={{ height: gridWin.top }} aria-hidden="true" />
          )}
          <div
            className={`node-grid ${virtualized ? "node-grid-virtual" : ""}${clickTest ? " spot-armed" : ""}`}
          >
            {gridItems
              .slice(gridWin.first, gridWin.last)
              .map((item) =>
                item.type === "group"
                  ? renderGroupHead(item)
                  : renderNodeCard(item.n),
              )}
          </div>
          {gridWin.bottom < (gridOffsets[gridOffsets.length - 1] ?? 0) && (
            <div style={{ height: gridWin.bottomPad }} aria-hidden="true" />
          )}
        </div>
      )}
      {!loading && nodes.length < total && (
        <div style={{ display: "flex", justifyContent: "center", padding: 12 }}>
          <GlassButton disabled={loadingMore} onClick={() => void reload(true)}>
            {loadingMore ? t("common.loading") : `加载更多（${nodes.length}/${total}）`}
          </GlassButton>
        </div>
      )}
    </div>
  );
}
