import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ReactFlow,
  ReactFlowProvider,
  Background,
  Controls,
  Handle,
  MarkerType,
  Position,
  useEdgesState,
  useNodesInitialized,
  useNodesState,
  useReactFlow,
  addEdge,
  type Connection,
  type Edge,
  type Node as FlowNode,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import {
  createChain,
  createPool,
  deleteChain,
  deletePool,
  diagnoseChain,
  listAllNodes,
  listChainUsage,
  listChains,
  listPools,
  updateChain,
  updatePool,
} from "../api";
import { GlassButton } from "../components/GlassButton";
import { GlassSeg } from "../components/GlassSeg";
import { ErrorModal } from "../components/ErrorModal";
import { useI18n } from "../i18n";
import type {
  ChainDiagnosis,
  ChainHop,
  NodePool,
  PoolMode,
  ProxyChain,
  ProxyNode,
} from "../types";

/** Display label for one hop, resolved against the current node/pool lists
 *  (plain name — pool-ness is conveyed by the caller's glyph). */
function hopLabel(
  hop: ChainHop,
  nodeById: Map<string, ProxyNode>,
  poolById: Map<string, NodePool>,
): { text: string; stale: boolean } {
  if (hop.kind === "node") {
    const n = nodeById.get(hop.node_id);
    return n ? { text: n.name, stale: false } : { text: hop.node_id, stale: true };
  }
  const p = poolById.get(hop.pool_id);
  return p ? { text: p.name, stale: false } : { text: hop.pool_id, stale: true };
}

/** Client-side mirror of a keyword pool's include/exclude name filter —
 *  display-only (dot count preview); the backend re-evaluates at build time. */
function poolKeywordMatch(name: string, include: string[], exclude: string[]): boolean {
  const n = name.toLowerCase();
  if (include.length && !include.some((k) => n.includes(k.toLowerCase()))) return false;
  return !exclude.some((k) => n.includes(k.toLowerCase()));
}

