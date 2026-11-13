import { isTauri } from "@tauri-apps/api/core";
import { Lock, TriangleAlert, Zap } from "lucide-react";
import { type CSSProperties, type KeyboardEvent, useMemo, useState } from "react";
import {
  commands,
  type MetricsAvailability,
  type RightSizingItem,
  type RightSizingReport,
  type RowStatus,
} from "../generated/bindings";
import { type ConnectionStatus, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import {
  type AttentionItem,
  type CapacitySummary,
  type ClusterSummary,
  type OverviewData,
  useOverviewStore,
  type WarningEvent,
} from "../stores/overview";
import type { Row } from "../stores/pods";
import { ActionConfirmDialog } from "./ActionConfirmDialog";
import { formatAge } from "./age";
import { filterWarnings, OVERVIEW_LIST_CAP, RANGES, type WarningRange } from "./overviewWarnings";
import {
  plural,
  useConnectCluster,
  useIssuesByCluster,
  useOverviewDrawer,
  useOverviewNow,
  useOverviewSubscription,
} from "./overviewHooks";
import { ResourceDrawer } from "./ResourceDrawer";

const ICON_SIZE = 14;
const ICON_STROKE = 1.7;


/** Usage at or above these percentages tints the bar (mockup: 83 % memory is amber). */
const WARN_PCT = 80;
const ERR_PCT = 95;

/** Kinds without a namespace; their events carry the event's namespace, not the object's. */
const CLUSTER_SCOPED = new Set([
  "Node",
  "Namespace",
  "PersistentVolume",
  "StorageClass",
  "ClusterRole",
  "ClusterRoleBinding",
  "CustomResourceDefinition",
  "PriorityClass",
  "IngressClass",
]);


/** Drawer row for an object known only by (cluster, kind, namespace, name). */
function targetRow(
  cluster: string,
  kind: string,
  namespace: string | null,
  name: string,
  status: RowStatus = "unknown",
  statusReason = "",
): Row {
  const ns = CLUSTER_SCOPED.has(kind) || namespace === "" ? null : namespace;
  return {
    uid: `overview:${cluster}/${kind}/${ns ?? ""}/${name}`,
    cluster,
    kind,
    namespace: ns,
    name,
    status,
    statusReason,
    cells: [],
    labels: [],
    custom: [],
  };
}

function statusTone(status: RowStatus): "err" | "warn" | "off" {
  switch (status) {
    case "failed":
      return "err";
    case "pending":
    case "degraded":
    case "terminating":
      return "warn";
    default:
      return "off";
  }
}

function metricsText(metrics: MetricsAvailability): string {
  switch (metrics.state) {
    case "notInstalled":
      return "metrics-server not installed";
    case "forbidden":
      return "No permission to read metrics";
    case "error":
      return `Metrics unavailable: ${metrics.message}`;
    case "pending":
    case "available":
      return "Metrics loading…";
  }
}

function CapacityRow({
  label,
  summary,
  metrics,
}: {
  label: string;
  summary: CapacitySummary | null;
  metrics: MetricsAvailability;
}) {
  if (summary === null || summary.allocatable === 0) {
    return (
      <div className="overview-cap" data-testid={`capacity-${label}`}>
        <div className="overview-cap__head">
          <span className="overview-cap__label">{label}</span>
          <span className="overview-cap__none" title={metrics.state === "error" ? metrics.message : undefined}>
            {summary === null ? metricsText(metrics) : "Allocatable capacity unknown"}
          </span>
        </div>
      </div>
    );
  }
  const used = Math.min(100, Math.max(0, summary.usedPct));
  const req = Math.min(100, Math.max(0, summary.requestedPct));
  const tone = used >= ERR_PCT ? "err" : used >= WARN_PCT ? "warn" : "";
  return (
    <div className="overview-cap" data-testid={`capacity-${label}`}>
      <div className="overview-cap__head">
        <span className="overview-cap__label">{label}</span>
        <span
          title={`${summary.formattedUsed} used · ${summary.formattedRequested} requested · ${summary.formattedAllocatable} allocatable`}
        >
          {summary.usedPct}% used · <span className="overview-cap__req">{summary.requestedPct}% requested</span>
        </span>
      </div>
      <div
        className="bar overview-cap__bar"
        role="img"
        aria-label={`${label}: ${String(summary.usedPct)}% used, ${String(summary.requestedPct)}% requested`}
      >
        <i className="req" style={{ width: `${String(req)}%` }} />
        <i className={tone} style={{ width: `${String(used)}%` }} />
      </div>
    </div>
  );
}

function connectionNotice(connection: ConnectionStatus): { text: string; tone: "err" | "warn" | "off" } | null {
  switch (connection.state) {
    case "connected":
      return null;
    case "connecting":
      return { text: "Connecting…", tone: "off" };
    case "disconnected":
      return { text: "Disconnected", tone: "off" };
    case "degraded":
      return { text: connection.message, tone: "warn" };
    case "error":
      return { text: connection.message, tone: "err" };
  }
}

/** Shown instead of a count the backend could not compute (never a fake 0). */
const UNKNOWN = "—";

function problemFor(summary: ClusterSummary, kind: string): string {
  return summary.problems.find((p) => p.kind === kind)?.message ?? "Not loaded yet";
}

export function IssuesPill({ count, truncated, connected }: { count: number; truncated: boolean; connected: boolean }) {
  if (!connected && count === 0) return <span className="pill off">No data yet</span>;
  if (count > 0) {
    return (
      <span className="pill warn">
        {truncated ? `${String(count)}+ issues` : plural(count, "issue")}
      </span>
    );
  }
  // The capped list may have no room left for this cluster's issues.
  if (truncated) return <span className="pill off">Not in the first {OVERVIEW_LIST_CAP} issues</span>;
  return <span className="pill ok">No issues</span>;
}

export function ClusterCard({
  summary,
  issues,
  truncated,
  onConnect,
}: {
  summary: ClusterSummary;
  issues: number;
  truncated: boolean;
  onConnect: (cluster: string) => void;
}) {
  const settings = useContextsStore((s) => s.settingsByContext[summary.cluster]);
  const { connection } = summary;
  const notice = connectionNotice(connection);
  const showStats = connection.state === "connected" || connection.state === "degraded";
  const canConnect = connection.state === "error" || connection.state === "disconnected";
  const style = settings?.colour ? ({ "--ctx": settings.colour } as CSSProperties) : undefined;
  const podsKnown =
    summary.podsRunning !== null && summary.podsPending !== null && summary.podsFailing !== null;
  const podsTotal = podsKnown
    ? (summary.podsRunning ?? 0) + (summary.podsPending ?? 0) + (summary.podsFailing ?? 0)
    : null;
  // The connection notice already shows one problem; list the others so none is hidden.
  const extraProblems = summary.problems.filter((p) => p.message !== notice?.text);

  return (
    <article
      className="card overview-cluster"
      style={style}
      aria-label={`Cluster ${summary.cluster}`}
      data-testid={`overview-cluster-${summary.cluster}`}
      data-connection={connection.state}
    >
      <h4 className="overview-cluster__title">
        <span className="overview-cluster__name">{summary.cluster}</span>
        {settings?.protected && (
          <span className="chip overview-cluster__protected" title="Protected context (type name to confirm writes)">
            <Lock size={12} strokeWidth={ICON_STROKE} aria-hidden="true" />
            protected
          </span>
        )}
      </h4>
      <div className="body">
        <div className="overview-cluster__status">
          <IssuesPill count={issues} truncated={truncated} connected={showStats} />
        </div>
        {notice && (
          <div
            className={`overview-cluster__notice overview-cluster__notice--${notice.tone}`}
            role={notice.tone === "err" ? "alert" : "status"}
          >
            <span className="overview-cluster__notice-text">{notice.text}</span>
            {canConnect && (
              <button
                type="button"
                className="btn sm"
                onClick={() => {
                  onConnect(summary.cluster);
                }}
              >
                Connect
              </button>
            )}
          </div>
        )}
        {showStats && extraProblems.length > 0 && (
          <ul className="overview-cluster__problems" aria-label="Problems">
            {extraProblems.map((p) => (
              <li key={p.kind} className="overview-cluster__notice overview-cluster__notice--warn">
                {p.message}
              </li>
            ))}
          </ul>
        )}
        {showStats && (
          <>
            <div className="overview-cluster__stats">
              <div>
                <div className="overview-stat__label">NODES READY</div>
                <div
                  className="overview-stat__value"
                  title={summary.nodesTotal === null ? problemFor(summary, "Node") : undefined}
                >
                  {summary.nodesReady === null || summary.nodesTotal === null
                    ? UNKNOWN
                    : `${String(summary.nodesReady)}/${String(summary.nodesTotal)}`}
                </div>
              </div>
              <div>
                <div className="overview-stat__label">PODS</div>
                <div
                  className="overview-stat__value"
                  title={podsTotal === null ? problemFor(summary, "Pod") : undefined}
                >
                  {podsTotal ?? UNKNOWN}
                </div>
                {podsKnown && (
                  <div className="overview-stat__sub">
                    {summary.podsRunning} running
                    {(summary.podsFailing ?? 0) > 0 && (
                      <>
                        {" · "}
                        <span className="overview-stat__err">{summary.podsFailing} failing</span>
                      </>
                    )}
                    {(summary.podsPending ?? 0) > 0 && (
                      <>
                        {" · "}
                        <span className="overview-stat__warn">{summary.podsPending} pending</span>
                      </>
                    )}
                  </div>
                )}
              </div>
              <div>
                <div className="overview-stat__label">WARNINGS · 1H</div>
                <div
                  className="overview-stat__value"
                  data-tone={(summary.warningEvents1h ?? 0) > 0 ? "warn" : undefined}
                  title={summary.warningEvents1h === null ? problemFor(summary, "Event") : undefined}
                >
                  {summary.warningEvents1h ?? UNKNOWN}
                </div>
              </div>
            </div>
            <CapacityRow label="CPU" summary={summary.cpu} metrics={summary.metrics} />
            <CapacityRow label="Memory" summary={summary.memory} metrics={summary.metrics} />
          </>
        )}
      </div>
    </article>
  );
}

/** Up/Down/Home/End move focus between the row buttons of a list. */
function onListKeyDown(e: KeyboardEvent<HTMLUListElement>) {
  const keys = ["ArrowDown", "ArrowUp", "Home", "End"];
  if (!keys.includes(e.key)) return;
  const buttons = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>("button.overview-row"));
  const at = buttons.findIndex((b) => b === document.activeElement);
  if (at === -1 && e.key !== "Home" && e.key !== "End") return;
  e.preventDefault();
  const next =
    e.key === "Home" ? 0 : e.key === "End" ? buttons.length - 1 : at + (e.key === "ArrowDown" ? 1 : -1);
  buttons[Math.max(0, Math.min(buttons.length - 1, next))]?.focus();
}

