import { Box, Check, CircleX, ScrollText, SquareTerminal, TriangleAlert } from "lucide-react";
import { useState } from "react";
import type {
  ConditionSummary,
  ContainerSummary,
  PodSummary as PodSummaryData,
  RelatedRef,
  ResourceDetail,
} from "../generated/bindings";
import type { Row } from "../stores/pods";
import { useResourceDetailStore } from "../stores/resourceDetail";
import { ActivePortForwardsCard } from "./ActivePortForwards";
import { ICON_SIZE, ICON_STROKE, KindIcon } from "./KindIcon";
import { PortForwardForm } from "./PortForwardForm";
import type { DrawerTab } from "./ResourceDrawer";
import type { StatusTone } from "./statusTone";

export interface PodSummaryProps {
  detail: ResourceDetail;
  row: Row;
  pod?: PodSummaryData | null;
  onOpenRelated?: (ref: RelatedRef) => void;
  onSelectTab?: (tab: DrawerTab, containerName?: string) => void;
  onPortForward?: (containerName: string, port?: number) => void;
  /**
   * Container whose port-forward form is open. When given (also `null`), the parent owns this
   * state (the drawer header's Forward button opens it); otherwise the summary keeps its own.
   */
  portForwardContainer?: string | null;
  /** False hides Shell and Port-forward (not available on Android 0.3). */
  remoteOps?: boolean;
  onOpenInBrowser?: ((url: string) => void | Promise<void>) | undefined;
}

function conditionTooltip(cond: ConditionSummary): string {
  const parts: string[] = [];
  if (cond.reason) parts.push(`Reason: ${cond.reason}`);
  if (cond.message) parts.push(`Message: ${cond.message}`);
  if (cond.lastTransition) parts.push(`Last transition: ${cond.lastTransition}`);
  return parts.length > 0 ? parts.join("\n") : `Status: ${cond.status}`;
}

function containerTone(c: ContainerSummary): StatusTone | null {
  switch (c.state.toLowerCase()) {
    case "running":
      return "ok";
    case "waiting":
      return "warn";
    case "terminated":
      return c.lastTermination ? "err" : "off";
    default:
      return null;
  }
}

/** CPU in millicores ("250m", "0.5", "2"); `null` when the quantity cannot be read. */
function parseCpu(q: string): number | null {
  const m = /^(\d+(?:\.\d+)?)(m?)$/.exec(q.trim());
  if (!m?.[1]) return null;
  const n = Number(m[1]);
  return m[2] === "m" ? n : n * 1000;
}

const MEMORY_UNITS: Record<string, number> = {
  "": 1,
  k: 1e3,
  M: 1e6,
  G: 1e9,
  T: 1e12,
  Ki: 1024,
  Mi: 1024 ** 2,
  Gi: 1024 ** 3,
  Ti: 1024 ** 4,
};

/** Memory in bytes ("128Mi", "1G", "1048576"); `null` when the quantity cannot be read. */
function parseMemory(q: string): number | null {
  const m = /^(\d+(?:\.\d+)?)([kMGT]i?|Ki)?$/.exec(q.trim());
  if (!m?.[1]) return null;
  const unit = MEMORY_UNITS[m[2] ?? ""];
  return unit === undefined ? null : Number(m[1]) * unit;
}

/**
 * Request / limit of one resource. The summary has no per-container usage, so nothing here may
 * read as usage: the request's share of the limit is spelled out as text, and the bar beside it
 * uses only the faint request fill on a dashed track (`.bar--reqlim`). With only one side set
 * there is no share, so text only and no bar.
 */
function RequestLimit({
  label,
  request,
  limit,
  parse,
}: {
  label: string;
  request: string | null | undefined;
  limit: string | null | undefined;
  parse: (q: string) => number | null;
}) {
  const req = request ? parse(request) : null;
  const lim = limit ? parse(limit) : null;
  const share =
    req !== null && lim !== null && lim > 0
      ? Math.min(100, Math.max(0, Math.round((req / lim) * 100)))
      : null;
  return (
    <div>
      <div className="use">
        <div className="v">
          {`${request || "—"} / ${limit || "—"}`}
          {share !== null && <span>{`req ${String(share)}% of lim`}</span>}
        </div>
        {share !== null && (
          <div className="bar bar--reqlim" aria-hidden="true">
            <i className="req" style={{ width: `${String(share)}%` }} />
          </div>
        )}
      </div>
      <div className="pod-summary__grid-label pod-summary__grid-label--below">
        {label} <span className="pod-summary__hint">req / lim</span>
      </div>
    </div>
  );
}

