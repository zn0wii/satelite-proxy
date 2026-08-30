/**
 * Shared helpers for raw core stdout lines (sing-box / Xray / mihomo log
 * files). The cores don't emit structured entries — the level is inferred
 * from bracketed tags ([Error]/[Warning]/[Debug] for Xray) or the bare level
 * word (INFO/WARN/ERROR for sing-box), mirroring the TrafficPage's core-log
 * view.
 */

export type CoreLogLevel = "error" | "warn" | "info" | "debug";

/** Minimum-level options for the core log (no trace level in core output). */
export const CORE_LEVELS: CoreLogLevel[] = ["debug", "info", "warn", "error"];

export function coreLevelRank(l: CoreLogLevel): number {
  switch (l) {
    case "debug":
      return 0;
    case "info":
      return 1;
    case "warn":
      return 2;
    case "error":
      return 3;
  }
}

export function coreLogLevel(line: string): CoreLogLevel {
  const s = line.toLowerCase();
  if (/\[error\]|\bfatal\b/.test(s) || /\berror\b/.test(s)) return "error";
  if (/\[warning\]|\bwarn(ing)?\b/.test(s)) return "warn";
  if (/\[debug\]|\bdebug\b|\btrace\b/.test(s)) return "debug";
  return "info";
}
