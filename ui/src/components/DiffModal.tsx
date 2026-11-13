import { useEffect, useMemo, useRef, useState } from "react";
import {
  commands,
  type CompareOptions_Deserialize,
  type DiffResult,
  type ErrorKind,
} from "../generated/bindings";
import { useBackLayer } from "../lib/backLayers";
import { useContextsStore } from "../stores/contexts";

export interface ResourceDiffTarget {
  context?: string | undefined;
  cluster?: string | undefined;
  namespace?: string | undefined;
  ns?: string | undefined;
  kind?: string | undefined;
  name: string;
  manifest?: string | undefined;
}

export type ResourceSpecifier = ResourceDiffTarget | string;

export type CompareResourcesFn = (
  left: string,
  right: string,
  options?: CompareOptions_Deserialize | null,
) => Promise<DiffResult>;

export interface DiffModalProps {
  open?: boolean | undefined;
  left?: ResourceSpecifier | undefined;
  right?: ResourceSpecifier | undefined;
  leftResource?: ResourceSpecifier | undefined;
  rightResource?: ResourceSpecifier | undefined;
  leftManifest?: string | undefined;
  rightManifest?: string | undefined;
  viewMode?: ("side-by-side" | "unified") | undefined;
  defaultViewMode?: ("side-by-side" | "unified") | undefined;
  options?: CompareOptions_Deserialize | undefined;
  /** Computes the diff. Defaults to the Rust `compare_resources` IPC command. */
  compare?: CompareResourcesFn | undefined;
  onClose?: (() => void) | undefined;

  /** When true, provides the write-safety apply controls in the modal. */
  applyMode?: boolean | undefined;
  /** Kubeconfig context to apply against. Defaults to context in resource specifier. */
  context?: string | undefined;
  /** Resource name to match for protected context confirmation. */
  resourceName?: string | undefined;
  /** Resource kind for display and preview. */
  resourceKind?: string | undefined;
  /** Resource namespace for kubectl preview. */
  resourceNamespace?: string | null | undefined;
  /** Overrides read-only setting. */
  readOnly?: boolean | undefined;
  /** Overrides protected context setting. */
  isProtected?: boolean | undefined;
  /** Optional custom apply action; defaults to commands.applyYaml. */
  onApply?: ((confirmedName: string | null) => Promise<string | undefined>) | undefined;
  /** Callback on successful apply with the applied YAML. */
  onApplySuccess?: ((newYaml: string) => void) | undefined;
}

/** Typed failure shown in the modal; `kind` is null for non-`IpcError` rejections. */
export interface DiffError {
  kind: ErrorKind | null;
  message: string;
}

type DiffState =
  | { status: "loading" }
  | { status: "ready"; result: DiffResult }
  | { status: "error"; error: DiffError };

interface CompareRequest {
  compare: CompareResourcesFn;
  left: string;
  right: string;
  options?: CompareOptions_Deserialize | null | undefined;
}

type LineKind = "add" | "delete" | "context" | "hunk" | "file-old" | "file-new";

interface UnifiedLine {
  kind: LineKind;
  raw: string;
  text: string;
}

interface SideCell {
  lineNumber: number;
  text: string;
  type: "normal" | "delete" | "add";
}

interface SideBySideRow {
  left?: SideCell | undefined;
  right?: SideCell | undefined;
}

class CompareError extends Error {
  constructor(readonly diffError: DiffError) {
    super(diffError.message);
  }
}

const defaultCompare: CompareResourcesFn = async (left, right, options = null) => {
  const hasCustomOptions = Boolean(
    options &&
      (options.includeMetadataAndStatus ||
        options.onlyDifferences ||
        options.revealSecrets),
  );
  const res = hasCustomOptions
    ? await commands.compareResourcesWithOptions(left, right, options)
    : await commands.compareResources(left, right);
  if (res.status === "ok") return res.data;
  throw new CompareError({ kind: res.error.kind, message: res.error.message });
};

/**
 * The generated wrapper types every non-`Error` rejection as `IpcError`, but Tauri
 * itself rejects with plain strings (e.g. when not running inside Tauri).
 */
