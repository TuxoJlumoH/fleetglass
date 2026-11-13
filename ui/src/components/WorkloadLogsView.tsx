import { observeElementRect, useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useStore } from "zustand";
import type { WorkloadLogTarget } from "../generated/bindings";
import { useIsAndroid } from "../stores/appInfo";
import {
  createWorkloadLogsStore,
  saveLogFile,
  shareLogText,
  type LogEntry,
  type SaveLogResult,
  type WorkloadLogsError,
  type WorkloadLogsStatus,
} from "../stores/workloadLogs";
import {
  entryLevel,
  rangeOverrides,
  type LevelFilter,
  type TimeRange,
} from "../lib/logLevel";

const ESTIMATED_LINE_HEIGHT = 20;
const DEFAULT_VIEWPORT_WIDTH = 1000;
const DEFAULT_VIEWPORT_HEIGHT = 560;

export interface WorkloadLogsViewProps {
  target: WorkloadLogTarget;
  /** Rendered in its own `logs-<n>` window: no "Pop out" button. */
  detached?: boolean;
  /** Client-side line cap; defaults to the store's 50,000. */
  maxLines?: number;
  /**
   * Phone layout (03-04b, the full-screen detail): touch-sized toolbar with search, wrap and,
   * for a single pod, the previous container instance. The desktop drawer leaves it off.
   */
  phone?: boolean;
  /**
   * The pod's container (or one of them) is not running and has restarted, from the detail
   * data: with no live lines the phone view offers the previous instance's logs.
   */
  notRunning?: boolean;
}

/** Search box text → line matcher: a valid regex (case-insensitive), else a plain substring. */
function lineMatcher(query: string): ((text: string) => boolean) | null {
  if (query === "") return null;
  try {
    const re = new RegExp(query, "i");
    return (text) => re.test(text);
  } catch {
    const needle = query.toLowerCase();
    return (text) => text.toLowerCase().includes(needle);
  }
}

function selectorSummary(target: WorkloadLogTarget): string {
  const s = target.selector;
  if (s.type === "pod") return `pod/${s.name}`;
  return s.type === "labels" ? s.selector : `${s.kind}/${s.name}`;
}

/** File-name-safe base name for downloads: the pod/workload name. */
function logFileName(target: WorkloadLogTarget): string {
  const s = target.selector;
  const base = s.type === "labels" ? `${target.namespace}-logs` : s.name;
  return base.replace(/[^A-Za-z0-9._-]/g, "_") || "logs";
}

function pad(n: number, width = 2): string {
  return String(n).padStart(width, "0");
}

/** Local wall-clock time the UI received the line, `HH:MM:SS.mmm`. */
function formatReceiveTime(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

/** Pretty-printed JSON, or `null` if the backend's `json` flag disagrees with `JSON.parse`. */
function prettyJson(text: string): string | null {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return null;
  }
}

const STATUS_LABEL: Record<WorkloadLogsStatus, string> = {
  idle: "Idle",
  starting: "Connecting…",
  streaming: "Streaming",
  stopped: "Stopped",
  failed: "Failed to start",
};

const ERROR_SOURCE_LABEL: Record<WorkloadLogsError["source"], string> = {
  start: "Could not start log stream",
  stream: "Log stream",
  stop: "Could not stop log stream",
  popOut: "Could not open logs window",
  save: "Could not save",
};

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

function toggleInSet<T>(set: ReadonlySet<T>, value: T): Set<T> {
  const next = new Set(set);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}

interface LogLineProps {
  entry: LogEntry;
  showTime: boolean;
  /** `false` when the stream has one pod and one container: the label would repeat on every line. */
  showSource: boolean;
  collapsed: boolean;
  onToggleCollapsed: (id: number) => void;
}

