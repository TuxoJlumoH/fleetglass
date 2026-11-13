import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  argoRolloutPromoteActionFor,
  canArgoRollout,
  canCertRenew,
  canExternalSecretForceSync,
  canFluxReconcile,
  canRestart,
  canScale,
  certRenewActionFor,
  deleteActionFor,
  externalSecretForceSyncActionFor,
  fluxReconcileActionFor,
  resetActionsStore,
  restartActionFor,
  scaleActionFor,
  useActionsStore,
  type PendingAction,
} from "./actions";
import { initialContextsState, useContextsStore } from "./contexts";
import type { Row } from "./pods";

const pod: Row = {
  uid: "uid-web-0",
  cluster: "kind-dev",
  kind: "Pod",
  namespace: "default",
  name: "web-0",
  status: "healthy",
  statusReason: "Running",
  cells: [{ type: "ready", value: { ready: 1, total: 1 } }],
  labels: [],
  custom: [],
};

const deployment: Row = {
  ...pod,
  uid: "uid-web",
  kind: "Deployment",
  name: "web",
  cells: [{ type: "ready", value: { ready: 2, total: 3 } }],
};

const settings = { colour: null, tags: [], readOnly: false, protected: false, pinned: false };

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("actions store (R-07)", () => {
  let calls: Call[];

  beforeEach(() => {
    resetActionsStore();
    useContextsStore.setState({ ...initialContextsState });
    mockWindows("main");
    calls = [];
  });

  afterEach(() => {
    clearMocks();
    resetActionsStore();
  });

  function mockBackend(over: Partial<Record<string, (args: Record<string, unknown>) => unknown>> = {}) {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      const handler = over[cmd];
      if (handler) return handler(a);
      if (cmd === "preview_kubectl_action") return `preview:${JSON.stringify(a.action)}`;
      if (cmd === "get_context_settings") return settings;
      if (cmd === "execute_action") return null;
      return undefined;
    });
  }

  it("request loads the backend preview and the context settings", async () => {
    mockBackend({
      preview_kubectl_action: () => "kubectl --context kind-dev -n default delete pod web-0",
      get_context_settings: () => ({ ...settings, protected: true }),
    });
    useActionsStore.getState().request(deleteActionFor(pod));
    expect(useActionsStore.getState().pending?.action).toEqual({
      type: "delete",
      kind: "Pod",
      namespace: "default",
      name: "web-0",
    });
    await flush();
    const s = useActionsStore.getState();
    expect(s.preview).toBe("kubectl --context kind-dev -n default delete pod web-0");
    expect(s.settings?.protected).toBe(true);
    expect(calls.find((c) => c.cmd === "preview_kubectl_action")?.args).toEqual({
      context: "kind-dev",
      action: { type: "delete", kind: "Pod", namespace: "default", name: "web-0" },
    });
    // The sidebar copy follows the backend's settings.
    expect(useContextsStore.getState().settingsByContext["kind-dev"]?.protected).toBe(true);
  });

  it("drops a stale preview from a previous request", async () => {
    const first = deferred<string>();
    mockBackend({
      preview_kubectl_action: (a) =>
        (a.action as { name: string }).name === "web-0" ? first.promise : "preview-web-1",
    });
    useActionsStore.getState().request(deleteActionFor(pod));
    useActionsStore.getState().cancel();
    useActionsStore.getState().request(deleteActionFor({ ...pod, uid: "u1", name: "web-1" }));
    await flush();
    expect(useActionsStore.getState().preview).toBe("preview-web-1");
    first.resolve("preview-web-0");
    await flush();
    expect(useActionsStore.getState().preview).toBe("preview-web-1");
    expect(useActionsStore.getState().pending?.action).toMatchObject({ name: "web-1" });
  });

  it("drops a preview that arrives after cancel", async () => {
    const slow = deferred<string>();
    mockBackend({ preview_kubectl_action: () => slow.promise });
    useActionsStore.getState().request(deleteActionFor(pod));
    useActionsStore.getState().cancel();
    slow.resolve("late");
    await flush();
    expect(useActionsStore.getState().pending).toBeNull();
    expect(useActionsStore.getState().preview).toBeNull();
  });

  it("setReplicas refreshes the preview and ignores an older replica count's preview", async () => {
    const forOne = deferred<string>();
    mockBackend({
      preview_kubectl_action: (a) => {
        const replicas = (a.action as { replicas: number }).replicas;
        return replicas === 1 ? forOne.promise : `scale to ${String(replicas)}`;
      },
    });
    const pending = scaleActionFor(deployment);
    if (!pending) throw new Error("deployment should be scalable");
    useActionsStore.getState().request(pending);
    await flush();
    expect(useActionsStore.getState().preview).toBe("scale to 3");
    useActionsStore.getState().setReplicas(1);
    useActionsStore.getState().setReplicas(5);
    await flush();
    forOne.resolve("scale to 1");
    await flush();
    expect(useActionsStore.getState().preview).toBe("scale to 5");
    expect(useActionsStore.getState().pending?.action).toMatchObject({ replicas: 5 });
  });

  it("preview failure is reported, not replaced by a client-side string", async () => {
    mockBackend({
      preview_kubectl_action: () => {
        throw new Error("command preview_kubectl_action not allowed");
      },
    });
    useActionsStore.getState().request(deleteActionFor(pod));
    await flush();
    expect(useActionsStore.getState().preview).toBeNull();
    expect(useActionsStore.getState().previewError).toBe(
      "command preview_kubectl_action not allowed",
    );
  });

  it("confirm passes confirmedName, closes the action and records the success", async () => {
    mockBackend();
    useActionsStore.getState().setSelection(pod);
    useActionsStore.getState().request(deleteActionFor(pod));
    await flush();
    const ok = await useActionsStore.getState().confirm("web-0");
    expect(ok).toBe(true);
    expect(calls.find((c) => c.cmd === "execute_action")?.args).toEqual({
      context: "kind-dev",
      action: { type: "delete", kind: "Pod", namespace: "default", name: "web-0" },
      confirmedName: "web-0",
    });
    const s = useActionsStore.getState();
    expect(s.pending).toBeNull();
    expect(s.running).toBe(false);
    expect(s.lastSuccess).toBe("Delete requested for pod default/web-0 on kind-dev");
    expect(s.selection).toBe(pod);
  });

  it.each([
    ["readOnlyContext", "context kind-dev is read-only"],
    ["confirmationRequired", "type web-0 to confirm on protected context kind-dev"],
    ["kubernetes", 'pods "web-0" is forbidden'],
  ] as const)("keeps the dialog open and exposes a %s backend error", async (kind, message) => {
    mockBackend({
      execute_action: () => {
        // Tauri rejects with the serialised IpcError.
        throw { kind, message } as unknown as Error;
      },
    });
    useActionsStore.getState().request(deleteActionFor(pod));
    await flush();
    const ok = await useActionsStore.getState().confirm(null);
    expect(ok).toBe(false);
    const s = useActionsStore.getState();
    expect(s.pending).not.toBeNull();
    expect(s.running).toBe(false);
    expect(s.error).toBe(message);
    expect(s.errorKind).toBe(kind);
    expect(s.lastSuccess).toBeNull();
  });

  it("drops an execute result for an action that was replaced meanwhile", async () => {
    const exec = deferred<null>();
    mockBackend({ execute_action: () => exec.promise });
    useActionsStore.getState().request(deleteActionFor(pod));
    await flush();
    const result = useActionsStore.getState().confirm(null);
    expect(useActionsStore.getState().running).toBe(true);
    // cancel is refused while running.
    useActionsStore.getState().cancel();
    expect(useActionsStore.getState().pending).not.toBeNull();
    resetActionsStore();
    exec.resolve(null);
    expect(await result).toBe(false);
    expect(useActionsStore.getState().lastSuccess).toBeNull();
  });

  it("request is refused while an action runs: one execute, and its result is shown", async () => {
    const exec = deferred<null>();
    mockBackend({ execute_action: () => exec.promise });
    useActionsStore.getState().request(deleteActionFor(pod));
    await flush();
    const result = useActionsStore.getState().confirm(null);
    expect(useActionsStore.getState().running).toBe(true);
    const requestId = useActionsStore.getState().requestId;

    // E.g. ctrl-k → "Delete pod web-1" while the first delete is in flight.
    const other = deleteActionFor({ ...pod, uid: "u1", name: "web-1" });
    expect(useActionsStore.getState().request(other)).toBe(false);
    // A second confirm is refused as well.
    expect(await useActionsStore.getState().confirm(null)).toBe(false);
    let s = useActionsStore.getState();
    expect(s.running).toBe(true);
    expect(s.requestId).toBe(requestId);
    expect(s.pending?.action).toMatchObject({ name: "web-0" });

    exec.resolve(null);
    expect(await result).toBe(true);
    s = useActionsStore.getState();
    expect(calls.filter((c) => c.cmd === "execute_action")).toHaveLength(1);
    expect(s.running).toBe(false);
    expect(s.pending).toBeNull();
    expect(s.lastSuccess).toBe("Delete requested for pod default/web-0 on kind-dev");
  });

  it("request is refused while another action is pending; it works after cancel", async () => {
    mockBackend();
    expect(useActionsStore.getState().request(deleteActionFor(pod))).toBe(true);
    await flush();
    const other = deleteActionFor({ ...pod, uid: "u1", name: "web-1" });
    expect(useActionsStore.getState().request(other)).toBe(false);
    expect(useActionsStore.getState().pending?.action).toMatchObject({ name: "web-0" });
    expect(useActionsStore.getState().preview).toContain("web-0");
    useActionsStore.getState().cancel();
    expect(useActionsStore.getState().request(other)).toBe(true);
    expect(useActionsStore.getState().pending?.action).toMatchObject({ name: "web-1" });
  });

  it("confirm is refused without a preview or while the context settings are loading", async () => {
    const settingsLoad = deferred<typeof settings>();
    const previewLoad = deferred<string>();
    mockBackend({
      get_context_settings: () => settingsLoad.promise,
      preview_kubectl_action: () => previewLoad.promise,
    });
    useActionsStore.getState().request(deleteActionFor(pod));
    await flush();
    // Neither preview nor settings yet.
    expect(await useActionsStore.getState().confirm(null)).toBe(false);
    previewLoad.resolve("kubectl delete pod web-0");
    await flush();
    // Preview there, settings still loading.
    expect(await useActionsStore.getState().confirm(null)).toBe(false);
    settingsLoad.resolve(settings);
    await flush();
    expect(calls.some((c) => c.cmd === "execute_action")).toBe(false);
    expect(await useActionsStore.getState().confirm(null)).toBe(true);
    expect(calls.filter((c) => c.cmd === "execute_action")).toHaveLength(1);
  });

  it("confirm is refused when the preview failed, even with settings loaded", async () => {
    mockBackend({
      preview_kubectl_action: () => {
        throw new Error("no preview");
      },
    });
    useActionsStore.getState().request(deleteActionFor(pod));
    await flush();
    expect(useActionsStore.getState().settings).not.toBeNull();
    expect(await useActionsStore.getState().confirm(null)).toBe(false);
    expect(calls.some((c) => c.cmd === "execute_action")).toBe(false);
  });

  it("scale without a known desired count leaves replicas unset and loads no preview", async () => {
    mockBackend();
    const pending = scaleActionFor({ ...deployment, cells: [] });
    expect(pending?.replicasUnset).toBe(true);
    if (!pending) throw new Error("deployment should be scalable");
    useActionsStore.getState().request(pending);
    await flush();
    expect(calls.some((c) => c.cmd === "preview_kubectl_action")).toBe(false);
    expect(await useActionsStore.getState().confirm(null)).toBe(false);

    useActionsStore.getState().setReplicas(2);
    await flush();
    expect(useActionsStore.getState().pending?.replicasUnset).toBe(false);
    expect(useActionsStore.getState().preview).toContain('"replicas":2');

    // Clearing the field drops the preview again and blocks confirm.
    useActionsStore.getState().clearReplicas();
    expect(useActionsStore.getState().preview).toBeNull();
    expect(await useActionsStore.getState().confirm(null)).toBe(false);

    // Out-of-range counts never reach the backend.
    useActionsStore.getState().setReplicas(2147483648);
    expect(useActionsStore.getState().pending?.replicasUnset).toBe(true);
    expect(calls.some((c) => c.cmd === "execute_action")).toBe(false);
  });

  it("success messages say the action was requested, with singular replica wording", async () => {
    mockBackend();
    const scale = scaleActionFor(deployment);
    if (!scale) throw new Error("deployment should be scalable");
    useActionsStore.getState().request(scale);
    await flush();
    useActionsStore.getState().setReplicas(1);
    await flush();
    await useActionsStore.getState().confirm(null);
    expect(useActionsStore.getState().lastSuccess).toBe(
      "Scale to 1 replica requested for deployment default/web on kind-dev",
    );

    const restart = restartActionFor(deployment);
    if (!restart) throw new Error("deployment should be restartable");
    useActionsStore.getState().request(restart);
    await flush();
    await useActionsStore.getState().confirm(null);
    expect(useActionsStore.getState().lastSuccess).toBe(
      "Restart requested for deployment default/web on kind-dev",
    );
  });

  it("only workloads can be scaled or restarted", () => {
    expect(canScale(pod)).toBe(false);
    expect(canRestart(pod)).toBe(false);
    expect(scaleActionFor(pod)).toBeNull();
    expect(restartActionFor(pod)).toBeNull();
    expect(canScale(deployment)).toBe(true);
    expect(canScale({ ...deployment, kind: "DaemonSet" })).toBe(false);
    expect(canRestart({ ...deployment, kind: "DaemonSet" })).toBe(true);
    const restart: PendingAction | null = restartActionFor(deployment);
    expect(restart?.action).toEqual({
      type: "restart",
      kind: "Deployment",
      namespace: "default",
      name: "web",
    });
  });

  it("ecosystem actions can be created for their respective kinds", () => {
    const kustomization: Row = { ...pod, kind: "Kustomization", name: "infra" };
    expect(canFluxReconcile(kustomization)).toBe(true);
    expect(fluxReconcileActionFor(kustomization).action).toEqual({
      type: "fluxReconcile",
      kind: "Kustomization",
      namespace: "default",
      name: "infra",
    });

    const rollout: Row = { ...pod, kind: "Rollout", name: "web-ro" };
    expect(canArgoRollout(rollout)).toBe(true);
    expect(argoRolloutPromoteActionFor(rollout, true).action).toEqual({
      type: "argoRolloutPromote",
      namespace: "default",
      name: "web-ro",
      full: true,
    });

    const cert: Row = { ...pod, kind: "Certificate", name: "tls-cert" };
    expect(canCertRenew(cert)).toBe(true);
    expect(certRenewActionFor(cert).action).toEqual({
      type: "certRenew",
      namespace: "default",
      name: "tls-cert",
    });

    const es: Row = { ...pod, kind: "ExternalSecret", name: "db-pass" };
    expect(canExternalSecretForceSync(es)).toBe(true);
    expect(externalSecretForceSyncActionFor(es).action).toEqual({
      type: "externalSecretForceSync",
      namespace: "default",
      name: "db-pass",
    });
  });
});
