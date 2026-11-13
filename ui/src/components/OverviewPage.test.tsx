import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Channel } from "@tauri-apps/api/core";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { MetricsAvailability, OverviewData_Deserialize } from "../generated/bindings";
import { defaultContextSettings, initialContextsState, useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import {
  type AttentionItem,
  type ClusterSummary,
  type OverviewData,
  resetOverviewStore,
  useOverviewStore,
  type WarningEvent,
} from "../stores/overview";
import { resetResourceDetailStore, useResourceDetailStore } from "../stores/resourceDetail";
import { OverviewPage } from "./OverviewPage";
import { filterWarnings } from "./overviewWarnings";

const NOW = Date.parse("2026-11-10T12:00:00Z");

function minutesAgo(m: number): string {
  return new Date(NOW - m * 60_000).toISOString();
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
    metrics: { state: "available" },
    cpu: {
      used: 1520,
      requested: 2440,
      allocatable: 4000,
      formattedUsed: "1520m",
      formattedRequested: "2440m",
      formattedAllocatable: "4",
      usedPct: 38,
      requestedPct: 61,
    },
    memory: {
      used: 4423,
      requested: 5898,
      allocatable: 8192,
      formattedUsed: "4.3Gi",
      formattedRequested: "5.8Gi",
      formattedAllocatable: "8Gi",
      usedPct: 54,
      requestedPct: 72,
    },
    problems: [],
    ...patch,
  };
}

function attention(name: string, patch: Partial<AttentionItem> = {}): AttentionItem {
  return {
    cluster: "kind-fleetglass-dev",
    kind: "Pod",
    namespace: "default",
    name,
    reason: "CrashLoopBackOff",
    message: "14 restarts · last: Error (1) 4m ago",
    since: minutesAgo(30),
    status: "failed",
    ...patch,
  };
}

function warning(name: string, minutes: number, patch: Partial<WarningEvent> = {}): WarningEvent {
  return {
    cluster: "kind-fleetglass-dev",
    namespace: "default",
    name: `${name}.17a`,
    involvedKind: "Pod",
    involvedName: name,
    reason: "BackOff",
    message: `Back-off restarting failed container in ${name}`,
    count: 3,
    lastTimestamp: minutesAgo(minutes),
    age: `${String(minutes)}m`,
    ...patch,
  };
}

function overview(patch: Partial<OverviewData> = {}): OverviewData {
  return {
    clusters: [cluster("kind-fleetglass-dev")],
    attention: {
      items: [
        attention("crashloop-6f7b9c-x8k2"),
        attention("pending-q2m4", { reason: "Unschedulable", status: "pending", message: "0/3 nodes have enough CPU" }),
      ],
      truncated: false,
    },
    recentWarnings: [warning("crashloop-6f7b9c-x8k2", 5), warning("old-pod", 3 * 60)],
    rightSizing: null,
    ...patch,
  };
}

interface Backend {
  subscribes: Channel<OverviewData_Deserialize>[];
  /** `clusters` argument of each `subscribe_overview` call. */
  subscribeClusters: string[][];
  unsubscribes: number[];
  details: Record<string, unknown>[];
}

function mockBackend(): Backend {
  (globalThis as { isTauri?: boolean }).isTauri = true;
  mockWindows("main");
  const calls: Backend = { subscribes: [], subscribeClusters: [], unsubscribes: [], details: [] };
  mockIPC((cmd, args) => {
    const a = args as Record<string, unknown>;
    if (cmd === "subscribe_overview") {
      calls.subscribeClusters.push(a.clusters as string[]);
      calls.subscribes.push(a.onUpdate as Channel<OverviewData_Deserialize>);
      return calls.subscribes.length;
    }
    if (cmd === "unsubscribe_overview") {
      calls.unsubscribes.push(a.subscriptionId as number);
      return true;
    }
    if (cmd === "get_resource_detail") {
      calls.details.push(a);
      // Never resolves: the drawer stays in its loading state.
      return new Promise(() => undefined);
    }
    if (cmd === "get_context_settings") return defaultContextSettings;
    return undefined;
  });
  return calls;
}

