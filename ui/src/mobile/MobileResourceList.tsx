import { observeElementRect, useVirtualizer } from "@tanstack/react-virtual";
import { ArrowDown, ArrowUp } from "lucide-react";
import {
  type PointerEvent as ReactPointerEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { ActionConfirmDialog } from "../components/ActionConfirmDialog";
import { formatAge } from "../components/age";
import { AGE_TICK_MS } from "../components/overviewHooks";
import { ResourceDrawer } from "../components/ResourceDrawer";
import {
  columnsForKind,
  hasKnownLayout,
  kindNoun,
  kindPlural,
  type ResourceColumn,
} from "../components/resourceColumns";
import { RESTARTS_HOT, STATUS_LABEL, STATUS_TONE, statusPickerLabel } from "../components/statusTone";
import { ToolbarPicker } from "../components/ToolbarPicker";
import { UsageCell } from "../components/UsageCell";
import { useListStatus } from "../components/useListStatus";
import { useActionsStore } from "../stores/actions";
import { clustersForQuery, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import {
  type Cell,
  PAGE_SIZE,
  ROW_STATUSES,
  type Row,
  type RowStatus,
  type SortField,
  sortFieldsEqual,
  stopLiveUpdates,
  usePodsStore,
} from "../stores/pods";
import { useResourceDetailStore } from "../stores/resourceDetail";

/** Two-line row (mockup v0.3 `.row`, min 64 px); fixed so the virtualiser needs no measuring. */
export const MOBILE_ROW_HEIGHT = 68;
/** Pull distance (after damping) that triggers a refresh on release. */
export const PULL_THRESHOLD = 56;
const PULL_MAX = 88;
/** Finger travel is halved so the indicator lags behind it, like native pull-to-refresh. */
const PULL_DAMPING = 0.5;
const ACTION_STATUS_MS = 6000;
const DEFAULT_VIEWPORT = { width: 412, height: 640 };

const formatCount = (n: number) => n.toLocaleString("en-US");

/** Pod cell indexes (`resourceColumns.ts` KIND_LAYOUTS.pod, core `projection.rs`). */
const POD = { ready: 0, status: 1, restarts: 2, age: 7, cpu: 8, memory: 9 } as const;

function cellText(cell: Cell | undefined, now: number): string | null {
  if (!cell) return null;
  switch (cell.type) {
    case "ready":
      return `${String(cell.value.ready)}/${String(cell.value.total)}`;
    case "status":
      return cell.value.label || null;
    case "int":
      return String(cell.value);
    case "text":
      return cell.value || null;
    case "timestamp":
      return cell.value ? formatAge(cell.value, now) : null;
    case "usage":
      return cell.value.formatted || null;
  }
}

/**
 * The row's status pill, like the desktop Status column; null for kinds that have no status
 * (ConfigMap, Service, ...), where the desktop table shows none either.
 */
function statusOf(row: Row): { phase: RowStatus; text: string } | null {
  const cell = row.cells.find((c) => c.type === "status");
  if (cell?.type === "status") {
    return { phase: cell.value.phase, text: cell.value.label || row.statusReason || STATUS_LABEL[cell.value.phase] };
  }
  return row.statusReason ? { phase: row.status, text: row.statusReason } : null;
}

/** CPU / memory on line 2: the desktop usage bar, or "CPU —" when not measured (never a 0). */
function UsageItem({ label, cell }: { label: string; cell: Cell | undefined }) {
  const usage = cell?.type === "usage" ? cell.value : null;
  // Same "not measured" rule as `UsageCell` (no value yet, metrics-server missing).
  const measured = usage !== null && !!usage.formatted && usage.formatted !== "—";
  return (
    <span className="m-row__use" data-testid={`m-row-${label.toLowerCase()}`}>
      {measured ? (
        <UsageCell cell={usage} />
      ) : (
        <span className="use-empty" aria-label={`${label} not measured`}>
          {`${label === "Memory" ? "Mem" : label} —`}
        </span>
      )}
    </span>
  );
}

/** Line-2 columns for kinds other than Pod: the first useful cells, the age last. */
function secondaryColumns(columns: readonly ResourceColumn[]): ResourceColumn[] {
  const cells = columns.filter((c) => c.source.type === "cell");
  const statusCol = cells.find((c) => c.pinned === true);
  const ageCol = [...cells].reverse().find((c) => c.label === "Age");
  const rest = cells.filter((c) => c !== statusCol && c !== ageCol);
  return [...rest.slice(0, ageCol ? 2 : 3), ...(ageCol ? [ageCol] : [])];
}

function MetaItems({ items }: { items: readonly React.ReactNode[] }) {
  return (
    <span className="m-row__meta">
      {items.map((item, i) => (
        // Positional: the items of a row never reorder.
        <span key={i} className="m-row__meta-item">
          {i > 0 && <span aria-hidden="true">{" · "}</span>}
          {item}
        </span>
      ))}
    </span>
  );
}

function PodLine2({ row, now, showCluster }: { row: Row; now: number; showCluster: boolean }) {
  const cpu = row.cells[POD.cpu];
  const memory = row.cells[POD.memory];
  const restarts = row.cells[POD.restarts];
  const restartCount = restarts?.type === "int" ? restarts.value : null;
  const items: React.ReactNode[] = [
    ...(showCluster ? [row.cluster] : []),
    ...(row.namespace !== null ? [row.namespace] : []),
    ...[cellText(row.cells[POD.ready], now)].filter((t) => t !== null),
    ...(restartCount !== null
      ? [
          <span
            key="rs"
            className={restartCount >= RESTARTS_HOT ? "restarts hot" : "m-row__restarts"}
            data-testid="m-row-restarts"
            aria-label={`${String(restartCount)} restarts`}
          >
            ↻{restartCount}
          </span>,
        ]
      : []),
    ...[cellText(row.cells[POD.age], now)].filter((t) => t !== null),
  ];
  return (
    <span className="m-row__l2">
      <UsageItem label="CPU" cell={cpu} />
      <UsageItem label="Memory" cell={memory} />
      <MetaItems items={items} />
    </span>
  );
}

function GenericLine2({
  row,
  columns,
  now,
  showCluster,
}: {
  row: Row;
  columns: readonly ResourceColumn[];
  now: number;
  showCluster: boolean;
}) {
  const usage: React.ReactNode[] = [];
  const items: React.ReactNode[] = [
    ...(showCluster ? [row.cluster] : []),
    ...(row.namespace !== null ? [row.namespace] : []),
  ];
  for (const col of columns) {
    if (col.source.type !== "cell") continue;
    const cell = row.cells[col.source.index];
    if (cell?.type === "usage") {
      usage.push(<UsageItem key={col.key} label={col.label} cell={cell} />);
      continue;
    }
    const text = cellText(cell, now);
    if (text === null) continue;
    if (cell?.type === "int") {
      const hot = col.hotAt !== undefined && cell.value >= col.hotAt;
      items.push(
        <span key={col.key} className={hot ? "restarts hot" : undefined}>
          {`${col.label} ${text}`}
        </span>,
      );
    } else {
      items.push(text);
    }
  }
  return (
    <span className="m-row__l2">
      {usage}
      <MetaItems items={items} />
    </span>
  );
}

export interface MobileResourceListProps {
  kind: string;
  initialTextFilter?: string;
  /** Exec shell and port-forward in the detail (hidden on Android 0.3). */
  remoteOps?: boolean;
}

interface SortOption {
  key: string;
  label: string;
  field: SortField;
}

function sortKey(field: SortField): string {
  return field.type === "cell" || field.type === "custom"
    ? `${field.type}:${String(field.index)}`
    : field.type;
}

/**
 * Phone resource list (03-03b, mockups v0.3 `2-pods-dark` / `5-pods-light`): the desktop
 * table's query, notices and actions, laid out as virtualised two-line rows. Same windowed
 * store as `PodTable`; only the rows the screen shows are requested.
 */
export function MobileResourceList({ kind, initialTextFilter, remoteOps = true }: MobileResourceListProps) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => {
      setNow(Date.now());
    }, AGE_TICK_MS);
    return () => {
      clearInterval(timer);
    };
  }, []);

  const querySpec = usePodsStore((s) => s.querySpec);
  const total = usePodsStore((s) => s.total);
  const rowsByIndex = usePodsStore((s) => s.rowsByIndex);
  const error = usePodsStore((s) => s.error);
  const loading = usePodsStore((s) => s.loading);
  const statusCounts = usePodsStore((s) => s.statusCounts);
  const ensureRange = usePodsStore((s) => s.ensureRange);

  // Same wiring order as PodTable: kind first, then clusters, so no subscription for a stale kind.
  const initialFilterRef = useRef(initialTextFilter);
  useEffect(() => {
    void usePodsStore.getState().setKind(kind, initialFilterRef.current);
  }, [kind]);
  const clustersKey = useContextsStore((s) => clustersForQuery(s).join("\n"));
  useEffect(() => {
    void usePodsStore.getState().setClusters(clustersKey === "" ? [] : clustersKey.split("\n"));
  }, [clustersKey]);
  useEffect(() => stopLiveUpdates, []);
  useEffect(
    () => () => {
      useResourceDetailStore.getState().close();
      const actions = useActionsStore.getState();
      actions.setSelection(null);
      actions.cancel();
    },
    [],
  );

  const status = useListStatus(kind);
  const { queried, notices, metricsForbidden, metricsNotInstalled, hasNotices, progress, ipcFailed } =
    status;
  const statusFilter = querySpec.statuses;
  const noun = kindNoun(kind);
  const showCluster = queried.length > 1;
  const isPod = kind === "Pod";

  const sampleRow = hasKnownLayout(kind) ? undefined : Object.values(rowsByIndex)[0];
  const sampleShape = sampleRow?.cells.map((c) => c.type).join(",") ?? "";
  const columns = useMemo(
    () => columnsForKind(kind, sampleRow),
    // Only the cell shape of the sample matters, not the row object.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [kind, sampleShape],
  );
  const line2Columns = useMemo(() => secondaryColumns(columns), [columns]);
  const sortOptions = useMemo<SortOption[]>(
    () =>
      columns.flatMap((c) =>
        c.sortField === null ? [] : [{ key: sortKey(c.sortField), label: c.label, field: c.sortField }],
      ),
    [columns],
  );
  const currentSort = querySpec.sort;
  const currentSortKey =
    currentSort === null
      ? ""
      : (sortOptions.find((o) => sortFieldsEqual(o.field, currentSort.field))?.key ?? "");

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: total,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => MOBILE_ROW_HEIGHT,
    overscan: 6,
    initialRect: DEFAULT_VIEWPORT,
    observeElementRect: (instance, cb) =>
      observeElementRect(instance, (rect) => {
        cb({
          width: rect.width > 0 ? rect.width : DEFAULT_VIEWPORT.width,
          height: rect.height > 0 ? rect.height : DEFAULT_VIEWPORT.height,
        });
      }),
  });
  const virtualItems = virtualizer.getVirtualItems();
  const firstIndex = virtualItems[0]?.index;
  const lastIndex = virtualItems[virtualItems.length - 1]?.index;
  useEffect(() => {
    void ensureRange(0, PAGE_SIZE - 1);
  }, [ensureRange]);
  useEffect(() => {
    if (firstIndex !== undefined && lastIndex !== undefined) void ensureRange(firstIndex, lastIndex);
  }, [ensureRange, firstIndex, lastIndex]);

  // Detail: full screen over the list; back (gesture or arrow) closes it, focus returns to the row.
  const detailRow = useResourceDetailStore((s) => s.target);
  const detail = useResourceDetailStore((s) => s.detail);
  const detailLoading = useResourceDetailStore((s) => s.loading);
  const detailError = useResourceDetailStore((s) => s.error);
  const originRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    // The palette's "actions for the selected resource" follow the open detail.
    useActionsStore.getState().setSelection(detailRow);
  }, [detailRow]);
  const openDetail = useCallback((row: Row, from: HTMLElement) => {
    originRef.current = from;
    void useResourceDetailStore.getState().open(row);
  }, []);
  const closeDetail = useCallback(() => {
    useResourceDetailStore.getState().close();
    const from = originRef.current;
    originRef.current = null;
    if (from?.isConnected) from.focus();
  }, []);

  const actionSuccess = useActionsStore((s) => s.lastSuccess);
  useEffect(() => {
    if (actionSuccess === null) return;
    const timer = setTimeout(() => {
      useActionsStore.getState().clearSuccess();
    }, ACTION_STATUS_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [actionSuccess]);

  // Pull-to-refresh: a downward drag that starts with the list at the top. While at the top the
  // list only allows upward panning (`touch-action: pan-down`), so the WebView hands the downward
  // drag to these pointer handlers instead of cancelling them for its own overscroll.
  const [atTop, setAtTop] = useState(true);
  const [pull, setPull] = useState(0);
  const [refreshing, setRefreshing] = useState(false);
  const dragRef = useRef<{ id: number; startY: number; pulled: boolean } | null>(null);
  const suppressClickRef = useRef(false);
  const canPull = queried.length > 0 && !refreshing;

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!canPull || !e.isPrimary || e.button !== 0) return;
    if ((scrollRef.current?.scrollTop ?? 0) > 0) return;
    dragRef.current = { id: e.pointerId, startY: e.clientY, pulled: false };
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (drag?.id !== e.pointerId) return;
    const dy = e.clientY - drag.startY;
    if (dy <= 0 || (scrollRef.current?.scrollTop ?? 0) > 0) {
      if (drag.pulled) setPull(0);
      drag.pulled = false;
      return;
    }
    if (!drag.pulled && dy > 6) {
      drag.pulled = true;
      // jsdom has no pointer capture; real WebViews do.
      if (typeof e.currentTarget.setPointerCapture === "function") e.currentTarget.setPointerCapture(e.pointerId);
    }
    if (drag.pulled) setPull(Math.min(PULL_MAX, dy * PULL_DAMPING));
  };
  const endPull = (e: ReactPointerEvent<HTMLDivElement>, cancelled: boolean) => {
    const drag = dragRef.current;
    if (drag?.id !== e.pointerId) return;
    dragRef.current = null;
    const distance = pull;
    setPull(0);
    if (!drag.pulled) return;
    // The release of a drag is not a tap on the row under the finger.
    suppressClickRef.current = true;
    setTimeout(() => {
      suppressClickRef.current = false;
    }, 0);
    if (cancelled || distance < PULL_THRESHOLD) return;
    setRefreshing(true);
    void usePodsStore
      .getState()
      .refresh()
      .finally(() => {
        setRefreshing(false);
      });
  };

  const ptrLabel = refreshing
    ? "Refreshing…"
    : pull >= PULL_THRESHOLD
      ? "Release to refresh"
      : "Pull to refresh";
  const ptrHeight = refreshing ? PULL_THRESHOLD * 0.75 : pull;

  const listHidden = queried.length === 0 || (total === 0 && !status.anyReady);

  return (
    <div className="m-list" data-testid="m-list" data-kind={kind}>
      <div className="m-titlebar">
        <h1 className="m-titlebar__t">{kindPlural(kind)}</h1>
        {queried.length > 0 && (
          <span className="m-titlebar__s m-list__clusters">
            {queried.length === 1 ? queried[0] : `${String(queried.length)} clusters`}
          </span>
        )}
      </div>
      <div className="m-list__toolbar">
        <label className="tb-search m-list__search">
          <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="11" cy="11" r="7" />
            <path d="m20 20-3.5-3.5" />
          </svg>
          <input
            type="search"
            className="tb-search__input"
            aria-label={`Filter ${noun}`}
            placeholder={`Filter ${noun}…`}
            value={querySpec.textFilter ?? ""}
            onChange={(e) => {
              const val = e.target.value;
              const { activeTabId, updateTabState } = useLayoutStore.getState();
              updateTabState(activeTabId, { textFilter: val });
              void usePodsStore.getState().setTextFilter(val);
            }}
          />
        </label>
        <label className="tb-field m-list__ns">
          <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M3 7l9-4 9 4-9 4z" />
            <path d="M3 12l9 4 9-4M3 17l9 4 9-4" />
          </svg>
          <input
            type="text"
            className="tb-field__input"
            aria-label="Namespace filter"
            placeholder="All namespaces"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            value={querySpec.namespaces.join(",")}
            onChange={(e) => {
              const namespaces = e.target.value
                .split(",")
                .map((ns) => ns.trim())
                .filter((ns) => ns.length > 0);
              const { activeTabId, updateTabState } = useLayoutStore.getState();
              updateTabState(activeTabId, { namespace: namespaces[0] ?? null });
              void usePodsStore.getState().setNamespaceFilter(namespaces);
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
            count: status.allReady && statusCounts !== null ? statusCounts[s] : null,
          }))}
          onToggle={(key) => {
            const s = key as RowStatus;
            void usePodsStore
              .getState()
              .setStatusFilter(
                statusFilter.includes(s) ? statusFilter.filter((x) => x !== s) : [...statusFilter, s],
              );
          }}
          reset={{
            label: "Any status",
            disabled: statusFilter.length === 0,
            onReset: () => {
              void usePodsStore.getState().setStatusFilter([]);
            },
          }}
        />
        <div className="m-list__summary">
          <div className="tb-summary" data-testid="pod-summary">
            <span data-testid="pod-total">
              <b>{formatCount(total)}</b>
              {` ${total === 1 ? kind.toLowerCase() : noun}`}
            </span>
            {status.summary !== null && (
              <>
                <span data-testid="pod-failing">
                  <b className={status.summary.failing > 0 ? "e" : undefined}>
                    {formatCount(status.summary.failing)}
                  </b>
                  {" failing"}
                </span>
                <span data-testid="pod-pending">
                  <b>{formatCount(status.summary.pending)}</b>
                  {" pending"}
                </span>
              </>
            )}
          </div>
          <label className="m-list__sort">
            <span className="m-list__sort-label">Sort</span>
            <select
              className="m-list__sort-select"
              aria-label="Sort by"
              data-testid="m-sort"
              value={currentSortKey}
              onChange={(e) => {
                const option = sortOptions.find((o) => o.key === e.target.value);
                if (option) {
                  void usePodsStore.getState().setSort(option.field, currentSort?.direction ?? "asc");
                }
              }}
            >
              {currentSortKey === "" && <option value="">—</option>}
              {sortOptions.map((o) => (
                <option key={o.key} value={o.key}>
                  {o.label}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="m-list__sort-dir"
            data-testid="m-sort-dir"
            disabled={currentSort === null}
            aria-label={
              currentSort?.direction === "desc" ? "Sorted descending; sort ascending" : "Sorted ascending; sort descending"
            }
            onClick={() => {
              if (currentSort === null) return;
              void usePodsStore
                .getState()
                .setSort(currentSort.field, currentSort.direction === "asc" ? "desc" : "asc");
            }}
          >
            {currentSort?.direction === "desc" ? (
              <ArrowDown size={16} strokeWidth={1.7} aria-hidden="true" />
            ) : (
              <ArrowUp size={16} strokeWidth={1.7} aria-hidden="true" />
            )}
          </button>
        </div>
      </div>

      {actionSuccess !== null && (
        <p role="status" className="pod-table__action-status" data-testid="action-status">
          {actionSuccess}
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
              <p role="alert" className="pod-table__notice pod-table__notice--alert" data-testid="metrics-notice">
                No permission to read metrics
              </p>
            )}
            {metricsNotInstalled && (
              <p role="status" className="pod-table__notice pod-table__notice--status" data-testid="metrics-notice">
                metrics-server not installed — CPU and memory are unavailable
              </p>
            )}
          </div>
        )
      )}
      {queried.length > 0 && progress !== null && (
        <p role="status" className="pod-table__notice pod-table__notice--progress" data-testid="pod-loading-progress">
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
            {querySpec.textFilter !== null || querySpec.namespaces.length > 0 || statusFilter.length > 0
              ? `No ${noun} match the filter`
              : `No ${noun}`}
          </p>
        )}

      <div
        className="m-ptr"
        data-testid="m-ptr"
        data-state={refreshing ? "refreshing" : pull >= PULL_THRESHOLD ? "armed" : pull > 0 ? "pulling" : "idle"}
        style={{ height: `${String(ptrHeight)}px` }}
        aria-hidden={ptrHeight === 0}
      >
        {ptrHeight > 0 && (
          <span role="status" className="m-ptr__label">
            {ptrLabel}
          </span>
        )}
      </div>

      <div
        ref={scrollRef}
        className="m-list__scroll"
        data-testid="m-list-scroll"
        data-at-top={atTop ? "true" : "false"}
        role="list"
        aria-label={kindPlural(kind)}
        hidden={listHidden}
        onScroll={(e) => {
          setAtTop(e.currentTarget.scrollTop <= 0);
        }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={(e) => {
          endPull(e, false);
        }}
        onPointerCancel={(e) => {
          endPull(e, true);
        }}
      >
        <div className="m-list__inner" style={{ height: `${String(virtualizer.getTotalSize())}px` }}>
          {virtualItems.map((item) => {
            const row = rowsByIndex[item.index];
            const style = { transform: `translateY(${String(item.start)}px)`, height: `${String(MOBILE_ROW_HEIGHT)}px` };
            if (!row) {
              return (
                <div
                  key={item.key}
                  role="listitem"
                  className="m-row m-row--skeleton"
                  data-index={item.index}
                  style={style}
                  aria-busy="true"
                >
                  <span className="pod-table__skeleton-bar" />
                </div>
              );
            }
            const st = statusOf(row);
            return (
              <div key={item.key} role="listitem" className="m-row__item" data-index={item.index} style={style}>
                <button
                  type="button"
                  className={row.status === "failed" ? "m-row m-row--failed" : "m-row"}
                  data-testid="m-row"
                  data-uid={row.uid}
                  data-status={row.status}
                  onClick={(e) => {
                    if (suppressClickRef.current) return;
                    openDetail(row, e.currentTarget);
                  }}
                >
                  <span className="m-row__l1">
                    <span className="m-row__name">{row.name}</span>
                    {st !== null && (
                      <span className={`pill ${STATUS_TONE[st.phase]}`} title={st.text} data-testid="m-row-status">
                        {st.text}
                      </span>
                    )}
                  </span>
                  {isPod ? (
                    <PodLine2 row={row} now={now} showCluster={showCluster} />
                  ) : (
                    <GenericLine2 row={row} columns={line2Columns} now={now} showCluster={showCluster} />
                  )}
                </button>
              </div>
            );
          })}
        </div>
      </div>

      {detailRow && (
        <ResourceDrawer
          key={detailRow.uid}
          variant="screen"
          remoteOps={remoteOps}
          row={detailRow}
          detail={detail}
          loading={detailLoading}
          error={detailError}
          onClose={closeDetail}
        />
      )}
      <ActionConfirmDialog />
    </div>
  );
}