export function PodSummary({
  detail,
  row,
  pod: podProp,
  onOpenRelated,
  onSelectTab,
  onPortForward,
  portForwardContainer,
  remoteOps = true,
  onOpenInBrowser,
}: PodSummaryProps) {
  const pod = podProp ?? detail.pod;
  const [ownPfContainer, setOwnPfContainer] = useState<string | null>(null);
  const controlled = portForwardContainer !== undefined;
  const activePfContainer = controlled ? portForwardContainer : ownPfContainer;

  if (!pod) return null;

  const handleOpenRelated = (ref: RelatedRef) => {
    if (onOpenRelated) {
      onOpenRelated(ref);
      return;
    }
    const targetRow: Row = {
      uid: ref.uid || `${ref.kind}/${ref.namespace ?? row.namespace ?? ""}/${ref.name}`,
      cluster: row.cluster,
      kind: ref.kind,
      namespace: ref.namespace ?? (ref.kind === "Node" ? null : row.namespace),
      name: ref.name,
      status: "unknown",
      statusReason: "",
      cells: [],
      labels: [],
      custom: [],
    };
    void useResourceDetailStore.getState().open(targetRow);
  };

  const controller = detail.related.find((r) => {
    const rel = r.relationship.toLowerCase();
    return rel === "owner" || rel.includes("owner") || rel.includes("controlled");
  });

  const handlePortForwardClick = (c: ContainerSummary) => {
    const port = c.ports[0]?.containerPort;
    if (!controlled) setOwnPfContainer((prev) => (prev === c.name ? null : c.name));
    onPortForward?.(c.name, port);
  };

  const iconProps = { size: ICON_SIZE, strokeWidth: ICON_STROKE, "aria-hidden": true } as const;

  return (
    <div className="pod-summary" data-testid="pod-summary">
      {/* 1. OVERVIEW CARD */}
      <div className="card" data-testid="pod-overview-card">
        <h4>OVERVIEW</h4>
        <div className="body">
          <dl className="kv">
            <dt>Node</dt>
            <dd>
              {pod.node ? (
                <button
                  type="button"
                  className="link-button"
                  onClick={() => {
                    handleOpenRelated({
                      kind: "Node",
                      name: pod.node,
                      uid: "",
                      relationship: "node",
                    });
                  }}
                >
                  {pod.node}
                </button>
              ) : (
                "—"
              )}
            </dd>

            <dt>Pod IP</dt>
            <dd className="mono">{pod.podIp || "—"}</dd>

            <dt>QoS class</dt>
            <dd>{pod.qosClass || "—"}</dd>

            <dt>Service account</dt>
            <dd>
              {pod.serviceAccount ? (
                <button
                  type="button"
                  className="link-button"
                  onClick={() => {
                    handleOpenRelated({
                      kind: "ServiceAccount",
                      name: pod.serviceAccount,
                      namespace: row.namespace,
                      uid: "",
                      relationship: "serviceAccount",
                    });
                  }}
                >
                  {pod.serviceAccount}
                </button>
              ) : (
                "—"
              )}
            </dd>

            <dt>Controlled by</dt>
            <dd>
              {controller ? (
                <button
                  type="button"
                  className="link-button"
                  onClick={() => {
                    handleOpenRelated(controller);
                  }}
                >
                  {controller.kind} {controller.name}
                </button>
              ) : (
                "—"
              )}
            </dd>

            {pod.priorityClass != null && pod.priorityClass !== "" && (
              <>
                <dt>Priority</dt>
                <dd>{pod.priorityClass}</dd>
              </>
            )}
          </dl>
        </div>
      </div>

      {remoteOps && row.namespace && (
        <ActivePortForwardsCard
          context={row.cluster}
          namespace={row.namespace}
          pod={row.name}
          onOpenInBrowser={onOpenInBrowser}
        />
      )}

      {/* 2. CONDITIONS CARD */}
      <div className="card" data-testid="pod-conditions-card">
        <h4>CONDITIONS</h4>
        <div className="body pod-summary__chips">
          {pod.conditions.length === 0 ? (
            <span className="resource-drawer__empty">No conditions</span>
          ) : (
            pod.conditions.map((cond) => {
              const isOk = cond.status === "True";
              const isWarn = cond.status === "Unknown";
              const chipClass = isOk ? "chip ok" : isWarn ? "chip warn" : "chip err";
              const Icon = isOk ? Check : isWarn ? TriangleAlert : CircleX;

              return (
                <span
                  key={cond.type}
                  className={chipClass}
                  title={conditionTooltip(cond)}
                  data-status={cond.status}
                  data-testid={`condition-${cond.type}`}
                >
                  <Icon {...iconProps} />
                  <span>{cond.type}</span>
                  {!isOk && <span className="k">({cond.status})</span>}
                </span>
              );
            })
          )}
        </div>
      </div>

      {/* 3. CONTAINERS CARD */}
      <div className="card" data-testid="pod-containers-card">
        <h4>
          CONTAINERS<span className="r">{pod.containers.length}</span>
        </h4>
        <div className="body">
          {pod.containers.map((c) => {
            const displayName = c.init ? `Init: ${c.name}` : c.name;
            const tone = containerTone(c);
            const portsText =
              c.ports.length > 0
                ? c.ports.map((p) => `${String(p.containerPort)}/${p.protocol || "TCP"}`).join(", ")
                : "—";

            return (
              <div
                key={c.name}
                className="pod-summary__container-item"
                data-testid={`container-${c.name}`}
              >
                <div className="pod-summary__container-header">
                  <Box {...iconProps} className="pod-summary__container-icon" />
                  <b className="pod-summary__container-name">{displayName}</b>
                  <span className={tone ? `pill ${tone}` : "pill"}>
                    {c.stateReason ? `${c.state} (${c.stateReason})` : c.state}
                  </span>
                  <span className="mono pod-summary__image" title={c.image}>
                    {c.image}
                  </span>
                  <div className="pod-summary__container-actions">
                    <button
                      type="button"
                      className="btn sm"
                      onClick={() => {
                        onSelectTab?.("Logs", c.name);
                      }}
                    >
                      <ScrollText {...iconProps} />
                      Logs
                    </button>
                    {remoteOps && (
                      <button
                        type="button"
                        className="btn sm"
                        onClick={() => {
                          onSelectTab?.("Terminal", c.name);
                        }}
                      >
                        <SquareTerminal {...iconProps} />
                        Shell
                      </button>
                    )}
                  </div>
                </div>

                <div className="pod-summary__container-grid">
                  <div>
                    <div className="pod-summary__grid-label">Ready</div>
                    <div>
                      {c.ready ? "1/1" : "0/1"} · {c.restarts} {c.restarts === 1 ? "restart" : "restarts"}
                    </div>
                  </div>
                  <div>
                    <div className="pod-summary__grid-label">Ports</div>
                    <div className="pod-summary__ports">
                      <span>{portsText}</span>
                      {remoteOps && (
                        <button
                          type="button"
                          className="link-button"
                          aria-expanded={activePfContainer === c.name}
                          onClick={() => {
                            handlePortForwardClick(c);
                          }}
                        >
                          Port-forward
                        </button>
                      )}
                    </div>
                  </div>
                  <RequestLimit
                    label="CPU"
                    request={c.requests.cpu}
                    limit={c.limits.cpu}
                    parse={parseCpu}
                  />
                  <RequestLimit
                    label="Memory"
                    request={c.requests.memory}
                    limit={c.limits.memory}
                    parse={parseMemory}
                  />
                  {c.lastTermination && (
                    <div className="pod-summary__last-termination">
                      <div className="pod-summary__grid-label">Last termination</div>
                      <div className="lr">
                        <span className="r">{c.lastTermination}</span>
                      </div>
                    </div>
                  )}
                </div>

                {remoteOps && activePfContainer === c.name && (
                  <div
                    className="pod-summary__port-forward-section"
                    data-testid={`port-forward-section-${c.name}`}
                  >
                    <PortForwardForm
                      key={c.name}
                      context={row.cluster}
                      namespace={row.namespace ?? "default"}
                      pod={row.name}
                      yaml={detail.yaml}
                      initialRemotePort={c.ports[0]?.containerPort}
                    />
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>

      {/* 4. USES CARD */}
      {pod.uses.length > 0 && (
        <div className="card" data-testid="pod-uses-card">
          <h4>USES</h4>
          <div className="body pod-summary__chips">
            {pod.uses.map((ref) => (
              <button
                key={`${ref.kind}-${ref.name}`}
                type="button"
                className="chip link"
                onClick={() => {
                  handleOpenRelated(ref);
                }}
                data-testid={`uses-chip-${ref.kind}-${ref.name}`}
              >
                <KindIcon kind={ref.kind} />
                <span className="k">{ref.kind}</span>
                <span>{ref.name}</span>
              </button>
            ))}
          </div>
        </div>
      )}

      {/* 5. LABELS CARD */}
      <div className="card" data-testid="pod-labels-card">
        <h4>LABELS</h4>
        <div className="body pod-summary__chips">
          {row.labels.length > 0 ? (
            row.labels.map(([k, v]) => (
              <span key={`${k}=${v}`} className="chip" data-testid="label-chip">
                <span className="k">{k}=</span>
                <span>{v}</span>
              </span>
            ))
          ) : (
            <span className="resource-drawer__empty">None</span>
          )}
        </div>
      </div>
    </div>
  );
}
