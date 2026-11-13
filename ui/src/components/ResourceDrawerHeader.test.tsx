import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ResourceDetail } from "../generated/bindings";
import { resetActionsStore, useActionsStore } from "../stores/actions";
import type { Row } from "../stores/pods";
import { ResourceDrawer } from "./ResourceDrawer";

// 02-07d: drawer header, action row, tab strip and summary cards.

const CREATED = "2026-11-10T09:00:00Z";
const NOW = new Date("2026-11-10T12:00:00Z").getTime();

const podRow: Row = {
  uid: "uid-web-0",
  cluster: "kind-dev",
  kind: "Pod",
  namespace: "shop",
  name: "web-0",
  status: "healthy",
  statusReason: "Running",
  cells: [
    { type: "ready", value: { ready: 1, total: 1 } },
    { type: "status", value: { label: "Running", phase: "healthy" } },
    { type: "int", value: 0 },
    // "Last restart" comes before "Age"; the header must use the creation time.
    { type: "timestamp", value: "2026-11-10T11:59:00Z" },
    { type: "text", value: "" },
    { type: "text", value: "node-1" },
    { type: "text", value: "10.244.0.10" },
    { type: "timestamp", value: CREATED },
  ],
  labels: [["app", "web"]],
  custom: [],
};

const deploymentRow: Row = {
  uid: "uid-deploy-web",
  cluster: "kind-dev",
  kind: "Deployment",
  namespace: "shop",
  name: "web",
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
};

const nodeRow: Row = {
  uid: "uid-node-1",
  cluster: "kind-dev",
  kind: "Node",
  namespace: null,
  name: "node-1",
  status: "healthy",
  statusReason: "Ready",
  cells: [
    { type: "status", value: { label: "Ready", phase: "healthy" } },
    { type: "timestamp", value: CREATED },
  ],
  labels: [],
  custom: [],
};

function eventRow(uid: string, reason: string): Row {
  return {
    uid,
    cluster: "kind-dev",
    kind: "Event",
    namespace: "shop",
    name: uid,
    status: "healthy",
    statusReason: reason,
    cells: [
      { type: "status", value: { label: "Normal", phase: "healthy" } },
      { type: "text", value: reason },
      { type: "text", value: `${reason} ok` },
      { type: "timestamp", value: "2026-11-10T11:00:00Z" },
    ],
    labels: [],
    custom: [],
  };
}

const podDetail: ResourceDetail = {
  uid: "uid-web-0",
  cluster: "kind-dev",
  kind: "Pod",
  namespace: "shop",
  name: "web-0",
  yaml: "apiVersion: v1\nkind: Pod\nmetadata:\n  name: web-0\n",
  related: [],
  events: [eventRow("ev-1", "Pulled"), eventRow("ev-2", "Started")],
  eventsTruncated: false,
  pod: {
    node: "node-1",
    podIp: "10.244.0.10",
    qosClass: "Burstable",
    serviceAccount: "default",
    priorityClass: null,
    conditions: [
      { type: "Ready", status: "True" },
      { type: "ContainersReady", status: "False", reason: "ContainersNotReady" },
    ],
    tolerations: [],
    containers: [
      {
        name: "web",
        image: "nginx:1.27-alpine",
        init: false,
        state: "Running",
        ready: true,
        restarts: 0,
        ports: [{ containerPort: 80, protocol: "TCP" }],
        requests: { cpu: "100m", memory: "128Mi" },
        limits: { cpu: "500m", memory: "512Mi" },
      },
    ],
    uses: [
      { kind: "ConfigMap", name: "web-config", namespace: "shop", uid: "uid-cm", relationship: "uses" },
    ],
  },
};

function header() {
  return screen.getByTestId("drawer-header");
}

function action(id: string) {
  return screen.getByTestId(`drawer-action-${id}`);
}