function LogLine({ entry, showTime, showSource, collapsed, onToggleCollapsed }: LogLineProps) {
  const pretty = entry.json ? prettyJson(entry.text) : null;
  return (
    <>
      {showTime && (
        <span className="workload-logs__time" data-testid="log-receive-time">
          {formatReceiveTime(entry.receivedAt)}
        </span>
      )}
      {showSource && (
        <span
          className="workload-logs__source"
          data-testid="log-source"
          title={`${entry.pod} / ${entry.container}`}
        >
          {entry.pod}/{entry.container}
        </span>
      )}
      {pretty !== null ? (
        <span className="workload-logs__json">
          <button
            type="button"
            className="workload-logs__json-toggle"
            aria-expanded={!collapsed}
            aria-label={collapsed ? "Expand JSON" : "Collapse JSON"}
            onClick={() => {
              onToggleCollapsed(entry.id);
            }}
          >
            {collapsed ? "▸" : "▾"}
          </button>
          {collapsed ? (
            <span className="workload-logs__text">{entry.text}</span>
          ) : (
            <pre className="workload-logs__json-body" data-testid="log-json">
              {pretty}
            </pre>
          )}
        </span>
      ) : (
        <span className="workload-logs__text">{entry.text}</span>
      )}
    </>
  );
}

/**
 * Follows the logs of every pod of a workload (P2-03).
 *
 * Pod/container filtering here is display-only over the bounded client ring. This is
 * an accepted exception to "the backend filters" because logs are a stream with a
 * fixed client-side cap, not a list query over cluster objects.
 */
