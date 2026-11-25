import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { Channel } from "@tauri-apps/api/core";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PageDelta_Deserialize, QuerySpec_Deserialize } from "../generated/bindings";
import { clustersForQuery, initialContextsState, useContextsStore } from "../stores/contexts";
import {
  type Page,
  type QuerySpec,
  type Row,
  type StoreAccess,
  type StoreAccessState,
  resetPodsStore,
  setQueryFetcher,
  usePodsStore,
} from "../stores/pods";
import { PodTable } from "./PodTable";
import { accessNotice, restrictedScope } from "./podAccess";

function makeRow(cluster: string, name: string): Row {
  return {
    uid: `${cluster}/${name}`,
    cluster,
    kind: "Pod",
    namespace: "default",
    name,
    status: "healthy",
    statusReason: "Running",
    cells: [
      { type: "ready", value: { ready: 1, total: 1 } },
      { type: "status", value: { label: "Running", phase: "healthy" } },
      { type: "int", value: 0 },
      { type: "text", value: "node-0" },
      { type: "text", value: "10.0.0.1" },
      { type: "timestamp", value: "2026-11-01T00:00:00Z" },
    ],
    labels: [],
    custom: [],
  };
}

function podAccess(cluster: string, access: StoreAccessState, rows = 0): StoreAccess {
  return { cluster, kind: "Pod", access, rows, metrics: null };
}