describe("02-07d ResourceDrawer header", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["Date"], now: NOW });
    resetActionsStore();
  });

  afterEach(() => {
    vi.useRealTimers();
    resetActionsStore();
  });

  it("shows the name, a status pill in the row's tone and the Kind · namespace · cluster · age line", () => {
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);

    expect(within(header()).getByRole("heading", { level: 2, name: "web-0" })).toBeInTheDocument();
    const pill = screen.getByTestId("drawer-status-reason");
    expect(pill).toHaveClass("pill", "ok");
    expect(pill).toHaveTextContent("Running");
    expect(pill).toHaveAttribute("title", "Running");
    // Age from the last timestamp cell (creation), not the "Last restart" cell.
    expect(screen.getByTestId("drawer-meta")).toHaveTextContent(/^Pod · shop · kind-dev · 3h$/);
  });

  it("omits the namespace for cluster-scoped objects", () => {
    render(<ResourceDrawer row={nodeRow} readOnly={false} onClose={vi.fn()} />);

    expect(screen.getByTestId("drawer-meta")).toHaveTextContent(/^Node · kind-dev · 3h$/);
  });

  it("omits the age when the row carries no timestamp (drawer opened from a related link)", () => {
    render(
      <ResourceDrawer
        row={{ ...nodeRow, cells: [], statusReason: "" }}
        readOnly={false}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByTestId("drawer-meta")).toHaveTextContent(/^Node · kind-dev$/);
    // No reason, no made-up status pill.
    expect(screen.queryByTestId("drawer-status-reason")).toBeNull();
  });

  it("close button with an X icon calls onClose", async () => {
    vi.useRealTimers();
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<ResourceDrawer row={podRow} readOnly={false} onClose={onClose} />);

    const close = within(header()).getByRole("button", { name: "Close drawer" });
    expect(close.querySelector("svg")).not.toBeNull();
    await user.click(close);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("02-07d ResourceDrawer action row", () => {
  beforeEach(() => {
    resetActionsStore();
  });

  afterEach(() => {
    resetActionsStore();
  });

  it("pod: Logs and Shell switch tabs, Forward opens the port-forward form", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);

    await user.click(action("logs"));
    expect(screen.getByRole("tab", { name: "Logs" })).toHaveAttribute("aria-selected", "true");

    await user.click(action("shell"));
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByTestId("exec-terminal")).toBeInTheDocument();

    await user.click(action("attach"));
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByTestId("attach-terminal")).toBeInTheDocument();

    await user.click(action("forward"));
    expect(screen.getByRole("tab", { name: "Summary" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByTestId("port-forward-section-web")).toBeInTheDocument();

    // Pods are not scaled or restarted (same rule as the table and the palette).
    expect(screen.queryByTestId("drawer-action-scale")).toBeNull();
    expect(screen.queryByTestId("drawer-action-restart")).toBeNull();
  });

  it("node: Debug action opens the node debug modal (04-03d)", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={nodeRow} readOnly={false} onClose={vi.fn()} />);

    const debugBtn = action("debug-node");
    expect(debugBtn).toBeInTheDocument();
    await user.click(debugBtn);

    expect(screen.getByRole("dialog", { name: "Debug Node: node-1" })).toBeInTheDocument();
  });

  it("delete goes through the existing confirm flow with the drawer's row", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);

    const del = action("delete");
    expect(del).toHaveClass("btn", "sm", "danger");
    await user.click(del);

    expect(useActionsStore.getState().pending).toEqual({
      context: "kind-dev",
      action: { type: "delete", kind: "Pod", namespace: "shop", name: "web-0" },
      originUid: "uid-web-0",
    });
  });

  it("workload: Restart and Scale request the existing restart / scale actions", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={deploymentRow} readOnly={false} onClose={vi.fn()} />);

    expect(screen.queryByTestId("drawer-action-logs")).toBeNull();

    expect(screen.getByTestId("drawer-status-reason")).toHaveClass("pill", "warn");
    expect(screen.getByTestId("drawer-meta")).toHaveTextContent(/^Deployment · shop · kind-dev · /);

    await user.click(action("restart"));
    expect(useActionsStore.getState().pending?.action).toEqual({
      type: "restart",
      kind: "Deployment",
      namespace: "shop",
      name: "web",
    });

    useActionsStore.getState().cancel();
    await user.click(action("scale"));
    expect(useActionsStore.getState().pending?.action).toEqual({
      type: "scale",
      kind: "Deployment",
      namespace: "shop",
      name: "web",
      replicas: 3,
    });
  });

  it("read-only context disables the write buttons with a reason, but not Logs / Shell / Forward", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={deploymentRow} readOnly={true} onClose={vi.fn()} />);

    for (const id of ["restart", "scale", "delete"]) {
      const btn = action(id);
      expect(btn).toBeDisabled();
      expect(btn).toHaveAttribute("title", "Context kind-dev is read-only: write actions are disabled.");
    }
    await user.click(action("delete"));
    expect(useActionsStore.getState().pending).toBeNull();
  });

  it("read-only pod keeps the read actions enabled", () => {
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={true} onClose={vi.fn()} />);

    expect(action("logs")).toBeEnabled();
    expect(action("shell")).toBeEnabled();
    expect(action("forward")).toBeEnabled();
    expect(action("delete")).toBeDisabled();
  });

  it("shows kbd hints for the drawer keys whenever the action is available", () => {
    // No table selection at all (e.g. opened from a related link): the keys act on the drawer.
    const { unmount } = render(
      <ResourceDrawer row={deploymentRow} readOnly={false} onClose={vi.fn()} />,
    );

    expect(action("restart").querySelector("kbd")).toHaveTextContent(/^r$/);
    expect(action("restart")).toHaveAttribute("aria-keyshortcuts", "r");
    expect(action("scale").querySelector("kbd")).toHaveTextContent(/^s$/);
    expect(action("scale")).toHaveAttribute("aria-keyshortcuts", "s");
    expect(action("delete").querySelector("kbd")).toHaveTextContent(/^Ctrl\+D$/);
    expect(action("delete")).toHaveAttribute("aria-keyshortcuts", "Control+D");
    unmount();

    // Pod: Logs / Shell / Forward have no drawer key, so no hint.
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);
    for (const id of ["logs", "shell", "forward"]) {
      expect(action(id).querySelector("kbd")).toBeNull();
      expect(action(id)).not.toHaveAttribute("aria-keyshortcuts");
    }
    expect(action("delete").querySelector("kbd")).toHaveTextContent(/^Ctrl\+D$/);
  });
});