let pushIndex = 0;
function push(channel: Channel<OverviewData_Deserialize> | undefined, data: OverviewData) {
  if (channel === undefined) throw new Error("no subscription");
  act(() => {
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: { runCallback: (id: number, data: unknown) => void };
      }
    ).__TAURI_INTERNALS__;
    internals.runCallback(channel.id, { index: pushIndex, message: data });
    pushIndex += 1;
  });
}

/** Renders the page and pushes `data` as the subscription's first message. */
async function renderWith(data: OverviewData): Promise<Backend> {
  const calls = mockBackend();
  render(<OverviewPage />);
  await waitFor(() => {
    expect(calls.subscribes).toHaveLength(1);
  });
  push(calls.subscribes[0], data);
  return calls;
}

function card(name: string): HTMLElement {
  return screen.getByTestId(`overview-cluster-${name}`);
}

beforeEach(() => {
  pushIndex = 0;
  resetOverviewStore();
  resetResourceDetailStore();
  useContextsStore.setState(initialContextsState);
  useLayoutStore.setState(initialLayoutState);
});

afterEach(() => {
  resetOverviewStore();
  resetResourceDetailStore();
  clearMocks();
  delete (globalThis as { isTauri?: boolean }).isTauri;
});

describe("overview store lifecycle", () => {
  it("subscribes once while mounted and unsubscribes on unmount", async () => {
    const calls = mockBackend();
    const { unmount } = render(<OverviewPage />);
    expect(screen.getByText("Loading overview…")).toBeInTheDocument();
    await waitFor(() => {
      expect(calls.subscribes).toHaveLength(1);
    });
    push(calls.subscribes[0], overview());
    expect(useOverviewStore.getState().status).toBe("ready");
    expect(useOverviewStore.getState().lastUpdatedAt).not.toBeNull();
    expect(calls.unsubscribes).toEqual([]);

    unmount();
    await waitFor(() => {
      expect(calls.unsubscribes).toEqual([1]);
    });
    expect(calls.subscribes).toHaveLength(1);
  });

  it("does not leak a subscription under StrictMode's double mount", async () => {
    const calls = mockBackend();
    const { unmount } = render(
      <StrictMode>
        <OverviewPage />
      </StrictMode>,
    );
    // The first mount's subscription is closed as soon as its id is known.
    await waitFor(() => {
      expect(calls.subscribes.length - calls.unsubscribes.length).toBe(1);
    });
    expect(calls.subscribes.length).toBeLessThanOrEqual(2);
    // Only the live subscription's messages reach the store.
    const liveIndex = calls.subscribes.length - 1;
    push(calls.subscribes[liveIndex], overview());
    expect(screen.getByText("1 cluster")).toBeInTheDocument();

    unmount();
    await waitFor(() => {
      expect(calls.unsubscribes).toHaveLength(calls.subscribes.length);
    });
  });

  it("subscribes for the selected context and re-subscribes when the selection changes", async () => {
    useContextsStore.setState({ selectedContext: "kind-fleetglass-dev", activeClusterSelection: [] });
    const calls = mockBackend();
    render(<OverviewPage />);
    await waitFor(() => {
      expect(calls.subscribeClusters).toEqual([["kind-fleetglass-dev"]]);
    });

    act(() => {
      useContextsStore.setState({ activeClusterSelection: ["a", "b", "a"] });
    });
    await waitFor(() => {
      expect(calls.subscribeClusters).toEqual([["kind-fleetglass-dev"], ["a", "b"]]);
    });
    // The old subscription is closed; only one stays open.
    await waitFor(() => {
      expect(calls.unsubscribes).toEqual([1]);
    });
  });

  it("is honest outside the desktop app", () => {
    render(<OverviewPage />);
    expect(screen.getByText("Not running in the Fleetglass app")).toBeInTheDocument();
    expect(screen.queryByText("Loading overview…")).toBeNull();
  });

  it("shows a subscription error", async () => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    mockIPC((cmd) => {
      if (cmd === "subscribe_overview") throw Object.assign(new Error("boom"), { kind: "internal" });
      return undefined;
    });
    render(<OverviewPage />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Cannot load the overview: boom");
  });
});

