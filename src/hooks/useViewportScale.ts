import { useEffect } from "react";
import type { UiMode } from "../ui/UiModeContext";
import { PRO_WINDOW, SIMPLE_WINDOW } from "../ui/windowLayout";
import { markZoomChanged } from "./viewportScale";

/**
 * Magnify the whole UI when the OS window grows past the design size
 * (drag-resize enlargement, Windows maximize etc.). Sets CSS `zoom` on the
 * root element, so the app renders at design proportions scaled up —
 * instead of a small UI floating in a big window. Sets `data-ui-scaled`
 * on <html> for companion CSS (centering the simple strip) whenever any
 * dimension exceeds the design, even when the zoom itself stays 1.
 *
 * Scale = min(width / designWidth, height / designHeight) — the full
 * design area stays visible; the extra width is used by the fluid layout.
 *
 * Window size comes from the Tauri window API (OS-level px), never
 * `window.innerWidth`: root zoom changes what innerWidth reports, so a
 * DOM measurement would feed back into the scale and oscillate.
 * `window.devicePixelRatio` is the OS DPI ratio — element zoom cannot
 * change it, so physical→logical conversion stays stable.
 */
export function useViewportScale(mode: UiMode): void {
  useEffect(() => {
    const design = mode === "simple" ? SIMPLE_WINDOW : PRO_WINDOW;
    let disposed = false;
    let unlisten: (() => void) | undefined;

    const applyScale = (logicalWidth: number, logicalHeight: number) => {
      const fit = Math.min(
        logicalWidth / design.width,
        logicalHeight / design.height,
      );
      // Scale up only: windowed sizes stay pixel-exact (no zoom).
      // Quantize to 1% so drag-resize writes a stable style value.
      const scale = fit > 1.02 ? Math.round(fit * 100) / 100 : 1;
      // `data-ui-scaled` marks ANY dimension past the design (>2%), even
      // when only one axis grew and the min-ratio zoom stays 1 — companion
      // CSS pins the simple strip to its design width instead of letting
      // the fixed-width design stretch.
      const oversized =
        logicalWidth > design.width * 1.02 ||
        logicalHeight > design.height * 1.02;
      const root = document.documentElement;
      const nextZoom = scale === 1 ? "" : String(scale);
      // Skip no-op rewrites so repeated resize events don't retrigger the
      // transition / settle dispatch.
      if (
        root.style.zoom === nextZoom &&
        root.hasAttribute("data-ui-scaled") === oversized
      ) {
        return;
      }
      // Fresh webview load (cold start / tray recreate at the persisted
      // size): jump straight to the scale — animating right after the
      // window appears reads as a magnify-in animation.
      const jump = root.style.zoom === "" && nextZoom !== "";
      if (jump) root.style.transition = "none";
      root.style.zoom = nextZoom;
      if (oversized) {
        root.setAttribute("data-ui-scaled", "1");
      } else {
        root.removeAttribute("data-ui-scaled");
      }
      if (jump) {
        // Commit the zoom while the transition is off, then restore it.
        void root.offsetWidth;
        root.style.transition = "";
      }
      // Measurement-driven code skips while the transition animates and
      // refits on the at-rest resize this schedules (see viewportScale.ts).
      markZoomChanged();
    };

    const applyPhysical = (width: number, height: number) => {
      const dpr = window.devicePixelRatio || 1;
      applyScale(width / dpr, height / dpr);
    };

    // Initial size (permission already granted in capabilities).
    const measure = async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const size = await getCurrentWindow().innerSize();
        if (disposed) return;
        applyPhysical(size.width, size.height);
      } catch {
        /* plain browser dev: keep zoom 1 */
      }
    };

    // Scale changes on one-shot transitions (maximize / restore / mode
    // switch) and continuously during drag-resize — the 1% quantization and
    // no-op rewrite skip keep live drag cheap, so no debounce is needed.
    void measure();
    void import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) =>
        getCurrentWindow().onResized((event) =>
          applyPhysical(event.payload.width, event.payload.height),
        ),
      )
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch(() => {
        /* plain browser dev */
      });

    return () => {
      disposed = true;
      unlisten?.();
      document.documentElement.style.zoom = "";
      document.documentElement.removeAttribute("data-ui-scaled");
    };
  }, [mode]);
}
