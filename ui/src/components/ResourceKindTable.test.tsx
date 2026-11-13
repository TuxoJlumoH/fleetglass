import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { ContextSettings_Serialize } from "../generated/bindings";
import { resetActionsStore, useActionsStore } from "../stores/actions";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { podsLayoutState } from "../test/layout";
import {
  type Page,
  type QuerySpec,
  type Row,
  type StoreAccessState,
  resetPodsStore,
  setQueryFetcher,
} from "../stores/pods";
import { resetResourceDetailStore, useResourceDetailStore } from "../stores/resourceDetail";
import { PodTable } from "./PodTable";
import { TabsArea } from "./TabsArea";

// Cell order mirrors `project_deployment` in crates/core/src/projection.rs.
function deploymentRow(name: string, ready: number, desired: number): Row {
  return {
    uid: `uid-deploy-${name}`,
    cluster: "kind-dev",
    kind: "Deployment",
    namespace: "fg-demo",
    name,
    status: ready === desired ? "healthy" : "pending",
    statusReason: ready === desired ? "Available" : "Progressing",
    cells: [
      { type: "ready", value: { ready, total: desired } },
      { type: "int", value: desired },
      { type: "int", value: ready },
      {
        type: "status",
        value: {
          label: ready === desired ? "Available" : "Progressing",
          phase: ready === desired ? "healthy" : "pending",
        },
      },
      { type: "timestamp", value: "2026-11-01T10:00:00Z" },
    ],
    labels: [["app", name]],
    custom: [],
  };
}

function podRow(name: string): Row {
  return {
    uid: `uid-pod-${name}`,
    cluster: "kind-dev",
    kind: "Pod",
    namespace: "fg-demo",
    name,
    status: "healthy",
    statusReason: "Running",
    cells: [
      { type: "ready", value: { ready: 1, total: 1 } },
      { type: "status", value: { label: "Running", phase: "healthy" } },
      { type: "int", value: 0 },
      { type: "text", value: "node-1" },
      { type: "text", value: "10.244.0.5" },
      { type: "timestamp", value: "2026-11-01T10:00:00Z" },
    ],
    labels: [],
    custom: [],
  };
}

const DEPLOYMENTS = [deploymentRow("api", 1, 2), deploymentRow("web", 2, 2)];
const PODS = [podRow("web-abc"), podRow("web-def"), podRow("api-xyz")];

