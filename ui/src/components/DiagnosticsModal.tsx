import { useEffect, useRef, useState } from "react";
import { useBackLayer } from "../lib/backLayers";
import { useResourceSchemasStore } from "../stores/resourceSchemas";
import { commands, type DiagnosticReport, type DiagnosticStatus } from "../generated/bindings";

export interface DiagnosticsModalProps {
  open: boolean;
  report: DiagnosticReport | null;
  onClose: () => void;
  /** Context being diagnosed; used for the title while no report is available. */
  context?: string | null;
  /** Diagnostics are running. */
  loading?: boolean;
  /** Backend error message when diagnostics could not run. */
  error?: string | null;
  /** Optional callback after reconnecting a cluster. */
  onReconnect?: ((context: string) => Promise<void> | void) | undefined;
}

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Keeps Tab / Shift+Tab cycling among the dialog's focusable elements. */
function trapTab(dialog: HTMLElement, e: globalThis.KeyboardEvent) {
  const focusables = Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE));
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  if (!first || !last) {
    e.preventDefault();
    dialog.focus();
    return;
  }
  const current = document.activeElement;
  const inside = current instanceof Node && dialog.contains(current);
  if (!inside || (e.shiftKey && current === first) || (!e.shiftKey && current === last)) {
    e.preventDefault();
    (e.shiftKey ? last : first).focus();
  }
}

function statusBadgeLabel(status: DiagnosticStatus): "PASS" | "WARN" | "FAIL" {
  switch (status) {
    case "pass":
      return "PASS";
    case "warn":
      return "WARN";
    case "fail":
      return "FAIL";
  }
}

