import { lazy, Suspense, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { TopNav } from "./components/TopNav";
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
import { useTheme } from "./theme";
import { OceanBackgroundLazy } from "./components/OceanBackgroundLazy";
import { StarfieldBackgroundLazy } from "./components/StarfieldBackgroundLazy";
import "./App.css";

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
  const { theme, homeBackground } = useTheme();

  // One-click subscribe → jump to profiles so ConfigPage can open the add form.
  useEffect(() => {
    if (token && prefill) setNav("config");
  }, [token, prefill]);

  return (
    <div
      className={`app-shell ${nav === "dashboard" ? "dashboard-shell" : ""}`}
    >
      {/* Background canvases mount here, NOT inside DashboardPage: a
          position:fixed canvas nested in .main's scroll container drifts with
          the scroll position in WKWebView (window too small to fit the page
          → the sky no longer covers the navbar band). */}
      {nav === "dashboard" && theme === "aerospace" && (
        <>
          {homeBackground === "ocean" ? (
            <OceanBackgroundLazy />
          ) : (
            <StarfieldBackgroundLazy />
          )}
        </>
      )}
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
      {mode === "simple" ? <SimpleShell /> : <ProShell />}
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
