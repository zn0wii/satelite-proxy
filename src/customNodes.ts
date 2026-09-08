import type { ProxyNode, SortMode } from "./types";

/** Session-only latency values for custom-mode nodes (never persisted backend-side). */
export type CustomLatencyMap = Map<string, { ms: number | null; at: number | null }>;

/** Overlay session latency results onto extracted nodes. */
export function applyCustomLatency(
  nodes: ProxyNode[],
  map: CustomLatencyMap,
): ProxyNode[] {
  if (map.size === 0) return nodes;
  return nodes.map((n) => {
    const v = map.get(n.id);
    return v ? { ...n, latency_ms: v.ms, latency_at: v.at } : n;
  });
}

/** Sort nodes in place by the given mode — shared by the initial page load
 * and by in-place re-sorts as streamed latency results land (see
 * NodesPage's `onTest`). Latency order: ok < timeout < untested, then name. */
export function sortNodes(nodes: ProxyNode[], sortMode: SortMode): ProxyNode[] {
  const byName = (a: ProxyNode, b: ProxyNode) => {
    const an = a.name.toLowerCase();
    const bn = b.name.toLowerCase();
    return an < bn ? -1 : an > bn ? 1 : 0;
  };
  if (sortMode === "name") {
    nodes.sort(byName);
  } else if (sortMode === "latency") {
    const score = (n: ProxyNode): [number, number] =>
      n.latency_ms != null
        ? [0, n.latency_ms]
        : n.latency_at != null
          ? [1, 0]
          : [2, 0];
    nodes.sort((a, b) => {
      const sa = score(a);
      const sb = score(b);
      return sa[0] !== sb[0] || sa[1] !== sb[1]
        ? sa[0] !== sb[0]
          ? sa[0] - sb[0]
          : sa[1] - sb[1]
        : byName(a, b);
    });
  }
  return nodes;
}

/**
 * Client-side mirror of `list_nodes_page` semantics for custom-mode nodes
 * (read-only, extracted from the selected sing-box config on the backend).
 * Latency sort uses session results only; untested nodes sink to the bottom.
 */
export function filterCustomNodes(
  nodes: ProxyNode[],
  query: string,
  sortMode: SortMode,
  offset = 0,
  limit = 200,
): { nodes: ProxyNode[]; total: number } {
  const q = query.trim().toLowerCase();
  const filtered = nodes.filter(
    (n) =>
      !q ||
      n.name.toLowerCase().includes(q) ||
      n.server.toLowerCase().includes(q) ||
      n.protocol.toLowerCase().includes(q) ||
      (n.subscription_name ?? "").toLowerCase().includes(q),
  );
  sortNodes(filtered, sortMode);
  return {
    nodes: filtered.slice(offset, offset + limit),
    total: filtered.length,
  };
}
