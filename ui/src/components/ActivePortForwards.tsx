import { useMemo } from "react";
import type { PortForwardInfo_Serialize } from "../generated/bindings";
import { usePortForwardsStore } from "../stores/portForwards";
import { CopyUrlButton, OpenInBrowserButton } from "./OpsPanels";

export interface ActivePortForwardsProps {
  context: string;
  namespace: string;
  pod: string;
  onOpenInBrowser?: ((url: string) => void | Promise<void>) | undefined;
}

function usePodForwards(context: string, namespace: string, pod: string): PortForwardInfo_Serialize[] {
  const forwards = usePortForwardsStore((s) => s.forwards);
  return useMemo(
    () =>
      forwards.filter(
        (pf) =>
          pf.context === context && pf.namespace === namespace && pf.pod === pod && !pf.error,
      ),
    [forwards, context, namespace, pod],
  );
}

/** Active port-forwards of one Pod with Open in browser / Copy actions (`05-05`). */
export function ActivePortForwards({
  context,
  namespace,
  pod,
  onOpenInBrowser,
}: ActivePortForwardsProps) {
  const mine = usePodForwards(context, namespace, pod);
  if (mine.length === 0) return null;
  return (
    <div className="resource-drawer__active-pfs" data-testid="active-pod-forwards">
      {mine.map((pf) => (
        <div
          key={pf.id}
          className="resource-drawer__active-pf-item"
          data-testid={`active-pf-${String(pf.id)}`}
        >
          <span className="mono">
            {pf.localPort}→{pf.remotePort}
          </span>
          <OpenInBrowserButton url={pf.browserUrl} onOpen={onOpenInBrowser} />
          <CopyUrlButton url={pf.browserUrl} showUrl={false} />
        </div>
      ))}
    </div>
  );
}

/** The list wrapped in a summary card; renders nothing without active forwards. */
export function ActivePortForwardsCard(props: ActivePortForwardsProps) {
  const mine = usePodForwards(props.context, props.namespace, props.pod);
  if (mine.length === 0) return null;
  return (
    <div className="card" data-testid="pod-active-forwards-card">
      <h4>
        ACTIVE PORT-FORWARDS<span className="r">{mine.length}</span>
      </h4>
      <div className="body">
        <ActivePortForwards {...props} />
      </div>
    </div>
  );
}
