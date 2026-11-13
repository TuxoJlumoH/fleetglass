import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import type { AppPage, AppQuerySpec_Deserialize } from "../generated/bindings";
import {
  type ClusterGroupClient,
  initialContextsState,
  setClusterGroupClient,
  setConnectHandler,
  useContextsStore,
} from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { podsLayoutState } from "../test/layout";
import { resetPodsStore } from "../stores/pods";

const PAGE: AppPage = {
  access: [],
  total: 1,
  offset: 0,
  apps: [
    {
      name: "shop",
      namespace: "default",
      cluster: "kind-a",
      components: [
        {
          cluster: "kind-a",
          namespace: "default",
          kind: "Deployment",
          name: "shop-web",
          status: "healthy",
          statusReason: "Available",
        },
      ],
    },
  ],
};

function fakeFetchApps() {
  return vi.fn<(spec: AppQuerySpec_Deserialize) => Promise<AppPage>>(() => Promise.resolve(PAGE));
}

function fakeGroupClient(): ClusterGroupClient {
  return {
    getClusterGroups: vi.fn(() =>
      Promise.resolve({ status: "ok" as const, data: [{ name: "Prod", contexts: ["kind-a"] }] }),
    ),
    setClusterGroup: vi.fn(() => Promise.resolve({ status: "ok" as const, data: null })),
    deleteClusterGroup: vi.fn(() => Promise.resolve({ status: "ok" as const, data: null })),
  };
}

async function runPaletteAction(user: ReturnType<typeof userEvent.setup>, name: string) {
  await user.keyboard("{Control>}k{/Control}");
  await user.click(screen.getByRole("option", { name }));
  expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
}

describe("App shell views and split panes", () => {
  beforeEach(() => {
    useLayoutStore.setState(podsLayoutState);
    useContextsStore.setState({ ...initialContextsState, activeClusterSelection: ["kind-a"] });
    setConnectHandler();
    resetPodsStore();
  });

  afterEach(() => {
    setClusterGroupClient();
    resetPodsStore();
  });

  it("opens an Apps tab from the palette and renders AppView for the active cluster selection", async () => {
    const user = userEvent.setup();
    useContextsStore.setState({ activeClusterSelection: ["kind-a"] });
    const fetchApps = fakeFetchApps();
    render(<App fetchApps={fetchApps} />);

    await runPaletteAction(user, "Open Apps view");

    expect(screen.getByRole("tab", { name: "Applications" })).toHaveAttribute("aria-selected", "true");
    const panel = screen.getByRole("tabpanel");
    expect(await within(panel).findByTestId("app-card-shop")).toBeInTheDocument();
    expect(fetchApps).toHaveBeenCalledWith(
      expect.objectContaining({ clusters: ["kind-a"], namespaces: [], offset: 0 }),
    );

    // Re-running the action focuses the same tab rather than adding a duplicate.
    await runPaletteAction(user, "Open Apps view");
    expect(screen.getAllByRole("tab", { name: "Applications" })).toHaveLength(1);
  });

  it("opens a Helm tab from the palette and shows the HelmView", async () => {
    const user = userEvent.setup();
    render(<App fetchApps={fakeFetchApps()} />);

    await runPaletteAction(user, "Open Helm releases");

    expect(screen.getByRole("tab", { name: "Helm releases" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByTestId("helm-view")).toBeInTheDocument();
    expect(screen.getByRole("table", { name: "Helm Releases List" })).toBeInTheDocument();
  });

  it("Ctrl+\\ shows two panes with an independent secondary view, and collapses them again", async () => {
    const user = userEvent.setup();
    const fetchApps = fakeFetchApps();
    render(<App fetchApps={fetchApps} />);

    expect(screen.queryByTestId("split-pane-secondary")).toBeNull();
    // Primary still shows the initial Pods resources tab.
    expect(screen.getByRole("tabpanel")).toHaveAttribute("data-view", "resources");

    fireEvent.keyDown(document.body, { key: "\\", code: "Backslash", ctrlKey: true });
    expect(useLayoutStore.getState().splitMode).toBe("horizontal");
    expect(screen.getByTestId("split-pane-primary")).toBeInTheDocument();
    const secondary = screen.getByTestId("split-pane-secondary");
    expect(within(secondary).getByLabelText("Secondary pane view")).toHaveValue("apps");
    expect(await within(secondary).findByTestId("app-card-shop")).toBeInTheDocument();
    expect(screen.getByRole("tabpanel")).toHaveAttribute("data-view", "resources");

    // Ctrl+Shift+\ switches orientation (Shift makes key "|" on US layouts).
    fireEvent.keyDown(document.body, { key: "|", code: "Backslash", ctrlKey: true, shiftKey: true });
    expect(screen.getByTestId("split-pane-container")).toHaveAttribute("data-split-mode", "vertical");

    // The secondary pane switches view without touching the primary tab.
    await user.selectOptions(within(secondary).getByLabelText("Secondary pane view"), "helm");
    expect(within(secondary).getByTestId("helm-view")).toBeInTheDocument();
    expect(screen.getByRole("tabpanel")).toHaveAttribute("data-view", "resources");

    fireEvent.keyDown(document.body, { key: "\\", code: "Backslash", ctrlKey: true });
    expect(useLayoutStore.getState().splitMode).toBe("none");
    expect(screen.queryByTestId("split-pane-secondary")).toBeNull();
  });

  it("toggles the split and its orientation from the palette", async () => {
    const user = userEvent.setup();
    render(<App fetchApps={fakeFetchApps()} />);

    await runPaletteAction(user, "Toggle split pane");
    expect(useLayoutStore.getState().splitMode).toBe("horizontal");
    await runPaletteAction(user, "Switch split orientation");
    expect(useLayoutStore.getState().splitMode).toBe("vertical");
    await runPaletteAction(user, "Toggle split pane");
    expect(useLayoutStore.getState().splitMode).toBe("none");
  });

  it("loads cluster groups once on mount through the injected client", async () => {
    const client = fakeGroupClient();
    setClusterGroupClient(client);

    const { rerender } = render(<App fetchApps={fakeFetchApps()} />);
    await act(async () => {
      await Promise.resolve();
    });
    rerender(<App fetchApps={fakeFetchApps()} />);

    expect(client.getClusterGroups).toHaveBeenCalledTimes(1);
    expect(useContextsStore.getState().clusterGroups).toEqual([{ name: "Prod", contexts: ["kind-a"] }]);
  });
});

describe("layout store split orientation", () => {
  beforeEach(() => {
    useLayoutStore.setState(podsLayoutState);
  });

  it("swaps orientation only while a split is open, and new tabs default to resources", () => {
    const store = useLayoutStore.getState();
    store.toggleSplitOrientation();
    expect(useLayoutStore.getState().splitMode).toBe("none");
    store.toggleSplit();
    store.toggleSplitOrientation();
    expect(useLayoutStore.getState().splitMode).toBe("vertical");
    store.toggleSplitOrientation();
    expect(useLayoutStore.getState().splitMode).toBe("horizontal");

    const id = store.openTab({ title: "Pods" });
    expect(useLayoutStore.getState().tabs.find((t) => t.id === id)?.view).toBe("resources");
  });
});
