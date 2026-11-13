import { create } from "zustand";
import {
  commands,
  type ContextSettings_Serialize,
  type ErrorKind,
  type MutatingAction_Deserialize,
} from "../generated/bindings";
import { useContextsStore } from "./contexts";
import type { Row } from "./pods";

export type MutatingAction = MutatingAction_Deserialize;

/** A mutating action waiting for confirmation in `ActionConfirmDialog` (R-07). */
export interface PendingAction {
  /** Kubeconfig context the action runs against (a row's `cluster`). */
  context: string;
  action: MutatingAction;
  /** Row the action was started from, so focus can return to it when the dialog closes. */
  originUid: string | null;
  /**
   * Scale only: no valid replica count has been entered yet (unknown desired count, or the
   * field is empty / invalid). No preview is loaded and the action cannot be confirmed.
   */
  replicasUnset?: boolean;
}

/** Largest replica count the API accepts (`int32`). */
export const MAX_REPLICAS = 2147483647;

export interface ActionsState {
  pending: PendingAction | null;
  /** Increases with every `request`; the dialog is keyed by it. */
  requestId: number;
  /** `kubectl` equivalent from `preview_kubectl_action`; `null` while loading or on error. */
  preview: string | null;
  previewError: string | null;
  /** Write-safety settings of `pending.context` from `get_context_settings`. */
  settings: ContextSettings_Serialize | null;
  settingsError: string | null;
  running: boolean;
  /** Last `execute_action` failure; the dialog stays open and shows it. */
  error: string | null;
  /** `null` when the failure did not come from the backend as an `IpcError`. */
  errorKind: ErrorKind | null;
  /** Short description of the last accepted action, e.g. "Delete requested for pod default/web-0 on kind". */
  lastSuccess: string | null;
  /** Resource selected in the active table; the command palette offers actions for it. */
  selection: Row | null;
  /**
   * Opens the confirm dialog for `pending` and loads its preview and context settings.
   * Refused (returns `false`) while another action is pending or running; cancel it first.
   */
  request: (pending: PendingAction) => boolean;
  /** Changes the replica count of a pending scale action and refreshes the preview. */
  setReplicas: (replicas: number) => void;
  /** Marks the replica count of a pending scale action as missing (field empty or invalid). */
  clearReplicas: () => void;
  /** Runs the pending action. Resolves `true` when the backend reported success. */
  confirm: (confirmedName: string | null) => Promise<boolean>;
  /** Closes the dialog. Ignored while the action is running (it cannot be called back). */
  cancel: () => void;
  clearSuccess: () => void;
  setSelection: (row: Row | null) => void;
}

export const initialActionsState = {
  pending: null as PendingAction | null,
  requestId: 0,
  preview: null as string | null,
  previewError: null as string | null,
  settings: null as ContextSettings_Serialize | null,
  settingsError: null as string | null,
  running: false,
  error: null as string | null,
  errorKind: null as ErrorKind | null,
  lastSuccess: null as string | null,
  selection: null as Row | null,
};

// Bumped whenever the pending action changes (request, cancel, success, reset). Responses
// from an older generation are dropped so a slow preview, settings or execute result for a
// previous action never shows up in the dialog of the current one.
let generation = 0;
// Bumped on every preview request, so a preview for an older replica count is dropped too.
let previewGeneration = 0;

function errorMessage(err: unknown): string {
  return err instanceof Error
    ? err.message
    : typeof err === "object" && err !== null && "message" in err
      ? String(err.message)
      : String(err);
}

function errorKindOf(err: unknown): ErrorKind | null {
  if (typeof err === "object" && err !== null && "kind" in err && typeof err.kind === "string") {
    return err.kind as ErrorKind;
  }
  return null;
}

/** Name the backend's protected-context guard compares the typed name against. */
export function actionTargetName(action: MutatingAction): string {
  return "node" in action ? action.node : action.name;
}

/** Human-readable target, e.g. "pod default/web-0" or "node worker-1". */
export function describeTarget(action: MutatingAction): string {
  if ("node" in action) return `node ${action.node}`;
  const ns = action.namespace ?? null;
  const name = ns !== null && ns !== "" ? `${ns}/${action.name}` : action.name;
  let kind = "resource";
  if ("kind" in action) {
    kind = action.kind.toLowerCase();
  } else if (action.type.startsWith("argoRollout")) {
    kind = "rollout";
  } else if (action.type === "argoAppSync") {
    kind = "application";
  } else if (action.type === "certRenew") {
    kind = "certificate";
  } else if (action.type === "externalSecretForceSync") {
    kind = "externalsecret";
  }
  return `${kind} ${name}`;
}

