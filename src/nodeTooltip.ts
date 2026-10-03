import type { MessageKey } from "./i18n";
import type { ProxyNode } from "./types";

/**
 * Inline subscription label `名字(id:末4位)` — the one inline id format,
 * shared by the node hover card, the detail modal's 订阅 row, subscription
 * group headers and the simple-mode stopped-list tooltip. Matches the old
 * backend `display_name()` suffix (`末4位` chars, lowercased).
 */
export function subLabel(
  name: string | undefined,
  id: string | undefined,
): string | undefined {
  if (!name && !id) return undefined;
  if (!name) return id;
  // Builtin WARP owner: no subscription row exists behind the id, and the
  // suffix would read as a fake hash — render the clean name alone.
  if (id === "builtin-warp") return name;
  const tail = (id ?? "").slice(-4).toLowerCase();
  return tail ? `${name}(id:${tail})` : name;
}

/**
 * Hover tooltip for a node row/card — the ONE hover popup for the whole
 * card: name, owning subscription (`名字(id:末4位)` plus the full
 * subscription id on its own line — the hash string users need to copy),
 * endpoint, transport & TLS markers (only when present), UDP support.
 * Elements inside a node card must NOT carry their own `title`, otherwise
 * hovering different parts of the card pops different tooltips.
 */
export function nodeHoverTitle(
  n: ProxyNode,
  t: (key: MessageKey) => string,
): string {
  const markers: string[] = [];
  if (n.transport && n.transport.type !== "tcp") {
    markers.push(n.transport.type);
  }
  if (n.tls?.enabled) {
    markers.push("TLS");
    if (n.tls.reality_public_key) markers.push("REALITY");
    if (n.tls.server_name) markers.push(`SNI ${n.tls.server_name}`);
    if (n.tls.insecure) markers.push(t("rules.nodeTipInsecure"));
  }
  const endpoint = `${n.protocol} · ${n.server}:${n.port}`;
  const lines: string[] = [n.name];
  const sub = subLabel(n.subscription_name, n.subscription_id);
  if (sub) {
    lines.push(sub);
    if (n.subscription_id) lines.push(`id: ${n.subscription_id}`);
  }
  lines.push(
    endpoint,
    ...(markers.length ? [markers.join(" · ")] : []),
    n.udp === false ? t("rules.nodeTipUdpNo") : t("rules.nodeTipUdp"),
  );
  return lines.join("\n");
}

/** Spread onto an element to give it the node hover card with the fast
 *  trigger tier: `{...nodeTip(n, t)}` — title plus the kind marker the
 *  global tooltip installer reads for its delay. */
export function nodeTip(
  n: ProxyNode,
  t: (key: MessageKey) => string,
): { title: string; "data-tip-kind": string } {
  return { title: nodeHoverTitle(n, t), "data-tip-kind": "node" };
}

/** Secondary capability badges for node rows/cards — the "vless·reality"
 *  class of attached info. TLS layer first (reality outranks plain tls),
 *  then the transport when it isn't plain TCP. Grouping and search stay
 *  keyed on the main protocol only; these are display-only. */
export function nodeFeatureBadges(n: ProxyNode): string[] {
  const badges: string[] = [];
  if (n.tls?.reality_public_key) badges.push("reality");
  else if (n.tls?.enabled) badges.push("tls");
  if (n.transport && n.transport.type !== "tcp") badges.push(n.transport.type);
  return badges;
}
