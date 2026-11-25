import { observeElementRect, useVirtualizer } from "@tanstack/react-virtual";
import { AlertCircle, HelpCircle } from "lucide-react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
} from "react";
import { commands } from "../generated/bindings";
import {
  deleteActionFor,
  restartActionFor,
  scaleActionFor,
  useActionsStore,
  type PendingAction,
} from "../stores/actions";
import { clustersForQuery, useContextsStore } from "../stores/contexts";
import { type PaneId, useLayoutStore } from "../stores/layout";
import {
  PAGE_SIZE,
  ROW_STATUSES,
  type Cell,
  type QuerySpec,
  type Row,
  type RowStatus,
  sortFieldsEqual,
  usePodsStore,
  useSecondaryPodsStore,
} from "../stores/pods";
import { useResourceDetailStore } from "../stores/resourceDetail";
import { ROW_HEIGHTS, useSettingsStore } from "../stores/settings";
import { hiddenColumnsOf, useTableColumnsStore } from "../stores/tableColumns";
import { ActionConfirmDialog } from "./ActionConfirmDialog";
import { ResourceDrawer } from "./ResourceDrawer";
import { ToolbarPicker } from "./ToolbarPicker";
import { UsageCell } from "./UsageCell";
import { formatAge } from "./age";
import { useListStatus } from "./useListStatus";
import { STATUS_LABEL, STATUS_TONE, statusPickerLabel } from "./statusTone";
import {
  type ResourceColumn,
  columnsForKind,
  gridTemplate,
  hasKnownLayout,
  kindNoun,
  kindPlural,
} from "./resourceColumns";

const DEFAULT_VIEWPORT_WIDTH = 1000;
const DEFAULT_VIEWPORT_HEIGHT = 560;
/** How long the action success / notice line stays in the toolbar. */
const ACTION_STATUS_MS = 6000;

/** How often relative ages ("3m") are recomputed. */
const AGE_TICK_MS = 30_000;

function formatCell(cell: Cell | undefined, now: number): React.ReactNode {
  if (!cell) return "—";
  switch (cell.type) {
    case "ready":
      return `${String(cell.value.ready)}/${String(cell.value.total)}`;
    case "status":
      return cell.value.label || "—";
    case "int":
      return String(cell.value);
    case "text":
      return cell.value || "—";
    case "timestamp":
      return cell.value ? formatAge(cell.value, now) : "—";
    case "usage":
      return <UsageCell cell={cell.value} />;
  }
}

