import { Channel, isTauri } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { commands, type DeviceCodeInfo, type SavedPortForward } from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";
import { usePortForwardsStore } from "../stores/portForwards";
import {
  getExecClient,
  type ExecClient,
} from "./execTerminal";
import { TerminalEmulator } from "./TerminalEmulator";
import { TerminalStream } from "./terminalStream";

export type { ExecClient };

export interface ExecShellPanelProps {
  pod: string;
  containers: string[];
  readOnly: boolean;
  onStartExec?: (container: string, shell: string) => void;
  cluster?: string;
  namespace?: string;
  execClient?: ExecClient;
  onSessionChange?: (sessionId: number | null) => void;
  isProtected?: boolean;
  mode?: "exec" | "attach";
}

const SHELL_OPTIONS = ["/bin/sh", "/bin/bash"] as const;

export function ExecShellPanel({
  pod,
  containers,
  readOnly,
  onStartExec,
  cluster = "",
  namespace = "default",
  execClient,
  onSessionChange,
  isProtected: isProtectedProp,
  mode: initialMode = "exec",
}: ExecShellPanelProps) {
  const [userMode, setUserMode] = useState<"exec" | "attach" | null>(null);
  const [prevInitialMode, setPrevInitialMode] = useState(initialMode);

  if (prevInitialMode !== initialMode) {
    setPrevInitialMode(initialMode);
    setUserMode(null);
  }

  const currentMode = userMode ?? initialMode;
  const setCurrentMode = (m: "exec" | "attach") => {
    setUserMode(m);
  };

  const isAttach = currentMode === "attach";
  const actionNoun = isAttach ? "Attach" : "Exec";
  const client = execClient ?? getExecClient();
  const storeIsProtected = useContextsStore((s) => {
    if (!cluster) return false;
    return s.settingsByContext[cluster]?.protected ?? false;
  });
  const protectedContext = isProtectedProp ?? storeIsProtected;
  const [selectedContainer, setSelectedContainer] = useState<string>(containers[0] ?? "");
  const [selectedShell, setSelectedShell] = useState<string>(SHELL_OPTIONS[0]);
  const [sessionId, setSessionId] = useState<number | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [execError, setExecError] = useState<string | null>(null);
  // Bounded output pipe for the current terminal; kept across reconnects so history stays visible.
  const [stream, setStream] = useState<TerminalStream | null>(null);
  const [confirmPodName, setConfirmPodName] = useState("");
  const sessionIdRef = useRef<number | null>(null);

  const effectiveContainer =
    containers.includes(selectedContainer) ? selectedContainer : (containers[0] ?? "");

  const isConfirmed = !protectedContext || confirmPodName === pod;

  const [prevTarget, setPrevTarget] = useState({ pod, cluster });

  if (prevTarget.pod !== pod || prevTarget.cluster !== cluster) {
    setPrevTarget({ pod, cluster });
    setConfirmPodName("");
  }

  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  useEffect(() => {
    return () => {
      if (sessionIdRef.current !== null) {
        const idToStop = sessionIdRef.current;
        sessionIdRef.current = null;
        void client.stopExecSession(idToStop).catch(() => undefined);
      }
    };
  }, [client]);

  const handleExternalTerminal = async () => {
    if (readOnly || effectiveContainer.length === 0 || !isConfirmed) return;
    try {
      await commands.openExternalTerminal({
        target: {
          kind: "podExec",
          cluster,
          namespace,
          pod,
          container: effectiveContainer || null,
          shell: isAttach ? "attach" : selectedShell,
        },
        confirmedName: protectedContext ? confirmPodName : null,
      });
    } catch (err) {
      setExecError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleConnect = async () => {
    if (
      readOnly ||
      effectiveContainer.length === 0 ||
      connecting ||
      sessionId !== null ||
      !isConfirmed
    ) {
      return;
    }
    onStartExec?.(effectiveContainer, isAttach ? "attach" : selectedShell);

    setConnecting(true);
    setExecError(null);

    const previousStream = stream;
    const out = previousStream ?? new TerminalStream();
    setStream(out);

    try {
      const onChunk = (chunk: string) => {
        out.write(chunk);
      };
      const channel = client.createChannel
        ? client.createChannel(onChunk)
        : new Channel<string>(onChunk);

      const confirmedName = protectedContext ? confirmPodName : null;
      const res = isAttach
        ? await (client.startAttachSession ?? commands.startAttachSession)(
            cluster,
            namespace,
            pod,
            effectiveContainer || null,
            confirmedName,
            channel,
          )
        : await client.startExecSession(
            cluster,
            namespace,
            pod,
            effectiveContainer || null,
            [selectedShell],
            confirmedName,
            channel,
          );

      setConnecting(false);
      if (res.status === "ok") {
        setSessionId(res.data);
        sessionIdRef.current = res.data;
        onSessionChange?.(res.data);
      } else {
        setStream(previousStream);
        setExecError(res.error.message || `${actionNoun} failed: ${res.error.kind}`);
      }
    } catch (err: unknown) {
      setConnecting(false);
      setStream(previousStream);
      if (onStartExec && !execClient && !isTauri()) {
        return;
      }
      setExecError(
        typeof err === "object" && err !== null && "message" in err
          ? String(err.message)
          : String(err),
      );
    }
  };

  const handleDisconnect = async () => {
    if (sessionId === null) return;
    const currentId = sessionId;
    sessionIdRef.current = null;
    setSessionId(null);
    setConfirmPodName("");
    onSessionChange?.(null);
    try {
      await client.stopExecSession(currentId);
    } catch {
      // Ignore stop errors on manual disconnect
    }
    stream?.write("\r\n[Session disconnected]\r\n");
  };

  const confirmGuard =
    sessionId === null && protectedContext && !readOnly ? (
      <div
        className="exec-panel__guard"
        data-testid={isAttach ? "attach-protected-guard" : "exec-protected-guard"}
      >
        <label className="exec-panel__guard-label" htmlFor="exec-confirm-input">
          {`Protected context '${cluster}'. Type '${pod}' to confirm ${isAttach ? "process attach" : "shell access"}:`}
        </label>
        <input
          id="exec-confirm-input"
          type="text"
          data-testid="exec-confirm-input"
          className="exec-panel__confirm-input"
          placeholder={pod}
          autoComplete="off"
          spellCheck={false}
          value={confirmPodName}
          onChange={(e) => {
            setConfirmPodName(e.target.value);
          }}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter" && confirmPodName === pod && !connecting) {
              e.preventDefault();
              void handleConnect();
            }
          }}
        />
      </div>
    ) : null;

  const placeholder =
    confirmGuard ??
    (stream !== null ? null : readOnly ? (
      <div className="exec-panel__placeholder">{`${actionNoun} disabled: context is read-only`}</div>
    ) : (
      <div className="exec-panel__placeholder">
        {isAttach
          ? "Select a container, then click Attach to Process."
          : "Select a container and shell, then click Connect Shell."}
      </div>
    ));

  return (
    <section aria-label={`${actionNoun} for ${pod}`} className="exec-panel">
      <div className="exec-panel__toolbar">
        <span className="exec-panel__pod">{pod}</span>

        <label className="exec-panel__field">
          <span className="visually-hidden">Mode</span>
          <select
            aria-label="Select mode"
            className="exec-panel__select"
            value={currentMode}
            disabled={sessionId !== null}
            onChange={(e) => {
              setCurrentMode(e.target.value as "exec" | "attach");
            }}
          >
            <option value="exec">Shell</option>
            <option value="attach">Attach (PID 1)</option>
          </select>
        </label>

        <label className="exec-panel__field">
          <span className="visually-hidden">Container</span>
          <select
            aria-label="Select container"
            className="exec-panel__select"
            value={effectiveContainer}
            disabled={sessionId !== null}
            onChange={(e) => {
              setSelectedContainer(e.target.value);
            }}
          >
            {containers.map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
        </label>

        {!isAttach && (
          <label className="exec-panel__field">
            <span className="visually-hidden">Shell</span>
            <select
              aria-label="Select shell"
              className="exec-panel__select"
              value={selectedShell}
              disabled={sessionId !== null}
              onChange={(e) => {
                setSelectedShell(e.target.value);
              }}
            >
              {SHELL_OPTIONS.map((sh) => (
                <option key={sh} value={sh}>
                  {sh}
                </option>
              ))}
            </select>
          </label>
        )}

        {sessionId === null ? (
          <>
            <button
              type="button"
              data-testid={isAttach ? "attach-connect-btn" : "exec-connect-btn"}
              className="exec-panel__connect-btn"
              disabled={readOnly || effectiveContainer.length === 0 || connecting || !isConfirmed}
              onClick={() => {
                void handleConnect();
              }}
            >
              {connecting ? "Connecting…" : isAttach ? "Attach to Process" : "Connect Shell"}
            </button>
            <button
              type="button"
              data-testid="exec-external-terminal-btn"
              className="exec-panel__ext-term-btn"
              disabled={readOnly || effectiveContainer.length === 0 || connecting || !isConfirmed}
              onClick={() => {
                void handleExternalTerminal();
              }}
            >
              External Terminal
            </button>
          </>
        ) : (
          <button
            type="button"
            data-testid={isAttach ? "attach-disconnect-btn" : "exec-disconnect-btn"}
            className="exec-panel__disconnect-btn"
            onClick={() => {
              void handleDisconnect();
            }}
          >
            Disconnect
          </button>
        )}

        {sessionId !== null && (
          <span className="exec-panel__status" data-testid={isAttach ? "attach-status" : "exec-status"}>
            <span className="exec-panel__status-dot" aria-hidden="true" />
            Connected
          </span>
        )}
      </div>

      <div className="exec-panel__preview" data-testid={isAttach ? "attach-preview" : "exec-preview"}>
        <span className="exec-panel__preview-label">kubectl preview:</span>
        <code className="exec-panel__preview-cmd">
          {isAttach
            ? `kubectl ${cluster ? `--context ${cluster} ` : ""}-n ${namespace} attach pod/${pod}${effectiveContainer ? ` -c ${effectiveContainer}` : ""} -it`
            : `kubectl ${cluster ? `--context ${cluster} ` : ""}-n ${namespace} exec -it ${pod}${effectiveContainer ? ` -c ${effectiveContainer}` : ""} -- ${selectedShell}`}
        </code>
      </div>

      {readOnly && (
        <div
          role="alert"
          data-testid={isAttach ? "attach-readonly-notice" : "exec-readonly-notice"}
          className="exec-panel__readonly"
        >
          {`${actionNoun} disabled: context is read-only`}
        </div>
      )}

      {execError && (
        <div
          role="alert"
          data-testid={isAttach ? "attach-error-notice" : "exec-error-notice"}
          className="error-state exec-panel__error"
        >
          {execError}
        </div>
      )}

      <TerminalEmulator
        stream={stream}
        sessionId={sessionId}
        testId={isAttach ? "attach-terminal" : "exec-terminal"}
        ariaLabel={`${actionNoun} terminal for ${pod}`}
        onStdin={(data) => {
          if (sessionId !== null) {
            void client.writeExecStdin(sessionId, data).catch(() => undefined);
          }
        }}
        onResize={(cols, rows) => {
          if (sessionId !== null && client.resizeExecSession) {
            void client.resizeExecSession(sessionId, cols, rows).catch(() => undefined);
          }
        }}
        placeholder={placeholder}
      />
    </section>
  );
}