export function AttentionCard({
  items,
  truncated,
  onOpen,
}: {
  items: readonly AttentionItem[];
  truncated: boolean;
  onOpen: (row: Row, origin: HTMLElement) => void;
}) {
  return (
    <section className="card overview-list overview-list--attention" aria-labelledby="overview-attention-title">
      <h4 id="overview-attention-title">
        <TriangleAlert size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
        Needs attention
        <span className="r" data-testid="attention-count">
          {truncated ? `${String(OVERVIEW_LIST_CAP)}+ · showing first ${String(OVERVIEW_LIST_CAP)}` : items.length}
        </span>
      </h4>
      {items.length === 0 ? (
        <p className="overview-list__empty">Nothing needs attention on the connected clusters.</p>
      ) : (
        <ul className="overview-list__rows" aria-label="Needs attention" onKeyDown={onListKeyDown}>
          {items.map((item) => (
            <li key={`${item.cluster}/${item.kind}/${item.namespace}/${item.name}/${item.reason}`}>
              <button
                type="button"
                className="overview-row overview-row--attention"
                title={item.since ? `Since ${item.since}` : undefined}
                onClick={(e) => {
                  onOpen(
                    targetRow(item.cluster, item.kind, item.namespace, item.name, item.status, item.reason),
                    e.currentTarget,
                  );
                }}
              >
                <span className={`pill ${statusTone(item.status)} overview-row__pill`}>{item.reason}</span>
                <span className="overview-row__main">
                  <span className="overview-row__name">
                    {item.kind === "Pod" ? item.name : `${item.kind} ${item.name}`}
                  </span>
                  <span className="overview-row__sub">
                    {item.namespace ? `${item.namespace} · ` : ""}
                    {item.cluster}
                  </span>
                </span>
                <span className="overview-row__msg" title={item.message}>
                  {item.message}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function WarningsCard({
  events,
  total,
  range,
  now,
  multiCluster,
  onOpen,
}: {
  events: readonly WarningEvent[];
  total: number;
  range: WarningRange;
  now: number;
  multiCluster: boolean;
  onOpen: (row: Row, origin: HTMLElement) => void;
}) {
  const capped = total >= OVERVIEW_LIST_CAP;
  return (
    <section className="card overview-list overview-list--events" aria-labelledby="overview-events-title">
      <h4 id="overview-events-title">
        <Zap size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
        Recent warning events
        <span className="r">{multiCluster ? "All clusters" : `Last ${range}`}</span>
      </h4>
      {events.length === 0 ? (
        <p className="overview-list__empty">No warning events in the last {range}</p>
      ) : (
        <ul className="overview-list__rows" aria-label="Recent warning events" onKeyDown={onListKeyDown}>
          {events.map((ev) => {
            const age = ev.lastTimestamp ? formatAge(ev.lastTimestamp, now) : ev.age;
            const content = (
              <>
                <span className="overview-row__age">{age}</span>
                <span className="overview-row__main">
                  <span className="overview-row__line">
                    <span className="overview-row__reason">{ev.reason}</span>
                    {" · "}
                    <span>{ev.involvedName || ev.name}</span>
                    {multiCluster && <span className="overview-row__cluster">{ev.cluster}</span>}
                  </span>
                  <span className="overview-row__event-msg" title={ev.message}>
                    {ev.message}
                  </span>
                </span>
              </>
            );
            const key = `${ev.cluster}/${ev.namespace}/${ev.name}`;
            if (ev.involvedKind === "" || ev.involvedName === "") {
              return (
                <li key={key} className="overview-row overview-row--event overview-row--static">
                  {content}
                </li>
              );
            }
            return (
              <li key={key}>
                <button
                  type="button"
                  className="overview-row overview-row--event"
                  aria-label={`${ev.reason}: ${ev.involvedKind} ${ev.involvedName}${multiCluster ? ` on ${ev.cluster}` : ""}`}
                  onClick={(e) => {
                    onOpen(targetRow(ev.cluster, ev.involvedKind, ev.namespace, ev.involvedName), e.currentTarget);
                  }}
                >
                  {content}
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {capped && (
        <p className="overview-list__note">
          From the {OVERVIEW_LIST_CAP} most recent warning events; older ones are not listed.
        </p>
      )}
    </section>
  );
}

function OverviewBody({
  data,
  range,
  now,
  onOpen,
}: {
  data: OverviewData;
  range: WarningRange;
  now: number;
  onOpen: (row: Row, origin: HTMLElement) => void;
}) {
  const issuesByCluster = useIssuesByCluster(data);
  const warnings = useMemo(
    () => filterWarnings(data.recentWarnings, range, now),
    [data.recentWarnings, range, now],
  );
  const connect = useConnectCluster();

  return (
    <>
      <div className="overview__clusters">
        {data.clusters.map((c) => (
          <ClusterCard
            key={c.cluster}
            summary={c}
            issues={issuesByCluster.get(c.cluster) ?? 0}
            truncated={data.attention.truncated}
            onConnect={connect}
          />
        ))}
      </div>
      <div className="overview__lists">
        <AttentionCard items={data.attention.items} truncated={data.attention.truncated} onOpen={onOpen} />
        <WarningsCard
          events={warnings}
          total={data.recentWarnings.length}
          range={range}
          now={now}
          multiCluster={data.clusters.length > 1}
          onOpen={onOpen}
        />
      </div>
    </>
  );
}

export function RightSizingBody({
  report,
  threshold,
  onThresholdChange,
  onOpen,
}: {
  report: RightSizingReport | null | undefined;
  threshold: number;
  onThresholdChange: (t: number) => void;
  onOpen: (row: Row, origin: HTMLElement) => void;
}) {
  return (
    <div className="rs-view" data-testid="right-sizing-view">
      <div className="rs-toolbar">
        {report?.notice ? (
          <div className="cluster-notice-banner__alert" role="status">
            {report.notice}
          </div>
        ) : (
          <span className="overview__count">
            Resource efficiency analysis across connected clusters
          </span>
        )}
        <div className="overview__range" role="group" aria-label="Unused requests threshold">
          <span className="overview__range-label">Unused threshold</span>
          {[10, 20, 50].map((t) => (
            <button
              key={t}
              type="button"
              className="btn sm overview__range-btn"
              aria-pressed={threshold === t}
              onClick={() => {
                onThresholdChange(t);
              }}
            >
              &lt; {String(t)}%
            </button>
          ))}
        </div>
      </div>

      <div className="rs-grid">
        <RightSizingCard
          title="Near limit (≥ 80%)"
          subtitle="Throttling & OOMKill risk"
          items={report?.nearLimit ?? []}
          emptyText="No workloads near limit"
          badgeTone="err"
          onOpen={onOpen}
        />
        <RightSizingCard
          title="Over request (> 100%)"
          subtitle="Using more than allocated"
          items={report?.overRequest ?? []}
          emptyText="No workloads over request"
          badgeTone="warn"
          onOpen={onOpen}
        />
        <RightSizingCard
          title={`Unused requests (< ${String(threshold)}%)`}
          subtitle="Over-provisioned workloads (wasted)"
          items={report?.unusedRequests ?? []}
          emptyText="No over-provisioned workloads"
          badgeTone="off"
          onOpen={onOpen}
        />
        <RightSizingCard
          title="Missing requests/limits"
          subtitle="Unbounded workloads"
          items={report?.missingResources ?? []}
          emptyText="All workloads have requests and limits"
          badgeTone="warn"
          onOpen={onOpen}
        />
      </div>
    </div>
  );
}

function RightSizingCard({
  title,
  subtitle,
  items,
  emptyText,
  badgeTone,
  onOpen,
}: {
  title: string;
  subtitle: string;
  items: readonly RightSizingItem[];
  emptyText: string;
  badgeTone: "err" | "warn" | "off";
  onOpen: (row: Row, origin: HTMLElement) => void;
}) {
  return (
    <section className="card overview-list" aria-label={title}>
      <h4>
        {title}
        <span className={`pill ${badgeTone} r`}>{String(items.length)}</span>
      </h4>
      <p className="overview-list__note">{subtitle}</p>
      {items.length === 0 ? (
        <p className="overview-list__empty">{emptyText}</p>
      ) : (
        <ul className="overview-list__rows" onKeyDown={onListKeyDown}>
          {items.map((item) => (
            <li key={`${item.cluster}/${item.namespace}/${item.name}/${item.resource}`}>
              <button
                type="button"
                className="overview-row overview-row--attention"
                onClick={(e) => {
                  onOpen(
                    targetRow(item.cluster, item.kind || "Pod", item.namespace, item.name),
                    e.currentTarget,
                  );
                }}
              >
                <span className={`pill ${item.resource === "memory" ? "warn" : "off"} overview-row__pill`}>
                  {item.resource.toUpperCase()}
                </span>
                <span className="overview-row__main">
                  <span className="overview-row__name">{item.name}</span>
                  <span className="overview-row__sub">
                    {item.namespace ? `${item.namespace} · ` : ""}
                    {item.cluster}
                  </span>
                </span>
                <span className="overview-row__msg" title={item.label}>
                  {item.label}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * What the overview shows before it has cluster data: not in the app, load error, loading, or
 * no cluster selected. `null` once there is at least one cluster summary. Shared with mobile.
 */
export function OverviewPlaceholder({
  onOpenPods,
  emptyHint = "No cluster selected — select a context in the sidebar.",
}: {
  onOpenPods: () => void;
  emptyHint?: string;
}) {
  const available = useOverviewStore((s) => s.available);
  const status = useOverviewStore((s) => s.status);
  const data = useOverviewStore((s) => s.data);
  const error = useOverviewStore((s) => s.error);
  if (!available) {
    return (
      <p className="empty-state" role="status">
        Not running in the Fleetglass app
      </p>
    );
  }
  if (status === "error" && data === null) {
    return (
      <p className="error-state" role="alert">
        Cannot load the overview: {error ?? "unknown error"}
      </p>
    );
  }
  if (data === null) {
    return (
      <p className="empty-state" role="status">
        Loading overview…
      </p>
    );
  }
  if (data.clusters.length > 0) return null;
  return (
    <div className="overview__empty" role="status">
      <p>{emptyHint}</p>
      <p className="overview__empty-hint">
        The overview connects only the selected context or cluster group.
      </p>
      <button type="button" className="btn sm" onClick={onOpenPods}>
        Open Pods
      </button>
    </div>
  );
}

/** Cross-cluster overview (02-08b, mockup `2-overview-dark`). */
export function OverviewPage() {
  const status = useOverviewStore((s) => s.status);
  const data = useOverviewStore((s) => s.data);
  const error = useOverviewStore((s) => s.error);
  const showKind = useLayoutStore((s) => s.showKind);
  const [overviewTab, setOverviewTab] = useState<"overview" | "right-sizing">("overview");
  const [range, setRange] = useState<WarningRange>("1h");
  const [rsThreshold, setRsThreshold] = useState<number>(20);
  const [customRsReport, setCustomRsReport] = useState<RightSizingReport | null>(null);
  const [now, touchNow] = useOverviewNow();
  useOverviewSubscription();
  const drawer = useOverviewDrawer(touchNow);
  const openDrawer = drawer.open;
  const closeDrawer = drawer.close;

  const clusterCount = data?.clusters.length ?? 0;

  const handleThresholdChange = (t: number) => {
    setRsThreshold(t);
    if (!isTauri() || !data) return;
    const clusterNames = data.clusters.map((c) => c.cluster);
    commands
      .getRightSizing(clusterNames, t)
      .then((res) => {
        if (res.status === "ok") {
          setCustomRsReport(res.data);
        }
      })
      .catch(() => {});
  };

  const activeRsReport =
    customRsReport && customRsReport.thresholdPct === rsThreshold
      ? customRsReport
      : data?.rightSizing;

  const placeholder = (
    <OverviewPlaceholder
      onOpenPods={() => {
        showKind("Pods", "Pod");
      }}
    />
  );
  const body =
    data !== null && data.clusters.length > 0 ? (
      overviewTab === "overview" ? (
        <OverviewBody data={data} range={range} now={now} onOpen={openDrawer} />
      ) : (
        <RightSizingBody
          report={activeRsReport}
          threshold={rsThreshold}
          onThresholdChange={handleThresholdChange}
          onOpen={openDrawer}
        />
      )
    ) : (
      placeholder
    );

  return (
    <div className="overview-layout">
      <div className="overview" data-testid="overview-page">
        <header className="overview__header">
          <h1 className="overview__title">Overview</h1>
          {data !== null && <span className="overview__count">{plural(clusterCount, "cluster")}</span>}
          <div className="overview__tabs" role="tablist" aria-label="Overview views">
            <button
              type="button"
              role="tab"
              aria-selected={overviewTab === "overview"}
              className={`overview__tab-btn ${overviewTab === "overview" ? "overview__tab-btn--active" : ""}`}
              onClick={() => {
                setOverviewTab("overview");
              }}
            >
              Cluster overview
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={overviewTab === "right-sizing"}
              className={`overview__tab-btn ${overviewTab === "right-sizing" ? "overview__tab-btn--active" : ""}`}
              onClick={() => {
                setOverviewTab("right-sizing");
              }}
            >
              Right-sizing & Efficiency
            </button>
          </div>
          {status === "error" && data !== null && (
            <span className="overview__stale" role="alert">
              Updates stopped: {error}
            </span>
          )}
          {overviewTab === "overview" && (
            <div className="overview__range" role="group" aria-label="Warning events time range">
              <span className="overview__range-label">Warnings</span>
              {RANGES.map((r) => (
                <button
                  key={r.id}
                  type="button"
                  className="btn sm overview__range-btn"
                  aria-pressed={range === r.id}
                  onClick={() => {
                    touchNow();
                    setRange(r.id);
                  }}
                >
                  Last {r.label}
                </button>
              ))}
            </div>
          )}
        </header>
        {body}
      </div>
      {drawer.row && (
        <ResourceDrawer
          key={drawer.row.uid}
          row={drawer.row}
          detail={drawer.detail}
          loading={drawer.loading}
          error={drawer.error}
          onClose={closeDrawer}
        />
      )}
      <ActionConfirmDialog />
    </div>
  );
}
