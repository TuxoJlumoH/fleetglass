import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { commands } from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";
import { type Row } from "../stores/pods";
import { NodeDebugModal } from "./NodeDebugModal";

const testNodeRow: Row = {
  uid: "node-uid-1",
  cluster: "prod-cluster",
  kind: "Node",
  namespace: null,
  name: "worker-node-1",
  status: "healthy",
  statusReason: "Ready",
  cells: [],
  labels: [],
  custom: [],
};

describe("<NodeDebugModal /> (04-03d)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    useContextsStore.setState({
      settingsByContext: {
        "prod-cluster": {
          colour: null,
          tags: [],
          readOnly: false,
          protected: false,
          pinned: false,
        },
      },
    });

    vi.spyOn(commands, "previewDebugNode").mockResolvedValue(
      "kubectl --context prod-cluster debug node/worker-node-1 -it --image=busybox:1.37",
    );
  });

  it("does not render when closed", () => {
    const { container } = render(
      <NodeDebugModal open={false} onClose={vi.fn()} row={testNodeRow} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders presets and updates image and custom command", async () => {
    const user = userEvent.setup();
    render(<NodeDebugModal open={true} onClose={vi.fn()} row={testNodeRow} />);

    expect(screen.getByText("Debug Node: worker-node-1")).toBeInTheDocument();

    const netshootBtn = screen.getByText("Netshoot (networking & troubleshooting)");
    await user.click(netshootBtn);

    const imageInput = screen.getByTestId("node-debug-image-input");
    expect(imageInput).toHaveValue("nicolaka/netshoot");

    const cmdInput = screen.getByTestId("node-debug-cmd-input");
    await user.type(cmdInput, "chroot /host");
    expect(cmdInput).toHaveValue("chroot /host");
  });

  it("displays preview from previewDebugNode and copies to clipboard", async () => {
    const user = userEvent.setup();
    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();

    render(<NodeDebugModal open={true} onClose={vi.fn()} row={testNodeRow} />);

    await waitFor(() => {
      expect(screen.getByTestId("node-debug-preview")).toHaveTextContent(
        "kubectl --context prod-cluster debug node/worker-node-1 -it --image=busybox:1.37",
      );
    });

    const copyBtn = screen.getByTestId("node-debug-copy-btn");
    await user.click(copyBtn);
    expect(writeText).toHaveBeenCalledWith(
      "kubectl --context prod-cluster debug node/worker-node-1 -it --image=busybox:1.37",
    );
    expect(screen.getByText("Copied")).toBeInTheDocument();
  });

  it("enforces read-only context: shows alert and disables launch button", () => {
    useContextsStore.setState({
      settingsByContext: {
        "prod-cluster": {
          colour: null,
          tags: [],
          readOnly: true,
          protected: false,
          pinned: false,
        },
      },
    });

    render(<NodeDebugModal open={true} onClose={vi.fn()} row={testNodeRow} />);

    expect(screen.getByTestId("node-debug-readonly-notice")).toHaveTextContent(
      "Node debug disabled: context prod-cluster is read-only.",
    );
    expect(screen.getByTestId("node-debug-launch-btn")).toBeDisabled();
  });

  it("enforces protected context: requires typing node name before launch is enabled", async () => {
    useContextsStore.setState({
      settingsByContext: {
        "prod-cluster": {
          colour: null,
          tags: [],
          readOnly: false,
          protected: true,
          pinned: false,
        },
      },
    });

    const user = userEvent.setup();
    render(<NodeDebugModal open={true} onClose={vi.fn()} row={testNodeRow} />);

    const launchBtn = screen.getByTestId("node-debug-launch-btn");
    expect(launchBtn).toBeDisabled();

    const confirmInput = screen.getByTestId("node-debug-confirm-input");
    await user.type(confirmInput, "wrong-node");
    expect(launchBtn).toBeDisabled();

    await user.clear(confirmInput);
    await user.type(confirmInput, "worker-node-1");
    expect(launchBtn).toBeEnabled();
  });

  it("launches node debug shell, streams output, accepts input, and disconnects", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();

    const mockClient = {
      startExecSession: vi.fn(),
      startNodeDebugSession: vi.fn().mockResolvedValue({ status: "ok", data: 42 }),
      writeExecStdin: vi.fn().mockResolvedValue({ status: "ok", data: null }),
      stopExecSession: vi.fn().mockResolvedValue({ status: "ok", data: null }),
      createChannel: (cb: (chunk: string) => void) => ({
        id: 1,
        send: (chunk: string) => {
          cb(chunk);
        },
      }) as never,
    };

    render(
      <NodeDebugModal
        open={true}
        onClose={onClose}
        row={testNodeRow}
        execClient={mockClient}
      />,
    );

    const launchBtn = screen.getByTestId("node-debug-launch-btn");
    await user.click(launchBtn);

    expect(mockClient.startNodeDebugSession).toHaveBeenCalledWith(
      expect.objectContaining({
        context: "prod-cluster",
        node: "worker-node-1",
        image: "busybox:1.37",
      }),
      expect.anything(),
    );

    await waitFor(() => {
      expect(screen.getByTestId("node-debug-terminal")).toBeInTheDocument();
      expect(screen.getByTestId("node-debug-status")).toHaveTextContent("Connected");
    });

    const terminal = screen.getByTestId("node-debug-terminal");
    await user.click(terminal);
    await user.keyboard("ls -la{Enter}");

    expect(mockClient.writeExecStdin).toHaveBeenCalled();

    const disconnectBtn = screen.getByTestId("node-debug-disconnect-btn");
    await user.click(disconnectBtn);

    expect(mockClient.stopExecSession).toHaveBeenCalledWith(42);
    expect(onClose).toHaveBeenCalled();
  });

  it("04-10: NodeDebugModal allows opening in external terminal", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const openExtTermSpy = vi.spyOn(commands, "openExternalTerminal").mockResolvedValue({ status: "ok", data: null } as never);

    render(
      <NodeDebugModal
        open={true}
        onClose={onClose}
        row={testNodeRow}
      />,
    );

    const extBtn = screen.getByTestId("node-debug-external-terminal-btn");
    expect(extBtn).toBeEnabled();

    await user.click(extBtn);
    expect(openExtTermSpy).toHaveBeenCalledWith({
      target: {
        kind: "nodeDebug",
        cluster: "prod-cluster",
        namespace: null,
        node: "worker-node-1",
        image: "busybox:1.37",
      },
      confirmedName: null,
    });
    expect(onClose).toHaveBeenCalled();
  });
});