export interface PortForwardItem {
  id: number;
  context: string;
  namespace: string;
  pod: string;
  remotePort: number;
  localPort: number;
  browserUrl: string;
  activeConnections?: number;
  totalConnections?: number;
}

export interface PortForwardBarProps {
  forwards: PortForwardItem[];
  onStop: (id: number) => void;
  onStart?: (localPort: number, remotePort: number) => void;
  onOpenInBrowser?: (url: string) => void | Promise<void>;
  saved?: SavedPortForward[];
  onBookmarkToggle?: (forward: PortForwardItem) => void;
  onStartSaved?: (saved: SavedPortForward) => void;
  onDeleteSaved?: (id: string) => void;
}

/**
 * The webview has no new-window handler and the app ships no opener plugin, so a
 * `target=_blank` link would do nothing. Show the local URL and copy it instead.
 */
export function CopyUrlButton({
  url,
  showUrl = true,
}: {
  url: string;
  showUrl?: boolean;
}) {
  const [status, setStatus] = useState<"idle" | "copied" | "failed">("idle");
  const copy = () => {
    // `navigator.clipboard` is missing in insecure contexts / old webviews.
    if (typeof navigator.clipboard === "undefined") {
      setStatus("failed");
      return;
    }
    navigator.clipboard.writeText(url).then(
      () => {
        setStatus("copied");
      },
      () => {
        setStatus("failed");
      },
    );
  };
  return (
    <>
      {showUrl && <code className="pf-bar__url">{url}</code>}
      <button
        type="button"
        className="pf-bar__link"
        aria-label={`Copy URL ${url}`}
        onClick={copy}
      >
        {status === "copied" ? "Copied" : "Copy URL"}
      </button>
      {status === "failed" ? (
        <span role="alert" className="pf-bar__copy-error">
          Could not copy; open {url} in your browser
        </span>
      ) : null}
    </>
  );
}