const VERBS: Record<MutatingAction["type"], string> = {
  delete: "Delete",
  scale: "Scale",
  restart: "Restart",
  cordon: "Cordon",
  uncordon: "Uncordon",
  drain: "Drain",
  fluxReconcile: "Reconcile",
  fluxSuspend: "Suspend / Resume",
  argoRolloutPromote: "Promote",
  argoRolloutAbort: "Abort",
  argoRolloutRetry: "Retry",
  argoAppSync: "Sync",
  certRenew: "Renew Certificate",
  externalSecretForceSync: "Force Sync",
};

export function replicasLabel(replicas: number): string {
  return `${String(replicas)} ${replicas === 1 ? "replica" : "replicas"}`;
}

// The API server accepted the request; the effect (pod terminating, rollout) happens later
// and shows up through the live watch, so the message says "requested", not "done".
function successMessage(pending: PendingAction): string {
  const { action, context } = pending;
  const verb =
    action.type === "scale" ? `Scale to ${replicasLabel(action.replicas)}` : VERBS[action.type];
  return `${verb} requested for ${describeTarget(action)} on ${context}`;
}

// Kinds the backend's `scale_workload` / `restart_workload` handle by name. Other kinds are
// not offered: the backend falls back to `Deployment` for an unknown kind.
const SCALABLE_KINDS = new Set(["deployment", "statefulset"]);
const RESTARTABLE_KINDS = new Set(["deployment", "statefulset", "daemonset"]);

export function canScale(row: Row): boolean {
  return SCALABLE_KINDS.has(row.kind.toLowerCase()) && row.namespace !== null;
}

export function canRestart(row: Row): boolean {
  return RESTARTABLE_KINDS.has(row.kind.toLowerCase()) && row.namespace !== null;
}

export function canFluxReconcile(row: Row): boolean {
  const k = row.kind.toLowerCase();
  return (k === "kustomization" || k === "helmrelease") && row.namespace !== null;
}

export function canArgoRollout(row: Row): boolean {
  return row.kind.toLowerCase() === "rollout" && row.namespace !== null;
}

export function canArgoAppSync(row: Row): boolean {
  return row.kind.toLowerCase() === "application" && row.namespace !== null;
}

export function canCertRenew(row: Row): boolean {
  return row.kind.toLowerCase() === "certificate" && row.namespace !== null;
}

export function canExternalSecretForceSync(row: Row): boolean {
  return row.kind.toLowerCase() === "externalsecret" && row.namespace !== null;
}

export function fluxReconcileActionFor(row: Row): PendingAction {
  return {
    context: row.cluster,
    action: { type: "fluxReconcile", kind: row.kind, namespace: row.namespace ?? "default", name: row.name },
    originUid: row.uid,
  };
}

export function fluxSuspendActionFor(row: Row, suspend: boolean): PendingAction {
  return {
    context: row.cluster,
    action: { type: "fluxSuspend", kind: row.kind, namespace: row.namespace ?? "default", name: row.name, suspend },
    originUid: row.uid,
  };
}

export function argoRolloutPromoteActionFor(row: Row, full: boolean): PendingAction {
  return {
    context: row.cluster,
    action: { type: "argoRolloutPromote", namespace: row.namespace ?? "default", name: row.name, full },
    originUid: row.uid,
  };
}

export function argoRolloutAbortActionFor(row: Row): PendingAction {
  return {
    context: row.cluster,
    action: { type: "argoRolloutAbort", namespace: row.namespace ?? "default", name: row.name },
    originUid: row.uid,
  };
}

export function argoRolloutRetryActionFor(row: Row): PendingAction {
  return {
    context: row.cluster,
    action: { type: "argoRolloutRetry", namespace: row.namespace ?? "default", name: row.name },
    originUid: row.uid,
  };
}

export function argoAppSyncActionFor(row: Row): PendingAction {
  return {
    context: row.cluster,
    action: { type: "argoAppSync", namespace: row.namespace ?? "default", name: row.name },
    originUid: row.uid,
  };
}

export function certRenewActionFor(row: Row): PendingAction {
  return {
    context: row.cluster,
    action: { type: "certRenew", namespace: row.namespace ?? "default", name: row.name },
    originUid: row.uid,
  };
}

export function externalSecretForceSyncActionFor(row: Row): PendingAction {
  return {
    context: row.cluster,
    action: { type: "externalSecretForceSync", namespace: row.namespace ?? "default", name: row.name },
    originUid: row.uid,
  };
}

export function deleteActionFor(row: Row): PendingAction {
  return {
    context: row.cluster,
    action: { type: "delete", kind: row.kind, namespace: row.namespace, name: row.name },
    originUid: row.uid,
  };
}

/**
 * Starts at the current desired count (`spec.replicas`, which workload rows carry as the
 * Ready cell's total). Without it the count is left unset and must be typed; it is never
 * guessed.
 */
export function scaleActionFor(row: Row): PendingAction | null {
  if (!canScale(row) || row.namespace === null) return null;
  const ready = row.cells.find((c) => c.type === "ready");
  const known = ready?.type === "ready" ? ready.value.total : null;
  return {
    context: row.cluster,
    action: {
      type: "scale",
      kind: row.kind,
      namespace: row.namespace,
      name: row.name,
      replicas: known ?? 0,
    },
    originUid: row.uid,
    ...(known === null ? { replicasUnset: true } : {}),
  };
}

