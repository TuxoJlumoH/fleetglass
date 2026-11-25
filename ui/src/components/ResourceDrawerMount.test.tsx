import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ResourceDetail } from "../generated/bindings";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { resetPodsStore, type Row } from "../stores/pods";
import { usePortForwardsStore } from "../stores/portForwards";
import { resetResourceDetailStore, useResourceDetailStore } from "../stores/resourceDetail";
import { DiagnosticsModal } from "./DiagnosticsModal";
import { PodTable } from "./PodTable";
import { ResourceDrawer } from "./ResourceDrawer";

const sampleRow: Row = {
  uid: "uid-pod-0",
  cluster: "kind-dev",
  kind: "Pod",
  namespace: "default",
  name: "nginx-0",
  status: "healthy",
  statusReason: "Running",
  cells: [
    { type: "ready", value: { ready: 1, total: 1 } },
    { type: "status", value: { label: "Running", phase: "healthy" } },
    { type: "int", value: 0 },
    { type: "text", value: "node-1" },
    { type: "text", value: "10.244.0.5" },
    { type: "timestamp", value: "2026-03-30T00:00:00Z" },
  ],
  labels: [["app", "nginx"]],
  custom: [],
};

function podRow(i: number): Row {
  return { ...sampleRow, uid: `uid-pod-${String(i)}`, name: `nginx-${String(i)}` };
}

function eventRow(i: number): Row {
  return {
    uid: `ev-${String(i)}`,
    cluster: "kind-dev",
    kind: "Event",
    namespace: "default",
    name: `ev-${String(i)}`,
    status: "healthy",
    statusReason: "Pulled",
    cells: [
      { type: "status", value: { label: "Normal", phase: "healthy" } },
      { type: "text", value: `Reason${String(i)}` },
      { type: "text", value: `message ${String(i)}` },
      { type: "timestamp", value: "2026-03-30T10:00:00Z" },
    ],
    labels: [],
    custom: [],
  };
}

function rowElement(name: string): HTMLElement {
  const table = screen.getByRole("table", { name: "Pods" });
  const el = within(table).getByText(name).closest<HTMLElement>('[role="row"]');
  if (!el) throw new Error(`row ${name} not rendered`);
  return el;
}

/** Mocks a three-pod table; `get_resource_detail` echoes the requested pod. */
function mockThreePods(calls: { cmd: string; args: unknown }[]) {
  const rows = [podRow(0), podRow(1), podRow(2)];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === "query_page") {
      return {
        total: rows.length,
        offset: 0,
        limit: 100,
        rows,
        version: 1,
        access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
      };
    }
    if (cmd === "subscribe_query") return 1;
    if (cmd === "unsubscribe_query") return true;
    if (cmd === "get_resource_detail") {
      const name = (args as { name: string }).name;
      return { ...sampleDetail, name, uid: `uid-${name}` };
    }
    if (cmd === "start_workload_logs") return { status: "ok", data: 1 };
    if (cmd === "stop_workload_logs") return { status: "ok", data: true };
    return undefined;
  });
}

function detailRequests(calls: { cmd: string; args: unknown }[]): string[] {
  return calls
    .filter((c) => c.cmd === "get_resource_detail")
    .map((c) => (c.args as { name: string }).name);
}

const sampleDetail: ResourceDetail = {
  uid: "uid-pod-0",
  cluster: "kind-dev",
  kind: "Pod",
  namespace: "default",
  name: "nginx-0",
  yaml: "apiVersion: v1\nkind: Pod\nmetadata:\n  name: nginx-0\n  namespace: default\nspec:\n  containers:\n  - name: nginx\n    image: nginx:1.27\n",
  related: [
    {
      kind: "ReplicaSet",
      name: "nginx-rs-1",
      namespace: "default",
      uid: "uid-rs-1",
      relationship: "owner",
    },
  ],
  events: [
    {
      uid: "ev-1",
      cluster: "kind-dev",
      kind: "Event",
      name: "ev-1",
      status: "healthy",
      statusReason: "Pulled",
      cells: [
        { type: "status", value: { label: "Normal", phase: "healthy" } },
        { type: "text", value: "Pulled" },
        { type: "text", value: "Successfully pulled image nginx:1.27" },
        { type: "timestamp", value: "2026-03-30T10:00:00Z" },
      ],
      labels: [],
      custom: [],
    },
    {
      uid: "ev-2",
      cluster: "kind-dev",
      kind: "Event",
      name: "ev-2",
      status: "degraded",
      statusReason: "BackOff",
      cells: [
        { type: "status", value: { label: "Warning", phase: "degraded" } },
        { type: "text", value: "BackOff" },
        { type: "text", value: "Back-off restarting failed container" },
        { type: "timestamp", value: "2026-03-30T10:05:00Z" },
      ],
      labels: [],
      custom: [],
    },
  ],
};

