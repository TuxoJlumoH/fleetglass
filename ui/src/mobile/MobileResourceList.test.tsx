import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { resetActionsStore, useActionsStore } from "../stores/actions";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import {
  PAGE_SIZE,
  type Page,
  type QuerySpec,
  type Row,
  resetPodsStore,
  setQueryFetcher,
  usePodsStore,
} from "../stores/pods";
import { resetResourceDetailStore, useResourceDetailStore } from "../stores/resourceDetail";
import { MOBILE_ROW_HEIGHT, MobileResourceList, PULL_THRESHOLD } from "./MobileResourceList";

const CREATED = new Date(Date.now() - 2 * 86_400_000).toISOString();

type Cell = Row["cells"][number];

const usage = (formatted: string, value: number): Cell => ({
  type: "usage",
  value: { value, formatted, request: value * 2, limit: null },
});

/** Pod row in the KIND_LAYOUTS.pod cell order (core `projection.rs`). */
function podRow(i: number, patch: Partial<Row> & { restarts?: number; metrics?: boolean } = {}): Row {
  const { restarts = 0, metrics = true, ...rowPatch } = patch;
  const failed = rowPatch.status === "failed";
  const label = rowPatch.statusReason ?? (failed ? "CrashLoopBackOff" : "Running");
  return {
    uid: `uid-${String(i)}`,
    cluster: "kind-a",
    kind: "Pod",
    namespace: "shop",
    name: `web-${String(i).padStart(4, "0")}`,
    status: "healthy",
    statusReason: label,
    cells: [
      { type: "ready", value: { ready: failed ? 0 : 1, total: 1 } },
      { type: "status", value: { label, phase: rowPatch.status ?? "healthy" } },
      { type: "int", value: restarts },
      { type: "timestamp", value: "" },
      { type: "text", value: "" },
      { type: "text", value: "node-1" },
      { type: "text", value: "10.244.0.10" },
      { type: "timestamp", value: CREATED },
      metrics ? usage("12m", 12) : { type: "usage", value: { value: 0, formatted: "—", request: null, limit: null } },
      metrics ? usage("64Mi", 64) : { type: "usage", value: { value: 0, formatted: "—", request: null, limit: null } },
    ],
    labels: [],
    custom: [],
    ...rowPatch,
  };
}

function page(spec: QuerySpec, total: number, make: (i: number) => Row = podRow): Page {
  const rows: Row[] = [];
  for (let i = spec.offset; i < Math.min(total, spec.offset + spec.limit); i += 1) rows.push(make(i));
  return {
    total,
    offset: spec.offset,
    limit: spec.limit,
    rows,
    version: 1,
    access: spec.clusters.map((cluster) => ({
      cluster,
      kind: spec.kinds[0] ?? "Pod",
      access: { state: "ready" },
      rows: total,
      metrics: null,
    })),
    statusCounts: { healthy: total - 3, pending: 1, degraded: 0, failed: 2, completed: 0, terminating: 0, unknown: 0 },
  };
}

function installFetcher(total = 500, make?: (i: number) => Row) {
  const specs: QuerySpec[] = [];
  const fetcher = vi.fn((spec: QuerySpec) => {
    specs.push(spec);
    return Promise.resolve(page(spec, total, make));
  });
  setQueryFetcher(fetcher);
  return { specs, fetcher };
}

const lastSpec = (specs: QuerySpec[]) => specs[specs.length - 1];

beforeEach(() => {
  resetPodsStore();
  resetResourceDetailStore();
  resetActionsStore();
  useLayoutStore.setState({ ...initialLayoutState, tabs: [...initialLayoutState.tabs] });
  useContextsStore.setState({ ...initialContextsState, selectedContext: "kind-a" });
});

afterEach(() => {
  resetPodsStore();
  resetResourceDetailStore();
  resetActionsStore();
});

