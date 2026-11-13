import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { commands, type KindCount } from "../generated/bindings";

/** Poll interval for the sidebar counts while the window is visible. */
export const KIND_COUNTS_POLL_MS = 2500;

/** Counts of one kind summed over the polled clusters. */
export interface KindTotals {
  total: number;
  failing: number;
  /** Every polled cluster reported this kind and all of them are `ready`. */
  ready: boolean;
}

/**
 * Sums per-cluster counts by kind. A kind that some polled cluster did not report (store never
 * opened there, or the call failed) is summed over the rest but marked not ready: the total is
 * partial. Kinds no cluster reported have no entry at all.
 */
export function aggregateKindCounts(
  perCluster: readonly (readonly KindCount[] | null)[],
): Map<string, KindTotals> {
  const sums = new Map<string, KindTotals & { clusters: number }>();
  for (const counts of perCluster) {
    if (!counts) continue;
    for (const c of counts) {
      const prev = sums.get(c.kind);
      sums.set(c.kind, {
        total: (prev?.total ?? 0) + c.total,
        failing: (prev?.failing ?? 0) + c.failing,
        ready: (prev?.ready ?? true) && c.ready,
        clusters: (prev?.clusters ?? 0) + 1,
      });
    }
  }
  const out = new Map<string, KindTotals>();
  for (const [kind, s] of sums) {
    out.set(kind, {
      total: s.total,
      failing: s.failing,
      ready: s.ready && s.clusters === perCluster.length,
    });
  }
  return out;
}

/** Same kinds with the same total/failing/ready. */
export function sameKindCounts(
  a: ReadonlyMap<string, KindTotals>,
  b: ReadonlyMap<string, KindTotals>,
): boolean {
  if (a.size !== b.size) return false;
  for (const [kind, x] of a) {
    const y = b.get(kind);
    if (!y || y.total !== x.total || y.failing !== x.failing || y.ready !== x.ready) return false;
  }
  return true;
}

async function fetchCounts(cluster: string): Promise<KindCount[] | null> {
  try {
    const res = await commands.kindCounts(cluster);
    return res.status === "ok" && Array.isArray(res.data) ? res.data : null;
  } catch {
    return null;
  }
}

const EMPTY: ReadonlyMap<string, KindTotals> = new Map();

/**
 * Polls `kind_counts` for `clusters` every {@link KIND_COUNTS_POLL_MS} while the document is
 * visible: immediately on mount, on a cluster change and on becoming visible again; never two
 * requests at once; stops when hidden or unmounted. `kind_counts` only reads stores that are
 * already watched, so polling never connects a cluster.
 */
export function useKindCounts(clusters: readonly string[]): ReadonlyMap<string, KindTotals> {
  const key = clusters.join("\n");
  const [state, setState] = useState<{ key: string; counts: ReadonlyMap<string, KindTotals> }>({
    key,
    counts: EMPTY,
  });

  useEffect(() => {
    const names = key === "" ? [] : key.split("\n");
    if (names.length === 0 || !isTauri()) return;
    let cancelled = false;
    // A call, so TypeScript does not keep a narrowing of `cancelled` across the `await` below.
    const live = () => !cancelled;
    let inFlight = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // Last counts set for this `key`: an unchanged poll skips setState entirely (even a
    // bail-out functional update can cost a render).
    let last: ReadonlyMap<string, KindTotals> | null = null;

    const visible = () => document.visibilityState !== "hidden";

    const schedule = () => {
      if (!live() || !visible() || timer !== undefined) return;
      timer = setTimeout(() => {
        timer = undefined;
        void poll();
      }, KIND_COUNTS_POLL_MS);
    };

    const poll = async () => {
      if (!live() || inFlight || !visible()) return;
      inFlight = true;
      const results = await Promise.all(names.map(fetchCounts));
      inFlight = false;
      if (!live()) return;
      const counts = aggregateKindCounts(results);
      // An unchanged poll keeps the state object, so the sidebar does not re-render.
      if (last === null || !sameKindCounts(last, counts)) {
        last = counts;
        setState({ key, counts });
      }
      schedule();
    };

    const onVisibility = () => {
      if (!visible()) {
        clearTimeout(timer);
        timer = undefined;
      } else if (!inFlight && timer === undefined) {
        void poll();
      }
    };

    void poll();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [key]);

  // Counts of the previous selection are never shown for the new one.
  return state.key === key ? state.counts : EMPTY;
}
