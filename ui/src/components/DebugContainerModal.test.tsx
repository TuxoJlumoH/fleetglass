import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { commands } from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";
import { type Row } from "../stores/pods";
import { DebugContainerModal } from "./DebugContainerModal";

const testRow: Row = {
  uid: "pod-uid-1",
  cluster: "test-cluster",
  kind: "Pod",
  namespace: "default",
  name: "checkout-api-123",
  status: "healthy",
  statusReason: "Running",
  cells: [],
  labels: [],
  custom: [],
};

describe("<DebugContainerModal />", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    useContextsStore.setState({
      settingsByContext: {
        "test-cluster": {
          colour: null,
          tags: [],
          readOnly: false,
          protected: false,
          pinned: false,
        },
      },
    });

    vi.spyOn(commands, "previewDebugPod").mockResolvedValue(
      "kubectl --context test-cluster -n default debug pod/checkout-api-123 -it --image=nicolaka/netshoot --container=debugger -- /bin/bash"
    );
    vi.spyOn(commands, "debugPod").mockResolvedValue({
      status: "ok",
      data: { containerName: "debugger" },
    });
  });

  it("does not render when closed", () => {
    const { container } = render(
      <DebugContainerModal open={false} onClose={vi.fn()} row={testRow} />
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders presets and updates image when preset button is clicked", async () => {
    const user = userEvent.setup();
    render(<DebugContainerModal open={true} onClose={vi.fn()} row={testRow} />);

    expect(screen.getByText("Debug Pod: checkout-api-123")).toBeInTheDocument();
    const busyboxBtn = screen.getByText("BusyBox 1.37 (minimal utils)");
    await user.click(busyboxBtn);

    const imageInput = screen.getByPlaceholderText("e.g. nicolaka/netshoot, busybox:1.37, alpine");
    expect(imageInput).toHaveValue("busybox:1.37");
  });

  it("displays preview from previewDebugPod and copies to clipboard", async () => {
    const user = userEvent.setup();
    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue(undefined);

    render(<DebugContainerModal open={true} onClose={vi.fn()} row={testRow} />);

    await waitFor(() => {
      expect(screen.getByTestId("kubectl-preview")).toHaveTextContent("kubectl --context test-cluster");
    });

    const copyBtn = screen.getByRole("button", { name: "Copy as kubectl" });
    await user.click(copyBtn);

    expect(writeText).toHaveBeenCalledWith(
      "kubectl --context test-cluster -n default debug pod/checkout-api-123 -it --image=nicolaka/netshoot --container=debugger -- /bin/bash"
    );
    writeText.mockRestore();
  });

  it("disables launch button and shows alert when context is read-only", () => {
    useContextsStore.setState({
      settingsByContext: {
        "test-cluster": {
          colour: null,
          tags: [],
          readOnly: true,
          protected: false,
          pinned: false,
        },
      },
    });

    render(<DebugContainerModal open={true} onClose={vi.fn()} row={testRow} />);

    expect(screen.getByText(/Context.*is read-only/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Launch Debug Container" })).toBeDisabled();
  });

  it("requires typing the pod name when context is protected", async () => {
    const user = userEvent.setup();
    useContextsStore.setState({
      settingsByContext: {
        "test-cluster": {
          colour: null,
          tags: [],
          readOnly: false,
          protected: true,
          pinned: false,
        },
      },
    });

    render(<DebugContainerModal open={true} onClose={vi.fn()} row={testRow} />);

    const launchBtn = screen.getByRole("button", { name: "Launch Debug Container" });
    expect(launchBtn).toBeDisabled();

    const confirmInput = screen.getByPlaceholderText("checkout-api-123");
    await user.type(confirmInput, "wrong-name");
    expect(launchBtn).toBeDisabled();

    await user.clear(confirmInput);
    await user.type(confirmInput, "checkout-api-123");
    expect(launchBtn).not.toBeDisabled();
  });

  it("calls debugPod and triggers onClose on success", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<DebugContainerModal open={true} onClose={onClose} row={testRow} />);

    const launchBtn = screen.getByRole("button", { name: "Launch Debug Container" });
    await user.click(launchBtn);

    expect(commands.debugPod).toHaveBeenCalledWith(
      expect.objectContaining({
        context: "test-cluster",
        pod: "checkout-api-123",
        containerName: "debugger",
      })
    );
    expect(onClose).toHaveBeenCalled();
  });
});
