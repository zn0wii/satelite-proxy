import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listConnectionChanges } from "../../api";
import { applyConnectionChanges } from "../../connectionChanges";
import { useVisibleInterval } from "../../hooks/useVisibleInterval";
import { useI18n } from "../../i18n";
import type { ConnectionView } from "../../types";

interface Sample {
  up: number;
  down: number;
  conns: number;
}

interface Link {
  process: string;
  node: string;
  /** Direct (non-proxied) link — its arc renders green instead of blue. */
  direct: boolean;
  bytes: number;
  n: number;
}

interface Props {
  samples: Sample[];
  up: number;
  down: number;
  conns: number;
  running: boolean;
  label: string;
  idleLabel: string;
  idleConnsLabel: string;
  connsLabel: string;
  onOpen?: () => void;
}

const W = 320;
const H = 72;
const PAD = 6;
const MAX_LINKS = 5;

function fmtRate(bps: number) {
  if (bps < 1024) return `${Math.round(bps)} B/s`;
  if (bps < 1024 * 1024) {
    const k = bps / 1024;
    return `${k >= 10 ? Math.round(k) : k.toFixed(1)} KB/s`;
  }
  const m = bps / (1024 * 1024);
  return `${m >= 10 ? Math.round(m) : m.toFixed(1)} MB/s`;
}

function yAt(v: number, max: number) {
  const usable = H - PAD * 2;
  if (max <= 0) return PAD + usable;
  return PAD + usable - (v / max) * usable;
}

function xAt(i: number, n: number) {
  if (n <= 1) return W;
  return (i / (n - 1)) * W;
}

function points(values: number[], max: number) {
  return values.map((v, i) => ({
    x: xAt(i, values.length),
    y: yAt(v, max),
  }));
}

/** Monotone cubic (Fritsch–Carlson) so spikes stay soft without overshoot. */
function linePath(values: number[], max: number) {
  if (values.length === 0) return "";
  if (values.length === 1) {
    const y = yAt(values[0], max).toFixed(2);
    return `M0 ${y} L${W} ${y}`;
  }
  const pts = points(values, max);
  const n = pts.length;
  const dx: number[] = [];
  const slope: number[] = [];
  for (let i = 0; i < n - 1; i++) {
    const dxi = pts[i + 1].x - pts[i].x;
    dx.push(dxi);
    slope.push(dxi === 0 ? 0 : (pts[i + 1].y - pts[i].y) / dxi);
  }
  const tan: number[] = new Array(n);
  tan[0] = slope[0];
  tan[n - 1] = slope[n - 2];
  for (let i = 1; i < n - 1; i++) {
    tan[i] = slope[i - 1] * slope[i] <= 0 ? 0 : (slope[i - 1] + slope[i]) / 2;
  }
  for (let i = 0; i < n - 1; i++) {
    if (Math.abs(slope[i]) < 1e-6) {
      tan[i] = 0;
      tan[i + 1] = 0;
      continue;
    }
    const a = tan[i] / slope[i];
    const b = tan[i + 1] / slope[i];
    const s = a * a + b * b;
    if (s > 9) {
      const tau = 3 / Math.sqrt(s);
      tan[i] = tau * a * slope[i];
      tan[i + 1] = tau * b * slope[i];
    }
  }
  let d = `M${pts[0].x.toFixed(2)} ${pts[0].y.toFixed(2)}`;
  for (let i = 0; i < n - 1; i++) {
    const x1 = pts[i].x + dx[i] / 3;
    const y1 = pts[i].y + (tan[i] * dx[i]) / 3;
    const x2 = pts[i + 1].x - dx[i] / 3;
    const y2 = pts[i + 1].y - (tan[i + 1] * dx[i]) / 3;
    d += ` C${x1.toFixed(2)} ${y1.toFixed(2)} ${x2.toFixed(2)} ${y2.toFixed(2)} ${pts[i + 1].x.toFixed(2)} ${pts[i + 1].y.toFixed(2)}`;
  }
  return d;
}

function areaPath(values: number[], max: number) {
  const line = linePath(values, max);
  if (!line) return "";
  const lastX = xAt(Math.max(values.length - 1, 0), Math.max(values.length, 2));
  return `${line} L${lastX.toFixed(2)} ${H} L0 ${H} Z`;
}

function shortProcess(raw: string) {
  const base = (raw.split(/[/\\]/).pop() ?? raw).replace(/\.exe$/i, "");
  return base.trim();
}

function buildLinks(rows: ConnectionView[], directLabel: string): Link[] {
  const map = new Map<string, Link>();
  for (const r of rows) {
    if (r.closed) continue;
    const process = shortProcess(r.process) || r.host || "—";
    // "direct" is the raw outbound tag from the core — show it localized.
    const rawNode = r.node_name || r.node_tag || "—";
    const direct = rawNode === "direct";
    const node = direct ? directLabel : rawNode;
    const key = `${process}\0${node}`;
    const prev = map.get(key) ?? { process, node, direct, bytes: 0, n: 0 };
    prev.bytes += r.upload + r.download;
    prev.n += 1;
    map.set(key, prev);
  }
  return [...map.values()]
    .sort((a, b) => b.bytes - a.bytes || b.n - a.n)
    .slice(0, MAX_LINKS);
}