/**
 * Safely launches the URL in the system default browser via Tauri IPC (`05-05`).
 */
export function OpenInBrowserButton({
  url,
  onOpen,
}: {
  url: string;
  onOpen?: ((url: string) => void | Promise<void>) | undefined;
}) {
  const [error, setError] = useState<string | null>(null);

  const handleClick = async () => {
    setError(null);
    try {
      if (onOpen) {
        await onOpen(url);
      } else {
        const res = await commands.openInBrowser(url);
        if (res.status === "error") {
          setError(res.error.message);
        }
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <>
      <button
        type="button"
        className="pf-bar__open-btn"
        aria-label={`Open in browser ${url}`}
        title={`Open ${url} in default browser`}
        onClick={() => {
          void handleClick();
        }}
      >
        Open in browser
      </button>
      {error !== null && (
        <span role="alert" className="pf-bar__copy-error" title={error}>
          Could not open: {error}
        </span>
      )}
    </>
  );
}

export function PortForwardBar({
  forwards,
  onStop,
  onOpenInBrowser,
  saved: savedProp,
  onBookmarkToggle,
  onStartSaved,
  onDeleteSaved,
}: PortForwardBarProps) {
  const storeSaved = usePortForwardsStore((s) => s.saved);
  const saveBookmark = usePortForwardsStore((s) => s.saveBookmark);
  const deleteBookmark = usePortForwardsStore((s) => s.deleteBookmark);
  const startSavedForward = usePortForwardsStore((s) => s.startSavedForward);

  const saved = savedProp ?? storeSaved;

  return (
    <section aria-label="Active port forwards" className="pf-bar">
      {forwards.length === 0 ? (
        <span className="pf-bar__empty">No active port forwards</span>
      ) : (
        <ul className="pf-bar__list">
          {forwards.map((f) => {
            const isBookmarked = saved.some(
              (s) =>
                s.context === f.context &&
                s.namespace === f.namespace &&
                s.targetName === f.pod &&
                s.containerPort === f.remotePort,
            );
            return (
              <li
                key={f.id}
                data-testid={`pf-item-${String(f.id)}`}
                className="pf-bar__item"
              >
                <button
                  type="button"
                  className={`pf-bar__star-btn ${isBookmarked ? "pf-bar__star-btn--saved" : ""}`}
                  aria-label={isBookmarked ? `Remove bookmark for ${f.pod}` : `Bookmark ${f.pod}`}
                  title={isBookmarked ? "Remove bookmark" : "Save as bookmark"}
                  onClick={() => {
                    if (onBookmarkToggle) {
                      onBookmarkToggle(f);
                    } else if (isBookmarked) {
                      const match = saved.find(
                        (s) =>
                          s.context === f.context &&
                          s.namespace === f.namespace &&
                          s.targetName === f.pod &&
                          s.containerPort === f.remotePort,
                      );
                      if (match) void deleteBookmark(match.id);
                    } else {
                      void saveBookmark({
                        id: `${f.context}/${f.namespace}/pod/${f.pod}/${String(f.remotePort)}`,
                        context: f.context,
                        namespace: f.namespace,
                        targetKind: "pod",
                        targetName: f.pod,
                        containerPort: f.remotePort,
                        localPort: f.localPort,
                        label: `${f.pod}:${String(f.remotePort)}`,
                      });
                    }
                  }}
                >
                  {isBookmarked ? "★" : "☆"}
                </button>
                <span className="pf-bar__target">
                  {f.context}/{f.namespace}/{f.pod} {String(f.localPort)}→{String(f.remotePort)}
                </span>
                {f.activeConnections !== undefined && (
                  <span
                    className={`pf-bar__conns ${f.activeConnections > 0 ? "pf-bar__conns--active" : ""}`}
                    data-testid={`pf-conns-${String(f.id)}`}
                    title={`${String(f.totalConnections ?? 0)} total connections`}
                  >
                    {`${String(f.activeConnections)} active`}
                  </span>
                )}
                <CopyUrlButton url={f.browserUrl} />
                <OpenInBrowserButton url={f.browserUrl} onOpen={onOpenInBrowser} />
                <button
                  type="button"
                  aria-label={`Stop forward ${String(f.localPort)}`}
                  className="pf-bar__stop-btn"
                  onClick={() => {
                    onStop(f.id);
                  }}
                >
                  Stop
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {saved.length > 0 && (
        <div className="pf-bar__saved" data-testid="pf-saved-list">
          <span className="pf-bar__saved-title">Saved:</span>
          {saved.map((s) => {
            const active = forwards.find(
              (f) =>
                f.context === s.context &&
                f.namespace === s.namespace &&
                (f.remotePort === s.containerPort || f.localPort === s.localPort),
            );
            return (
              <div
                key={s.id}
                className="pf-bar__saved-item"
                data-testid={`pf-saved-${s.id}`}
              >
                <span
                  className="pf-bar__saved-label"
                  title={`${s.context}/${s.namespace}/${s.targetKind}/${s.targetName} (${String(s.localPort)}→${String(s.containerPort)})`}
                >
                  {s.label || `${s.targetName}:${String(s.containerPort)}`}
                </span>
                {active ? (
                  <button
                    type="button"
                    className="pf-bar__stop-btn"
                    aria-label={`Stop forward ${String(active.localPort)}`}
                    onClick={() => {
                      onStop(active.id);
                    }}
                  >
                    Stop
                  </button>
                ) : (
                  <button
                    type="button"
                    className="pf-bar__start-btn"
                    aria-label={`Start saved forward ${s.label || s.targetName}`}
                    onClick={() => {
                      if (onStartSaved) {
                        onStartSaved(s);
                      } else {
                        void startSavedForward(s);
                      }
                    }}
                  >
                    Start
                  </button>
                )}
                <button
                  type="button"
                  className="pf-bar__delete-saved-btn"
                  aria-label={`Delete saved forward ${s.label || s.targetName}`}
                  onClick={() => {
                    if (onDeleteSaved) {
                      onDeleteSaved(s.id);
                    } else {
                      void deleteBookmark(s.id);
                    }
                  }}
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

export type ResourceAccessState =
  | { state: "ready" }
  | { state: "forbidden"; resource: string; namespace: string | null; message: string }
  | { state: "notFound"; resource: string; message: string }
  | {
      state: "authExpired";
      provider: string;
      hint: string;
      loginCommand: string;
      deviceCode?: DeviceCodeInfo | null;
      partialRows?: number | null;
    };

export type MetricsAvailabilityState =
  | { state: "available" }
  | { state: "notInstalled"; message: string };

export interface ClusterNoticeBannerProps {
  accessState?: ResourceAccessState;
  metricsState?: MetricsAvailabilityState;
  onRecoverAuth?: () => void;
}

export function ClusterNoticeBanner({
  accessState,
  metricsState,
  onRecoverAuth,
}: ClusterNoticeBannerProps) {
  const showAccessAlert =
    accessState?.state === "forbidden" ||
    accessState?.state === "notFound" ||
    accessState?.state === "authExpired";
  const showMetricsNotice = metricsState?.state === "notInstalled";

  if (!showAccessAlert && !showMetricsNotice) {
    return null;
  }

  const renderAccessContent = () => {
    if (!accessState) return null;
    if (accessState.state === "authExpired") {
      return (
        <div className="cluster-notice-banner__auth">
          <span>{`Authentication expired (${accessState.provider}): ${accessState.hint}`}</span>
          {accessState.deviceCode && (
            <div className="cluster-notice-banner__device" data-testid="banner-device-code">
              <span>Code: {accessState.deviceCode.userCode}</span>
              <button
                type="button"
                className="btn btn--sm"
                onClick={() => {
                  if (accessState.deviceCode?.userCode) {
                    void navigator.clipboard.writeText(accessState.deviceCode.userCode);
                  }
                }}
                data-testid="banner-copy-code-btn"
              >
                Copy code
              </button>
            </div>
          )}
          {onRecoverAuth && (
            <button
              type="button"
              className="btn btn--sm btn--primary"
              onClick={onRecoverAuth}
              data-testid="banner-recover-btn"
            >
              Sign in
            </button>
          )}
        </div>
      );
    }
    if (accessState.state === "ready") return null;
    return accessState.message;
  };

  return (
    <div className="cluster-notice-banner">
      {showAccessAlert && (
        <div
          role="alert"
          data-testid="access-state-alert"
          className="cluster-notice-banner__alert"
        >
          {renderAccessContent()}
        </div>
      )}
      {showMetricsNotice && (
        <div
          data-testid="metrics-not-installed"
          className="cluster-notice-banner__metrics"
        >
          {metricsState.message.includes("metrics-server not installed")
            ? metricsState.message
            : `metrics-server not installed: ${metricsState.message}`}
        </div>
      )}
    </div>
  );
}
