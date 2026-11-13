import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { commands } from "../generated/bindings";
import { DiagnosticsModal } from "./DiagnosticsModal";
import {
  ClusterNoticeBanner,
  ExecShellPanel,
  PortForwardBar,
} from "./OpsPanels";

describe("P1-02, P1-03, P1-09, P1-10, P1-13 Ops panels and diagnostics", () => {
  it("P1-02: DiagnosticsModal renders pass/warn/fail steps and RBAC allowed/denied rows", () => {
    const onClose = vi.fn();

    render(
      <DiagnosticsModal
        open={true}
        onClose={onClose}
        report={{
          context: "prod-east",
          steps: [
            { name: "dns", status: "pass", detail: "Resolved api.prod.internal (10.0.0.1)" },
            { name: "tls", status: "warn", detail: "Certificate expires in 5 days" },
            { name: "auth", status: "fail", detail: "Exec plugin token expired" },
          ],
          rbac: [
            { resource: "pods", verb: "list", allowed: true },
            { resource: "secrets", verb: "list", allowed: false },
          ],
        }}
      />,
    );

    expect(
      screen.getByRole("dialog", { name: "Connection diagnostics" }),
    ).toBeInTheDocument();

    const dnsStep = screen.getByTestId("diag-step-dns");
    expect(dnsStep).toHaveTextContent("PASS");
    expect(dnsStep).toHaveTextContent("Resolved api.prod.internal (10.0.0.1)");

    const tlsStep = screen.getByTestId("diag-step-tls");
    expect(tlsStep).toHaveTextContent("WARN");
    expect(tlsStep).toHaveTextContent("Certificate expires in 5 days");

    const authStep = screen.getByTestId("diag-step-auth");
    expect(authStep).toHaveTextContent("FAIL");
    expect(authStep).toHaveTextContent("Exec plugin token expired");

    const rbacTable = screen.getByTestId("diag-rbac-table");
    expect(within(rbacTable).getByText("pods")).toBeInTheDocument();
    expect(within(rbacTable).getByText("Allowed")).toBeInTheDocument();
    expect(within(rbacTable).getByText("secrets")).toBeInTheDocument();
    expect(within(rbacTable).getByText("Denied")).toBeInTheDocument();
  });

  it("04-02: DiagnosticsModal renders auth recovery section when authError is present and handles reconnect", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const onReconnect = vi.fn();

    // Mock clipboard API
    const writeTextMock = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: writeTextMock },
      configurable: true,
    });

    // Mock IPC
    const { mockIPC } = await import("@tauri-apps/api/mocks");
    mockIPC((cmd) => {
      if (cmd === "reconnect_context") return Promise.resolve();
    });

    render(
      <DiagnosticsModal
        open={true}
        onClose={onClose}
        onReconnect={onReconnect}
        report={{
          context: "eks-prod",
          steps: [
            { name: "auth", status: "fail", detail: "error: SSO session has expired" },
          ],
          rbac: [],
          authError: {
            provider: "aws-sso",
            loginCommand: "aws sso login",
            deviceCode: {
              url: "https://device.sso.us-east-1.amazonaws.com/",
              userCode: "ABCD-EFGH",
            },
          },
        }}
      />,
    );

    const recoveryCard = screen.getByTestId("diag-auth-recovery");
    expect(recoveryCard).toBeInTheDocument();
    expect(within(recoveryCard).getByText("Authentication Expired (aws-sso)")).toBeInTheDocument();

    const cmd = screen.getByTestId("diag-auth-cmd");
    expect(cmd).toHaveTextContent("aws sso login");

    const copyBtn = within(recoveryCard).getByRole("button", { name: "Copy" });
    await user.click(copyBtn);
    expect(writeTextMock).toHaveBeenCalledWith("aws sso login");

    const deviceSection = screen.getByTestId("diag-auth-device");
    expect(deviceSection).toHaveTextContent("ABCD-EFGH");
    expect(deviceSection).toHaveTextContent("https://device.sso.us-east-1.amazonaws.com/");

    const reconnectBtn = screen.getByTestId("diag-auth-reconnect-btn");
    await user.click(reconnectBtn);
    expect(onReconnect).toHaveBeenCalledWith("eks-prod");
  });

  it("P1-09: ExecShellPanel allows picking a container and shell and calling onStartExec, and disables exec when readOnly={true}", async () => {
    const user = userEvent.setup();
    const onStartExec = vi.fn();

    const { rerender } = render(
      <ExecShellPanel
        pod="web-0"
        containers={["app", "sidecar"]}
        readOnly={false}
        onStartExec={onStartExec}
      />,
    );

    expect(screen.getByTestId("exec-terminal")).toBeInTheDocument();
    expect(screen.queryByTestId("exec-readonly-notice")).toBeNull();

    const containerSelect = screen.getByLabelText("Select container");
    const shellSelect = screen.getByLabelText("Select shell");
    const connectBtn = screen.getByTestId("exec-connect-btn");

    expect(connectBtn).toBeEnabled();

    await user.selectOptions(containerSelect, "sidecar");
    await user.selectOptions(shellSelect, "/bin/bash");
    await user.click(connectBtn);

    expect(onStartExec).toHaveBeenCalledTimes(1);
    expect(onStartExec).toHaveBeenCalledWith("sidecar", "/bin/bash");

    rerender(
      <ExecShellPanel
        pod="web-0"
        containers={["app", "sidecar"]}
        readOnly={true}
        onStartExec={onStartExec}
      />,
    );

    expect(screen.getByTestId("exec-readonly-notice")).toHaveTextContent(
      "Exec disabled: context is read-only",
    );
    expect(screen.queryByTestId("exec-connect-btn")).toBeNull();
  });

  it("P1-10: PortForwardBar renders active forwards with browserUrl and calls onStop(id) when Stop is clicked", async () => {
    const user = userEvent.setup();
    const onStop = vi.fn();

    render(
      <PortForwardBar
        forwards={[
          {
            id: 42,
            context: "prod",
            namespace: "default",
            pod: "web-0",
            remotePort: 80,
            localPort: 8080,
            browserUrl: "http://127.0.0.1:8080",
          },
        ]}
        onStop={onStop}
      />,
    );

    const item = screen.getByTestId("pf-item-42");
    expect(item).toBeInTheDocument();

    // No dead `target=_blank` link: the URL is shown and copied.
    expect(within(item).queryByRole("link")).not.toBeInTheDocument();
    expect(within(item).getByText("http://127.0.0.1:8080")).toBeInTheDocument();
    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    await user.click(
      within(item).getByRole("button", { name: "Copy URL http://127.0.0.1:8080" }),
    );
    expect(writeText).toHaveBeenCalledWith("http://127.0.0.1:8080");
    expect(await within(item).findByText("Copied")).toBeInTheDocument();

    writeText.mockRejectedValueOnce(new Error("denied"));
    await user.click(
      within(item).getByRole("button", { name: "Copy URL http://127.0.0.1:8080" }),
    );
    expect(await within(item).findByRole("alert")).toHaveTextContent(
      "Could not copy; open http://127.0.0.1:8080 in your browser",
    );

    const stopBtn = within(item).getByRole("button", {
      name: "Stop forward 8080",
    });
    await user.click(stopBtn);

    expect(onStop).toHaveBeenCalledTimes(1);
    expect(onStop).toHaveBeenCalledWith(42);
  });

  it("P1-03 & P1-13: ClusterNoticeBanner shows No permission to list secrets on forbidden and metrics-server not installed when metrics-server is absent", () => {
    render(
      <ClusterNoticeBanner
        accessState={{
          state: "forbidden",
          resource: "secrets",
          namespace: "prod",
          message: "No permission to list secrets in namespace \"prod\"",
        }}
        metricsState={{
          state: "notInstalled",
          message: "metrics-server not installed",
        }}
      />,
    );

    const alert = screen.getByTestId("access-state-alert");
    expect(alert).toHaveAttribute("role", "alert");
    expect(alert).toHaveTextContent("No permission to list secrets");

    const metricsNotice = screen.getByTestId("metrics-not-installed");
    expect(metricsNotice).toHaveTextContent("metrics-server not installed");
  });

  it("04-02: ClusterNoticeBanner shows Auth Expired alert, copies device code, and triggers onRecoverAuth", async () => {
    const user = userEvent.setup();
    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    const onRecoverAuth = vi.fn();

    render(
      <ClusterNoticeBanner
        accessState={{
          state: "authExpired",
          provider: "aws-sso",
          hint: "Run 'aws sso login' to re-authenticate",
          loginCommand: "aws sso login",
          deviceCode: {
            url: "https://device.sso.us-east-1.amazonaws.com/",
            userCode: "BCDF-GHJK",
          },
        }}
        onRecoverAuth={onRecoverAuth}
      />,
    );

    const alert = screen.getByTestId("access-state-alert");
    expect(alert).toHaveAttribute("role", "alert");
    expect(alert).toHaveTextContent(
      "Authentication expired (aws-sso): Run 'aws sso login' to re-authenticate",
    );

    const deviceCodeSection = screen.getByTestId("banner-device-code");
    expect(deviceCodeSection).toHaveTextContent("Code: BCDF-GHJK");

    const copyBtn = screen.getByTestId("banner-copy-code-btn");
    await user.click(copyBtn);
    expect(writeText).toHaveBeenCalledWith("BCDF-GHJK");

    const recoverBtn = screen.getByTestId("banner-recover-btn");
    await user.click(recoverBtn);
    expect(onRecoverAuth).toHaveBeenCalledTimes(1);
  });

  it("04-03c: ExecShellPanel in attach mode formats kubectl attach preview and invokes startAttachSession", async () => {
    const user = userEvent.setup();
    const mockClient = {
      startExecSession: vi.fn(),
      startAttachSession: vi.fn().mockResolvedValue({ status: "ok", data: 99 }),
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
      <ExecShellPanel
        pod="api-backend-0"
        containers={["app", "helper"]}
        readOnly={false}
        cluster="prod-eu"
        namespace="apps"
        mode="attach"
        execClient={mockClient}
      />,
    );

    expect(screen.getByTestId("attach-terminal")).toBeInTheDocument();
    expect(screen.queryByLabelText("Select shell")).toBeNull();

    const preview = screen.getByTestId("attach-preview");
    expect(preview).toHaveTextContent(
      "kubectl --context prod-eu -n apps attach pod/api-backend-0 -c app -it",
    );

    const containerSelect = screen.getByLabelText("Select container");
    await user.selectOptions(containerSelect, "helper");
    expect(preview).toHaveTextContent(
      "kubectl --context prod-eu -n apps attach pod/api-backend-0 -c helper -it",
    );

    const attachBtn = screen.getByTestId("attach-connect-btn");
    expect(attachBtn).toHaveTextContent("Attach to Process");
    await user.click(attachBtn);

    expect(mockClient.startAttachSession).toHaveBeenCalledWith(
      "prod-eu",
      "apps",
      "api-backend-0",
      "helper",
      null,
      expect.anything(),
    );

    expect(screen.getByTestId("attach-status")).toHaveTextContent("Connected");

    const disconnectBtn = screen.getByTestId("attach-disconnect-btn");
    await user.click(disconnectBtn);
    expect(mockClient.stopExecSession).toHaveBeenCalledWith(99);
  });

  it("04-03c: ExecShellPanel enforces read-only and protected-context guards in attach mode", async () => {
    const user = userEvent.setup();
    const mockClient = {
      startExecSession: vi.fn(),
      startAttachSession: vi.fn().mockResolvedValue({ status: "ok", data: 12 }),
      writeExecStdin: vi.fn().mockResolvedValue({ status: "ok", data: null }),
      stopExecSession: vi.fn().mockResolvedValue({ status: "ok", data: null }),
    };

    const { rerender } = render(
      <ExecShellPanel
        pod="worker-1"
        containers={["worker"]}
        readOnly={true}
        cluster="prod"
        namespace="prod-ns"
        mode="attach"
        execClient={mockClient}
      />,
    );

    expect(screen.getByTestId("attach-readonly-notice")).toHaveTextContent(
      "Attach disabled: context is read-only",
    );
    expect(screen.getByTestId("attach-connect-btn")).toBeDisabled();

    rerender(
      <ExecShellPanel
        pod="worker-1"
        containers={["worker"]}
        readOnly={false}
        cluster="prod"
        namespace="prod-ns"
        mode="attach"
        isProtected={true}
        execClient={mockClient}
      />,
    );

    const attachBtn = screen.getByTestId("attach-connect-btn");
    expect(attachBtn).toBeDisabled();

    const guardInput = screen.getByTestId("exec-confirm-input");
    await user.type(guardInput, "wrong-name");
    expect(attachBtn).toBeDisabled();

    await user.clear(guardInput);
    await user.type(guardInput, "worker-1");
    expect(attachBtn).toBeEnabled();

    await user.click(attachBtn);
    expect(mockClient.startAttachSession).toHaveBeenCalledWith(
      "prod",
      "prod-ns",
      "worker-1",
      "worker",
      "worker-1",
      expect.anything(),
    );
  });

  it("04-03c: ExecShellPanel allows switching between exec and attach mode via the mode dropdown", async () => {
    const user = userEvent.setup();
    render(
      <ExecShellPanel
        pod="redis-0"
        containers={["redis"]}
        readOnly={false}
        cluster="staging"
        namespace="cache"
        mode="exec"
      />,
    );

    expect(screen.getByLabelText("Select shell")).toBeInTheDocument();
    expect(screen.getByTestId("exec-preview")).toHaveTextContent(
      "kubectl --context staging -n cache exec -it redis-0 -c redis -- /bin/sh",
    );
    expect(screen.getByTestId("exec-connect-btn")).toHaveTextContent("Connect Shell");

    const modeSelect = screen.getByLabelText("Select mode");
    await user.selectOptions(modeSelect, "attach");

    expect(screen.queryByLabelText("Select shell")).toBeNull();
    expect(screen.getByTestId("attach-preview")).toHaveTextContent(
      "kubectl --context staging -n cache attach pod/redis-0 -c redis -it",
    );
    expect(screen.getByTestId("attach-connect-btn")).toHaveTextContent("Attach to Process");
  });

  it("04-10: ExecShellPanel allows opening in external terminal", async () => {
    const user = userEvent.setup();
    const openExtTermSpy = vi.spyOn(commands, "openExternalTerminal").mockResolvedValue({ status: "ok", data: null } as never);

    render(
      <ExecShellPanel
        pod="web-app-1"
        containers={["app"]}
        readOnly={false}
        cluster="prod"
        namespace="prod-ns"
        mode="exec"
      />,
    );

    const extBtn = screen.getByTestId("exec-external-terminal-btn");
    expect(extBtn).toBeEnabled();

    await user.click(extBtn);
    expect(openExtTermSpy).toHaveBeenCalledWith({
      target: {
        kind: "podExec",
        cluster: "prod",
        namespace: "prod-ns",
        pod: "web-app-1",
        container: "app",
        shell: "/bin/sh",
      },
      confirmedName: null,
    });
  });
});
