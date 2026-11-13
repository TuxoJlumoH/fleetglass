import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { useActionsStore } from "../stores/actions";
import { useAppInfoStore } from "../stores/appInfo";
import { initialContextsState, setConnectHandler, useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import { type ClusterSummary, type OverviewData, resetOverviewStore, useOverviewStore } from "../stores/overview";
import { resetPodsStore, setQueryFetcher } from "../stores/pods";
import { resetResourceDetailStore } from "../stores/resourceDetail";
import { useSettingsStore } from "../stores/settings";
import { historyDepthOf } from "./mobileNav";
import { EVENTS_HINT_MAX_AGE_MS } from "./MobileShell";
import { historyLayersBusy } from "./useHistoryLayers";

// ---- controllable matchMedia: `(max-width: Npx)` against a fake viewport width ----
let viewportWidth = 1280;
const listeners = new Set<() => void>();

function matches(query: string): boolean {
  const m = /max-width:\s*([\d.]+)px/.exec(query);
  return m?.[1] !== undefined ? viewportWidth <= Number(m[1]) : false;
}

function installMatchMedia() {
  window.matchMedia = vi.fn((query: string) => ({
    get matches() {
      return matches(query);
    },
    media: query,
    onchange: null,
    addEventListener: (_: string, cb: () => void) => listeners.add(cb),
    removeEventListener: (_: string, cb: () => void) => listeners.delete(cb),
    addListener: () => undefined,
    removeListener: () => undefined,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

function resize(width: number) {
  act(() => {
    viewportWidth = width;
    for (const cb of [...listeners]) cb();
  });
}

function cluster(name: string, patch: Partial<ClusterSummary> = {}): ClusterSummary {
  return {
    cluster: name,
    connection: { state: "connected" },
    nodesReady: 3,
    nodesTotal: 3,
    podsRunning: 35,
    podsPending: 1,
    podsFailing: 2,
    warningEvents1h: 7,
    metrics: { state: "notInstalled", message: "metrics.k8s.io not served" },
    cpu: null,
    memory: null,
    problems: [],
    ...patch,
  };
}

function overview(): OverviewData {
  return {
    clusters: [cluster("kind-a"), cluster("kind-b", { warningEvents1h: 0, podsFailing: 0 })],
    attention: {
      items: [
        {
          cluster: "kind-a",
          kind: "Pod",
          namespace: "default",
          name: "crashloop-6f7b9c-x8k2",
          reason: "CrashLoopBackOff",
          message: "14 restarts",
          since: null,
          status: "failed",
        },
      ],
      truncated: false,
    },
    recentWarnings: [],
    rightSizing: null,
  };
}

/** History back as the Android gesture does it; resolves after the popstate is handled. */
async function back() {
  await act(async () => {
    const popped = new Promise<void>((resolve) => {
      window.addEventListener(
        "popstate",
        () => {
          resolve();
        },
        { once: true },
      );
    });
    window.history.back();
    await popped;
  });
}

const originalMatchMedia = Object.getOwnPropertyDescriptor(window, "matchMedia");

const depthNow = () => historyDepthOf(window.history.state);

/** Waits until no rewind of ours is in flight (between tests, after an unmount). */
async function historySettled() {
  await waitFor(() => {
    expect(historyLayersBusy()).toBe(false);
  });
}

beforeEach(async () => {
  await historySettled();
  viewportWidth = 412;
  listeners.clear();
  installMatchMedia();
  useAppInfoStore.setState({ platform: "linux", version: "0.3.0" });
  useLayoutStore.setState({ ...initialLayoutState, tabs: [...initialLayoutState.tabs] });
  useContextsStore.setState({
    ...initialContextsState,
    status: "ready",
    contexts: [
      { name: "kind-a", cluster: "a", user: "u", namespace: null, server: null, auth: "token", isCurrent: true },
      { name: "kind-b", cluster: "b", user: "u", namespace: null, server: null, auth: "token", isCurrent: false },
    ] as never,
    selectedContext: "kind-a",
    current: "kind-a",
  });
  setConnectHandler();
  resetOverviewStore();
  resetPodsStore();
  resetResourceDetailStore();
  useActionsStore.setState({ running: false });
  useActionsStore.getState().cancel();
});

afterEach(() => {
  if (originalMatchMedia) Object.defineProperty(window, "matchMedia", originalMatchMedia);
  else delete (window as { matchMedia?: unknown }).matchMedia;
  resetOverviewStore();
  resetPodsStore();
});

describe("shell selection", () => {
  it("uses the phone shell below 600 px on any platform", () => {
    render(<App />);
    expect(screen.getByTestId("mobile-shell")).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Contexts" })).toBeNull();
  });

  it("renders nothing at 600–899 px in the app until the platform is known", () => {
    // Inside the app (so `app_info` would answer), with an IPC that answers nothing.
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    mockIPC((cmd) => (cmd === "get_cluster_groups" ? [] : undefined));
    try {
      useAppInfoStore.setState({ platform: null });
      viewportWidth = 800;
      const { container, unmount } = render(<App />);
      expect(container).toBeEmptyDOMElement();
      act(() => {
        useAppInfoStore.setState({ platform: "android" });
      });
      expect(screen.getByTestId("mobile-shell")).toBeInTheDocument();
      unmount();
      // Desktop widths never wait.
      useAppInfoStore.setState({ platform: null });
      viewportWidth = 1280;
      render(<App />);
      expect(screen.queryByTestId("mobile-shell")).toBeNull();
      expect(screen.getByRole("navigation", { name: "Contexts" })).toBeInTheDocument();
    } finally {
      clearMocks();
      delete (globalThis as { isTauri?: boolean }).isTauri;
    }
  });

  it("keeps the desktop shell between 600 and 900 px off Android, and on Android tablets", () => {
    viewportWidth = 800;
    const { unmount } = render(<App />);
    expect(screen.queryByTestId("mobile-shell")).toBeNull();
    unmount();

    useAppInfoStore.setState({ platform: "android" });
    viewportWidth = 1000;
    render(<App />);
    expect(screen.queryByTestId("mobile-shell")).toBeNull();
  });

  it("uses the phone shell on Android below 900 px and follows rotation", () => {
    useAppInfoStore.setState({ platform: "android" });
    viewportWidth = 800;
    render(<App />);
    expect(screen.getByTestId("mobile-shell")).toBeInTheDocument();
    resize(1100);
    expect(screen.queryByTestId("mobile-shell")).toBeNull();
    resize(412);
    expect(screen.getByTestId("mobile-shell")).toBeInTheDocument();
  });
});

describe("mobile overview", () => {
  it("shows the selected cluster's full card, the others compact, and the attention list", () => {
    useOverviewStore.setState({ status: "ready", data: overview(), lastUpdatedAt: Date.now() });
    useContextsStore.setState({
      settingsByContext: {
        "kind-b": { colour: "#a371f7", readOnly: false, protected: true, tags: [] },
      } as never,
    });
    render(<App />);
    const page = screen.getByTestId("m-overview");
    expect(within(page).getByRole("heading", { name: "Overview" })).toBeInTheDocument();
    expect(within(page).getByText("2 clusters")).toBeInTheDocument();
    expect(within(page).getByTestId("overview-cluster-kind-a")).toBeInTheDocument();
    const mini = within(page).getByTestId("m-cluster-mini-kind-b");
    expect(within(mini).getByRole("img", { name: "Protected context" })).toBeInTheDocument();
    expect(within(page).queryByTestId("overview-cluster-kind-b")).toBeNull();
    const attention = within(page).getByRole("list", { name: "Needs attention" });
    expect(within(attention).getByText("crashloop-6f7b9c-x8k2")).toBeInTheDocument();
    expect(within(attention).getByText("CrashLoopBackOff")).toBeInTheDocument();
  });

  it("tapping a compact card makes it the full card", async () => {
    const user = userEvent.setup();
    useOverviewStore.setState({ status: "ready", data: overview(), lastUpdatedAt: Date.now() });
    render(<App />);
    await user.click(screen.getByTestId("m-cluster-mini-kind-b"));
    expect(screen.getByTestId("overview-cluster-kind-b")).toBeInTheDocument();
    expect(screen.getByTestId("m-cluster-mini-kind-a")).toBeInTheDocument();
  });

  it("is honest outside the app instead of showing zeros", () => {
    render(<App />);
    expect(screen.getByText("Not running in the Fleetglass app")).toBeInTheDocument();
  });

  it("marks Events with a red dot only when a cluster reports warnings", () => {
    const { unmount } = render(<App />);
    expect(screen.getByTestId("m-bnav-events")).toHaveAccessibleName("Events");
    unmount();
    useOverviewStore.setState({ status: "ready", data: overview(), lastUpdatedAt: Date.now() });
    render(<App />);
    expect(screen.getByTestId("m-bnav-events")).toHaveAccessibleName("Events, warnings");
  });

  it("drops the Events dot once the overview data is older than the hint's lifetime", async () => {
    useOverviewStore.setState({
      status: "ready",
      data: overview(),
      lastUpdatedAt: Date.now() - EVENTS_HINT_MAX_AGE_MS - 1,
    });
    render(<App />);
    await waitFor(() => {
      expect(screen.getByTestId("m-bnav-events")).toHaveAccessibleName("Events");
    });
    // A fresh update brings it back.
    act(() => {
      useOverviewStore.setState({ lastUpdatedAt: Date.now() });
    });
    expect(screen.getByTestId("m-bnav-events")).toHaveAccessibleName("Events, warnings");
  });
});

describe("bottom nav and back", () => {
  it("switches screens and marks the active item", async () => {
    const user = userEvent.setup();
    render(<App />);
    expect(screen.getByTestId("m-bnav-overview")).toHaveAttribute("aria-current", "page");

    await user.click(screen.getByTestId("m-bnav-workloads"));
    expect(screen.getByTestId("m-view")).toHaveAttribute("data-view", "resources");
    expect(screen.getByTestId("m-bnav-workloads")).toHaveAttribute("aria-current", "page");
    const active = useLayoutStore.getState().tabs.find((t) => t.id === useLayoutStore.getState().activeTabId);
    expect(active?.kind).toBe("Pod");

    await user.click(screen.getByTestId("m-bnav-events"));
    expect(screen.getByTestId("m-bnav-events")).toHaveAttribute("aria-current", "page");
    const ev = useLayoutStore.getState().tabs.find((t) => t.id === useLayoutStore.getState().activeTabId);
    expect(ev?.kind).toBe("Event");

    await user.click(screen.getByTestId("m-bnav-settings"));
    expect(screen.getByTestId("m-settings")).toBeInTheDocument();

    await user.click(screen.getByTestId("m-bnav-overview"));
    expect(screen.getByTestId("m-overview")).toBeInTheDocument();
  });

  it("history back returns to Overview from another destination", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-bnav-workloads"));
    expect(window.history.state).toEqual({ fgMobile: 1 });
    await back();
    await waitFor(() => {
      expect(screen.getByTestId("m-overview")).toBeInTheDocument();
    });
    expect(screen.getByTestId("m-bnav-overview")).toHaveAttribute("aria-current", "page");
  });
});

describe("back with layers", () => {
  it("a scrim tap rewinds the history, so the next back leaves the app", async () => {
    const user = userEvent.setup();
    window.history.pushState({ start: true }, "");
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    expect(depthNow()).toBe(1);
    await user.click(screen.getByTestId("m-scrim"));
    await historySettled();
    expect(window.history.state).toEqual({ start: true });
  });

  it("bottom nav Overview rewinds every screen", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-bnav-settings"));
    await user.click(screen.getByTestId("m-settings-import"));
    expect(depthNow()).toBe(2);
    await user.click(screen.getByTestId("m-bnav-overview"));
    await historySettled();
    expect(depthNow()).toBe(0);
  });

  it("closes the detail opened from an attention row, not the screen", async () => {
    const user = userEvent.setup();
    useOverviewStore.setState({ status: "ready", data: overview(), lastUpdatedAt: Date.now() });
    render(<App />);
    const attention = screen.getByRole("list", { name: "Needs attention" });
    await user.click(within(attention).getByRole("button", { name: /crashloop-6f7b9c-x8k2/ }));
    expect(screen.getByRole("region", { name: "Resource details" })).toBeInTheDocument();
    expect(depthNow()).toBe(1);
    await back();
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    });
    expect(screen.getByTestId("m-overview")).toBeInTheDocument();
    expect(depthNow()).toBe(0);
  });

  it("Workloads shows the phone list; BACK closes a row's detail, then the screen", async () => {
    const user = userEvent.setup();
    setQueryFetcher((spec) =>
      Promise.resolve({
        total: 1,
        offset: spec.offset,
        limit: spec.limit,
        version: 1,
        access: [{ cluster: "kind-a", kind: "Pod", access: { state: "ready" }, rows: 1, metrics: null }],
        statusCounts: null,
        rows: [
          {
            uid: "uid-web",
            cluster: "kind-a",
            kind: "Pod",
            namespace: "default",
            name: "web-0",
            status: "healthy",
            statusReason: "Running",
            cells: [],
            labels: [],
            custom: [],
          },
        ],
      } as never),
    );
    render(<App />);
    await user.click(screen.getByTestId("m-bnav-workloads"));
    expect(screen.getByTestId("m-list")).toBeInTheDocument();
    // Not the desktop table.
    expect(screen.queryByRole("grid")).toBeNull();
    expect(screen.queryByRole("table")).toBeNull();
    await user.click(await screen.findByTestId("m-row"));
    expect(screen.getByRole("region", { name: "Resource details" })).toBeInTheDocument();
    expect(depthNow()).toBe(2);

    await back();
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    });
    expect(screen.getByTestId("m-list")).toBeInTheDocument();
    expect(depthNow()).toBe(1);

    await back();
    await waitFor(() => {
      expect(screen.getByTestId("m-overview")).toBeInTheDocument();
    });
  });

  it("cancels a pending action confirm first, then the drawer", async () => {
    const user = userEvent.setup();
    useOverviewStore.setState({ status: "ready", data: overview(), lastUpdatedAt: Date.now() });
    render(<App />);
    const attention = screen.getByRole("list", { name: "Needs attention" });
    await user.click(within(attention).getByRole("button", { name: /crashloop-6f7b9c-x8k2/ }));
    act(() => {
      useActionsStore.getState().request({
        context: "kind-a",
        action: { type: "delete", kind: "Pod", namespace: "default", name: "crashloop-6f7b9c-x8k2" },
        originUid: null,
      });
    });
    expect(screen.getByTestId("action-confirm-dialog")).toBeInTheDocument();
    expect(depthNow()).toBe(2);

    await back();
    await waitFor(() => {
      expect(screen.queryByTestId("action-confirm-dialog")).toBeNull();
    });
    expect(useActionsStore.getState().pending).toBeNull();
    expect(screen.getByRole("region", { name: "Resource details" })).toBeInTheDocument();
    expect(depthNow()).toBe(1);
  });

  it("never leaves the app while an action is running: back keeps the dialog and its entry", async () => {
    useOverviewStore.setState({ status: "ready", data: overview(), lastUpdatedAt: Date.now() });
    render(<App />);
    act(() => {
      useActionsStore.getState().request({
        context: "kind-a",
        action: { type: "delete", kind: "Pod", namespace: "default", name: "crashloop-6f7b9c-x8k2" },
        originUid: null,
      });
      useActionsStore.setState({ running: true });
    });
    expect(depthNow()).toBe(1);
    await back();
    await historySettled();
    expect(screen.getByTestId("action-confirm-dialog")).toBeInTheDocument();
    expect(depthNow()).toBe(1);
    expect(screen.getByTestId("m-overview")).toBeInTheDocument();
  });

  it("closes diagnostics opened from the slide-over before the slide-over", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    await user.click(screen.getByRole("button", { name: "Run diagnostics for kind-b" }));
    expect(screen.getByRole("dialog", { name: "Connection diagnostics" })).toBeInTheDocument();
    // Diagnostics neither select the context nor close the slide-over.
    expect(screen.getByTestId("m-slide")).toBeInTheDocument();
    expect(useContextsStore.getState().selectedContext).toBe("kind-a");
    expect(depthNow()).toBe(2);

    await back();
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Connection diagnostics" })).toBeNull();
    });
    expect(screen.getByTestId("m-slide")).toBeInTheDocument();
    await back();
    await waitFor(() => {
      expect(screen.queryByTestId("m-slide")).toBeNull();
    });
    expect(depthNow()).toBe(0);
  });

  it("rotating to the desktop layout leaves no phone history entries", async () => {
    const user = userEvent.setup();
    useAppInfoStore.setState({ platform: "android" });
    window.history.pushState({ start: true }, "");
    viewportWidth = 800;
    render(<App />);
    await user.click(screen.getByTestId("m-bnav-settings"));
    await user.click(screen.getByTestId("m-menu"));
    expect(depthNow()).toBe(2);
    resize(1100);
    expect(screen.queryByTestId("mobile-shell")).toBeNull();
    await historySettled();
    expect(window.history.state).toEqual({ start: true });
  });
});