function toDiffError(raw: unknown): DiffError {
  if (raw instanceof CompareError) return raw.diffError;
  if (typeof raw === "object" && raw !== null && "message" in raw) {
    const { kind, message } = raw as { kind?: unknown; message: unknown };
    return {
      kind: typeof kind === "string" ? (kind as ErrorKind) : null,
      message: String(message),
    };
  }
  return { kind: null, message: String(raw) };
}

function extractErrorMessage(err: unknown): string {
  if (typeof err === "object" && err !== null) {
    if ("error" in err && typeof err.error === "object" && err.error !== null && "message" in err.error) {
      return String(err.error.message);
    }
    if ("message" in err) return String(err.message);
  }
  return String(err);
}

function formatResourceTarget(target?: ResourceSpecifier): string {
  if (!target) return "";
  if (typeof target === "string") return target;
  const ctx = target.context || target.cluster || "default";
  const ns = target.namespace || target.ns || "default";
  const name = target.name || "";
  return `${ctx}/${ns}/${name}`;
}

/** Classifies backend diff lines for display only; no diffing happens here. */
function classifyLines(unifiedDiff: string): UnifiedLine[] {
  if (unifiedDiff === "") return [];
  return unifiedDiff.split("\n").map((raw): UnifiedLine => {
    if (raw.startsWith("@@")) return { kind: "hunk", raw, text: raw };
    if (raw === "---" || raw.startsWith("--- ")) return { kind: "file-old", raw, text: raw };
    if (raw === "+++" || raw.startsWith("+++ ")) return { kind: "file-new", raw, text: raw };
    if (raw.startsWith("+")) return { kind: "add", raw, text: raw.slice(1) };
    if (raw.startsWith("-")) return { kind: "delete", raw, text: raw.slice(1) };
    return { kind: "context", raw, text: raw.startsWith(" ") ? raw.slice(1) : raw };
  });
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/**
 * Lays the backend's unified diff out in two columns: `-` lines go left, `+` lines
 * go right (paired within each change block), context lines appear on both sides.
 */
function toSideBySide(lines: UnifiedLine[]): SideBySideRow[] {
  const rows: SideBySideRow[] = [];
  let leftNum = 1;
  let rightNum = 1;
  let dels: SideCell[] = [];
  let adds: SideCell[] = [];

  const flush = () => {
    const len = Math.max(dels.length, adds.length);
    for (let k = 0; k < len; k++) rows.push({ left: dels[k], right: adds[k] });
    dels = [];
    adds = [];
  };

  for (const line of lines) {
    switch (line.kind) {
      case "delete":
        dels.push({ lineNumber: leftNum++, text: line.text, type: "delete" });
        break;
      case "add":
        adds.push({ lineNumber: rightNum++, text: line.text, type: "add" });
        break;
      case "context":
        flush();
        rows.push({
          left: { lineNumber: leftNum++, text: line.text, type: "normal" },
          right: { lineNumber: rightNum++, text: line.text, type: "normal" },
        });
        break;
      case "hunk": {
        flush();
        const m = HUNK_RE.exec(line.raw);
        if (m) {
          leftNum = Number(m[1]);
          rightNum = Number(m[2]);
        }
        break;
      }
      case "file-old":
      case "file-new":
        flush();
        break;
    }
  }
  flush();
  return rows;
}

function unifiedLineClass(kind: LineKind): string {
  switch (kind) {
    case "add":
      return "diff-line diff-line--added";
    case "delete":
      return "diff-line diff-line--deleted";
    case "hunk":
      return "diff-line hint";
    case "file-old":
      return "diff-line diff-unified__header-left";
    case "file-new":
      return "diff-line diff-unified__header-right";
    case "context":
      return "diff-line diff-line--normal";
  }
}

export function DiffModal({
  open = true,
  left,
  right,
  leftResource,
  rightResource,
  leftManifest,
  rightManifest,
  viewMode,
  defaultViewMode = "side-by-side",
  options: optionsProp,
  compare = defaultCompare,
  onClose,
  applyMode,
  context,
  resourceName,
  resourceKind,
  resourceNamespace,
  readOnly: readOnlyProp,
  isProtected: isProtectedProp,
  onApply,
  onApplySuccess,
}: DiffModalProps) {
  const [internalViewMode, setInternalViewMode] = useState<"side-by-side" | "unified">(
    viewMode ?? defaultViewMode,
  );
  const [copied, setCopied] = useState(false);
  // The settled result is tagged with the request it answers; anything else is loading.
  const [settled, setSettled] = useState<{ request: CompareRequest; state: DiffState } | null>(
    null,
  );

  const activeMode = viewMode ?? internalViewMode;

  const resolvedLeftTarget = leftResource ?? left;
  const resolvedRightTarget = rightResource ?? right;

  const leftTargetStr = formatResourceTarget(resolvedLeftTarget);
  const rightTargetStr = formatResourceTarget(resolvedRightTarget);

  const [fetchedLeftManifest, setFetchedLeftManifest] = useState<string | null>(null);
  const [fetchedRightManifest, setFetchedRightManifest] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    if (typeof resolvedLeftTarget === "object" && !resolvedLeftTarget.manifest && !leftManifest) {
      const ctx = resolvedLeftTarget.context || resolvedLeftTarget.cluster;
      const kind = resolvedLeftTarget.kind || resourceKind;
      const ns = resolvedLeftTarget.namespace || resolvedLeftTarget.ns || null;
      const name = resolvedLeftTarget.name;
      if (ctx && kind && name) {
        void commands.getResourceDetail(ctx, kind, ns, name).then((res) => {
          if (res.status === "ok") setFetchedLeftManifest(res.data.yaml);
        });
      }
    }
  }, [open, resolvedLeftTarget, leftManifest, resourceKind]);

  useEffect(() => {
    if (!open) return;
    if (typeof resolvedRightTarget === "object" && !resolvedRightTarget.manifest && !rightManifest) {
      const ctx = resolvedRightTarget.context || resolvedRightTarget.cluster;
      const kind = resolvedRightTarget.kind || resourceKind;
      const ns = resolvedRightTarget.namespace || resolvedRightTarget.ns || null;
      const name = resolvedRightTarget.name;
      if (ctx && kind && name) {
        void commands.getResourceDetail(ctx, kind, ns, name).then((res) => {
          if (res.status === "ok") setFetchedRightManifest(res.data.yaml);
        });
      }
    }
  }, [open, resolvedRightTarget, rightManifest, resourceKind]);

  const leftText =
    leftManifest ??
    (typeof resolvedLeftTarget === "object" ? resolvedLeftTarget.manifest : undefined) ??
    fetchedLeftManifest ??
    "";
  const rightText =
    rightManifest ??
    (typeof resolvedRightTarget === "object" ? resolvedRightTarget.manifest : undefined) ??
    fetchedRightManifest ??
    "";

  const [includeMetadataAndStatus, setIncludeMetadataAndStatus] = useState(
    optionsProp?.includeMetadataAndStatus ?? false,
  );
  const [onlyDifferences, setOnlyDifferences] = useState(
    optionsProp?.onlyDifferences ?? false,
  );
  const [revealSecrets, setRevealSecrets] = useState(
    optionsProp?.revealSecrets ?? false,
  );
  const [holdingSecret, setHoldingSecret] = useState(false);
  const holdTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const startSecretHold = () => {
    if (revealSecrets) return;
    setHoldingSecret(true);
    holdTimerRef.current = setTimeout(() => {
      setRevealSecrets(true);
      setHoldingSecret(false);
    }, 1000);
  };

  const cancelSecretHold = () => {
    if (holdTimerRef.current) {
      clearTimeout(holdTimerRef.current);
      holdTimerRef.current = null;
    }
    setHoldingSecret(false);
  };

  const isSecret =
    resourceKind?.toLowerCase() === "secret" ||
    (typeof resolvedLeftTarget === "object" && resolvedLeftTarget.kind?.toLowerCase() === "secret") ||
    (typeof resolvedRightTarget === "object" && resolvedRightTarget.kind?.toLowerCase() === "secret") ||
    leftText.includes("kind: Secret") ||
    rightText.includes("kind: Secret");

  const options = useMemo<CompareOptions_Deserialize>(
    () => ({
      includeMetadataAndStatus,
      onlyDifferences,
      revealSecrets,
    }),
    [includeMetadataAndStatus, onlyDifferences, revealSecrets],
  );

  const isApplyMode = Boolean(applyMode || onApply || onApplySuccess);

  const targetContext =
    context ??
    (typeof resolvedRightTarget === "object"
      ? resolvedRightTarget.context || resolvedRightTarget.cluster
      : undefined) ??
    (typeof resolvedLeftTarget === "object"
      ? resolvedLeftTarget.context || resolvedLeftTarget.cluster
      : undefined) ??
    "";

  const targetName =
    resourceName ??
    (typeof resolvedRightTarget === "object" ? resolvedRightTarget.name : undefined) ??
    (typeof resolvedLeftTarget === "object" ? resolvedLeftTarget.name : undefined) ??
    "";

  const targetNamespace =
    resourceNamespace !== undefined
      ? resourceNamespace
      : (typeof resolvedRightTarget === "object"
          ? resolvedRightTarget.namespace || resolvedRightTarget.ns
          : undefined) ?? null;

  const cachedSettings = useContextsStore((s) =>
    targetContext ? s.settingsByContext[targetContext] : undefined,
  );
  const [fetchedSettings, setFetchedSettings] = useState<{
    readOnly: boolean;
    protected: boolean;
  } | null>(null);

  useEffect(() => {
    if (!open || !isApplyMode || !targetContext) return;
    if (readOnlyProp !== undefined && isProtectedProp !== undefined) return;
    let active = true;
    void commands.getContextSettings(targetContext).then(
      (res) => {
        if (!active) return;
        if (res.status === "ok") {
          setFetchedSettings({
            readOnly: res.data.readOnly,
            protected: res.data.protected,
          });
        }
      },
      () => {
        // Fallback to cached
      },
    );
    return () => {
      active = false;
    };
  }, [open, isApplyMode, targetContext, readOnlyProp, isProtectedProp]);

  const settingsLoading =
    isApplyMode &&
    Boolean(targetContext) &&
    readOnlyProp === undefined &&
    isProtectedProp === undefined &&
    cachedSettings === undefined &&
    fetchedSettings === null;

  const isReadOnly =
    readOnlyProp !== undefined
      ? readOnlyProp
      : fetchedSettings?.readOnly ?? cachedSettings?.readOnly ?? false;

  const isProtectedContext =
    isProtectedProp !== undefined
      ? isProtectedProp
      : fetchedSettings?.protected ?? cachedSettings?.protected ?? false;

  const [typedName, setTypedName] = useState("");
  const [applying, setApplying] = useState(false);
  const [applyError, setApplyError] = useState<string | null>(null);
  const [copiedKubectl, setCopiedKubectl] = useState(false);

  const nameMatches = !isProtectedContext || typedName.trim() === targetName.trim();
  const applyDisabled =
    applying ||
    settingsLoading ||
    isReadOnly ||
    !nameMatches ||
    settled?.state.status === "loading" ||
    settled?.state.status === "error";

  const kubectlPreview = useMemo(() => {
    if (!targetContext) return "";
    const nsPart = targetNamespace ? ` -n ${targetNamespace}` : "";
    return `kubectl --context ${targetContext}${nsPart} apply -f -`;
  }, [targetContext, targetNamespace]);

  const handleApply = async () => {
    if (applyDisabled) return;
    setApplying(true);
    setApplyError(null);
    const confirmedName = isProtectedContext ? typedName.trim() : null;
    try {
      if (onApply) {
        const res = await onApply(confirmedName);
        setApplying(false);
        const applied = typeof res === "string" ? res : rightText;
        onApplySuccess?.(applied);
        onClose?.();
      } else {
        const res = await commands.applyYaml(targetContext, rightText, confirmedName);
        setApplying(false);
        if (res.status === "error") {
          setApplyError(res.error.message);
          return;
        }
        onApplySuccess?.(res.data);
        onClose?.();
      }
    } catch (err: unknown) {
      setApplying(false);
      setApplyError(extractErrorMessage(err));
    }
  };

  const handleCopyKubectl = async () => {
    try {
      await navigator.clipboard.writeText(kubectlPreview);
      setCopiedKubectl(true);
      setTimeout(() => {
        setCopiedKubectl(false);
      }, 2000);
    } catch {
      // ignore
    }
  };

  const request = useMemo<CompareRequest>(
    () => ({ compare, left: leftText, right: rightText, options }),
    [compare, leftText, rightText, options],
  );

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const hasCustomOptions = Boolean(
      request.options &&
        (request.options.includeMetadataAndStatus ||
          request.options.onlyDifferences ||
          request.options.revealSecrets),
    );
    const promise = hasCustomOptions
      ? request.compare(request.left, request.right, request.options)
      : request.compare(request.left, request.right);
    promise.then(
      (result) => {
        if (!cancelled) setSettled({ request, state: { status: "ready", result } });
      },
      (err: unknown) => {
        if (!cancelled) setSettled({ request, state: { status: "error", error: toDiffError(err) } });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [open, request]);

  const state: DiffState =
    settled?.request === request ? settled.state : { status: "loading" };

  const unifiedDiff = state.status === "ready" ? state.result.unifiedDiff : "";
  const lines = useMemo(() => classifyLines(unifiedDiff), [unifiedDiff]);
  const sideBySideRows = useMemo(() => toSideBySide(lines), [lines]);

  useBackLayer(open && onClose !== undefined, () => {
    onClose?.();
  });

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && onClose) {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [onClose]);

  if (!open) {
    return null;
  }

  const handleCopyPatch = async () => {
    try {
      await navigator.clipboard.writeText(unifiedDiff);
      setCopied(true);
      setTimeout(() => {
        setCopied(false);
      }, 2000);
    } catch {
      // Ignore clipboard failure in testing environment
    }
  };

  const renderBody = () => {
    if (state.status === "loading") {
      return (
        <p className="empty-state" role="status" data-testid="diff-loading">
          Comparing resources…
        </p>
      );
    }
    if (state.status === "error") {
      return (
        <p className="error-state" role="alert" data-testid="diff-error">
          Failed to compare resources
          {state.error.kind ? ` (${state.error.kind})` : ""}: {state.error.message}
        </p>
      );
    }
    if (state.result.identical) {
      return (
        <p className="empty-state" role="status" data-testid="diff-identical">
          Resources are identical (ignoring runtime metadata)
        </p>
      );
    }
    if (lines.length === 0) {
      return (
        <p className="error-state" role="alert" data-testid="diff-error">
          Backend reported differences but returned an empty diff.
        </p>
      );
    }

    if (activeMode === "side-by-side") {
      return (
        <div className="diff-modal__side-by-side" data-testid="diff-side-by-side">
          <div className="diff-pane diff-pane--left" data-testid="diff-left-pane">
            <div className="diff-pane__header">
              <span>{leftTargetStr || "Left Manifest"}</span>
            </div>
            <div className="diff-pane__content">
              {sideBySideRows.map((row, idx) => (
                <div
                  key={`left-${String(idx)}`}
                  className={`diff-line ${row.left ? (row.left.type === "delete" ? "diff-line--deleted" : "diff-line--normal") : "diff-line--empty"}`}
                >
                  <span className="diff-line__num">{row.left ? row.left.lineNumber : ""}</span>
                  <span className="diff-line__marker">
                    {row.left?.type === "delete" ? "-" : " "}
                  </span>
                  <span className="diff-line__text">{row.left?.text ?? ""}</span>
                </div>
              ))}
            </div>
          </div>

          <div className="diff-pane diff-pane--right" data-testid="diff-right-pane">
            <div className="diff-pane__header">
              <span>{rightTargetStr || "Right Manifest"}</span>
            </div>
            <div className="diff-pane__content">
              {sideBySideRows.map((row, idx) => (
                <div
                  key={`right-${String(idx)}`}
                  className={`diff-line ${row.right ? (row.right.type === "add" ? "diff-line--added" : "diff-line--normal") : "diff-line--empty"}`}
                >
                  <span className="diff-line__num">{row.right ? row.right.lineNumber : ""}</span>
                  <span className="diff-line__marker">{row.right?.type === "add" ? "+" : " "}</span>
                  <span className="diff-line__text">{row.right?.text ?? ""}</span>
                </div>
              ))}
            </div>
          </div>
        </div>
      );
    }

    return (
      <div className="diff-modal__unified" data-testid="diff-unified-viewer">
        <div className="diff-unified__header">
          <span className="diff-unified__header-left">--- {leftTargetStr || "left"}</span>
          <span className="diff-unified__header-right">+++ {rightTargetStr || "right"}</span>
        </div>
        <div className="diff-unified__content">
          {lines.map((line, idx) => {
            const isChange = line.kind === "add" || line.kind === "delete";
            return (
              <div
                key={`unified-${String(idx)}`}
                className={unifiedLineClass(line.kind)}
                data-testid={
                  line.kind === "add"
                    ? "diff-line-added"
                    : line.kind === "delete"
                      ? "diff-line-deleted"
                      : line.kind === "hunk"
                        ? "diff-line-hunk"
                        : line.kind === "file-old" || line.kind === "file-new"
                          ? "diff-line-file-header"
                          : undefined
                }
              >
                <span className="diff-line__marker">
                  {isChange ? line.raw.charAt(0) : line.kind === "context" ? " " : ""}
                </span>
                <span className="diff-line__text">{line.text}</span>
              </div>
            );
          })}
        </div>
      </div>
    );
  };

  return (
    <div className="diff-modal-backdrop" onClick={onClose}>
      <div
        role="dialog"
        aria-label="Resource comparison diff"
        aria-modal="true"
        className="diff-modal"
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div className="diff-modal__header">
          <div className="diff-modal__title-row">
            <h2 className="diff-modal__title">{resourceKind ? `${resourceKind} Diff` : "Resource Diff"}</h2>
            <div className="diff-modal__actions">
              <button
                type="button"
                className="diff-modal__btn"
                disabled={state.status !== "ready" || unifiedDiff === ""}
                onClick={() => {
                  void handleCopyPatch();
                }}
              >
                {copied ? "Copied!" : "Copy Diff"}
              </button>
              {onClose && (
                <button
                  type="button"
                  aria-label="Close diff modal"
                  className="diff-modal__close"
                  onClick={onClose}
                >
                  ×
                </button>
              )}
            </div>
          </div>

          <div className="diff-modal__targets" aria-label="Compared resources">
            <div className="diff-modal__target-item" data-testid="diff-left-resource">
              <span className="diff-modal__target-tag diff-modal__target-tag--left">LEFT</span>
              <span className="diff-modal__target-val">{leftTargetStr || "left"}</span>
            </div>
            <span className="diff-modal__targets-vs">vs</span>
            <div className="diff-modal__target-item" data-testid="diff-right-resource">
              <span className="diff-modal__target-tag diff-modal__target-tag--right">RIGHT</span>
              <span className="diff-modal__target-val">{rightTargetStr || "right"}</span>
            </div>
          </div>

          <div className="diff-modal__options-row">
            <div className="diff-modal__view-modes" role="tablist" aria-label="Diff view mode">
              <button
                type="button"
                role="tab"
                aria-selected={activeMode === "side-by-side"}
                className={`diff-modal__tab ${activeMode === "side-by-side" ? "diff-modal__tab--active" : ""}`}
                onClick={() => {
                  setInternalViewMode("side-by-side");
                }}
              >
                Side-by-Side
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={activeMode === "unified"}
                className={`diff-modal__tab ${activeMode === "unified" ? "diff-modal__tab--active" : ""}`}
                onClick={() => {
                  setInternalViewMode("unified");
                }}
              >
                Unified Diff
              </button>
            </div>

            <div className="diff-modal__toggles">
              <label className="diff-modal__toggle-label">
                <input
                  type="checkbox"
                  data-testid="toggle-metadata-status"
                  checked={includeMetadataAndStatus}
                  onChange={(e) => {
                    setIncludeMetadataAndStatus(e.target.checked);
                  }}
                />
                Metadata & status
              </label>
              <label className="diff-modal__toggle-label">
                <input
                  type="checkbox"
                  data-testid="toggle-only-differences"
                  checked={onlyDifferences}
                  onChange={(e) => {
                    setOnlyDifferences(e.target.checked);
                  }}
                />
                Only differences
              </label>

              {isSecret && (
                <button
                  type="button"
                  className={`diff-modal__secret-btn ${revealSecrets ? "diff-modal__secret-btn--revealed" : ""}`}
                  data-testid="reveal-secrets-btn"
                  onMouseDown={startSecretHold}
                  onMouseUp={cancelSecretHold}
                  onMouseLeave={cancelSecretHold}
                  onTouchStart={startSecretHold}
                  onTouchEnd={cancelSecretHold}
                  title="Hold 1 second to decode and diff secret values in memory"
                >
                  {revealSecrets
                    ? "Secret values revealed"
                    : holdingSecret
                      ? "Holding…"
                      : "Hold 1s to decode & diff secret values"}
                </button>
              )}
            </div>
          </div>
        </div>

        <div className="diff-modal__body">{renderBody()}</div>

        {isApplyMode && (
          <div className="diff-modal__footer" data-testid="diff-modal-footer">
            {isReadOnly && (
              <div
                role="alert"
                className="diff-modal__alert diff-modal__alert--warning"
                data-testid="diff-readonly-alert"
              >
                Blocked: context &lsquo;{targetContext}&rsquo; is read-only. Changes cannot be applied.
              </div>
            )}

            {!isReadOnly && isProtectedContext && (
              <div className="diff-modal__guard" data-testid="diff-protected-guard">
                <label className="diff-modal__guard-label" htmlFor="diff-confirm-name-input">
                  This context is protected. Type <strong>{targetName}</strong> to enable Apply:
                </label>
                <input
                  id="diff-confirm-name-input"
                  type="text"
                  aria-label="Confirm resource name"
                  placeholder={targetName}
                  value={typedName}
                  onChange={(e) => {
                    setTypedName(e.target.value);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !applyDisabled) {
                      e.preventDefault();
                      void handleApply();
                    }
                  }}
                  className="diff-modal__input"
                  data-testid="diff-confirm-name-input"
                  disabled={applying}
                />
              </div>
            )}

            {applyError && (
              <div
                role="alert"
                className="diff-modal__alert diff-modal__alert--error"
                data-testid="diff-apply-error"
              >
                {applyError}
              </div>
            )}

            {kubectlPreview && (
              <div className="diff-modal__preview-box">
                <code className="diff-modal__kubectl" data-testid="diff-kubectl-preview">
                  {kubectlPreview}
                </code>
                <button
                  type="button"
                  className="diff-modal__btn diff-modal__btn--sm"
                  onClick={() => {
                    void handleCopyKubectl();
                  }}
                  data-testid="diff-copy-kubectl-btn"
                >
                  {copiedKubectl ? "Copied!" : "Copy Command"}
                </button>
              </div>
            )}

            <div className="diff-modal__footer-actions">
              {onClose && (
                <button
                  type="button"
                  className="diff-modal__btn"
                  onClick={onClose}
                  disabled={applying}
                >
                  Cancel
                </button>
              )}
              <button
                type="button"
                className="diff-modal__btn diff-modal__btn--apply"
                disabled={applyDisabled}
                onClick={() => {
                  void handleApply();
                }}
                data-testid="diff-apply-btn"
              >
                {applying ? "Applying…" : "Apply"}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
