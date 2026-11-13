import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ResourceDetail } from "../generated/bindings";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import type { Row } from "../stores/pods";
import {
  setExecClient,
  type ExecClient,
} from "./execTerminal";
import { ExecShellPanel } from "./OpsPanels";
import { ResourceDrawer } from "./ResourceDrawer";

interface IpcCall {
  cmd: string;
  args: Record<string, unknown>;
}

const samplePodRow: Row = {
  uid: "pod-uid-1",
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
    { type: "text", value: "kind-worker" },
    { type: "text", value: "10.244.0.5" },
    { type: "timestamp", value: "2026-03-30T00:00:00Z" },
  ],
  labels: [["app", "nginx"]],
  custom: [],
};

const sampleDeploymentRow: Row = {
  uid: "deploy-uid-1",
  cluster: "kind-dev",
  kind: "Deployment",
  namespace: "default",
  name: "nginx-deployment",
  status: "healthy",
  statusReason: "Available",
  cells: [
    { type: "ready", value: { ready: 3, total: 3 } },
    { type: "status", value: { label: "Available", phase: "healthy" } },
    { type: "int", value: 3 },
    { type: "timestamp", value: "2026-03-30T00:00:00Z" },
  ],
  labels: [["app", "nginx"]],
  custom: [],
};

const samplePodDetail: ResourceDetail = {
  uid: "pod-uid-1",
  cluster: "kind-dev",
  kind: "Pod",
  namespace: "default",
  name: "nginx-0",
  yaml: `apiVersion: v1
kind: Pod
metadata:
  name: nginx-0
  namespace: default
spec:
  containers:
  - name: nginx
    image: nginx:1.27
  - name: sidecar
    image: alpine:latest
`,
  related: [],
  events: [],
};

