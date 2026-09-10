import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { getProxyStatus, onProxySnapshot } from "../api";
import { useCoreBusy } from "../coreBusy";
import { useVisibleInterval } from "../hooks/useVisibleInterval";
import { useI18n } from "../i18n";
import type { MessageKey } from "../i18n/messages";
import type { NavKey } from "../types";
import { ThemeSwitch } from "./ThemeSwitch";
import { UiModeMenu } from "../ui/UiModeMenu";

type NavItem = { key: NavKey; labelKey: MessageKey };

/** Compact capsule order — style3 horizontal nav. */
const ITEMS: NavItem[] = [
  { key: "dashboard", labelKey: "dashboard.title" },
  { key: "nodes", labelKey: "nodes.title" },
  { key: "config", labelKey: "config.title" },
  { key: "traffic", labelKey: "traffic.title" },
  { key: "logs", labelKey: "logs.title" },
  { key: "settings", labelKey: "settings.title" },
];

// Fixed slot width (sized for the longest English label, "Connections" =
// 11 chars) plus the button's own left/right padding (0.5rem 0.75rem,
// App.css:434), scaled to 80% to narrow the nav capsule. Long English
// labels may clip/wrap at this width — accepted tradeoff. Every item
// shares this width so switching locale only swaps the text — the pill
// never resizes.
const ITEM_WIDTH = "calc((11ch + 1.5rem) * 0.8)";

interface Props {
  active: NavKey;
  onChange: (key: NavKey) => void;
}

export function TopNav({ active, onChange }: Props) {
  const { t } = useI18n();
  const coreBusy = useCoreBusy();
  const [running, setRunning] = useState(false);
  const [coreState, setCoreState] = useState("stopped");

  // Sliding highlight indicator: measure the active button's position/size.
  const itemRefs = useRef<Record<string, HTMLButtonElement>>({});
  const [indicatorStyle, setIndicatorStyle] = useState<CSSProperties>({
    opacity: 0,
  });
  useLayoutEffect(() => {
    const el = itemRefs.current[active];
    if (!el) return;
    setIndicatorStyle({
      opacity: 1,
      transform: `translateX(${el.offsetLeft}px)`,
      width: `${el.offsetWidth}px`,
    });
  }, [active]);

  const tick = useCallback(async () => {
    try {
      const status = await getProxyStatus().catch(() => null);
      setRunning(status?.running ?? false);
      setCoreState(status?.core_state ?? "stopped");
    } catch {
      /* ignore */
    }
  }, []);

  useEffect(() => {
    void tick();
  }, [tick]);

  // Watchdog push: the shared snapshot was just refreshed from a backend
  // lifecycle edge — read it instead of waiting out the 3s poll.
  useEffect(() => {
    return onProxySnapshot((status) => {
      setRunning(status.running);
      setCoreState(status.core_state);
    });
  }, []);

  // Steady poll when idle. While coreBusy the status pill already spins via
  // useCoreBusy — avoid hammering get_proxy_status (it contends for the same
  // runtime lock held by set_capture_mode / restart).
  useVisibleInterval(() => {
    if (coreBusy) return;
    return tick();
  }, 3000);

  const wasCoreBusyRef = useRef(coreBusy);
  useEffect(() => {
    const wasCoreBusy = wasCoreBusyRef.current;
    wasCoreBusyRef.current = coreBusy;
    if (wasCoreBusy && !coreBusy) void tick();
  }, [coreBusy, tick]);

  const transitioning =
    coreBusy ||
    coreState === "starting" ||
    coreState === "stopping";

  const stateLabel = transitioning
    ? "…"
    : running
      ? "RUN"
      : coreState === "error"
        ? "ERR"
        : "OFF";
  const dotClass = transitioning
    ? "busy"
    : running || coreState === "running"
      ? "on"
      : "off";

  return (
    <header className="topnav-wrap">
      <div className="topnav" role="navigation" aria-label="Main">
        {/* Clips the ::after glow to the capsule. Must be its own layer —
            overflow on .topnav itself would clip the ⋯ menu popup. */}
        <span className="topnav-glow" aria-hidden />
        <div className="topnav-brand" title="Satelite">
          <span className="topnav-mark" aria-hidden>
            ◈
          </span>
          <span className="topnav-brand-text">SATELITE</span>
        </div>
        <div className="topnav-divider" aria-hidden />
        <nav className="topnav-items">
          {/* Sliding highlight: positioned over the active button via layout
              effect measurements below. Width is fixed by the ref callback so
              the pill travels smoothly between unequal-width items. */}
          <span
            className="topnav-indicator"
            aria-hidden="true"
            style={indicatorStyle}
          />
          {ITEMS.map((item) => (
            <button
              key={item.key}
              type="button"
              ref={(el) => {
                if (el) itemRefs.current[item.key] = el;
              }}
              className={`topnav-item ${active === item.key ? "active" : ""}`}
              style={{ width: ITEM_WIDTH }}
              onClick={() => onChange(item.key)}
            >
              {t(item.labelKey)}
            </button>
          ))}
        </nav>
        <div className="topnav-tools">
          <ThemeSwitch />
          <div
            className="topnav-status"
            title={transitioning ? "内核切换中" : stateLabel}
            aria-busy={transitioning}
          >
            <span className={`status-dot ${dotClass}`} />
            <span className="topnav-status-text">{stateLabel}</span>
          </div>
          <UiModeMenu />
        </div>
      </div>
    </header>
  );
}
