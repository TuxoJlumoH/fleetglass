import { Channel, isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import {
  commands,
  type Cell,
  type Cell_Deserialize,
  type Cell_Serialize,
  type PageDelta_Deserialize,
  type Page_Serialize,
  type QuerySpec_Serialize,
  type RowStatus,
  type Row_Deserialize,
  type Row_Serialize,
  type SortDirection,
  type SortField,
  type SortSpec,
  type StatusCounts,
  type StoreAccessState_Serialize,
  type StoreAccess_Serialize,
} from "../generated/bindings";

// IPC types are generated from crates/core/src/api (ADR 0001). The `_Serialize`
// variants are what the backend sends; a full spec is also a valid `_Deserialize`.
export type { Cell, RowStatus, SortDirection, SortField, SortSpec, StatusCounts };

/** Every row status, in the order the status picker lists them. */
export const ROW_STATUSES: readonly RowStatus[] = [
  "healthy",
  "pending",
  "degraded",
  "failed",
  "completed",
  "terminating",
  "unknown",
];
export type Row = Row_Serialize;
export type Page = Page_Serialize;
export type QuerySpec = QuerySpec_Serialize;
export type StoreAccess = StoreAccess_Serialize;
export type StoreAccessState = StoreAccessState_Serialize;

export type QueryFetcher = (spec: QuerySpec) => Promise<Page>;

export const PAGE_SIZE = 100;

export function sortFieldsEqual(a: SortField, b: SortField): boolean {
  if (a.type !== b.type) return false;
  if (a.type === "cell" && b.type === "cell") return a.index === b.index;
  if (a.type === "custom" && b.type === "custom") return a.index === b.index;
  return true;
}

export const defaultQuerySpec: QuerySpec = {
  // Filled from the contexts store via `setClusters` (see `clustersForQuery`).
  // Empty means "no clusters" for the backend, never "all clusters".
  clusters: [],
  kinds: ["Pod"],
  namespaces: [],
  labelSelector: null,
  textFilter: null,
  sort: { field: { type: "name" }, direction: "asc" },
  offset: 0,
  limit: PAGE_SIZE,
  statuses: [],
};

let activeQueryFetcher: QueryFetcher | null = null;
let queryGeneration = 0;
const inFlightOffsets = new Set<number>();
/** Last range the table asked for; refetched when a live delta changes the data. */
let lastRange: { start: number; end: number } | null = null;

interface LiveSubscription {
  generation: number;
  id: number | null;
  closed: boolean;
}
let live: LiveSubscription | null = null;

function errorMessage(err: unknown): string {
  return err instanceof Error
    ? err.message
    : typeof err === "object" && err !== null && "message" in err
      ? String(err.message)
      : String(err);
}

async function runQueryFetcher(spec: QuerySpec): Promise<Page | null> {
  if (activeQueryFetcher !== null) {
    return activeQueryFetcher(spec);
  }
  if (!isTauri()) {
    return null;
  }
  const res = await commands.queryPage(spec);
  if (res.status === "error") {
    throw new Error(res.error.message);
  }
  return res.data;
}

/** Closes the live subscription, if any. Safe to call while `subscribe_query` is in flight. */
export function stopLiveUpdates(): void {
  const sub = live;
  live = null;
  if (sub === null) return;
  sub.closed = true;
  usePodsStore.setState({ liveSubscribed: false });
  if (sub.id !== null) {
    void commands.unsubscribeQuery(sub.id).catch(() => undefined);
  }
}

/** True while a live subscription is open or being opened. */
function liveActive(): boolean {
  return live !== null && !live.closed;
}

/** Injected fetchers and older payloads may omit `statusCounts`; never render from `undefined`. */
function statusCountsOf(page: { statusCounts?: StatusCounts | null }): StatusCounts | null {
  return page.statusCounts ?? null;
}

function sameClusters(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((c, i) => c === b[i]);
}

export interface PodsState {
  querySpec: QuerySpec;
  total: number;
  version: number;
  rowsByIndex: Record<number, Row>;
  /** Per-(cluster, kind) watch state from the last page; drives the honest states. */
  access: StoreAccess[];
  /**
   * Rows per status from the last page, over every filter except `querySpec.statuses`
   * (the status picker shows what each choice would yield). Null until a page arrived.
   */
  statusCounts: StatusCounts | null;
  /** False until a page for the current spec arrived (clusters still connecting). */
  pageLoaded: boolean;
  /**
   * `Date.now()` of the last data change for the current spec: the first page, or a page / live
   * delta with a new `version`. Access-only deltas and re-fetches of the same version keep it.
   * Null before the first page.
   */
  lastUpdatedAt: number | null;
  /** True while a `subscribe_query` live subscription is open (or opening). */
  liveSubscribed: boolean;
  loading: boolean;
  error: string | null;
  setQueryFetcher: (fn: QueryFetcher | null) => void;
  applyPage: (page: Page) => void;
  fetchWindow: (offset: number, limit?: number) => Promise<void>;
  /** Points the query at `clusters`; re-queries and resubscribes when they change. */
  setClusters: (clusters: readonly string[]) => Promise<void>;
  setTextFilter: (text: string) => Promise<void>;
  /**
   * Points the query at one resource `kind` (the active tab's). When the kind changes, the sort
   * resets (cell indexes mean different columns per kind) and the text filter becomes
   * `textFilter`; same kind and filter is a no-op.
   */
  setKind: (kind: string, textFilter?: string) => Promise<void>;
  setNamespaceFilter: (namespaces: string[]) => Promise<void>;
  /** Shows only rows with one of `statuses` (empty = no status filter). */
  setStatusFilter: (statuses: readonly RowStatus[]) => Promise<void>;
  toggleSort: (field: SortField) => Promise<void>;
  /** Sorts by `field` in `direction` (the phone's sort picker; the table header toggles). */
  setSort: (field: SortField, direction: SortDirection) => Promise<void>;
  /**
   * Re-issues the current query (pull-to-refresh): reopens the live subscription and refetches
   * the windows last asked for. Cached rows stay on screen until the fresh pages replace them.
   */
  refresh: () => Promise<void>;
  ensureRange: (startIndex: number, endIndex: number) => Promise<void>;
}

export const initialPodsState = {
  querySpec: defaultQuerySpec,
  total: 0,
  version: 0,
  rowsByIndex: {} as Record<number, Row>,
  access: [] as StoreAccess[],
  statusCounts: null as StatusCounts | null,
  pageLoaded: false,
  lastUpdatedAt: null as number | null,
  liveSubscribed: false,
  loading: false,
  error: null as string | null,
};

export function setQueryFetcher(fn: QueryFetcher | null): void {
  activeQueryFetcher = fn;
  queryGeneration += 1;
  inFlightOffsets.clear();
}

export function resetPodsStore(): void {
  stopLiveUpdates();
  activeQueryFetcher = null;
  queryGeneration += 1;
  inFlightOffsets.clear();
  lastRange = null;
  usePodsStore.setState({
    ...initialPodsState,
    querySpec: {
      ...defaultQuerySpec,
      clusters: [...defaultQuerySpec.clusters],
      kinds: [...defaultQuerySpec.kinds],
      namespaces: [...defaultQuerySpec.namespaces],
      statuses: [...defaultQuerySpec.statuses],
      sort: defaultQuerySpec.sort
        ? { field: { ...defaultQuerySpec.sort.field }, direction: defaultQuerySpec.sort.direction }
        : null,
    },
    rowsByIndex: {},
    access: [],
  });
}

export const usePodsStore = create<PodsState>()((set, get) => {
  /**
   * Opens a `subscribe_query` channel for the first window of the current spec.
   * Every delta carries fresh access states (Loading → Ready, Forbidden, ...) and
   * signals that the data changed, so cached windows beyond the first are refetched.
   * Only used against the real backend; injected fetchers are pull-only.
   */
  function startLive(): void {
    stopLiveUpdates();
    const spec = get().querySpec;
    if (activeQueryFetcher !== null || !isTauri() || spec.clusters.length === 0) return;

    const generation = queryGeneration;
    const sub: LiveSubscription = { generation, id: null, closed: false };
    live = sub;
    set({ liveSubscribed: true });
    const channel = new Channel<PageDelta_Deserialize>();
    channel.onmessage = (delta) => {
      if (sub.closed || sub.generation !== queryGeneration) return;
      const { page } = delta;
      const changed = page.version !== get().version || !get().pageLoaded;
      set((state) => {
        const rowsByIndex: Record<number, Row> = {};
        for (const [key, row] of Object.entries(state.rowsByIndex)) {
          const index = Number(key);
          if (index < page.total) rowsByIndex[index] = row;
        }
        page.rows.forEach((row, i) => {
          rowsByIndex[page.offset + i] = normaliseRow(row);
        });
        return {
          total: page.total,
          version: page.version,
          access: page.access.map(normaliseAccess),
          statusCounts: statusCountsOf(page),
          pageLoaded: true,
          // Every access change sends a delta too; only a new version is new data.
          lastUpdatedAt: changed ? Date.now() : state.lastUpdatedAt,
          rowsByIndex,
          loading: false,
        };
      });
      // The delta only carries the first window; refresh the visible windows past it.
      // Their old rows stay on screen until the fresh ones arrive (no skeleton flicker).
      if (changed && lastRange !== null) {
        const first = Math.floor(lastRange.start / PAGE_SIZE) * PAGE_SIZE;
        for (let offset = first; offset <= lastRange.end; offset += PAGE_SIZE) {
          if (offset >= page.offset + page.limit && offset < page.total && !inFlightOffsets.has(offset)) {
            void get().fetchWindow(offset, PAGE_SIZE);
          }
        }
      }
    };

    const subscribeSpec: QuerySpec = { ...spec, offset: 0, limit: PAGE_SIZE };
    commands
      .subscribeQuery(subscribeSpec, channel)
      .then((res) => {
        if (res.status === "error") {
          if (!sub.closed) set({ error: res.error.message, liveSubscribed: false });
          return;
        }
        sub.id = res.data;
        if (sub.closed) {
          void commands.unsubscribeQuery(res.data).catch(() => undefined);
        }
      })
      .catch((err: unknown) => {
        if (!sub.closed) set({ error: errorMessage(err), liveSubscribed: false });
      });
  }

  /** Applies `patch` to the spec, drops cached rows, refetches the first window and resubscribes. */
  async function requery(patch: Partial<QuerySpec>): Promise<void> {
    queryGeneration += 1;
    inFlightOffsets.clear();
    const nextSpec: QuerySpec = { ...get().querySpec, ...patch, offset: 0 };
    set({
      querySpec: nextSpec,
      rowsByIndex: {},
      loading: nextSpec.clusters.length > 0,
      error: null,
    });
    if (patch.clusters !== undefined || patch.kinds !== undefined) {
      // Access states and totals belong to the previous clusters / kind.
      set({
        total: 0,
        version: 0,
        access: [],
        statusCounts: null,
        pageLoaded: false,
        lastUpdatedAt: null,
      });
    }
    startLive();
    await get().fetchWindow(0, nextSpec.limit);
  }

  return {
    ...initialPodsState,
    setQueryFetcher: (fn) => {
      setQueryFetcher(fn);
    },
    applyPage: (page) => {
      set((state) => {
        const nextRows: Record<number, Row> = { ...state.rowsByIndex };
        for (let i = 0; i < page.rows.length; i += 1) {
          const row = page.rows[i];
          if (row !== undefined) {
            nextRows[page.offset + i] = row;
          }
        }
        // A pull that raced a newer live delta must not roll the access states back
        // (e.g. to Loading after the subscription already reported Ready).
        const stale = liveActive() && state.pageLoaded && page.version < state.version;
        // Scroll windows of the same version are not new data.
        const changed = !stale && (!state.pageLoaded || page.version !== state.version);
        return {
          rowsByIndex: nextRows,
          pageLoaded: true,
          lastUpdatedAt: changed ? Date.now() : state.lastUpdatedAt,
          loading: false,
          error: null,
          ...(stale
            ? {}
            : {
                total: page.total,
                version: page.version,
                access: page.access,
                statusCounts: statusCountsOf(page),
              }),
        };
      });
    },
    fetchWindow: async (offset, limit) => {
      const currentSpec = get().querySpec;
      if (currentSpec.clusters.length === 0) {
        // Nothing selected: no IPC; the table says "Select a context".
        set({
          loading: false,
          total: 0,
          rowsByIndex: {},
          access: [],
          statusCounts: null,
          pageLoaded: false,
          lastUpdatedAt: null,
        });
        return;
      }
      const resolvedLimit = limit ?? currentSpec.limit;
      const nextSpec: QuerySpec = {
        ...currentSpec,
        offset,
        limit: resolvedLimit,
      };
      const gen = queryGeneration;
      inFlightOffsets.add(offset);
      set({ querySpec: nextSpec, loading: true, error: null });
      try {
        const page = await runQueryFetcher(nextSpec);
        if (gen !== queryGeneration) {
          return;
        }
        inFlightOffsets.delete(offset);
        if (page === null) {
          set({ loading: false });
          return;
        }
        get().applyPage(page);
      } catch (err: unknown) {
        if (gen !== queryGeneration) {
          return;
        }
        inFlightOffsets.delete(offset);
        set({ loading: false, error: errorMessage(err) });
      }
    },
    setClusters: async (clusters) => {
      const current = get().querySpec.clusters;
      const needsLive = activeQueryFetcher === null && isTauri() && clusters.length > 0;
      const { pageLoaded, loading } = get();
      if (sameClusters(current, clusters) && (pageLoaded || loading || clusters.length === 0)) {
        // Same selection (table remounted, StrictMode re-run): only reopen a closed subscription.
        if (needsLive && !liveActive()) startLive();
        return;
      }
      await requery({ clusters: [...clusters] });
    },
    setTextFilter: async (text) => {
      await requery({ textFilter: text === "" ? null : text });
    },
    setKind: async (kind, textFilter) => {
      const spec = get().querySpec;
      const sameKind = spec.kinds.length === 1 && spec.kinds[0] === kind;
      const filter = textFilter === undefined || textFilter === "" ? null : textFilter;
      if (sameKind && (textFilter === undefined || filter === spec.textFilter)) return;
      if (sameKind) {
        await requery({ textFilter: filter });
        return;
      }
      await requery({
        kinds: [kind],
        sort: defaultQuerySpec.sort,
        // A status filter left over from another tab would hide rows without saying why.
        statuses: [],
        ...(textFilter === undefined ? {} : { textFilter: filter }),
      });
    },
    setNamespaceFilter: async (namespaces) => {
      await requery({ namespaces: [...namespaces] });
    },
    setStatusFilter: async (statuses) => {
      // Picker order, deduplicated, so the same choice is always the same spec.
      const next = ROW_STATUSES.filter((s) => statuses.includes(s));
      const current = get().querySpec.statuses;
      if (next.length === current.length && next.every((s, i) => s === current[i])) return;
      await requery({ statuses: next });
    },
    toggleSort: async (field) => {
      const currentSort = get().querySpec.sort;
      const nextDirection: SortDirection =
        currentSort !== null &&
        sortFieldsEqual(currentSort.field, field) &&
        currentSort.direction === "asc"
          ? "desc"
          : "asc";
      await requery({ sort: { field, direction: nextDirection } });
    },
    setSort: async (field, direction) => {
      const currentSort = get().querySpec.sort;
      if (
        currentSort !== null &&
        sortFieldsEqual(currentSort.field, field) &&
        currentSort.direction === direction
      ) {
        return;
      }
      await requery({ sort: { field, direction } });
    },
    refresh: async () => {
      if (get().querySpec.clusters.length === 0) return;
      // Answers of the previous generation are dropped; the rows they would replace stay.
      queryGeneration += 1;
      inFlightOffsets.clear();
      set({ error: null });
      startLive();
      const range = lastRange ?? { start: 0, end: PAGE_SIZE - 1 };
      const offsets = new Set<number>([0]);
      for (
        let offset = Math.floor(range.start / PAGE_SIZE) * PAGE_SIZE;
        offset <= range.end;
        offset += PAGE_SIZE
      ) {
        offsets.add(offset);
      }
      await Promise.all([...offsets].map((offset) => get().fetchWindow(offset, PAGE_SIZE)));
    },
    ensureRange: async (startIndex, endIndex) => {
      const clampedStart = Math.max(0, startIndex);
      const clampedEnd = Math.max(clampedStart, endIndex);
      lastRange = { start: clampedStart, end: clampedEnd };
      const { rowsByIndex } = get();
      const offsetsToFetch: number[] = [];
      const seenOffsets = new Set<number>();

      for (let i = clampedStart; i <= clampedEnd; i += 1) {
        if (rowsByIndex[i] === undefined) {
          const alignedOffset = Math.floor(i / PAGE_SIZE) * PAGE_SIZE;
          if (!seenOffsets.has(alignedOffset) && !inFlightOffsets.has(alignedOffset)) {
            seenOffsets.add(alignedOffset);
            offsetsToFetch.push(alignedOffset);
          }
        }
      }

      if (offsetsToFetch.length === 0) {
        return;
      }

      await Promise.all(offsetsToFetch.map((offset) => get().fetchWindow(offset, PAGE_SIZE)));
    },
  };
});

function normaliseCell(cell: Cell_Deserialize): Cell_Serialize {
  if (cell.type === "usage") {
    return {
      type: "usage",
      value: {
        value: cell.value.value,
        formatted: cell.value.formatted,
        request: cell.value.request ?? null,
        limit: cell.value.limit ?? null,
      },
    };
  }
  return cell;
}

function normaliseRow(row: Row_Deserialize): Row_Serialize {
  return {
    ...row,
    namespace: row.namespace ?? null,
    cells: row.cells.map(normaliseCell),
    custom: row.custom.map(normaliseCell),
  };
}

/** Channel payloads use the `_Deserialize` shapes (optional `null`s); normalise to `_Serialize`. */
type AccessStateIn = PageDelta_Deserialize["page"]["access"][number]["access"];

function normaliseAccessState(access: AccessStateIn): StoreAccessState {
  return access.state === "forbidden"
    ? { ...access, namespace: access.namespace ?? null }
    : access.state === "error"
      ? { ...access, partialRows: access.partialRows ?? null }
      : access.state === "authExpired"
        ? {
            ...access,
            deviceCode: access.deviceCode ?? null,
            partialRows: access.partialRows ?? null,
          }
        : access;
}

function normaliseAccess(entry: PageDelta_Deserialize["page"]["access"][number]): StoreAccess {
  const { scope } = entry;
  return {
    cluster: entry.cluster,
    kind: entry.kind,
    access: normaliseAccessState(entry.access),
    rows: entry.rows ?? 0,
    metrics: entry.metrics ?? null,
    ...(entry.paused === true ? { paused: true } : {}),
    ...(scope !== null && scope !== undefined
      ? {
          scope: {
            ...scope,
            namespaces: scope.namespaces.map((n) => ({
              namespace: n.namespace,
              access: normaliseAccessState(n.access),
            })),
          },
        }
      : {}),
  };
}
