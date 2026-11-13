import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ContextList_Serialize, IpcError } from "../generated/bindings";
import { App } from "../App";
import {
  initialContextsState,
  setConnectHandler,
  useContextsStore,
} from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { podsLayoutState } from "../test/layout";
import { resetPodsStore, setQueryFetcher, usePodsStore, type Row } from "../stores/pods";
import { PodTable } from "./PodTable";
import { Sidebar } from "./Sidebar";

const LIST: ContextList_Serialize = {
  contexts: [
    {
      name: "dev",
      cluster: "dev-cluster",
      user: "dev-user",
      namespace: "team-a",
      server: "https://dev.example.test:6443",
      auth: { type: "token" },
      isCurrent: true,
    },
    {
      name: "eks-prod",
      cluster: "eks",
      user: "eks-user",
      namespace: null,
      server: "https://eks.example.test",
      auth: { type: "exec", command: "/usr/local/bin/aws" },
      isCurrent: false,
    },
    {
      name: "sso",
      cluster: "c",
      user: "u",
      namespace: null,
      server: null,
      auth: { type: "oidc" },
      isCurrent: false,
    },
  ],
  current: "dev",
  warnings: [],
};

/** Pretend to be inside Tauri and answer `list_contexts` with `respond`. */
function mockBackend(respond: () => unknown): string[] {
  (globalThis as { isTauri?: boolean }).isTauri = true;
  mockWindows("main");
  const calls: string[] = [];
  mockIPC((cmd, args) => {
    calls.push(cmd);
    if (cmd === "list_contexts") return respond();
    // App loads cluster groups on mount.
    if (cmd === "get_cluster_groups") return [];
    if (cmd === "reload_team_settings") {
      return {
        loadedPath: (args as { path?: string }).path ?? "team-settings.yaml",
        appliedGroups: 2,
        appliedContexts: 4,
        warnings: ["ignored unknown field 'extra'"],
      };
    }
    return undefined;
  });
  return calls;
}

async function renderLoaded() {
  render(<Sidebar />);
  await act(() => useContextsStore.getState().loadContexts());
}