describe("OverviewPage cluster cards", () => {
  it("renders the header, stats and capacity bars when metrics are available", async () => {
    useContextsStore.setState({
      settingsByContext: {
        "kind-fleetglass-dev": { ...defaultContextSettings, colour: "#a371f7", protected: true },
      },
    });
    await renderWith(overview());

    expect(screen.getByRole("heading", { name: "Overview" })).toBeInTheDocument();
    expect(screen.getByText("1 cluster")).toBeInTheDocument();
    const c = card("kind-fleetglass-dev");
    expect(c.style.getPropertyValue("--ctx")).toBe("#a371f7");
    expect(within(c).getByText("protected")).toBeInTheDocument();
    expect(within(c).getByText("2 issues")).toBeInTheDocument();
    expect(within(c).getByText("NODES READY")).toBeInTheDocument();
    expect(within(c).getByText("3/3")).toBeInTheDocument();
    expect(within(c).getByText("38")).toBeInTheDocument();
    expect(within(c).getByText("2 failing")).toBeInTheDocument();
    expect(within(c).getByText("1 pending")).toBeInTheDocument();
    expect(within(c).getByText("WARNINGS · 1H")).toBeInTheDocument();
    expect(within(c).getByText("7")).toBeInTheDocument();
    expect(within(c).getByTestId("capacity-CPU")).toHaveTextContent("38% used · 61% requested");
    expect(within(c).getByTestId("capacity-Memory")).toHaveTextContent("54% used · 72% requested");
    expect(within(c).getByRole("img", { name: "CPU: 38% used, 61% requested" })).toBeInTheDocument();
  });

  it("shows 'No issues' when the cluster has no attention items", async () => {
    await renderWith(overview({ attention: { items: [], truncated: false } }));
    expect(within(card("kind-fleetglass-dev")).getByText("No issues")).toHaveClass("pill", "ok");
    expect(screen.getByText("Nothing needs attention on the connected clusters.")).toBeInTheDocument();
  });

  it.each<[MetricsAvailability, string]>([
    [{ state: "notInstalled", message: "404" }, "metrics-server not installed"],
    [{ state: "forbidden", message: "403" }, "No permission to read metrics"],
    [{ state: "pending" }, "Metrics loading…"],
    [{ state: "error", message: "timeout" }, "Metrics unavailable: timeout"],
  ])("never shows 0%% without metrics (%o)", async (metrics, text) => {
    await renderWith(overview({ clusters: [cluster("kind-fleetglass-dev", { metrics, cpu: null, memory: null })] }));
    const c = card("kind-fleetglass-dev");
    expect(within(c).getByTestId("capacity-CPU")).toHaveTextContent(text);
    expect(within(c).getByTestId("capacity-Memory")).toHaveTextContent(text);
    expect(c).not.toHaveTextContent("0%");
    expect(within(c).queryByRole("img")).toBeNull();
  });

  it("shows — and every problem instead of zeros when Nodes and Events are forbidden", async () => {
    const nodes = "No permission to list Node";
    const events = "No permission to list Event";
    await renderWith(
      overview({
        clusters: [
          cluster("kind-fleetglass-dev", {
            connection: { state: "degraded", message: nodes },
            nodesReady: null,
            nodesTotal: null,
            warningEvents1h: null,
            cpu: null,
            memory: null,
            problems: [
              { kind: "Node", message: nodes },
              { kind: "Event", message: events },
            ],
          }),
        ],
      }),
    );
    const c = card("kind-fleetglass-dev");
    expect(within(c).getAllByText(nodes)).toHaveLength(1);
    expect(within(c).getByText(events)).toBeInTheDocument();
    expect(within(c).getAllByText("—")).toHaveLength(2);
    expect(within(c).queryByText("0/0")).toBeNull();
    // Pods are still known: 35 + 1 + 2.
    expect(within(c).getByText("38")).toBeInTheDocument();
  });

  it("shows the error and a Connect button for a cluster in error", async () => {
    useContextsStore.setState({ selectedContext: "kind-fleetglass-dev" });
    await renderWith(
      overview({
        clusters: [
          cluster("kind-fleetglass-dev"),
          cluster("staging-eu", { connection: { state: "error", message: "connection refused" } }),
        ],
      }),
    );
    expect(screen.getByText("2 clusters")).toBeInTheDocument();
    const c = card("staging-eu");
    expect(within(c).getByRole("alert")).toHaveTextContent("connection refused");
    // No stats for a cluster whose stores failed: they would be zeros.
    expect(within(c).queryByText("NODES READY")).toBeNull();

    await userEvent.setup().click(within(c).getByRole("button", { name: "Connect" }));
    expect(useContextsStore.getState().selectedContext).toBe("staging-eu");
    const layout = useLayoutStore.getState();
    const active = layout.tabs.find((t) => t.id === layout.activeTabId);
    expect(active).toMatchObject({ view: "resources", kind: "Pod" });
  });

  it("shows the message of a degraded cluster with its stats", async () => {
    await renderWith(
      overview({
        clusters: [cluster("kind-fleetglass-dev", { connection: { state: "degraded", message: "No permission to list events" } })],
      }),
    );
    const c = card("kind-fleetglass-dev");
    expect(within(c).getByRole("status")).toHaveTextContent("No permission to list events");
    expect(within(c).getByText("NODES READY")).toBeInTheDocument();
  });

  it("shows an honest empty state when no cluster is selected", async () => {
    const calls = await renderWith({
      clusters: [],
      attention: { items: [], truncated: false },
      recentWarnings: [],
      rightSizing: null,
    });
    // Nothing selected: an empty list, which the backend answers without connecting anything.
    expect(calls.subscribeClusters).toEqual([[]]);
    expect(screen.getByText("No cluster selected — select a context in the sidebar.")).toBeInTheDocument();
    expect(screen.getByText("0 clusters")).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole("button", { name: "Open Pods" }));
    const layout = useLayoutStore.getState();
    expect(layout.tabs.find((t) => t.id === layout.activeTabId)?.kind).toBe("Pod");
  });
});

