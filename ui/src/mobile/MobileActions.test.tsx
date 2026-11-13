import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ContextSettings_Serialize, MutatingAction_Deserialize } from "../generated/bindings";
import { closeTopBackLayer, hasBackLayer, resetBackLayers } from "../lib/backLayers";
import { resetActionsStore } from "../stores/actions";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import { type OverviewData, resetOverviewStore, useOverviewStore } from "../stores/overview";
import {
  type Page,
  type QuerySpec,
  type Row,
  resetPodsStore,
  setQueryFetcher,
} from "../stores/pods";
import { resetResourceDetailStore } from "../stores/resourceDetail";
import { MobileOverview } from "./MobileOverview";
import { MobileResourceList } from "./MobileResourceList";

const CREATED = new Date(Date.now() - 3600_000).toISOString();

function podRow(name = "web-pod-0"): Row {
  return {
    uid: `uid-${name}`,
    cluster: "kind-a",
    kind: "Pod",
    namespace: "shop",
    name,
    status: "healthy",
    statusReason: "Running",
    cells: [
      { type: "ready", value: { ready: 1, total: 1 } },
      { type: "status", value: { label: "Running", phase: "healthy" } },
      { type: "int", value: 0 },
      { type: "timestamp", value: "" },
      { type: "text", value: "" },
      { type: "text", value: "node-1" },
      { type: "text", value: "10.244.0.5" },
      { type: "timestamp", value: CREATED },
    ],
    labels: [["app", "web"]],
    custom: [],
  };
}

function deploymentRow(name = "web-deploy-0", replicas = 3): Row {
  return {
    uid: `uid-${name}`,
    cluster: "kind-a",
    kind: "Deployment",
    namespace: "shop",
    name,
    status: "healthy",
    statusReason: "Available",
    cells: [
      { type: "ready", value: { ready: replicas, total: replicas } },
      { type: "text", value: `${String(replicas)}/${String(replicas)}` },
      { type: "timestamp", value: CREATED },
    ],
    labels: [["app", "web"]],
    custom: [],
  };
}

