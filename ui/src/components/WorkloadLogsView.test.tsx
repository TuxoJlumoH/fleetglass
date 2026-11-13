import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MuxLogEvent, WorkloadLogTarget } from "../generated/bindings";
import { logLevel, rangeOverrides } from "../lib/logLevel";
import { setWorkloadLogsClient, type WorkloadLogsClient } from "../stores/workloadLogs";
import { WorkloadLogsView } from "./WorkloadLogsView";

describe("<WorkloadLogsView />", () => {
  let emitEvent: (event: MuxLogEvent) => void = () => {};
  let fakeClient: WorkloadLogsClient;
  const target: WorkloadLogTarget = {
    cluster: "dev",
    namespace: "default",
    selector: { type: "labels", selector: "app=web" },
    container: null,
    tailLines: null,
  };

  beforeEach(() => {
    fakeClient = {
      createChannel: (onEvent) => {
        emitEvent = onEvent;
        return {
          id: 1,
          send: vi.fn(),
        } as unknown as ReturnType<WorkloadLogsClient["createChannel"]>;
      },
      startWorkloadLogs: vi.fn().mockResolvedValue({ status: "ok", data: 42 }),
      stopWorkloadLogs: vi.fn().mockResolvedValue({ status: "ok", data: true }),
      openDetachedLogs: vi.fn().mockResolvedValue({ status: "ok", data: null }),
    };
    setWorkloadLogsClient(fakeClient);
  });

  afterEach(() => {
    setWorkloadLogsClient(undefined);
  });

  it("renders lines from streamed events and dims pods on PodLeft", async () => {
    render(<WorkloadLogsView target={target} />);

    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });

    emitEvent({
      type: "lines",
      lines: [
        { seq: 1, pod: "web-pod-1", container: "nginx", text: "GET /index.html", json: false },
        { seq: 2, pod: "web-pod-2", container: "nginx", text: '{"level":"info","msg":"ready"}', json: true },
      ],
    });

    expect(await screen.findByText("GET /index.html")).toBeInTheDocument();
    expect(screen.getByText(/ready/)).toBeInTheDocument();

    const pod1Chip = screen.getByRole("button", { name: /web-pod-1/i });
    expect(pod1Chip).not.toHaveClass("workload-logs__chip--left");

    emitEvent({ type: "podLeft", pod: "web-pod-1" });

    await waitFor(() => {
      expect(screen.getByRole("button", { name: /web-pod-1/i })).toHaveClass(
        "workload-logs__chip--left",
      );
    });
  });

  it("single pod and container: no source column, message rendered", async () => {
    render(<WorkloadLogsView target={target} />);
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });

    emitEvent({ type: "podJoined", pod: "web-57b7df9cfc-4prg9" });
    emitEvent({
      type: "lines",
      lines: [
        {
          seq: 1,
          pod: "web-57b7df9cfc-4prg9",
          container: "nginx",
          text: '10.244.0.1 - - "GET / HTTP/1.1" 200 615 "-" "kube-probe/1.31"',
          json: false,
        },
      ],
    });

    const message = await screen.findByText(/GET \/ HTTP\/1\.1/);
    expect(message).toHaveClass("workload-logs__text");
    expect(screen.queryByTestId("log-source")).not.toBeInTheDocument();
    expect(screen.queryByText("web-57b7df9cfc-4prg9/nginx")).not.toBeInTheDocument();
  });

  it("multiple pods: source column with ellipsis class and full-name title", async () => {
    render(<WorkloadLogsView target={target} />);
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });

    emitEvent({
      type: "lines",
      lines: [
        { seq: 1, pod: "web-pod-1", container: "nginx", text: "GET /a", json: false },
        { seq: 2, pod: "web-pod-2", container: "nginx", text: "GET /b", json: false },
      ],
    });

    expect(await screen.findByText("GET /a")).toHaveClass("workload-logs__text");
    const sources = screen.getAllByTestId("log-source");
    expect(sources).toHaveLength(2);
    expect(sources[0]).toHaveClass("workload-logs__source");
    expect(sources[0]).toHaveTextContent("web-pod-1/nginx");
    expect(sources[0]).toHaveAttribute("title", "web-pod-1 / nginx");
  });

  it("one pod with several containers keeps the source column", async () => {
    render(<WorkloadLogsView target={target} />);
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });

    emitEvent({
      type: "lines",
      lines: [
        { seq: 1, pod: "web-pod-1", container: "nginx", text: "GET /a", json: false },
        { seq: 2, pod: "web-pod-1", container: "sidecar", text: "tick", json: false },
      ],
    });

    expect(await screen.findByText("tick")).toBeInTheDocument();
    expect(screen.getAllByTestId("log-source")).toHaveLength(2);
  });

  it("handles pop out in inline mode and hides it in detached mode", async () => {
    const user = userEvent.setup();
    const { unmount } = render(<WorkloadLogsView target={target} />);

    const popOutBtn = screen.getByRole("button", { name: /pop out/i });
    expect(popOutBtn).toBeInTheDocument();
    await user.click(popOutBtn);
    expect(fakeClient.openDetachedLogs).toHaveBeenCalledWith(target);

    unmount();

    render(<WorkloadLogsView target={target} detached />);
    expect(screen.queryByRole("button", { name: /pop out/i })).not.toBeInTheDocument();
  });

  it("renders error events in an honest alert banner", async () => {
    render(<WorkloadLogsView target={target} />);

    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });

    emitEvent({
      type: "error",
      message: "RBAC denied pod log access",
    });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("RBAC denied pod log access");
  });

  it("renders selector summary correctly for pod selector", () => {
    const podTarget: WorkloadLogTarget = {
      cluster: "kind-dev",
      namespace: "default",
      selector: { type: "pod", name: "nginx-0" },
      container: null,
      tailLines: 500,
    };
    render(<WorkloadLogsView target={podTarget} />);
    expect(
      screen.getByRole("region", { name: "Workload logs for kind-dev/default/pod/nginx-0" }),
    ).toBeInTheDocument();
    expect(screen.getByText("kind-dev/default/pod/nginx-0")).toBeInTheDocument();
  });

  it("02-06a level filter keeps only matching lines (JSON level field first)", async () => {
    const user = userEvent.setup();
    render(<WorkloadLogsView target={target} />);
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });
    emitEvent({
      type: "lines",
      lines: [
        { seq: 1, pod: "p", container: "c", text: "ERROR db down", json: false },
        { seq: 2, pod: "p", container: "c", text: "warn: slow query", json: false },
        { seq: 3, pod: "p", container: "c", text: "GET / 200", json: false },
        { seq: 4, pod: "p", container: "c", text: '{"level":"error","msg":"boom"}', json: true },
      ],
    });
    await screen.findByText("GET / 200");
    await user.selectOptions(screen.getByLabelText("Log level"), "error");
    await waitFor(() => {
      expect(screen.queryByText("GET / 200")).toBeNull();
    });
    expect(screen.getByText("ERROR db down")).toBeInTheDocument();
    expect(screen.queryByText("warn: slow query")).toBeNull();
    expect(screen.getAllByTestId("log-line")).toHaveLength(2);
    await user.selectOptions(screen.getByLabelText("Log level"), "warn");
    expect(await screen.findByText("warn: slow query")).toBeInTheDocument();
    expect(screen.getAllByTestId("log-line")).toHaveLength(1);
  });

  it("02-06a time range restarts the stream with sinceSeconds", async () => {
    const user = userEvent.setup();
    render(<WorkloadLogsView target={{ ...target, tailLines: 500 }} />);
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalledTimes(1);
    });
    await user.selectOptions(screen.getByLabelText("Time range"), "5m");
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalledTimes(2);
    });
    const calls = vi.mocked(fakeClient.startWorkloadLogs).mock.calls;
    const last = JSON.stringify(calls[calls.length - 1]);
    expect(last).toContain('"sinceSeconds":300');
    expect(last).toContain('"tailLines":null');
    expect(screen.queryByTestId("logs-range-hint")).toBeNull();
    await user.selectOptions(screen.getByLabelText("Time range"), "start");
    expect(await screen.findByTestId("logs-range-hint")).toHaveTextContent(
      "only the newest 50,000 lines are kept",
    );
  });

  it("02-06a Download saves loaded lines as <name>.log", async () => {
    const user = userEvent.setup();
    const createUrl = vi.fn().mockReturnValue("blob:x");
    const revokeUrl = vi.fn();
    Object.assign(URL, { createObjectURL: createUrl, revokeObjectURL: revokeUrl });
    const clicked: string[] = [];
    const clickSpy = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(function (this: HTMLAnchorElement) {
        clicked.push(this.download);
      });
    render(
      <WorkloadLogsView target={{ ...target, selector: { type: "pod", name: "web-1" } }} />,
    );
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });
    emitEvent({
      type: "lines",
      lines: [{ seq: 1, pod: "web-1", container: "nginx", text: "hello", json: false }],
    });
    await screen.findByText("hello");
    await user.click(screen.getByRole("button", { name: "Download" }));
    expect(clicked).toEqual(["web-1.log"]);
    const blob = createUrl.mock.calls[0]?.[0] as Blob;
    expect(await blob.text()).toBe("web-1/nginx hello\n");
    clickSpy.mockRestore();
  });

  it("02-06a Download uses the backend save command when the client has one", async () => {
    const user = userEvent.setup();
    const createUrl = vi.fn().mockReturnValue("blob:x");
    Object.assign(URL, { createObjectURL: createUrl, revokeObjectURL: vi.fn() });
    const saveLogFile = vi
      .fn<NonNullable<WorkloadLogsClient["saveLogFile"]>>()
      .mockResolvedValueOnce({ status: "ok", data: { path: "/x/web-1.log" } })
      .mockResolvedValueOnce({
        status: "error",
        error: { kind: "internal", message: "disk full" },
      });
    setWorkloadLogsClient({ ...fakeClient, saveLogFile });
    render(
      <WorkloadLogsView target={{ ...target, selector: { type: "pod", name: "web-1" } }} />,
    );
    await waitFor(() => {
      expect(fakeClient.startWorkloadLogs).toHaveBeenCalled();
    });
    emitEvent({
      type: "lines",
      lines: [{ seq: 1, pod: "web-1", container: "nginx", text: "hello", json: false }],
    });
    await screen.findByText("hello");
    await user.click(screen.getByRole("button", { name: "Download" }));
    expect(await screen.findByText("Saved to /x/web-1.log")).toBeInTheDocument();
    expect(saveLogFile).toHaveBeenCalledWith("web-1.log", "web-1/nginx hello\n");
    expect(createUrl).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Download" }));
    expect(await screen.findByText("Could not save: disk full")).toBeInTheDocument();
  });

  it("02-06a logLevel heuristic and range mapping", () => {
    expect(logLevel("panic: nil map", false)).toBe("error");
    expect(logLevel("WARNING disk 90%", false)).toBe("warn");
    expect(logLevel("3 errors while syncing", false)).toBe("error");
    expect(logLevel("GET / 200", false)).toBe("info");
    expect(logLevel('{"level":50,"msg":"x"}', true)).toBe("error");
    expect(logLevel('{"level":30,"msg":"warn in text"}', true)).toBe("info");
    expect(logLevel('{"level":"warn","msg":"error in text"}', true)).toBe("warn");
    expect(logLevel('{"msg":"fatal"}', true)).toBe("error");
    expect(rangeOverrides("1h")).toEqual({ tailLines: null, sinceSeconds: 3600 });
    expect(rangeOverrides("tail")).toEqual({});
  });
});
