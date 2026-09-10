import type { LatencyResult } from "./types";

/** Buffer per-node results from a streaming batch latency test and apply
 * them once per animation frame — each node's cell still flips to its value
 * the moment its probe completes (≤1 frame later), but a fast run collapses
 * dozens of channel messages into one state churn instead of one per probe.
 * The backend pushes results over a Tauri IPC channel (`api.ts` latency
 * wrappers' `onResult`); feed {@link buffer.push} into it and call
 * {@link buffer.flushNow} once the invoke resolves to apply any straggler. */
export function createLatencyResultBuffer(
  apply: (batch: Map<string, LatencyResult>) => void,
) {
  const pending = new Map<string, LatencyResult>();
  let raf = 0;
  function flush() {
    raf = 0;
    if (pending.size === 0) return;
    const batch = new Map(pending);
    pending.clear();
    apply(batch);
  }
  return {
    push(r: LatencyResult) {
      pending.set(r.id, r);
      if (!raf) raf = window.requestAnimationFrame(flush);
    },
    /** Cancel the scheduled frame and apply whatever is still buffered. */
    flushNow() {
      if (raf) {
        window.cancelAnimationFrame(raf);
        raf = 0;
      }
      flush();
    },
    /** Cancel a scheduled flush without applying (unmount cleanup). */
    stop() {
      if (raf) {
        window.cancelAnimationFrame(raf);
        raf = 0;
      }
    },
  };
}
