import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type SyntheticEvent,
} from "react";
import { useBackLayer } from "../lib/backLayers";
import { useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import {
  actionTargetName,
  describeTarget,
  MAX_REPLICAS,
  replicasLabel,
  useActionsStore,
  type PendingAction,
} from "../stores/actions";

export type ActionDialogOutcome = "cancelled" | "done";

export interface ActionConfirmDialogProps {
  /** Called after the dialog closed, with the action it was open for (focus restore). */
  onClosed?: (pending: PendingAction, outcome: ActionDialogOutcome) => void;
}

const TITLES: Record<PendingAction["action"]["type"], string> = {
  delete: "Delete",
  scale: "Scale",
  restart: "Restart",
  cordon: "Cordon",
  uncordon: "Uncordon",
  drain: "Drain",
  fluxReconcile: "Force Reconcile",
  fluxSuspend: "Suspend / Resume",
  argoRolloutPromote: "Promote Rollout",
  argoRolloutAbort: "Abort Rollout",
  argoRolloutRetry: "Retry Rollout",
  argoAppSync: "Sync Application",
  certRenew: "Renew Certificate",
  externalSecretForceSync: "Force Sync Secret",
};

/** How long "Copied" stays on the copy button. */
export const COPIED_RESET_MS = 2000;

const FOCUSABLE =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href], [tabindex]:not([tabindex="-1"])';

/** Validation message for the replicas field, or `null` when `raw` is a valid count. */
function replicasProblem(raw: string): string | null {
  const text = raw.trim();
  if (text === "") return "Enter the number of replicas.";
  if (!/^\d+$/.test(text)) return "Replicas must be a whole number of 0 or more.";
  if (Number(text) > MAX_REPLICAS) {
    return `Replicas must be at most ${String(MAX_REPLICAS)}.`;
  }
  return null;
}

function backendErrorText(kind: string | null, message: string): string {
  switch (kind) {
    case "readOnlyContext":
      return `Blocked: the context is read-only. ${message}`;
    case "confirmationRequired":
      return `Blocked: this context is protected and needs the exact name. ${message}`;
    default:
      return `Action failed: ${message}`;
  }
}

/**
 * Confirmation dialog for the pending mutating action of the actions store (R-07).
 *
 * Shows the backend's `kubectl` preview, disables confirmation on read-only contexts and
 * asks for the exact resource name on protected ones. The backend enforces both guards
 * again; its errors are shown here as they come back.
 */
export function ActionConfirmDialog({ onClosed }: ActionConfirmDialogProps) {
  const pending = useActionsStore((s) => s.pending);
  const requestId = useActionsStore((s) => s.requestId);
  if (!pending) return null;
  // Keyed by request so the typed name never carries over to another action.
  return <ActionConfirmDialogBody key={requestId} pending={pending} onClosed={onClosed} />;
}

