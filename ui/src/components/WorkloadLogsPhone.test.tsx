import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MuxLogEvent, ResourceDetail, WorkloadLogTarget } from "../generated/bindings";
import { useAppInfoStore } from "../stores/appInfo";
import type { Row } from "../stores/pods";
import { setWorkloadLogsClient, type WorkloadLogsClient } from "../stores/workloadLogs";
import { ResourceDrawer } from "./ResourceDrawer";
import { WorkloadLogsView } from "./WorkloadLogsView";

// 03-04b: the phone log toolbar, previous container, Share and the crash-loop hint.
describe("WorkloadLogsView on the phone", () => {
  let emitEvent: (event: MuxLogEvent) => void = () => {};
  let fakeClient: WorkloadLogsClient;
  let start: ReturnType<typeof vi.fn<WorkloadLogsClient["startWorkloadLogs"]>>;
  const podTarget: WorkloadLogTarget = {
    cluster: "dev",
    namespace: "default",
    selector: { type: "pod", name: "crashloop" },
    container: null,
    tailLines: 500,
  };
  const labelsTarget: WorkloadLogTarget = {
    ...podTarget,
    selector: { type: "labels", selector: "app=web" },
  };

  beforeEach(() => {
    start = vi.fn<WorkloadLogsClient["startWorkloadLogs"]>().mockResolvedValue({ status: "ok", data: 7 });
    fakeClient = {
      createChannel: (onEvent) => {
        emitEvent = onEvent;
        return { id: 1, send: vi.fn() } as unknown as ReturnType<WorkloadLogsClient["createChannel"]>;
      },
      startWorkloadLogs: start,
      stopWorkloadLogs: vi.fn().mockResolvedValue({ status: "ok", data: true }),
      openDetachedLogs: vi.fn().mockResolvedValue({ status: "ok", data: null }),
    };
    setWorkloadLogsClient(fakeClient);
    useAppInfoStore.setState({ platform: "android" });
  });

  afterEach(() => {
    setWorkloadLogsClient(undefined);
    useAppInfoStore.setState({ platform: null });
  });

  const lastTarget = () => start.mock.calls.at(-1)?.[0];

  it("touch-sized toolbar: search, Pause/Follow, Wrap, Previous, Share, level, range", async () => {
    render(<WorkloadLogsView phone target={podTarget} />);
    const toolbar = await screen.findByTestId("logs-phone-toolbar");
    const t = within(toolbar);
    expect(t.getByRole("searchbox", { name: "Search logs (regex or text)" }).closest("label")).toHaveClass(
      "workload-logs__ctl",
    );
    for (const name of ["Pause", "Wrap", "Previous", "Share"]) {
      expect(t.getByRole("button", { name })).toHaveClass("btn", "workload-logs__ctl");
    }
    expect(t.getByRole("combobox", { name: "Log level" })).toHaveClass("workload-logs__ctl");
    expect(t.getByRole("combobox", { name: "Time range" })).toHaveClass("workload-logs__ctl");
    expect(t.queryByRole("button", { name: "Download" })).toBeNull();
    expect(t.queryByRole("button", { name: "Pop out" })).toBeNull();
    expect(screen.getByRole("region", { name: /Workload logs/ })).toHaveClass("workload-logs--phone");
  });

  it("search filters lines by regex, falling back to plain text for an invalid regex", async () => {
    const user = userEvent.setup();
    render(<WorkloadLogsView phone target={podTarget} />);
    await waitFor(() => {
      expect(start).toHaveBeenCalled();
    });
    emitEvent({
      type: "lines",
      lines: [
        { seq: 1, pod: "crashloop", container: "main", text: "ERROR timeout (a", json: false },
        { seq: 2, pod: "crashloop", container: "main", text: "ok", json: false },
      ],
    });
    await screen.findByText("ok");
    const search = screen.getByRole("searchbox", { name: "Search logs (regex or text)" });
    await user.type(search, "error.*timeout");
    expect(screen.queryByText("ok")).toBeNull();
    expect(screen.getByText("ERROR timeout (a")).toBeInTheDocument();
    await user.clear(search);
    await user.type(search, "(a");
    expect(screen.getByText("ERROR timeout (a")).toBeInTheDocument();
    expect(screen.queryByText("ok")).toBeNull();
  });

  it("wrap toggle switches the nowrap class", async () => {
    const user = userEvent.setup();
    render(<WorkloadLogsView phone target={podTarget} />);
    const region = await screen.findByRole("region", { name: /Workload logs/ });
    expect(region).toHaveAttribute("data-wrap", "true");
    await user.click(screen.getByRole("button", { name: "Wrap" }));
    expect(region).toHaveAttribute("data-wrap", "false");
    expect(region).toHaveClass("workload-logs--nowrap");
  });

  it("Previous restarts the stream with previous: true and shows the previous-instance state", async () => {
    const user = userEvent.setup();
    render(<WorkloadLogsView phone target={podTarget} />);
    await waitFor(() => {
      expect(start).toHaveBeenCalledTimes(1);
    });
    expect(lastTarget()).not.toHaveProperty("previous");

    await user.click(screen.getByTestId("logs-previous-toggle"));
    await waitFor(() => {
      expect(start).toHaveBeenCalledTimes(2);
    });
    expect(lastTarget()).toMatchObject({ selector: { type: "pod", name: "crashloop" }, previous: true });
    expect(screen.getByTestId("logs-previous-toggle")).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByTestId("logs-previous-banner")).toHaveTextContent("Previous instance");
    // One-shot fetch: no Pause/Follow while showing the previous instance.
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Follow" })).toBeNull();
    await waitFor(() => {
      expect(screen.getByTestId("workload-logs-status")).toHaveTextContent("Previous instance (not following)");
    });

    // No previous instance: the backend's message is shown as is.
    emitEvent({ type: "error", message: "No previous container logs for main" });
    expect(await screen.findByText("No previous container logs for main")).toBeInTheDocument();

    await user.click(screen.getByTestId("logs-previous-toggle"));
    await waitFor(() => {
      expect(start).toHaveBeenCalledTimes(3);
    });
    expect(lastTarget()).not.toHaveProperty("previous");
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
  });

  it("no Previous toggle for a workload (label) target, and never previous: true", async () => {
    render(<WorkloadLogsView phone target={labelsTarget} />);
    await screen.findByTestId("logs-phone-toolbar");
    expect(screen.queryByTestId("logs-previous-toggle")).toBeNull();
    await waitFor(() => {
      expect(start).toHaveBeenCalled();
    });
    for (const call of start.mock.calls) expect(call[0]).not.toHaveProperty("previous");
  });

  it("Share sends the Download text through shareText; errors are shown", async () => {
    const user = userEvent.setup();
    const shareText = vi
      .fn<NonNullable<WorkloadLogsClient["shareText"]>>()
      .mockResolvedValueOnce({ status: "ok", data: null })
      .mockResolvedValueOnce({ status: "error", error: { kind: "internal", message: "no app" } });
    setWorkloadLogsClient({ ...fakeClient, shareText });
    render(<WorkloadLogsView phone target={podTarget} />);
    await waitFor(() => {
      expect(start).toHaveBeenCalled();
    });
    emitEvent({
      type: "lines",
      lines: [
        { seq: 1, pod: "crashloop", container: "main", text: "one", json: false },
        { seq: 2, pod: "crashloop", container: "main", text: "two", json: false },
      ],
    });
    await screen.findByText("two");
    await user.click(screen.getByRole("button", { name: "Share" }));
    expect(shareText).toHaveBeenCalledWith(
      "Logs crashloop",
      "crashloop.log",
      "crashloop/main one\ncrashloop/main two\n",
    );
    expect(screen.queryByTestId("workload-logs-share-error")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Share" }));
    expect(await screen.findByTestId("workload-logs-share-error")).toHaveTextContent("Could not share: no app");
  });

  it("Share of the previous instance names the file <pod>-previous.log", async () => {
    const user = userEvent.setup();
    const shareText = vi
      .fn<NonNullable<WorkloadLogsClient["shareText"]>>()
      .mockResolvedValue({ status: "ok", data: null });
    setWorkloadLogsClient({ ...fakeClient, shareText });
    render(<WorkloadLogsView phone target={podTarget} />);
    await user.click(await screen.findByTestId("logs-previous-toggle"));
    await waitFor(() => {
      expect(start).toHaveBeenCalledTimes(2);
    });
    emitEvent({
      type: "lines",
      lines: [{ seq: 1, pod: "crashloop", container: "main", text: "panic: boom", json: false }],
    });
    await screen.findByText("panic: boom");
    await user.click(screen.getByRole("button", { name: "Share" }));
    expect(shareText).toHaveBeenCalledWith("Logs crashloop", "crashloop-previous.log", "crashloop/main panic: boom\n");
  });

  it("desktop: Download and Pop out, no Share, no phone toolbar", async () => {
    useAppInfoStore.setState({ platform: "linux" });
    const shareText = vi.fn<NonNullable<WorkloadLogsClient["shareText"]>>();
    setWorkloadLogsClient({ ...fakeClient, shareText });
    render(<WorkloadLogsView target={podTarget} />);
    await waitFor(() => {
      expect(start).toHaveBeenCalled();
    });
    expect(screen.getByRole("button", { name: "Download" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pop out" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Share" })).toBeNull();
    expect(screen.queryByTestId("logs-phone-toolbar")).toBeNull();
    expect(screen.queryByTestId("logs-previous-toggle")).toBeNull();
    expect(shareText).not.toHaveBeenCalled();
  });

  it("not-running container with no live lines offers the previous instance", async () => {
    const user = userEvent.setup();
    render(<WorkloadLogsView phone notRunning target={podTarget} />);
    const offer = await screen.findByTestId("logs-previous-offer");
    expect(offer).toHaveTextContent("The container is not running — show the previous instance's logs?");
    await user.click(within(offer).getByRole("button", { name: "Show previous logs" }));
    await waitFor(() => {
      expect(lastTarget()).toMatchObject({ previous: true });
    });
    expect(screen.queryByTestId("logs-previous-offer")).toBeNull();
  });

  it("no hint for a running container or once live lines arrive", async () => {
    const { unmount } = render(<WorkloadLogsView phone target={podTarget} />);
    await waitFor(() => {
      expect(start).toHaveBeenCalled();
    });
    expect(screen.queryByTestId("logs-previous-offer")).toBeNull();
    unmount();

    render(<WorkloadLogsView phone notRunning target={podTarget} />);
    await screen.findByTestId("logs-previous-offer");
    emitEvent({
      type: "lines",
      lines: [{ seq: 1, pod: "crashloop", container: "main", text: "starting", json: false }],
    });
    await screen.findByText("starting");
    expect(screen.queryByTestId("logs-previous-offer")).toBeNull();
  });
});

