import { invoke } from "@tauri-apps/api/core";
import type { UiMode } from "./UiModeContext";

/** Pro console — matches tauri.conf.json default. */
export const PRO_WINDOW = { width: 960, height: 720 } as const;
/** Simple vertical strip — content ~380–400px + chrome. */
export const SIMPLE_WINDOW = { width: 420, height: 720 } as const;
/** Simple mode is user-resizable; content scrolls below this floor. */
export const SIMPLE_MIN = { width: 320, height: 480 } as const;
/** Pro cannot shrink below its design size: the viewport zoom mechanism
 *  (useViewportScale) is scale-up only, the layout is built for 960x720. */
export const PRO_MIN = PRO_WINDOW;

const SIZE_KEYS: Record<UiMode, string> = {
  pro: "satelite.proWindowSize",
  simple: "satelite.simpleWindowSize",
};

/** Persist mode for next WebView recreate (Rust reads app_data/data/ui_mode). */
export async function persistUiModePref(mode: UiMode): Promise<void> {
  try {
    await invoke("set_ui_mode_pref", { mode });
  } catch {
    /* browser / missing command */
  }
}

type Size = { width: number; height: number };

function minFor(mode: UiMode) {
  return mode === "simple" ? SIMPLE_MIN : PRO_MIN;
}

/** Clamp to the mode floor only — growing past the design size is allowed
 *  (the UI magnifies via useViewportScale), so there is no upper bound. */
function clampSize(mode: UiMode, width: number, height: number): Size {
  const min = minFor(mode);
  return {
    width: Math.max(Math.round(width), min.width),
    height: Math.max(Math.round(height), min.height),
  };
}

function readSavedSize(mode: UiMode): Size | null {
  try {
    const raw = localStorage.getItem(SIZE_KEYS[mode]);
    if (!raw) return null;
    const v = JSON.parse(raw) as { width?: unknown; height?: unknown };
    if (typeof v.width !== "number" || typeof v.height !== "number") return null;
    if (!Number.isFinite(v.width) || !Number.isFinite(v.height)) return null;
    return clampSize(mode, v.width, v.height);
  } catch {
    return null;
  }
}

/**
 * Save the window size for the active mode (debounced) so it survives WebView
 * recreate and app restarts. Restore happens in applyWindowSizeForUiMode.
 *
 * Size is read from the Tauri window API (OS-level px ÷ devicePixelRatio),
 * never `window.innerWidth`: root CSS zoom changes what innerWidth reports,
 * so a DOM measurement would store the zoomed-down design size instead of
 * the real enlarged window. Maximized sizes are skipped — restoring them on
 * next launch would produce a non-maximized full-screen window.
 */
export function watchWindowSize(mode: UiMode): () => void {
  let timer: number | undefined;
  const onResize = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      void (async () => {
        try {
          const { getCurrentWindow } = await import("@tauri-apps/api/window");
          const win = getCurrentWindow();
          if (await win.isMaximized()) return;
          const physical = await win.innerSize();
          const dpr = window.devicePixelRatio || 1;
          const size = clampSize(mode, physical.width / dpr, physical.height / dpr);
          localStorage.setItem(SIZE_KEYS[mode], JSON.stringify(size));
        } catch {
          /* browser / missing permission */
        }
      })();
    }, 300);
  };
  window.addEventListener("resize", onResize);
  return () => {
    window.removeEventListener("resize", onResize);
    window.clearTimeout(timer);
  };
}

/** Apply window size / resize policy for the active UI mode (no-op outside Tauri). */
export async function applyWindowSizeForUiMode(mode: UiMode): Promise<void> {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const { LogicalSize } = await import("@tauri-apps/api/dpi");
    const win = getCurrentWindow();
    const min = minFor(mode);
    await win.setMinSize(new LogicalSize(min.width, min.height));
    await win.setMaxSize(null);
    await win.setResizable(true);
    const design = mode === "simple" ? SIMPLE_WINDOW : PRO_WINDOW;
    let size = readSavedSize(mode) ?? design;
    // Guard against restoring an off-screen window on a smaller display.
    try {
      const { currentMonitor } = await import("@tauri-apps/api/window");
      const monitor = await currentMonitor();
      if (monitor) {
        const dpr = monitor.scaleFactor || 1;
        const avail = {
          width: monitor.size.width / dpr,
          height: monitor.size.height / dpr,
        };
        if (size.width > avail.width || size.height > avail.height) size = design;
      }
    } catch {
      /* keep saved size */
    }
    await win.setSize(new LogicalSize(size.width, size.height));
  } catch {
    /* browser / missing permission */
  }
}
