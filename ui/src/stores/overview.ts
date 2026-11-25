import { Channel, isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import {
  commands,
  type AttentionItem_Serialize,
  type CapacitySummary,
  type ClusterSummary_Serialize,
  type OverviewData_Deserialize,
  type OverviewData_Serialize,
  type RightSizingItem_Serialize,
  type RightSizingReport_Serialize,
  type WarningEvent_Serialize,
} from "../generated/bindings";

export type OverviewData = OverviewData_Serialize;
export type ClusterSummary = ClusterSummary_Serialize;
export type AttentionItem = AttentionItem_Serialize;
export type WarningEvent = WarningEvent_Serialize;
export type RightSizingReport = RightSizingReport_Serialize;
export type RightSizingItem = RightSizingItem_Serialize;
export type { CapacitySummary };

export type OverviewStatus = "idle" | "loading" | "ready" | "error";

export interface OverviewState {
  /** False outside the desktop app (plain browser, tests without the Tauri global). */
  available: boolean;
  status: OverviewStatus;
  data: OverviewData | null;
  error: string | null;
  /** `Date.now()` of the last pushed update; null before the first one. */
  lastUpdatedAt: number | null;
}

export const initialOverviewState: OverviewState = {
  available: true,
  status: "idle",
  data: null,
  error: null,
  lastUpdatedAt: null,
};

export const useOverviewStore = create<OverviewState>()(() => ({ ...initialOverviewState }));

interface Subscription {
  id: number | null;
  closed: boolean;
  /** `clusters.join("\n")` the subscription was opened for. */
  key: string;
}

/** The one open (or opening) `subscribe_overview` subscription; the page is its only owner. */
let current: Subscription | null = null;

function errorMessage(err: unknown): string {
  return err instanceof Error
    ? err.message
    : typeof err === "object" && err !== null && "message" in err
      ? String(err.message)
      : String(err);
}

/** Channel payloads use the `_Deserialize` shapes (optional `null`s); normalise to `_Serialize`. */
export function normaliseOverview(data: OverviewData_Deserialize): OverviewData {
  return {
    clusters: data.clusters.map((c) => ({
      ...c,
      nodesReady: c.nodesReady ?? null,
      nodesTotal: c.nodesTotal ?? null,
      podsRunning: c.podsRunning ?? null,
      podsPending: c.podsPending ?? null,
      podsFailing: c.podsFailing ?? null,
      warningEvents1h: c.warningEvents1h ?? null,
      cpu: c.cpu ?? null,
      memory: c.memory ?? null,
    })),
    attention: {
      truncated: data.attention.truncated,
      items: data.attention.items.map((i) => ({ ...i, since: i.since ?? null })),
    },
    recentWarnings: data.recentWarnings.map((w) => ({
      ...w,
      lastTimestamp: w.lastTimestamp ?? null,
    })),
    rightSizing: data.rightSizing
      ? {
          metricsAvailable: data.rightSizing.metricsAvailable,
          notice: data.rightSizing.notice ?? null,
          thresholdPct: data.rightSizing.thresholdPct,
          nearLimit: data.rightSizing.nearLimit.map((item) => ({
            ...item,
            request: item.request ?? null,
            limit: item.limit ?? null,
            percent: item.percent ?? null,
            wasted: item.wasted ?? null,
          })),
          overRequest: data.rightSizing.overRequest.map((item) => ({
            ...item,
            request: item.request ?? null,
            limit: item.limit ?? null,
            percent: item.percent ?? null,
            wasted: item.wasted ?? null,
          })),
          unusedRequests: data.rightSizing.unusedRequests.map((item) => ({
            ...item,
            request: item.request ?? null,
            limit: item.limit ?? null,
            percent: item.percent ?? null,
            wasted: item.wasted ?? null,
          })),
          missingResources: data.rightSizing.missingResources.map((item) => ({
            ...item,
            request: item.request ?? null,
            limit: item.limit ?? null,
            percent: item.percent ?? null,
            wasted: item.wasted ?? null,
          })),
        }
      : null,
  };
}

/**
 * Opens the overview subscription for `clusters` (first message is immediate, then ≤ 1/s).
 * The backend connects exactly these clusters (lazily, like a resource table); an empty list
 * reports only already-connected clusters and never connects one. Calling it again with the
 * same clusters while a subscription is open is a no-op, so a remount never opens a second
 * one; different clusters replace the subscription.
 */
export function startOverview(clusters: readonly string[]): void {
  const key = clusters.join("\n");
  if (current !== null && !current.closed) {
    if (current.key === key) return;
    stopOverview();
  }
  if (!isTauri()) {
    useOverviewStore.setState({ available: false, status: "idle", error: null });
    return;
  }
  const sub: Subscription = { id: null, closed: false, key };
  current = sub;
  useOverviewStore.setState((s) => ({
    available: true,
    // Keep showing the last data while re-subscribing (tab switched back).
    status: s.data === null ? "loading" : s.status,
    error: null,
  }));
  const channel = new Channel<OverviewData_Deserialize>();
  channel.onmessage = (data) => {
    if (sub.closed) return;
    useOverviewStore.setState({
      status: "ready",
      data: normaliseOverview(data),
      error: null,
      lastUpdatedAt: Date.now(),
    });
  };
  commands
    .subscribeOverview([...clusters], channel)
    .then((res) => {
      if (res.status === "error") {
        if (!sub.closed) {
          sub.closed = true;
          useOverviewStore.setState({ status: "error", error: res.error.message });
        }
        return;
      }
      sub.id = res.data;
      // Stopped while `subscribe_overview` was in flight (StrictMode's unmount, a fast tab
      // switch): close the backend side now that its id is known.
      if (sub.closed) void commands.unsubscribeOverview(res.data).catch(() => undefined);
    })
    .catch((err: unknown) => {
      if (!sub.closed) {
        sub.closed = true;
        useOverviewStore.setState({ status: "error", error: errorMessage(err) });
      }
    });
}

// Mounted overview pages (both split panes can show one). They share the subscription, which
// closes when the last of them unmounts.
let users = 0;

/**
 * Registers a user of the overview subscription; the returned release closes it once no user
 * is left. Pair with `startOverview`, which opens (or keeps) the subscription itself.
 */
export function retainOverview(): () => void {
  users += 1;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    users -= 1;
    if (users === 0) stopOverview();
  };
}

/** Closes the subscription, if any. Safe while `subscribe_overview` is still in flight. */
export function stopOverview(): void {
  const sub = current;
  current = null;
  if (sub === null || sub.closed) return;
  sub.closed = true;
  if (sub.id !== null) void commands.unsubscribeOverview(sub.id).catch(() => undefined);
}

export function resetOverviewStore(): void {
  stopOverview();
  useOverviewStore.setState({ ...initialOverviewState });
}
