import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PodTable } from "./PodTable";
import { SplitPaneContainer } from "./SplitPaneContainer";
import { TabsArea } from "./TabsArea";
import { useActionsStore } from "../stores/actions";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import {
  resetPodsStore,
  setQueryFetcher,
  usePodsStore,
  useSecondaryPodsStore,
  type Page,
  type QueryFetcher,
  type Row,
} from "../stores/pods";
import { resetResourceDetailStore, useResourceDetailStore } from "../stores/resourceDetail";
import { hiddenColumnsOf, useTableColumnsStore } from "../stores/tableColumns";
import { podsLayoutState } from "../test/layout";

const SAMPLE_POD_ROW_A: Row = {
  uid: "uid-pod-a",
  cluster: "kind-a",
  kind: "Pod",
  namespace: "default",
  name: "pod-in-cluster-a",
  status: "healthy",
  statusReason: "Running",
  cells: [
    { type: "text", value: "pod-in-cluster-a" },
    { type: "text", value: "1/1" },
    { type: "status", value: { label: "Running", phase: "healthy" } },
  ],
  labels: [],
  custom: [],
};

const SAMPLE_POD_ROW_B: Row = {
  uid: "uid-pod-b",
  cluster: "kind-b",
  kind: "Pod",
  namespace: "production",
  name: "pod-in-cluster-b",
  status: "healthy",
  statusReason: "Running",
  cells: [
    { type: "text", value: "pod-in-cluster-b" },
    { type: "text", value: "2/2" },
    { type: "status", value: { label: "Running", phase: "healthy" } },
  ],
  labels: [],
  custom: [],
};

function fakePage(rows: Row[]): Page {
  return {
    rows,
    total: rows.length,
    offset: 0,
    limit: 100,
    version: 1,
    access: [],
    statusCounts: {
      healthy: rows.length,
      pending: 0,
      degraded: 0,
      failed: 0,
      completed: 0,
      terminating: 0,
      unknown: 0,
    },
  };
}