describe("mobile resource list: pod rows", () => {
  it("renders 2-line pod rows ≥ 64 px with the status pill, usage bars and meta", async () => {
    installFetcher(3, (i) => podRow(i, { restarts: i === 1 ? 7 : 2 }));
    render(<MobileResourceList kind="Pod" />);
    const rows = await screen.findAllByTestId("m-row");
    expect(rows).toHaveLength(3);
    expect(MOBILE_ROW_HEIGHT).toBeGreaterThanOrEqual(64);
    expect(rows[0]?.parentElement).toHaveStyle({ height: `${String(MOBILE_ROW_HEIGHT)}px` });

    const row = rows[0] as HTMLElement;
    expect(within(row).getByText("web-0000")).toHaveClass("m-row__name");
    expect(within(row).getByTestId("m-row-status")).toHaveTextContent("Running");
    expect(within(row).getByTestId("m-row-status")).toHaveClass("pill", "ok");
    // Line 2: desktop usage bars, then namespace · ready · ↻restarts · age.
    expect(within(within(row).getByTestId("m-row-cpu")).getByText("12m")).toBeInTheDocument();
    expect(within(row).getByTestId("m-row-cpu").querySelector(".use .bar")).not.toBeNull();
    expect(within(row).getByTestId("m-row-memory")).toHaveTextContent("64Mi");
    const meta = row.querySelector(".m-row__meta");
    expect(meta).toHaveTextContent("shop · 1/1 · ↻2 · 2d");
    // Restarts ≥ 5 are red like the desktop column; fewer are not.
    expect(within(row).getByTestId("m-row-restarts")).not.toHaveClass("hot");
    expect(within(rows[1] as HTMLElement).getByTestId("m-row-restarts")).toHaveClass("restarts", "hot");
    // One cluster: no cluster name on the row.
    expect(meta).not.toHaveTextContent("kind-a");
  });

  it("shows — for unmeasured usage, never a fake 0", async () => {
    installFetcher(1, (i) => podRow(i, { metrics: false }));
    render(<MobileResourceList kind="Pod" />);
    const row = await screen.findByTestId("m-row");
    expect(within(row).getByTestId("m-row-cpu")).toHaveTextContent("CPU —");
    expect(within(row).getByTestId("m-row-memory")).toHaveTextContent("Mem —");
    expect(row.querySelector(".use .bar")).toBeNull();
    expect(row).not.toHaveTextContent(/\b0m\b|\b0Mi\b/);
  });

  it("tints failing rows (--row-err) like the desktop table", async () => {
    installFetcher(2, (i) => podRow(i, i === 1 ? { status: "failed", restarts: 14 } : {}));
    render(<MobileResourceList kind="Pod" />);
    const rows = await screen.findAllByTestId("m-row");
    expect(rows[0]).not.toHaveClass("m-row--failed");
    expect(rows[1]).toHaveClass("m-row--failed");
    expect(within(rows[1] as HTMLElement).getByTestId("m-row-status")).toHaveClass("err");
    expect(within(rows[1] as HTMLElement).getByTestId("m-row-status")).toHaveTextContent("CrashLoopBackOff");
  });

  it("shows the cluster on each row when several clusters are selected", async () => {
    useContextsStore.setState({ activeClusterSelection: ["kind-a", "kind-b"] });
    installFetcher(1);
    render(<MobileResourceList kind="Pod" />);
    const row = await screen.findByTestId("m-row");
    expect(row.querySelector(".m-row__meta")).toHaveTextContent(/^kind-a · shop · /);
  });

  it("generic kinds show their own useful columns on line 2", async () => {
    const deployment = (i: number): Row => ({
      uid: `d-${String(i)}`,
      cluster: "kind-a",
      kind: "Deployment",
      namespace: "shop",
      name: `api-${String(i)}`,
      status: "degraded",
      statusReason: "1/3 available",
      cells: [
        { type: "ready", value: { ready: 1, total: 3 } },
        { type: "int", value: 3 },
        { type: "int", value: 1 },
        { type: "status", value: { label: "Progressing", phase: "degraded" } },
        { type: "timestamp", value: CREATED },
      ],
      labels: [],
      custom: [],
    });
    const { specs } = installFetcher(1, deployment);
    render(<MobileResourceList kind="Deployment" />);
    const row = await screen.findByTestId("m-row");
    expect(lastSpec(specs)?.kinds).toEqual(["Deployment"]);
    expect(within(row).getByTestId("m-row-status")).toHaveTextContent("Progressing");
    expect(row.querySelector(".m-row__meta")).toHaveTextContent("shop · 1/3 · Up-to-date 3 · 2d");
    expect(within(row).queryByTestId("m-row-cpu")).toBeNull();
  });
});

