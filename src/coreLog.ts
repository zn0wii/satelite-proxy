/**
 * Shared helpers for raw core stdout lines (sing-box / Xray / mihomo log
 * files). The cores don't emit structured entries — the level is inferred
 * from bracketed tags ([Error]/[Warning]/[Debug] for Xray) or the bare level
 * word (INFO/WARN/ERROR for sing-box). sing-box bakes ANSI color codes into
 * its stdout and leads its timestamp with the zone offset, so lines are
 * cleaned up for display via `parseCoreLogLine`.
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

/** ANSI CSI sequences — sing-box bakes color codes into its stdout. */
const ANSI_RE = /\x1b\[[0-9;?]*[A-Za-z]/g;

export function stripAnsi(line: string): string {
  return line.replace(ANSI_RE, "");
}

/**
 * Timestamp prefix shared by all three cores, with per-core quirks:
 * sing-box leads with the zone offset (`+0800 2026-08-31 07:39:40`), Xray
 * uses slashes (`2026/08/31 07:39:40`), mihomo may append the offset and
 * fractional seconds (`2026-08-31 07:39:40.123 +08:00`).
 */
const TS_PREFIX_RE =
  /^(?:[+-]\d{2}:?\d{2}\s+)?(\d{4}[-/]\d{2}[-/]\d{2})[T ](\d{2}:\d{2}:\d{2}(?:\.\d+)?)(?:\s*[+-]\d{2}:?\d{2})?\s*/;

export interface ParsedCoreLogLine {
  /** Display time — `HH:MM:SS(.fff)`, or `MM-DD HH:MM:SS` for other days. */
  ts: string;
  /** Message with the ANSI codes and timestamp prefix removed. */
  msg: string;
  level: CoreLogLevel;
}

function localDateStamp(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Parse one raw core log line into display fields (LogsPage kernel view). */
export function parseCoreLogLine(raw: string): ParsedCoreLogLine {
  const line = stripAnsi(raw).replace(/\r$/, "");
  const m = TS_PREFIX_RE.exec(line);
  if (!m) return { ts: "", msg: line, level: coreLogLevel(line) };
  const [, date, time] = m;
  const msg = line.slice(m[0].length);
  // Clamp runaway fractional seconds; keep the whole time when it's today,
  // collapse to `MM-DD HH:MM:SS` otherwise (the tail file is per-hour, so
  // the date only differs after a core has been idle across midnight).
  const compact = time.replace(/(\.\d{3})\d+/, "$1");
  const normalized = date.replace(/\//g, "-");
  const ts =
    normalized === localDateStamp(new Date())
      ? compact
      : `${normalized.slice(5)} ${compact.slice(0, 8)}`;
  return { ts, msg, level: coreLogLevel(msg) };
}