describe("OverviewPage needs attention", () => {
  it("lists items with tone, name and namespace · cluster", async () => {
    await renderWith(overview());
    const list = screen.getByRole("list", { name: "Needs attention" });
    const rows = within(list).getAllByRole("button");
    expect(rows).toHaveLength(2);
    expect(within(rows[0] as HTMLElement).getByText("CrashLoopBackOff")).toHaveClass("pill", "err");
    expect(rows[0]).toHaveTextContent("crashloop-6f7b9c-x8k2");
    expect(rows[0]).toHaveTextContent("default · kind-fleetglass-dev");
    expect(rows[0]).toHaveTextContent("14 restarts · last: Error (1) 4m ago");
    expect(within(rows[1] as HTMLElement).getByText("Unschedulable")).toHaveClass("pill", "warn");
    expect(screen.getByTestId("attention-count")).toHaveTextContent("2");
  });

  it("labels a truncated list", async () => {
    const items = Array.from({ length: 50 }, (_, i) => attention(`pod-${String(i)}`));
    await renderWith(overview({ attention: { items, truncated: true } }));
    expect(screen.getByTestId("attention-count")).toHaveTextContent("50+ · showing first 50");
    expect(within(card("kind-fleetglass-dev")).getByText("50+ issues")).toBeInTheDocument();
  });

  it("opens the object's drawer on click and on Enter, and restores focus on close", async () => {
    const user = userEvent.setup();
    const calls = await renderWith(
      overview({
        attention: {
          items: [
            attention("crashloop-6f7b9c-x8k2"),
            attention("checkout", { kind: "Deployment", namespace: "shop", reason: "Not ready", status: "degraded" }),
          ],
          truncated: false,
        },
      }),
    );
    const list = screen.getByRole("list", { name: "Needs attention" });
    const [first, second] = within(list).getAllByRole("button");
    if (!first || !second) throw new Error("rows missing");

    await user.click(first);
    expect(useResourceDetailStore.getState().target).toMatchObject({
      cluster: "kind-fleetglass-dev",
      kind: "Pod",
      namespace: "default",
      name: "crashloop-6f7b9c-x8k2",
    });
    await waitFor(() => {
      expect(calls.details[0]).toEqual({
        context: "kind-fleetglass-dev",
        kind: "Pod",
        namespace: "default",
        name: "crashloop-6f7b9c-x8k2",
      });
    });
    const drawer = screen.getByRole("region", { name: "Resource details" });
    expect(drawer).toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    expect(first).toHaveFocus();

    // Keyboard: arrow to the next row, Enter opens it.
    await user.keyboard("{ArrowDown}");
    expect(second).toHaveFocus();
    expect(second).toHaveTextContent("Deployment checkout");
    await user.keyboard("{Enter}");
    expect(useResourceDetailStore.getState().target).toMatchObject({ kind: "Deployment", namespace: "shop", name: "checkout" });
  });
});