function renderCell(row: Row, col: ResourceColumn, now: number) {
  const { source } = col;
  if (source.type === "cluster") {
    const colour = useContextsStore.getState().settingsByContext[row.cluster]?.colour;
    return (
      <div key={col.key} role="cell" className="pod-table__cell">
        <span
          className="cluster-badge"
          style={colour ? ({ "--ctx": colour } as CSSProperties) : undefined}
          title={row.cluster}
        >
          {row.cluster}
        </span>
      </div>
    );
  }
  if (source.type === "namespace") {
    return (
      <div key={col.key} role="cell" className="pod-table__cell">
        {row.namespace ?? "—"}
      </div>
    );
  }
  if (source.type === "name") {
    return (
      <div key={col.key} role="cell" className="pod-table__cell pod-table__cell--name">
        {row.name}
      </div>
    );
  }
  const cell = row.cells[source.index];
  if (cell?.type === "status") {
    const text = cell.value.label || row.statusReason || "Unknown";
    return (
      <div
        key={col.key}
        role="cell"
        className="pod-table__cell pod-table__status"
        data-status={cell.value.phase}
      >
        <span className={`pill ${STATUS_TONE[cell.value.phase]}`} title={text}>
          {text}
        </span>
      </div>
    );
  }
  if (cell?.type === "int" && col.hotAt !== undefined && cell.value >= col.hotAt) {
    return (
      <div key={col.key} role="cell" className="pod-table__cell pod-table__cell--mono">
        <span className="restarts hot">{String(cell.value)}</span>
      </div>
    );
  }
  return (
    <div
      key={col.key}
      role="cell"
      className={col.mono ? "pod-table__cell pod-table__cell--mono" : "pod-table__cell"}
      {...(cell?.type === "timestamp" && cell.value ? { title: cell.value } : {})}
    >
      {formatCell(cell, now)}
    </div>
  );
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

// Another modal (command palette, diagnostics, confirm dialogs) owns the keyboard while open.
function isOtherModalOpen(): boolean {
  if (useLayoutStore.getState().paletteOpen) return true;
  return document.querySelector('[role="dialog"][aria-modal="true"]') !== null;
}

// Enter opens the drawer only from a row, the table container or the page body, never from
// buttons or inputs. The row opened is always the selected one (focus and selection are kept
// in sync), so it can never differ from the highlighted row.
function isEnterTarget(target: EventTarget | null, container: HTMLElement | null): boolean {
  if (!(target instanceof HTMLElement)) return true;
  if (target.getAttribute("role") === "row") return true;
  return target === container || target === document.body || target === document.documentElement;
}

function isInsideDrawer(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(".resource-drawer") !== null;
}

function isInsideToolbar(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(".pod-table__toolbar") !== null;
}

function isInsideActionDialog(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(".action-confirm-dialog") !== null;
}

/**
 * `s` / `r` on a pod: pods have no replicas of their own and the table has no clean owner
 * (the Row carries no owner refs, and a pod's owner is usually a ReplicaSet, not the
 * Deployment). They therefore apply to workload rows only and say so for pods.
 */
function workloadOnlyNotice(verb: "Scale" | "Restart", row: Row): string {
  const kinds = verb === "Scale" ? "Deployments and StatefulSets" : "Deployments, StatefulSets and DaemonSets";
  const hint = row.kind === "Pod" ? " Use its workload instead." : "";
  return `${verb} works on ${kinds}; ${row.kind} ${row.name} cannot be ${
    verb === "Scale" ? "scaled" : "restarted"
  } directly.${hint}`;
}

function findRenderedRow(
  container: HTMLElement | null,
  match: { uid: string } | { index: number },
): HTMLElement | null {
  if (!container) return null;
  const rows = container.querySelectorAll<HTMLElement>('[role="row"][data-uid]');
  for (const el of rows) {
    if ("uid" in match ? el.dataset.uid === match.uid : el.dataset.index === String(match.index)) {
      return el;
    }
  }
  return null;
}

type PendingFocus = { uid: string } | { index: number };

/** Runs `fn` after the next two animation frames (after the virtualizer's commit); returns a cancel. */
function afterFrames(fn: () => void): () => void {
  if (typeof requestAnimationFrame !== "function") {
    const t = setTimeout(fn, 0);
    return () => {
      clearTimeout(t);
    };
  }
  let inner = 0;
  const outer = requestAnimationFrame(() => {
    inner = requestAnimationFrame(fn);
  });
  return () => {
    cancelAnimationFrame(outer);
    cancelAnimationFrame(inner);
  };
}

/**
 * The selected row: its index plus the uid of the row it was anchored to (null if unloaded),
 * and the query the anchor belongs to. A new filter/sort/kind/cluster set drops the anchor and
 * keeps only the index, as before anchoring existed.
 */
interface Selection {
  index: number;
  uid: string | null;
  query: string;
}

/** What defines the row set; `offset`/`limit` only pick a window of it. */
function queryIdentity(spec: QuerySpec): string {
  return JSON.stringify([
    spec.clusters,
    spec.kinds,
    spec.namespaces,
    spec.textFilter,
    spec.sort,
    spec.statuses,
  ]);
}

const formatCount = (n: number) => n.toLocaleString("en-US");

/**
 * Re-anchors `sel` to the current rows. Rows shift under a fixed index (batched initial list,
 * watch inserts/deletes), so the selection follows its row's uid while that row is loaded.
 * Otherwise the index stays (clamped to `total`) and adopts whatever row is there now; no
 * scrolling and no extra fetches. Idempotent: resolving a resolved selection returns it as is.
 */
function resolveSelection(
  rows: Record<number, Row>,
  total: number,
  query: string,
  sel: Selection,
): Selection {
  if (sel.uid !== null && sel.query === query) {
    if (rows[sel.index]?.uid === sel.uid) return sel;
    for (const [key, row] of Object.entries(rows)) {
      if (row.uid === sel.uid) return { index: Number(key), uid: sel.uid, query };
    }
  }
  const index = Math.min(Math.max(0, sel.index), Math.max(0, total - 1));
  const uid = rows[index]?.uid ?? null;
  return index === sel.index && uid === sel.uid && query === sel.query
    ? sel
    : { index, uid, query };
}

/**
 * Remembers the table's filters where its pane keeps its state: the active tab for the primary
 * pane, the layout store's secondary fields for the secondary one. The query itself is
 * re-issued by the caller; the secondary pane's view is never keyed by these values.
 */
function saveFilters(pane: PaneId, patch: { textFilter?: string; namespace?: string | null }) {
  const layout = useLayoutStore.getState();
  if (pane === "secondary") {
    if (patch.textFilter !== undefined) layout.setSecondaryTextFilter(patch.textFilter);
    if (patch.namespace !== undefined) layout.setSecondaryNamespace(patch.namespace);
  } else {
    layout.updateTabState(layout.activeTabId, patch);
  }
}

export interface PodTableProps {
  /** Resource kind listed by this table (the active tab's `kind`). Defaults to Pods. */
  kind?: string;
  /** Text filter restored when the table mounts for a tab (the tab's saved filter). */
  initialTextFilter?: string;
  /** Namespace filter restored on mount; `null` clears it, `undefined` keeps the current one. */
  initialNamespace?: string | null;
  /** Split pane showing the table; picks its own query store, columns, drawer and dialogs. */
  paneId?: PaneId;
  /** Clusters to query instead of the sidebar's selection (a pane pinned to one cluster). */
  clusters?: string[];
}

/** Virtualised resource table for one kind; named for its origin as the pods table. */
export function PodTable({
  kind = "Pod",
  initialTextFilter,
  initialNamespace,
  paneId = "primary",
  clusters: propClusters,
}: PodTableProps = {}) {
  const podsStore = paneId === "secondary" ? useSecondaryPodsStore : usePodsStore;
  const containerRef = useRef<HTMLDivElement | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const filterInputRef = useRef<HTMLInputElement | null>(null);
  const [selection, setSelection] = useState<Selection>({ index: 0, uid: null, query: "" });
  // The drawer store is shared by both panes; this table only shows a drawer opened in it.
  const drawerRow = useResourceDetailStore((s) => (s.ownerPane === paneId ? s.target : null));
  const drawerDetail = useResourceDetailStore((s) => (s.ownerPane === paneId ? s.detail : null));
  const drawerLoading = useResourceDetailStore((s) => s.ownerPane === paneId && s.loading);
  const drawerError = useResourceDetailStore((s) => (s.ownerPane === paneId ? s.error : null));
  // Mirrors `selection` synchronously so key handlers never read a stale value. Handlers still
  // re-anchor it against the store's current rows (`resolveSelection`) before using it.
  const selectedRef = useRef<Selection>({ index: 0, uid: null, query: "" });
  // Row to focus once it is rendered (keyboard navigation and focus restore after closing).
  const pendingFocusRef = useRef<PendingFocus | null>(null);
  const actionSuccess = useActionsStore((s) => s.lastSuccess);
  const [actionNotice, setActionNotice] = useState<string | null>(null);
  const [filterError, setFilterError] = useState<string | null>(null);
  const [showSyntaxHint, setShowSyntaxHint] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => {
      setNow(Date.now());
    }, AGE_TICK_MS);
    return () => {
      clearInterval(timer);
    };
  }, []);

  const querySpec = podsStore((s) => s.querySpec);
  const total = podsStore((s) => s.total);
  const rowsByIndex = podsStore((s) => s.rowsByIndex);
  const error = podsStore((s) => s.error);
  const setTextFilter = podsStore((s) => s.setTextFilter);
  const setNamespaceFilter = podsStore((s) => s.setNamespaceFilter);
  const setStatusFilter = podsStore((s) => s.setStatusFilter);
  const statusCounts = podsStore((s) => s.statusCounts);
  const toggleSort = podsStore((s) => s.toggleSort);
  const ensureRange = podsStore((s) => s.ensureRange);
  const loading = podsStore((s) => s.loading);
  const setClusters = podsStore((s) => s.setClusters);
  const setKind = podsStore((s) => s.setKind);

  useEffect(() => {
    const text = querySpec.textFilter;
    if (!text || text.trim() === "") {
      setFilterError(null);
      return;
    }
    let cancelled = false;
    new Promise<string | null>((resolve) => {
      resolve(commands.validateFilterQuery(text));
    })
      .then((err) => {
        if (!cancelled) setFilterError(err);
      })
      .catch(() => {
        if (!cancelled) setFilterError(null);
      });
    return () => {
      cancelled = true;
    };
  }, [querySpec.textFilter]);

  // The tab's kind drives `querySpec.kinds`. Runs before the clusters effect so a remount for
  // another kind never reopens a live subscription for the previous kind first. The saved text
  // and namespace filters are applied in the same requery.
  const initialFilterRef = useRef(initialTextFilter);
  const initialNamespacesRef = useRef(
    initialNamespace === undefined ? undefined : initialNamespace === null ? [] : [initialNamespace],
  );
  useEffect(() => {
    void setKind(kind, initialFilterRef.current, initialNamespacesRef.current);
  }, [setKind, kind]);
  // The query follows the sidebar: cluster group / multi-selection, else the selected context.
  // Selected as a joined string so unrelated contexts-store updates keep the same value.
  const contextClustersKey = useContextsStore((s) => clustersForQuery(s).join("\n"));
  const contextIsMultiCluster = useContextsStore((s) => clustersForQuery(s).length > 1);
  const clustersKey = propClusters ? propClusters.join("\n") : contextClustersKey;
  const isMultiCluster = propClusters ? propClusters.length > 1 : contextIsMultiCluster;
  // Settings changes (e.g. cluster colours) trigger re-render of cluster badges.
  useContextsStore((s) => s.settingsByContext);
  useEffect(() => {
    void setClusters(clustersKey === "" ? [] : clustersKey.split("\n"));
  }, [setClusters, clustersKey]);
  useEffect(
    () => () => {
      podsStore.getState().stopLive();
    },
    [podsStore],
  );
  // The drawer belongs to this table; leaving the view closes it and drops in-flight fetches.
  useEffect(
    () => () => {
      const drawer = useResourceDetailStore.getState();
      if (drawer.target !== null && drawer.ownerPane === paneId) drawer.close();
    },
    [paneId],
  );
  // Pending actions and the palette's selection belong to this table, too.
  useEffect(
    () => () => {
      const actions = useActionsStore.getState();
      actions.clearSelection(paneId);
      if (actions.pendingPane === paneId) actions.cancel();
    },
    [paneId],
  );
  useEffect(() => {
    if (actionSuccess === null) return;
    const timer = setTimeout(() => {
      useActionsStore.getState().clearSuccess();
    }, ACTION_STATUS_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [actionSuccess]);
  useEffect(() => {
    if (actionNotice === null) return;
    const timer = setTimeout(() => {
      setActionNotice(null);
    }, ACTION_STATUS_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [actionNotice]);

  const {
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
  } = useListStatus(kind, podsStore);

  const noun = kindNoun(kind);
  // Columns follow the kind; kinds without a known layout follow the cells of a returned row.
  const sampleRow = hasKnownLayout(kind) ? undefined : Object.values(rowsByIndex)[0];
  const sampleShape = sampleRow?.cells.map((c) => c.type).join(",") ?? "";
  const baseColumns = useMemo(
    () => columnsForKind(kind, sampleRow),
    // Only the cell shape of the sample matters, not the row object.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [kind, sampleShape],
  );
  const allColumns = useMemo(() => {
    if (!isMultiCluster) return baseColumns;
    const clusterCol = baseColumns.find((c) => c.key === "cluster");
    if (!clusterCol) return baseColumns;
    const pinnedClusterCol: ResourceColumn = { ...clusterCol, pinned: true };
    const rest = baseColumns.filter((c) => c.key !== "cluster");
    return [pinnedClusterCol, ...rest];
  }, [baseColumns, isMultiCluster]);
  // Column picker: hidden columns per kind, by label; pinned columns (name, status) stay.
  const hiddenColumns = useTableColumnsStore((s) => hiddenColumnsOf(s, kind, paneId));
  const toggleColumn = useTableColumnsStore((s) => s.toggleColumn);
  const showAllColumns = useTableColumnsStore((s) => s.showAll);
  const columns = useMemo(
    () => allColumns.filter((c) => c.pinned === true || !hiddenColumns.includes(c.label)),
    [allColumns, hiddenColumns],
  );
  const anyColumnHidden = allColumns.some((c) => c.pinned !== true && hiddenColumns.includes(c.label));

  const statusFilter = querySpec.statuses;

  // Row height follows the density setting ("Toggle compact rows" in the palette).
  const density = useSettingsStore((s) => s.density);
  const rowHeight = ROW_HEIGHTS[density];

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: total,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: 10,
    initialRect: {
      width: DEFAULT_VIEWPORT_WIDTH,
      height: DEFAULT_VIEWPORT_HEIGHT,
    },
    observeElementRect: (instance, cb) =>
      observeElementRect(instance, (rect) => {
        cb({
          width: rect.width > 0 ? rect.width : DEFAULT_VIEWPORT_WIDTH,
          height: rect.height > 0 ? rect.height : DEFAULT_VIEWPORT_HEIGHT,
        });
      }),
  });

  // Cached sizes still hold the old height after a density change.
  useLayoutEffect(() => {
    virtualizer.measure();
  }, [virtualizer, rowHeight]);

  const virtualItems = virtualizer.getVirtualItems();
  const firstIndex = virtualItems[0]?.index;
  const lastIndex = virtualItems[virtualItems.length - 1]?.index;

  useEffect(() => {
    void ensureRange(0, PAGE_SIZE - 1);
  }, [ensureRange]);

  useEffect(() => {
    if (firstIndex !== undefined && lastIndex !== undefined) {
      void ensureRange(firstIndex, lastIndex);
    }
  }, [ensureRange, firstIndex, lastIndex]);

  // Anchors the selection to the row now at `index` (keyboard moves, clicks, focus).
  const selectRow = useCallback(
    (index: number) => {
      const { rowsByIndex: rows, querySpec: spec } = podsStore.getState();
      const next: Selection = { index, uid: rows[index]?.uid ?? null, query: queryIdentity(spec) };
      selectedRef.current = next;
      setSelection(next);
    },
    [podsStore],
  );

  // Rows moved or changed under the selection: follow its row (or adopt the row now at the
  // clamped index). Adjusting state during render avoids a frame with the wrong row selected.
  const resolvedSelection = resolveSelection(
    rowsByIndex,
    total,
    queryIdentity(querySpec),
    selection,
  );
  if (resolvedSelection !== selection) {
    setSelection(resolvedSelection);
  }
  const selectedRowIndex = resolvedSelection.index;
  useLayoutEffect(() => {
    selectedRef.current = selection;
  }, [selection]);

  // Focuses the pending row if it is rendered; returns whether it was.
  const flushPendingFocus = useCallback((): boolean => {
    const pending = pendingFocusRef.current;
    if (!pending) return true;
    // Never steal focus from somewhere the user moved to meanwhile (an input, another panel).
    const active = document.activeElement;
    const focusIsFree =
      active === null ||
      active === document.body ||
      isInsideDrawer(active) ||
      isInsideActionDialog(active) ||
      (containerRef.current?.contains(active) === true && !isEditableTarget(active));
    if (!focusIsFree) {
      pendingFocusRef.current = null;
      return true;
    }
    const el = findRenderedRow(containerRef.current, pending);
    if (!el) return false;
    pendingFocusRef.current = null;
    el.focus();
    return true;
  }, []);

  // Roving focus follows a selection that moved with its row (rows are keyed by index, so the
  // focused element would otherwise show a different pod than the selected one).
  const selectedUid = resolvedSelection.uid;
  useEffect(() => {
    const active = document.activeElement;
    if (
      selectedUid !== null &&
      active instanceof HTMLElement &&
      active.getAttribute("role") === "row" &&
      containerRef.current?.contains(active) === true &&
      active.dataset.index !== String(selectedRowIndex)
    ) {
      const pending = { uid: selectedUid };
      pendingFocusRef.current = pending;
      // The row may now be off screen and never render; give up after a frame (like
      // `restoreFocusTo`) so focus doesn't jump to it whenever the user scrolls there.
      const cancel = afterFrames(() => {
        if (pendingFocusRef.current === pending && !flushPendingFocus()) {
          pendingFocusRef.current = null;
        }
      });
      return () => {
        cancel();
        // Superseded before the frames ran: never leave this one pending with no give-up.
        if (pendingFocusRef.current === pending) pendingFocusRef.current = null;
      };
    }
  }, [selectedRowIndex, selectedUid, flushPendingFocus]);

  // A row scrolled into view by the virtualizer is rendered on a later commit.
  useEffect(() => {
    flushPendingFocus();
  });

  const openDrawer = useCallback(
    (row: Row, index: number) => {
      selectRow(index);
      void useResourceDetailStore.getState().open(row, paneId);
    },
    [selectRow, paneId],
  );

  // Restores focus to the row with `uid`, matched by uid so a re-sorted or re-filtered table
  // never focuses a different pod. Falls back to the table container.
  const restoreFocusTo = useCallback((uid: string | null) => {
    const container = containerRef.current;
    if (uid === null) {
      container?.focus();
      return;
    }
    pendingFocusRef.current = { uid };
    if (flushPendingFocus()) return;
    const entry = Object.entries(podsStore.getState().rowsByIndex).find(
      ([, r]) => r.uid === uid,
    );
    if (!entry) {
      pendingFocusRef.current = null;
      container?.focus();
      return;
    }
    virtualizer.scrollToIndex(Number(entry[0]));
    const giveUp = () => {
      if (!flushPendingFocus()) {
        pendingFocusRef.current = null;
        container?.focus();
      }
    };
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => {
        requestAnimationFrame(giveUp);
      });
    } else {
      giveUp();
    }
  }, [flushPendingFocus, virtualizer, podsStore]);

  const handleCloseDrawer = useCallback(() => {
    const drawer = useResourceDetailStore.getState();
    if (drawer.target === null || drawer.ownerPane !== paneId) return;
    drawer.close();
    restoreFocusTo(drawer.target.uid);
  }, [restoreFocusTo, paneId]);

  const handleActionClosed = useCallback(
    (pending: PendingAction) => {
      restoreFocusTo(pending.originUid);
    },
    [restoreFocusTo],
  );

  // Keeps the command palette's "actions for the selected resource" in sync.
  const selectedRow = rowsByIndex[selectedRowIndex] ?? null;
  useEffect(() => {
    useActionsStore.getState().setSelection(selectedRow, paneId);
  }, [selectedRow, paneId]);

  const moveSelection = (index: number, target: EventTarget | null) => {
    selectRow(index);
    virtualizer.scrollToIndex(index);
    // Roving focus: when a row has focus, focus follows the selection.
    if (target instanceof HTMLElement && target.getAttribute("role") === "row") {
      pendingFocusRef.current = { index };
    }
  };

  const handleKeyNavigation = (e: {
    key: string;
    ctrlKey: boolean;
    shiftKey: boolean;
    metaKey: boolean;
    altKey: boolean;
    target: EventTarget | null;
    defaultPrevented: boolean;
    preventDefault: () => void;
  }) => {
    const ctrlD =
      e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "d";
    if ((e.ctrlKey || e.metaKey || e.altKey) && !ctrlD) return;
    if (e.defaultPrevented) return;
    if (isOtherModalOpen()) return;
    if (isEditableTarget(e.target)) return;
    // Keys pressed inside the open drawer belong to the drawer (it handles its own Escape).
    // With focus in the table, navigation, '/' and Enter keep working while the drawer is open.
    if (isInsideDrawer(e.target)) return;
    // Toolbar controls (filters, picker buttons and menus) own their keys: no table actions or
    // navigation from there.
    if (isInsideToolbar(e.target)) return;

    const { total: count, rowsByIndex: rows, querySpec: spec } = podsStore.getState();
    const last = Math.max(0, count - 1);
    const current = resolveSelection(rows, count, queryIdentity(spec), selectedRef.current).index;
    const currentRow = rows[current];

    if (ctrlD) {
      // k9s: ctrl-d deletes the selected resource (after confirmation). Always swallowed, so
      // the webview's own ctrl-d (bookmark) never fires from the table.
      e.preventDefault();
      if (currentRow) {
        setActionNotice(null);
        useActionsStore.getState().request(deleteActionFor(currentRow), paneId);
      }
    } else if (e.key === "s" || e.key === "r") {
      if (!currentRow) return;
      e.preventDefault();
      const pending =
        e.key === "s" ? scaleActionFor(currentRow) : restartActionFor(currentRow);
      if (pending) {
        setActionNotice(null);
        useActionsStore.getState().request(pending, paneId);
      } else {
        setActionNotice(workloadOnlyNotice(e.key === "s" ? "Scale" : "Restart", currentRow));
      }
    } else if (e.key === "Escape") {
      const drawer = useResourceDetailStore.getState();
      if (drawer.target !== null && drawer.ownerPane === paneId) {
        e.preventDefault();
        handleCloseDrawer();
      }
    } else if (e.key === "j" || e.key === "ArrowDown") {
      e.preventDefault();
      moveSelection(Math.min(last, current + 1), e.target);
    } else if (e.key === "k" || e.key === "ArrowUp") {
      e.preventDefault();
      moveSelection(Math.max(0, current - 1), e.target);
    } else if (e.key === "g") {
      e.preventDefault();
      moveSelection(0, e.target);
    } else if (e.key === "G") {
      e.preventDefault();
      moveSelection(last, e.target);
    } else if (e.key === "/") {
      e.preventDefault();
      filterInputRef.current?.focus();
    } else if (e.key === "Enter") {
      if (!isEnterTarget(e.target, containerRef.current)) return;
      const row = rows[current];
      if (row) {
        e.preventDefault();
        openDrawer(row, current);
      }
    }
  };

  // The window listener is registered once and always calls the latest handler.
  const keyHandlerRef = useRef(handleKeyNavigation);
  useEffect(() => {
    keyHandlerRef.current = handleKeyNavigation;
  });

  const onContainerKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    handleKeyNavigation(e);
  };

  useEffect(() => {
    const onWindowKeyDown = (e: globalThis.KeyboardEvent) => {
      // With two tables on screen, only the active pane's answers keys from outside both.
      const { splitMode, activePane } = useLayoutStore.getState();
      if (splitMode !== "none" && paneId !== activePane) return;
      if (
        containerRef.current &&
        e.target instanceof Node &&
        containerRef.current.contains(e.target)
      ) {
        return;
      }
      // Only keys with no other owner: the page itself or a focused (empty) pane. Toolbars,
      // buttons, selects and editors elsewhere keep their keys.
      if (
        e.target instanceof HTMLElement &&
        e.target !== document.body &&
        e.target !== document.documentElement &&
        !e.target.classList.contains("split-pane")
      ) {
        return;
      }
      keyHandlerRef.current(e);
    };
    window.addEventListener("keydown", onWindowKeyDown);
    return () => {
      window.removeEventListener("keydown", onWindowKeyDown);
    };
  }, [paneId]);

  const currentSort = querySpec.sort;

  return (
    <div
      ref={containerRef}
      className="pod-table-container"
      tabIndex={0}
      data-pane-id={paneId}
      onKeyDown={onContainerKeyDown}
    >
      <div className="pod-table-layout">
        <div className="pod-table__main">
          <div className="pod-table__toolbar">
            <div
              className={`tb-search${filterError ? " tb-search--invalid" : ""}`}
              onClick={(e) => {
                if ((e.target as HTMLElement).closest("button, .filter-syntax-popover")) return;
                filterInputRef.current?.focus();
              }}
            >
              <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
                <circle cx="11" cy="11" r="7" />
                <path d="m20 20-3.5-3.5" />
              </svg>
              <input
                ref={filterInputRef}
                type="search"
                className="tb-search__input"
                aria-label={`Filter ${noun}`}
                aria-keyshortcuts="/"
                placeholder={`Filter ${noun}…`}
                value={querySpec.textFilter ?? ""}
                onChange={(e) => {
                  const val = e.target.value;
                  saveFilters(paneId, { textFilter: val });
                  void setTextFilter(val);
                }}
              />
              {filterError ? (
                <span className="tb-search__error" title={filterError} role="alert">
                  <AlertCircle size={13} aria-hidden="true" />
                  <span className="tb-search__error-text">{filterError}</span>
                </span>
              ) : null}
              <kbd aria-hidden="true">/</kbd>
              <button
                type="button"
                className="tb-search__hint-btn"
                aria-label="Search syntax help"
                title="Search syntax help"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowSyntaxHint((v) => !v);
                }}
              >
                <HelpCircle size={13} aria-hidden="true" />
              </button>
              {showSyntaxHint ? (
                <div className="filter-syntax-popover" role="tooltip">
                  <div className="filter-syntax-popover__header">
                    <span>Search Syntax</span>
                    <button
                      type="button"
                      className="filter-syntax-popover__close"
                      aria-label="Close syntax help"
                      onClick={(e) => {
                        e.stopPropagation();
                        setShowSyntaxHint(false);
                      }}
                    >
                      ×
                    </button>
                  </div>
                  <div className="filter-syntax-popover__body">
                    <div><code>web AND api</code> – both terms match</div>
                    <div><code>web OR worker</code> – either term matches</div>
                    <div><code>NOT crash</code> or <code>!crash</code> – negation</div>
                    <div><code>app=backend</code> – label / annotation match</div>
                    <div><code>status:Running</code> – status match</div>
                    <div><code>is:near-limit</code> – resource near limit (≥80%)</div>
                    <div><code>/redis-\d+/</code> – regex match</div>
                    <div><code>(a OR b) AND c</code> – grouped expressions</div>
                  </div>
                </div>
              ) : null}
            </div>
            {kind === "Pod" && (
              <div
                className="pod-table__quick-filters"
                role="group"
                aria-label="Quick resource filters"
              >
                {[
                  { label: "Near limit", filter: "is:near-limit" },
                  { label: "Over request", filter: "is:over-request" },
                  { label: "Unused (<20%)", filter: "is:unused" },
                  { label: "Missing limits", filter: "is:missing-limits" },
                ].map((pill) => {
                  const currentFilter = querySpec.textFilter ?? "";
                  const active = currentFilter.includes(pill.filter);
                  return (
                    <button
                      key={pill.filter}
                      type="button"
                      className={`pod-table__quick-pill ${active ? "pod-table__quick-pill--active" : ""}`}
                      aria-pressed={active}
                      onClick={() => {
                        let nextFilter: string;
                        if (active) {
                          nextFilter = currentFilter
                            .replace(pill.filter, "")
                            .replace(/\s+/g, " ")
                            .trim();
                        } else if (currentFilter.trim() === "") {
                          nextFilter = pill.filter;
                        } else {
                          nextFilter = `${currentFilter} ${pill.filter}`.trim();
                        }
                        saveFilters(paneId, { textFilter: nextFilter });
                        void setTextFilter(nextFilter);
                      }}
                    >
                      {pill.label}
                    </button>
                  );
                })}
              </div>
            )}
            <label className="tb-field">
              <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
                <path d="M3 7l9-4 9 4-9 4z" />
                <path d="M3 12l9 4 9-4M3 17l9 4 9-4" />
              </svg>
              <input
                type="text"
                className="tb-field__input"
                aria-label="Namespace filter"
                placeholder="All namespaces"
                value={querySpec.namespaces.join(",")}
                onChange={(e) => {
                  const raw = e.target.value;
                  const namespaces = raw
                    .split(",")
                    .map((ns) => ns.trim())
                    .filter((ns) => ns.length > 0);
                  saveFilters(paneId, { namespace: namespaces[0] ?? null });
                  void setNamespaceFilter(namespaces);
                }}
              />
            </label>
            <ToolbarPicker
              name="Status filter"
              testId="status-picker"
              label={statusPickerLabel(statusFilter)}
              icon={
                <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M3 5h18l-7 8v6l-4-2v-4z" />
                </svg>
              }
              options={ROW_STATUSES.map((s) => ({
                key: s,
                label: STATUS_LABEL[s],
                checked: statusFilter.includes(s),
                tone: STATUS_TONE[s],
                // Counts over every filter except the status filter; hidden while not Ready.
                count: allReady && statusCounts !== null ? statusCounts[s] : null,
              }))}
              onToggle={(key) => {
                const s = key as RowStatus;
                void setStatusFilter(
                  statusFilter.includes(s)
                    ? statusFilter.filter((x) => x !== s)
                    : [...statusFilter, s],
                );
              }}
              reset={{
                label: "Any status",
                disabled: statusFilter.length === 0,
                onReset: () => {
                  void setStatusFilter([]);
                },
              }}
            />
            <ToolbarPicker
              name="Columns"
              testId="column-picker"
              label="Columns"
              icon={
                <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
                  <rect x="3" y="4" width="18" height="16" rx="2" />
                  <path d="M9 4v16M15 4v16" />
                </svg>
              }
              options={allColumns
                .filter((c) => c.pinned !== true)
                .map((c) => ({
                  key: c.label,
                  label: c.label,
                  checked: !hiddenColumns.includes(c.label),
                }))}
              onToggle={(key) => {
                toggleColumn(kind, key, paneId);
              }}
              reset={{
                label: "Show all",
                disabled: !anyColumnHidden,
                onReset: () => {
                  showAllColumns(kind, paneId);
                },
              }}
            />
            <div className="tb-summary" data-testid="pod-summary">
              <span data-testid="pod-total">
                <b>{formatCount(total)}</b>
                {` ${total === 1 ? kind.toLowerCase() : noun}`}
              </span>
              {summary !== null && (
                <>
                  <span data-testid="pod-failing">
                    <b className={summary.failing > 0 ? "e" : undefined}>
                      {formatCount(summary.failing)}
                    </b>
                    {" failing"}
                  </span>
                  <span data-testid="pod-pending">
                    <b>{formatCount(summary.pending)}</b>
                    {" pending"}
                  </span>
                </>
              )}
            </div>
          </div>

          {(actionSuccess !== null || actionNotice !== null) && (
            <p role="status" className="pod-table__action-status" data-testid="action-status">
              {actionNotice ?? actionSuccess}
            </p>
          )}

          {error !== null && (
            <p role="alert" className="error-state">
              {error}
            </p>
          )}

          {queried.length === 0 ? (
            <p role="status" className="pod-table__state" data-testid="pod-select-context">
              {`Select a context to list ${noun}`}
            </p>
          ) : (
            hasNotices && (
              <div className="pod-table__notices" data-testid="pod-cluster-notices">
                {restricted.map((r) => (
                  <p
                    key={`restricted\n${r.cluster}`}
                    role="status"
                    className="pod-table__notice pod-table__notice--scope"
                    data-testid="restricted-scope"
                    data-cluster={r.cluster}
                    title={r.detail}
                  >
                    <span className="pill warn">{r.label}</span>
                    <span className="visually-hidden"> {r.detail}</span>
                  </p>
                ))}
                {notices.map((n) => (
                  <p
                    key={`${n.cluster}\n${n.text}`}
                    role={n.role}
                    className={`pod-table__notice pod-table__notice--${n.role}`}
                    data-cluster={n.cluster}
                  >
                    {n.text}
                  </p>
                ))}
                {metricsForbidden && (
                  <p
                    role="alert"
                    className="pod-table__notice pod-table__notice--alert"
                    data-testid="metrics-notice"
                  >
                    No permission to read metrics
                  </p>
                )}
                {metricsNotInstalled && (
                  <p
                    role="status"
                    className="pod-table__notice pod-table__notice--status"
                    data-testid="metrics-notice"
                  >
                    metrics-server not installed — CPU and memory are unavailable
                  </p>
                )}
              </div>
            )
          )}

          {queried.length > 0 && progress !== null && (
            <p
              role="status"
              className="pod-table__notice pod-table__notice--progress"
              data-testid="pod-loading-progress"
            >
              {progress}
            </p>
          )}

          {queried.length > 0 &&
            notices.length === 0 &&
            progress === null &&
            !ipcFailed &&
            total === 0 &&
            !loading && (
            <p role="status" className="pod-table__state">
              {querySpec.textFilter !== null ||
              querySpec.namespaces.length > 0 ||
              querySpec.statuses.length > 0
                ? `No ${noun} match the filter`
                : `No ${noun}`}
            </p>
          )}

          <div
            className="pod-table"
            role="table"
            aria-label={kindPlural(kind)}
            aria-rowcount={total}
            data-kind={kind}
            data-density={density}
            style={{ ["--pod-grid-cols" as string]: gridTemplate(columns) }}
            hidden={queried.length === 0 || (total === 0 && !anyReady)}
          >
            <div className="pod-table__header" role="rowgroup">
              <div className="pod-table__header-row" role="row">
                {columns.map((col) => {
                  const sortField = col.sortField;
                  const isSorted =
                    sortField !== null &&
                    currentSort !== null &&
                    sortFieldsEqual(currentSort.field, sortField);
                  const direction = isSorted ? currentSort.direction : null;
                  const ariaSort =
                    direction === "asc"
                      ? "ascending"
                      : direction === "desc"
                        ? "descending"
                        : "none";

                  if (sortField === null) {
                    return (
                      <div key={col.key} role="columnheader" className="pod-table__th">
                        {col.label}
                      </div>
                    );
                  }

                  return (
                    <div
                      key={col.key}
                      role="columnheader"
                      aria-sort={ariaSort}
                      className="pod-table__th pod-table__th--sortable"
                      onClick={() => {
                        void toggleSort(sortField);
                      }}
                    >
                      <button type="button" className="pod-table__sort-btn">
                        <span>{col.label}</span>
                        {direction !== null && (
                          <span className="pod-table__sort-indicator" aria-hidden="true">
                            {direction === "asc" ? "▲" : "▼"}
                          </span>
                        )}
                      </button>
                    </div>
                  );
                })}
              </div>
            </div>

            <div ref={scrollRef} className="pod-table__viewport" role="rowgroup">
              <div
                className="pod-table__virtual-inner"
                style={{ height: `${String(virtualizer.getTotalSize())}px` }}
              >
                {virtualItems.map((virtualRow) => {
                  const row = rowsByIndex[virtualRow.index];
                  const isSelected = virtualRow.index === selectedRowIndex;
                  const rowClassName = [
                    "pod-table__row",
                    row ? "" : "pod-table__row--skeleton",
                    row?.status === "failed" ? "pod-table__row--failed" : "",
                    isSelected ? "pod-table__row--selected" : "",
                  ]
                    .filter(Boolean)
                    .join(" ");
                  return (
                    <div
                      key={virtualRow.key}
                      role="row"
                      tabIndex={row ? (isSelected ? 0 : -1) : undefined}
                      aria-rowindex={virtualRow.index + 1}
                      data-index={virtualRow.index}
                      data-uid={row?.uid}
                      data-selected={isSelected ? "true" : undefined}
                      data-testid={row ? "pod-row" : "pod-row-skeleton"}
                      className={rowClassName}
                      onFocus={(e) => {
                        if (e.target === e.currentTarget) selectRow(virtualRow.index);
                      }}
                      onClick={() => {
                        selectRow(virtualRow.index);
                      }}
                      onDoubleClick={() => {
                        selectRow(virtualRow.index);
                        if (row) {
                          openDrawer(row, virtualRow.index);
                        }
                      }}
                      style={{
                        height: `${String(virtualRow.size)}px`,
                        transform: `translateY(${String(virtualRow.start)}px)`,
                      }}
                    >
                      {row
                        ? columns.map((col) => renderCell(row, col, now))
                        : columns.map((col) => (
                            <div
                              key={col.key}
                              role="cell"
                              className="pod-table__cell pod-table__cell--skeleton"
                            >
                              <span className="pod-table__skeleton-bar" />
                            </div>
                          ))}
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
        </div>
        {drawerRow && (
          <ResourceDrawer
            key={drawerRow.uid}
            row={drawerRow}
            detail={drawerDetail}
            loading={drawerLoading}
            error={drawerError}
            onClose={handleCloseDrawer}
            pane={paneId}
          />
        )}
      </div>
      <ActionConfirmDialog onClosed={handleActionClosed} pane={paneId} />
    </div>
  );
}