function makePage(spec: QuerySpec, rows: Row[], access: StoreAccess[], version = 1): Page {
  return {
    total: rows.length,
    offset: spec.offset,
    limit: spec.limit,
    rows: rows.slice(spec.offset, spec.offset + spec.limit),
    version,
    access,
    statusCounts: { healthy: 0, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
  };
}

function selectContext(name: string | null) {
  act(() => {
    useContextsStore.setState({ selectedContext: name });
  });
}

beforeEach(() => {
  resetPodsStore();
  useContextsStore.setState(initialContextsState);
});

afterEach(() => {
  resetPodsStore();
  clearMocks();
  delete (globalThis as { isTauri?: boolean }).isTauri;
});

describe("clustersForQuery", () => {
  it("returns no clusters when nothing is selected", () => {
    expect(clustersForQuery({ activeClusterSelection: [], selectedContext: null })).toEqual([]);
  });

  it("uses the selected context when no cluster group is active", () => {
    expect(clustersForQuery({ activeClusterSelection: [], selectedContext: "dev" })).toEqual([
      "dev",
    ]);
  });

  it("prefers the active cluster selection (group / multi-select), deduplicated", () => {
    expect(
      clustersForQuery({
        activeClusterSelection: ["a", "b", "a"],
        selectedContext: "dev",
      }),
    ).toEqual(["a", "b"]);
  });
});

describe("PodTable clusters derivation and requery", () => {
  it("shows 'Select a context' and sends no query when nothing is selected", async () => {
    const fetcher = vi.fn((spec: QuerySpec) => Promise.resolve(makePage(spec, [], [])));
    setQueryFetcher(fetcher);

    render(<PodTable />);

    expect(await screen.findByRole("status")).toHaveTextContent("Select a context to list pods");
    expect(fetcher).not.toHaveBeenCalled();
    expect(screen.queryByRole("table", { name: "Pods" })).toBeNull();
  });

  it("queries the selected context and re-queries when the selection changes", async () => {
    const specs: QuerySpec[] = [];
    setQueryFetcher((spec) => {
      specs.push(spec);
      const rows = spec.clusters.map((c) => makeRow(c, `${c}-pod`));
      return Promise.resolve(
        makePage(
          spec,
          rows,
          spec.clusters.map((c) => podAccess(c, { state: "ready" })),
        ),
      );
    });
    selectContext("dev");

    render(<PodTable />);
    expect(await screen.findByText("dev-pod")).toBeInTheDocument();
    expect(specs.at(-1)?.clusters).toEqual(["dev"]);

    // Selecting another context in the sidebar re-queries from offset 0.
    selectContext("staging");
    expect(await screen.findByText("staging-pod")).toBeInTheDocument();
    expect(screen.queryByText("dev-pod")).toBeNull();
    expect(specs.at(-1)).toMatchObject({ clusters: ["staging"], offset: 0 });

    // A cluster group wins over the selected context.
    act(() => {
      useContextsStore.setState({ activeClusterSelection: ["dev", "prod"] });
    });
    expect(await screen.findByText("prod-pod")).toBeInTheDocument();
    expect(screen.getByText("dev-pod")).toBeInTheDocument();
    expect(specs.at(-1)?.clusters).toEqual(["dev", "prod"]);

    // An unrelated contexts-store change does not re-query.
    const before = specs.length;
    act(() => {
      useContextsStore.getState().setConnectionStatus("dev", { state: "connected" });
    });
    await act(() => Promise.resolve());
    expect(specs.length).toBe(before);

    // Clearing everything goes back to the prompt.
    act(() => {
      useContextsStore.setState({ activeClusterSelection: [], selectedContext: null });
    });
    expect(await screen.findByText("Select a context to list pods")).toBeInTheDocument();
    expect(usePodsStore.getState().querySpec.clusters).toEqual([]);
  });

  it("keeps the text filter and sort when the clusters change", async () => {
    const specs: QuerySpec[] = [];
    setQueryFetcher((spec) => {
      specs.push(spec);
      return Promise.resolve(makePage(spec, [], [podAccess("dev", { state: "ready" })]));
    });
    selectContext("dev");
    render(<PodTable />);
    await waitFor(() => {
      expect(specs.length).toBeGreaterThan(0);
    });
    await act(() => usePodsStore.getState().setTextFilter("web"));

    selectContext("staging");
    await waitFor(() => {
      expect(specs.at(-1)?.clusters).toEqual(["staging"]);
    });
    expect(specs.at(-1)).toMatchObject({
      textFilter: "web",
      sort: { field: { type: "name" }, direction: "asc" },
    });
  });
});

describe("PodTable access states", () => {
  const cases: { name: string; access: StoreAccessState; role: "status" | "alert"; text: string }[] =
    [
      { name: "loading", access: { state: "loading", rowsSoFar: 0 }, role: "status", text: "Connecting to dev…" },
      {
        name: "reconnecting",
        access: { state: "reconnecting" },
        role: "status",
        text: "Reconnecting to dev…",
      },
      {
        name: "forbidden",
        access: {
          state: "forbidden",
          resource: "pods",
          namespace: null,
          message: "No permission to list pods",
        },
        role: "alert",
        text: "No permission to list pods in dev",
      },
      {
        name: "forbidden in a namespace",
        access: {
          state: "forbidden",
          resource: "pods",
          namespace: "team-a",
          message: 'No permission to list pods in namespace "team-a"',
        },
        role: "alert",
        text: "No permission to list pods in dev (namespace team-a)",
      },
      {
        name: "error",
        access: { state: "error", message: "connection refused", partialRows: null },
        role: "alert",
        text: "Cannot list pods in dev: connection refused",
      },
      {
        name: "unsupported",
        access: {
          state: "unsupported",
          kind: "Pod",
          message: "Live listing of Pod is not supported yet",
        },
        role: "status",
        text: "Live listing of Pod is not supported yet (dev)",
      },
      {
        name: "notFound",
        access: {
          state: "notFound",
          resource: "pods",
          message: "pods API is not installed on this cluster",
        },
        role: "status",
        text: "The pods API is not installed in dev",
      },
      {
        name: "authExpired",
        access: {
          state: "authExpired",
          provider: "aws-sso",
          hint: "Run 'aws sso login' to re-authenticate",
          loginCommand: "aws sso login",
          deviceCode: null,
          partialRows: null,
        },
        role: "alert",
        text: "Authentication expired for dev: Run 'aws sso login' to re-authenticate",
      },
    ];

  it.each(cases)("$name → $text", async ({ access, role, text }) => {
    setQueryFetcher((spec) => Promise.resolve(makePage(spec, [], [podAccess("dev", access)])));
    selectContext("dev");

    render(<PodTable />);

    const notice = await screen.findByText(text);
    expect(notice).toHaveAttribute("role", role);
    // Never an empty table or a "0 pods / No pods" claim for a store that is not Ready.
    expect(screen.queryByRole("table", { name: "Pods" })).toBeNull();
    expect(screen.queryByText("No pods")).toBeNull();
  });

  it("says 'Connecting' before the first page arrives", async () => {
    setQueryFetcher(() => new Promise<Page>(() => undefined));
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByText("Connecting to dev…")).toHaveAttribute("role", "status");
  });

  it("shows a failed query as an alert, not as 'Connecting' or 'No pods'", async () => {
    setQueryFetcher(() => Promise.reject(new Error("Command query_page not allowed by ACL")));
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Command query_page not allowed by ACL",
    );
    expect(screen.queryByText("Connecting to dev…")).toBeNull();
    expect(screen.queryByText("No pods")).toBeNull();
  });

  it("shows 'No pods' only when every queried store is Ready and empty", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(makePage(spec, [], [podAccess("dev", { state: "ready" })])),
    );
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByText("No pods")).toHaveAttribute("role", "status");
  });

  it("shows notice when metrics-server is notInstalled", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(
        makePage(spec, [], [
          {
            cluster: "dev",
            kind: "Pod",
            access: { state: "ready" },
            rows: 0,
            metrics: { state: "notInstalled", message: "metrics-server not installed" },
          },
        ]),
      ),
    );
    selectContext("dev");

    render(<PodTable />);

    const notice = await screen.findByText(
      "metrics-server not installed — CPU and memory are unavailable",
    );
    expect(notice).toHaveAttribute("role", "status");
    expect(notice).toHaveClass("pod-table__notice--status");
  });

  it("shows notice when metrics access is forbidden", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(
        makePage(spec, [], [
          {
            cluster: "dev",
            kind: "Pod",
            access: { state: "ready" },
            rows: 0,
            metrics: { state: "forbidden", message: "forbidden" },
          },
        ]),
      ),
    );
    selectContext("dev");

    render(<PodTable />);

    const notice = await screen.findByText("No permission to read metrics");
    expect(notice).toHaveAttribute("role", "alert");
    expect(notice).toHaveClass("pod-table__notice--alert");
  });

  it("labels a partial list during the initial list with the row count", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve({
        ...makePage(
          spec,
          [makeRow("dev", "web-0"), makeRow("dev", "web-1")],
          [podAccess("dev", { state: "loading", rowsSoFar: 1500 })],
        ),
        total: 1500,
      }),
    );
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByText("web-0")).toBeInTheDocument();
    expect(screen.getByRole("table", { name: "Pods" })).toBeVisible();
    expect(screen.getByTestId("pod-loading-progress")).toHaveTextContent(
      "Loading… 1,500 rows so far (dev)",
    );
    expect(screen.getByTestId("pod-loading-progress")).toHaveAttribute("role", "status");
    // Rows are shown, so no "Connecting" notice.
    expect(screen.queryByText("Connecting to dev…")).toBeNull();
  });

  it("keeps the last known rows of a reconnecting store, labelled, not 'Connecting'", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(
        makePage(
          spec,
          [makeRow("dev", "web-0"), makeRow("dev", "web-1")],
          [podAccess("dev", { state: "reconnecting" }, 2)],
        ),
      ),
    );
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByText("web-0")).toBeInTheDocument();
    expect(screen.getByRole("table", { name: "Pods" })).toBeVisible();
    expect(screen.getByText("Reconnecting to dev… (showing last known rows)")).toHaveAttribute(
      "role",
      "status",
    );
    expect(screen.queryByText("Connecting to dev…")).toBeNull();
    expect(screen.queryByTestId("pod-loading-progress")).toBeNull();
  });

  it("R-14d: a synced store that fails labels its kept rows as last known", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(
        makePage(
          spec,
          [makeRow("dev", "web-0")],
          [
            podAccess(
              "dev",
              { state: "forbidden", resource: "pods", namespace: null, message: "denied" },
              1,
            ),
          ],
        ),
      ),
    );
    selectContext("dev");
    render(<PodTable />);
    expect(
      await screen.findByText(
        "No permission to list pods in dev — showing last known rows (may be outdated)",
      ),
    ).toHaveAttribute("role", "alert");
  });

  it("05-01: a restricted store shows a status pill, not an error banner", async () => {
    const restricted: StoreAccess = {
      ...podAccess("dev", { state: "ready" }, 2),
      scope: {
        namespaces: ["api", "web", "jobs"].map((namespace) => ({
          namespace,
          access: { state: "ready" },
        })),
        source: "configured",
        denied: ["prod"],
        truncated: false,
      },
    };
    setQueryFetcher((spec) =>
      Promise.resolve(
        makePage(spec, [makeRow("dev", "web-0"), makeRow("dev", "web-1")], [restricted]),
      ),
    );
    selectContext("dev");
    render(<PodTable />);
    const pill = await screen.findByTestId("restricted-scope");
    expect(pill).toHaveAttribute("role", "status");
    expect(within(pill).getByText("Restricted: 3 namespaces (cluster-wide watch forbidden)"))
      .toHaveClass("pill", "warn");
    expect(pill).toHaveAttribute("title", expect.stringContaining("api, web, jobs"));
    expect(pill).toHaveAttribute("title", expect.stringContaining("Not permitted: prod."));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("05-01: a single restricted namespace is named; no scope means no pill", () => {
    const one = restrictedScope({
      ...podAccess("dev", { state: "ready" }),
      scope: {
        namespaces: [{ namespace: "dev", access: { state: "ready" } }],
        source: "contextNamespace",
        denied: [],
        truncated: true,
      },
    });
    expect(one?.label).toBe("Restricted: namespace dev (cluster-wide watch forbidden)");
    expect(one?.detail).toContain("the kubeconfig context's namespace");
    expect(one?.detail).toContain("More namespaces exist");
    expect(restrictedScope(podAccess("dev", { state: "ready" }))).toBeNull();
  });

  it("R-14d: a partial first list keeps its own label, not 'last known'", () => {
    const notice = accessNotice(
      podAccess("dev", { state: "error", message: "timeout", partialRows: 3 }, 3),
    );
    expect(notice?.text).toContain("received before the error (incomplete)");
    expect(notice?.text).not.toContain("last known");
    expect(
      accessNotice(podAccess("dev", { state: "error", message: "timeout", partialRows: null }, 4))
        ?.text,
    ).toBe("Cannot list pods in dev: timeout — showing last known rows (may be outdated)");
  });

  it("sums loading rows across clusters and names each one", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(
        makePage(
          spec,
          [makeRow("a", "web-0"), makeRow("b", "web-1"), makeRow("c", "web-2")],
          [
            podAccess("a", { state: "loading", rowsSoFar: 500 }),
            podAccess("b", { state: "loading", rowsSoFar: 1000 }),
            podAccess("c", { state: "ready" }),
          ],
        ),
      ),
    );
    act(() => {
      useContextsStore.setState({ activeClusterSelection: ["a", "b", "c"] });
    });

    render(<PodTable />);

    expect(await screen.findByTestId("pod-loading-progress")).toHaveTextContent(
      "Loading… 1,500 rows so far (a: 500, b: 1,000)",
    );
  });

  it("never claims 'No pods' while the initial list is loading", async () => {
    // e.g. a namespace filter that matches none of the rows received so far.
    setQueryFetcher((spec) =>
      Promise.resolve(makePage(spec, [], [podAccess("dev", { state: "loading", rowsSoFar: 800 })])),
    );
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByTestId("pod-loading-progress")).toHaveTextContent(
      "Loading… 800 rows so far (dev)",
    );
    expect(screen.queryByText(/No pods/)).toBeNull();
  });

  it("labels rows kept after a failed initial list as incomplete", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve({
        ...makePage(
          spec,
          [makeRow("dev", "web-0")],
          [podAccess("dev", { state: "error", message: "list timed out", partialRows: 1500 })],
        ),
        total: 1500,
      }),
    );
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByText("web-0")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Cannot list pods in dev: list timed out — showing 1,500 rows received before the error (incomplete)",
      ),
    ).toHaveAttribute("role", "alert");
    expect(screen.queryByTestId("pod-loading-progress")).toBeNull();
  });

  it("removes the banner once the store is Ready", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(makePage(spec, [makeRow("dev", "web-0")], [podAccess("dev", { state: "ready" })])),
    );
    selectContext("dev");

    render(<PodTable />);

    expect(await screen.findByText("web-0")).toBeInTheDocument();
    expect(screen.queryByTestId("pod-loading-progress")).toBeNull();
  });

  it("mixed clusters: rows from the Ready ones plus a banner per other cluster", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve(
        makePage(
          spec,
          [makeRow("dev", "web-0"), makeRow("dev", "web-1")],
          [
            podAccess("dev", { state: "ready" }),
            podAccess("locked", {
              state: "forbidden",
              resource: "pods",
              namespace: null,
              message: "No permission to list pods",
            }),
            podAccess("slow", { state: "loading", rowsSoFar: 0 }),
          ],
        ),
      ),
    );
    act(() => {
      useContextsStore.setState({ activeClusterSelection: ["dev", "locked", "slow"] });
    });

    render(<PodTable />);

    expect(await screen.findByText("web-0")).toBeInTheDocument();
    expect(screen.getByText("web-1")).toBeInTheDocument();
    expect(screen.getByRole("table", { name: "Pods" })).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("No permission to list pods in locked");
    expect(screen.getByRole("status")).toHaveTextContent("Connecting to slow…");
    const notices = screen.getByTestId("pod-cluster-notices");
    expect(notices.children).toHaveLength(2);
    expect(within(notices).queryByText(/dev/)).toBeNull();
  });
});

