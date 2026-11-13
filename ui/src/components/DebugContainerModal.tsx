import { useEffect, useState } from "react";
import { commands } from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";
import { type Row } from "../stores/pods";

interface Props {
  open: boolean;
  onClose: () => void;
  row: Row;
}

const PRESETS = [
  { label: "Netshoot (network tools)", image: "nicolaka/netshoot", defaultCmd: "/bin/bash" },
  { label: "BusyBox 1.37 (minimal utils)", image: "busybox:1.37", defaultCmd: "sh" },
  { label: "Alpine (standard Linux / apk)", image: "alpine", defaultCmd: "/bin/sh" },
] as const;

export function DebugContainerModal({ open, onClose, row }: Props) {
  const [image, setImage] = useState("nicolaka/netshoot");
  const [targetContainer, setTargetContainer] = useState("");
  const [command, setCommand] = useState("/bin/bash");
  const [containerName, setContainerName] = useState("debugger");
  const [typedName, setTypedName] = useState("");
  const [preview, setPreview] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const cached = useContextsStore((s) => s.settingsByContext[row.cluster]);
  const readOnly = cached?.readOnly ?? false;
  const protectedContext = cached?.protected ?? false;
  const nameMatches = !protectedContext || typedName.trim() === row.name;

  // Keep kubectl preview in sync with dialog fields
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const parsedCommand = command.trim() ? command.trim().split(/\s+/) : [];
    const req = {
      context: row.cluster,
      namespace: row.namespace ?? "default",
      pod: row.name,
      containerName: containerName.trim() || "debugger",
      image: image.trim(),
      targetContainer: targetContainer.trim() || null,
      command: parsedCommand,
      confirmedName: typedName.trim() || null,
    };
    commands
      .previewDebugPod(req)
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
  }, [open, row.cluster, row.namespace, row.name, containerName, image, targetContainer, command, typedName]);

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
    if (readOnly || !nameMatches || running) return;
    setRunning(true);
    setError(null);
    try {
      const parsedCommand = command.trim() ? command.trim().split(/\s+/) : [];
      const res = await commands.debugPod({
        context: row.cluster,
        namespace: row.namespace ?? "default",
        pod: row.name,
        containerName: containerName.trim() || "debugger",
        image: image.trim(),
        targetContainer: targetContainer.trim() || null,
        command: parsedCommand,
        confirmedName: protectedContext ? typedName.trim() : null,
      });
      if (res.status === "error") {
        setError(res.error.message);
      } else {
        onClose();
      }
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  };

  const launchDisabled =
    running || readOnly || !nameMatches || !image.trim() || !containerName.trim();

  return (
    <div
      className="modal-backdrop"
      onClick={() => {
        onClose();
      }}
      role="presentation"
    >
      <div
        className="action-confirm-dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        role="dialog"
        aria-modal="true"
        aria-labelledby="debug-container-title"
      >
        <header className="action-confirm-dialog__header">
          <h3 id="debug-container-title" className="action-confirm-dialog__title">
            Debug Pod: {row.name}
          </h3>
          <p className="action-confirm-dialog__note">
            Adds an ephemeral debug container to <strong>{row.name}</strong> ({row.namespace ?? "default"}) on context <code>{row.cluster}</code>.
          </p>
        </header>

        <div className="action-confirm-dialog__body">
          {readOnly && (
            <div className="action-confirm-dialog__alert" role="alert">
              Context <code>{row.cluster}</code> is read-only. Adding debug containers is blocked.
            </div>
          )}

          {error && (
            <div className="action-confirm-dialog__alert" role="alert">
              {error}
            </div>
          )}

          <div className="action-confirm-dialog__guard">
            <span className="action-confirm-dialog__label">Image Presets</span>
            <div style={{ display: "flex", gap: "6px", flexWrap: "wrap", marginTop: "4px" }}>
              {PRESETS.map((p) => (
                <button
                  key={p.image}
                  type="button"
                  className="action-confirm-dialog__copy-btn"
                  style={{ opacity: image === p.image ? 1 : 0.7 }}
                  onClick={() => {
                    setImage(p.image);
                    setCommand(p.defaultCmd);
                  }}
                >
                  {p.label}
                </button>
              ))}
            </div>
          </div>

          <label className="action-confirm-dialog__guard">
            <span className="action-confirm-dialog__label">Image</span>
            <input
              className="action-confirm-dialog__input"
              value={image}
              onChange={(e) => {
                setImage(e.target.value);
              }}
              placeholder="e.g. nicolaka/netshoot, busybox:1.37, alpine"
              disabled={readOnly || running}
            />
          </label>

          <label className="action-confirm-dialog__guard">
            <span className="action-confirm-dialog__label">Debug Container Name</span>
            <input
              className="action-confirm-dialog__input"
              value={containerName}
              onChange={(e) => {
                setContainerName(e.target.value);
              }}
              placeholder="debugger"
              disabled={readOnly || running}
            />
          </label>

          <label className="action-confirm-dialog__guard">
            <span className="action-confirm-dialog__label">Target Container (optional process-namespace sharing)</span>
            <input
              className="action-confirm-dialog__input"
              value={targetContainer}
              onChange={(e) => {
                setTargetContainer(e.target.value);
              }}
              placeholder="Leave empty or specify target container name"
              disabled={readOnly || running}
            />
          </label>

          <label className="action-confirm-dialog__guard">
            <span className="action-confirm-dialog__label">Command</span>
            <input
              className="action-confirm-dialog__input"
              value={command}
              onChange={(e) => {
                setCommand(e.target.value);
              }}
              placeholder="/bin/bash"
              disabled={readOnly || running}
            />
          </label>

          <div className="action-confirm-dialog__preview-box">
            {preview !== null ? (
              <code data-testid="kubectl-preview" className="action-confirm-dialog__kubectl">
                {preview}
              </code>
            ) : previewError !== null ? (
              <span role="alert" data-testid="kubectl-preview-error">
                kubectl preview unavailable: {previewError}
              </span>
            ) : (
              <span role="status" data-testid="kubectl-preview-loading">
                Loading kubectl preview…
              </span>
            )}
            <button
              type="button"
              className="action-confirm-dialog__copy-btn"
              disabled={preview === null}
              onClick={handleCopy}
            >
              {copied ? "Copied" : "Copy as kubectl"}
            </button>
          </div>

          {protectedContext && (
            <label className="action-confirm-dialog__guard">
              <span className="action-confirm-dialog__label">
                Context is protected. Type <code>{row.name}</code> to confirm:
              </span>
              <input
                className="action-confirm-dialog__input"
                value={typedName}
                onChange={(e) => {
                  setTypedName(e.target.value);
                }}
                placeholder={row.name}
                disabled={readOnly || running}
              />
            </label>
          )}
        </div>

        <footer className="action-confirm-dialog__footer">
          <button
            type="button"
            className="action-confirm-dialog__btn"
            onClick={() => {
              onClose();
            }}
            disabled={running}
          >
            Cancel
          </button>
          <button
            type="button"
            className="action-confirm-dialog__btn action-confirm-dialog__btn--primary"
            onClick={() => {
              void handleLaunch();
            }}
            disabled={launchDisabled}
          >
            {running ? "Launching…" : "Launch Debug Container"}
          </button>
        </footer>
      </div>
    </div>
  );
}
