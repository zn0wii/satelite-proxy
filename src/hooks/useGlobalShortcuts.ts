import { useEffect } from "react";
import { restartProxy } from "../api";

const isMac = /Mac|iPhone|iPad/i.test(navigator.userAgent);

/** True when the active element would consume the keystroke as text input. */
function isTypingTarget(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  return (el as HTMLElement).isContentEditable;
}

/**
 * Global Cmd/Ctrl+<digit> page-switch shortcuts plus Cmd/Ctrl+, (settings)
 * and Cmd/Ctrl+R (restart core). Skipped while a text input is focused so
 * typing (e.g. "1", ",") is never intercepted.
 *
 * @param navMap digit → NavKey for this shell's tab set (1-based, matches
 *   the on-screen tab order).
 * @param onNav switches the shell's active tab.
 * @param settingsKey the NavKey that Cmd/Ctrl+, should open.
 */
export function useGlobalShortcuts<K extends string>(
  navMap: Partial<Record<string, K>>,
  onNav: (key: K) => void,
  settingsKey: K,
) {
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const mod = isMac ? e.metaKey : e.ctrlKey;
      if (!mod || e.altKey) return;
      if (isTypingTarget(document.activeElement)) return;

      if (e.key === ",") {
        e.preventDefault();
        onNav(settingsKey);
        return;
      }
      if (e.key === "r" || e.key === "R") {
        e.preventDefault();
        void restartProxy();
        return;
      }
      const target = navMap[e.key];
      if (target) {
        e.preventDefault();
        onNav(target);
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [navMap, onNav, settingsKey]);
}