describe("05-03 Split view & multi-pane layout manager", () => {
  beforeEach(() => {
    useContextsStore.setState({
      ...initialContextsState,
      contexts: [
        {
          name: "kind-a",
          cluster: "kind-a",
          user: "u",
          namespace: null,
          server: "https://127.0.0.1:7443",
          auth: { type: "token" },
          isCurrent: true,
        },
        {
          name: "kind-b",
          cluster: "kind-b",
          user: "u",
          namespace: null,
          server: "https://127.0.0.1:7443",
          auth: { type: "token" },
          isCurrent: false,
        },
      ],
      selectedContext: "kind-a",
    });
    useLayoutStore.setState({
      ...podsLayoutState,
      tabs: [
        { id: "tab-1", title: "Pods A", view: "resources", kind: "Pod", cluster: "kind-a" },
        { id: "tab-2", title: "Pods B", view: "resources", kind: "Pod", cluster: "kind-b" },
      ],
      activeTabId: "tab-1",
      splitMode: "none",
      activePane: "primary",
    });
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockIPC((cmd) => {
      if (cmd === "get_resource_detail") {
        return {
          yaml: "apiVersion: v1\nkind: Pod\nmetadata:\n  name: pod-in-cluster-a\n",
          events: [],
        };
      }
      if (cmd === "start_workload_logs") return 1;
      if (cmd === "stop_workload_logs") return true;
      if (cmd === "query_page") {
        return fakePage([SAMPLE_POD_ROW_A]);
      }
      if (cmd === "subscribe_query") return 1;
      if (cmd === "unsubscribe_query") return true;
      return undefined;
    });
    useActionsStore.getState().setSelection(null);
    resetResourceDetailStore();
    resetPodsStore();
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
    useActionsStore.getState().setSelection(null);
    resetResourceDetailStore();
    resetPodsStore();
    vi.restoreAllMocks();
  });

  describe("Interactive splitter resizing & keyboard navigation", () => {
    it("resizes with mouse drag and clamps ratio between 10% and 90%", () => {
      let currentRatio = 50;
      render(
        <SplitPaneContainer
          splitMode="horizontal"
          ratio={currentRatio}
          onRatioChange={(r) => {
            currentRatio = r;
          }}
          primary={<div data-testid="prim">Primary</div>}
          secondary={<div data-testid="sec">Secondary</div>}
        />,
      );

      const divider = screen.getByRole("separator", { name: /split pane resize handle/i });
      expect(divider).toHaveAttribute("aria-valuenow", "50");

      const container = screen.getByTestId("split-pane-container");
      vi.spyOn(container, "getBoundingClientRect").mockReturnValue({
        left: 0,
        top: 0,
        width: 1000,
        height: 600,
        right: 1000,
        bottom: 600,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      });

      // Mouse drag to 700px (70%)
      fireEvent.mouseDown(divider, { clientX: 500, clientY: 300 });
      fireEvent.mouseMove(window, { clientX: 700, clientY: 300 });
      fireEvent.mouseUp(window);

      expect(currentRatio).toBe(70);

      // Mouse drag beyond 90% (clamped to 90)
      fireEvent.mouseDown(divider, { clientX: 700, clientY: 300 });
      fireEvent.mouseMove(window, { clientX: 980, clientY: 300 });
      fireEvent.mouseUp(window);

      expect(currentRatio).toBe(90);

      // Mouse drag below 10% (clamped to 10)
      fireEvent.mouseDown(divider, { clientX: 900, clientY: 300 });
      fireEvent.mouseMove(window, { clientX: 50, clientY: 300 });
      fireEvent.mouseUp(window);

      expect(currentRatio).toBe(10);
    });

    it("resizes with keyboard on the separator (Arrow keys, Home, End, Enter)", () => {
      let currentRatio = 50;
      const { rerender } = render(
        <SplitPaneContainer
          splitMode="horizontal"
          ratio={currentRatio}
          onRatioChange={(r) => {
            currentRatio = r;
          }}
          primary={<div>Left</div>}
          secondary={<div>Right</div>}
        />,
      );

      const divider = screen.getByRole("separator", { name: /split pane resize handle/i });

      // ArrowRight increases by 5%
      fireEvent.keyDown(divider, { key: "ArrowRight" });
      expect(currentRatio).toBe(55);

      rerender(
        <SplitPaneContainer
          splitMode="horizontal"
          ratio={currentRatio}
          onRatioChange={(r) => {
            currentRatio = r;
          }}
          primary={<div>Left</div>}
          secondary={<div>Right</div>}
        />,
      );

      // ArrowLeft decreases by 5%
      fireEvent.keyDown(divider, { key: "ArrowLeft" });
      expect(currentRatio).toBe(50);

      // End sets ratio to 90%
      fireEvent.keyDown(divider, { key: "End" });
      expect(currentRatio).toBe(90);

      // Home sets ratio to 10%
      fireEvent.keyDown(divider, { key: "Home" });
      expect(currentRatio).toBe(10);

      // Enter resets ratio to 50%
      fireEvent.keyDown(divider, { key: "Enter" });
      expect(currentRatio).toBe(50);
    });

    it("toggles splitter controls: Side-by-Side, Stacked, 50/50, and Close", async () => {
      const user = userEvent.setup();
      render(<TabsArea />);

      // Initially no split
      expect(screen.queryByTestId("split-pane-secondary")).not.toBeInTheDocument();

      // Open horizontal split
      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("resources");
      });

      expect(screen.getByTestId("split-pane-secondary")).toBeInTheDocument();
      expect(screen.getByTestId("split-pane-container")).toHaveAttribute("data-split-mode", "horizontal");

      // Switch to Stacked
      const stackedBtn = screen.getByRole("button", { name: "Split vertically" });
      await user.click(stackedBtn);
      expect(useLayoutStore.getState().splitMode).toBe("vertical");
      expect(screen.getByTestId("split-pane-container")).toHaveAttribute("data-split-mode", "vertical");

      // Switch back to Side-by-Side
      const sideBtn = screen.getByRole("button", { name: "Split horizontally" });
      await user.click(sideBtn);
      expect(useLayoutStore.getState().splitMode).toBe("horizontal");

      // Close split
      const closeBtn = screen.getByRole("button", { name: "Close split" });
      await user.click(closeBtn);
      expect(useLayoutStore.getState().splitMode).toBe("none");
      expect(screen.queryByTestId("split-pane-secondary")).not.toBeInTheDocument();
    });
  });

  describe("Keyboard navigation & Ctrl+W pane switching", () => {
    it("switches pane focus with Ctrl+W when split mode is active", () => {
      render(<TabsArea />);

      // Open split
      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("resources");
      });

      expect(useLayoutStore.getState().activePane).toBe("primary");
      const primaryPane = screen.getByTestId("split-pane-primary");
      const secondaryPane = screen.getByTestId("split-pane-secondary");

      expect(primaryPane).toHaveAttribute("data-pane-active", "true");
      expect(secondaryPane).toHaveAttribute("data-pane-active", "false");

      // Press Ctrl+W to switch to secondary pane
      fireEvent.keyDown(window, { key: "w", ctrlKey: true });

      expect(useLayoutStore.getState().activePane).toBe("secondary");
      expect(primaryPane).toHaveAttribute("data-pane-active", "false");
      expect(secondaryPane).toHaveAttribute("data-pane-active", "true");

      // Press Ctrl+W again to switch back to primary pane
      fireEvent.keyDown(window, { key: "w", ctrlKey: true });

      expect(useLayoutStore.getState().activePane).toBe("primary");
      expect(primaryPane).toHaveAttribute("data-pane-active", "true");
      expect(secondaryPane).toHaveAttribute("data-pane-active", "false");
    });

    it("closes the active tab with Ctrl+W when split mode is none", () => {
      render(<TabsArea />);

      expect(useLayoutStore.getState().splitMode).toBe("none");
      expect(useLayoutStore.getState().tabs).toHaveLength(2);
      expect(useLayoutStore.getState().activeTabId).toBe("tab-1");

      // Press Ctrl+W in single-pane mode closes tab-1
      fireEvent.keyDown(window, { key: "w", ctrlKey: true });

      expect(useLayoutStore.getState().tabs).toHaveLength(1);
      expect(useLayoutStore.getState().activeTabId).toBe("tab-2");
    });

    it("switches active pane when clicking or focusing inside a pane", () => {
      render(<TabsArea />);

      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("resources");
      });

      const secondaryPane = screen.getByTestId("split-pane-secondary");
      fireEvent.mouseDown(secondaryPane);
      expect(useLayoutStore.getState().activePane).toBe("secondary");

      const primaryPane = screen.getByTestId("split-pane-primary");
      fireEvent.mouseDown(primaryPane);
      expect(useLayoutStore.getState().activePane).toBe("primary");
    });
  });

  describe("Tab dragging & reordering", () => {
    it("reorders tabs when dragged and dropped within tab bar", () => {
      render(<TabsArea />);

      const tab1 = screen.getByRole("tab", { name: "Pods A" }).parentElement;
      const tab2 = screen.getByRole("tab", { name: "Pods B" }).parentElement;

      expect(tab1).not.toBeNull();
      expect(tab2).not.toBeNull();
      if (!tab1 || !tab2) return;

      expect(tab1).toHaveAttribute("draggable", "true");
      expect(tab2).toHaveAttribute("draggable", "true");

      // Drag tab-1 onto tab-2
      const dataTransfer = {
        data: {} as Record<string, string>,
        setData(format: string, data: string) {
          this.data[format] = data;
        },
        getData(format: string) {
          return this.data[format] ?? "";
        },
        effectAllowed: "move",
        dropEffect: "none",
      };

      fireEvent.dragStart(tab1, { dataTransfer });
      fireEvent.dragOver(tab2, { dataTransfer });
      fireEvent.drop(tab2, { dataTransfer });

      // Verify order changed in store: tab-2 is first, tab-1 is second
      const tabs = useLayoutStore.getState().tabs;
      expect(tabs[0]?.id).toBe("tab-2");
      expect(tabs[1]?.id).toBe("tab-1");
    });

    it("drops a tab onto secondary pane to open that view in the secondary split", () => {
      render(<TabsArea />);

      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("apps");
      });

      const secondaryPane = screen.getByTestId("secondary-pane");
      expect(within(secondaryPane).getByLabelText("Secondary pane view")).toHaveValue("apps");

      const tab2 = screen.getByRole("tab", { name: "Pods B" }).parentElement;
      expect(tab2).not.toBeNull();
      if (!tab2) return;

      const dataTransfer = {
        data: {
          "text/plain": "tab-2",
          "application/x-fleetglass-tab": "tab-2",
        },
        setData() {},
        getData(format: string) {
          return this.data[format as keyof typeof this.data];
        },
        dropEffect: "none",
      };

      fireEvent.dragStart(tab2, { dataTransfer });
      fireEvent.dragOver(secondaryPane, { dataTransfer });
      expect(secondaryPane).toHaveAttribute("data-dragover", "true");

      fireEvent.drop(secondaryPane, { dataTransfer });

      // Secondary view adopted tab-2's view and cluster
      expect(useLayoutStore.getState().secondaryView).toBe("resources");
      expect(useLayoutStore.getState().secondaryCluster).toBe("kind-b");
    });
  });

  describe("Side-by-side independent tables & query states", () => {
    it("renders Pods alongside Workload Logs simultaneously", async () => {
      render(<TabsArea />);

      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("logs");
      });

      // Primary pane displays PodTable
      const primary = screen.getByTestId("split-pane-primary");
      expect(within(primary).getByLabelText("Filter pods")).toBeInTheDocument();

      // Secondary pane shows empty prompt before selection
      const secondary = screen.getByTestId("split-pane-secondary");
      expect(within(secondary).getByTestId("secondary-logs-empty")).toHaveTextContent(
        "Select a pod or workload to view logs",
      );
      expect(within(secondary).getByLabelText("Secondary pane view")).toHaveValue("logs");

      // Select a pod -> logs view starts
      act(() => {
        useActionsStore.getState().setSelection(SAMPLE_POD_ROW_A);
      });
      expect(await within(secondary).findByTestId("workload-logs-status")).toBeInTheDocument();
    });

    it("renders Pod list alongside YAML Editor simultaneously", async () => {
      render(<TabsArea />);

      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("yaml");
      });

      // Primary pane displays PodTable
      const primary = screen.getByTestId("split-pane-primary");
      expect(within(primary).getByLabelText("Filter pods")).toBeInTheDocument();

      // Secondary pane shows empty prompt before selection
      const secondary = screen.getByTestId("split-pane-secondary");
      expect(within(secondary).getByTestId("secondary-yaml-empty")).toHaveTextContent(
        "Select a resource to view or edit YAML",
      );

      // Select a resource -> real YAML loads
      act(() => {
        useActionsStore.getState().setSelection(SAMPLE_POD_ROW_A);
      });
      expect(await within(secondary).findByTestId("secondary-yaml-editor")).toBeInTheDocument();
      expect(within(secondary).getByLabelText("Resource YAML")).toBeInTheDocument();
      expect(within(secondary).getByTestId("yaml-apply-btn")).toBeInTheDocument();
    });

    it("renders two different cluster views with independent query states", async () => {
      // Injected query fetchers for primary and secondary stores
      const fetcherA: QueryFetcher = () => Promise.resolve(fakePage([SAMPLE_POD_ROW_A]));
      const fetcherB: QueryFetcher = () => Promise.resolve(fakePage([SAMPLE_POD_ROW_B]));

      setQueryFetcher(fetcherA, usePodsStore);
      setQueryFetcher(fetcherB, useSecondaryPodsStore);

      render(
        <div style={{ height: "600px", width: "1000px" }}>
          <SplitPaneContainer
            splitMode="horizontal"
            primary={<PodTable paneId="primary" kind="Pod" clusters={["kind-a"]} />}
            secondary={<PodTable paneId="secondary" kind="Pod" clusters={["kind-b"]} />}
          />
        </div>,
      );

      // Verify primary fetched kind-a
      await act(async () => {
        await Promise.resolve();
      });

      expect(usePodsStore.getState().querySpec.clusters).toEqual(["kind-a"]);
      expect(useSecondaryPodsStore.getState().querySpec.clusters).toEqual(["kind-b"]);

      // Verify row text rendered independently in each pane
      const primary = screen.getByTestId("split-pane-primary");
      const secondary = screen.getByTestId("split-pane-secondary");

      expect(within(primary).getAllByText("pod-in-cluster-a")[0]).toBeInTheDocument();
      expect(within(secondary).getAllByText("pod-in-cluster-b")[0]).toBeInTheDocument();

      // Update text filter on secondary pane only
      await act(async () => {
        await useSecondaryPodsStore.getState().setTextFilter("cluster-b");
      });

      expect(usePodsStore.getState().querySpec.textFilter).toBeNull();
      expect(useSecondaryPodsStore.getState().querySpec.textFilter).toBe("cluster-b");
    });

    it("maintains independent column configurations per pane", () => {
      const store = useTableColumnsStore.getState();

      // Hide "IP" column on primary pane
      store.toggleColumn("Pod", "IP", "primary");

      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "primary")).toEqual(["IP"]);
      // Secondary pane still shows all columns
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "secondary")).toEqual([]);

      // Hide "Node" column on secondary pane
      store.toggleColumn("Pod", "Node", "secondary");

      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "primary")).toEqual(["IP"]);
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "secondary")).toEqual(["Node"]);

      // Reset primary pane columns
      store.showAll("Pod", "primary");
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "primary")).toEqual([]);
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "secondary")).toEqual(["Node"]);
    });

    it("binds useListStatus to secondary podsStore so secondary table is not hidden when primary has no queried clusters", async () => {
      useLayoutStore.setState({
        ...podsLayoutState,
        tabs: [{ id: "tab-ov", title: "Overview", view: "overview" }],
        activeTabId: "tab-ov",
        splitMode: "horizontal",
        secondaryView: "resources",
        secondaryKind: "Pod",
        secondaryCluster: "kind-b",
      });

      const fetcherB: QueryFetcher = () => Promise.resolve(fakePage([SAMPLE_POD_ROW_B]));
      setQueryFetcher(fetcherB, useSecondaryPodsStore);

      render(<TabsArea />);

      await act(async () => {
        await Promise.resolve();
      });

      const secondary = screen.getByTestId("split-pane-secondary");
      const table = within(secondary).getByRole("table", { name: "Pods" });
      expect(table).not.toHaveAttribute("hidden");
      expect(within(table).getAllByText("pod-in-cluster-b")[0]).toBeInTheDocument();
    });

    it("persists text and namespace filter changes in secondary pane without mutating primary tab state", async () => {
      const fetcherB: QueryFetcher = () => Promise.resolve(fakePage([SAMPLE_POD_ROW_B]));
      setQueryFetcher(fetcherB, useSecondaryPodsStore);

      render(<TabsArea />);

      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("resources");
      });

      await act(async () => {
        await Promise.resolve();
      });

      const secondary = screen.getByTestId("split-pane-secondary");
      const searchInput = within(secondary).getByLabelText("Filter pods");

      fireEvent.change(searchInput, { target: { value: "my-filter" } });

      expect(useLayoutStore.getState().secondaryTextFilter).toBe("my-filter");
      const primaryTab = useLayoutStore.getState().tabs.find((t) => t.id === "tab-1");
      expect(primaryTab?.textFilter).toBeUndefined();

      const nsInput = within(secondary).getByLabelText("Namespace filter");
      fireEvent.change(nsInput, { target: { value: "kube-system" } });

      expect(useLayoutStore.getState().secondaryNamespace).toBe("kube-system");
      expect(primaryTab?.namespace).toBeUndefined();
    });

    it("scopes column picker toggles to secondary pane and preserves showAll without reverting to unscoped", () => {
      const store = useTableColumnsStore.getState();
      store.toggleColumn("Pod", "IP");
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod")).toEqual(["IP"]);

      store.showAll("Pod", "secondary");
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "secondary")).toEqual([]);
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod")).toEqual(["IP"]);

      store.toggleColumn("Pod", "Node", "secondary");
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "secondary")).toEqual(["Node"]);
      store.toggleColumn("Pod", "Node", "secondary");
      expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "secondary")).toEqual([]);
    });

    it("drops a StatefulSet tab onto secondary pane, transferring cluster, textFilter, namespace, and selecting StatefulSet", () => {
      useLayoutStore.setState({
        ...podsLayoutState,
        tabs: [
          { id: "tab-1", title: "Pods A", view: "resources", kind: "Pod", cluster: "kind-a" },
          {
            id: "tab-sts",
            title: "StatefulSets",
            view: "resources",
            kind: "StatefulSet",
            cluster: null,
            textFilter: "redis",
            namespace: "database",
          },
        ],
        activeTabId: "tab-1",
        splitMode: "horizontal",
        secondaryView: "apps",
        secondaryCluster: "stale-cluster",
      });

      render(<TabsArea />);

      const secondaryPane = screen.getByTestId("secondary-pane");
      const tabSts = screen.getByRole("tab", { name: "StatefulSets" }).parentElement;
      expect(tabSts).not.toBeNull();
      if (!tabSts) return;

      const dataTransfer = {
        data: {
          "text/plain": "tab-sts",
          "application/x-fleetglass-tab": "tab-sts",
        },
        setData() {},
        getData(format: string) {
          return this.data[format as keyof typeof this.data];
        },
        dropEffect: "none",
      };

      fireEvent.dragStart(tabSts, { dataTransfer });
      fireEvent.dragOver(secondaryPane, { dataTransfer });
      fireEvent.drop(secondaryPane, { dataTransfer });

      expect(useLayoutStore.getState().secondaryView).toBe("resources");
      expect(useLayoutStore.getState().secondaryKind).toBe("StatefulSet");
      expect(useLayoutStore.getState().secondaryCluster).toBeNull();
      expect(useLayoutStore.getState().secondaryTextFilter).toBe("redis");
      expect(useLayoutStore.getState().secondaryNamespace).toBe("database");

      const kindSelect = within(secondaryPane).getByLabelText("Secondary pane kind");
      expect(kindSelect).toHaveValue("StatefulSet");
    });

    it("Ctrl+W focuses inner .pod-table-container so j/k row navigation works immediately", async () => {
      const fetcherA: QueryFetcher = () => Promise.resolve(fakePage([SAMPLE_POD_ROW_A]));
      const fetcherB: QueryFetcher = () => Promise.resolve(fakePage([SAMPLE_POD_ROW_B]));

      setQueryFetcher(fetcherA, usePodsStore);
      setQueryFetcher(fetcherB, useSecondaryPodsStore);

      render(<TabsArea />);

      act(() => {
        useLayoutStore.getState().setSplitMode("horizontal");
        useLayoutStore.getState().setSecondaryView("resources");
      });

      await act(async () => {
        await Promise.resolve();
      });

      expect(useLayoutStore.getState().activePane).toBe("primary");

      fireEvent.keyDown(window, { key: "w", ctrlKey: true });

      expect(useLayoutStore.getState().activePane).toBe("secondary");

      const secondaryContainer = document.querySelector('.pod-table-container[data-pane-id="secondary"]');
      expect(secondaryContainer).toBeInTheDocument();
      expect(document.activeElement).toBe(secondaryContainer);

      if (secondaryContainer) {
        fireEvent.keyDown(secondaryContainer, { key: "j" });
      }
      expect(useLayoutStore.getState().activePane).toBe("secondary");
    });

    it("scopes resource drawer ownership to opening pane and prevents secondary unmount from closing primary drawer", async () => {
      render(
        <div style={{ height: "600px", width: "1000px" }}>
          <SplitPaneContainer
            splitMode="horizontal"
            primary={<PodTable paneId="primary" kind="Pod" clusters={["kind-a"]} />}
            secondary={<PodTable paneId="secondary" kind="Pod" clusters={["kind-b"]} />}
          />
        </div>,
      );

      await act(async () => {
        await useResourceDetailStore.getState().open(SAMPLE_POD_ROW_A, "primary");
      });

      expect(useResourceDetailStore.getState().ownerPane).toBe("primary");
      const primary = screen.getByTestId("split-pane-primary");
      const secondary = screen.getByTestId("split-pane-secondary");

      expect(within(primary).getByRole("region", { name: "Resource details" })).toBeInTheDocument();
      expect(within(secondary).queryByRole("region", { name: "Resource details" })).toBeNull();
    });
  });
});
