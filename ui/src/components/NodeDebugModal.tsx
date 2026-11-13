import { Channel } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { commands } from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";
import { type Row } from "../stores/pods";
import {
  getExecClient,
  stripAnsi,
  type ExecClient,
} from "./execTerminal";

interface Props {
  open: boolean;
  onClose: () => void;
  row: Row;
  execClient?: ExecClient;
}

const PRESETS = [
  { label: "BusyBox 1.37 (minimal host utils)", image: "busybox:1.37" },
  { label: "Netshoot (networking & troubleshooting)", image: "nicolaka/netshoot" },
  { label: "Alpine (standard Linux / apk)", image: "alpine" },
] as const;

export function NodeDebugModal({ open, onClose, row, execClient }: Props) {
  const client = execClient ?? getExecClient();
  const [image, setImage] = useState("busybox:1.37");
  const [command, setCommand] = useState("");
  const [namespace, setNamespace] = useState("default");
  const [typedName, setTypedName] = useState("");
  const [preview, setPreview] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [sessionId, setSessionId] = useState<number | null>(null);
  const [terminalOutput, setTerminalOutput] = useState("");
  const [error, setError] = useState<string | null>(null);

  const sessionIdRef = useRef<number | null>(null);
  const terminalRef = useRef<HTMLDivElement | null>(null);

  const cached = useContextsStore((s) => s.settingsByContext[row.cluster]);
  const readOnly = cached?.readOnly ?? false;
  const protectedContext = cached?.protected ?? false;
  const nameMatches = !protectedContext || typedName.trim() === row.name;

  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  useEffect(() => {
    if (sessionId !== null) {
      terminalRef.current?.focus();
    }
  }, [sessionId]);

  useEffect(() => {
    if (terminalRef.current) {
      terminalRef.current.scrollTop = terminalRef.current.scrollHeight;
    }
  }, [terminalOutput]);

  useEffect(() => {
    return () => {
      if (sessionIdRef.current !== null) {
        const idToStop = sessionIdRef.current;
        sessionIdRef.current = null;
        void client.stopExecSession(idToStop).catch(() => undefined);
      }
    };
  }, [client]);

  // Keep kubectl preview in sync with dialog fields
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const parsedCommand = command.trim() ? command.trim().split(/\s+/) : [];
    const req = {
      context: row.cluster,
      node: row.name,
      image: image.trim(),
      command: parsedCommand,
      confirmedName: typedName.trim() || null,
      namespace: namespace.trim() || null,
    };
    commands
      .previewDebugNode(req)
      .then((p) => {
        if (!cancelled) {
          setPreview(p);
          setPreviewError(null);
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setPreview(null);
          setPreviewError(err instanceof Error ? err.message : String(err));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [open, row.cluster, row.name, image, command, typedName, namespace]);

  if (!open) return null;

  const handleCopy = () => {
    if (!preview) return;
    void navigator.clipboard.writeText(preview).then(() => {
      setCopied(true);
      setTimeout(() => {
        setCopied(false);
      }, 2000);
    });
  };

  const handleLaunch = async () => {
    if (readOnly || !nameMatches || connecting || sessionId !== null) return;
    setConnecting(true);
    setError(null);
    setTerminalOutput("");

    try {
      const channel = client.createChannel
        ? client.createChannel((chunk: string) => {
            setTerminalOutput((prev) => prev + chunk);
          })
        : new Channel<string>((chunk: string) => {
            setTerminalOutput((prev) => prev + chunk);
          });

      const parsedCommand = command.trim() ? command.trim().split(/\s+/) : [];
      const req = {
        context: row.cluster,
        node: row.name,
        image: image.trim(),
        command: parsedCommand,
        confirmedName: protectedContext ? typedName.trim() : null,
        namespace: namespace.trim() || null,
      };

      const res = await (client.startNodeDebugSession ?? commands.startNodeDebugSession)(
        req,
        channel,
      );

      setConnecting(false);
      if (res.status === "ok") {
        setSessionId(res.data);
        sessionIdRef.current = res.data;
      } else {
        setError(res.error.message || `Node debug failed: ${res.error.kind}`);
      }
    } catch (err: unknown) {
      setConnecting(false);
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleExternalTerminal = async () => {
    if (launchDisabled) return;
    try {
      await commands.openExternalTerminal({
        target: {
          kind: "nodeDebug",
          cluster: row.cluster,
          namespace: row.namespace ?? null,
          node: row.name,
          image: image.trim() || null,
        },
        confirmedName: protectedContext ? typedName.trim() : null,
      });
      onClose();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleDisconnect = async () => {
    if (sessionIdRef.current !== null) {
      const idToStop = sessionIdRef.current;
      sessionIdRef.current = null;
      setSessionId(null);
      try {
        await client.stopExecSession(idToStop);
      } catch {
        // Ignore stop errors on manual disconnect
      }
    }
    onClose();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLElement>) => {
    if (sessionId === null) return;
    if (e.metaKey || e.altKey) return;

    let dataToSend: string | null = null;
    if (e.key === "Enter") {
      dataToSend = "\n";
    } else if (e.key === "Backspace") {
      dataToSend = "\x7f";
    } else if (e.key === "Tab") {
      dataToSend = "\t";
    } else if (e.key === "Escape") {
      dataToSend = "\x1b";
      e.stopPropagation();
    } else if (e.ctrlKey) {
      const lower = e.key.toLowerCase();
      if (lower === "c") {
        dataToSend = "\x03";
      } else if (lower === "d") {
        dataToSend = "\x04";
      } else if (lower === "l") {
        dataToSend = "\x0c";
      }
    } else if (e.key.length === 1) {
      dataToSend = e.key;
    }

    if (dataToSend !== null) {
      e.preventDefault();
      void client.writeExecStdin(sessionId, dataToSend).catch(() => undefined);
    }
  };

  const handlePaste = (e: React.ClipboardEvent<HTMLElement>) => {
    if (sessionId === null) return;
    const text = e.clipboardData.getData("text");
    if (text) {
      e.preventDefault();
      void client.writeExecStdin(sessionId, text).catch(() => undefined);
    }
  };

  const launchDisabled = connecting || readOnly || !nameMatches || !image.trim();

  return (
    <div
      className="modal-backdrop"
      onClick={() => {
        void handleDisconnect();
      }}
      role="presentation"
    >
      <div
        className="action-confirm-dialog node-debug-dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="node-debug-title"
      >
        <header className="action-confirm-dialog__header">
          <h3 id="node-debug-title" className="action-confirm-dialog__title">
            Debug Node: {row.name}
          </h3>
          <p className="action-confirm-dialog__note">
            Creates a short-lived host-namespace debug pod on node <strong>{row.name}</strong> ({row.cluster}).
            The pod is deleted automatically when the debug shell terminates.
          </p>
        </header>

        <div className="action-confirm-dialog__body">
          {readOnly && (
            <div
              className="action-confirm-dialog__alert"
              role="alert"
              data-testid="node-debug-readonly-notice"
            >
              Node debug disabled: context <code>{row.cluster}</code> is read-only.
            </div>
          )}

          {error && (
            <div
              className="action-confirm-dialog__alert action-confirm-dialog__alert--error"
              role="alert"
              data-testid="node-debug-error-notice"
            >
              {error}
            </div>
          )}

          {sessionId === null && !connecting ? (
            <>
              <div className="action-confirm-dialog__field">
                <label htmlFor="node-debug-image">Debug Image</label>
                <input
                  id="node-debug-image"
                  data-testid="node-debug-image-input"
                  type="text"
                  value={image}
                  onChange={(e) => {
                    setImage(e.target.value);
                  }}
                  placeholder="e.g. busybox:1.37"
                />
                <div className="action-confirm-dialog__presets">
                  {PRESETS.map((p) => (
                    <button
                      key={p.image}
                      type="button"
                      className={`action-confirm-dialog__preset-btn${image === p.image ? " is-active" : ""}`}
                      onClick={() => {
                        setImage(p.image);
                      }}
                    >
                      {p.label}
                    </button>
                  ))}
                </div>
              </div>

              <div className="action-confirm-dialog__field">
                <label htmlFor="node-debug-command">Custom Command (optional)</label>
                <input
                  id="node-debug-command"
                  data-testid="node-debug-cmd-input"
                  type="text"
                  value={command}
                  onChange={(e) => {
                    setCommand(e.target.value);
                  }}
                  placeholder="e.g. chroot /host (defaults to image shell)"
                />
              </div>

              <div className="action-confirm-dialog__field">
                <label htmlFor="node-debug-namespace">Namespace</label>
                <input
                  id="node-debug-namespace"
                  data-testid="node-debug-namespace-input"
                  type="text"
                  value={namespace}
                  onChange={(e) => {
                    setNamespace(e.target.value);
                  }}
                  placeholder="default"
                />
              </div>

              <div className="action-confirm-dialog__preview-box">
                <div className="action-confirm-dialog__preview-header">
                  <span>kubectl preview</span>
                  {preview && (
                    <button
                      type="button"
                      className="action-confirm-dialog__copy-btn"
                      onClick={handleCopy}
                      data-testid="node-debug-copy-btn"
                    >
                      {copied ? "Copied" : "Copy"}
                    </button>
                  )}
                </div>
                {preview ? (
                  <pre className="action-confirm-dialog__code" data-testid="node-debug-preview">
                    {preview}
                  </pre>
                ) : previewError ? (
                  <div className="action-confirm-dialog__preview-error">{previewError}</div>
                ) : (
                  <div className="action-confirm-dialog__preview-loading">Generating preview…</div>
                )}
              </div>

              {protectedContext && !readOnly && (
                <div
                  className="action-confirm-dialog__field action-confirm-dialog__field--protected"
                  data-testid="node-debug-protected-guard"
                >
                  <label htmlFor="node-debug-confirm">
                    Protected context. Type <strong>{row.name}</strong> to confirm node access:
                  </label>
                  <input
                    id="node-debug-confirm"
                    data-testid="node-debug-confirm-input"
                    type="text"
                    value={typedName}
                    onChange={(e) => {
                      setTypedName(e.target.value);
                    }}
                    placeholder={row.name}
                    autoComplete="off"
                  />
                </div>
              )}
            </>
          ) : (
            <div className="node-debug-terminal-container">
              <div className="node-debug-terminal-header">
                <span className="node-debug-terminal-status" data-testid="node-debug-status">
                  <span className="exec-panel__status-dot" aria-hidden="true" />
                  {connecting ? "Creating host debug pod…" : "Connected (host PID/Net/IPC)"}
                </span>
                <button
                  type="button"
                  data-testid="node-debug-disconnect-btn"
                  className="exec-panel__disconnect-btn"
                  onClick={() => {
                    void handleDisconnect();
                  }}
                >
                  Disconnect & Delete Pod
                </button>
              </div>

              <div
                data-testid="node-debug-terminal"
                className="exec-panel__terminal node-debug-terminal"
                tabIndex={0}
                role="region"
                aria-label={`Node debug terminal for ${row.name}`}
                ref={terminalRef}
                onKeyDown={handleKeyDown}
                onPaste={handlePaste}
              >
                <pre data-testid="node-debug-output" className="exec-panel__output">
                  {stripAnsi(terminalOutput)}
                  {sessionId !== null && <span className="exec-panel__cursor" aria-hidden="true" />}
                </pre>
              </div>
            </div>
          )}
        </div>

        {sessionId === null && !connecting && (
          <footer className="action-confirm-dialog__footer">
            <button
              type="button"
              className="action-confirm-dialog__cancel-btn"
              onClick={() => {
                void handleDisconnect();
              }}
            >
              Cancel
            </button>
            <button
              type="button"
              data-testid="node-debug-external-terminal-btn"
              className="exec-panel__ext-term-btn"
              disabled={launchDisabled}
              onClick={() => {
                void handleExternalTerminal();
              }}
            >
              External Terminal
            </button>
            <button
              type="button"
              data-testid="node-debug-launch-btn"
              className="action-confirm-dialog__execute-btn"
              disabled={launchDisabled}
              onClick={() => {
                void handleLaunch();
              }}
            >
              Launch Debug Shell
            </button>
          </footer>
        )}
      </div>
    </div>
  );
}