describe("Sidebar contexts", () => {
  beforeEach(() => {
    useLayoutStore.setState(podsLayoutState);
    useContextsStore.setState(initialContextsState);
    setConnectHandler();
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("outside Tauri shows the not-in-desktop-app state and does not invoke", async () => {
    await renderLoaded();
    expect(screen.getByText("Not running in the Fleetglass app")).toBeInTheDocument();
    expect(useContextsStore.getState().status).toBe("idle");
  });

  it("lists contexts with namespace and auth badges", async () => {
    const calls = mockBackend(() => LIST);
    await renderLoaded();

    // The nav polls `kind_counts` for the selected context (02-07c) and the per-context settings
    // are loaded once (local, no cluster I/O); nothing else is invoked.
    expect(calls.filter((c) => c !== "kind_counts" && c !== "get_context_settings")).toEqual([
      "list_contexts",
    ]);
    expect(calls.filter((c) => c === "get_context_settings")).toHaveLength(3);
    const items = within(screen.getByRole("list", { name: "Kubeconfig contexts" })).getAllByRole(
      "listitem",
    );
    expect(items).toHaveLength(3);
    expect(items[0]).toHaveTextContent("dev");
    expect(items[0]).toHaveTextContent("team-a");
    expect(items[0]).toHaveTextContent("token");
    expect(items[1]).toHaveTextContent("eks-prod");
    expect(items[1]).toHaveTextContent("exec: aws");
    expect(items[2]).toHaveTextContent("oidc");
  });

  it("marks the current context", async () => {
    mockBackend(() => LIST);
    await renderLoaded();

    const current = screen.getByText("dev").closest("li");
    expect(current).toHaveAttribute("aria-current", "true");
    expect(current).toHaveTextContent("(current)");
    expect(screen.getByText("eks-prod").closest("li")).not.toHaveAttribute("aria-current");
  });

  it("shows the backend error message", async () => {
    const err: IpcError = {
      kind: "kubeconfigInvalid",
      message: "kubeconfig file /x/config (line 7, column 6) is invalid: bad indentation",
    };
    mockBackend(() => {
      // Tauri rejects with the serialized `IpcError` object, not an `Error`.
      // eslint-disable-next-line @typescript-eslint/only-throw-error
      throw err;
    });
    await renderLoaded();

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Cannot load contexts: kubeconfig file /x/config (line 7, column 6) is invalid: bad indentation",
    );
    expect(useContextsStore.getState().error?.kind).toBe("kubeconfigInvalid");
  });

  it("shows warnings next to the list", async () => {
    mockBackend(() => ({
      ...LIST,
      warnings: ["kubeconfig file /nope/config does not exist; skipped"],
    }));
    await renderLoaded();

    const notice = screen.getByRole("list", { name: "Kubeconfig warnings" });
    expect(notice).toHaveTextContent("kubeconfig file /nope/config does not exist; skipped");
    expect(screen.getByText("dev")).toBeInTheDocument();
  });

  it("shows an empty state when the kubeconfig has no contexts", async () => {
    mockBackend(() => ({ contexts: [], current: null, warnings: [] }));
    await renderLoaded();
    expect(screen.getByText("No contexts in kubeconfig")).toBeInTheDocument();
  });

  it("moves focus with ArrowUp/ArrowDown using a roving tabindex", async () => {
    const user = userEvent.setup();
    mockBackend(() => LIST);
    await renderLoaded();

    const [dev, eks, sso] = screen.getAllByRole("listitem");
    // Only the current context is in the tab order.
    expect(dev).toHaveAttribute("tabindex", "0");
    expect(eks).toHaveAttribute("tabindex", "-1");

    await user.tab();
    expect(dev).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(eks).toHaveFocus();
    expect(eks).toHaveAttribute("tabindex", "0");
    expect(dev).toHaveAttribute("tabindex", "-1");
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(sso).toHaveFocus();
    await user.keyboard("{ArrowUp}");
    expect(eks).toHaveFocus();
  });

  it("lazy-connects on selection and updates status lights (connected, degraded, error)", async () => {
    const user = userEvent.setup();
    mockBackend(() => LIST);
    await renderLoaded();

    // Nothing is known on load (lazy connect): neutral dots, no "Disconnected" claim.
    const devLight = screen.getByTestId("status-light-dev");
    const eksLight = screen.getByTestId("status-light-eks-prod");
    const ssoLight = screen.getByTestId("status-light-sso");
    expect(devLight).toHaveClass("dot", "off");
    expect(devLight).toHaveAttribute("aria-label", "No connection status");
    expect(eksLight).toHaveClass("dot", "off");
    expect(ssoLight).toHaveClass("dot", "off");

    // Without a connect handler, selecting makes no claim either.
    await user.click(screen.getByText("eks-prod"));
    expect(useContextsStore.getState().selectedContext).toBe("eks-prod");
    expect(eksLight).toHaveAttribute("aria-label", "No connection status");

    // A connect handler that reports success -> green dot, Connected.
    setConnectHandler(() => Promise.resolve("connected"));
    await user.click(screen.getByText("dev"));
    expect(devLight).toHaveClass("dot", "ok");
    expect(devLight).toHaveAttribute("aria-label", "Connected");
    expect(useContextsStore.getState().selectedContext).toBe("dev");

    // Simulating an error when connecting to eks-prod -> red dot, Error.
    setConnectHandler(() => Promise.reject(new Error("TLS handshake timeout")));
    await user.click(screen.getByText("eks-prod"));
    expect(eksLight).toHaveClass("dot", "err");
    expect(eksLight).toHaveAttribute("aria-label", "Error: TLS handshake timeout");

    // Simulating degraded status via Enter key on sso -> amber dot, Degraded.
    setConnectHandler(() => Promise.resolve({ state: "degraded", message: "API latency high" }));
    const ssoItem = screen.getByText("sso").closest("li");
    expect(ssoItem).not.toBeNull();
    ssoItem?.focus();
    await user.keyboard("{Enter}");
    expect(ssoLight).toHaveClass("dot", "warn");
    expect(ssoLight).toHaveAttribute("aria-label", "Degraded: API latency high");
  });

  it("derives the dot of a queried context from the table's watch-store access", async () => {
    mockBackend(() => LIST);
    await renderLoaded();
    const devLight = screen.getByTestId("status-light-dev");
    expect(devLight).toHaveAttribute("aria-label", "No connection status");

    act(() => {
      usePodsStore.setState({
        access: [
          { cluster: "dev", kind: "Pod", access: { state: "reconnecting" }, rows: 1, metrics: null },
        ],
      });
    });
    expect(devLight).toHaveClass("dot", "warn");
    expect(devLight).toHaveAttribute("aria-label", "Reconnecting (re-listing)");

    act(() => {
      usePodsStore.setState({
        access: [{ cluster: "dev", kind: "Pod", access: { state: "ready" }, rows: 1, metrics: null }],
      });
    });
    expect(devLight).toHaveClass("dot", "ok");
    // Not queried: no claim even though a store state exists elsewhere.
    expect(screen.getByTestId("status-light-sso")).toHaveAttribute(
      "aria-label",
      "No connection status",
    );
    act(() => {
      resetPodsStore();
    });
  });

  it("loads every context's persisted settings once after listing, so swatches show colours", async () => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    const settingsCalls: string[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "list_contexts") return LIST;
      if (cmd === "get_context_settings") {
        const context = (args as { context: string }).context;
        settingsCalls.push(context);
        return {
          colour: context === "eks-prod" ? "#22aa88" : null,
          tags: [],
          readOnly: context === "eks-prod",
          protected: false,
          pinned: false,
        };
      }
      return undefined;
    });
    await renderLoaded();

    const ctxList = screen.getByRole("list", { name: "Kubeconfig contexts" });
    const eksItem = within(ctxList).getByText("eks-prod").closest("li");
    await waitFor(() => {
      expect(eksItem).toHaveStyle({ "--ctx": "#22aa88" });
    });
    expect(screen.getByTestId("badge-readonly-eks-prod")).toBeInTheDocument();
    expect([...settingsCalls].sort()).toEqual(["dev", "eks-prod", "sso"]);

    // Selecting a cached context does not ask again.
    const user = userEvent.setup();
    await user.click(within(ctxList).getByText("eks-prod"));
    expect(settingsCalls).toHaveLength(3);
  });

  it("renders context settings badges (RO, PROTECTED, tags, colour swatch) and app shell indicators", async () => {
    mockBackend(() => LIST);
    render(<App />);
    await act(() => useContextsStore.getState().loadContexts());

    act(() => {
      useContextsStore.getState().setContextSettings("dev", {
        colour: "#ff5500",
        tags: ["prod-eu", "critical"],
        readOnly: true,
        protected: true,
      });
    });

    // The status bar names the selected context too, so look inside the context list.
    const ctxList = screen.getByRole("list", { name: "Kubeconfig contexts" });
    const devItem = within(ctxList).getByText("dev").closest("li");
    expect(devItem).not.toBeNull();
    expect(devItem).toHaveStyle({ "--ctx": "#ff5500" });

    const roBadge = screen.getByTestId("badge-readonly-dev");
    expect(roBadge).toHaveTextContent("RO");
    expect(roBadge).toHaveAttribute("title", "Read-only context");

    const protBadge = screen.getByTestId("badge-protected-dev");
    expect(protBadge).toHaveTextContent("PROTECTED");
    expect(protBadge).toHaveAttribute("title", "Protected context (type name to confirm writes)");

    if (devItem) {
      expect(within(devItem).getByText("prod-eu")).toBeInTheDocument();
      expect(within(devItem).getByText("critical")).toBeInTheDocument();
    }

    // Since "dev" is the selectedContext (from current), App shell reflects its colour and readOnly banner.
    const shell = document.querySelector(".app-shell");
    expect(shell).toHaveStyle({ "--context-accent": "#ff5500" });
    expect(screen.getByTestId("active-context-readonly")).toBeInTheDocument();
  });

  it("reloads team settings, updates store state, and displays the team settings badge", async () => {
    mockBackend(() => LIST);
    await renderLoaded();

    expect(screen.queryByTestId("team-settings-badge")).toBeNull();
    expect(useContextsStore.getState().teamSettingsSummary).toBeNull();
    expect(useContextsStore.getState().teamSettingsStatus).toBe("idle");

    let success = false;
    await act(async () => {
      success = await useContextsStore.getState().reloadTeamSettings("team-settings.yaml");
    });

    expect(success).toBe(true);
    const summary = useContextsStore.getState().teamSettingsSummary;
    expect(summary).toEqual({
      loadedPath: "team-settings.yaml",
      appliedGroups: 2,
      appliedContexts: 4,
      warnings: ["ignored unknown field 'extra'"],
    });
    expect(useContextsStore.getState().teamSettingsStatus).toBe("ready");

    const badge = screen.getByTestId("team-settings-badge");
    expect(badge).toBeInTheDocument();
    expect(badge).toHaveTextContent("team: 2 groups (1 warning)");
  });

  it("reloads team settings when clicking the badge", async () => {
    const user = userEvent.setup();
    const calls = mockBackend(() => LIST);
    await renderLoaded();

    await act(async () => {
      await useContextsStore.getState().reloadTeamSettings("team-settings.yaml");
    });

    const badge = screen.getByTestId("team-settings-badge");
    expect(badge).toBeInTheDocument();

    await user.click(badge);
    expect(calls.filter((c) => c === "reload_team_settings")).toHaveLength(2);
  });

  it("reuses the existing tab for a kind instead of opening duplicates", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);

    await user.click(screen.getByTestId("nav-item-Deployment"));
    const deploymentsTab = useLayoutStore.getState().activeTabId;
    await user.click(screen.getByTestId("nav-item-Service"));
    await user.click(screen.getByTestId("nav-item-Deployment"));
    await user.click(screen.getByTestId("nav-item-Deployment"));

    const { tabs, activeTabId } = useLayoutStore.getState();
    expect(activeTabId).toBe(deploymentsTab);
    expect(tabs.filter((t) => t.kind === "Deployment")).toHaveLength(1);
    expect(tabs.filter((t) => t.kind === "Service")).toHaveLength(1);
  });

  it("renders grouped resource sections with the expected items", () => {
    render(<Sidebar />);

    const expectedGroups: Record<string, string[]> = {
      Workloads: ["Pods", "Deployments", "StatefulSets", "DaemonSets", "Jobs", "CronJobs"],
      Network: ["Services", "Ingresses"],
      Config: ["ConfigMaps", "Secrets"],
      Storage: ["PersistentVolumeClaims"],
      Cluster: ["Nodes", "Namespaces", "Events", "Helm releases"],
      Ecosystem: ["Gateways", "HTTPRoutes", "Argo Apps", "Flux Kustomize", "Karpenter Pools"],
    };

    for (const [groupName, items] of Object.entries(expectedGroups)) {
      const list = screen.getByRole("list", { name: groupName });
      expect(list).toBeInTheDocument();
      for (const item of items) {
        expect(within(list).getByRole("button", { name: new RegExp(`^${item}`) })).toBeInTheDocument();
      }
    }
  });

  it("opens a tab with the right kind for each new resource item", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);

    const newItems = [
      { label: "StatefulSets", kind: "StatefulSet" },
      { label: "DaemonSets", kind: "DaemonSet" },
      { label: "CronJobs", kind: "CronJob" },
      { label: "Ingresses", kind: "Ingress" },
      { label: "PersistentVolumeClaims", kind: "PersistentVolumeClaim" },
      { label: "Events", kind: "Event" },
    ];

    for (const item of newItems) {
      const btn = screen.getByRole("button", { name: item.label });
      expect(btn).toBeInTheDocument();
      await user.click(btn);

      const state = useLayoutStore.getState();
      const activeTab = state.tabs.find((t) => t.id === state.activeTabId);
      expect(activeTab).toBeDefined();
      expect(activeTab?.kind).toBe(item.kind);
      expect(activeTab?.title).toBe(item.label);
      expect(activeTab?.view).toBe("resources");
      expect(btn).toHaveAttribute("data-selected", "true");
    }
  });

  it("04-05a: renders Access Control group and opens tabs for RBAC kinds", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);

    const accessControlList = screen.getByRole("list", { name: "Access Control" });
    expect(accessControlList).toBeInTheDocument();

    const rbacItems = [
      { label: "ServiceAccounts", kind: "ServiceAccount" },
      { label: "Roles", kind: "Role" },
      { label: "ClusterRoles", kind: "ClusterRole" },
      { label: "RoleBindings", kind: "RoleBinding" },
      { label: "ClusterRoleBindings", kind: "ClusterRoleBinding" },
    ];

    for (const item of rbacItems) {
      const btn = within(accessControlList).getByRole("button", { name: item.label });
      expect(btn).toBeInTheDocument();
      await user.click(btn);

      const state = useLayoutStore.getState();
      const activeTab = state.tabs.find((t) => t.id === state.activeTabId);
      expect(activeTab).toBeDefined();
      expect(activeTab?.kind).toBe(item.kind);
      expect(activeTab?.title).toBe(item.label);
      expect(btn).toHaveAttribute("data-selected", "true");
    }
  });

  it("lists Overview first and reopens the overview tab after it was closed", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);
    const overviewBtn = screen.getByRole("button", { name: "Overview" });
    const navButtons = screen
      .getAllByRole("button")
      .filter((b) => b.dataset.testid?.startsWith("nav-item-"));
    expect(navButtons[0]).toBe(overviewBtn);
    expect(overviewBtn).not.toHaveAttribute("data-selected");

    await user.click(overviewBtn);
    expect(useLayoutStore.getState().activeTabId).toBe("overview");
    expect(useLayoutStore.getState().tabs.find((t) => t.id === "overview")?.view).toBe("overview");
    expect(overviewBtn).toHaveAttribute("data-selected", "true");

    act(() => {
      useLayoutStore.getState().closeTab("overview");
    });
    expect(overviewBtn).not.toHaveAttribute("data-selected");
    await user.click(overviewBtn);
    const state = useLayoutStore.getState();
    expect(state.tabs.filter((t) => t.view === "overview")).toHaveLength(1);
    expect(state.activeTabId).toBe("overview");
  });

  it("opens Helm releases tab with id view-helm and reuses it on subsequent clicks", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);

    const helmBtn = screen.getByRole("button", { name: "Helm releases" });
    expect(helmBtn).toBeInTheDocument();
    await user.click(helmBtn);

    let state = useLayoutStore.getState();
    expect(state.activeTabId).toBe("view-helm");
    const activeTab = state.tabs.find((t) => t.id === "view-helm");
    expect(activeTab).toBeDefined();
    expect(activeTab?.view).toBe("helm");
    expect(activeTab?.title).toBe("Helm releases");
    expect(helmBtn).toHaveAttribute("data-selected", "true");

    // Click another kind (e.g. Pods), then click Helm releases again
    await user.click(screen.getByRole("button", { name: "Pods" }));
    state = useLayoutStore.getState();
    expect(state.activeTabId).not.toBe("view-helm");
    expect(helmBtn).not.toHaveAttribute("data-selected");

    await user.click(helmBtn);
    state = useLayoutStore.getState();
    expect(state.activeTabId).toBe("view-helm");
    expect(state.tabs.filter((t) => t.id === "view-helm")).toHaveLength(1);
    expect(helmBtn).toHaveAttribute("data-selected", "true");
  });

  it("opens an ecosystem tab when clicking an ecosystem nav item and highlights active item", async () => {
    const user = userEvent.setup();
    render(<Sidebar />);

    const ecoItems = [
      { label: "Gateways", kind: "Gateway", badge: "Gateway API" },
      { label: "HTTPRoutes", kind: "HTTPRoute", badge: "Gateway API" },
      { label: "Argo Apps", kind: "Application", badge: "Argo CD" },
      { label: "Flux Kustomize", kind: "Kustomization", badge: "Flux" },
      { label: "Karpenter Pools", kind: "NodePool", badge: "Karpenter" },
    ];

    const ecoSection = screen.getByRole("list", { name: "Ecosystem" });
    expect(ecoSection).toBeInTheDocument();

    for (const item of ecoItems) {
      const btn = within(ecoSection).getByRole("button", { name: new RegExp(item.label, "i") });
      expect(btn).toBeInTheDocument();
      expect(within(btn).getByText(item.badge)).toBeInTheDocument();

      await user.click(btn);

      const activeTab = useLayoutStore
        .getState()
        .tabs.find((t) => t.id === useLayoutStore.getState().activeTabId);
      expect(activeTab).toBeDefined();
      expect(activeTab?.title).toBe(item.label);
      expect(activeTab?.kind).toBe(item.kind);
      expect(activeTab?.view).toBe("resources");
      expect(btn).toHaveAttribute("data-selected", "true");
    }
  });

  it("supports multi-context selection via Ctrl+click or Cmd+click", async () => {
    mockBackend(() => LIST);
    const user = userEvent.setup();
    await renderLoaded();

    const devItem = screen.getByText("dev").closest("li");
    const eksItem = screen.getByText("eks-prod").closest("li");
    const ssoItem = screen.getByText("sso").closest("li");
    expect(devItem).not.toBeNull();
    expect(eksItem).not.toBeNull();
    expect(ssoItem).not.toBeNull();
    if (!devItem || !eksItem || !ssoItem) throw new Error("Elements missing");

    // Initial state: dev is selected
    expect(devItem).toHaveAttribute("data-selected", "true");
    expect(eksItem).not.toHaveAttribute("data-selected");
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);

    // Ctrl+click eks-prod toggles it into multi-selection alongside dev
    await user.keyboard("{Control>}");
    await user.click(eksItem);
    await user.keyboard("{/Control}");

    expect(useContextsStore.getState().activeClusterSelection).toEqual(["dev", "eks-prod"]);
    expect(devItem).toHaveAttribute("data-selected", "true");
    expect(eksItem).toHaveAttribute("data-selected", "true");
    expect(ssoItem).not.toHaveAttribute("data-selected");

    // Ctrl+click sso adds third cluster
    await user.keyboard("{Control>}");
    await user.click(ssoItem);
    await user.keyboard("{/Control}");

    expect(useContextsStore.getState().activeClusterSelection).toEqual(["dev", "eks-prod", "sso"]);
    expect(ssoItem).toHaveAttribute("data-selected", "true");

    // Ctrl+click dev removes it from multi-selection
    await user.keyboard("{Control>}");
    await user.click(devItem);
    await user.keyboard("{/Control}");

    expect(useContextsStore.getState().activeClusterSelection).toEqual(["eks-prod", "sso"]);
    expect(devItem).not.toHaveAttribute("data-selected");
    expect(eksItem).toHaveAttribute("data-selected", "true");
    expect(ssoItem).toHaveAttribute("data-selected", "true");

    // Regular click on dev resets multi-selection and picks dev as single context
    await user.click(devItem);
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);
    expect(useContextsStore.getState().selectedContext).toBe("dev");
    expect(devItem).toHaveAttribute("data-selected", "true");
    expect(eksItem).not.toHaveAttribute("data-selected");
  });

  it("supports multi-context selection via ⌘+click (Cmd+click) and clears on single context click", async () => {
    mockBackend(() => LIST);
    const user = userEvent.setup();
    await renderLoaded();

    const devItem = screen.getByText("dev").closest("li");
    const eksItem = screen.getByText("eks-prod").closest("li");
    expect(devItem).not.toBeNull();
    expect(eksItem).not.toBeNull();
    if (!devItem || !eksItem) throw new Error("Elements missing");

    // Cmd+click eks-prod
    await user.keyboard("{Meta>}");
    await user.click(eksItem);
    await user.keyboard("{/Meta}");

    expect(useContextsStore.getState().activeClusterSelection).toEqual(["dev", "eks-prod"]);
    expect(devItem).toHaveAttribute("data-selected", "true");
    expect(eksItem).toHaveAttribute("data-selected", "true");

    // Cmd+click dev toggles it out, leaving 1 item in activeClusterSelection
    await user.keyboard("{Meta>}");
    await user.click(devItem);
    await user.keyboard("{/Meta}");

    expect(useContextsStore.getState().activeClusterSelection).toEqual(["eks-prod"]);
    expect(devItem).not.toHaveAttribute("data-selected");
    expect(eksItem).toHaveAttribute("data-selected", "true");

    // Cmd+click eks-prod toggles it out, leaving 0 items
    await user.keyboard("{Meta>}");
    await user.click(eksItem);
    await user.keyboard("{/Meta}");

    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);
    expect(useContextsStore.getState().selectedContext).toBe("dev");
  });

  it("renders Cluster column with colour-coded badge in PodTable when multi-cluster active", async () => {
    resetPodsStore();
    useContextsStore.setState({
      activeClusterSelection: ["dev", "eks-prod"],
      settingsByContext: {
        dev: { colour: "#3b82f6", tags: [], readOnly: false, protected: false, pinned: false },
        "eks-prod": { colour: "#10b981", tags: [], readOnly: false, protected: false, pinned: false },
      },
    });

    const fakeRow: Row = {
      uid: "uid-web-0",
      cluster: "dev",
      kind: "Pod",
      namespace: "default",
      name: "web-0",
      status: "healthy",
      statusReason: "Running",
      cells: [
        { type: "ready", value: { ready: 1, total: 1 } },
        { type: "status", value: { label: "Running", phase: "healthy" } },
        { type: "int", value: 0 },
        { type: "timestamp", value: "" },
        { type: "text", value: "" },
        { type: "text", value: "node-0" },
        { type: "text", value: "10.244.0.1" },
        { type: "timestamp", value: "2026-01-01T00:00:00Z" },
        { type: "usage", value: { value: 3, formatted: "3m", request: 100, limit: null } },
        { type: "text", value: "" },
      ],
      labels: [],
      custom: [],
    };

    setQueryFetcher((spec) =>
      Promise.resolve({
        total: 1,
        offset: spec.offset,
        limit: spec.limit,
        rows: [fakeRow],
        version: 1,
        access: [{ cluster: "dev", kind: "Pod", access: { state: "ready" }, rows: 1, metrics: null }],
        statusCounts: { healthy: 1, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
      }),
    );

    render(<PodTable />);
    expect(await screen.findByText("web-0")).toBeInTheDocument();

    // Cluster column header is visible and pinned first
    const headers = screen.getAllByRole("columnheader");
    expect(headers[0]).toHaveTextContent("Cluster");

    // Badge is rendered with colour style
    const badge = screen.getByText("dev");
    expect(badge).toHaveClass("cluster-badge");
    expect(badge).toHaveStyle({ "--ctx": "#3b82f6" });

    // Cluster column cannot be hidden via column picker
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /^Columns/ }));
    const menu = screen.getByRole("menu", { name: "Columns" });
    expect(within(menu).queryByRole("menuitemcheckbox", { name: "Cluster" })).toBeNull();
  });
});