/** Fake query engine: answers by `spec.kinds`, like `query_page`. */
function pageFor(spec: QuerySpec, access?: StoreAccessState): Page {
  const kind = spec.kinds[0] ?? "Pod";
  const rows = access !== undefined ? [] : kind === "Deployment" ? DEPLOYMENTS : kind === "Pod" ? PODS : [];
  return {
    total: rows.length,
    offset: spec.offset,
    limit: spec.limit,
    rows: rows.slice(spec.offset, spec.offset + spec.limit),
    version: 1,
    access: [{ cluster: "kind-dev", kind, access: access ?? { state: "ready" }, rows: 0, metrics: null }],
    statusCounts: { healthy: 0, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
  };
}

describe("R-16 resource tabs list the tab's kind", () => {
  let specs: QuerySpec[];

  beforeEach(() => {
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
    useLayoutStore.setState(podsLayoutState);
    useContextsStore.setState({ ...initialContextsState, selectedContext: "kind-dev" });
    specs = [];
    setQueryFetcher((spec) => {
      specs.push(spec);
      return Promise.resolve(pageFor(spec));
    });
  });

  afterEach(() => {
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
    useLayoutStore.setState(podsLayoutState);
  });

  it("a Deployments tab queries kinds ['Deployment'] and renders deployment columns and rows", async () => {
    act(() => {
      useLayoutStore.getState().openTab({ title: "Deployments", kind: "Deployment", view: "resources" });
    });
    render(<TabsArea />);

    const table = await screen.findByRole("table", { name: "Deployments" });
    expect(await within(table).findByText("web")).toBeInTheDocument();
    expect(specs.length).toBeGreaterThan(0);
    for (const spec of specs) expect(spec.kinds).toEqual(["Deployment"]);

    const headers = within(table)
      .getAllByRole("columnheader")
      .map((h) => h.textContent.replace(/[▲▼]/g, ""));
    expect(headers).toEqual([
      "Cluster",
      "Namespace",
      "Name",
      "Ready",
      "Up-to-date",
      "Available",
      "Status",
      "Age",
    ]);
    const webRow = within(table).getByText("web").closest<HTMLElement>('[role="row"]');
    if (!webRow) throw new Error("web row not rendered");
    expect(within(webRow).getAllByRole("cell").map((c) => c.textContent)).toEqual([
      "kind-dev",
      "fg-demo",
      "web",
      "2/2",
      "2",
      "2",
      "Available",
      expect.stringMatching(/^\d+[smhdy]$/),
    ]);
    expect(within(webRow).getAllByRole("cell").at(-1)).toHaveAttribute("title", "2026-11-01T10:00:00Z");
    expect(within(table).getByText("Progressing")).toBeInTheDocument();
    expect(screen.getByTestId("pod-total")).toHaveTextContent("2 deployments");
    expect(screen.getByLabelText("Filter deployments")).toBeInTheDocument();
    // No pod columns or pod rows leak into the Deployments tab.
    expect(within(table).queryByRole("columnheader", { name: /Restarts/ })).toBeNull();
    expect(screen.queryByText("web-abc")).toBeNull();
  });

  it("switching tabs re-queries with the other tab's kind", async () => {
    const deployTab = useLayoutStore.getState().openTab({ title: "Deployments", kind: "Deployment" });
    render(<TabsArea />);
    expect(await screen.findByRole("table", { name: "Deployments" })).toBeInTheDocument();

    act(() => {
      useLayoutStore.getState().setActiveTab("pods");
    });
    const podsTable = await screen.findByRole("table", { name: "Pods" });
    expect(await within(podsTable).findByText("web-abc")).toBeInTheDocument();
    expect(specs.at(-1)?.kinds).toEqual(["Pod"]);
    expect(screen.getByTestId("pod-total")).toHaveTextContent("3 pods");

    act(() => {
      useLayoutStore.getState().setActiveTab(deployTab);
    });
    const deployTable = await screen.findByRole("table", { name: "Deployments" });
    expect(await within(deployTable).findByText("api")).toBeInTheDocument();
    expect(specs.at(-1)?.kinds).toEqual(["Deployment"]);
    expect(screen.getByTestId("pod-total")).toHaveTextContent("2 deployments");
  });

  it("changing kind resets a cell sort, since cell indexes mean other columns per kind", async () => {
    const { rerender } = render(<PodTable kind="Pod" />);
    await screen.findByText("web-abc");
    fireEvent.click(screen.getByRole("columnheader", { name: /Restarts/ }));
    await waitFor(() => {
      expect(specs.at(-1)?.sort).toEqual({ field: { type: "cell", index: 2 }, direction: "asc" });
    });

    rerender(<PodTable kind="Deployment" />);
    expect(await screen.findByText("web")).toBeInTheDocument();
    expect(specs.at(-1)).toMatchObject({
      kinds: ["Deployment"],
      sort: { field: { type: "name" }, direction: "asc" },
    });
  });

  it("Enter and double-click open the drawer for the Deployment row", async () => {
    render(<PodTable kind="Deployment" />);
    const table = await screen.findByRole("table", { name: "Deployments" });
    const webRow = (await within(table).findByText("web")).closest<HTMLElement>('[role="row"]');
    if (!webRow) throw new Error("web row not rendered");

    fireEvent.doubleClick(webRow);
    expect(useResourceDetailStore.getState().target).toMatchObject({
      kind: "Deployment",
      name: "web",
    });

    act(() => {
      useResourceDetailStore.getState().close();
    });
    const apiRow = within(table).getByText("api").closest<HTMLElement>('[role="row"]');
    if (!apiRow) throw new Error("api row not rendered");
    apiRow.focus();
    fireEvent.keyDown(apiRow, { key: "Enter" });
    expect(useResourceDetailStore.getState().target).toMatchObject({
      kind: "Deployment",
      name: "api",
    });
  });

  it.each<[string, StoreAccessState, string]>([
    [
      "forbidden",
      {
        state: "forbidden",
        resource: "deployments",
        namespace: null,
        message: "deployments is forbidden",
      },
      "No permission to list deployments in kind-dev",
    ],
    [
      "error",
      { state: "error", message: "connection refused", partialRows: null },
      "Cannot list deployments in kind-dev: connection refused",
    ],
    ["loading", { state: "loading", rowsSoFar: 0 }, "Connecting to kind-dev…"],
    [
      "reconnecting",
      { state: "reconnecting" },
      // No rows held: no "(showing last known rows)" claim (R-14d).
      "Reconnecting to kind-dev…",
    ],
  ])("shows the %s state for deployments, never an empty table", async (_name, access, text) => {
    setQueryFetcher((spec) => {
      specs.push(spec);
      return Promise.resolve(pageFor(spec, access));
    });
    render(<PodTable kind="Deployment" />);
    expect(await screen.findByText(text)).toBeInTheDocument();
    expect(screen.queryByRole("table", { name: "Deployments" })).toBeNull();
    expect(screen.queryByText("No deployments")).toBeNull();
  });

  it("labels deployments received so far during the initial list, never an empty state", async () => {
    setQueryFetcher((spec) => {
      specs.push(spec);
      const page = pageFor(spec);
      return Promise.resolve({
        ...page,
        access: [{ cluster: "kind-dev", kind: "Deployment", access: { state: "loading", rowsSoFar: 2 }, rows: 0, metrics: null }],
      });
    });
    render(<PodTable kind="Deployment" />);

    const table = await screen.findByRole("table", { name: "Deployments" });
    expect(await within(table).findByText("web")).toBeInTheDocument();
    expect(screen.getByTestId("pod-loading-progress")).toHaveTextContent(
      "Loading… 2 rows so far (kind-dev)",
    );
    expect(screen.queryByText("No deployments")).toBeNull();
    expect(screen.queryByText("Connecting to kind-dev…")).toBeNull();
    for (const spec of specs) expect(spec.kinds).toEqual(["Deployment"]);
  });

  it("says 'No deployments' when the Ready store is empty", async () => {
    setQueryFetcher((spec) =>
      Promise.resolve({ ...pageFor(spec), total: 0, rows: [] }),
    );
    render(<PodTable kind="Deployment" />);
    expect(await screen.findByText("No deployments")).toHaveAttribute("role", "status");
    expect(screen.getByTestId("pod-total")).toHaveTextContent("0 deployments");
  });
});

describe("R-16 keyboard actions use the row's real kind", () => {
  const UNPROTECTED: ContextSettings_Serialize = {
    colour: null,
    tags: [],
    readOnly: false,
    protected: false,
    pinned: false,
  };
  let calls: { cmd: string; args: Record<string, unknown> }[];

  beforeEach(() => {
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
    useLayoutStore.setState(podsLayoutState);
    useContextsStore.setState({ ...initialContextsState, selectedContext: "kind-dev" });
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    calls = [];
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });
      switch (cmd) {
        case "query_page":
          return pageFor(a.spec as QuerySpec);
        case "subscribe_query":
          return 1;
        case "unsubscribe_query":
          return true;
        case "preview_kubectl_action": {
          const act = a.action as { type: string; kind: string; namespace: string; name: string };
          return `kubectl --context ${String(a.context)} -n ${act.namespace} ${act.type} ${act.kind.toLowerCase()} ${act.name}`;
        }
        case "get_context_settings":
          return UNPROTECTED;
        default:
          return undefined;
      }
    });
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
  });

  async function focusRow(name: string): Promise<HTMLElement> {
    const table = await screen.findByRole("table", { name: "Deployments" });
    const row = (await within(table).findByText(name)).closest<HTMLElement>('[role="row"]');
    if (!row) throw new Error(`row ${name} not rendered`);
    fireEvent.click(row);
    row.focus();
    return row;
  }

  it("`s` on a Deployment row opens the scale dialog at its desired replicas", async () => {
    render(<PodTable kind="Deployment" />);
    const row = await focusRow("web");

    fireEvent.keyDown(row, { key: "s" });

    const dialog = await screen.findByRole("dialog", { name: "Scale deployment fg-demo/web" });
    expect(within(dialog).getByLabelText("Replicas")).toHaveValue("2");
    expect(useActionsStore.getState().pending?.action).toEqual({
      type: "scale",
      kind: "Deployment",
      namespace: "fg-demo",
      name: "web",
      replicas: 2,
    });
    expect(screen.queryByTestId("action-status")).toBeNull();
    const subscribe = calls.find((c) => c.cmd === "subscribe_query");
    expect((subscribe?.args.spec as QuerySpec | undefined)?.kinds).toEqual(["Deployment"]);
  });

  it("`r` on a Deployment row opens the restart dialog", async () => {
    render(<PodTable kind="Deployment" />);
    const row = await focusRow("api");

    fireEvent.keyDown(row, { key: "r" });

    await screen.findByRole("dialog", { name: /Restart deployment fg-demo\/api/i });
    expect(useActionsStore.getState().pending?.action).toEqual({
      type: "restart",
      kind: "Deployment",
      namespace: "fg-demo",
      name: "api",
    });
  });
});