function uniqueKeep(items: string[]) {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of items) {
    if (seen.has(item)) continue;
    seen.add(item);
    out.push(item);
  }
  return out;
}

export function SimpleTrafficSpark({
  samples,
  up,
  down,
  conns,
  running,
  label,
  idleLabel,
  idleConnsLabel,
  connsLabel,
  onOpen,
}: Props) {
  const { t } = useI18n();
  const [rows, setRows] = useState<ConnectionView[]>([]);
  const revisionRef = useRef<number | null>(null);
  const orderRevRef = useRef<number | null>(null);
  /** Fullscreen overlay on the dashboard: the card detaches from the grid
   *  (position: fixed) and fills the window; a placeholder keeps its cell. */
  const [expanded, setExpanded] = useState(false);

  const reloadLinks = useCallback(async () => {
    if (!running) {
      revisionRef.current = null;
      orderRevRef.current = null;
      setRows([]);
      return;
    }
    try {
      const batch = await listConnectionChanges(revisionRef.current, orderRevRef.current);
      revisionRef.current = batch.revision;
      orderRevRef.current = batch.order_revision;
      if (!batch.unchanged) {
        setRows((current) => applyConnectionChanges(current, batch));
      }
    } catch {
      /* keep last snapshot */
    }
  }, [running]);

  useEffect(() => {
    void reloadLinks();
  }, [reloadLinks]);

  useVisibleInterval(() => reloadLinks(), 2000);

  // While expanded: Escape exits, and the scroll container behind the overlay
  // is frozen (class on <html>; .main keeps its reserved gutter, no reflow).
  useEffect(() => {
    if (!expanded) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setExpanded(false);
    };
    document.addEventListener("keydown", onKey);
    document.documentElement.classList.add("spark-fullscreen-open");
    return () => {
      document.removeEventListener("keydown", onKey);
      document.documentElement.classList.remove("spark-fullscreen-open");
    };
  }, [expanded]);

  const downs = samples.map((s) => s.down);
  const ups = samples.map((s) => s.up);
  const max = Math.max(1, ...downs, ...ups);
  const downLine = linePath(downs, max);
  const upLine = linePath(ups, max);
  const downArea = areaPath(downs, max);
  const upArea = areaPath(ups, max);
  const hasTraffic =
    up > 0 || down > 0 || samples.some((s) => s.up + s.down > 0);
  const hasConns = conns > 0 || samples.some((s) => s.conns > 0);
  const quiet = running && hasConns && !hasTraffic;
  const empty = !running || (!hasTraffic && !hasConns);

  const links = useMemo(
    () => buildLinks(rows, t("simple.sparkDirect")),
    [rows, t],
  );
  const processCounts = useMemo(() => {
    const map = new Map<string, number>();
    for (const r of rows) {
      if (r.closed) continue;
      const process = shortProcess(r.process) || r.host || "—";
      map.set(process, (map.get(process) ?? 0) + 1);
    }
    return map;
  }, [rows]);
  const left = useMemo(
    () => uniqueKeep(links.map((l) => l.process)),
    [links],
  );
  const right = useMemo(() => uniqueKeep(links.map((l) => l.node)), [links]);

  const flowRef = useRef<HTMLDivElement>(null);
  const leftRefs = useRef(new Map<string, HTMLElement>());
  const rightRefs = useRef(new Map<string, HTMLElement>());
  const [arcs, setArcs] = useState<
    { key: string; d: string; hot: boolean; direct: boolean }[]
  >([]);

  const measureArcs = useCallback(() => {
    const root = flowRef.current;
    if (!root || links.length === 0) {
      setArcs([]);
      return;
    }
    const box = root.getBoundingClientRect();
    if (box.width < 2 || box.height < 2) return;
    const next = links.flatMap((l) => {
      const from = leftRefs.current.get(l.process)?.getBoundingClientRect();
      const to = rightRefs.current.get(l.node)?.getBoundingClientRect();
      if (!from || !to) return [];
      const x1 = from.right - box.left;
      const y1 = from.top + from.height / 2 - box.top;
      const x2 = to.left - box.left;
      const y2 = to.top + to.height / 2 - box.top;
      if (x2 - x1 < 8) return [];
      const dx = (x2 - x1) * 0.42;
      return [
        {
          key: `${l.process}->${l.node}`,
          d: `M ${x1.toFixed(1)} ${y1.toFixed(1)} C ${(x1 + dx).toFixed(1)} ${y1.toFixed(1)}, ${(x2 - dx).toFixed(1)} ${y2.toFixed(1)}, ${x2.toFixed(1)} ${y2.toFixed(1)}`,
          hot: l.bytes > 0,
          direct: l.direct,
        },
      ];
    });
    setArcs(next);
  }, [links]);

  useLayoutEffect(() => {
    measureArcs();
  }, [measureArcs, left, right]);

  useEffect(() => {
    const root = flowRef.current;
    if (!root) return;
    const ro = new ResizeObserver(() => measureArcs());
    ro.observe(root);
    return () => ro.disconnect();
  }, [measureArcs]);

  return (
    <>
      {expanded && (
        <div
          className="simple-spark-backdrop"
          onClick={() => setExpanded(false)}
          aria-hidden
        />
      )}
      <div
        role="button"
        tabIndex={0}
        className={`simple-spark${hasTraffic ? " live" : ""}${quiet ? " quiet" : ""}${expanded ? " expanded" : ""}`}
        onClick={expanded ? undefined : onOpen}
        onKeyDown={
          expanded
            ? undefined
            : (e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onOpen?.();
                }
              }
        }
        aria-label={label}
      >
        <header className="simple-spark-head">
          <span className="instrument-label">
            {label}
            {quiet ? (
              <span className="simple-spark-tag muted">{idleConnsLabel}</span>
            ) : null}
          </span>
          <span className="simple-spark-head-right">
            <span className="simple-spark-legend mono">
              <span className="simple-spark-conns">{connsLabel}</span>
              <span className="tr-dir down">↓ {fmtRate(down)}</span>
              <span className="tr-dir up">↑ {fmtRate(up)}</span>
            </span>
            {/* Fullscreen toggle: stopPropagation keeps the card's own
                click/Enter "open traffic page" action out of the way. */}
            <button
              type="button"
              className="icon-btn simple-spark-fs"
              aria-label={expanded ? t("simple.sparkShrink") : t("simple.sparkExpand")}
              title={expanded ? t("simple.sparkShrink") : t("simple.sparkExpand")}
              aria-pressed={expanded}
              onClick={(e) => {
                e.stopPropagation();
                setExpanded((v) => !v);
              }}
              onKeyDown={(e) => e.stopPropagation()}
            >
              <svg
                viewBox="0 0 16 16"
                width="12"
                height="12"
                aria-hidden
                fill="none"
                stroke="currentColor"
                strokeWidth="1.6"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                {expanded ? (
                  <path d="M6.5 2v4.5H2M9.5 2v4.5H14M6.5 14V9.5H2M9.5 14V9.5H14" />
                ) : (
                  <path d="M2 6V2h4M14 6V2h-4M2 10v4h4M14 10v4h-4" />
                )}
              </svg>
            </button>
          </span>
        </header>
        <div className="simple-spark-plot">
          <svg
            className="simple-spark-bg"
            viewBox={`0 0 ${W} ${H}`}
            preserveAspectRatio="none"
            aria-hidden
          >
            <path className="simple-spark-area down" d={downArea} />
            <path className="simple-spark-area up" d={upArea} />
            <path className="simple-spark-line down" d={downLine} />
            <path className="simple-spark-line up" d={upLine} />
          </svg>
          <div className="simple-spark-flow" ref={flowRef}>
          <aside className="simple-spark-rail left">
            <div className="simple-spark-rail-kicker">
              {t("simple.sparkApps")}
            </div>
            <ul className="simple-spark-col left">
              {left.map((name) => {
                const count = processCounts.get(name) ?? 0;
                const itemLabel = `${name}:${count}`;
                return (
                  <li
                    key={name}
                    title={itemLabel}
                    ref={(el) => {
                      if (el) leftRefs.current.set(name, el);
                      else leftRefs.current.delete(name);
                    }}
                  >
                    <span className="simple-spark-app-name">{name}</span>
                    <span className="simple-spark-app-count">:{count}</span>
                  </li>
                );
              })}
            </ul>
          </aside>
          <div className="simple-spark-mid">
            {empty && (
              <div className="simple-spark-idle muted">{idleLabel}</div>
            )}
          </div>
          <aside className="simple-spark-rail right">
            <div className="simple-spark-rail-kicker">
              {t("simple.sparkNodes")}
            </div>
            <ul className="simple-spark-col right">
              {right.map((name) => (
                <li
                  key={name}
                  title={name}
                  ref={(el) => {
                    if (el) rightRefs.current.set(name, el);
                    else rightRefs.current.delete(name);
                  }}
                >
                  {name}
                </li>
              ))}
            </ul>
          </aside>
          <svg className="simple-spark-arcs" aria-hidden>
            {arcs.map((a) => (
              <path
                key={a.key}
                className={`simple-spark-arc${a.hot ? " hot" : ""}${a.direct ? " direct" : ""}`}
                d={a.d}
              />
            ))}
          </svg>
          </div>
        </div>
      </div>
      {expanded && <div className="simple-spark-placeholder" aria-hidden />}
    </>
  );
}