export function restartActionFor(row: Row): PendingAction | null {
  if (!canRestart(row) || row.namespace === null) return null;
  return {
    context: row.cluster,
    action: { type: "restart", kind: row.kind, namespace: row.namespace, name: row.name },
    originUid: row.uid,
  };
}

export function resetActionsStore(): void {
  generation += 1;
  previewGeneration += 1;
  useActionsStore.setState({ ...initialActionsState });
}

export const useActionsStore = create<ActionsState>()((set, get) => {
  async function loadPreview(context: string, action: MutatingAction): Promise<void> {
    previewGeneration += 1;
    const gen = generation;
    const pgen = previewGeneration;
    set({ preview: null, previewError: null });
    try {
      const preview = await commands.previewKubectlAction(context, action);
      if (gen !== generation || pgen !== previewGeneration) return;
      set({ preview });
    } catch (err: unknown) {
      if (gen !== generation || pgen !== previewGeneration) return;
      set({ previewError: errorMessage(err) });
    }
  }

  async function loadSettings(context: string): Promise<void> {
    const gen = generation;
    try {
      const res = await commands.getContextSettings(context);
      if (gen !== generation) return;
      if (res.status === "error") {
        set({ settingsError: res.error.message });
        return;
      }
      set({ settings: res.data });
      // Keep the sidebar badges in line with what the backend enforces.
      useContextsStore.getState().setContextSettings(context, {
        colour: res.data.colour,
        tags: res.data.tags,
        readOnly: res.data.readOnly,
        protected: res.data.protected,
        pinned: res.data.pinned,
      });
    } catch (err: unknown) {
      if (gen !== generation) return;
      set({ settingsError: errorMessage(err) });
    }
  }

  return {
    ...initialActionsState,
    request: (pending) => {
      // One action at a time: replacing a running one would drop its result and re-enable
      // Confirm (a second execute); replacing a pending one would discard what the user sees.
      if (get().pending !== null || get().running) return false;
      generation += 1;
      set((state) => ({
        pending,
        requestId: state.requestId + 1,
        preview: null,
        previewError: null,
        settings: null,
        settingsError: null,
        running: false,
        error: null,
        errorKind: null,
        lastSuccess: null,
      }));
      if (pending.replicasUnset === true) {
        previewGeneration += 1;
      } else {
        void loadPreview(pending.context, pending.action);
      }
      void loadSettings(pending.context);
      return true;
    },
    setReplicas: (replicas) => {
      const { pending, running } = get();
      if (!pending || pending.action.type !== "scale" || running) return;
      if (!Number.isInteger(replicas) || replicas < 0 || replicas > MAX_REPLICAS) return;
      const next: PendingAction = {
        ...pending,
        action: { ...pending.action, replicas },
        replicasUnset: false,
      };
      set({ pending: next, error: null, errorKind: null });
      void loadPreview(next.context, next.action);
    },
    clearReplicas: () => {
      const { pending, running } = get();
      if (!pending || pending.action.type !== "scale" || running) return;
      // Drops any preview still in flight for the previous count.
      previewGeneration += 1;
      set({
        pending: { ...pending, replicasUnset: true },
        preview: null,
        previewError: null,
        error: null,
        errorKind: null,
      });
    },
    confirm: async (confirmedName) => {
      const { pending, running, preview, settings, settingsError } = get();
      if (!pending || running) return false;
      // Defence in depth; the dialog disables Confirm in the same cases.
      if (preview === null || pending.replicasUnset === true) return false;
      if (settings === null && settingsError === null) return false;
      const gen = generation;
      set({ running: true, error: null, errorKind: null });
      try {
        const res = await commands.executeAction(pending.context, pending.action, confirmedName);
        if (gen !== generation) return false;
        if (res.status === "error") {
          set({ running: false, error: res.error.message, errorKind: res.error.kind });
          return false;
        }
        // The live watch removes / updates the row; nothing is patched locally.
        generation += 1;
        set((state) => ({
          ...initialActionsState,
          requestId: state.requestId,
          selection: state.selection,
          lastSuccess: successMessage(pending),
        }));
        return true;
      } catch (err: unknown) {
        if (gen !== generation) return false;
        set({ running: false, error: errorMessage(err), errorKind: errorKindOf(err) });
        return false;
      }
    },
    cancel: () => {
      if (get().running) return;
      generation += 1;
      set({
        pending: null,
        preview: null,
        previewError: null,
        settings: null,
        settingsError: null,
        error: null,
        errorKind: null,
      });
    },
    clearSuccess: () => {
      set({ lastSuccess: null });
    },
    setSelection: (row) => {
      set({ selection: row });
    },
  };
});
