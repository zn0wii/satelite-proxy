import { lazy, Suspense, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { refreshProxyStatus } from "./api";
import { TopNav } from "./components/TopNav";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { ErrorModal } from "./components/ErrorModal";
import { beginCoreBusy } from "./coreBusy";
import { ImportIntentProvider, useImportIntent } from "./ImportIntentContext";
import { LocaleProvider } from "./i18n";
import { ThemeProvider } from "./theme";
import { DashboardPage } from "./pages/DashboardPage";
import type { NavKey } from "./types";
import { UiModeProvider, useUiMode } from "./ui/UiModeContext";
import { SimpleShell } from "./ui/simple";
import { useViewportScale } from "./hooks/useViewportScale";
import { useGlobalShortcuts } from "./hooks/useGlobalShortcuts";
import "./App.css";

// Cmd/Ctrl+<digit> → tab, matching TopNav's on-screen order.
const PRO_SHORTCUT_MAP: Partial<Record<string, NavKey>> = {
  "1": "dashboard",
  "2": "nodes",
  "3": "config",
  "4": "traffic",
  "5": "logs",
};

// Secondary pages: code-split so low-memory WebView recreate only parses home first.
const ConfigPage = lazy(() =>
  import("./pages/ConfigPage").then((m) => ({ default: m.ConfigPage })),
);
const NodesPage = lazy(() =>
  import("./pages/NodesPage").then((m) => ({ default: m.NodesPage })),
);
const TrafficPage = lazy(() =>
  import("./pages/TrafficPage").then((m) => ({ default: m.TrafficPage })),
);
const LogsPage = lazy(() =>
  import("./pages/LogsPage").then((m) => ({ default: m.LogsPage })),
);
const SettingsPage = lazy(() =>
  import("./pages/SettingsPage").then((m) => ({ default: m.SettingsPage })),
);

function PageFallback() {
  return (
    <div className="page page-fallback" aria-busy="true">
      <div className="skel skel-line skel-w-40" />
      <div className="skel skel-block" />
      <div className="skel skel-line skel-w-60" />
      <div className="skel skel-line skel-w-50" />
    </div>
  );
}

function ProShell() {
  const [nav, setNav] = useState<NavKey>("dashboard");
  const { token, prefill } = useImportIntent();

  // One-click subscribe → jump to profiles so ConfigPage can open the add form.
  useEffect(() => {
    if (token && prefill) setNav("config");
  }, [token, prefill]);

  useGlobalShortcuts(PRO_SHORTCUT_MAP, setNav, "settings");

  return (
    <div
      className={`app-shell ${nav === "dashboard" ? "dashboard-shell" : ""}`}
    >
      <TopNav active={nav} onChange={setNav} />
      <main className="main">
        {/* key={nav} forces a remount on page switch → triggers the CSS
            page-enter fade/slide animation below. */}
        <div className="page-enter" key={nav}>
          {nav === "dashboard" && (
            <DashboardPage
              onGoProfiles={() => setNav("config")}
              onGoNodes={() => setNav("nodes")}
              onGoTraffic={() => setNav("traffic")}
              onGoSettings={() => setNav("settings")}
            />
          )}
          {nav !== "dashboard" && (
            <Suspense fallback={<PageFallback />}>
              {nav === "config" && <ConfigPage />}
              {nav === "nodes" && <NodesPage />}
              {nav === "traffic" && <TrafficPage />}
              {nav === "logs" && <LogsPage />}
              {nav === "settings" && <SettingsPage />}
            </Suspense>
          )}
        </div>
      </main>
    </div>
  );
}

function AppShell() {
  const { mode } = useUiMode();
  const [applyError, setApplyError] = useState<string | null>(null);

  // Maximize magnification: zoom the whole UI when the OS window exceeds
  // the design size (see hooks/useViewportScale.ts).
  useViewportScale(mode);

  // The watchdog announces core lifecycle edges (unexpected exit, auto
  // revival, restarts). Polling alone leaves pages stale while hidden or
  // while the runtime lock is busy — resync the shared snapshot the moment
  // the backend announces a change; subscribed pages re-render from it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen("core-status-changed", () => {
      void refreshProxyStatus();
    }).then((dispose) => {
      unlisten = dispose;
    });
    return () => unlisten?.();
  }, []);

  // Background rule/config apply restarts the core outside invoke wrappers —
  // keep the navbar spinner in sync via the apply-status event.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let endBusy: (() => Promise<void>) | undefined;
    void listen<{
      status: "restarting" | "ready" | "error";
      error?: string | null;
    }>("config-apply-status", (event) => {
      const status = event.payload.status;
      if (status === "restarting") {
        if (!endBusy) endBusy = beginCoreBusy();
      } else {
        void endBusy?.();
        endBusy = undefined;
        if (status === "error") {
          setApplyError(event.payload.error || "配置应用失败");
        } else {
          setApplyError(null);
        }
      }
    }).then((dispose) => {
      unlisten = dispose;
    });
    return () => {
      void endBusy?.();
      unlisten?.();
    };
  }, []);

  // Paint immediately from localStorage mode (Rust already sized window on recreate).
  return (
    <>
      {applyError && (
        <ErrorModal
          message={applyError}
          onClose={() => setApplyError(null)}
        />
      )}
      {/* Crash net: any render exception below would otherwise blank the
         whole window; the boundary reports it to the app log and offers a
         remount. Providers sit above it — a crash inside those still blanks
         (they run before this boundary mounts), but page/shell crashes are
         the realistic class and are fully covered. */}
      <ErrorBoundary>
        {mode === "simple" ? <SimpleShell /> : <ProShell />}
      </ErrorBoundary>
    </>
  );
}

function App() {
  return (
    <ThemeProvider>
      <LocaleProvider>
        <UiModeProvider>
          <ImportIntentProvider>
            <AppShell />
          </ImportIntentProvider>
        </UiModeProvider>
      </LocaleProvider>
    </ThemeProvider>
  );
}

export default App;