describe("OverviewPage recent warning events", () => {
  it("filters by the selected time range within the bounded list", async () => {
    const user = userEvent.setup();
    const now = Date.now();
    const at = (m: number) => new Date(now - m * 60_000).toISOString();
    await renderWith(
      overview({
        recentWarnings: [
          warning("recent-pod", 5, { lastTimestamp: at(5) }),
          warning("three-hours", 180, { lastTimestamp: at(180) }),
          warning("twelve-hours", 720, { lastTimestamp: at(720) }),
        ],
      }),
    );
    const list = () => screen.getByRole("list", { name: "Recent warning events" });
    expect(within(list()).getAllByRole("listitem")).toHaveLength(1);
    expect(list()).toHaveTextContent("recent-pod");

    await user.click(screen.getByRole("button", { name: "Last 6h" }));
    expect(screen.getByRole("button", { name: "Last 6h" })).toHaveAttribute("aria-pressed", "true");
    expect(within(list()).getAllByRole("listitem")).toHaveLength(2);

    await user.click(screen.getByRole("button", { name: "Last 24h" }));
    expect(within(list()).getAllByRole("listitem")).toHaveLength(3);
    // The per-cluster count stays labelled 1h whatever the range.
    expect(within(card("kind-fleetglass-dev")).getByText("WARNINGS · 1H")).toBeInTheDocument();
  });

  it("says so when the range has no events", async () => {
    const user = userEvent.setup();
    await renderWith(overview({ recentWarnings: [warning("old", 0, { lastTimestamp: "2020-01-01T00:00:00Z" })] }));
    expect(screen.getByText("No warning events in the last 1h")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Last 6h" }));
    expect(screen.getByText("No warning events in the last 6h")).toBeInTheDocument();
  });

  it("notes that only the 50 most recent events are listed", async () => {
    const events = Array.from({ length: 50 }, (_, i) =>
      warning(`p-${String(i)}`, 1, { lastTimestamp: new Date().toISOString() }),
    );
    await renderWith(overview({ recentWarnings: events }));
    expect(screen.getByText(/From the 50 most recent warning events/)).toBeInTheDocument();
  });

  it("shows the reason, involved object, cluster label and opens the involved object", async () => {
    const user = userEvent.setup();
    const ts = new Date().toISOString();
    await renderWith(
      overview({
        clusters: [cluster("kind-fleetglass-dev"), cluster("staging-eu")],
        recentWarnings: [
          warning("api-5c6b2d9f7-wq8zn", 0, {
            cluster: "staging-eu",
            namespace: "shop",
            reason: "Unhealthy",
            message: "Readiness probe failed: HTTP 503",
            lastTimestamp: ts,
          }),
        ],
      }),
    );
    const row = screen.getByRole("button", { name: "Unhealthy: Pod api-5c6b2d9f7-wq8zn on staging-eu" });
    expect(within(row).getByText("Unhealthy")).toHaveClass("overview-row__reason");
    expect(within(row).getByText("staging-eu")).toBeInTheDocument();
    expect(within(row).getByTitle("Readiness probe failed: HTTP 503")).toBeInTheDocument();
    await user.click(row);
    expect(useResourceDetailStore.getState().target).toMatchObject({
      cluster: "staging-eu",
      kind: "Pod",
      namespace: "shop",
      name: "api-5c6b2d9f7-wq8zn",
    });
  });

  it("opens cluster-scoped involved objects without a namespace", async () => {
    const user = userEvent.setup();
    await renderWith(
      overview({
        recentWarnings: [
          warning("worker-1", 0, {
            involvedKind: "Node",
            reason: "NodeNotReady",
            lastTimestamp: new Date().toISOString(),
          }),
        ],
      }),
    );
    await user.click(screen.getByRole("button", { name: "NodeNotReady: Node worker-1" }));
    expect(useResourceDetailStore.getState().target).toMatchObject({ kind: "Node", namespace: null, name: "worker-1" });
  });

  it("switches to Right-sizing & Efficiency tab and renders categories", async () => {
    const user = userEvent.setup();
    const rsData = overview({
      rightSizing: {
        metricsAvailable: true,
        notice: null,
        thresholdPct: 20,
        nearLimit: [
          {
            cluster: "kind-fleetglass-dev",
            namespace: "default",
            name: "pod-near-lim",
            kind: "Pod",
            resource: "memory",
            usage: 920,
            formattedUsage: "920Mi",
            request: 500,
            formattedRequest: "500Mi",
            limit: 1000,
            formattedLimit: "1000Mi",
            percent: 92,
            wasted: null,
            formattedWasted: "—",
            label: "92% limit · OOMKill risk",
          },
        ],
        overRequest: [
          {
            cluster: "kind-fleetglass-dev",
            namespace: "default",
            name: "pod-over-req",
            kind: "Pod",
            resource: "cpu",
            usage: 650,
            formattedUsage: "650m",
            request: 500,
            formattedRequest: "500m",
            limit: 1000,
            formattedLimit: "1000m",
            percent: 130,
            wasted: null,
            formattedWasted: "—",
            label: "130% of request",
          },
        ],
        unusedRequests: [
          {
            cluster: "kind-fleetglass-dev",
            namespace: "default",
            name: "pod-unused",
            kind: "Pod",
            resource: "cpu",
            usage: 50,
            formattedUsage: "50m",
            request: 1000,
            formattedRequest: "1000m",
            limit: 2000,
            formattedLimit: "2000m",
            percent: 5,
            wasted: 950,
            formattedWasted: "950m",
            label: "950m wasted (5% used)",
          },
        ],
        missingResources: [
          {
            cluster: "kind-fleetglass-dev",
            namespace: "default",
            name: "pod-missing",
            kind: "Pod",
            resource: "cpu/memory",
            usage: 0,
            formattedUsage: "—",
            request: null,
            formattedRequest: "—",
            limit: null,
            formattedLimit: "—",
            percent: null,
            wasted: null,
            formattedWasted: "—",
            label: "No requests or limits configured",
          },
        ],
      },
    });

    await renderWith(rsData);

    const rsTab = screen.getByRole("tab", { name: "Right-sizing & Efficiency" });
    await user.click(rsTab);

    expect(screen.getByTestId("right-sizing-view")).toBeInTheDocument();
    expect(screen.getByText("pod-near-lim")).toBeInTheDocument();
    expect(screen.getByText("92% limit · OOMKill risk")).toBeInTheDocument();
    expect(screen.getByText("pod-over-req")).toBeInTheDocument();
    expect(screen.getByText("130% of request")).toBeInTheDocument();
    expect(screen.getByText("pod-unused")).toBeInTheDocument();
    expect(screen.getByText("950m wasted (5% used)")).toBeInTheDocument();
    expect(screen.getByText("pod-missing")).toBeInTheDocument();
    expect(screen.getByText("No requests or limits configured")).toBeInTheDocument();

    // Clicking item opens drawer
    await user.click(screen.getByText("pod-near-lim"));
    expect(useResourceDetailStore.getState().target).toMatchObject({
      kind: "Pod",
      namespace: "default",
      name: "pod-near-lim",
    });
  });

  it("renders honest notice when metrics-server is not installed in Right-sizing tab", async () => {
    const user = userEvent.setup();
    const rsNoMetrics = overview({
      rightSizing: {
        metricsAvailable: false,
        notice: "metrics-server not installed: right-sizing requires live container metrics from metrics.k8s.io",
        thresholdPct: 20,
        nearLimit: [],
        overRequest: [],
        unusedRequests: [],
        missingResources: [],
      },
    });

    await renderWith(rsNoMetrics);
    await user.click(screen.getByRole("tab", { name: "Right-sizing & Efficiency" }));

    expect(screen.getByRole("status")).toHaveTextContent("metrics-server not installed");
    expect(screen.getByText("No workloads near limit")).toBeInTheDocument();
  });
});

describe("filterWarnings", () => {
  it("drops events without a timestamp and keeps the boundary", () => {
    const events = [
      warning("a", 60),
      warning("b", 61),
      warning("c", 0, { lastTimestamp: null }),
    ];
    expect(filterWarnings(events, "1h", NOW).map((e) => e.involvedName)).toEqual(["a"]);
    expect(filterWarnings(events, "6h", NOW).map((e) => e.involvedName)).toEqual(["a", "b"]);
  });
});
