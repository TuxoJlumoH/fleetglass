import { Lock } from "lucide-react";
import { type CSSProperties, useMemo, useState } from "react";
import { ActionConfirmDialog } from "../components/ActionConfirmDialog";
import {
  AttentionCard,
  ClusterCard,
  IssuesPill,
  OverviewPlaceholder,
  WarningsCard,
} from "../components/OverviewPage";
import {
  plural,
  useConnectCluster,
  useIssuesByCluster,
  useOverviewDrawer,
  useOverviewNow,
  useOverviewSubscription,
} from "../components/overviewHooks";
import { filterWarnings, RANGES, type WarningRange } from "../components/overviewWarnings";
import { ResourceDrawer } from "../components/ResourceDrawer";
import { useIsAndroid } from "../stores/appInfo";
import { useContextsStore } from "../stores/contexts";
import { type ClusterSummary, type OverviewData, useOverviewStore } from "../stores/overview";

/** Other clusters folded to one tappable line each (mockup v0.3 `1-overview-dark`). */
function MiniClusterCard({
  summary,
  issues,
  truncated,
  onSelect,
}: {
  summary: ClusterSummary;
  issues: number;
  truncated: boolean;
  onSelect: (cluster: string) => void;
}) {
  const settings = useContextsStore((s) => s.settingsByContext[summary.cluster]);
  const style = settings?.colour ? ({ "--ctx": settings.colour } as CSSProperties) : undefined;
  const connected = summary.connection.state === "connected" || summary.connection.state === "degraded";
  return (
    <button
      type="button"
      className="card overview-cluster m-cluster-mini"
      style={style}
      aria-label={`Show cluster ${summary.cluster}`}
      data-testid={`m-cluster-mini-${summary.cluster}`}
      onClick={() => {
        onSelect(summary.cluster);
      }}
    >
      <span className="m-cluster-mini__name">
        <span className="m-cluster-mini__text">{summary.cluster}</span>
        {settings?.protected && (
          <Lock size={12} strokeWidth={1.7} aria-label="Protected context" role="img" />
        )}
      </span>
      <IssuesPill count={issues} truncated={truncated} connected={connected} />
    </button>
  );
}

function MobileOverviewBody({
  data,
  range,
  now,
  onOpen,
}: {
  data: OverviewData;
  range: WarningRange;
  now: number;
  onOpen: ReturnType<typeof useOverviewDrawer>["open"];
}) {
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const [focused, setFocused] = useState<string | null>(null);
  const issuesByCluster = useIssuesByCluster(data);
  const connect = useConnectCluster();
  const warnings = useMemo(
    () => filterWarnings(data.recentWarnings, range, now),
    [data.recentWarnings, range, now],
  );
  // The full card: the one tapped last, else the selected context, else the first.
  const names = data.clusters.map((c) => c.cluster);
  const fullName =
    [focused, selectedContext].find((n): n is string => n !== null && names.includes(n)) ?? names[0];
  const full = data.clusters.find((c) => c.cluster === fullName);
  const others = data.clusters.filter((c) => c.cluster !== fullName);

  return (
    <>
      {full && (
        <ClusterCard
          summary={full}
          issues={issuesByCluster.get(full.cluster) ?? 0}
          truncated={data.attention.truncated}
          onConnect={connect}
        />
      )}
      {others.length > 0 && (
        <div className="m-cluster-minis" aria-label="Other clusters" role="group">
          {others.map((c) => (
            <MiniClusterCard
              key={c.cluster}
              summary={c}
              issues={issuesByCluster.get(c.cluster) ?? 0}
              truncated={data.attention.truncated}
              onSelect={setFocused}
            />
          ))}
        </div>
      )}
      <AttentionCard items={data.attention.items} truncated={data.attention.truncated} onOpen={onOpen} />
      <WarningsCard
        events={warnings}
        total={data.recentWarnings.length}
        range={range}
        now={now}
        multiCluster={data.clusters.length > 1}
        onOpen={onOpen}
      />
    </>
  );
}

/** Mobile Overview (03-03a): the 02-08 cards in one column; same store and subscription. */
export function MobileOverview({ onOpenPods }: { onOpenPods: () => void }) {
  const status = useOverviewStore((s) => s.status);
  const data = useOverviewStore((s) => s.data);
  const error = useOverviewStore((s) => s.error);
  const isAndroid = useIsAndroid();
  const [range, setRange] = useState<WarningRange>("1h");
  const [now, touchNow] = useOverviewNow();
  useOverviewSubscription();
  const drawer = useOverviewDrawer(touchNow);

  return (
    <div className="m-screen m-overview" data-testid="m-overview">
      <div className="m-titlebar">
        <h1 className="m-titlebar__t">Overview</h1>
        {data !== null && <span className="m-titlebar__s">{plural(data.clusters.length, "cluster")}</span>}
        <select
          className="btn sm m-titlebar__pick"
          aria-label="Warning events time range"
          value={range}
          onChange={(e) => {
            touchNow();
            setRange(e.target.value as WarningRange);
          }}
        >
          {RANGES.map((r) => (
            <option key={r.id} value={r.id}>
              Last {r.label}
            </option>
          ))}
        </select>
      </div>
      <div className="m-scroll">
        <div className="m-pad">
          {status === "error" && data !== null && (
            <p className="overview__stale" role="alert">
              Updates stopped: {error}
            </p>
          )}
          {data !== null && data.clusters.length > 0 ? (
            <MobileOverviewBody data={data} range={range} now={now} onOpen={drawer.open} />
          ) : (
            <OverviewPlaceholder
              onOpenPods={onOpenPods}
              emptyHint="No cluster selected — pick a context with ☰."
            />
          )}
        </div>
      </div>
      {drawer.row && (
        <ResourceDrawer
          key={drawer.row.uid}
          variant="screen"
          remoteOps={!isAndroid}
          row={drawer.row}
          detail={drawer.detail}
          loading={drawer.loading}
          error={drawer.error}
          onClose={drawer.close}
        />
      )}
      <ActionConfirmDialog />
    </div>
  );
}
