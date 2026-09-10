import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { CoreDownloadProgress, CoreKind } from "./types";

/**
 * Global core download/reset state — mirrors coreBusy.ts's module-singleton
 * pattern so it survives SettingsPage unmounting when the user switches to
 * another top-level tab mid-download (the progress event listener would
 * otherwise be torn down with the page and silently miss every update).
 */
export interface CoreDownloadState {
  kind: CoreKind;
  progress: CoreDownloadProgress | null;
  error: string | null;
}

let state: CoreDownloadState | null = null;
const listeners = new Set<(s: CoreDownloadState | null) => void>();

function emit() {
  for (const l of listeners) l(state);
}

// Registered once at module load — outlives any page component, so progress
// events keep landing even while SettingsPage is unmounted.
void listen<CoreDownloadProgress>("core-download-progress", (event) => {
  const kind = (event.payload.kind as CoreKind) ?? state?.kind;
  if (!kind) return;
  state = { kind, progress: event.payload, error: null };
  emit();
});

/** Call when a download/reset starts, before awaiting the backend call.
 * Seeds a "preparing" placeholder so the progress UI shows immediately,
 * before the backend's first real progress event arrives. */
export function beginCoreDownload(kind: CoreKind, viaProxy = false): void {
  state = {
    kind,
    progress: {
      kind,
      stage: "preparing",
      downloaded: 0,
      total: null,
      percent: null,
      via_proxy: viaProxy,
    },
    error: null,
  };
  emit();
}

/** Call from the catch branch with the failure message. Left in place (not
 * cleared automatically) so the floating toast can surface it if the user
 * had already left the settings page when it happened; SettingsPage's own
 * ErrorModal handles the in-page case and calls clearCoreDownload on close. */
export function setCoreDownloadError(kind: CoreKind, error: string): void {
  state = { kind, progress: state?.progress ?? null, error };
  emit();
}

/** Dismiss the current state — call on a successful finish, or after an
 * error has been shown (SettingsPage's ErrorModal close, or the toast). */
export function clearCoreDownload(): void {
  state = null;
  emit();
}

export function subscribeCoreDownload(
  listener: (s: CoreDownloadState | null) => void,
): () => void {
  listeners.add(listener);
  listener(state);
  return () => {
    listeners.delete(listener);
  };
}

export function useCoreDownloadState(): CoreDownloadState | null {
  const [s, setS] = useState(state);
  useEffect(() => subscribeCoreDownload(setS), []);
  return s;
}

/** Shared by SettingsPage's inline progress bar and the floating toast. */
export function fmtCoreBytes(value: number): string {
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}