describe("mobile resource list: query", () => {
  it("only ever asks the backend for bounded windows and mounts few rows", async () => {
    const { specs } = installFetcher(50_000);
    render(<MobileResourceList kind="Pod" />);
    await screen.findByText("web-0000");
    expect(screen.getByTestId("pod-total")).toHaveTextContent("50,000 pods");
    expect(specs.length).toBeGreaterThan(0);
    for (const spec of specs) expect(spec.limit).toBeLessThanOrEqual(PAGE_SIZE);
    expect(screen.getAllByTestId("m-row").length).toBeLessThan(40);
    expect(Object.keys(usePodsStore.getState().rowsByIndex).length).toBeLessThanOrEqual(PAGE_SIZE);
  });

  it("shows the desktop summary: N pods · F failing · P pending", async () => {
    installFetcher(42);
    render(<MobileResourceList kind="Pod" />);
    await screen.findByText("web-0000");
    expect(screen.getByTestId("pod-total")).toHaveTextContent("42 pods");
    expect(await screen.findByTestId("pod-failing")).toHaveTextContent("2 failing");
    expect(screen.getByTestId("pod-pending")).toHaveTextContent("1 pending");
  });

  it("filter, namespace and sort update the backend query", async () => {
    const user = userEvent.setup();
    const { specs } = installFetcher(10);
    render(<MobileResourceList kind="Pod" />);
    await screen.findByText("web-0000");

    await user.type(screen.getByLabelText("Filter pods"), "web");
    expect(lastSpec(specs)).toMatchObject({ textFilter: "web", offset: 0 });

    await user.type(screen.getByLabelText("Namespace filter"), "shop");
    expect(lastSpec(specs)?.namespaces).toEqual(["shop"]);

    await user.selectOptions(screen.getByLabelText("Sort by"), "Restarts");
    expect(lastSpec(specs)?.sort).toEqual({ field: { type: "cell", index: 2 }, direction: "asc" });
    await user.click(screen.getByTestId("m-sort-dir"));
    expect(lastSpec(specs)?.sort).toEqual({ field: { type: "cell", index: 2 }, direction: "desc" });
    expect(lastSpec(specs)?.offset).toBe(0);
  });

  it("the status filter goes to the backend", async () => {
    const user = userEvent.setup();
    const { specs } = installFetcher(10);
    render(<MobileResourceList kind="Pod" />);
    await screen.findByText("web-0000");
    await user.click(within(screen.getByTestId("status-picker")).getByRole("button", { name: /Status/ }));
    await user.click(screen.getByRole("menuitemcheckbox", { name: /Failed/ }));
    expect(lastSpec(specs)?.statuses).toEqual(["failed"]);
  });

  it("says so honestly when no context is selected", () => {
    useContextsStore.setState({ selectedContext: null });
    const { fetcher } = installFetcher(10);
    render(<MobileResourceList kind="Pod" />);
    expect(screen.getByTestId("pod-select-context")).toHaveTextContent("Select a context to list pods");
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("pull-to-refresh at the top re-issues the current query", async () => {
    const { specs } = installFetcher(10);
    render(<MobileResourceList kind="Pod" />);
    await screen.findByText("web-0000");
    const refresh = vi.spyOn(usePodsStore.getState(), "refresh");
    const before = specs.length;
    const list = screen.getByTestId("m-list-scroll");

    fireEvent.pointerDown(list, { pointerId: 1, clientY: 100, isPrimary: true, button: 0 });
    fireEvent.pointerMove(list, { pointerId: 1, clientY: 100 + PULL_THRESHOLD * 3, isPrimary: true });
    expect(screen.getByTestId("m-ptr")).toHaveAttribute("data-state", "armed");
    expect(screen.getByText("Release to refresh")).toBeInTheDocument();
    fireEvent.pointerUp(list, { pointerId: 1, clientY: 100 + PULL_THRESHOLD * 3, isPrimary: true });

    await waitFor(() => {
      expect(specs.length).toBeGreaterThan(before);
    });
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(lastSpec(specs)).toMatchObject({ offset: 0, limit: PAGE_SIZE, kinds: ["Pod"] });
    await waitFor(() => {
      expect(screen.getByTestId("m-ptr")).toHaveAttribute("data-state", "idle");
    });
    // The release of the drag did not open the row under the finger.
    expect(useResourceDetailStore.getState().target).toBeNull();
    refresh.mockRestore();
  });

  it("a short pull, or a pull that does not start at the top, does not refresh", async () => {
    const { specs } = installFetcher(10);
    render(<MobileResourceList kind="Pod" />);
    await screen.findByText("web-0000");
    const list = screen.getByTestId("m-list-scroll");
    const before = specs.length;

    fireEvent.pointerDown(list, { pointerId: 1, clientY: 100, isPrimary: true, button: 0 });
    fireEvent.pointerMove(list, { pointerId: 1, clientY: 120, isPrimary: true });
    fireEvent.pointerUp(list, { pointerId: 1, clientY: 120, isPrimary: true });

    list.scrollTop = 200;
    fireEvent.pointerDown(list, { pointerId: 2, clientY: 100, isPrimary: true, button: 0 });
    fireEvent.pointerMove(list, { pointerId: 2, clientY: 400, isPrimary: true });
    fireEvent.pointerUp(list, { pointerId: 2, clientY: 400, isPrimary: true });

    await act(async () => {
      await Promise.resolve();
    });
    expect(specs.length).toBe(before);
  });
});

describe("mobile resource list: detail screen", () => {
  it("tapping a row opens the full-screen detail with name, meta, pill and no key hints", async () => {
    const user = userEvent.setup();
    installFetcher(3, (i) => podRow(i, i === 0 ? { status: "failed", restarts: 14 } : {}));
    render(<MobileResourceList kind="Pod" remoteOps={false} />);
    const rows = await screen.findAllByTestId("m-row");
    await user.click(rows[0] as HTMLElement);

    const detail = screen.getByRole("region", { name: "Resource details" });
    expect(detail).toHaveClass("resource-drawer--screen");
    expect(useResourceDetailStore.getState().target?.uid).toBe("uid-0");
    expect(within(detail).getByRole("heading", { name: "web-0000" })).toBeInTheDocument();
    expect(within(detail).getByTestId("drawer-meta")).toHaveTextContent("Pod · shop · kind-a · 2d");
    expect(within(detail).getByTestId("drawer-status-reason")).toHaveTextContent("CrashLoopBackOff");
    expect(within(detail).getByTestId("drawer-action-logs")).toBeInTheDocument();
    expect(within(detail).getByTestId("drawer-action-delete")).toHaveClass("danger");
    // Phone: no keyboard hints; Android 0.3 hides Shell and Port-forward.
    expect(detail.querySelector(".resource-drawer__actions kbd")).toBeNull();
    expect(within(detail).queryByTestId("drawer-action-shell")).toBeNull();
    expect(within(detail).queryByTestId("drawer-action-forward")).toBeNull();
    expect(within(detail).queryByRole("tab", { name: "Terminal" })).toBeNull();
    for (const tab of ["Summary", "YAML", "Logs", "Events"]) {
      expect(within(detail).getByRole("tab", { name: tab })).toBeInTheDocument();
    }

    await user.click(within(detail).getByRole("button", { name: "Back" }));
    expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    expect(useResourceDetailStore.getState().target).toBeNull();
  });

  it("detail actions go through the confirm dialog", async () => {
    const user = userEvent.setup();
    installFetcher(1);
    render(<MobileResourceList kind="Pod" />);
    await user.click(await screen.findByTestId("m-row"));
    await user.click(screen.getByTestId("drawer-action-delete"));
    expect(useActionsStore.getState().pending).toMatchObject({
      context: "kind-a",
      action: { type: "delete", kind: "Pod", namespace: "shop", name: "web-0000" },
    });
    expect(screen.getByTestId("action-confirm-dialog")).toBeInTheDocument();
  });

  it("a read-only context disables the write actions", async () => {
    const user = userEvent.setup();
    useContextsStore.setState({
      settingsByContext: {
        "kind-a": { colour: null, readOnly: true, protected: false, tags: [], pinned: false },
      },
    });
    installFetcher(1);
    render(<MobileResourceList kind="Pod" />);
    await user.click(await screen.findByTestId("m-row"));
    expect(screen.getByTestId("drawer-action-delete")).toBeDisabled();
    expect(screen.getByTestId("drawer-action-logs")).toBeEnabled();
    await user.click(screen.getByTestId("drawer-action-delete"));
    expect(useActionsStore.getState().pending).toBeNull();
  });
});
