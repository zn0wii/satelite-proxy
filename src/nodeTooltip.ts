import type { MessageKey } from "./i18n";
import type { ProxyNode } from "./types";

/**
 * Hover tooltip for a node row — one fact per line: name, endpoint,
 * transport & TLS markers (own line, only when present), UDP support.
 * Shared by the 节点 tab rows/cards and the rule-set modal's node picker.
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
  return [
    n.name,
    endpoint,
    ...(markers.length ? [markers.join(" · ")] : []),
    n.udp === false ? t("rules.nodeTipUdpNo") : t("rules.nodeTipUdp"),
  ].join("\n");
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