export function DiagnosticsModal({
  open,
  report,
  onClose,
  context = null,
  loading = false,
  error = null,
  onReconnect,
}: DiagnosticsModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const [copied, setCopied] = useState(false);
  const [reconnecting, setReconnecting] = useState(false);
  useBackLayer(open, onClose);

  const authError = report?.authError;

  const handleCopyCmd = async () => {
    if (!authError?.loginCommand) return;
    try {
      await navigator.clipboard.writeText(authError.loginCommand);
      setCopied(true);
      setTimeout(() => {
        setCopied(false);
      }, 2000);
    } catch (err: unknown) {
      console.warn("Clipboard write failed", err);
    }
  };

  const handleReconnect = async () => {
    const ctx = report?.context ?? context;
    if (!ctx) return;
    setReconnecting(true);
    try {
      await commands.reconnectContext(ctx);
      useResourceSchemasStore.getState().invalidateContext(ctx);
      if (onReconnect) {
        await onReconnect(ctx);
      }
    } catch (err: unknown) {
      console.warn("reconnectContext failed", err);
    } finally {
      setReconnecting(false);
    }
  };

  // Window capture phase runs before every other keydown listener in the app, so Escape and
  // Tab are handled here and never reach global shortcuts. Other keys typed inside the dialog
  // are stopped at the dialog (onKeyDown below).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      const dialog = dialogRef.current;
      if (!dialog) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key === "Tab") {
        e.stopPropagation();
        trapTab(dialog, e);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open, onClose]);

  if (!open) {
    return null;
  }

  const title = report?.context ?? context;

  let body;
  if (loading) {
    body = (
      <p className="empty-state" role="status">
        Running diagnostics…
      </p>
    );
  } else if (error !== null) {
    body = (
      <p className="error-state" role="alert">
        Cannot run diagnostics: {error}
      </p>
    );
  } else if (!report) {
    body = <p className="empty-state">No diagnostic report available.</p>;
  } else {
    body = (
      <>
        {authError && (
          <section
            aria-label="Authentication recovery"
            className="diagnostics-modal__section diagnostics-modal__auth-card"
            data-testid="diag-auth-recovery"
          >
            <div className="diagnostics-modal__auth-header">
              <span className="dot err" aria-hidden="true" />
              <h3 className="diagnostics-modal__auth-title">
                Authentication Expired ({authError.provider})
              </h3>
            </div>
            <p className="diagnostics-modal__auth-desc">
              The credentials or session for this cluster have expired. Run the command below in your
              terminal to re-authenticate:
            </p>
            <div className="diagnostics-modal__auth-cmd-row">
              <pre className="diagnostics-modal__auth-cmd" data-testid="diag-auth-cmd">
                {authError.loginCommand}
              </pre>
              <button
                type="button"
                className="btn diagnostics-modal__auth-copy-btn"
                onClick={() => {
                  void handleCopyCmd();
                }}
                title="Copy login command"
              >
                {copied ? "Copied!" : "Copy"}
              </button>
            </div>
            {authError.deviceCode && (
              <div className="diagnostics-modal__auth-device" data-testid="diag-auth-device">
                <span>Verification URL: </span>
                <span className="diagnostics-modal__auth-url">{authError.deviceCode.url}</span>
                <span style={{ marginLeft: "12px" }}>User code: </span>
                <code className="diagnostics-modal__auth-code">{authError.deviceCode.userCode}</code>
              </div>
            )}
            <div className="diagnostics-modal__auth-actions">
              <button
                type="button"
                className="btn btn--primary diagnostics-modal__auth-reconnect-btn"
                onClick={() => {
                  void handleReconnect();
                }}
                disabled={reconnecting}
                data-testid="diag-auth-reconnect-btn"
              >
                {reconnecting ? "Reconnecting…" : "Reconnect cluster"}
              </button>
            </div>
          </section>
        )}

        <section aria-label="Diagnostic steps" className="diagnostics-modal__section">
          <h3 className="diagnostics-modal__subtitle">Checks</h3>
          <ul className="diagnostics-modal__steps">
            {report.steps.map((step) => (
              <li
                key={step.name}
                data-testid={`diag-step-${step.name}`}
                className="diagnostics-modal__step"
              >
                <span
                  className={`diagnostics-modal__badge diagnostics-modal__badge--${step.status}`}
                >
                  {statusBadgeLabel(step.status)}
                </span>
                <span className="diagnostics-modal__step-name">{step.name}</span>
                <span className="diagnostics-modal__step-detail">{step.detail}</span>
              </li>
            ))}
          </ul>
        </section>

        <section aria-label="RBAC SelfSubjectAccessReview" className="diagnostics-modal__section">
          <h3 className="diagnostics-modal__subtitle">RBAC SelfSubjectAccessReview</h3>
          <table data-testid="diag-rbac-table" className="diagnostics-modal__rbac-table">
            <thead>
              <tr>
                <th>Resource</th>
                <th>Verb</th>
                <th>Access</th>
              </tr>
            </thead>
            <tbody>
              {report.rbac.map((item) => (
                <tr key={`${item.resource}:${item.verb}`}>
                  <td>{item.resource}</td>
                  <td>{item.verb}</td>
                  <td
                    className={
                      item.allowed
                        ? "diagnostics-modal__rbac--allowed"
                        : "diagnostics-modal__rbac--denied"
                    }
                  >
                    {item.allowed ? "Allowed" : "Denied"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      </>
    );
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        role="dialog"
        aria-label="Connection diagnostics"
        aria-modal="true"
        aria-busy={loading || undefined}
        className="diagnostics-modal"
        ref={dialogRef}
        tabIndex={-1}
        onClick={(e) => {
          e.stopPropagation();
        }}
        // Native stopPropagation at the portal root keeps keys away from window/document
        // shortcut listeners and from React ancestors of the portal (the context list).
        onKeyDown={(e) => {
          e.stopPropagation();
        }}
      >
        <div className="diagnostics-modal__header">
          <h2 className="diagnostics-modal__title">
            Connection diagnostics{title ? `: ${title}` : ""}
          </h2>
          <button
            type="button"
            aria-label="Close diagnostics"
            className="diagnostics-modal__close"
            // Keyboard users land in the dialog; Escape closes it.
            autoFocus
            onClick={onClose}
          >
            ×
          </button>
        </div>

        <div className="diagnostics-modal__body">{body}</div>
      </div>
    </div>
  );
}
