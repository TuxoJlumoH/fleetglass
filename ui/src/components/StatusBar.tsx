import { Shield, ShieldAlert } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { PortForwardBar } from "./OpsPanels";
import {
  accessMode,
  allReady,
  formatUpdatedAgo,
  metricsSummary,
  summariseConnections,
} from "./connectionStatus";
import { useAccessLights, useResourceTabActive } from "./useConnectionLights";
import { useAppInfoStore } from "../stores/appInfo";
import { clustersForQuery, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { usePodsStore } from "../stores/pods";
import { usePortForwardsStore } from "../stores/portForwards";

const ICON_SIZE = 13;
const ICON_STROKE = 1.7;

/** "Updated Ns ago" for a pull-only table; one 1 s interval while it is shown. */
export function UpdatedAgo({ updatedAt }: { updatedAt: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => {
      setNow(Date.now());
    }, 1000);
    return () => {
      window.clearInterval(id);
    };
  }, []);
  // A fresh update can land between ticks; never show a negative age.
  return (
    <span className="statusbar__item" data-testid="status-updated">
      {formatUpdatedAgo(updatedAt, Math.max(now, updatedAt))}
    </span>
  );
}

/** Live countdown pill for temporarily unlocked contexts (`04-10`). */
export function UnlockedCountdownPill({
  cluster,
  unlockedUntil,
  onLock,
  fallbackModeBadge,
}: {
  cluster: string;
  unlockedUntil: number;
  onLock: (cluster: string) => void;
  fallbackModeBadge?: React.ReactNode;
}) {
  const [secondsLeft, setSecondsLeft] = useState(() =>
    Math.max(0, unlockedUntil - Math.floor(Date.now() / 1000)),
  );

  useEffect(() => {
    const update = () => {
      const remaining = Math.max(0, unlockedUntil - Math.floor(Date.now() / 1000));
      setSecondsLeft(remaining);
      if (remaining === 0) {
        onLock(cluster);
      }
    };
    update();
    const id = window.setInterval(update, 1000);
    return () => {
      window.clearInterval(id);
    };
  }, [cluster, unlockedUntil, onLock]);

  if (secondsLeft <= 0) {
    return fallbackModeBadge ?? null;
  }

  const m = Math.floor(secondsLeft / 60);
  const s = secondsLeft % 60;
  const timeStr = `${String(m)}:${s < 10 ? "0" : ""}${String(s)}`;

  return (
    <span
      className="statusbar__item statusbar__unlocked-pill"
      data-testid="status-unlocked-pill"
      title={`Context ${cluster} is temporarily unlocked for writes`}
    >
      <ShieldAlert size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
      <span>
        Unlocked · {timeStr} left ·{" "}
        <button
          type="button"
          className="statusbar__lock-now-btn"
          data-testid="status-lock-now-btn"
          onClick={(e) => {
            e.stopPropagation();
            onLock(cluster);
          }}
        >
          Lock now
        </button>
      </span>
    </span>
  );
}

/** Modal dialog for unlocking a protected or read-only context (`04-10`). */
export function ContextUnlockModal({
  context,
  isProtected,
  onClose,
  onUnlock,
}: {
  context: string;
  isProtected: boolean;
  onClose: () => void;
  onUnlock: (context: string, confirmedName: string | null) => void;
}) {
  const [confirmName, setConfirmName] = useState("");
  const matches = !isProtected || confirmName === context;

  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div
        className="action-confirm-dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="unlock-dialog-title"
      >
        <header className="action-confirm-dialog__header">
          <h3 id="unlock-dialog-title" className="action-confirm-dialog__title">
            Unlock Context: {context}
          </h3>
          <p className="action-confirm-dialog__note">
            Temporarily unlocks write operations on <strong>{context}</strong> for <strong>15 minutes</strong>.
            The context will automatically re-lock when the countdown expires.
          </p>
        </header>

        {isProtected && (
          <div className="action-confirm-dialog__body">
            <p className="action-confirm-dialog__prompt">
              This context is protected. Type <code>{context}</code> to confirm:
            </p>
            <input
              type="text"
              className="action-confirm-dialog__input"
              data-testid="unlock-confirm-input"
              placeholder={context}
              value={confirmName}
              autoFocus
              onChange={(e) => {
                setConfirmName(e.target.value);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && matches) {
                  onUnlock(context, confirmName);
                  onClose();
                } else if (e.key === "Escape") {
                  onClose();
                }
              }}
            />
          </div>
        )}

        <footer className="action-confirm-dialog__footer">
          <button
            type="button"
            className="action-confirm-dialog__cancel-btn"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            type="button"
            className="action-confirm-dialog__execute-btn"
            data-testid="unlock-confirm-execute-btn"
            disabled={!matches}
            onClick={() => {
              onUnlock(context, isProtected ? confirmName : null);
              onClose();
            }}
          >
            Unlock for 15 minutes
          </button>
        </footer>
      </div>
    </div>
  );
}