describe("slide-over", () => {
  it("selecting a view with the keyboard closes it too", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    within(screen.getByTestId("m-slide")).getByRole("button", { name: /^Deployments/ }).focus();
    await user.keyboard("{Enter}");
    expect(screen.queryByTestId("m-slide")).toBeNull();
    expect(screen.getByTestId("m-view")).toHaveAttribute("data-view", "resources");
  });

  it("selecting a cluster group closes it", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    await user.click(within(screen.getByTestId("m-slide")).getByRole("button", { name: /^All Clusters/ }));
    expect(screen.queryByTestId("m-slide")).toBeNull();
    expect(screen.getByTestId("m-ctxsel")).toHaveTextContent("2 clusters");
  });

  it("toggling a resource group keeps it open", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    const toggle = within(screen.getByTestId("m-slide")).getByTestId("sidebar-group-toggle-Workloads");
    await user.click(toggle);
    expect(screen.getByTestId("m-slide")).toBeInTheDocument();
    // Expand again (the collapse state is persisted).
    await user.click(toggle);
  });

  it("opens the desktop sidebar from ☰ and closes on scrim tap", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    const slide = screen.getByTestId("m-slide");
    expect(within(slide).getByRole("navigation", { name: "Contexts" })).toBeInTheDocument();
    expect(screen.getByTestId("m-menu")).toHaveAttribute("aria-expanded", "true");
    await user.click(screen.getByTestId("m-scrim"));
    expect(screen.queryByTestId("m-slide")).toBeNull();
  });

  it("opens even when the desktop sidebar was collapsed", async () => {
    const user = userEvent.setup();
    useLayoutStore.setState({ sidebarCollapsed: true });
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    expect(within(screen.getByTestId("m-slide")).getByRole("navigation", { name: "Contexts" })).toBeVisible();
  });

  it("selecting a view closes it and shows that view", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    const slide = screen.getByTestId("m-slide");
    await user.click(within(slide).getByRole("button", { name: /^Deployments/ }));
    expect(screen.queryByTestId("m-slide")).toBeNull();
    expect(screen.getByTestId("m-view")).toHaveAttribute("data-view", "resources");
    const active = useLayoutStore.getState().tabs.find((t) => t.id === useLayoutStore.getState().activeTabId);
    expect(active?.kind).toBe("Deployment");
    // Not one of the four destinations.
    expect(screen.getByRole("navigation", { name: "Main" }).querySelector("[aria-current]")).toBeNull();
  });

  it("selecting a context closes it and keeps the screen", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-menu"));
    await user.click(within(screen.getByTestId("m-slide")).getByText("kind-b"));
    expect(screen.queryByTestId("m-slide")).toBeNull();
    expect(useContextsStore.getState().selectedContext).toBe("kind-b");
    expect(screen.getByTestId("m-ctxsel")).toHaveTextContent("kind-b");
    expect(screen.getByTestId("m-overview")).toBeInTheDocument();
  });

  it("history back closes it first, then the screen", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-bnav-settings"));
    await user.click(screen.getByTestId("m-menu"));
    expect(window.history.state).toEqual({ fgMobile: 2 });
    await back();
    await waitFor(() => {
      expect(screen.queryByTestId("m-slide")).toBeNull();
    });
    expect(screen.getByTestId("m-settings")).toBeInTheDocument();
    await back();
    await waitFor(() => {
      expect(screen.getByTestId("m-overview")).toBeInTheDocument();
    });
  });
});