function ActionConfirmDialogBody({
  pending,
  onClosed,
}: {
  pending: PendingAction;
  onClosed: ActionConfirmDialogProps["onClosed"];
}) {
  const preview = useActionsStore((s) => s.preview);
  const previewError = useActionsStore((s) => s.previewError);
  const settings = useActionsStore((s) => s.settings);
  const settingsError = useActionsStore((s) => s.settingsError);
  const running = useActionsStore((s) => s.running);
  const error = useActionsStore((s) => s.error);
  const errorKind = useActionsStore((s) => s.errorKind);
  const cached = useContextsStore((s) => s.settingsByContext[pending.context]);
  const [typedName, setTypedName] = useState("");
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<string | null>(null);
  // Local text of the replicas field, so it can be cleared or hold an invalid value while
  // typing; only valid counts reach the store (and the preview).
  const [replicasText, setReplicasText] = useState(() =>
    pending.action.type === "scale" && pending.replicasUnset !== true
      ? String(pending.action.replicas)
      : "",
  );
  const dialogRef = useRef<HTMLDivElement>(null);
  // The backdrop closes the dialog only for a press that starts and ends on it, so a text
  // selection dragged out of an input does not cancel the action.
  const backdropPressRef = useRef(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => {
      setCopied(false);
    }, COPIED_RESET_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [copied]);

  const { action, context } = pending;
  const targetName = actionTargetName(action);
  // The backend's settings win; the sidebar's copy only covers a failed settings load, and
  // an unknown protected flag asks for the name rather than hiding the gate.
  const readOnly = settings?.readOnly ?? cached?.readOnly ?? false;
  const protectedContext = settings?.protected ?? (settingsError !== null ? true : false);
  const settingsLoading = settings === null && settingsError === null;
  const nameMatches = typedName === targetName;
  const replicasError = action.type === "scale" ? replicasProblem(replicasText) : null;
  const confirmDisabled =
    running ||
    settingsLoading ||
    readOnly ||
    preview === null ||
    replicasError !== null ||
    pending.replicasUnset === true ||
    (protectedContext && !nameMatches);

  /** Returns false while the action runs (it cannot be called back). */
  const close = (): boolean => {
    if (useActionsStore.getState().running) return false;
    useActionsStore.getState().cancel();
    onClosed?.(pending, "cancelled");
    return true;
  };
  // The phone's back gesture cancels the dialog before anything below it (03-03a).
  useBackLayer(true, close);

  // Escape closes the dialog wherever focus is (the palette handles its own Escape).
  const closeRef = useRef(close);
  useEffect(() => {
    closeRef.current = close;
  });
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (useLayoutStore.getState().paletteOpen) return;
      e.preventDefault();
      closeRef.current();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, []);

  const onSubmit = (e: SyntheticEvent) => {
    e.preventDefault();
    if (confirmDisabled) return;
    void useActionsStore
      .getState()
      .confirm(protectedContext ? typedName : null)
      .then((ok) => {
        if (ok) onClosed?.(pending, "done");
      });
  };

  const handleCopy = () => {
    if (preview === null) return;
    setCopyError(null);
    const fail = (err: unknown) => {
      setCopied(false);
      setCopyError(err instanceof Error && err.message !== "" ? err.message : String(err));
    };
    try {
      void navigator.clipboard.writeText(preview).then(() => {
        setCopied(true);
      }, fail);
    } catch (err: unknown) {
      // No clipboard API (insecure context, old webview).
      fail(err);
    }
  };

  const onReplicasChange = (raw: string) => {
    setReplicasText(raw);
    if (replicasProblem(raw) === null) {
      useActionsStore.getState().setReplicas(Number(raw.trim()));
    } else {
      useActionsStore.getState().clearReplicas();
    }
  };

  // Keeps Tab / Shift-Tab inside the dialog (it is aria-modal).
  const onDialogKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Tab" || e.ctrlKey || e.metaKey || e.altKey) return;
    const root = dialogRef.current;
    if (!root) return;
    const items = Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE));
    if (items.length === 0) {
      e.preventDefault();
      root.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    const inside = active instanceof Node && root.contains(active);
    if (e.shiftKey && (!inside || active === first || active === root)) {
      e.preventDefault();
      last?.focus();
    } else if (!e.shiftKey && (!inside || active === last)) {
      e.preventDefault();
      first?.focus();
    }
  };

  const title = `${TITLES[action.type]} ${describeTarget(action)}`;

  return (
    <div
      className="modal-backdrop"
      data-testid="action-confirm-backdrop"
      onMouseDown={(e) => {
        backdropPressRef.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        const pressedHere = backdropPressRef.current;
        backdropPressRef.current = false;
        if (pressedHere && e.target === e.currentTarget) close();
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="action-confirm-title"
        className="action-confirm-dialog"
        data-testid="action-confirm-dialog"
        tabIndex={-1}
        onKeyDown={onDialogKeyDown}
      >
        <form onSubmit={onSubmit}>
          <div className="action-confirm-dialog__header">
            <h2 id="action-confirm-title" className="action-confirm-dialog__title">
              {title}
            </h2>
            <p className="action-confirm-dialog__context">
              Context <strong>{context}</strong>
            </p>
          </div>

          <div className="action-confirm-dialog__body">
            {readOnly && (
              <div role="alert" data-testid="readonly-blocked" className="action-confirm-dialog__alert">
                Context {context} is read-only: this action is disabled. Turn off read-only in
                the context settings to run it.
              </div>
            )}
            {settingsLoading && (
              <p role="status" className="action-confirm-dialog__note">
                Checking the context&apos;s write-safety settings…
              </p>
            )}
            {settingsError !== null && (
              <p role="alert" className="action-confirm-dialog__alert">
                Could not load the context settings ({settingsError}). The name is required; the
                backend still enforces read-only and protected contexts.
              </p>
            )}

            {action.type === "scale" && (
              <label className="action-confirm-dialog__label">
                <span>Replicas</span>
                <input
                  type="text"
                  inputMode="numeric"
                  autoComplete="off"
                  aria-label="Replicas"
                  aria-invalid={replicasError !== null}
                  aria-describedby={replicasError !== null ? "action-confirm-replicas-error" : undefined}
                  className="action-confirm-dialog__input"
                  value={replicasText}
                  disabled={running}
                  onChange={(e) => {
                    onReplicasChange(e.target.value);
                  }}
                />
                {replicasError === null && pending.replicasUnset !== true && (
                  <span className="action-confirm-dialog__note">
                    Scale to {replicasLabel(action.replicas)}
                  </span>
                )}
              </label>
            )}
            {replicasError !== null && (
              <p
                id="action-confirm-replicas-error"
                role="alert"
                data-testid="replicas-error"
                className="action-confirm-dialog__alert"
              >
                {replicasError}
              </p>
            )}

            <div className="action-confirm-dialog__preview-box">
              {action.type === "scale" && pending.replicasUnset === true ? (
                <span role="status" data-testid="kubectl-preview-waiting">
                  Enter a replica count to see the kubectl command.
                </span>
              ) : preview !== null ? (
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
            {copyError !== null && (
              <p role="alert" data-testid="copy-error" className="action-confirm-dialog__alert">
                Could not copy to the clipboard: {copyError}
              </p>
            )}

            {protectedContext && !readOnly && (
              <div className="action-confirm-dialog__guard">
                <label className="action-confirm-dialog__label">
                  <span>
                    Protected context <strong>{context}</strong>: type <code>{targetName}</code>{" "}
                    to confirm
                  </span>
                  <input
                    type="text"
                    autoFocus
                    autoComplete="off"
                    spellCheck={false}
                    aria-label="Type resource name to confirm"
                    className="action-confirm-dialog__input"
                    value={typedName}
                    disabled={running}
                    onChange={(e) => {
                      setTypedName(e.target.value);
                    }}
                  />
                </label>
              </div>
            )}

            {error !== null && (
              <div role="alert" data-testid="action-error" className="action-confirm-dialog__alert">
                {backendErrorText(errorKind, error)}
              </div>
            )}
          </div>

          <div className="action-confirm-dialog__footer">
            <button
              type="button"
              // Unprotected contexts focus Cancel, so a stray Enter never runs the action.
              autoFocus={!protectedContext || readOnly}
              className="action-confirm-dialog__btn"
              disabled={running}
              onClick={close}
            >
              Cancel
            </button>
            <button
              type="submit"
              data-testid="confirm-action-btn"
              className="action-confirm-dialog__btn action-confirm-dialog__btn--danger"
              disabled={confirmDisabled}
            >
              {running ? "Running…" : TITLES[action.type]}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
