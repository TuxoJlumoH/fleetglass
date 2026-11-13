import type { WarningEvent } from "../stores/overview";

/** The backend keeps at most this many attention items and warning events (02-08a). */
export const OVERVIEW_LIST_CAP = 50;

export type WarningRange = "1h" | "6h" | "24h";

export const RANGES: readonly { id: WarningRange; label: string; seconds: number }[] = [
  { id: "1h", label: "1h", seconds: 3600 },
  { id: "6h", label: "6h", seconds: 6 * 3600 },
  { id: "24h", label: "24h", seconds: 24 * 3600 },
];

/** Events in `range` among the bounded list; events without a timestamp cannot be placed. */
export function filterWarnings(
  events: readonly WarningEvent[],
  range: WarningRange,
  now: number,
): WarningEvent[] {
  const seconds = RANGES.find((r) => r.id === range)?.seconds ?? 3600;
  const from = now - seconds * 1000;
  return events.filter((e) => {
    if (e.lastTimestamp === null) return false;
    const t = Date.parse(e.lastTimestamp);
    return !Number.isNaN(t) && t >= from;
  });
}
