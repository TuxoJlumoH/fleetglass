import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type {
  IpcError,
  PortForwardInfo_Serialize,
  SavedPortForward,
} from "../generated/bindings";
import { useAppInfoStore } from "../stores/appInfo";
import {
  initialPortForwardsState,
  resetPortForwardsStore,
  usePortForwardsStore,
} from "../stores/portForwards";
import { containerPortsFromYaml } from "./containerPorts";
import { PortForwardForm } from "./PortForwardForm";
import { StatusBar } from "./StatusBar";

const SAMPLE_FORWARD: PortForwardInfo_Serialize = {
  id: 1,
  context: "fleetglass-dev",
  namespace: "default",
  pod: "nginx-7854ff8877-abcde",
  remotePort: 80,
  localPort: 8080,
  browserUrl: "http://127.0.0.1:8080",
  activeConnections: 0,
  totalConnections: 0,
};

interface IpcCall {
  cmd: string;
  args: unknown;
}

describe("PortForward UI and Store (Task R-09)", () => {
  beforeEach(() => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    resetPortForwardsStore();
    useAppInfoStore.setState({ version: null });
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
    resetPortForwardsStore();
    useAppInfoStore.setState({ version: null });
  });

  it("StatusBar loads forwards on mount via list_port_forwards and renders PortForwardBar with active forwards", async () => {
    const calls: IpcCall[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "list_port_forwards") {
        return [SAMPLE_FORWARD];
      }
      return undefined;
    });

    useAppInfoStore.setState({ version: "0.2.1" });

    render(<StatusBar />);

    // Renders active forward target
    expect(
      await screen.findByText("fleetglass-dev/default/nginx-7854ff8877-abcde 8080→80"),
    ).toBeInTheDocument();

    // Shows browserUrl with a copy button (no dead target=_blank link)
    expect(screen.getByText("http://127.0.0.1:8080")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Copy URL http://127.0.0.1:8080" }),
    ).toBeInTheDocument();

    // Preserves the version item (now on the right, subtle)
    expect(screen.getByRole("contentinfo")).toBeInTheDocument();
    expect(screen.getByTestId("app-version")).toHaveTextContent("v0.2.1");

    expect(calls.some((c) => c.cmd === "list_port_forwards")).toBe(true);
  });

  it("Clicking the Stop button in PortForwardBar calls stop_port_forward with forward ID and removes the item from the bar", async () => {
    const calls: IpcCall[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "list_port_forwards") {
        return [SAMPLE_FORWARD];
      }
      if (cmd === "stop_port_forward") {
        return true;
      }
      return undefined;
    });

    render(<StatusBar />);

    const stopButton = await screen.findByRole("button", { name: "Stop forward 8080" });
    expect(stopButton).toBeInTheDocument();

    await userEvent.click(stopButton);

    const stopCall = calls.find((c) => c.cmd === "stop_port_forward");
    expect(stopCall).toBeDefined();
    expect(stopCall?.args).toEqual({ id: 1 });

    await waitFor(() => {
      expect(screen.queryByTestId("pf-item-1")).not.toBeInTheDocument();
    });
    expect(screen.getByText("No active port forwards")).toBeInTheDocument();
  });

  it("startForward in usePortForwardsStore calls start_port_forward with correct arguments and updates forwards", async () => {
    const calls: IpcCall[] = [];
    const newForward: PortForwardInfo_Serialize = {
      id: 42,
      context: "fleetglass-dev",
      namespace: "production",
      pod: "api-backend-987",
      remotePort: 3000,
      localPort: 3001,
      browserUrl: "http://127.0.0.1:3001",
      activeConnections: 0,
      totalConnections: 0,
    };

    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "start_port_forward") {
        return newForward;
      }
      return undefined;
    });

    const result = await usePortForwardsStore
      .getState()
      .startForward("fleetglass-dev", "production", "api-backend-987", 3000, 3001);

    expect(result).toEqual(newForward);

    const startCall = calls.find((c) => c.cmd === "start_port_forward");
    expect(startCall?.args).toEqual({
      context: "fleetglass-dev",
      namespace: "production",
      pod: "api-backend-987",
      remotePort: 3000,
      localPort: 3001,
    });

    expect(usePortForwardsStore.getState().forwards).toEqual([newForward]);

    // Calling again with the same item doesn't duplicate it in store
    await usePortForwardsStore
      .getState()
      .startForward("fleetglass-dev", "production", "api-backend-987", 3000, 3001);
    expect(usePortForwardsStore.getState().forwards).toHaveLength(1);
  });

  it("handles backend errors in loadForwards and stopForward", async () => {
    const listErr: IpcError = {
      kind: "internal",
      message: "Failed to list active port forwards",
    };
    const stopErr: IpcError = {
      kind: "internal",
      message: "Forward ID 999 not found",
    };
    const startErr: IpcError = {
      kind: "invalidInput",
      message: "Port 8080 already in use",
    };

    mockIPC((cmd) => {
      if (cmd === "list_port_forwards") {
        // Tauri rejects with the serialized `IpcError` object, not an `Error`.
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw listErr;
      }
      if (cmd === "stop_port_forward") {
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw stopErr;
      }
      if (cmd === "start_port_forward") {
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw startErr;
      }
      return undefined;
    });

    // loadForwards error handling
    await usePortForwardsStore.getState().loadForwards();
    expect(usePortForwardsStore.getState().loading).toBe(false);
    expect(usePortForwardsStore.getState().error).toBe("Failed to list active port forwards");
    expect(usePortForwardsStore.getState().forwards).toEqual([]);

    // stopForward error handling
    usePortForwardsStore.setState({ forwards: [SAMPLE_FORWARD], error: null });
    await usePortForwardsStore.getState().stopForward(999);
    expect(usePortForwardsStore.getState().error).toBe("Forward ID 999 not found");
    expect(usePortForwardsStore.getState().forwards).toEqual([SAMPLE_FORWARD]);

    // startForward error handling
    const startRes = await usePortForwardsStore
      .getState()
      .startForward("fleetglass-dev", "default", "nginx", 80, 8080);
    expect(startRes).toBeNull();
    expect(usePortForwardsStore.getState().error).toBe("Port 8080 already in use");
  });

  it("reads declared container ports from pod YAML", () => {
    const yaml = [
      "spec:",
      "  containers:",
      "    - name: web",
      "      ports:",
      "        - containerPort: 8080",
      "          protocol: TCP",
      "        - name: metrics",
      "          containerPort: 9090",
      "    - name: sidecar",
      "      ports:",
      "        - containerPort: 8080",
    ].join("\n");
    expect(containerPortsFromYaml(yaml)).toEqual([8080, 9090]);
    expect(containerPortsFromYaml("spec: {}")).toEqual([]);
  });

  it("starts a forward from the drawer form using the first declared port", async () => {
    const calls: IpcCall[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "start_port_forward") return { ...SAMPLE_FORWARD, remotePort: 80, localPort: 51000, browserUrl: "http://127.0.0.1:51000" };
      return undefined;
    });
    const user = userEvent.setup();
    render(
      <PortForwardForm
        context="fleetglass-dev"
        namespace="default"
        pod="nginx-7854ff8877-abcde"
        yaml={"      ports:\n        - containerPort: 80\n"}
      />,
    );

    expect(screen.getByLabelText("Pod port")).toHaveValue("80");
    await user.click(screen.getByRole("button", { name: "Forward" }));

    expect(await screen.findByRole("status")).toHaveTextContent("http://127.0.0.1:51000");
    expect(calls.find((c) => c.cmd === "start_port_forward")?.args).toMatchObject({
      context: "fleetglass-dev",
      namespace: "default",
      pod: "nginx-7854ff8877-abcde",
      remotePort: 80,
      localPort: 0,
    });
    expect(usePortForwardsStore.getState().forwards).toHaveLength(1);
  });

  it("rejects invalid ports and shows backend errors", async () => {
    mockIPC((cmd) => {
      if (cmd === "start_port_forward") {
        const err: IpcError = { kind: "invalidInput", message: "Port 8080 already in use" };
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw err;
      }
      return undefined;
    });
    const user = userEvent.setup();
    render(<PortForwardForm context="dev" namespace="default" pod="nginx" yaml="" />);

    await user.type(screen.getByLabelText("Pod port"), "99999");
    await user.click(screen.getByRole("button", { name: "Forward" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("between 1 and 65535");

    await user.clear(screen.getByLabelText("Pod port"));
    await user.type(screen.getByLabelText("Pod port"), "80");
    await user.type(screen.getByLabelText("Local port"), "8080");
    await user.click(screen.getByRole("button", { name: "Forward" }));
    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent("Port-forward failed: Port 8080 already in use");
    });
  });

  it("renders active connection telemetry and star bookmark toggle in PortForwardBar", async () => {
    const telemetryForward: PortForwardInfo_Serialize = {
      id: 5,
      context: "fleetglass-dev",
      namespace: "default",
      pod: "web-app-1",
      remotePort: 8080,
      localPort: 9090,
      browserUrl: "http://127.0.0.1:9090",
      activeConnections: 3,
      totalConnections: 15,
    };

    const savedCalls: IpcCall[] = [];
    mockIPC((cmd, args) => {
      savedCalls.push({ cmd, args });
      if (cmd === "list_port_forwards") {
        return [telemetryForward];
      }
      if (cmd === "list_saved_port_forwards") {
        return [];
      }
      if (cmd === "save_port_forward") {
        return null;
      }
      return undefined;
    });

    render(<StatusBar />);

    // Telemetry badge
    expect(await screen.findByTestId("pf-conns-5")).toHaveTextContent("3 active");

    // Bookmark toggle
    const starBtn = screen.getByRole("button", { name: "Bookmark web-app-1" });
    expect(starBtn).toBeInTheDocument();
    expect(starBtn).toHaveTextContent("☆");

    await userEvent.click(starBtn);

    const saveCall = savedCalls.find((c) => c.cmd === "save_port_forward");
    expect(saveCall).toBeDefined();
    expect(saveCall?.args).toEqual({
      forward: {
        id: "fleetglass-dev/default/pod/web-app-1/8080",
        context: "fleetglass-dev",
        namespace: "default",
        targetKind: "pod",
        targetName: "web-app-1",
        containerPort: 8080,
        localPort: 9090,
        label: "web-app-1:8080",
      },
    });
  });

  it("renders saved port-forward bookmarks and allows starting and deleting them", async () => {
    const savedBookmark: SavedPortForward = {
      id: "bm-redis",
      context: "fleetglass-dev",
      namespace: "cache",
      targetKind: "service",
      targetName: "redis-master",
      containerPort: 6379,
      localPort: 6379,
      label: "Redis Master",
    };

    const startedForward: PortForwardInfo_Serialize = {
      id: 10,
      context: "fleetglass-dev",
      namespace: "cache",
      pod: "redis-master-0",
      remotePort: 6379,
      localPort: 6379,
      browserUrl: "http://127.0.0.1:6379",
      activeConnections: 0,
      totalConnections: 0,
    };

    const calls: IpcCall[] = [];
    mockIPC((cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === "list_port_forwards") {
        return [];
      }
      if (cmd === "list_saved_port_forwards") {
        return [savedBookmark];
      }
      if (cmd === "start_port_forward_target") {
        return startedForward;
      }
      if (cmd === "delete_saved_port_forward") {
        return true;
      }
      return undefined;
    });

    render(<StatusBar />);

    expect(await screen.findByText("Redis Master")).toBeInTheDocument();

    const startBtn = screen.getByRole("button", { name: "Start saved forward Redis Master" });
    await userEvent.click(startBtn);

    const startTargetCall = calls.find((c) => c.cmd === "start_port_forward_target");
    expect(startTargetCall).toBeDefined();
    expect(startTargetCall?.args).toEqual({
      context: "fleetglass-dev",
      namespace: "cache",
      targetKind: "service",
      targetName: "redis-master",
      containerPort: 6379,
      localPort: 6379,
    });

    const deleteBtn = screen.getByRole("button", { name: "Delete saved forward Redis Master" });
    await userEvent.click(deleteBtn);

    const deleteCall = calls.find((c) => c.cmd === "delete_saved_port_forward");
    expect(deleteCall).toBeDefined();
    expect(deleteCall?.args).toEqual({ id: "bm-redis" });
  });

  it("exports initialPortForwardsState for testing resets", () => {
    expect(initialPortForwardsState).toEqual({
      forwards: [],
      saved: [],
      loading: false,
      error: null,
    });
  });
});
