import type { RowStatus, StatusCounts } from "../stores/pods";

/** Pill / dot colour classes from the v0.2 mockup (`.pill.ok` … `.pill.off`). */
export type StatusTone = "ok" | "warn" | "err" | "off";

/**
 * Mockup: green running, amber pending / unschedulable, red failed / back-off, grey completed.
 * Pod back-offs (CrashLoopBackOff, ImagePullBackOff) are `failed` in the core projection.
 * `degraded` (suspended, not ready, a Job between retries) and `terminating` are transitional
 * and stay amber; `unknown` is grey.
 */
export const STATUS_TONE: Record<RowStatus, StatusTone> = {
  healthy: "ok",
  pending: "warn",
  degraded: "warn",
  failed: "err",
  completed: "off",
  terminating: "warn",
  unknown: "off",
};

export const STATUS_LABEL: Record<RowStatus, string> = {
  healthy: "Healthy",
  pending: "Pending",
  degraded: "Degraded",
  failed: "Failed",
  completed: "Completed",
  terminating: "Terminating",
  unknown: "Unknown",
};

/** Restart counts at or above this are shown in red (mockup `.restarts.hot`). */
export const RESTARTS_HOT = 5;

/**
 * "Failing" matches the sidebar's `kind_counts` definition (failed + degraded). With a status
 * filter, only the selected statuses count, so the summary agrees with the filtered total.
 */
export function summaryCounts(
  counts: StatusCounts,
  statuses: readonly RowStatus[],
): { failing: number; pending: number } {
  const n = (s: RowStatus) => (statuses.length === 0 || statuses.includes(s) ? counts[s] : 0);
  return { failing: n("failed") + n("degraded"), pending: n("pending") };
}

/** Toolbar label of the status picker: "Status: any", "Status: Failed, Pending", "Status: 3 selected". */
export function statusPickerLabel(statuses: readonly RowStatus[]): string {
  if (statuses.length === 0) return "Status: any";
  if (statuses.length <= 2) return `Status: ${statuses.map((s) => STATUS_LABEL[s]).join(", ")}`;
  return `Status: ${String(statuses.length)} selected`;
}
