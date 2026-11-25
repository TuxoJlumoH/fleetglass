import { useEffect, useMemo, useState } from "react";
import { useStore } from "zustand";
import { type PodsStore, usePodsStore } from "../stores/pods";
import {
  type AccessNotice,
  accessNotices,
  loadingProgress,
  type RestrictedScope,
  restrictedScopes,
} from "./podAccess";
import { summaryCounts } from "./statusTone";

/** A fresh query that answers within this time never flashes "Connecting…". */
export const CONNECTING_GRACE_MS = 400;

export interface ListStatus {
  /** Clusters of the current query; empty means "Select a context". */
  queried: readonly string[];
  /** A failed IPC call with no access states: the error is shown, nothing is "Connecting". */
  ipcFailed: boolean;
  notices: AccessNotice[];
  /** Stores watched namespace by namespace after a cluster-wide 403 (05-01). */
  restricted: RestrictedScope[];
  metricsNotInstalled: boolean;
  metricsForbidden: boolean;
  hasNotices: boolean;
  anyReady: boolean;
  /** "Loading N of M…" while the initial lists stream in. */
  progress: string | null;
  /** Every queried store is Ready, so counts can be claimed. */
  allReady: boolean;
  /** Failing / pending counts; null unless `allReady`. */
  summary: { failing: number; pending: number } | null;
}

/**
 * Honest-UI state of the resource list shared by the desktop table and the phone list
 * (03-03b): access notices, metrics notices, loading progress and the counts summary, which is
 * only claimed when every queried store is Ready.
 */
export function useListStatus(
  kind: string,
  podsStore: PodsStore = usePodsStore,
): ListStatus {
  const querySpec = useStore(podsStore, (s) => s.querySpec);
  const access = useStore(podsStore, (s) => s.access);
  const error = useStore(podsStore, (s) => s.error);
  const pageLoaded = useStore(podsStore, (s) => s.pageLoaded);
  const statusCounts = useStore(podsStore, (s) => s.statusCounts);

  const queried = querySpec.clusters;
  const queryTarget = `${querySpec.kinds.join(",")}\n${queried.join(",")}`;
  // Every change of target starts a fresh grace period (also when switching back to an earlier
  // target), as the desktop table always did.
  const [graceOver, setGraceOver] = useState<{ target: string; over: boolean }>({
    target: queryTarget,
    over: false,
  });
  if (graceOver.target !== queryTarget) setGraceOver({ target: queryTarget, over: false });
  useEffect(() => {
    const timer = setTimeout(() => {
      setGraceOver({ target: queryTarget, over: true });
    }, CONNECTING_GRACE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [queryTarget]);
  const connectingGraceOver = graceOver.target === queryTarget && graceOver.over;
  // A failed IPC call (shown as an alert) means no access states arrived; don't claim "Connecting".
  const ipcFailed = error !== null && access.length === 0;
  // Before any access state arrives, wait briefly so switching kinds doesn't flash "Connecting…".
  const awaitingFirstAnswer = access.length === 0 && !connectingGraceOver;
  const notices = useMemo(
    () => (ipcFailed || awaitingFirstAnswer ? [] : accessNotices(queried, access)),
    [ipcFailed, awaitingFirstAnswer, queried, access],
  );
  const restricted = useMemo(
    () => (ipcFailed ? [] : restrictedScopes(queried, access)),
    [ipcFailed, queried, access],
  );
  const metricsState = (state: "notInstalled" | "forbidden") =>
    !ipcFailed &&
    !awaitingFirstAnswer &&
    access.some((a) => queried.includes(a.cluster) && a.kind === kind && a.metrics?.state === state);
  const metricsNotInstalled = metricsState("notInstalled");
  const metricsForbidden = metricsState("forbidden");
  const hasNotices =
    notices.length > 0 || restricted.length > 0 || metricsForbidden || metricsNotInstalled;
  const anyReady = access.some((a) => a.access.state === "ready");
  const progress = useMemo(() => (ipcFailed ? null : loadingProgress(access)), [ipcFailed, access]);

  // Live counts are only claimed when every queried store is Ready; otherwise the access
  // notices explain why rows are missing and a "0 failing" would be a lie.
  const allReady =
    queried.length > 0 &&
    !ipcFailed &&
    pageLoaded &&
    queried.every((c) => access.some((a) => a.cluster === c)) &&
    access.every((a) => a.access.state === "ready");
  const summary =
    allReady && statusCounts !== null ? summaryCounts(statusCounts, querySpec.statuses) : null;

  return {
    queried,
    ipcFailed,
    notices,
    restricted,
    metricsNotInstalled,
    metricsForbidden,
    hasNotices,
    anyReady,
    progress,
    allReady,
    summary,
  };
}