/** Whitespace-separated keyword list → trimmed non-empty tokens. */
function parseKeywords(raw: string): string[] {
  return raw
    .split(/\s+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** Vector glyph marking node pools (layers/stack = a reusable collection).
 *  Replaces the 📦 emoji: emoji fonts vary per platform and can't be themed;
 *  this renders identically everywhere and follows color via currentColor. */
function PoolGlyph({ size = 12 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M8 1.8 14 5 8 8.2 2 5Z" />
      <path d="m2 8.2 6 3.2 6-3.2" />
      <path d="m2 11.4 6 3.2 6-3.2" />
    </svg>
  );
}

const MAX_POOL_DOTS = 12;

/** Rich card payload for a flow node (network-console style): single nodes are
 *  compact entity cards (protocol + last-known latency), pools are bigger
 *  aggregate cards (member dot grid + count). Computed once when the hop is
 *  placed on the canvas; order badges are merged in live by the editor. */
function hopCardData(
  hop: ChainHop,
  nodeById: Map<string, ProxyNode>,
  poolById: Map<string, NodePool>,
): Record<string, unknown> {
  if (hop.kind === "node") {
    const n = nodeById.get(hop.node_id);
    return n
      ? {
          kind: "node",
          label: n.name,
          stale: false,
          protocol: n.protocol,
          latencyMs: n.latency_ms ?? null,
        }
      : { kind: "node", label: hop.node_id, stale: true };
  }
  const p = poolById.get(hop.pool_id);
  if (!p) return { kind: "pool", label: hop.pool_id, stale: true };
  if (p.mode.mode === "explicit") {
    const ids = p.mode.node_ids;
    return {
      kind: "pool",
      label: p.name,
      stale: false,
      poolMode: "explicit" as const,
      memberCount: ids.length,
      dots: ids.slice(0, MAX_POOL_DOTS).map((id) => nodeById.has(id)),
      overflow: Math.max(0, ids.length - MAX_POOL_DOTS),
    };
  }
  const { include, exclude } = p.mode;
  const matched = Array.from(nodeById.values()).filter((n) =>
    poolKeywordMatch(n.name, include, exclude),
  );
  return {
    kind: "pool",
    label: p.name,
    stale: false,
    poolMode: "keyword" as const,
    memberCount: matched.length,
    dots: matched.slice(0, MAX_POOL_DOTS).map(() => true),
    overflow: Math.max(0, matched.length - MAX_POOL_DOTS),
    include,
    exclude,
  };
}

/** Custom flow nodes, network-console style. Single nodes are compact entity
 *  cards (name · protocol · last-known latency); pools are one-size-bigger
 *  aggregate cards (member dot grid, count, mode subtitle). The 1px topline
 *  marks the chain role (green = entry/client side, blue = exit/internet
 *  side). Handles stay hidden until hover/connection drag (see App.css).
 *  `data.index`/`data.total` carry the live graph-derived order (null while
 *  the graph isn't a valid single path). */
function HopNode({ data }: NodeProps) {
  const { t } = useI18n();
  const label = data.label as string;
  const stale = data.stale as boolean;
  const index = (data.index as number | null | undefined) ?? null;
  const total = (data.total as number | null | undefined) ?? null;
  const kind = (data.kind as "node" | "pool" | undefined) ?? "node";
  const role =
    index == null || total == null || total < 2
      ? null
      : index === 1
        ? "entry"
        : index === total
          ? "exit"
          : null;

  let sub: React.ReactNode;
  if (stale) {
    sub = <span className="chain-hop-sub">{t("rules.nodeStale")}</span>;
  } else if (kind === "pool") {
    const poolMode = data.poolMode as "explicit" | "keyword" | undefined;
    const memberCount = (data.memberCount as number | undefined) ?? 0;
    const include = (data.include as string[] | undefined) ?? [];
    const exclude = (data.exclude as string[] | undefined) ?? [];
    const kws = [...include.map((k) => `+${k}`), ...exclude.map((k) => `−${k}`)].join(" ");
    const modeText =
      poolMode === "keyword"
        ? kws || t("chain.poolKeywordAll")
        : t("chain.poolModeExplicit");
    sub = (
      <span className="chain-hop-sub">
        {modeText} · {memberCount} {t("chain.poolMembersSuffix")}
      </span>
    );
  } else {
    const protocol = data.protocol as string | undefined;
    const latencyMs = data.latencyMs as number | null | undefined;
    sub = (
      <span className="chain-hop-sub">
        {protocol ? protocol.toUpperCase() : ""}
        {latencyMs != null && latencyMs > 0 && (
          <span className="chain-hop-latency">{latencyMs} ms</span>
        )}
      </span>
    );
  }

  const dots = (data.dots as boolean[] | undefined) ?? [];
  const overflow = (data.overflow as number | undefined) ?? 0;

  return (
    <div
      className={`chain-hop-node ${kind === "pool" ? "pool-card" : "node-card"}${
        stale ? " stale" : ""
      }${role ? ` ${role}` : ""}`}
    >
      <Handle type="target" position={Position.Left} isConnectableStart={false} />
      <div className="chain-hop-head">
        {kind === "pool" && (
          <span className="chain-hop-glyph" aria-hidden="true">
            <PoolGlyph />
          </span>
        )}
        <span className="chain-hop-name" title={label}>
          {label}
        </span>
        {index != null && (
          <span
            className="chain-hop-badge"
            title={
              role === "entry"
                ? t("chain.entryHop")
                : role === "exit"
                  ? t("chain.exitHop")
                  : undefined
            }
          >
            {index}
          </span>
        )}
      </div>
      {kind === "pool" && !stale && dots.length > 0 && (
        <div className="chain-hop-members" aria-hidden="true">
          {dots.map((ok, i) => (
            <span key={i} className={`chain-hop-dot${ok ? "" : " miss"}`} />
          ))}
          {overflow > 0 && <span className="chain-hop-overflow">+{overflow}</span>}
        </div>
      )}
      {sub}
      <Handle type="source" position={Position.Right} isConnectableEnd={false} />
    </div>
  );
}

const nodeTypes = { hop: HopNode };

/** Walk the graph from whichever node has no incoming edge and return hops
 *  in order. Returns `null` if the graph isn't a single simple path (a
 *  branch, a cycle, an isolated node, or more than one component) — chains
 *  only support straight-line hops. */
function hopsFromGraph(
  flowNodes: FlowNode[],
  edges: Edge[],
): { id: string; hop: ChainHop }[] | null {
  if (flowNodes.length === 0) return [];
  const outgoing = new Map<string, string[]>();
  const incoming = new Map<string, number>();
  for (const n of flowNodes) {
    outgoing.set(n.id, []);
    incoming.set(n.id, 0);
  }
  for (const e of edges) {
    if (!outgoing.has(e.source) || !incoming.has(e.target)) continue;
    outgoing.get(e.source)!.push(e.target);
    incoming.set(e.target, (incoming.get(e.target) ?? 0) + 1);
  }
  // Every node must have out-degree <= 1 and in-degree <= 1 for a simple path.
  for (const n of flowNodes) {
    if ((outgoing.get(n.id)?.length ?? 0) > 1) return null;
    if ((incoming.get(n.id) ?? 0) > 1) return null;
  }
  const starts = flowNodes.filter((n) => (incoming.get(n.id) ?? 0) === 0);
  if (starts.length !== 1) return null; // no unique entry, or a cycle with no start
  const order: FlowNode[] = [];
  let cur: FlowNode | undefined = starts[0];
  const seen = new Set<string>();
  while (cur) {
    if (seen.has(cur.id)) return null; // cycle
    seen.add(cur.id);
    order.push(cur);
    const nextId: string | undefined = outgoing.get(cur.id)?.[0];
    cur = nextId ? flowNodes.find((n) => n.id === nextId) : undefined;
  }
  if (order.length !== flowNodes.length) return null; // disconnected node(s)
  return order.map((n) => ({
    id: n.id,
    hop: n.data.hop as ChainHop,
  }));
}

function hopFlowId(hop: ChainHop): string {
  return hop.kind === "node" ? `node:${hop.node_id}` : `pool:${hop.pool_id}`;
}

/** Lay hops out left-to-right at a fixed spacing (used both for loading an
 *  existing chain and for a freshly-dropped candidate). Spacing covers the
 *  widest card (pool, 240px) plus connection breathing room. */
function layoutHops(
  hops: ChainHop[],
  nodeById: Map<string, ProxyNode>,
  poolById: Map<string, NodePool>,
): FlowNode[] {
  return hops.map((hop, i) => ({
    id: hopFlowId(hop),
    type: "hop",
    position: { x: 40 + i * 320, y: 60 },
    data: { ...hopCardData(hop, nodeById, poolById), hop },
  }));
}

/** Edge between two hops: bezier curve + animated dashes + closed arrow at
 *  the target — direction (client → internet) reads at a glance. Arrow color
 *  is themed via CSS (`.react-flow__arrowhead` override), since marker colors
 *  are baked as inline styles by xyflow. */
function hopEdge(sourceId: string, targetId: string): Edge {
  return {
    id: `${sourceId}->${targetId}`,
    source: sourceId,
    target: targetId,
    type: "default",
    animated: true,
    markerEnd: { type: MarkerType.ArrowClosed, strokeWidth: 2 },
  };
}

function edgesFromHops(hops: ChainHop[]): Edge[] {
  const out: Edge[] = [];
  for (let i = 0; i < hops.length - 1; i++) {
    out.push(hopEdge(hopFlowId(hops[i]), hopFlowId(hops[i + 1])));
  }
  return out;
}

/** One entry in a RowMenu dropdown. */
type RowMenuItem = {
  key: string;
  label: string;
  danger?: boolean;
  onClick: () => void;
};

/** Compact "⋮" overflow menu for pool rows and chain cards — the same idiom
    as the subscription card menu: opaque popover (never glass), capture-phase
    outside click via [data-chain-menu], Escape to close. The open id is owned
    by ChainPage (one menuId for every ⋮ on the page), so opening one menu
    closes the previous — per-instance state used to leave both open. Rows in
    the lower half of a list pass flipUp so the popup opens toward the
    roomier side. */
function RowMenu({
  id,
  openId,
  setOpenId,
  items,
  flipUp = false,
}: {
  id: string;
  openId: string | null;
  setOpenId: (id: string | null) => void;
  items: RowMenuItem[];
  flipUp?: boolean;
}) {
  const { t } = useI18n();
  const open = openId === id;

  return (
    <div className="rule-menu chain-menu" data-chain-menu>
      <button
        type="button"
        className="rule-menu-trigger"
        aria-label={t("common.actions")}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpenId(open ? null : id)}
      >
        ⋮
      </button>
      {open && (
        <div className={`rule-menu-pop chain-menu-pop${flipUp ? " up" : ""}`} role="menu">
          {items.map((it) => (
            <button
              key={it.key}
              type="button"
              role="menuitem"
              className={`rule-menu-item${it.danger ? " danger" : ""}`}
              onClick={() => {
                setOpenId(null);
                it.onClick();
              }}
            >
              {it.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function PoolRow({
  pool,
  nodes,
  usedByChains,
  flipUp,
  openMenuId,
  setOpenMenuId,
  onEdit,
  onDelete,
}: {
  pool: NodePool;
  nodes: ProxyNode[];
  /** How many chains route through this pool — deletion-risk hint. */
  usedByChains: number;
  /** Lower-half rows open the ⋮ menu upward (no room below the list). */
  flipUp: boolean;
  openMenuId: string | null;
  setOpenMenuId: (id: string | null) => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const { t } = useI18n();
  let memberCount: number;
  let keywords: string | null;
  if (pool.mode.mode === "explicit") {
    memberCount = pool.mode.node_ids.length;
    keywords = null;
  } else {
    const { include, exclude } = pool.mode;
    memberCount = nodes.filter((n) => poolKeywordMatch(n.name, include, exclude)).length;
    keywords = [
      ...include.map((k) => `+${k}`),
      ...exclude.map((k) => `−${k}`),
    ].join(" ");
  }
  return (
    <div className="chain-pool-row" title={keywords ?? undefined}>
      <span className="chain-pool-name">
        <span className="chain-pool-glyph" aria-hidden="true">
          <PoolGlyph />
        </span>
        {pool.name}
      </span>
      {keywords && <span className="chain-pool-row-kw">{keywords}</span>}
      <span className={`pill ${pool.mode.mode === "explicit" ? "target-node" : "target-smart"}`}>
        {pool.mode.mode === "explicit" ? t("chain.poolModeExplicit") : t("chain.poolModeKeyword")}
      </span>
      <span className="chain-row-meta">
        {memberCount} {t("chain.poolMembersSuffix")}
      </span>
      {usedByChains > 0 && (
        <span className="chain-row-meta chain-card-usage">
          {t("chain.usedByChains", { n: usedByChains })}
        </span>
      )}
      <RowMenu
        id={`pool-${pool.id}`}
        openId={openMenuId}
        setOpenId={setOpenMenuId}
        flipUp={flipUp}
        items={[
          { key: "edit", label: t("common.edit"), onClick: onEdit },
          { key: "delete", label: t("common.delete"), danger: true, onClick: onDelete },
        ]}
      />
    </div>
  );
}

function PoolEditorModal({
  pool,
  nodes,
  onClose,
  onSaved,
}: {
  /** `null` = creating a new pool. */
  pool: NodePool | null;
  nodes: ProxyNode[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState(pool?.name ?? "");
  const [mode, setMode] = useState<"explicit" | "keyword">(pool?.mode.mode ?? "explicit");
  const [nodeIds, setNodeIds] = useState<Set<string>>(
    new Set(pool?.mode.mode === "explicit" ? pool.mode.node_ids : []),
  );
  const [include, setInclude] = useState(
    pool?.mode.mode === "keyword" ? pool.mode.include.join(" ") : "",
  );
  const [exclude, setExclude] = useState(
    pool?.mode.mode === "keyword" ? pool.mode.exclude.join(" ") : "",
  );
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const filteredNodes = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return nodes;
    return nodes.filter((n) => n.name.toLowerCase().includes(q));
  }, [nodes, query]);

  function toggleNode(id: string) {
    setNodeIds((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  // Live keyword-filter preview: same parsing + matching as submit, so the
  // count/names shown while typing are exactly what the pool will contain.
  const matchedNodes = useMemo(
    () => nodes.filter((n) => poolKeywordMatch(n.name, parseKeywords(include), parseKeywords(exclude))),
    [nodes, include, exclude],
  );

  async function onSubmit() {
    const trimmed = name.trim();
    if (!trimmed) {
      setError(t("chain.needPoolName"));
      return;
    }
    const poolMode: PoolMode =
      mode === "explicit"
        ? { mode: "explicit", node_ids: Array.from(nodeIds) }
        : { mode: "keyword", include: parseKeywords(include), exclude: parseKeywords(exclude) };
    if (poolMode.mode === "explicit" && poolMode.node_ids.length === 0) {
      setError(t("chain.needPoolNodes"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (pool) {
        await updatePool(pool.id, trimmed, poolMode);
      } else {
        await createPool(trimmed, poolMode);
      }
      onSaved();
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop">
      <div className="modal rules-form-modal">
        <header className="modal-header">
          <h2>{pool ? t("chain.editPool") : t("chain.newPool")}</h2>
          <button type="button" className="icon-btn" onClick={onClose}>
            ×
          </button>
        </header>
        <div className="modal-body">
          <label className="field">
            <span>{t("chain.poolName")}</span>
            <input
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("chain.poolNamePh")}
              maxLength={64}
              autoFocus
            />
          </label>
          <label className="field">
            <span>{t("chain.poolMode")}</span>
            <GlassSeg
              value={mode}
              ariaLabel={t("chain.poolMode")}
              onChange={(v) => setMode(v as "explicit" | "keyword")}
              options={[
                { value: "explicit", label: t("chain.poolModeExplicit") },
                { value: "keyword", label: t("chain.poolModeKeyword") },
              ]}
            />
          </label>
          {mode === "explicit" ? (
            <div className="field rule-node-pick">
              <span>{t("chain.poolPickNodes")}</span>
              {nodes.length === 0 ? (
                <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                  {t("rules.noNodes")}
                </p>
              ) : (
                <>
                  <input
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    className="search"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    placeholder={t("rules.pickNodePh")}
                  />
                  <div className="chain-pool-node-list">
                    {filteredNodes.map((n) => (
                      <label key={n.id} className="chain-pool-node-item">
                        <input
                          type="checkbox"
                          checked={nodeIds.has(n.id)}
                          onChange={() => toggleNode(n.id)}
                        />
                        <span>{n.name}</span>
                      </label>
                    ))}
                  </div>
                  <p className="muted" style={{ margin: "6px 0 0", fontSize: 12 }}>
                    {t("chain.poolSelectedCount", { n: nodeIds.size })}
                  </p>
                </>
              )}
            </div>
          ) : (
            <div className="field rule-smart-filters">
              <label className="field" style={{ marginBottom: 8 }}>
                <span>{t("rules.smartInclude")}</span>
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={include}
                  onChange={(e) => setInclude(e.target.value)}
                  placeholder={t("rules.smartIncludePh")}
                />
              </label>
              <label className="field">
                <span>{t("rules.smartExclude")}</span>
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={exclude}
                  onChange={(e) => setExclude(e.target.value)}
                  placeholder={t("rules.smartExcludePh")}
                />
              </label>
              {/* Live match feedback while typing keywords. */}
              <div className="chain-pool-match">
                <span className="chain-pool-match-count">
                  {t("chain.matchedNodes", { n: matchedNodes.length })}
                  {parseKeywords(include).length === 0 &&
                    ` · ${t("chain.poolKeywordAll")}`}
                </span>
                {matchedNodes.length > 0 && (
                  <span
                    className="chain-pool-match-names"
                    title={matchedNodes.map((n) => n.name).join(" · ")}
                  >
                    {matchedNodes
                      .slice(0, 6)
                      .map((n) => n.name)
                      .join(" · ") + (matchedNodes.length > 6 ? " …" : "")}
                  </span>
                )}
              </div>
            </div>
          )}
        </div>
        <footer className="modal-footer">
          <GlassButton onClick={onClose}>{t("common.cancel")}</GlassButton>
          <GlassButton variant="primary" disabled={busy} onClick={() => void onSubmit()}>
            {t("common.save")}
          </GlassButton>
        </footer>
      </div>
      {error && <ErrorModal message={error} onClose={() => setError(null)} />}
    </div>
  );
}

function ChainFlowEditor({
  chain,
  nodes,
  pools,
  nodeById,
  poolById,
  onSubmit,
  busy,
  onClose,
  autoName,
}: {
  chain: ProxyChain | null;
  nodes: ProxyNode[];
  pools: NodePool[];
  nodeById: Map<string, ProxyNode>;
  poolById: Map<string, NodePool>;
  onSubmit: (flowNodes: FlowNode[], edges: Edge[]) => void;
  busy: boolean;
  onClose: () => void;
  /** New-chain mode: receives "A → B → C" derived from the live graph order,
   *  so the name field can auto-follow the hops being placed. */
  autoName?: (label: string) => void;
}) {
  const { t } = useI18n();
  const { screenToFlowPosition, fitView, getZoom, getViewport, setCenter } = useReactFlow();
  const canvasRef = useRef<HTMLDivElement>(null);

  const [flowNodes, setFlowNodes, onNodesChange] = useNodesState<FlowNode>(
    layoutHops(chain?.hops ?? [], nodeById, poolById),
  );
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>(
    edgesFromHops(chain?.hops ?? []),
  );
  const [candidateKind, setCandidateKind] = useState<"node" | "pool">("node");
  // Quick filter over the sidebar candidates — substring match on the name,
  // same convention as the pool editor's node picker.
  const [candidateQuery, setCandidateQuery] = useState("");
  const candidateNodes = useMemo(() => {
    const q = candidateQuery.trim().toLowerCase();
    if (!q) return nodes;
    return nodes.filter((n) => n.name.toLowerCase().includes(q));
  }, [nodes, candidateQuery]);
  const candidatePools = useMemo(() => {
    const q = candidateQuery.trim().toLowerCase();
    if (!q) return pools;
    return pools.filter((p) => p.name.toLowerCase().includes(q));
  }, [pools, candidateQuery]);
  // Manual pointer-based drag: WKWebView (macOS Tauri) doesn't reliably fire
  // HTML5 drag-and-drop events (dragstart/drop over DataTransfer), so the
  // sidebar → canvas drag is implemented with pointer events instead — a
  // "ghost" label follows the cursor, and dropping is resolved by hit-testing
  // the canvas element under the pointer on release.
  const [dragging, setDragging] = useState<
    {
      hop: ChainHop;
      label: string;
      x: number;
      y: number;
      sx: number;
      sy: number;
      isPool: boolean;
    } | null
  >(null);
  const draggingRef = useRef(dragging);
  draggingRef.current = dragging;

  // Current graph-derived hop order (null = not a single simple path), used
  // for the live validity status and the numbered badges on each node.
  const graphOrder = useMemo(() => hopsFromGraph(flowNodes, edges), [flowNodes, edges]);

  // "A → B → C" from the live graph order — feeds the new-chain auto-name.
  // Computed as a string so the effect below only fires when it actually
  // changes (position-only drags don't re-trigger it).
  const autoNameLabel = useMemo(() => {
    if (!graphOrder) return null;
    return graphOrder
      .map((o) => String(flowNodes.find((n) => n.id === o.id)?.data.label ?? ""))
      .join(" → ");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [graphOrder]);

  useEffect(() => {
    if (autoName && autoNameLabel != null) autoName(autoNameLabel);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoNameLabel]);

  // Fit exactly once, and only when the editor was opened with saved hops.
  // Fitting on node-count changes (the old behavior) panned/zoomed the
  // viewport away from the cursor right after a drop, so the new node looked
  // like it hadn't stayed where it was released. Waiting for
  // `useNodesInitialized` matters too: node dimensions are measured
  // asynchronously (ResizeObserver), and fitting against unmeasured 0×0
  // nodes computes a broken viewport that leaves the canvas looking empty.
  // The canvas-size re-check guards against the flex-sized modal settling
  // after the nodes were already measured.
  const openedWithHopsRef = useRef((chain?.hops?.length ?? 0) > 0);
  const nodesInitialized = useNodesInitialized();
  const didFitRef = useRef(false);
  useEffect(() => {
    if (didFitRef.current || !openedWithHopsRef.current) return;
    const el = canvasRef.current;
    if (!el) return;
    const tryFit = () => {
      if (didFitRef.current || !nodesInitialized) return;
      if (el.clientWidth < 2 || el.clientHeight < 2) return;
      didFitRef.current = true;
      fitView({ padding: 0.3, duration: 250, maxZoom: 1.1 });
    };
    tryFit();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(tryFit);
    ro.observe(el);
    return () => ro.disconnect();
  }, [nodesInitialized, fitView]);

  // Mirror the hop order into node data (badge number / entry-exit accent).
  // The merge is diffed so position-only changes (dragging nodes around)
  // don't loop through setFlowNodes.
  useEffect(() => {
    setFlowNodes((cur) => {
      let changed = false;
      const next = cur.map((n) => {
        const idx = graphOrder ? graphOrder.findIndex((o) => o.id === n.id) : -1;
        const index = idx === -1 ? null : idx + 1;
        const total = graphOrder ? graphOrder.length : null;
        if (n.data.index === index && n.data.total === total) return n;
        changed = true;
        return { ...n, data: { ...n.data, index, total } };
      });
      return changed ? next : cur;
    });
  }, [graphOrder]);

  // Linear-chain invariant, enforced while a connection is being dragged:
  // no self/parallel edges, at most one outgoing edge per hop and one
  // incoming edge per hop, and nothing that would close a cycle.
  // (`hopsFromGraph` at save time remains the backstop.)
  const isValidConnection = useCallback(
    (conn: Connection | Edge): boolean => {
      const { source, target } = conn;
      if (!source || !target || source === target) return false;
      if (edges.some((e) => e.source === source && e.target === target)) return false;
      if (edges.some((e) => e.source === source && e.target !== target)) return false;
      if (edges.some((e) => e.target === target && e.source !== source)) return false;
      const nextOf = new Map<string, string>();
      for (const e of edges) if (e.source !== source) nextOf.set(e.source, e.target);
      nextOf.set(source, target);
      let cur: string | undefined = target;
      const seen = new Set<string>();
      while (cur && !seen.has(cur)) {
        if (cur === source) return false; // target can already reach source → cycle
        seen.add(cur);
        cur = nextOf.get(cur);
      }
      return true;
    },
    [edges],
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      if (!isValidConnection(connection)) return;
      setEdges((eds) => addEdge(connection, eds));
    },
    [isValidConnection, setEdges],
  );

  function selectOnly(id: string) {
    setFlowNodes((cur) => cur.map((n) => ({ ...n, selected: n.id === id })));
    setEdges((cur) => cur.map((e) => ({ ...e, selected: false })));
  }

  /** Pan the viewport (zoom unchanged) just enough to bring a hop into view.
   *  Appending places the new hop to the right of the chain tail, which is
   *  regularly outside the current viewport on longer chains — without this
   *  the add looks like a no-op until "tidy layout" re-fits. */
  function revealPosition(position: { x: number; y: number }) {
    const el = canvasRef.current;
    const vp = getViewport();
    if (el) {
      const left = -vp.x / vp.zoom;
      const top = -vp.y / vp.zoom;
      const right = left + el.clientWidth / vp.zoom;
      const bottom = top + el.clientHeight / vp.zoom;
      // Card is ~260px wide worst case (pool) × ~100 tall; keep a margin so
      // it doesn't hug the edge.
      const m = 48;
      const visible =
        position.x - m >= left &&
        position.x + 260 + m <= right &&
        position.y - m >= top &&
        position.y + 100 + m <= bottom;
      if (visible) return;
    }
    setCenter(position.x + 130, position.y + 50, { zoom: vp.zoom, duration: 250 });
  }

  function dropHopAt(hop: ChainHop, clientX: number, clientY: number) {
    const id = hopFlowId(hop);
    if (flowNodes.some((n) => n.id === id)) {
      const existing = flowNodes.find((n) => n.id === id)!;
      selectOnly(id); // already on the canvas — surface it instead of a silent no-op
      revealPosition(existing.position);
      return;
    }
    // Center the card under the cursor (css-size/2 scaled back into flow
    // coordinates) so the node visually lands exactly where it was dropped.
    const p = screenToFlowPosition({ x: clientX, y: clientY });
    const zoom = getZoom();
    const position = { x: p.x - 95 / zoom, y: p.y - 30 / zoom };
    setFlowNodes((cur) => [
      ...cur,
      { id, type: "hop", position, data: { ...hopCardData(hop, nodeById, poolById), hop } },
    ]);
  }

  /** Click (as opposed to drag) on a sidebar candidate: append the hop after
   *  the current chain tail, auto-connecting it so a chain can be built with
   *  keyboard-free clicks alone. */
  function appendHop(hop: ChainHop) {
    const id = hopFlowId(hop);
    if (flowNodes.some((n) => n.id === id)) {
      const existing = flowNodes.find((n) => n.id === id)!;
      selectOnly(id);
      revealPosition(existing.position);
      return;
    }
    const card = hopCardData(hop, nodeById, poolById);
    const path = hopsFromGraph(flowNodes, edges);
    const tailId = path && path.length > 0 ? path[path.length - 1].id : null;
    const anchor = tailId ? flowNodes.find((n) => n.id === tailId) : undefined;
    let position: { x: number; y: number };
    if (anchor) {
      position = { x: anchor.position.x + 320, y: anchor.position.y };
    } else if (flowNodes.length > 0) {
      // Not a valid path right now — still append visibly to the right.
      const maxX = Math.max(...flowNodes.map((n) => n.position.x));
      position = { x: maxX + 320, y: 60 };
    } else {
      const rect = canvasRef.current?.getBoundingClientRect();
      position = rect
        ? screenToFlowPosition({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 })
        : { x: 60, y: 60 };
    }
    setFlowNodes((cur) => [
      ...cur.map((n) => ({ ...n, selected: false })),
      { id, type: "hop", position, data: { ...card, hop }, selected: true },
    ]);
    if (tailId && tailId !== id) {
      const edge = hopEdge(tailId, id);
      setEdges((cur) => (cur.some((e) => e.id === edge.id) ? cur : [...cur, edge]));
    }
    // Bring the freshly-appended hop into view (see revealPosition).
    revealPosition(position);
  }

  /** Re-lay the nodes into a clean left-to-right line (in graph order when
   *  the chain is valid, otherwise by current x position) and re-fit. */
  function tidyLayout() {
    const orderedIds =
      graphOrder && graphOrder.length === flowNodes.length
        ? graphOrder.map((o) => o.id)
        : [...flowNodes]
            .sort((a, b) => a.position.x - b.position.x || a.position.y - b.position.y)
            .map((n) => n.id);
    const rank = new Map(orderedIds.map((id, i) => [id, i] as const));
    setFlowNodes((cur) =>
      cur.map((n) => {
        const i = rank.get(n.id) ?? 0;
        return { ...n, position: { x: 40 + i * 320, y: 60 } };
      }),
    );
    requestAnimationFrame(() => fitView({ padding: 0.3, duration: 250, maxZoom: 1.1 }));
  }

  useEffect(() => {
    if (!dragging) return;

    function onMove(e: PointerEvent) {
      setDragging((cur) => (cur ? { ...cur, x: e.clientX, y: e.clientY } : cur));
    }

    function onUp(e: PointerEvent) {
      const current = draggingRef.current;
      setDragging(null);
      if (!current) return;
      // Released without moving = a plain click on the sidebar item.
      const moved = Math.hypot(e.clientX - current.sx, e.clientY - current.sy);
      if (moved < 5) {
        appendHop(current.hop);
        return;
      }
      const target = document.elementFromPoint(e.clientX, e.clientY);
      const overCanvas = !!canvasRef.current && !!target && canvasRef.current.contains(target);
      if (overCanvas) {
        dropHopAt(current.hop, e.clientX, e.clientY);
      }
    }

    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setDragging(null);
    }

    function onCancel() {
      setDragging(null);
    }

    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp, { once: true });
    document.addEventListener("pointercancel", onCancel);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
      document.removeEventListener("keydown", onKey);
    };
    // appendHop/dropHopAt/draggingRef are read through refs/closures scoped
    // to this effect's own render — re-running only on dragging's presence
    // (not its x/y) avoids tearing down/re-attaching listeners on every
    // pointermove.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dragging !== null]);

  function onCandidatePointerDown(e: React.PointerEvent, hop: ChainHop, label: string) {
    if (e.button !== 0) return;
    e.preventDefault();
    setDragging({
      hop,
      label,
      x: e.clientX,
      y: e.clientY,
      sx: e.clientX,
      sy: e.clientY,
      isPool: hop.kind === "pool",
    });
  }

  function removeSelected() {
    setFlowNodes((cur) => cur.filter((n) => !n.selected));
    setEdges((cur) => cur.filter((e) => !e.selected));
  }

  return (
    <>
      <div className="modal-body chain-editor-body">
        <p className="muted chain-editor-hint" title={t("chain.editorHint")}>
          {t("chain.editorHint")}
        </p>
        <div className="chain-editor-layout">
          <aside className="chain-editor-sidebar">
            <GlassSeg
              value={candidateKind}
              ariaLabel={t("chain.candidateKind")}
              onChange={(v) => setCandidateKind(v as "node" | "pool")}
              options={[
                { value: "node", label: t("rules.pickNode") },
                { value: "pool", label: t("chain.pool") },
              ]}
            />
            <input
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              className="search chain-candidate-search"
              value={candidateQuery}
              onChange={(e) => setCandidateQuery(e.target.value)}
              placeholder={
                candidateKind === "node" ? t("rules.pickNodePh") : t("chain.searchPoolPh")
              }
            />
            <div className="chain-candidate-list">
              {candidateKind === "node"
                ? candidateNodes.map((n) => (
                    <div
                      key={n.id}
                      onPointerDown={(e) =>
                        onCandidatePointerDown(e, { kind: "node", node_id: n.id }, n.name)
                      }
                      className="chain-candidate-item"
                      title={t("chain.dragHint")}
                    >
                      {n.name}
                    </div>
                  ))
                : candidatePools.map((p) => (
                    <div
                      key={p.id}
                      onPointerDown={(e) =>
                        onCandidatePointerDown(e, { kind: "pool", pool_id: p.id }, p.name)
                      }
                      className="chain-candidate-item"
                      title={t("chain.dragHint")}
                    >
                      <span className="chain-candidate-glyph" aria-hidden="true">
                        <PoolGlyph />
                      </span>
                      {p.name}
                    </div>
                  ))}
              {candidateKind === "node" && nodes.length === 0 && (
                <p className="muted" style={{ fontSize: 12 }}>
                  {t("rules.noNodes")}
                </p>
              )}
              {candidateKind === "pool" && pools.length === 0 && (
                <p className="muted" style={{ fontSize: 12 }}>
                  {t("chain.noPools")}
                </p>
              )}
              {((candidateKind === "node" && nodes.length > 0) ||
                (candidateKind === "pool" && pools.length > 0)) &&
                (candidateKind === "node" ? candidateNodes.length : candidatePools.length) ===
                  0 && (
                  <p className="muted" style={{ fontSize: 12 }}>
                    {t("chain.noMatch")}
                  </p>
                )}
            </div>
            <div className="chain-editor-sidebar-actions">
              <GlassButton onClick={removeSelected}>{t("chain.removeSelected")}</GlassButton>
              <GlassButton onClick={tidyLayout}>{t("chain.tidyLayout")}</GlassButton>
            </div>
          </aside>
          <div className="chain-editor-canvas-col">
            <div className="chain-editor-canvas" ref={canvasRef}>
              <ReactFlow
                nodes={flowNodes}
                edges={edges}
                onNodesChange={onNodesChange}
                onEdgesChange={onEdgesChange}
                onConnect={onConnect}
                isValidConnection={isValidConnection}
                nodeTypes={nodeTypes}
                defaultViewport={{ x: 0, y: 0, zoom: 1 }}
                minZoom={0.2}
                maxZoom={2}
                deleteKeyCode={["Backspace", "Delete"]}
                panOnScroll
                zoomOnScroll={false}
                defaultEdgeOptions={{
                  type: "default",
                  animated: true,
                  markerEnd: { type: MarkerType.ArrowClosed, strokeWidth: 2 },
                }}
                proOptions={{ hideAttribution: true }}
              >
                <Background gap={20} />
                <Controls showInteractive={false} />
              </ReactFlow>
            </div>
            <p
              className={`chain-graph-status${
                graphOrder === null ? " bad" : flowNodes.length === 0 ? "" : " ok"
              }`}
            >
              {graphOrder === null
                ? t("chain.graphInvalid")
                : flowNodes.length === 0
                  ? t("chain.graphEmpty")
                  : t("chain.graphValid", { n: graphOrder.length })}
            </p>
          </div>
        </div>
      </div>
      {dragging && (
        <div className="chain-drag-ghost" style={{ left: dragging.x, top: dragging.y }}>
          {dragging.isPool && (
            <span className="chain-drag-glyph" aria-hidden="true">
              <PoolGlyph />
            </span>
          )}
          {dragging.label}
        </div>
      )}
      <footer className="modal-footer">
        <GlassButton onClick={onClose}>{t("common.cancel")}</GlassButton>
        <GlassButton
          variant="primary"
          disabled={busy}
          onClick={() => onSubmit(flowNodes, edges)}
        >
          {t("common.save")}
        </GlassButton>
      </footer>
    </>
  );
}

/** One probe cell: green ms on success, red ✗ (tooltip = raw error) on
 *  failure, em-dash when there was nothing to probe. */
function DiagCell({
  ms,
  error,
  stale,
  staleText,
}: {
  ms?: number | null;
  error?: string | null;
  stale?: boolean;
  staleText: string;
}) {
  if (stale) return <span className="chain-diag-cell bad" title={staleText}>✗ {staleText}</span>;
  if (ms != null) return <span className="chain-diag-cell ok">{ms} ms</span>;
  if (error) return <span className="chain-diag-cell bad" title={error}>✗</span>;
  return <span className="chain-diag-cell">—</span>;
}

/** Chain diagnostics: per-hop solo + chain-prefix probes through the live
 *  Clash delay API. The last hop's "chained" column IS the whole chain (the
 *  exit the internet sees). solo-ok + chained-failed localizes the break to
 *  the relay into that hop. */
function ChainDiagModal({ chain, onClose }: { chain: ProxyChain; onClose: () => void }) {
  const { t } = useI18n();
  const [diag, setDiag] = useState<ChainDiagnosis | null>(null);
  // Manual-start flow: idle until the user clicks (a stale `true` here shows
  // the spinner AND disables the start button — permanently stuck).
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setDiag(await diagnoseChain(chain.id));
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setBusy(false);
    }
  }, [chain.id]);

  // No auto-run: probing switches a selector and spends real round-trips,
  // so it starts on an explicit click (the footer's primary button).

  return (
    <div className="modal-backdrop">
      <div className="modal chain-diag-modal">
        <header className="modal-header">
          <h2>{t("chain.diagTitle", { name: chain.name })}</h2>
          <button type="button" className="icon-btn" onClick={onClose}>
            ×
          </button>
        </header>
        <div className="modal-body">
          <div className="chain-diag-table">
            <div className="chain-diag-row chain-diag-head">
              <span>{t("chain.diagHop")}</span>
              <span>{t("chain.diagSolo")}</span>
              <span>{t("chain.diagChained")}</span>
            </div>
            {busy && (
              <div className="chain-diag-loading">
                <span className="chain-diag-spinner" aria-hidden="true" />
                <span>{t("chain.diagRunning")}</span>
              </div>
            )}
            {!busy && !diag && <p className="muted chain-diag-idle">{t("chain.diagIdle")}</p>}
            {diag?.hops.map((h, i) => {
              const last = i === diag.hops.length - 1;
              return (
                <div key={i} className={`chain-diag-row${last ? " whole" : ""}`}>
                  <span className="chain-diag-hop">
                    <span className="chain-diag-order">{i + 1}</span>
                    {h.kind === "pool" && (
                      <span className="chain-candidate-glyph" aria-hidden="true">
                        <PoolGlyph />
                      </span>
                    )}
                    <span className={`chain-diag-label${h.stale ? " stale" : ""}`} title={h.label}>
                      {h.label}
                    </span>
                    {last && <span className="chain-diag-exit">{t("chain.diagExit")}</span>}
                  </span>
                  <DiagCell
                    ms={h.soloMs}
                    error={h.soloError}
                    stale={h.stale}
                    staleText={t("rules.nodeStale")}
                  />
                  <DiagCell
                    ms={h.chainedMs}
                    error={h.chainedError}
                    stale={h.stale}
                    staleText={t("rules.nodeStale")}
                  />
                </div>
              );
            })}
            {diag && (
              <div className="chain-diag-exit-block">
                <div className="chain-diag-exit-row">
                  <span className="chain-diag-exit-label">ip.sb</span>
                  <DiagCell
                    ms={diag.exit.ipSbMs}
                    error={diag.exit.ipSbError}
                    staleText={t("rules.nodeStale")}
                  />
                </div>
                <div className="chain-diag-exit-row">
                  <span className="chain-diag-exit-label">{t("chain.diagRealExit")}</span>
                  {diag.exit.geo ? (
                    <span className="chain-diag-cell ok chain-diag-exit-ip" title={diag.exit.geo.ip}>
                      {diag.exit.geo.ip}
                    </span>
                  ) : (
                    <DiagCell error={diag.exit.ipError} staleText={t("rules.nodeStale")} />
                  )}
                </div>
                {diag.exit.geo && (
                  <div className="chain-diag-geo">
                    {!!diag.exit.geo.countryCode && (
                      <span className="chain-diag-geo-cc" title={diag.exit.geo.country ?? undefined}>
                        {diag.exit.geo.countryCode.toUpperCase()}
                      </span>
                    )}
                    <span className="chain-diag-geo-place">
                      {[diag.exit.geo.country, diag.exit.geo.region, diag.exit.geo.city]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                    {(diag.exit.geo.asn || diag.exit.geo.asnOrganization) && (
                      <span
                        className="chain-diag-geo-asn"
                        title={diag.exit.geo.organization ?? undefined}
                      >
                        {[diag.exit.geo.asn, diag.exit.geo.asnOrganization]
                          .filter(Boolean)
                          .join(" · ")}
                      </span>
                    )}
                    {!!diag.exit.geo.timezone && (
                      <span className="chain-diag-geo-tz">{diag.exit.geo.timezone}</span>
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
          <p className="muted chain-diag-hint">{t("chain.diagHint")}</p>
        </div>
        <footer className="modal-footer">
          <GlassButton
            variant="primary"
            disabled={busy}
            onClick={() => void run()}
          >
            {busy
              ? t("chain.diagRunning")
              : diag
                ? t("chain.diagRerun")
                : t("chain.diagStart")}
          </GlassButton>
          <GlassButton onClick={onClose}>{t("common.close")}</GlassButton>
        </footer>
      </div>
      {error && <ErrorModal message={error} onClose={() => setError(null)} />}
    </div>
  );
}

function ChainEditorModal({
  chain,
  nodes,
  pools,
  onClose,
  onSaved,
}: {
  /** `null` = creating a new chain. */
  chain: ProxyChain | null;
  nodes: ProxyNode[];
  pools: NodePool[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useI18n();
  const nodeById = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  const poolById = useMemo(() => new Map(pools.map((p) => [p.id, p])), [pools]);

  const [name, setName] = useState(chain?.name ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Empty-name on save is surfaced inline (focus + highlight) instead of an
  // error modal, so the user can just type the name and hit save again.
  const nameInputRef = useRef<HTMLInputElement>(null);
  const [nameMissing, setNameMissing] = useState(false);
  // New chains auto-name from the graph ("A → B → C") until the user edits
  // the field; clearing it entirely resumes auto-generation. Existing chains
  // never get their name touched.
  const nameDirtyRef = useRef(chain != null);
  const handleAutoName = useCallback((label: string) => {
    if (nameDirtyRef.current) return;
    setName(label.slice(0, 64));
  }, []);

  function onNameChange(value: string) {
    setName(value);
    nameDirtyRef.current = value.trim().length > 0;
    if (nameMissing && value.trim()) setNameMissing(false);
  }

  async function onSubmit(flowNodes: FlowNode[], edges: Edge[]) {
    const trimmed = name.trim();
    if (!trimmed) {
      setNameMissing(true);
      nameInputRef.current?.focus();
      nameInputRef.current?.select();
      return;
    }
    const ordered = hopsFromGraph(flowNodes, edges);
    if (ordered === null) {
      setError(t("chain.needLinearChain"));
      return;
    }
    if (ordered.length < 2) {
      setError(t("chain.needAtLeastTwoHops"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const hops = ordered.map((o) => o.hop);
      if (chain) {
        await updateChain(chain.id, trimmed, hops);
      } else {
        await createChain(trimmed, hops);
      }
      onSaved();
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop">
      <div className="modal chain-editor-modal">
        <header className="modal-header">
          <h2>{chain ? t("chain.editChain") : t("chain.newChain")}</h2>
          <button type="button" className="icon-btn" onClick={onClose}>
            ×
          </button>
        </header>
        <label
          className={`field chain-editor-name-field${nameMissing ? " field-invalid" : ""}`}
        >
          <span>{t("chain.chainName")}</span>
          <input
            ref={nameInputRef}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={name}
            onChange={(e) => onNameChange(e.target.value)}
            placeholder={t("chain.chainNamePh")}
            maxLength={64}
            autoFocus
          />
          {nameMissing && <em className="field-invalid-msg">{t("chain.needChainName")}</em>}
        </label>
        <ReactFlowProvider>
          <ChainFlowEditor
            chain={chain}
            nodes={nodes}
            pools={pools}
            nodeById={nodeById}
            poolById={poolById}
            onSubmit={onSubmit}
            busy={busy}
            onClose={onClose}
            autoName={chain ? undefined : handleAutoName}
          />
        </ReactFlowProvider>
      </div>
      {error && <ErrorModal message={error} onClose={() => setError(null)} />}
    </div>
  );
}

export function ChainPage({ embedded = false }: { embedded?: boolean }) {
  const { t } = useI18n();
  const [pools, setPools] = useState<NodePool[]>([]);
  const [chains, setChains] = useState<ProxyChain[]>([]);
  const [nodes, setNodes] = useState<ProxyNode[]>([]);
  /** Rule-set names referencing each chain (deletion-risk hint). */
  const [chainUsage, setChainUsage] = useState<Record<string, string[]>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [poolEditor, setPoolEditor] = useState<{ pool: NodePool | null } | null>(null);
  const [chainEditor, setChainEditor] = useState<{ chain: ProxyChain | null } | null>(null);
  const [diagFor, setDiagFor] = useState<ProxyChain | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<
    { kind: "pool" | "chain"; id: string; name: string } | null
  >(null);
  /** Which ⋮ menu is open ("pool-…"/"chain-…") — single owner for every
      RowMenu on the page so a second one replaces the first. */
  const [menuId, setMenuId] = useState<string | null>(null);

  useEffect(() => {
    if (!menuId) return;
    function onDocPointerDown(e: PointerEvent) {
      const t = e.target as HTMLElement | null;
      if (t?.closest?.("[data-chain-menu]")) return;
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

  const nodeById = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);
  const poolById = useMemo(() => new Map(pools.map((p) => [p.id, p])), [pools]);
  const poolChainCount = useMemo(() => {
    const counts = new Map<string, number>();
    for (const c of chains) {
      for (const h of c.hops) {
        if (h.kind === "pool") counts.set(h.pool_id, (counts.get(h.pool_id) ?? 0) + 1);
      }
    }
    return counts;
  }, [chains]);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [p, c, n, usage] = await Promise.all([
        listPools(),
        listChains(),
        listAllNodes(),
        listChainUsage(),
      ]);
      setPools(p);
      setChains(c);
      setNodes(n);
      setChainUsage(usage);
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function onConfirmDelete() {
    if (!confirmDelete) return;
    try {
      if (confirmDelete.kind === "pool") {
        await deletePool(confirmDelete.id);
      } else {
        await deleteChain(confirmDelete.id);
      }
      setConfirmDelete(null);
      await reload();
    } catch (err) {
      setConfirmDelete(null);
      setError(typeof err === "string" ? err : String(err));
    }
  }

  return (
    <div className={embedded ? "settings-embed chain-embed" : "page chain-page"}>
      {!embedded && (
        <header className="page-header">
          <div>
            <h1>{t("chain.title")}</h1>
            <p className="page-desc">{t("chain.desc")}</p>
          </div>
        </header>
      )}

      <section className="chain-section">
        <div className="chain-section-head">
          <h2>{t("chain.poolsHeading")}</h2>
          <GlassButton onClick={() => setPoolEditor({ pool: null })}>
            {t("common.create")}
          </GlassButton>
        </div>
        {loading ? (
          <p className="muted">{t("common.loading")}</p>
        ) : pools.length === 0 ? (
          <p className="muted">{t("chain.noPoolsYet")}</p>
        ) : (
          <div className="card chain-pool-list">
            {pools.map((p, i) => (
              <PoolRow
                key={p.id}
                pool={p}
                nodes={nodes}
                usedByChains={poolChainCount.get(p.id) ?? 0}
                flipUp={i >= Math.ceil(pools.length / 2)}
                openMenuId={menuId}
                setOpenMenuId={setMenuId}
                onEdit={() => setPoolEditor({ pool: p })}
                onDelete={() => setConfirmDelete({ kind: "pool", id: p.id, name: p.name })}
              />
            ))}
          </div>
        )}
      </section>

      <section className="chain-section">
        <div className="chain-section-head">
          <h2>{t("chain.chainsHeading")}</h2>
          <GlassButton onClick={() => setChainEditor({ chain: null })}>
            {t("common.create")}
          </GlassButton>
        </div>
        {loading ? (
          <p className="muted">{t("common.loading")}</p>
        ) : chains.length === 0 ? (
          <p className="muted">{t("chain.noChainsYet")}</p>
        ) : (
          <div className="chain-list">
            {chains.map((c, i) => {
              const usageNames = chainUsage[c.id] ?? [];
              return (
                <div key={c.id} className="card chain-card">
                  <div className="chain-card-head">
                    <span className="chain-card-name" title={c.name}>
                      {c.name}
                    </span>
                    <span className="chain-card-badges">
                      <span className="chain-row-meta">
                        {t("chain.hopsCount", { n: c.hops.length })}
                      </span>
                      {usageNames.length > 0 ? (
                        <span className="chain-card-usage" title={usageNames.join(" · ")}>
                          {t("chain.usedByRuleSets", { n: usageNames.length })}
                        </span>
                      ) : (
                        <span className="chain-card-usage none">{t("chain.notUsedByRules")}</span>
                      )}
                      <RowMenu
                        id={`chain-${c.id}`}
                        openId={menuId}
                        setOpenId={setMenuId}
                        flipUp={i >= Math.ceil(chains.length / 2)}
                        items={[
                          { key: "diag", label: t("chain.diag"), onClick: () => setDiagFor(c) },
                          {
                            key: "edit",
                            label: t("common.edit"),
                            onClick: () => setChainEditor({ chain: c }),
                          },
                          {
                            key: "delete",
                            label: t("common.delete"),
                            danger: true,
                            onClick: () =>
                              setConfirmDelete({ kind: "chain", id: c.id, name: c.name }),
                          },
                        ]}
                      />
                    </span>
                  </div>
                  {/* The pipeline IS the card: stations on a rail — entry
                      dot green, exit dot blue, arrows read client →
                      internet; pools marked by the layer glyph. */}
                  <div className="chain-stepper">
                    {c.hops.map((hop, i) => {
                      const { text, stale } = hopLabel(hop, nodeById, poolById);
                      const isPool = hop.kind === "pool";
                      const last = i === c.hops.length - 1;
                      const role = i === 0 ? "entry" : last ? "exit" : "mid";
                      return (
                        <div key={i} className={`chain-station${stale ? " stale" : ""}`} title={text}>
                          <span className="chain-station-marker" aria-hidden="true">
                            {/* Ghost rails keep first/last dots centered over
                                their names while the visible rail stays
                                connected edge-to-edge. */}
                            <span className={`chain-rail${i > 0 ? "" : " ghost"}`} />
                            <span className={`chain-station-dot ${role}`} />
                            {last ? (
                              <span className="chain-rail ghost" />
                            ) : (
                              <span className="chain-rail">
                                <span className="chain-rail-arrow" />
                              </span>
                            )}
                          </span>
                          <span className="chain-station-name">
                            {isPool && (
                              <span className="chain-flow-glyph" aria-hidden="true">
                                <PoolGlyph />
                              </span>
                            )}
                            {text}
                          </span>
                          <span className="chain-flow-type">
                            {isPool ? t("chain.pool") : t("rules.pickNode")}
                          </span>
                        </div>
                      );
                    })}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </section>

      {poolEditor && (
        <PoolEditorModal
          pool={poolEditor.pool}
          nodes={nodes}
          onClose={() => setPoolEditor(null)}
          onSaved={() => {
            setPoolEditor(null);
            void reload();
          }}
        />
      )}
      {chainEditor && (
        <ChainEditorModal
          chain={chainEditor.chain}
          nodes={nodes}
          pools={pools}
          onClose={() => setChainEditor(null)}
          onSaved={() => {
            setChainEditor(null);
            void reload();
          }}
        />
      )}
      {diagFor && <ChainDiagModal chain={diagFor} onClose={() => setDiagFor(null)} />}
      {confirmDelete && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-header">
              <h2>{t("common.confirm")}</h2>
            </header>
            <div className="modal-body">
              <p>
                {confirmDelete.kind === "pool"
                  ? t("chain.confirmDeletePool", { name: confirmDelete.name })
                  : t("chain.confirmDeleteChain", { name: confirmDelete.name })}
              </p>
            </div>
            <footer className="modal-footer">
              <GlassButton onClick={() => setConfirmDelete(null)}>
                {t("common.cancel")}
              </GlassButton>
              <GlassButton variant="danger" onClick={() => void onConfirmDelete()}>
                {t("common.delete")}
              </GlassButton>
            </footer>
          </div>
        </div>
      )}
      {error && <ErrorModal message={error} onClose={() => setError(null)} />}
    </div>
  );
}