describe("ResourceDrawer screen: crash-loop detection for the logs hint", () => {
  let start: ReturnType<typeof vi.fn<WorkloadLogsClient["startWorkloadLogs"]>>;
  const row: Row = {
    uid: "uid-crash",
    cluster: "kind-dev",
    kind: "Pod",
    namespace: "default",
    name: "crashloop",
    status: "failed",
    statusReason: "CrashLoopBackOff",
    cells: [],
    labels: [],
    custom: [],
  };

  beforeEach(() => {
    start = vi.fn<WorkloadLogsClient["startWorkloadLogs"]>().mockResolvedValue({ status: "ok", data: 1 });
    setWorkloadLogsClient({
      createChannel: () => ({ id: 1, send: vi.fn() }) as unknown as ReturnType<WorkloadLogsClient["createChannel"]>,
      startWorkloadLogs: start,
      stopWorkloadLogs: vi.fn().mockResolvedValue({ status: "ok", data: true }),
      openDetachedLogs: vi.fn().mockResolvedValue({ status: "ok", data: null }),
    });
    useAppInfoStore.setState({ platform: "android" });
  });

  afterEach(() => {
    setWorkloadLogsClient(undefined);
    useAppInfoStore.setState({ platform: null });
  });

  function detailWith(state: string, restarts: number): ResourceDetail {
    return {
      yaml: "",
      events: [],
      related: [],
      pod: {
        node: "n",
        podIp: "",
        qosClass: "",
        serviceAccount: "",
        priorityClass: null,
        conditions: [],
        tolerations: [],
        uses: [],
        containers: [
          {
            name: "main",
            image: "busybox",
            init: false,
            state,
            ready: false,
            restarts,
            ports: [],
            requests: { cpu: null, memory: null },
            limits: { cpu: null, memory: null },
          },
        ],
      },
    } as unknown as ResourceDetail;
  }

  it("Waiting container with restarts → hint; Running container → no hint", async () => {
    const user = userEvent.setup();
    const { unmount } = render(
      <ResourceDrawer variant="screen" remoteOps={false} row={row} detail={detailWith("Waiting", 4)} onClose={() => {}} />,
    );
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    expect(await screen.findByTestId("logs-previous-offer")).toBeInTheDocument();
    expect(screen.getByTestId("logs-phone-toolbar")).toBeInTheDocument();
    unmount();

    render(
      <ResourceDrawer
        variant="screen"
        remoteOps={false}
        row={{ ...row, status: "healthy", statusReason: "Running" }}
        detail={detailWith("Running", 4)}
        onClose={() => {}}
      />,
    );
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    await waitFor(() => {
      expect(start).toHaveBeenCalled();
    });
    expect(screen.queryByTestId("logs-previous-offer")).toBeNull();
  });

  it("without detail data, a CrashLoopBackOff row status still offers the hint", async () => {
    const user = userEvent.setup();
    render(<ResourceDrawer variant="screen" remoteOps={false} row={row} onClose={() => {}} />);
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    expect(await screen.findByTestId("logs-previous-offer")).toBeInTheDocument();
  });

  it("desktop drawer keeps the desktop toolbar", async () => {
    useAppInfoStore.setState({ platform: "linux" });
    const user = userEvent.setup();
    render(<ResourceDrawer row={row} onClose={() => {}} />);
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    await waitFor(() => {
      expect(start).toHaveBeenCalled();
    });
    expect(screen.queryByTestId("logs-phone-toolbar")).toBeNull();
    expect(screen.queryByTestId("logs-previous-offer")).toBeNull();
    expect(screen.getByRole("button", { name: "Download" })).toBeInTheDocument();
  });
});