describe("PodTable against the IPC commands (mockIPC)", () => {
  interface Calls {
    queries: QuerySpec_Deserialize[];
    subscribes: { spec: QuerySpec_Deserialize; channel: Channel<PageDelta_Deserialize> }[];
    unsubscribes: number[];
  }

  function mockBackend(pageFor: (spec: QuerySpec_Deserialize) => Page): Calls {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    const calls: Calls = { queries: [], subscribes: [], unsubscribes: [] };
    mockIPC((cmd, args) => {
      const a = args as Record<string, unknown>;
      if (cmd === "query_page") {
        const spec = a.spec as QuerySpec_Deserialize;
        calls.queries.push(spec);
        return pageFor(spec);
      }
      if (cmd === "subscribe_query") {
        calls.subscribes.push({
          spec: a.spec as QuerySpec_Deserialize,
          channel: a.onDelta as Channel<PageDelta_Deserialize>,
        });
        return calls.subscribes.length;
      }
      if (cmd === "unsubscribe_query") {
        calls.unsubscribes.push(a.subscriptionId as number);
        return true;
      }
      return undefined;
    });
    return calls;
  }

  function push(channel: Channel<PageDelta_Deserialize>, index: number, page: Page) {
    act(() => {
      const internals = (
        window as unknown as {
          __TAURI_INTERNALS__: { runCallback: (id: number, data: unknown) => void };
        }
      ).__TAURI_INTERNALS__;
      internals.runCallback(channel.id, { index, message: { subscriptionId: 1, page } });
    });
  }

  it("sends the selected context to query_page / subscribe_query and follows live access deltas", async () => {
    const calls = mockBackend((spec) =>
      makePage(spec as QuerySpec, [], [podAccess("fleetglass-dev", { state: "loading", rowsSoFar: 0 })]),
    );
    selectContext("fleetglass-dev");

    render(<PodTable />);

    expect(await screen.findByText("Connecting to fleetglass-dev…")).toBeInTheDocument();
    expect(calls.queries[0]?.clusters).toEqual(["fleetglass-dev"]);
    await waitFor(() => {
      expect(calls.subscribes).toHaveLength(1);
    });
    const first = calls.subscribes[0];
    if (first === undefined) throw new Error("no subscription");
    expect(first.spec).toMatchObject({ clusters: ["fleetglass-dev"], kinds: ["Pod"], offset: 0 });

    // The watcher finishes its initial list: the delta brings Ready plus rows.
    const live = makePage(
      first.spec as QuerySpec,
      [makeRow("fleetglass-dev", "nginx-0")],
      [podAccess("fleetglass-dev", { state: "ready" })],
      2,
    );
    push(first.channel, 0, live);
    expect(await screen.findByText("nginx-0")).toBeInTheDocument();
    expect(screen.queryByText("Connecting to fleetglass-dev…")).toBeNull();
    expect(screen.getByTestId("pod-total")).toHaveTextContent(/^1 pod$/);

    // Switching context closes the old subscription and opens one for the new cluster.
    selectContext("other");
    await waitFor(() => {
      expect(calls.subscribes).toHaveLength(2);
    });
    expect(calls.unsubscribes).toEqual([1]);
    expect(calls.subscribes[1]?.spec.clusters).toEqual(["other"]);
    expect(calls.queries.at(-1)?.clusters).toEqual(["other"]);

    // Late deltas from the closed subscription are ignored.
    push(first.channel, 1, live);
    expect(screen.queryByText("nginx-0")).toBeNull();
  });

  it("does not call the backend when no context is selected", async () => {
    const calls = mockBackend((spec) => makePage(spec as QuerySpec, [], []));

    render(<PodTable />);

    expect(await screen.findByText("Select a context to list pods")).toBeInTheDocument();
    expect(calls.queries).toEqual([]);
    expect(calls.subscribes).toEqual([]);
  });
});