export function WorkloadLogsView({
  target,
  detached = false,
  maxLines,
  phone = false,
  notRunning = false,
}: WorkloadLogsViewProps) {
  const isAndroid = useIsAndroid();
  const [store] = useState(() =>
    createWorkloadLogsStore(maxLines !== undefined ? { maxLines } : {}),
  );
  const status = useStore(store, (s) => s.status);
  const ring = useStore(store, (s) => s.ring);
  const version = useStore(store, (s) => s.version);
  const pods = useStore(store, (s) => s.pods);
  const containers = useStore(store, (s) => s.containers);
  const errors = useStore(store, (s) => s.errors);
  const errorCount = useStore(store, (s) => s.errorCount);

  const [hiddenPods, setHiddenPods] = useState<ReadonlySet<string>>(() => new Set());
  const [hiddenContainers, setHiddenContainers] = useState<ReadonlySet<string>>(() => new Set());
  const [showTime, setShowTime] = useState(false);
  const [follow, setFollow] = useState(true);
  const [collapsedJson, setCollapsedJson] = useState<ReadonlySet<number>>(() => new Set());
  const [level, setLevel] = useState<LevelFilter>("all");
  const [range, setRange] = useState<TimeRange>("tail");
  const [saving, setSaving] = useState(false);
  const [saveResult, setSaveResult] = useState<SaveLogResult | null>(null);
  const [shareError, setShareError] = useState<string | null>(null);
  // Phone-only controls (03-04b); the desktop drawer keeps its toolbar as it was.
  const [search, setSearch] = useState("");
  const [wrap, setWrap] = useState(true);
  const [previousWanted, setPreviousWanted] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  // `kubectl logs --previous` only exists for one pod.
  const canPrevious = phone && target.selector.type === "pod";
  const previous = canPrevious && previousWanted;

  // Restart only when the target's content changes, not its object identity.
  const targetKey = JSON.stringify({
    ...target,
    ...rangeOverrides(range),
    ...(previous ? { previous: true } : {}),
  });
  useEffect(() => {
    const parsed = JSON.parse(targetKey) as WorkloadLogTarget;
    void store.getState().start(parsed);
    return () => {
      void store.getState().stop();
    };
  }, [store, targetKey]);

  const matcher = useMemo(() => lineMatcher(search), [search]);
  const filtering =
    hiddenPods.size > 0 || hiddenContainers.size > 0 || level !== "all" || matcher !== null;
  // `ring` is mutated in place; `version` changes whenever its contents do.
  const filtered = useMemo<LogEntry[] | null>(() => {
    if (!filtering || version < 0) return null;
    const out: LogEntry[] = [];
    for (let i = 0; i < ring.size; i += 1) {
      const entry = ring.at(i);
      if (
        entry !== undefined &&
        !hiddenPods.has(entry.pod) &&
        !hiddenContainers.has(entry.container) &&
        (level === "all" || entryLevel(entry) === level) &&
        (matcher === null || matcher(entry.text))
      ) {
        out.push(entry);
      }
    }
    return out;
  }, [filtering, ring, version, hiddenPods, hiddenContainers, level, matcher]);

  const count = filtered !== null ? filtered.length : ring.size;
  const entryAt = (i: number): LogEntry | undefined =>
    filtered !== null ? filtered[i] : ring.at(i);

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ESTIMATED_LINE_HEIGHT,
    getItemKey: (i) => entryAt(i)?.id ?? `missing-${String(i)}`,
    overscan: 20,
    initialRect: { width: DEFAULT_VIEWPORT_WIDTH, height: DEFAULT_VIEWPORT_HEIGHT },
    observeElementRect: (instance, cb) =>
      observeElementRect(instance, (rect) => {
        cb({
          width: rect.width > 0 ? rect.width : DEFAULT_VIEWPORT_WIDTH,
          height: rect.height > 0 ? rect.height : DEFAULT_VIEWPORT_HEIGHT,
        });
      }),
  });

  useEffect(() => {
    if (follow && count > 0) {
      virtualizer.scrollToIndex(count - 1, { align: "end" });
    }
  }, [virtualizer, follow, count, version]);

  const podNames = Object.keys(pods).sort();
  const presentPods = podNames.filter((p) => pods[p] === "present").length;
  // Pods announced by `podJoined` count even before they log, so a second pod shows the column.
  const multiSource = podNames.length > 1 || containers.length > 1;
  const visibleErrors = errors;

  const pause = () => {
    setFollow(false);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.ctrlKey || e.metaKey || e.altKey || isEditableTarget(e.target)) return;
    switch (e.key) {
      case "f":
        e.preventDefault();
        setFollow((f) => !f);
        break;
      case "t":
        e.preventDefault();
        setShowTime((v) => !v);
        break;
      case "G":
      case "End":
        e.preventDefault();
        setFollow(true);
        break;
      case "k":
      case "ArrowUp":
      case "PageUp":
      case "Home":
        // Moving up through history pauses following so new lines don't yank the view.
        pause();
        if (e.key === "k" || e.key === "ArrowUp") {
          e.preventDefault();
          scrollRef.current?.scrollBy({ top: -ESTIMATED_LINE_HEIGHT });
        }
        break;
      case "j":
      case "ArrowDown":
        e.preventDefault();
        scrollRef.current?.scrollBy({ top: ESTIMATED_LINE_HEIGHT });
        break;
    }
  };

  const toggleCollapsed = (id: number) => {
    setCollapsedJson((prev) => toggleInSet(prev, id));
  };

  const title = `${target.cluster}/${target.namespace}/${selectorSummary(target)}`;

  /** The lines currently loaded (after filters), one `pod/container text` per line. */
  const loadedParts = (): string[] => {
    const parts: string[] = [];
    for (let i = 0; i < count; i += 1) {
      const entry = entryAt(i);
      if (entry !== undefined) parts.push(`${entry.pod}/${entry.container} ${entry.text}\n`);
    }
    return parts;
  };

  /** Android: the same text as Download, handed to the share sheet. */
  const share = async () => {
    const base = logFileName(target);
    const name = target.selector.type === "labels" ? base : target.selector.name;
    setSaving(true);
    setShareError(null);
    const res = await shareLogText(
      `Logs ${name}`,
      `${base}${previous ? "-previous" : ""}.log`,
      loadedParts().join(""),
    );
    setSaving(false);
    if (!res.ok) setShareError(res.message);
  };

  /** Saves the lines currently loaded (after filters) as `<name>.log`. */
  const download = async () => {
    const parts = loadedParts();
    const fileName = `${logFileName(target)}.log`;
    setSaving(true);
    setSaveResult(null);
    const saved = await saveLogFile(fileName, parts.join(""));
    setSaving(false);
    if (saved !== null) {
      setSaveResult(saved);
      return;
    }
    const blob = new Blob(parts, { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = fileName;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => {
      URL.revokeObjectURL(url);
    }, 0);
  };

  // Android has no Downloads folder to save into and no second window: Share replaces both.
  const saveControls = isAndroid ? (
    <>
      <button
        type="button"
        className={`workload-logs__btn${phone ? " btn workload-logs__ctl" : ""}`}
        title="Share the lines currently shown as a .log file"
        disabled={count === 0 || saving}
        onClick={() => {
          void share();
        }}
      >
        Share
      </button>
      {shareError !== null && (
        <span
          className="workload-logs__save-status workload-logs__save-status--err"
          role="alert"
          data-testid="workload-logs-share-error"
        >
          {`Could not share: ${shareError}`}
        </span>
      )}
    </>
  ) : (
    <>
      <button
        type="button"
        className={`workload-logs__btn${phone ? " btn workload-logs__ctl" : ""}`}
        title="Save the lines currently shown as a .log file"
        disabled={count === 0 || saving}
        onClick={() => {
          void download();
        }}
      >
        Download
      </button>
      {saveResult !== null && (
        <span
          className="workload-logs__save-status"
          role="status"
          data-testid="workload-logs-save-status"
        >
          {saveResult.ok ? `Saved to ${saveResult.path}` : `Could not save: ${saveResult.message}`}
        </span>
      )}

      {!detached && (
        <button
          type="button"
          className={`workload-logs__btn${phone ? " btn workload-logs__ctl" : ""}`}
          title="Open these logs in a separate window"
          onClick={() => {
            void store.getState().popOut(target);
          }}
        >
          Pop out
        </button>
      )}
    </>
  );

  const levelSelect = (
    <select
      aria-label="Log level"
      className={phone ? "workload-logs__ctl" : undefined}
      value={level}
      onChange={(e) => {
        setLevel(e.target.value as LevelFilter);
      }}
    >
      <option value="all">All</option>
      <option value="error">Error</option>
      <option value="warn">Warn</option>
      <option value="info">Info</option>
    </select>
  );

  const rangeSelect = (
    <select
      aria-label="Time range"
      className={phone ? "workload-logs__ctl" : undefined}
      value={range}
      onChange={(e) => {
        setRange(e.target.value as TimeRange);
      }}
    >
      <option value="tail">Recent lines</option>
      <option value="5m">Last 5m</option>
      <option value="1h">Last 1h</option>
      <option value="start">Since start</option>
    </select>
  );

  const rangeHint = range === "start" && (
    <span className="workload-logs__hint" role="note" data-testid="logs-range-hint">
      {`Whole log; only the newest ${ring.capacity.toLocaleString("en-US")} lines are kept`}
    </span>
  );

  const showPrevious = () => {
    setPreviousWanted(true);
    setFollow(false);
  };

  const phoneToolbar = (
    <div className="workload-logs__toolbar workload-logs__toolbar--phone" data-testid="logs-phone-toolbar">
      <label className="tb-search workload-logs__search workload-logs__ctl">
        <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="11" cy="11" r="7" />
          <path d="m20 20-3.5-3.5" />
        </svg>
        <input
          type="search"
          className="tb-search__input"
          aria-label="Search logs (regex or text)"
          placeholder="Search (regex or text)…"
          value={search}
          onChange={(e) => {
            setSearch(e.target.value);
          }}
        />
      </label>
      <div className="workload-logs__row">
        {!previous && (
          <button
            type="button"
            className="btn workload-logs__ctl"
            aria-pressed={follow}
            onClick={() => {
              setFollow((f) => !f);
            }}
          >
            {follow ? "Pause" : "Follow"}
          </button>
        )}
        <button
          type="button"
          className="btn workload-logs__ctl"
          aria-pressed={wrap}
          onClick={() => {
            setWrap((w) => !w);
          }}
        >
          Wrap
        </button>
        {canPrevious && (
          <button
            type="button"
            className="btn workload-logs__ctl"
            aria-pressed={previous}
            data-testid="logs-previous-toggle"
            title="Logs of the previous (terminated) container instance, like kubectl logs --previous"
            onClick={() => {
              if (previous) {
                setPreviousWanted(false);
                setFollow(true);
              } else {
                showPrevious();
              }
            }}
          >
            Previous
          </button>
        )}
        {saveControls}
      </div>
      <div className="workload-logs__row">
        <label className="workload-logs__select workload-logs__select--phone">
          Level
          {levelSelect}
        </label>
        <label className="workload-logs__select workload-logs__select--phone">
          Range
          {rangeSelect}
        </label>
      </div>
      {rangeHint}
    </div>
  );

  // Only a container that is not running gets the hint; a healthy quiet pod just has no lines.
  const offerPrevious =
    canPrevious && !previous && notRunning && status !== "starting" && ring.size === 0;

  return (
    <section
      aria-label={`Workload logs for ${title}`}
      className={[
        "workload-logs",
        detached ? "workload-logs--detached" : "",
        phone ? "workload-logs--phone" : "",
        phone && !wrap ? "workload-logs--nowrap" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      data-wrap={phone ? String(wrap) : undefined}
      data-previous={previous ? "true" : undefined}
      tabIndex={0}
      onKeyDown={onKeyDown}
    >
      {phone ? phoneToolbar : (
      <div className="workload-logs__toolbar">
        <span className="workload-logs__title" title={title}>
          {title}
          {target.container != null && target.container !== "" && (
            <span className="workload-logs__title-container"> · {target.container}</span>
          )}
        </span>

        <button
          type="button"
          className="workload-logs__btn"
          aria-pressed={follow}
          title="Keep scrolled to the newest line (f)"
          onClick={() => {
            setFollow((f) => !f);
          }}
        >
          {follow ? "Pause" : "Follow"}
        </button>

        <label
          className="workload-logs__toggle"
          title="Lines carry no Kubernetes timestamps; this is when this window received each line (t)"
        >
          <input
            type="checkbox"
            checked={showTime}
            onChange={(e) => {
              setShowTime(e.target.checked);
            }}
          />
          Receive time (local)
        </label>

        <label className="workload-logs__select">
          Level
          {levelSelect}
        </label>

        <label
          className="workload-logs__select"
          title="Restarts the stream with the chosen history (applies to pods present now)"
        >
          Range
          {rangeSelect}
        </label>
        {rangeHint}

        {saveControls}
      </div>
      )}

      {previous && (
        <p className="workload-logs__mode" role="status" data-testid="logs-previous-banner">
          Previous instance — logs of the last terminated container (not live)
        </p>
      )}

      {offerPrevious && (
        <div className="workload-logs__offer" role="note" data-testid="logs-previous-offer">
          <span>The container is not running — show the previous instance&apos;s logs?</span>
          <button type="button" className="btn workload-logs__ctl" onClick={showPrevious}>
            Show previous logs
          </button>
        </div>
      )}

      {(podNames.length > 0 || containers.length > 1) && (
        <div className="workload-logs__chips" aria-label="Filter lines by pod and container">
          {podNames.map((pod) => {
            const left = pods[pod] === "left";
            const shown = !hiddenPods.has(pod);
            return (
              <button
                key={`pod:${pod}`}
                type="button"
                data-testid="log-pod-chip"
                data-pod={pod}
                data-state={left ? "left" : "present"}
                aria-pressed={shown}
                title={`${shown ? "Hide" : "Show"} lines from ${pod}${left ? " (pod left the stream)" : ""}`}
                className={[
                  "workload-logs__chip",
                  left ? "workload-logs__chip--left" : "",
                  shown ? "" : "workload-logs__chip--off",
                ]
                  .filter(Boolean)
                  .join(" ")}
                onClick={() => {
                  setHiddenPods((prev) => toggleInSet(prev, pod));
                }}
              >
                {pod}
                {left && <span className="visually-hidden"> (left)</span>}
              </button>
            );
          })}
          {containers.length > 1 &&
            containers.map((container) => {
              const shown = !hiddenContainers.has(container);
              return (
                <button
                  key={`container:${container}`}
                  type="button"
                  data-testid="log-container-chip"
                  aria-pressed={shown}
                  title={`${shown ? "Hide" : "Show"} lines from container ${container}`}
                  className={[
                    "workload-logs__chip",
                    "workload-logs__chip--container",
                    shown ? "" : "workload-logs__chip--off",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  onClick={() => {
                    setHiddenContainers((prev) => toggleInSet(prev, container));
                  }}
                >
                  {container}
                </button>
              );
            })}
        </div>
      )}

      {visibleErrors.length > 0 && (
        <div role="alert" className="workload-logs__errors" data-testid="workload-logs-error">
          <ul className="workload-logs__error-list">
            {visibleErrors.map((err, i) => (
              <li key={`${String(err.receivedAt)}-${String(i)}`}>
                <strong>{ERROR_SOURCE_LABEL[err.source]}:</strong> {err.message}
              </li>
            ))}
          </ul>
          {errorCount > visibleErrors.length && (
            <span className="workload-logs__error-more">
              {`${String(errorCount - visibleErrors.length)} earlier errors not shown`}
            </span>
          )}
          <button
            type="button"
            className="workload-logs__btn"
            onClick={() => {
              store.getState().dismissErrors();
            }}
          >
            Dismiss
          </button>
        </div>
      )}

      <div
        ref={scrollRef}
        className="workload-logs__viewport"
        role="log"
        aria-live="off"
        aria-label="Log lines"
        onWheel={(e) => {
          if (e.deltaY < 0) pause();
        }}
      >
        {count === 0 ? (
          <p className="workload-logs__empty">
            {status === "starting"
              ? "Connecting…"
              : ring.size > 0
                ? "All lines are hidden by the pod/container/level filters."
                : status === "failed"
                  ? "No log stream."
                  : previous
                    ? "No lines received from the previous instance."
                    : "No log lines received yet."}
          </p>
        ) : (
          <div
            className="workload-logs__inner"
            style={{ height: `${String(virtualizer.getTotalSize())}px` }}
          >
            {virtualizer.getVirtualItems().map((item) => {
              const entry = entryAt(item.index);
              if (entry === undefined) return null;
              return (
                <div
                  key={item.key}
                  ref={virtualizer.measureElement}
                  data-index={item.index}
                  data-testid="log-line"
                  data-pod={entry.pod}
                  className="workload-logs__line"
                  style={{ transform: `translateY(${String(item.start)}px)` }}
                >
                  <LogLine
                    entry={entry}
                    showTime={showTime}
                    showSource={multiSource}
                    collapsed={collapsedJson.has(entry.id)}
                    onToggleCollapsed={toggleCollapsed}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>

      <div className="workload-logs__status" data-testid="workload-logs-status">
        {previous && status === "streaming" ? (
          // One-shot fetch: nothing is followed, so no "following" count.
          <span>Previous instance (not following)</span>
        ) : (
          <>
            <span>{STATUS_LABEL[status]}</span>
            <span>
              {`${String(podNames.length)} ${podNames.length === 1 ? "pod" : "pods"} (${String(presentPods)} following)`}
            </span>
          </>
        )}
        <span>
          {`${ring.size.toLocaleString("en-US")} lines`}
          {filtered !== null && ` (${count.toLocaleString("en-US")} shown)`}
        </span>
        {ring.dropped > 0 && (
          <span title={`Only the newest ${ring.capacity.toLocaleString("en-US")} lines are kept`}>
            {`${ring.dropped.toLocaleString("en-US")} oldest dropped`}
          </span>
        )}
        {!follow && !previous && <span className="workload-logs__paused">Paused</span>}
      </div>
    </section>
  );
}

export interface WorkloadLogsRouteErrorProps {
  message: string;
}

/** Shown in a detached logs window whose URL does not carry a valid target. */
export function WorkloadLogsRouteError({ message }: WorkloadLogsRouteErrorProps) {
  return (
    <main className="workload-logs workload-logs--detached">
      <p role="alert" className="workload-logs__route-error">
        Cannot open logs: {message}
      </p>
    </main>
  );
}
