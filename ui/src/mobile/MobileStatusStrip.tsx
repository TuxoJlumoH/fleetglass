import { Shield, ShieldAlert } from "lucide-react";
import { useEffect, useMemo } from "react";
import { useShallow } from "zustand/react/shallow";
import {
  accessMode,
  allReady,
  anyPaused,
  PAUSED_TEXT,
  summariseConnections,
} from "../components/connectionStatus";
import { UpdatedAgo } from "../components/StatusBar";
import { useAccessLights, useResourceTabActive } from "../components/useConnectionLights";
import { useAppLifecycleStore } from "../lib/appLifecycle";
import { clustersForQuery, useContextsStore } from "../stores/contexts";
import { useOverviewStore } from "../stores/overview";
import { usePodsStore } from "../stores/pods";

/**
 * Desktop status bar shortened for a phone (mockup v0.3 `.status`): connection, freshness and
 * the read-only mode. Freshness comes from what the screen shows (`freshness`): the overview
 * subscription, the resource table's store, or nothing (Settings, Helm, …).
 */
export function MobileStatusStrip({ freshness }: { freshness: "overview" | "table" | "none" }) {
  const onOverview = freshness === "overview";
  const activeClusterSelection = useContextsStore((s) => s.activeClusterSelection);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const connectionStatus = useContextsStore((s) => s.connectionStatus);
  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const ensureSettings = useContextsStore((s) => s.ensureSettings);
  const clusters = useMemo(
    () => clustersForQuery({ activeClusterSelection, selectedContext }),
    [activeClusterSelection, selectedContext],
  );
  // Access mode, context colour and the read-only banner need the persisted settings of the
  // queried clusters (cluster groups too), as in the desktop StatusBar.
  useEffect(() => {
    void ensureSettings(clusters);
  }, [ensureSettings, clusters]);
  const accessLights = useAccessLights(clusters);
  const resourceTab = useResourceTabActive();
  const overviewUpdated = useOverviewStore((s) => (s.status === "ready" ? s.lastUpdatedAt : null));
  const table = usePodsStore(
    useShallow((s) => {
      if (freshness !== "table" || !resourceTab) return null;
      const ready = allReady(s.access, clusters);
      return { ready, live: s.liveSubscribed, lastUpdatedAt: ready && !s.liveSubscribed ? s.lastUpdatedAt : null };
    }),
  );
  // In the background the backend stops every watcher (03-04a); claim neither "Live" nor
  // "Connected" while the stores say so or the app has told the backend it is hidden.
  const background = useAppLifecycleStore((s) => s.background);
  const storesPaused = usePodsStore((s) => resourceTab && anyPaused(s.access, clusters));
  const paused = background || storesPaused;
  const connection = paused ? null : summariseConnections(clusters, accessLights, connectionStatus);
  const mode = accessMode(clusters, settingsByContext);

  return (
    <footer className="statusbar m-status" role="contentinfo">
      {paused && (
        <span className="statusbar__item" data-testid="status-paused" title="The app is in the background: nothing is watched">
          <span className="dot off" aria-hidden="true" />
          <span className="statusbar__text">{PAUSED_TEXT}</span>
        </span>
      )}
      {connection && (
        <span className="statusbar__item" data-testid="status-connection" title={connection.label}>
          <span className={`dot ${connection.tone}`} aria-hidden="true" />
          <span className="statusbar__text">{connection.text}</span>
        </span>
      )}
      {onOverview && !paused && overviewUpdated !== null && <UpdatedAgo updatedAt={overviewUpdated} />}
      {!paused && table?.ready === true && table.live && (
        <span className="statusbar__item" data-testid="status-updated" data-live="true">
          Live
        </span>
      )}
      {!paused && table?.ready === true && !table.live && table.lastUpdatedAt !== null && (
        <UpdatedAgo updatedAt={table.lastUpdatedAt} />
      )}
      {mode && (
        <div className="statusbar__right">
          <span className="statusbar__item statusbar__mode" data-testid="status-access-mode" data-mode={mode}>
            {mode === "read-write" ? (
              <Shield size={13} strokeWidth={1.7} aria-hidden="true" />
            ) : (
              <ShieldAlert size={13} strokeWidth={1.7} aria-hidden="true" />
            )}
            {mode === "read-only" ? "Read-only" : mode === "mixed" ? "Mixed" : "Read-write"}
          </span>
        </div>
      )}
    </footer>
  );
}
