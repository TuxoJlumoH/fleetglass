export type LogLevel = "error" | "warn" | "info";
export type LevelFilter = "all" | LogLevel;

// Substring match as specified ("errors", "panicked" count); a word like "terror" also
// counts as an error. Good enough for a display-only filter.
const ERROR_RE = /err|fatal|panic|crit/i;
const WARN_RE = /warn/i;
const LEVEL_KEYS = ["level", "lvl", "severity", "log.level"] as const;

function fromWord(word: string): LogLevel | null {
  if (ERROR_RE.test(word)) return "error";
  if (WARN_RE.test(word)) return "warn";
  return null;
}

/**
 * Heuristic level of one log line: a JSON `level`-like field wins; otherwise
 * `error|err|fatal|panic` → error, `warn` → warn, everything else → info.
 */
export function logLevel(text: string, json: boolean): LogLevel {
  if (json) {
    try {
      const obj: unknown = JSON.parse(text);
      if (typeof obj === "object" && obj !== null) {
        const rec = obj as Record<string, unknown>;
        for (const k of LEVEL_KEYS) {
          const v = rec[k];
          if (typeof v === "string") return fromWord(v) ?? "info";
          // pino/bunyan numeric levels: 40 warn, 50 error, 60 fatal.
          if (typeof v === "number") return v >= 50 ? "error" : v >= 40 ? "warn" : "info";
        }
      }
    } catch {
      // Not valid JSON after all: fall through to the text heuristic.
    }
  }
  return fromWord(text) ?? "info";
}

const cache = new WeakMap<object, LogLevel>();

/** `logLevel` memoised per entry object (entries are immutable once in the ring). */
export function entryLevel(entry: { text: string; json: boolean }): LogLevel {
  let level = cache.get(entry);
  if (level === undefined) {
    level = logLevel(entry.text, entry.json);
    cache.set(entry, level);
  }
  return level;
}

export type TimeRange = "tail" | "5m" | "1h" | "start";

/** How a time-range choice maps onto the stream request. `tail` keeps the caller's target. */
export function rangeOverrides(
  range: TimeRange,
): { tailLines?: number | null; sinceSeconds?: number | null } {
  switch (range) {
    case "5m":
      return { tailLines: null, sinceSeconds: 300 };
    case "1h":
      return { tailLines: null, sinceSeconds: 3600 };
    case "start":
      return { tailLines: null, sinceSeconds: null };
    default:
      return {};
  }
}