function makePage(spec: QuerySpec, rows: Row[]): Page {
  return {
    total: rows.length,
    offset: spec.offset,
    limit: spec.limit,
    rows: rows.slice(spec.offset, spec.offset + spec.limit),
    version: 1,
    access: [{ cluster: "kind-a", kind: spec.kinds[0] ?? "Pod", access: { state: "ready" }, rows: 0, metrics: null }],
    statusCounts: { healthy: 0, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
  };
}

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

describe("03-05 Safe actions on mobile", () => {
  let calls: Call[] = [];
  let currentSettings: ContextSettings_Serialize = {
    colour: null,
    tags: [],
    readOnly: false,
    protected: false,
    pinned: false,
  };

  beforeEach(() => {
    calls = [];
    currentSettings = {
      colour: null,
      tags: [],
      readOnly: false,
      protected: false,
      pinned: false,
    };
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
    resetOverviewStore();
    resetBackLayers();
    useLayoutStore.setState({ ...initialLayoutState, tabs: [...initialLayoutState.tabs] });
    useContextsStore.setState({ ...initialContextsState, selectedContext: "kind-a" });

    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "get_context_settings":
          return currentSettings;
        case "preview_kubectl_action": {
          const action = a.action as MutatingAction_Deserialize;
          if (action.type === "delete") {
            return `kubectl delete ${action.kind.toLowerCase()} ${action.name} -n ${action.namespace ?? "default"}`;
          }
          if (action.type === "restart") {
            return `kubectl rollout restart ${action.kind.toLowerCase()}/${action.name} -n ${action.namespace}`;
          }
          if (action.type === "scale") {
            return `kubectl scale ${action.kind.toLowerCase()}/${action.name} --replicas=${String(action.replicas)} -n ${action.namespace}`;
          }
          return "kubectl preview";
        }
        case "execute_action":
          return null;
        case "get_resource_detail":
          return { yaml: "apiVersion: v1\nkind: Pod\n", events: [] };
        default:
          return undefined;
      }
    });
  });

  afterEach(() => {
    clearMocks();
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
    resetOverviewStore();
    resetBackLayers();
  });

  it("triggers Delete action from Pod mobile detail screen and confirms successfully", async () => {
    const user = userEvent.setup();
    setQueryFetcher((spec) => Promise.resolve(makePage(spec, [podRow("web-pod-0")])));

    render(<MobileResourceList kind="Pod" />);
    await user.click(await screen.findByTestId("m-row"));

    const detailScreen = screen.getByRole("region", { name: "Resource details" });
    expect(detailScreen).toHaveClass("resource-drawer--screen");

    // Pod has delete, but no restart or scale
    expect(within(detailScreen).getByTestId("drawer-action-delete")).toBeInTheDocument();
    expect(within(detailScreen).queryByTestId("drawer-action-restart")).toBeNull();
    expect(within(detailScreen).queryByTestId("drawer-action-scale")).toBeNull();

    // Tap Delete button
    await user.click(within(detailScreen).getByTestId("drawer-action-delete"));

    const dialog = await screen.findByRole("dialog", { name: "Delete pod shop/web-pod-0" });
    expect(dialog).toBeInTheDocument();
    expect(await within(dialog).findByTestId("kubectl-preview")).toHaveTextContent(
      "kubectl delete pod web-pod-0 -n shop",
    );

    // Confirm button is enabled on unprotected cluster
    const confirmBtn = within(dialog).getByTestId("confirm-action-btn");
    await waitFor(() => {
      expect(confirmBtn).toBeEnabled();
    });

    await user.click(confirmBtn);

    // Backend received execute_action
    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "execute_action")).toBe(true);
    });
    const execCall = calls.find((c) => c.cmd === "execute_action");
    expect(execCall?.args).toMatchObject({
      context: "kind-a",
      action: { type: "delete", kind: "Pod", namespace: "shop", name: "web-pod-0" },
      confirmedName: null,
    });

    // Dialog closes, but detail screen remains open
    await waitFor(() => {
      expect(screen.queryByTestId("action-confirm-dialog")).toBeNull();
    });
    expect(screen.getByRole("region", { name: "Resource details" })).toBeInTheDocument();
  });

  it("protected context requires typing exact resource name to confirm on mobile", async () => {
    currentSettings = { ...currentSettings, protected: true };
    const user = userEvent.setup();
    setQueryFetcher((spec) => Promise.resolve(makePage(spec, [podRow("web-pod-0")])));

    render(<MobileResourceList kind="Pod" />);
    await user.click(await screen.findByTestId("m-row"));
    await user.click(screen.getByTestId("drawer-action-delete"));

    const dialog = await screen.findByRole("dialog", { name: "Delete pod shop/web-pod-0" });
    const confirmBtn = within(dialog).getByTestId("confirm-action-btn");

    // Blocked until typed
    await waitFor(() => {
      expect(confirmBtn).toBeDisabled();
    });
    expect(within(dialog).getByLabelText("Type resource name to confirm")).toBeInTheDocument();

    const input = within(dialog).getByLabelText("Type resource name to confirm");
    await user.type(input, "wrong-name");
    expect(confirmBtn).toBeDisabled();

    await user.clear(input);
    await user.type(input, "web-pod-0");
    await waitFor(() => {
      expect(confirmBtn).toBeEnabled();
    });

    await user.click(confirmBtn);
    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "execute_action")).toBe(true);
    });
    const execCall = calls.find((c) => c.cmd === "execute_action");
    expect(execCall?.args).toMatchObject({
      context: "kind-a",
      confirmedName: "web-pod-0",
    });
  });

  it("read-only context disables Delete, Restart, and Scale buttons on mobile detail view", async () => {
    const user = userEvent.setup();
    useContextsStore.setState({
      settingsByContext: {
        "kind-a": { colour: null, readOnly: true, protected: false, tags: [], pinned: false },
      },
    });
    setQueryFetcher((spec) => Promise.resolve(makePage(spec, [deploymentRow("web-deploy-0")])));

    render(<MobileResourceList kind="Deployment" />);
    await user.click(await screen.findByTestId("m-row"));

    const detailScreen = screen.getByRole("region", { name: "Resource details" });
    const restartBtn = within(detailScreen).getByTestId("drawer-action-restart");
    const scaleBtn = within(detailScreen).getByTestId("drawer-action-scale");
    const deleteBtn = within(detailScreen).getByTestId("drawer-action-delete");

    expect(restartBtn).toBeDisabled();
    expect(scaleBtn).toBeDisabled();
    expect(deleteBtn).toBeDisabled();

    expect(restartBtn).toHaveAttribute(
      "title",
      "Context kind-a is read-only: write actions are disabled.",
    );

    await user.click(deleteBtn);
    expect(screen.queryByTestId("action-confirm-dialog")).toBeNull();
  });

  it("Deployment detail screen triggers Restart with rollout restart preview and executes", async () => {
    const user = userEvent.setup();
    setQueryFetcher((spec) => Promise.resolve(makePage(spec, [deploymentRow("web-deploy-0")])));

    render(<MobileResourceList kind="Deployment" />);
    await user.click(await screen.findByTestId("m-row"));

    const detailScreen = screen.getByRole("region", { name: "Resource details" });
    await user.click(within(detailScreen).getByTestId("drawer-action-restart"));

    const dialog = await screen.findByRole("dialog", {
      name: "Restart deployment shop/web-deploy-0",
    });
    expect(dialog).toBeInTheDocument();
    expect(await within(dialog).findByTestId("kubectl-preview")).toHaveTextContent(
      "kubectl rollout restart deployment/web-deploy-0 -n shop",
    );

    const confirmBtn = within(dialog).getByTestId("confirm-action-btn");
    await waitFor(() => {
      expect(confirmBtn).toBeEnabled();
    });
    await user.click(confirmBtn);

    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "execute_action")).toBe(true);
    });
    const execCall = calls.find((c) => c.cmd === "execute_action");
    expect(execCall?.args).toMatchObject({
      context: "kind-a",
      action: { type: "restart", kind: "Deployment", namespace: "shop", name: "web-deploy-0" },
    });
  });

  it("Deployment detail screen triggers Scale, edits replicas, updates preview and executes", async () => {
    const user = userEvent.setup();
    setQueryFetcher((spec) => Promise.resolve(makePage(spec, [deploymentRow("web-deploy-0", 3)])));

    render(<MobileResourceList kind="Deployment" />);
    await user.click(await screen.findByTestId("m-row"));

    const detailScreen = screen.getByRole("region", { name: "Resource details" });
    await user.click(within(detailScreen).getByTestId("drawer-action-scale"));

    const dialog = await screen.findByRole("dialog", {
      name: "Scale deployment shop/web-deploy-0",
    });
    const replicasInput = within(dialog).getByLabelText("Replicas");
    expect(replicasInput).toHaveValue("3");

    expect(await within(dialog).findByTestId("kubectl-preview")).toHaveTextContent(
      "kubectl scale deployment/web-deploy-0 --replicas=3 -n shop",
    );

    // Clear replicas field -> error and confirm disabled
    const confirmBtn = within(dialog).getByTestId("confirm-action-btn");
    await user.clear(replicasInput);
    expect(confirmBtn).toBeDisabled();
    expect(within(dialog).getByTestId("replicas-error")).toHaveTextContent(
      "Enter the number of replicas.",
    );

    // Type new replica count 5
    await user.type(replicasInput, "5");
    await waitFor(() => {
      expect(within(dialog).getByTestId("kubectl-preview")).toHaveTextContent(
        "kubectl scale deployment/web-deploy-0 --replicas=5 -n shop",
      );
    });
    expect(confirmBtn).toBeEnabled();

    await user.click(confirmBtn);
    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "execute_action")).toBe(true);
    });
    const execCall = calls.find((c) => c.cmd === "execute_action");
    expect(execCall?.args).toMatchObject({
      context: "kind-a",
      action: {
        type: "scale",
        kind: "Deployment",
        namespace: "shop",
        name: "web-deploy-0",
        replicas: 5,
      },
    });
  });

  it("closing confirm dialog via back-layer keeps the mobile detail screen open", async () => {
    const user = userEvent.setup();
    setQueryFetcher((spec) => Promise.resolve(makePage(spec, [podRow("web-pod-0")])));

    render(<MobileResourceList kind="Pod" />);
    await user.click(await screen.findByTestId("m-row"));
    await user.click(screen.getByTestId("drawer-action-delete"));

    expect(await screen.findByTestId("action-confirm-dialog")).toBeInTheDocument();
    expect(hasBackLayer()).toBe(true);

    // Back gesture triggers top back layer
    act(() => {
      expect(closeTopBackLayer()).toBe(true);
    });

    // Confirm dialog is closed
    await waitFor(() => {
      expect(screen.queryByTestId("action-confirm-dialog")).toBeNull();
    });

    // Detail screen remains open in screen mode
    expect(screen.getByRole("region", { name: "Resource details" })).toBeInTheDocument();

    // Tapping Back on detail screen closes the screen
    await user.click(screen.getByTestId("detail-back"));
    expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
  });

  it("MobileOverview detail screen triggers safe actions with confirm dialog", async () => {
    const user = userEvent.setup();
    const overviewData: OverviewData = {
      clusters: [
        {
          cluster: "kind-a",
          connection: { state: "connected" },
          nodesReady: 1,
          nodesTotal: 1,
          podsRunning: 5,
          podsPending: 0,
          podsFailing: 1,
          warningEvents1h: 2,
          metrics: { state: "notInstalled", message: "metrics.k8s.io not served" },
          cpu: null,
          memory: null,
          problems: [],
        },
      ],
      attention: {
        items: [
          {
            cluster: "kind-a",
            kind: "Deployment",
            namespace: "shop",
            name: "web-deploy-fail",
            reason: "ProgressDeadlineExceeded",
            message: "ReplicaSet did not progress",
            since: null,
            status: "failed",
          },
        ],
        truncated: false,
      },
      recentWarnings: [],
      rightSizing: null,
    };
    useOverviewStore.setState({ status: "ready", data: overviewData, lastUpdatedAt: Date.now() });

    render(<MobileOverview onOpenPods={() => {}} />);

    // Tap attention item
    const attentionBtn = screen.getByRole("button", { name: /web-deploy-fail/ });
    await user.click(attentionBtn);

    const detailScreen = await screen.findByRole("region", { name: "Resource details" });
    expect(detailScreen).toHaveClass("resource-drawer--screen");

    // Tapping Delete opens ActionConfirmDialog
    await user.click(within(detailScreen).getByTestId("drawer-action-delete"));
    expect(await screen.findByTestId("action-confirm-dialog")).toBeInTheDocument();

    // Cancel closes dialog, keeps screen open
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(screen.queryByTestId("action-confirm-dialog")).toBeNull();
    });
    expect(screen.getByRole("region", { name: "Resource details" })).toBeInTheDocument();
  });
});
