import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { clustersForQuery, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { type OverviewData, startOverview, stopOverview, useOverviewStore } from "../stores/overview";
import type { Row } from "../stores/pods";
import { useResourceDetailStore } from "../stores/resourceDetail";

/** Overview logic shared by the desktop page and the mobile overview (03-03a). */

/** How often relative ages and the time-range filter are recomputed. */
export const AGE_TICK_MS = 30_000;

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${String(n)} ${n === 1 ? one : many}`;
}

/** Attention items per cluster (the cards' issue pills). */
export function useIssuesByCluster(data: OverviewData): Map<string, number> {
  return useMemo(() => {
    const out = new Map<string, number>();
    for (const item of data.attention.items) out.set(item.cluster, (out.get(item.cluster) ?? 0) + 1);
    return out;
  }, [data.attention.items]);
}

/** A cluster card's "Connect": connecting is lazy, a resource table for the context opens its watches. */
export function useConnectCluster(): (cluster: string) => void {
  const selectContext = useContextsStore((s) => s.selectContext);
  const showKind = useLayoutStore((s) => s.showKind);
  return useCallback(
    (cluster: string) => {
      void selectContext(cluster);
      showKind("Pods", "Pod");
    },
    [selectContext, showKind],
  );
}

/**
 * The overview subscription for the queried clusters, plus their persisted settings (colours,
 * protected flags). Shared by the desktop page and the mobile overview (03-03a).
 */
export function useOverviewSubscription(): void {
  const ensureSettings = useContextsStore((s) => s.ensureSettings);
  const data = useOverviewStore((s) => s.data);
  // Same clusters as the resource tables: the active selection, else the selected context.
  // The backend connects exactly these (lazily); it never connects other contexts.
  const clustersKey = useContextsStore((s) => clustersForQuery(s).join("\n"));
  // One subscription while the page is shown; StrictMode's remount reuses / reopens it, and a
  // new selection replaces it.
  useEffect(() => {
    startOverview(clustersKey === "" ? [] : clustersKey.split("\n"));
  }, [clustersKey]);
  useEffect(() => stopOverview, []);
  // The drawer belongs to this view; leaving it closes the drawer and drops in-flight fetches.
  useEffect(
    () => () => {
      useResourceDetailStore.getState().close();
    },
    [],
  );
  const clusterKey = data?.clusters.map((c) => c.cluster).join("\n") ?? "";
  useEffect(() => {
    if (clusterKey !== "") void ensureSettings(clusterKey.split("\n"));
  }, [ensureSettings, clusterKey]);
}

/** `Date.now()` refreshed every `AGE_TICK_MS`; `touch` refreshes it right away. */
export function useOverviewNow(): [number, () => void] {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => {
      setNow(Date.now());
    }, AGE_TICK_MS);
    return () => {
      clearInterval(id);
    };
  }, []);
  const touch = useCallback(() => {
    setNow(Date.now());
  }, []);
  return [now, touch];
}

/** Resource drawer opened from an overview row; closing returns focus to that row. */
export function useOverviewDrawer(onOpen?: () => void) {
  const row = useResourceDetailStore((s) => s.target);
  const detail = useResourceDetailStore((s) => s.detail);
  const loading = useResourceDetailStore((s) => s.loading);
  const error = useResourceDetailStore((s) => s.error);
  const origin = useRef<HTMLElement | null>(null);
  const open = useCallback(
    (target: Row, from: HTMLElement) => {
      origin.current = from;
      onOpen?.();
      void useResourceDetailStore.getState().open(target);
    },
    [onOpen],
  );
  const close = useCallback(() => {
    useResourceDetailStore.getState().close();
    const from = origin.current;
    origin.current = null;
    if (from?.isConnected) from.focus();
  }, []);
  return { row, detail, loading, error, open, close };
}