describe("02-07d ResourceDrawer action keys", () => {
  beforeEach(() => {
    resetActionsStore();
  });

  afterEach(() => {
    resetActionsStore();
  });

  it("r / s / Ctrl+D from a drawer button request the same actions as the buttons", async () => {
    const user = userEvent.setup();
    const outer = vi.fn();
    render(
      <div onKeyDown={outer}>
        <ResourceDrawer row={deploymentRow} readOnly={false} onClose={vi.fn()} />
      </div>,
    );

    action("restart").focus();
    await user.keyboard("r");
    expect(useActionsStore.getState().pending).toEqual({
      context: "kind-dev",
      action: { type: "restart", kind: "Deployment", namespace: "shop", name: "web" },
      originUid: "uid-deploy-web",
    });

    useActionsStore.getState().cancel();
    action("restart").focus();
    await user.keyboard("s");
    expect(useActionsStore.getState().pending).toEqual({
      context: "kind-dev",
      action: { type: "scale", kind: "Deployment", namespace: "shop", name: "web", replicas: 3 },
      originUid: "uid-deploy-web",
    });

    useActionsStore.getState().cancel();
    action("restart").focus();
    await user.keyboard("{Control>}d{/Control}");
    expect(useActionsStore.getState().pending).toEqual({
      context: "kind-dev",
      action: { type: "delete", kind: "Deployment", namespace: "shop", name: "web" },
      originUid: "uid-deploy-web",
    });

    // Handled keys never reach the table / global handlers.
    const handledKeys = outer.mock.calls
      .map(([e]) => (e as { key: string }).key)
      .filter((k) => k === "r" || k === "s" || k === "d");
    expect(handledKeys).toEqual([]);
  });

  it("does nothing in a read-only context", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={deploymentRow} readOnly={true} onClose={vi.fn()} />);

    screen.getByRole("region", { name: "Resource details" }).focus();
    await user.keyboard("r");
    await user.keyboard("s");
    await user.keyboard("{Control>}d{/Control}");
    expect(useActionsStore.getState().pending).toBeNull();
  });

  it("pod: r and s do nothing, Ctrl+D deletes", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);

    action("logs").focus();
    await user.keyboard("r");
    await user.keyboard("s");
    expect(useActionsStore.getState().pending).toBeNull();

    await user.keyboard("{Control>}d{/Control}");
    expect(useActionsStore.getState().pending).toEqual({
      context: "kind-dev",
      action: { type: "delete", kind: "Pod", namespace: "shop", name: "web-0" },
      originUid: "uid-web-0",
    });
  });

  it("never fires from the terminal, the YAML editor or the logs search", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <ResourceDrawer
        row={podRow}
        detail={podDetail}
        logLines={[{ seq: 1, text: "hello" }]}
        readOnly={false}
        onClose={vi.fn()}
      />,
    );

    // Logs search input.
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    const search = screen.getByRole("searchbox", { name: "Search logs (regex)" });
    search.focus();
    await user.keyboard("{Control>}d{/Control}");
    expect(useActionsStore.getState().pending).toBeNull();

    // Terminal container (xterm mounts inside it) and an xterm-like child.
    await user.click(screen.getByRole("tab", { name: "Terminal" }));
    const terminal = screen.getByTestId("exec-terminal");
    fireEvent.keyDown(terminal, { key: "d", ctrlKey: true });
    const xterm = document.createElement("div");
    xterm.className = "xterm";
    xterm.tabIndex = 0;
    terminal.appendChild(xterm);
    fireEvent.keyDown(xterm, { key: "d", ctrlKey: true });
    expect(useActionsStore.getState().pending).toBeNull();
    xterm.remove();

    // YAML editor (workload drawer so r / s would otherwise apply).
    rerender(<ResourceDrawer row={deploymentRow} readOnly={false} onClose={vi.fn()} />);
    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const editor = screen.getByLabelText("Resource YAML");
    for (const key of ["r", "s"]) fireEvent.keyDown(editor, { key });
    fireEvent.keyDown(editor, { key: "d", ctrlKey: true });
    const cm = editor.closest(".cm-editor") ?? editor;
    fireEvent.keyDown(cm, { key: "r" });
    expect(useActionsStore.getState().pending).toBeNull();
  });

  it("Escape still closes the drawer", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<ResourceDrawer row={deploymentRow} readOnly={false} onClose={onClose} />);
    action("scale").focus();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(useActionsStore.getState().pending).toBeNull();
  });
});