describe("R-10 Exec shell UI and ResourceDrawer integration", () => {
  beforeEach(() => {
    useContextsStore.setState({
      ...initialContextsState,
      selectedContext: "kind-dev",
      settingsByContext: {
        "kind-dev": {
          colour: "#4299e1",
          tags: ["dev"],
          readOnly: false,
          protected: false,
          pinned: false,
        },
      },
    });
  });

  afterEach(() => {
    setExecClient(undefined);
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("Terminal tab appears for Pods but not for non-Pod resources", () => {
    const { rerender } = render(<ResourceDrawer row={samplePodRow} onClose={() => {}} />);
    expect(screen.getByRole("tab", { name: "Terminal" })).toBeInTheDocument();

    rerender(<ResourceDrawer row={sampleDeploymentRow} onClose={() => {}} />);
    expect(screen.queryByRole("tab", { name: "Terminal" })).not.toBeInTheDocument();
  });

  it("extracts containers from Pod YAML and populates container selector", async () => {
    const user = userEvent.setup();
    render(
      <ResourceDrawer
        row={samplePodRow}
        detail={samplePodDetail}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Terminal" }));

    const containerSelect = screen.getByLabelText("Select container");
    expect(containerSelect).toHaveValue("nginx");
    expect(screen.getByRole("option", { name: "nginx" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "sidecar" })).toBeInTheDocument();
  });

  it("read-only context disables Connect Shell with alert", async () => {
    const user = userEvent.setup();
    render(
      <ResourceDrawer
        row={samplePodRow}
        detail={samplePodDetail}
        readOnly={true}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Terminal" }));

    const connectBtn = screen.getByTestId("exec-connect-btn");
    expect(connectBtn).toBeDisabled();

    const readonlyAlert = screen.getByRole("alert");
    expect(readonlyAlert).toHaveTextContent("Exec disabled: context is read-only");
  });

  it("Connect Shell invokes startExecSession with selected container and shell", async () => {
    const user = userEvent.setup();
    const startExecSession = vi.fn().mockResolvedValue({ status: "ok", data: 101 });
    const writeExecStdin = vi.fn().mockResolvedValue({ status: "ok", data: null });
    const stopExecSession = vi.fn().mockResolvedValue({ status: "ok", data: true });

    const fakeClient: ExecClient = {
      createChannel: () => ({ id: 1, send: vi.fn() } as unknown as ReturnType<NonNullable<ExecClient["createChannel"]>>),
      startExecSession,
      writeExecStdin,
      stopExecSession,
    };

    render(
      <ResourceDrawer
        row={samplePodRow}
        detail={samplePodDetail}
        execClient={fakeClient}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Terminal" }));

    const containerSelect = screen.getByLabelText("Select container");
    const shellSelect = screen.getByLabelText("Select shell");
    const connectBtn = screen.getByTestId("exec-connect-btn");

    await user.selectOptions(containerSelect, "sidecar");
    await user.selectOptions(shellSelect, "/bin/bash");
    await user.click(connectBtn);

    expect(startExecSession).toHaveBeenCalledTimes(1);
    expect(startExecSession).toHaveBeenCalledWith(
      "kind-dev",
      "default",
      "nginx-0",
      "sidecar",
      ["/bin/bash"],
      null,
      expect.anything(),
    );

    await waitFor(() => {
      expect(screen.getByTestId("exec-status")).toHaveTextContent("Connected");
      expect(screen.getByTestId("exec-disconnect-btn")).toBeInTheDocument();
    });
  });

  it("channel output chunks are displayed in the terminal", async () => {
    const user = userEvent.setup();
    let emitChunk: (chunk: string) => void = () => {};

    const fakeClient: ExecClient = {
      createChannel: (onOutput) => {
        emitChunk = onOutput;
        return { id: 1, send: vi.fn() } as unknown as ReturnType<NonNullable<ExecClient["createChannel"]>>;
      },
      startExecSession: vi.fn().mockResolvedValue({ status: "ok", data: 42 }),
      writeExecStdin: vi.fn().mockResolvedValue({ status: "ok", data: null }),
      stopExecSession: vi.fn().mockResolvedValue({ status: "ok", data: true }),
    };

    render(
      <ResourceDrawer
        row={samplePodRow}
        detail={samplePodDetail}
        execClient={fakeClient}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Terminal" }));
    await user.click(screen.getByTestId("exec-connect-btn"));

    await waitFor(() => {
      expect(screen.getByTestId("exec-disconnect-btn")).toBeInTheDocument();
    });

    emitChunk("root@nginx-0:/# ");
    await waitFor(() => {
      expect(screen.getByTestId("exec-output")).toHaveTextContent("root@nginx-0:/#");
    });

    emitChunk("ls -l\n-rw-r--r-- 1 root root index.html\n");
    await waitFor(() => {
      expect(screen.getByTestId("exec-output")).toHaveTextContent("index.html");
    });
  });

  it("keystrokes in the terminal send writeExecStdin", async () => {
    const user = userEvent.setup();
    const writeExecStdin = vi.fn().mockResolvedValue({ status: "ok", data: null });

    const fakeClient: ExecClient = {
      createChannel: () => ({ id: 1, send: vi.fn() } as unknown as ReturnType<NonNullable<ExecClient["createChannel"]>>),
      startExecSession: vi.fn().mockResolvedValue({ status: "ok", data: 77 }),
      writeExecStdin,
      stopExecSession: vi.fn().mockResolvedValue({ status: "ok", data: true }),
    };

    render(
      <ExecShellPanel
        pod="nginx-0"
        cluster="kind-dev"
        namespace="default"
        containers={["nginx"]}
        readOnly={false}
        execClient={fakeClient}
      />,
    );

    await user.click(screen.getByTestId("exec-connect-btn"));
    await waitFor(() => {
      expect(screen.getByTestId("exec-disconnect-btn")).toBeInTheDocument();
    });

    const terminal = screen.getByTestId("exec-terminal");
    await user.type(terminal, "ls");

    expect(writeExecStdin).toHaveBeenCalledWith(77, "l");
    expect(writeExecStdin).toHaveBeenCalledWith(77, "s");

    fireEvent.keyDown(terminal, { key: "Enter" });
    expect(writeExecStdin).toHaveBeenCalledWith(77, "\n");

    fireEvent.keyDown(terminal, { key: "Backspace" });
    expect(writeExecStdin).toHaveBeenCalledWith(77, "\x7f");

    fireEvent.keyDown(terminal, { key: "c", ctrlKey: true });
    expect(writeExecStdin).toHaveBeenCalledWith(77, "\x03");
  });

  it("Disconnect button calls stopExecSession", async () => {
    const user = userEvent.setup();
    const stopExecSession = vi.fn().mockResolvedValue({ status: "ok", data: true });

    const fakeClient: ExecClient = {
      createChannel: () => ({ id: 1, send: vi.fn() } as unknown as ReturnType<NonNullable<ExecClient["createChannel"]>>),
      startExecSession: vi.fn().mockResolvedValue({ status: "ok", data: 88 }),
      writeExecStdin: vi.fn().mockResolvedValue({ status: "ok", data: null }),
      stopExecSession,
    };

    render(
      <ResourceDrawer
        row={samplePodRow}
        detail={samplePodDetail}
        execClient={fakeClient}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Terminal" }));
    await user.click(screen.getByTestId("exec-connect-btn"));

    const disconnectBtn = await screen.findByTestId("exec-disconnect-btn");
    await user.click(disconnectBtn);

    expect(stopExecSession).toHaveBeenCalledTimes(1);
    expect(stopExecSession).toHaveBeenCalledWith(88);

    await waitFor(() => {
      expect(screen.getByTestId("exec-connect-btn")).toBeInTheDocument();
      expect(screen.queryByTestId("exec-disconnect-btn")).toBeNull();
    });
  });

  it("cleans up active session on tab switch and on drawer unmount", async () => {
    const user = userEvent.setup();
    const stopExecSession = vi.fn().mockResolvedValue({ status: "ok", data: true });

    const fakeClient: ExecClient = {
      createChannel: () => ({ id: 1, send: vi.fn() } as unknown as ReturnType<NonNullable<ExecClient["createChannel"]>>),
      startExecSession: vi.fn().mockResolvedValue({ status: "ok", data: 99 }),
      writeExecStdin: vi.fn().mockResolvedValue({ status: "ok", data: null }),
      stopExecSession,
    };

    const { unmount } = render(
      <ResourceDrawer
        row={samplePodRow}
        detail={samplePodDetail}
        execClient={fakeClient}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "Terminal" }));
    await user.click(screen.getByTestId("exec-connect-btn"));

    await screen.findByTestId("exec-disconnect-btn");

    // Switch tab to Summary
    await user.click(screen.getByRole("tab", { name: "Summary" }));

    expect(stopExecSession).toHaveBeenCalledWith(99);

    // Switch back to Terminal and connect again
    await user.click(screen.getByRole("tab", { name: "Terminal" }));
    await user.click(screen.getByTestId("exec-connect-btn"));
    await screen.findByTestId("exec-disconnect-btn");

    // Close drawer (unmount)
    unmount();
    expect(stopExecSession).toHaveBeenCalledTimes(2);
  });

  it("end-to-end with Tauri mockIPC", async () => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");

    const calls: IpcCall[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: args as Record<string, unknown> });
      if (cmd === "start_exec_session") {
        return 1234;
      }
      if (cmd === "write_exec_stdin") {
        return null;
      }
      if (cmd === "stop_exec_session") {
        return true;
      }
      return undefined;
    });

    const user = userEvent.setup();
    render(<ResourceDrawer row={samplePodRow} detail={samplePodDetail} onClose={() => {}} />);

    await user.click(screen.getByRole("tab", { name: "Terminal" }));
    await user.click(screen.getByTestId("exec-connect-btn"));

    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "start_exec_session")).toBe(true);
    });

    const startCall = calls.find((c) => c.cmd === "start_exec_session");
    expect(startCall?.args).toMatchObject({
      context: "kind-dev",
      namespace: "default",
      pod: "nginx-0",
      container: "nginx",
      command: ["/bin/sh"],
      confirmedName: null,
    });

    const disconnectBtn = await screen.findByTestId("exec-disconnect-btn");
    await user.click(disconnectBtn);

    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "stop_exec_session")).toBe(true);
    });
    const stopCall = calls.find((c) => c.cmd === "stop_exec_session");
    expect(stopCall?.args).toMatchObject({
      id: 1234,
    });
  });

  it("connects directly on unprotected context without confirmation, but prompts for pod name on protected context", async () => {
    const user = userEvent.setup();
    const startExecSession = vi.fn().mockResolvedValue({ status: "ok", data: 101 });
    const fakeClient: ExecClient = {
      createChannel: () => ({ id: 1, send: vi.fn() } as unknown as ReturnType<NonNullable<ExecClient["createChannel"]>>),
      startExecSession,
      writeExecStdin: vi.fn().mockResolvedValue({ status: "ok", data: null }),
      stopExecSession: vi.fn().mockResolvedValue({ status: "ok", data: true }),
    };

    // 1. Unprotected context (kind-dev is protected: false)
    const { rerender } = render(
      <ExecShellPanel
        pod="nginx-0"
        cluster="kind-dev"
        namespace="default"
        containers={["nginx"]}
        readOnly={false}
        execClient={fakeClient}
      />,
    );

    expect(screen.queryByTestId("exec-protected-guard")).not.toBeInTheDocument();
    const connectBtn = screen.getByTestId("exec-connect-btn");
    expect(connectBtn).toBeEnabled();

    await user.click(connectBtn);

    expect(startExecSession).toHaveBeenCalledTimes(1);
    expect(startExecSession).toHaveBeenCalledWith(
      "kind-dev",
      "default",
      "nginx-0",
      "nginx",
      ["/bin/sh"],
      null,
      expect.anything(),
    );

    // Disconnect so we can test protected
    await screen.findByTestId("exec-disconnect-btn");
    await user.click(screen.getByTestId("exec-disconnect-btn"));
    startExecSession.mockClear();

    // 2. Protected context
    useContextsStore.setState({
      ...initialContextsState,
      selectedContext: "kind-prod",
      settingsByContext: {
        "kind-prod": {
          colour: "#ef4444",
          tags: ["prod"],
          readOnly: false,
          protected: true,
          pinned: false,
        },
      },
    });

    rerender(
      <ExecShellPanel
        pod="nginx-0"
        cluster="kind-prod"
        namespace="default"
        containers={["nginx"]}
        readOnly={false}
        execClient={fakeClient}
      />,
    );

    // Protected guard should be visible and connect button disabled
    const guard = screen.getByTestId("exec-protected-guard");
    expect(guard).toBeInTheDocument();
    expect(guard).toHaveTextContent("Protected context 'kind-prod'. Type 'nginx-0' to confirm shell access:");

    const connectBtnProtected = screen.getByTestId("exec-connect-btn");
    expect(connectBtnProtected).toBeDisabled();

    const confirmInput = screen.getByTestId("exec-confirm-input");
    await user.type(confirmInput, "wrong-name");
    expect(connectBtnProtected).toBeDisabled();
    expect(startExecSession).not.toHaveBeenCalled();

    await user.clear(confirmInput);
    await user.type(confirmInput, "nginx-0");
    expect(connectBtnProtected).toBeEnabled();

    await user.click(connectBtnProtected);

    expect(startExecSession).toHaveBeenCalledTimes(1);
    expect(startExecSession).toHaveBeenCalledWith(
      "kind-prod",
      "default",
      "nginx-0",
      "nginx",
      ["/bin/sh"],
      "nginx-0",
      expect.anything(),
    );
  });
});
