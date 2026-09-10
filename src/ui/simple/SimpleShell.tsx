import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { useImportIntent } from "../../ImportIntentContext";
import { useI18n } from "../../i18n";
import type { MessageKey } from "../../i18n";
import { useGlobalShortcuts } from "../../hooks/useGlobalShortcuts";
import { UiModeMenu } from "../UiModeMenu";
import { SimpleConnectPage } from "./SimpleConnectPage";

export type SimpleNavKey = "connect" | "servers" | "traffic" | "settings";

// Cmd/Ctrl+<digit> → tab, matching the on-screen tab order.
const SIMPLE_SHORTCUT_MAP: Partial<Record<string, SimpleNavKey>> = {
  "1": "connect",
  "2": "servers",
  "3": "traffic",
};

const SimpleServersPage = lazy(() =>
  import("./SimpleServersPage").then((m) => ({ default: m.SimpleServersPage })),
);
const SimpleTrafficPage = lazy(() =>
  import("./SimpleTrafficPage").then((m) => ({ default: m.SimpleTrafficPage })),
);
const SimpleSettingsPage = lazy(() =>
  import("./SimpleSettingsPage").then((m) => ({
    default: m.SimpleSettingsPage,
  })),
);

const TABS: { key: SimpleNavKey; labelKey: MessageKey }[] = [
  { key: "connect", labelKey: "nav.connect" },
  { key: "servers", labelKey: "nodes.title" },
  { key: "traffic", labelKey: "traffic.title" },
  { key: "settings", labelKey: "settings.title" },
];

function SimplePageFallback() {
  return (
    <div className="simple-page" aria-busy="true">
      <div className="skel skel-block" />
      <div className="skel skel-line skel-w-60" />
      <div className="skel skel-line skel-w-40" />
    </div>
  );
}

export function SimpleShell() {
  const { t } = useI18n();
  const [nav, setNav] = useState<SimpleNavKey>("connect");
  const { token, prefill } = useImportIntent();
  const itemRefs = useRef<Record<string, HTMLButtonElement>>({});
  const navItemsRef = useRef<HTMLElement>(null);
  const [indicatorStyle, setIndicatorStyle] = useState<CSSProperties>({
    opacity: 0,
  });

  const measureIndicator = useCallback(() => {
    const el = itemRefs.current[nav];
    if (!el) return;
    setIndicatorStyle({
      opacity: 1,
      transform: `translateX(${el.offsetLeft}px)`,
      width: `${el.offsetWidth}px`,
    });
  }, [nav]);

  useLayoutEffect(() => {
    measureIndicator();
  }, [measureIndicator]);

  // Startup race: the page can render before the window settles at the
  // simple-mode width; tabs are flex-1, so the first measurement may be stale
  // and leave an oversized indicator covering the tabs. Re-measure whenever
  // the nav container is resized.
  useEffect(() => {
    const container = navItemsRef.current;
    if (!container || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => measureIndicator());
    ro.observe(container);
    return () => ro.disconnect();
  }, [measureIndicator]);

  // One-click subscribe → open 节点 page (add subscription modal).
  useEffect(() => {
    if (token && prefill) setNav("servers");
  }, [token, prefill]);

  useGlobalShortcuts(SIMPLE_SHORTCUT_MAP, setNav, "settings");

  return (
    <div
      className={`app-shell simple-shell${nav === "connect" ? " dashboard-shell" : ""}`}
    >
      <header className="topnav-wrap simple-topnav-wrap">
        <div
          className="topnav simple-topnav"
          role="navigation"
          aria-label="Simple"
        >
          <div className="topnav-brand simple-brand" title="Satelite">
            <span className="topnav-mark" aria-hidden>
              ◈
            </span>
          </div>
          <div className="topnav-divider" aria-hidden />
          <nav
            className="topnav-items simple-topnav-items"
            ref={navItemsRef}
          >
            <span
              className="topnav-indicator"
              aria-hidden="true"
              style={indicatorStyle}
            />
            {TABS.map((item) => (
              <button
                key={item.key}
                type="button"
                ref={(el) => {
                  if (el) itemRefs.current[item.key] = el;
                }}
                className={`topnav-item ${nav === item.key ? "active" : ""}`}
                onClick={() => setNav(item.key)}
              >
                {t(item.labelKey)}
              </button>
            ))}
          </nav>
          <div className="topnav-tools simple-topnav-tools">
            <UiModeMenu />
          </div>
        </div>
      </header>
      <main className="main simple-main">
        <div className="page-enter" key={nav}>
          {nav === "connect" && (
            <SimpleConnectPage
              onGoServers={() => setNav("servers")}
              onGoTraffic={() => setNav("traffic")}
            />
          )}
          {nav !== "connect" && (
            <Suspense fallback={<SimplePageFallback />}>
              {nav === "servers" && <SimpleServersPage />}
              {nav === "traffic" && <SimpleTrafficPage />}
              {nav === "settings" && <SimpleSettingsPage />}
            </Suspense>
          )}
        </div>
      </main>
    </div>
  );
}