describe("02-07d ResourceDrawer tabs", () => {
  it("Events tab shows the count only when it is known", () => {
    const { rerender } = render(
      <ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />,
    );
    const events = screen.getByRole("tab", { name: "Events" });
    expect(within(events).getByTestId("events-count")).toHaveTextContent(/^2$/);

    rerender(<ResourceDrawer row={podRow} detail={null} loading readOnly={false} onClose={vi.fn()} />);
    expect(screen.queryByTestId("events-count")).toBeNull();

    rerender(
      <ResourceDrawer
        row={podRow}
        detail={podDetail}
        error="boom"
        readOnly={false}
        onClose={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("events-count")).toBeNull();
  });

  const fifty = () => Array.from({ length: 50 }, (_, i) => eventRow(`ev-${String(i)}`, "Pulled"));

  it("Events count is capped honestly when the backend truncated the list", async () => {
    const user = userEvent.setup();
    render(
      <ResourceDrawer
        row={podRow}
        detail={{ ...podDetail, events: fifty(), eventsTruncated: true }}
        readOnly={false}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("events-count")).toHaveTextContent(/^50\+$/);
    expect(screen.getByTestId("events-count")).toHaveAttribute("title", "Latest 50 events loaded");
    await user.click(screen.getByRole("tab", { name: "Events" }));
    expect(screen.getByTestId("events-limit")).toHaveTextContent("Showing latest 50 events");
  });

  it("exactly 50 events that are not truncated show a plain 50", async () => {
    const user = userEvent.setup();
    render(
      <ResourceDrawer
        row={podRow}
        detail={{ ...podDetail, events: fifty(), eventsTruncated: false }}
        readOnly={false}
        onClose={vi.fn()}
      />,
    );
    const count = screen.getByTestId("events-count");
    expect(count).toHaveTextContent(/^50$/);
    expect(count).toHaveAttribute("title", "50 events");
    await user.click(screen.getByRole("tab", { name: "Events" }));
    expect(screen.queryByTestId("events-limit")).toBeNull();
    expect(screen.getByRole("tabpanel")).not.toHaveTextContent(/latest 50/i);
    expect(screen.getAllByTestId("event-item")).toHaveLength(50);
  });

  it("tabs and the panel are linked with aria-controls / aria-labelledby", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);

    const panel = screen.getByRole("tabpanel");
    for (const tab of screen.getAllByRole("tab")) {
      expect(tab).toHaveAttribute("aria-controls", panel.id);
    }
    const summary = screen.getByRole("tab", { name: "Summary" });
    expect(panel).toHaveAttribute("aria-labelledby", summary.id);
    expect(screen.getByRole("tabpanel", { name: "Summary" })).toBe(panel);

    await user.click(screen.getByRole("tab", { name: "YAML" }));
    expect(panel).toHaveAttribute("aria-labelledby", screen.getByRole("tab", { name: "YAML" }).id);
  });

  it("roving tabindex: only the active tab is tabbable and arrow keys move between tabs", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);

    const summary = screen.getByRole("tab", { name: "Summary" });
    const yaml = screen.getByRole("tab", { name: "YAML" });
    const events = screen.getByRole("tab", { name: "Events" });
    expect(summary).toHaveAttribute("tabindex", "0");
    expect(yaml).toHaveAttribute("tabindex", "-1");

    summary.focus();
    await user.keyboard("{ArrowRight}");
    expect(yaml).toHaveFocus();
    expect(yaml).toHaveAttribute("aria-selected", "true");
    expect(yaml).toHaveAttribute("tabindex", "0");

    await user.keyboard("{End}");
    expect(events).toHaveFocus();
    await user.keyboard("{ArrowRight}");
    expect(summary).toHaveFocus();
    await user.keyboard("{ArrowLeft}");
    expect(events).toHaveFocus();
  });

  it("Escape from a header button still closes the drawer", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={onClose} />);

    action("logs").focus();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("02-07d summary cards", () => {
  it("pod summary renders the card headers, condition tones and kind-labelled uses chips", () => {
    render(<ResourceDrawer row={podRow} detail={podDetail} readOnly={false} onClose={vi.fn()} />);

    for (const title of ["OVERVIEW", "CONDITIONS", "USES", "LABELS"]) {
      expect(screen.getByRole("heading", { level: 4, name: title })).toBeInTheDocument();
    }
    const containers = screen.getByTestId("pod-containers-card");
    expect(within(containers).getByRole("heading", { level: 4 })).toHaveTextContent(/^CONTAINERS1$/);

    const ready = screen.getByTestId("condition-Ready");
    expect(ready).toHaveClass("chip", "ok");
    expect(ready.querySelector("svg")).not.toBeNull();
    expect(screen.getByTestId("condition-ContainersReady")).toHaveClass("chip", "err");

    const uses = screen.getByTestId("uses-chip-ConfigMap-web-config");
    expect(uses).toHaveClass("chip", "link");
    expect(uses.querySelector("svg")).not.toBeNull();
    expect(within(uses).getByText("ConfigMap")).toHaveClass("k");

    const web = screen.getByTestId("container-web");
    expect(within(web).getByText("nginx:1.27-alpine")).toHaveClass("mono");
    for (const label of ["Ready", "Ports", "CPU", "Memory"]) {
      expect(within(web).getByText(label)).toBeInTheDocument();
    }
    // Request is 20% of the CPU limit: said in text, the bar is a distinct req/lim style.
    const cpu = within(web).getByText("req 20% of lim");
    expect(cpu.parentElement).toHaveTextContent(/^100m \/ 500mreq 20% of lim$/);
    const cpuBar = cpu.closest(".use")?.querySelector(".bar");
    expect(cpuBar).toHaveClass("bar--reqlim");
    expect(cpuBar).not.toHaveAttribute("title");
    expect(cpuBar?.querySelector("i.req")).toHaveStyle({ width: "20%" });
    expect(within(web).getByText("req 25% of lim")).toBeInTheDocument();
  });

  it("request or limit alone shows text only, no bar", () => {
    const only = structuredClone(podDetail);
    const c = only.pod?.containers[0];
    if (c) {
      c.requests = { cpu: "100m" };
      c.limits = { memory: "512Mi" };
    }
    render(<ResourceDrawer row={podRow} detail={only} readOnly={false} onClose={vi.fn()} />);
    const web = screen.getByTestId("container-web");
    expect(within(web).getByText("100m / —")).toBeInTheDocument();
    expect(within(web).getByText("— / 512Mi")).toBeInTheDocument();
    expect(web.querySelector(".bar")).toBeNull();
    expect(within(web).queryByText(/% of lim/)).toBeNull();
  });

  it("generic summary renders OVERVIEW and LABELS cards", () => {
    render(<ResourceDrawer row={deploymentRow} readOnly={false} onClose={vi.fn()} />);

    expect(screen.getByRole("heading", { level: 4, name: "OVERVIEW" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 4, name: "LABELS" })).toBeInTheDocument();
    expect(screen.getByTestId("summary-overview-card").querySelector("dl.kv")).not.toBeNull();
  });
});