describe("app bar", () => {
  it("search opens the command palette, and history back closes it", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByTestId("m-search"));
    expect(screen.getByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
    await back();
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
    });
  });

  it("shows the context colour and the read-only banner", () => {
    useContextsStore.setState({
      settingsByContext: { "kind-a": { colour: "#ff0000", readOnly: true, protected: false, tags: [] } } as never,
    });
    render(<App />);
    expect(screen.getByTestId("mobile-shell").style.getPropertyValue("--context-accent")).toBe("#ff0000");
    expect(screen.getByTestId("active-context-readonly")).toHaveTextContent("Read-only context (kind-a)");
    expect(screen.getByTestId("status-access-mode")).toHaveTextContent("Read-only");
  });

  it("the status strip loads the settings of every cluster in a selected group", () => {
    const original = useContextsStore.getState().ensureSettings;
    const ensureSettings = vi.fn(() => Promise.resolve());
    useContextsStore.setState({ ensureSettings, activeClusterSelection: ["kind-a", "kind-b"] });
    try {
      render(<App />);
      expect(ensureSettings).toHaveBeenCalledWith(["kind-a", "kind-b"]);
    } finally {
      useContextsStore.setState({ ensureSettings: original });
    }
  });
});

describe("settings screen", () => {
  it("links to Import contexts and the audit log as sub-screens, and shows the version", async () => {
    const user = userEvent.setup();
    render(<App fetchAuditLog={() => Promise.resolve({ entries: [], total: 0 } as never)} />);
    await user.click(screen.getByTestId("m-bnav-settings"));
    expect(screen.getByTestId("m-settings-version")).toHaveTextContent("v0.3.0");

    await user.click(screen.getByTestId("m-settings-import"));
    expect(screen.getByRole("heading", { name: "Import contexts" })).toBeInTheDocument();
    expect(screen.getByTestId("m-bnav-settings")).toHaveAttribute("aria-current", "page");
    await back();
    await waitFor(() => {
      expect(screen.getByTestId("m-settings")).toBeInTheDocument();
    });

    await user.click(screen.getByTestId("m-settings-audit"));
    expect(screen.getByTestId("m-view")).toHaveAttribute("data-view", "audit");
  });

  it("changes the theme through the existing setting", async () => {
    const user = userEvent.setup();
    const before = useSettingsStore.getState().theme;
    render(<App />);
    await user.click(screen.getByTestId("m-bnav-settings"));
    await user.click(screen.getByRole("radio", { name: "Light" }));
    expect(useSettingsStore.getState().theme).toBe("light");
    expect(screen.getByRole("radio", { name: "Light" })).toHaveAttribute("aria-checked", "true");
    useSettingsStore.getState().setTheme(before);
  });
});