export function StatusBar() {
  const version = useAppInfoStore((s) => s.version);
  const forwards = usePortForwardsStore((s) => s.forwards);
  const loadForwards = usePortForwardsStore((s) => s.loadForwards);
  const stopForward = usePortForwardsStore((s) => s.stopForward);
  const activeClusterSelection = useContextsStore((s) => s.activeClusterSelection);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const connectionStatus = useContextsStore((s) => s.connectionStatus);
  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const ensureSettings = useContextsStore((s) => s.ensureSettings);
  const openPalette = useLayoutStore((s) => s.openPalette);
  const unlockContext = useContextsStore((s) => s.unlockContext);
  const relockContext = useContextsStore((s) => s.relockContext);
  const [unlockModalCluster, setUnlockModalCluster] = useState<string | null>(null);

  useEffect(() => {
    void loadForwards();
  }, [loadForwards]);

  // Same clusters the resource tables query.
  const clusters = useMemo(
    () => clustersForQuery({ activeClusterSelection, selectedContext }),
    [activeClusterSelection, selectedContext],
  );
  // Read-only badge (and sidebar swatches) need the persisted settings of the queried clusters.
  useEffect(() => {
    void ensureSettings(clusters);
  }, [ensureSettings, clusters]);

  // The pods store belongs to the resource table; on Helm / Apps / Audit / Plugins tabs its
  // access states and timestamps are left over from an unmounted table, so they are not shown.
  const resourceTab = useResourceTabActive();
  const accessLights = useAccessLights(clusters);
  // Derived primitives so a delta that changes nothing shown here does not re-render the bar.
  const table = usePodsStore(
    useShallow((s) => {
      if (!resourceTab) return null;
      const ready = allReady(s.access, clusters);
      const m = metricsSummary(s.access, clusters);
      return {
        ready,
        live: s.liveSubscribed,
        // While live the bar says "Live"; only pull-only tables need the timestamp.
        lastUpdatedAt: ready && !s.liveSubscribed ? s.lastUpdatedAt : null,
        metricsText: m?.text ?? null,
        metricsTitle: m?.title ?? null,
      };
    }),
  );
  const connection = summariseConnections(clusters, accessLights, connectionStatus);
  const mode = accessMode(clusters, settingsByContext);
  const [onlyCluster] = clusters;

  return (
    <div className="statusbar-area">
      <PortForwardBar
        forwards={forwards}
        onStop={(id) => {
          void stopForward(id);
        }}
      />
      <footer className="statusbar" role="contentinfo">
        {connection && (
          <span className="statusbar__item" data-testid="status-connection" title={connection.label}>
            <span className={`dot ${connection.tone}`} aria-hidden="true" />
            <span className="statusbar__text">{connection.text}</span>
          </span>
        )}
        {clusters.length === 1 && onlyCluster !== undefined && (
          <span className="statusbar__item statusbar__ctx" data-testid="status-context">
            {onlyCluster}
          </span>
        )}
        {table?.metricsText != null && (
          <span
            className="statusbar__item"
            data-testid="status-metrics"
            title={table.metricsTitle ?? undefined}
          >
            <span className="statusbar__text">{table.metricsText}</span>
          </span>
        )}
        {/* Freshness only while every queried store is ready; otherwise the connection item
            already says Loading / Reconnecting / Error. */}
        {table?.ready === true && table.live && (
          <span
            className="statusbar__item"
            data-testid="status-updated"
            data-live="true"
            title="Live: changes stream from the cluster as they happen"
          >
            Live
          </span>
        )}
        {table?.ready === true && !table.live && table.lastUpdatedAt !== null && (
          <UpdatedAgo updatedAt={table.lastUpdatedAt} />
        )}
        <div className="statusbar__right">
          {(() => {
            const activeCluster = clusters.length === 1 && onlyCluster !== undefined ? onlyCluster : null;
            const activeClusterSettings = activeCluster ? settingsByContext[activeCluster] : undefined;
            const unlockedUntil = activeClusterSettings?.unlockedUntil ?? null;

            const modeBadge = mode ? (
              <span
                className="statusbar__item statusbar__mode"
                data-testid="status-access-mode"
                data-mode={mode}
                role={mode === "read-only" && activeCluster ? "button" : undefined}
                tabIndex={mode === "read-only" && activeCluster ? 0 : undefined}
                style={mode === "read-only" && activeCluster ? { cursor: "pointer" } : undefined}
                onClick={() => {
                  if (mode === "read-only" && activeCluster) {
                    setUnlockModalCluster(activeCluster);
                  }
                }}
                onKeyDown={(e) => {
                  if ((e.key === "Enter" || e.key === " ") && mode === "read-only" && activeCluster) {
                    setUnlockModalCluster(activeCluster);
                  }
                }}
                title={
                  mode === "read-only"
                    ? "Read-only context: click to unlock for 15 minutes"
                    : mode === "mixed"
                      ? "Some selected contexts are read-only"
                      : "Read-write context"
                }
              >
                {mode === "read-write" ? (
                  <Shield size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
                ) : (
                  <ShieldAlert size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
                )}
                {mode === "read-only" ? "Read-only" : mode === "mixed" ? "Mixed" : "Read-write"}
              </span>
            ) : null;

            if (activeCluster && unlockedUntil !== null) {
              return (
                <UnlockedCountdownPill
                  cluster={activeCluster}
                  unlockedUntil={unlockedUntil}
                  onLock={(c) => {
                    void relockContext(c);
                  }}
                  fallbackModeBadge={modeBadge}
                />
              );
            }

            return modeBadge;
          })()}
          <button
            type="button"
            className="statusbar__item statusbar__hint"
            data-testid="status-commands"
            aria-keyshortcuts="Control+K"
            onClick={openPalette}
          >
            <kbd>Ctrl K</kbd> Commands
          </button>
          <span className="statusbar__item statusbar__version" data-testid="app-version">
            {version === null ? "version: —" : `v${version}`}
          </span>
        </div>
      </footer>
      {unlockModalCluster && (
        <ContextUnlockModal
          context={unlockModalCluster}
          isProtected={settingsByContext[unlockModalCluster]?.protected ?? false}
          onClose={() => {
            setUnlockModalCluster(null);
          }}
          onUnlock={(ctx, confirmed) => {
            void unlockContext(ctx, confirmed);
          }}
        />
      )}
    </div>
  );
}