describe("Task R-04: Mount ResourceDrawer from PodTable with mockIPC", () => {
  let ipcCalls: { cmd: string; args: unknown }[] = [];

  beforeEach(() => {
    resetPodsStore();
    resetResourceDetailStore();
    useContextsStore.setState({
      ...initialContextsState,
      selectedContext: "kind-dev",
    });

    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");

    ipcCalls = [];
    mockIPC((cmd, args) => {
      ipcCalls.push({ cmd, args });
      if (cmd === "query_page") {
        return {
          total: 1,
          offset: 0,
          limit: 100,
          rows: [sampleRow],
          version: 1,
          access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
        };
      }
      if (cmd === "subscribe_query") {
        return 1;
      }
      if (cmd === "unsubscribe_query") {
        return true;
      }
      if (cmd === "get_resource_detail") {
        return sampleDetail;
      }
      return undefined;
    });
  });

  afterEach(() => {
    clearMocks();
    resetPodsStore();
    resetResourceDetailStore();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("Test 1: Pressing Enter on a row opens ResourceDrawer and calls get_resource_detail", async () => {
    render(<PodTable />);

    const podCell = await screen.findByText("nginx-0");
    const rowEl = podCell.closest<HTMLElement>('[role="row"]');
    if (!rowEl) throw new Error("row not rendered");

    rowEl.focus();
    fireEvent.keyDown(rowEl, { key: "Enter" });

    expect(await screen.findByRole("region", { name: "Resource details" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "nginx-0" })).toBeInTheDocument();

    await waitFor(() => {
      const detailCall = ipcCalls.find((c) => c.cmd === "get_resource_detail");
      expect(detailCall).toBeDefined();
      expect(detailCall?.args).toEqual({
        context: "kind-dev",
        kind: "Pod",
        namespace: "default",
        name: "nginx-0",
      });
    });

    expect(await screen.findByText("ReplicaSet/nginx-rs-1")).toBeInTheDocument();
  });

  it("Test 2: Double clicking a row opens ResourceDrawer and renders YAML from detail.yaml", async () => {
    const user = userEvent.setup();
    render(<PodTable />);

    const podCell = await screen.findByText("nginx-0");
    const rowEl = podCell.closest<HTMLElement>('[role="row"]');
    if (!rowEl) throw new Error("row not rendered");

    fireEvent.doubleClick(rowEl);

    expect(await screen.findByRole("region", { name: "Resource details" })).toBeInTheDocument();

    const yamlTab = screen.getByRole("tab", { name: "YAML" });
    await user.click(yamlTab);

    const yamlEditor = await screen.findByLabelText("Resource YAML");
    await waitFor(() => {
      expect(yamlEditor).toHaveValue(sampleDetail.yaml);
    });
  });

  it("Test 3: Events tab displays events from detail.events", async () => {
    const user = userEvent.setup();
    render(<PodTable />);

    const podCell = await screen.findByText("nginx-0");
    const rowEl = podCell.closest<HTMLElement>('[role="row"]');
    if (!rowEl) throw new Error("row not rendered");

    fireEvent.doubleClick(rowEl);

    expect(await screen.findByRole("region", { name: "Resource details" })).toBeInTheDocument();

    const eventsTab = screen.getByRole("tab", { name: "Events" });
    await user.click(eventsTab);

    expect(await screen.findByText("Pulled")).toBeInTheDocument();
    expect(screen.getByText("Successfully pulled image nginx:1.27")).toBeInTheDocument();
    expect(screen.getByText("2026-03-30T10:00:00Z")).toBeInTheDocument();
    expect(screen.getByText("Normal")).toBeInTheDocument();

    expect(screen.getByText("BackOff")).toBeInTheDocument();
    expect(screen.getByText("Back-off restarting failed container")).toBeInTheDocument();
    expect(screen.getByText("2026-03-30T10:05:00Z")).toBeInTheDocument();
    expect(screen.getByText("Warning")).toBeInTheDocument();
  });

  it("Test 4: Pressing Escape closes drawer and restores focus to the row", async () => {
    render(<PodTable />);

    const podCell = await screen.findByText("nginx-0");
    const rowEl = podCell.closest<HTMLElement>('[role="row"]');
    if (!rowEl) throw new Error("row not rendered");

    rowEl.focus();
    fireEvent.keyDown(rowEl, { key: "Enter" });

    const drawer = await screen.findByRole("region", { name: "Resource details" });
    await waitFor(() => {
      expect(drawer).toHaveFocus();
    });

    fireEvent.keyDown(drawer, { key: "Escape" });

    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    });

    expect(rowEl).toHaveFocus();
  });

  it("shows empty state when detail has no events", async () => {
    mockIPC((cmd) => {
      if (cmd === "query_page") {
        return {
          total: 1,
          offset: 0,
          limit: 100,
          rows: [sampleRow],
          version: 1,
          access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
        };
      }
      if (cmd === "subscribe_query") return 1;
      if (cmd === "unsubscribe_query") return true;
      if (cmd === "get_resource_detail") {
        return { ...sampleDetail, events: [] };
      }
      return undefined;
    });

    const user = userEvent.setup();
    render(<PodTable />);

    const podCell = await screen.findByText("nginx-0");
    const rowEl = podCell.closest<HTMLElement>('[role="row"]');
    if (!rowEl) throw new Error("row not rendered");

    fireEvent.doubleClick(rowEl);
    expect(await screen.findByRole("region", { name: "Resource details" })).toBeInTheDocument();

    const eventsTab = screen.getByRole("tab", { name: "Events" });
    await user.click(eventsTab);

    expect(await screen.findByText("No events recorded for nginx-0.")).toBeInTheDocument();
  });

  it("never shows a previous row's detail when a newer open is still in flight", async () => {
    const rowA = sampleRow;
    const rowB: Row = { ...sampleRow, uid: "uid-pod-1", name: "nginx-1" };
    const detailFor = (row: Row): ResourceDetail => ({
      ...sampleDetail,
      uid: row.uid,
      name: row.name,
      yaml: `kind: Pod\nmetadata:\n  name: ${row.name}\n`,
      related: [
        {
          kind: "ReplicaSet",
          name: `rs-of-${row.name}`,
          namespace: "default",
          uid: `uid-rs-${row.name}`,
          relationship: "owner",
        },
      ],
    });
    const pending = new Map<string, (d: ResourceDetail) => void>();
    mockIPC((cmd, args) => {
      if (cmd === "query_page") {
        return {
          total: 2,
          offset: 0,
          limit: 100,
          rows: [rowA, rowB],
          version: 1,
          access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
        };
      }
      if (cmd === "subscribe_query") return 1;
      if (cmd === "unsubscribe_query") return true;
      if (cmd === "get_resource_detail") {
        const name = (args as { name: string }).name;
        return new Promise<ResourceDetail>((resolve) => {
          pending.set(name, resolve);
        });
      }
      return undefined;
    });

    render(<PodTable />);
    const cellA = await screen.findByText("nginx-0");
    const cellB = screen.getByText("nginx-1");

    fireEvent.doubleClick(cellA);
    await waitFor(() => {
      expect(pending.has("nginx-0")).toBe(true);
    });
    fireEvent.doubleClick(cellB);
    await waitFor(() => {
      expect(pending.has("nginx-1")).toBe(true);
    });

    // B resolves first, then the stale A response arrives.
    await act(async () => {
      pending.get("nginx-1")?.(detailFor(rowB));
      await Promise.resolve();
    });
    expect(await screen.findByText("ReplicaSet/rs-of-nginx-1")).toBeInTheDocument();
    await act(async () => {
      pending.get("nginx-0")?.(detailFor(rowA));
      await Promise.resolve();
    });

    expect(screen.getByRole("heading", { name: "nginx-1" })).toBeInTheDocument();
    expect(screen.getByText("ReplicaSet/rs-of-nginx-1")).toBeInTheDocument();
    expect(screen.queryByText("ReplicaSet/rs-of-nginx-0")).toBeNull();
    expect(screen.queryByTestId("drawer-loading")).toBeNull();
  });

  it("shows the loading state, then the IPC error instead of empty YAML or events", async () => {
    let reject: ((e: unknown) => void) | undefined;
    mockIPC((cmd) => {
      if (cmd === "query_page") {
        return {
          total: 1,
          offset: 0,
          limit: 100,
          rows: [sampleRow],
          version: 1,
          access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
        };
      }
      if (cmd === "subscribe_query") return 1;
      if (cmd === "unsubscribe_query") return true;
      if (cmd === "get_resource_detail") {
        return new Promise((_resolve, rej) => {
          reject = rej;
        });
      }
      return undefined;
    });

    const user = userEvent.setup();
    render(<PodTable />);
    fireEvent.doubleClick(await screen.findByText("nginx-0"));

    expect(await screen.findByTestId("drawer-loading")).toHaveTextContent("Loading Pod nginx-0");
    await user.click(screen.getByRole("tab", { name: "Events" }));
    expect(screen.getByText("Loading events…")).toBeInTheDocument();
    expect(screen.queryByText(/No events recorded/)).toBeNull();

    await waitFor(() => {
      expect(reject).toBeDefined();
    });
    await act(async () => {
      reject?.({ kind: "forbidden", message: 'pods "nginx-0" is forbidden' });
      await Promise.resolve();
    });

    expect(await screen.findByRole("alert")).toHaveTextContent(
      'Cannot load Pod nginx-0: pods "nginx-0" is forbidden',
    );
    expect(screen.queryByTestId("drawer-loading")).toBeNull();
    expect(screen.getByText("Events unavailable: the resource could not be loaded.")).toBeVisible();
    expect(screen.queryByText(/No events recorded/)).toBeNull();

    await user.click(screen.getByRole("tab", { name: "YAML" }));
    expect(screen.queryByLabelText("Resource YAML")).toBeNull();
    expect(screen.getByText("YAML unavailable: the resource could not be loaded.")).toBeVisible();
  });

  it("Enter in the filter input does not open the drawer", async () => {
    render(<PodTable />);
    await screen.findByText("nginx-0");

    const filter = screen.getByLabelText("Filter pods");
    filter.focus();
    fireEvent.keyDown(filter, { key: "Enter" });
    fireEvent.keyDown(screen.getByLabelText("Namespace filter"), { key: "Enter" });

    expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    expect(ipcCalls.some((c) => c.cmd === "get_resource_detail")).toBe(false);
  });

  it("Escape with the diagnostics modal open closes only the modal", async () => {
    function Harness() {
      const [diagOpen, setDiagOpen] = useState(false);
      return (
        <>
          <button
            type="button"
            onClick={() => {
              setDiagOpen(true);
            }}
          >
            Open diagnostics
          </button>
          <PodTable />
          <DiagnosticsModal
            open={diagOpen}
            report={null}
            context="kind-dev"
            onClose={() => {
              setDiagOpen(false);
            }}
          />
        </>
      );
    }

    const user = userEvent.setup();
    render(<Harness />);
    fireEvent.doubleClick(await screen.findByText("nginx-0"));
    const drawer = await screen.findByRole("region", { name: "Resource details" });

    await user.click(screen.getByRole("button", { name: "Open diagnostics" }));
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(drawer).toBeInTheDocument();

    // Escape dispatched on the drawer while a modal is open is ignored by the drawer, too.
    await user.click(screen.getByRole("button", { name: "Open diagnostics" }));
    fireEvent.keyDown(drawer, { key: "Escape" });
    expect(screen.getByRole("region", { name: "Resource details" })).toBeInTheDocument();
  });

  it("Enter on a focused row while the palette is open does not open the drawer", async () => {
    render(<PodTable />);
    const rowEl = (await screen.findByText("nginx-0")).closest<HTMLElement>('[role="row"]');
    if (!rowEl) throw new Error("row not rendered");
    act(() => {
      useLayoutStore.setState({ paletteOpen: true });
    });
    rowEl.focus();
    fireEvent.keyDown(rowEl, { key: "Enter" });
    expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    act(() => {
      useLayoutStore.setState({ paletteOpen: false });
    });
  });

  it("Enter after click + j opens the selected row, not the clicked one", async () => {
    mockThreePods(ipcCalls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    await user.click(rowElement("nginx-0"));
    expect(rowElement("nginx-0")).toHaveFocus();
    await user.keyboard("j");
    // Focus follows the selection (roving tabindex).
    await waitFor(() => {
      expect(rowElement("nginx-1")).toHaveFocus();
    });
    expect(rowElement("nginx-1")).toHaveAttribute("data-selected", "true");
    expect(rowElement("nginx-1")).toHaveAttribute("tabindex", "0");
    expect(rowElement("nginx-0")).toHaveAttribute("tabindex", "-1");

    await user.keyboard("{Enter}");
    expect(await screen.findByRole("heading", { name: "nginx-1" })).toBeInTheDocument();
    expect(detailRequests(ipcCalls)).toEqual(["nginx-1"]);
  });

  it("Enter with stale focus on another row still opens the selected row", async () => {
    mockThreePods(ipcCalls);
    render(<PodTable />);
    await screen.findByText("nginx-2");

    // Select row 2 via G with the table container focused, then dispatch Enter from row 0.
    const container = rowElement("nginx-0").closest<HTMLElement>(".pod-table-container");
    if (!container) throw new Error("container not rendered");
    container.focus();
    fireEvent.keyDown(container, { key: "G" });
    fireEvent.keyDown(rowElement("nginx-0"), { key: "Enter" });

    expect(await screen.findByRole("heading", { name: "nginx-2" })).toBeInTheDocument();
    expect(detailRequests(ipcCalls)).toEqual(["nginx-2"]);
  });

  it("Enter with focus on the body opens the selected row", async () => {
    mockThreePods(ipcCalls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    expect(document.activeElement).toBe(document.body);
    await user.keyboard("jj");
    expect(rowElement("nginx-2")).toHaveAttribute("data-selected", "true");
    await user.keyboard("{Enter}");

    expect(await screen.findByRole("heading", { name: "nginx-2" })).toBeInTheDocument();
    expect(detailRequests(ipcCalls)).toEqual(["nginx-2"]);
  });

  it("focusing a row (Tab / click) makes it the selected row", async () => {
    mockThreePods(ipcCalls);
    render(<PodTable />);
    await screen.findByText("nginx-2");

    act(() => {
      rowElement("nginx-2").focus();
    });
    expect(rowElement("nginx-2")).toHaveAttribute("data-selected", "true");
    fireEvent.keyDown(rowElement("nginx-2"), { key: "Enter" });
    expect(await screen.findByRole("heading", { name: "nginx-2" })).toBeInTheDocument();
  });

  it("while the drawer is open, j and / still work from the table, and Escape restores focus by uid", async () => {
    mockThreePods(ipcCalls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    fireEvent.doubleClick(rowElement("nginx-0"));
    const drawer = await screen.findByRole("region", { name: "Resource details" });
    await waitFor(() => {
      expect(drawer).toHaveFocus();
    });

    // Keys inside the drawer do not drive the table.
    fireEvent.keyDown(drawer, { key: "j" });
    expect(rowElement("nginx-0")).toHaveAttribute("data-selected", "true");

    // Back in the table: j moves the selection, / focuses the filter.
    await user.click(rowElement("nginx-0"));
    await user.keyboard("j");
    expect(rowElement("nginx-1")).toHaveAttribute("data-selected", "true");
    expect(screen.getByRole("heading", { name: "nginx-0" })).toBeInTheDocument();
    await waitFor(() => {
      expect(rowElement("nginx-1")).toHaveFocus();
    });
    await user.keyboard("/");
    expect(screen.getByLabelText("Filter pods")).toHaveFocus();

    // Escape on the drawer refocuses the pod it was opened for (nginx-0), not the selection.
    const openDrawer = screen.getByRole("region", { name: "Resource details" });
    act(() => {
      openDrawer.focus();
    });
    fireEvent.keyDown(openDrawer, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    });
    expect(rowElement("nginx-0")).toHaveFocus();
  });

  it("Escape restores focus to the table container when the origin pod is gone", async () => {
    mockThreePods(ipcCalls);
    render(<PodTable />);
    await screen.findByText("nginx-2");

    fireEvent.doubleClick(rowElement("nginx-1"));
    await screen.findByRole("region", { name: "Resource details" });
    // The origin row disappears from the table (e.g. pod deleted) while the drawer is open.
    act(() => {
      useResourceDetailStore.setState({ target: { ...podRow(1), uid: "uid-deleted" } });
    });
    fireEvent.keyDown(screen.getByRole("region", { name: "Resource details" }), {
      key: "Escape",
    });

    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    });
    const container = rowElement("nginx-0").closest(".pod-table-container");
    expect(container).toHaveFocus();
    for (const name of ["nginx-0", "nginx-1", "nginx-2"]) {
      expect(rowElement(name)).not.toHaveFocus();
    }
  });

  it("unmounting the table closes the drawer and resets the detail store", async () => {
    const { unmount } = render(<PodTable />);
    fireEvent.doubleClick(await screen.findByText("nginx-0"));
    await screen.findByText("ReplicaSet/nginx-rs-1");
    expect(useResourceDetailStore.getState().target?.uid).toBe("uid-pod-0");

    unmount();

    const state = useResourceDetailStore.getState();
    expect(state.target).toBeNull();
    expect(state.detail).toBeNull();
    expect(state.loading).toBe(false);
    expect(state.error).toBeNull();
  });

  it("Logs tab mounts WorkloadLogsView for Pods", async () => {
    const user = userEvent.setup();
    render(<PodTable />);
    fireEvent.doubleClick(await screen.findByText("nginx-0"));
    await screen.findByRole("region", { name: "Resource details" });

    await user.click(screen.getByRole("tab", { name: "Logs" }));
    expect(
      await screen.findByRole("region", { name: "Workload logs for kind-dev/default/pod/nginx-0" }),
    ).toBeVisible();
    expect(screen.getByText("kind-dev/default/pod/nginx-0")).toBeVisible();
    expect(screen.getByTestId("workload-logs-status")).toBeVisible();
    expect(screen.queryByTestId("logs-unavailable")).toBeNull();
  });

  it("Logs tab shows honest notice for non-Pod resources", async () => {
    const user = userEvent.setup();
    const serviceRow: Row = {
      ...sampleRow,
      uid: "uid-svc-0",
      kind: "Service",
      name: "frontend-svc",
    };
    render(<ResourceDrawer row={serviceRow} onClose={() => {}} />);
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    expect(screen.getByTestId("logs-unavailable")).toBeVisible();
    expect(screen.getByText("Logs are available for Pods and Workloads.")).toBeVisible();
    expect(screen.queryByRole("region", { name: /Workload logs/ })).toBeNull();
  });

  it("02-06c header shows the full status reason with a title tooltip", () => {
    const reason = "ImagePullBackOff: Back-off pulling image registry.example/app:1";
    render(<ResourceDrawer row={{ ...sampleRow, statusReason: reason }} onClose={() => {}} />);
    const el = screen.getByTestId("drawer-status-reason");
    expect(el).toHaveTextContent(reason);
    expect(el).toHaveAttribute("title", reason);
  });

  it("02-06c header omits the reason when it is empty", () => {
    render(<ResourceDrawer row={{ ...sampleRow, statusReason: "" }} onClose={() => {}} />);
    expect(screen.queryByTestId("drawer-status-reason")).toBeNull();
  });

  it("Logs tab renders legacy log lines view when logLines prop is passed", async () => {
    const user = userEvent.setup();
    const lines = [{ seq: 1, text: "legacy log line" }];
    render(<ResourceDrawer row={sampleRow} logLines={lines} onClose={() => {}} />);
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    expect(screen.getByTestId("log-lines")).toHaveTextContent("legacy log line");
    expect(screen.queryByRole("region", { name: /Workload logs/ })).toBeNull();
  });

  it("Escape in an edited YAML editor leaves the editor; a second Escape closes the drawer", async () => {
    const user = userEvent.setup();
    render(<PodTable />);
    fireEvent.doubleClick(await screen.findByText("nginx-0"));
    const drawer = await screen.findByRole("region", { name: "Resource details" });
    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const editor = await screen.findByLabelText("Resource YAML");
    await waitFor(() => {
      expect(editor).toHaveValue(sampleDetail.yaml);
    });

    await user.click(editor);
    await user.type(editor, "# edited");
    await user.keyboard("{Escape}");
    expect(drawer).toBeInTheDocument();
    expect(drawer).toHaveFocus();
    expect(screen.getByLabelText("Resource YAML")).toHaveValue(`${sampleDetail.yaml}# edited`);

    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Resource details" })).toBeNull();
    });
  });

  it("Events tab reads the projected cell order and marks Warning events", async () => {
    const user = userEvent.setup();
    render(<PodTable />);
    fireEvent.doubleClick(await screen.findByText("nginx-0"));
    await screen.findByRole("region", { name: "Resource details" });
    await user.click(screen.getByRole("tab", { name: "Events" }));

    const items = await screen.findAllByTestId("event-item");
    expect(items).toHaveLength(2);
    expect(items[1]).toHaveTextContent("Warning");
    expect(screen.getByText("Warning")).toHaveClass("badge", "badge--warning");
    expect(screen.getByText("Normal")).toHaveClass("badge");
    expect(screen.getByText("Normal")).not.toHaveClass("badge--warning");
    expect(screen.queryByTestId("events-limit")).toBeNull();
  });

  it("says the event list is truncated when the backend returns its 50-event cap", async () => {
    mockIPC((cmd) => {
      if (cmd === "query_page") {
        return {
          total: 1,
          offset: 0,
          limit: 100,
          rows: [sampleRow],
          version: 1,
          access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
        };
      }
      if (cmd === "subscribe_query") return 1;
      if (cmd === "unsubscribe_query") return true;
      if (cmd === "get_resource_detail") {
        return { ...sampleDetail, events: Array.from({ length: 50 }, (_, i) => eventRow(i)) };
      }
      return undefined;
    });
    const user = userEvent.setup();
    render(<PodTable />);
    fireEvent.doubleClick(await screen.findByText("nginx-0"));
    await screen.findByRole("region", { name: "Resource details" });
    await user.click(screen.getByRole("tab", { name: "Events" }));

    expect(await screen.findByTestId("events-limit")).toHaveTextContent("Showing latest 50 events");
    expect(screen.getAllByTestId("event-item")).toHaveLength(50);
  });

  it("04-05a: renders RBAC cross-links in RoleBinding drawer summary", async () => {
    const roleBindingRow: Row = {
      uid: "uid-rb-1",
      cluster: "kind-dev",
      kind: "RoleBinding",
      namespace: "default",
      name: "read-pods-binding",
      status: "healthy",
      statusReason: "Active",
      cells: [
        { type: "text", value: "Role/pod-reader" },
        { type: "text", value: "ServiceAccount:app-sa" },
        { type: "status", value: { label: "Active", phase: "healthy" } },
        { type: "timestamp", value: "2026-03-30T00:00:00Z" },
      ],
      labels: [],
      custom: [],
    };

    const roleBindingYaml = `
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: read-pods-binding
  namespace: default
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: pod-reader
subjects:
- kind: ServiceAccount
  name: app-sa
  namespace: default
`;

    const onOpenRelated = vi.fn();
    render(
      <ResourceDrawer
        row={roleBindingRow}
        yamlText={roleBindingYaml}
        onClose={vi.fn()}
        onOpenRelated={onOpenRelated}
      />,
    );

    expect(await screen.findByTestId("summary-rbac-role-card")).toBeInTheDocument();
    expect(screen.getByTestId("summary-rbac-subjects-card")).toBeInTheDocument();

    const roleBtn = screen.getByTestId("rbac-role-ref-btn");
    expect(roleBtn).toHaveTextContent("Role/pod-reader");
    fireEvent.click(roleBtn);

    expect(onOpenRelated).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "Role",
        name: "pod-reader",
        relationship: "roleRef",
      }),
    );

    const subjectBtn = screen.getByTestId("rbac-subject-0");
    expect(subjectBtn).toHaveTextContent("ServiceAccount/app-sa (default)");
    fireEvent.click(subjectBtn);

    expect(onOpenRelated).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "ServiceAccount",
        name: "app-sa",
        relationship: "subject",
      }),
    );
  });

  it("04-05a: renders RBAC rules in Role drawer summary", async () => {
    const roleRow: Row = {
      uid: "uid-role-1",
      cluster: "kind-dev",
      kind: "Role",
      namespace: "default",
      name: "pod-reader",
      status: "healthy",
      statusReason: "Active",
      cells: [
        { type: "status", value: { label: "Active", phase: "healthy" } },
        { type: "timestamp", value: "2026-03-30T00:00:00Z" },
      ],
      labels: [],
      custom: [],
    };

    const roleYaml = `
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: pod-reader
  namespace: default
rules:
- apiGroups: [""]
  resources: ["pods", "pods/log"]
  verbs: ["get", "list", "watch"]
`;

    render(
      <ResourceDrawer
        row={roleRow}
        yamlText={roleYaml}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByTestId("summary-rbac-rules-card")).toBeInTheDocument();
    expect(screen.getByTestId("rbac-rule-row-0")).toHaveTextContent("pods, pods/log");
    expect(screen.getByTestId("rbac-rule-row-0")).toHaveTextContent("get, list, watch");
  });

  it("04-07: renders Certificates card in drawer summary with SANs and validity badge", async () => {
    const secretRow: Row = {
      uid: "uid-secret-tls",
      cluster: "kind-dev",
      kind: "Secret",
      namespace: "prod",
      name: "ingress-tls",
      status: "healthy",
      statusReason: "Active",
      cells: [
        { type: "status", value: { label: "Active", phase: "healthy" } },
        { type: "timestamp", value: "2026-03-30T00:00:00Z" },
      ],
      labels: [],
      custom: [],
    };

    const certDetail: ResourceDetail = {
      uid: "uid-secret-tls",
      cluster: "kind-dev",
      kind: "Secret",
      namespace: "prod",
      name: "ingress-tls",
      yaml: "apiVersion: v1\nkind: Secret\n",
      related: [],
      events: [],
      eventsTruncated: false,
      pod: null,
      certificates: [
        {
          source: "ingress-tls/tls.crt",
          subject: "CN=example.com, O=Fleetglass",
          issuer: "CN=Fleetglass Root CA, O=Fleetglass Org",
          serialNumber: "04:A2:3B:11:22",
          signatureAlgorithm: "SHA256withRSA",
          sans: ["example.com", "api.example.com", "10.0.0.1"],
          notBefore: "2026-01-01T00:00:00Z",
          notAfter: "2027-01-01T00:00:00Z",
          daysLeft: 84,
          validityLabel: "Valid · 84d left",
          validityTone: "ok",
        },
      ],
    };

    render(
      <ResourceDrawer
        row={secretRow}
        detail={certDetail}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByTestId("summary-certificates-card")).toBeInTheDocument();
    expect(screen.getByTestId("cert-item-0")).toBeInTheDocument();
    expect(screen.getByText("ingress-tls/tls.crt")).toBeInTheDocument();
    const validity = screen.getByTestId("cert-validity-0");
    expect(validity).toHaveTextContent("Valid · 84d left");
    expect(validity).toHaveClass("pill", "ok");
    expect(screen.getByText("CN=example.com, O=Fleetglass")).toBeInTheDocument();
    expect(screen.getByText("CN=Fleetglass Root CA, O=Fleetglass Org")).toBeInTheDocument();
    expect(screen.getByText("04:A2:3B:11:22")).toBeInTheDocument();
    expect(screen.getByText("SHA256withRSA")).toBeInTheDocument();
    expect(screen.getByText("example.com")).toBeInTheDocument();
    expect(screen.getByText("api.example.com")).toBeInTheDocument();
    expect(screen.getByText("10.0.0.1")).toBeInTheDocument();
  });

  it("05-05: renders ENDPOINTS card for Ingress with Open in Browser button", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();

    const ingressRow: Row = {
      uid: "uid-ingress-1",
      cluster: "kind-dev",
      kind: "Ingress",
      namespace: "default",
      name: "web-ingress",
      status: "healthy",
      statusReason: "Active",
      cells: [
        { type: "text", value: "nginx" },
        { type: "status", value: { label: "Active", phase: "healthy" } },
        { type: "timestamp", value: "2026-03-30T00:00:00Z" },
      ],
      labels: [],
      custom: [],
    };

    const ingressDetail: ResourceDetail = {
      uid: "uid-ingress-1",
      cluster: "kind-dev",
      kind: "Ingress",
      namespace: "default",
      name: "web-ingress",
      yaml: `apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: web-ingress
  namespace: default
spec:
  rules:
  - host: app.example.com
    http:
      paths:
      - path: /dashboard
`,
      related: [],
      events: [],
      eventsTruncated: false,
      pod: null,
      certificates: [],
      endpoints: [
        { url: "http://app.example.com/dashboard", openable: true, source: "rule app.example.com" },
        { url: "http://*.example.com/", openable: false, source: "rule *.example.com" },
      ],
      endpointsTruncated: false,
    };

    render(
      <ResourceDrawer
        row={ingressRow}
        detail={ingressDetail}
        onClose={vi.fn()}
        onOpenInBrowser={onOpen}
      />,
    );

    expect(await screen.findByTestId("summary-endpoints-card")).toBeInTheDocument();
    expect(screen.getByTestId("endpoint-item-0")).toBeInTheDocument();
    expect(screen.getByText("http://app.example.com/dashboard")).toBeInTheDocument();

    const openBtn = screen.getByRole("button", {
      name: "Open in browser http://app.example.com/dashboard",
    });
    expect(openBtn).toBeInTheDocument();
    await user.click(openBtn);
    expect(onOpen).toHaveBeenCalledWith("http://app.example.com/dashboard");

    // Wildcard hosts are listed but cannot be opened.
    expect(screen.getByText("http://*.example.com/")).toBeInTheDocument();
    expect(screen.getByTestId("endpoint-not-openable-1")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Open in browser http://*.example.com/" }),
    ).toBeNull();
  });

  it("05-05: does not derive endpoints from YAML when the backend sends none", () => {
    const ingressRow: Row = {
      ...sampleRow,
      uid: "uid-ingress-2",
      kind: "Ingress",
      name: "yaml-only",
    };
    render(
      <ResourceDrawer
        row={ingressRow}
        detail={{
          ...sampleDetail,
          kind: "Ingress",
          name: "yaml-only",
          yaml: "kind: Ingress\nspec:\n  rules:\n  - host: app.example.com\n",
          pod: null,
          endpoints: [],
          endpointsTruncated: false,
        }}
        onClose={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("summary-endpoints-card")).toBeNull();
  });

  it("05-05: says the endpoint list is capped when endpointsTruncated is set", () => {
    const routeRow: Row = { ...sampleRow, uid: "uid-route", kind: "HTTPRoute", name: "r" };
    render(
      <ResourceDrawer
        row={routeRow}
        detail={{
          ...sampleDetail,
          kind: "HTTPRoute",
          name: "r",
          pod: null,
          endpoints: [{ url: "http://a.example.com/", openable: true, source: "hostname a" }],
          endpointsTruncated: true,
        }}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("summary-endpoints-card")).toHaveTextContent("first 1");
  });

  it("05-05: renders ENDPOINTS card for Gateway with validated endpoints", async () => {
    const gatewayRow: Row = {
      uid: "uid-gw-1",
      cluster: "kind-dev",
      kind: "Gateway",
      namespace: "gateway-system",
      name: "prod-gateway",
      status: "healthy",
      statusReason: "Programmed",
      cells: [
        { type: "status", value: { label: "Programmed", phase: "healthy" } },
        { type: "text", value: "10.0.0.1" },
        { type: "timestamp", value: "2026-03-30T00:00:00Z" },
      ],
      labels: [],
      custom: [],
    };

    const gatewayDetail: ResourceDetail = {
      uid: "uid-gw-1",
      cluster: "kind-dev",
      kind: "Gateway",
      namespace: "gateway-system",
      name: "prod-gateway",
      yaml: `apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: prod-gateway
spec:
  listeners:
  - name: web
    port: 8080
    protocol: HTTP
    hostname: api.example.com
`,
      related: [],
      events: [],
      eventsTruncated: false,
      pod: null,
      certificates: [],
      endpoints: [
        { url: "http://api.example.com:8080/", openable: true, source: "listener web" },
      ],
      endpointsTruncated: false,
    };

    render(
      <ResourceDrawer
        row={gatewayRow}
        detail={gatewayDetail}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByTestId("summary-endpoints-card")).toBeInTheDocument();
    expect(screen.getByTestId("endpoint-item-0")).toBeInTheDocument();
    expect(screen.getByText("http://api.example.com:8080/")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Open in browser http://api.example.com:8080/" }),
    ).toBeInTheDocument();
  });

  it("05-05: renders active port forwards with Open in browser button in pod port-forward card", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();

    const podRow: Row = {
      uid: "uid-pod-1",
      cluster: "kind-dev",
      kind: "Pod",
      namespace: "default",
      name: "web-pod",
      status: "healthy",
      statusReason: "Running",
      cells: [
        { type: "text", value: "1/1" },
        { type: "status", value: { label: "Running", phase: "healthy" } },
        { type: "timestamp", value: "2026-03-30T00:00:00Z" },
      ],
      labels: [],
      custom: [],
    };

    const podDetail: ResourceDetail = {
      uid: "uid-pod-1",
      cluster: "kind-dev",
      kind: "Pod",
      namespace: "default",
      name: "web-pod",
      yaml: `apiVersion: v1
kind: Pod
metadata:
  name: web-pod
  namespace: default
spec:
  containers:
  - name: web
    ports:
    - containerPort: 80
`,
      related: [],
      events: [],
      eventsTruncated: false,
      pod: {
        node: "fleetglass-dev-worker",
        podIp: "10.244.1.7",
        qosClass: "BestEffort",
        serviceAccount: "default",
        priorityClass: null,
        conditions: [{ type: "Ready", status: "True" }],
        tolerations: [],
        containers: [
          {
            name: "web",
            image: "nginx:1.27",
            init: false,
            state: "Running",
            ready: true,
            restarts: 0,
            ports: [{ containerPort: 80, protocol: "TCP", name: "http" }],
            requests: { cpu: "100m", memory: "64Mi" },
            limits: { memory: "128Mi" },
          },
        ],
        uses: [],
      },
      certificates: [],
      endpoints: [],
      endpointsTruncated: false,
    };

    usePortForwardsStore.setState({
      forwards: [
        {
          id: 99,
          context: "kind-dev",
          namespace: "default",
          pod: "web-pod",
          remotePort: 80,
          localPort: 9090,
          browserUrl: "http://127.0.0.1:9090",
          activeConnections: 0,
          totalConnections: 0,
        },
      ],
    });

    render(
      <ResourceDrawer
        row={podRow}
        detail={podDetail}
        onClose={vi.fn()}
        onOpenInBrowser={onOpen}
      />,
    );

    // The real Pod path renders PodSummary, which must carry the active forwards.
    expect(await screen.findByTestId("pod-summary")).toBeInTheDocument();
    expect(screen.getByTestId("pod-active-forwards-card")).toBeInTheDocument();
    expect(screen.getByTestId("active-pod-forwards")).toBeInTheDocument();
    expect(screen.getByTestId("active-pf-99")).toBeInTheDocument();
    expect(screen.getByText("9090→80")).toBeInTheDocument();

    const openBtn = screen.getByRole("button", {
      name: "Open in browser http://127.0.0.1:9090",
    });
    expect(openBtn).toBeInTheDocument();
    await user.click(openBtn);
    expect(onOpen).toHaveBeenCalledWith("http://127.0.0.1:9090");
  });
});
