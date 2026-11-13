import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { PodSummary as PodSummaryData, ResourceDetail } from "../generated/bindings";
import type { Row } from "../stores/pods";
import { PodSummary } from "./PodSummary";
import { ResourceDrawer } from "./ResourceDrawer";

const samplePodSummary: PodSummaryData = {
  node: "fleetglass-dev-worker",
  podIp: "10.244.1.12",
  qosClass: "Burstable",
  serviceAccount: "web-sa",
  priorityClass: "high-priority",
  conditions: [
    {
      type: "PodScheduled",
      status: "True",
      reason: "Scheduled",
      message: "Successfully assigned to node",
      lastTransition: "2026-01-01T00:00:00Z",
    },
    {
      type: "Initialized",
      status: "True",
      lastTransition: "2026-01-01T00:00:01Z",
    },
    {
      type: "ContainersReady",
      status: "False",
      reason: "ContainersNotReady",
      message: "containers with unready status: [crash]",
      lastTransition: "2026-01-01T00:00:05Z",
    },
    {
      type: "Ready",
      status: "Unknown",
      reason: "NodeLost",
    },
  ],
  tolerations: ["node.kubernetes.io/not-ready:NoExecute"],
  containers: [
    {
      name: "init-setup",
      image: "busybox:latest",
      init: true,
      state: "Terminated",
      ready: true,
      restarts: 0,
      ports: [],
      requests: { cpu: null, memory: null },
      limits: { cpu: null, memory: null },
    },
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
    {
      name: "crash",
      image: "broken-app:v1",
      init: false,
      state: "Waiting",
      stateReason: "CrashLoopBackOff",
      ready: false,
      restarts: 14,
      lastTermination: "Error (1)",
      ports: [{ containerPort: 8080, protocol: "TCP" }],
      requests: { cpu: "50m", memory: "64Mi" },
      limits: { cpu: "200m", memory: "256Mi" },
    },
  ],
  uses: [
    {
      kind: "ConfigMap",
      name: "web-config",
      namespace: "default",
      uid: "uid-cm-1",
      relationship: "uses",
    },
    {
      kind: "Secret",
      name: "web-secret",
      namespace: "default",
      uid: "uid-sec-1",
      relationship: "uses",
    },
  ],
};

const sampleRow: Row = {
  uid: "uid-web-0",
  cluster: "kind-fleetglass-dev",
  kind: "Pod",
  namespace: "default",
  name: "web-7d9f8c6b5-k2x4p",
  status: "healthy",
  statusReason: "Running",
  cells: [],
  labels: [
    ["app", "web"],
    ["tier", "frontend"],
  ],
  custom: [],
};

const sampleDetail: ResourceDetail = {
  uid: "uid-web-0",
  cluster: "kind-fleetglass-dev",
  kind: "Pod",
  namespace: "default",
  name: "web-7d9f8c6b5-k2x4p",
  yaml: "apiVersion: v1\nkind: Pod\nmetadata:\n  name: web-7d9f8c6b5-k2x4p\n",
  related: [
    {
      kind: "ReplicaSet",
      name: "web-7d9f8c6b5",
      namespace: "default",
      uid: "uid-rs-1",
      relationship: "owner",
    },
  ],
  events: [],
  eventsTruncated: false,
  pod: samplePodSummary,
};

describe("PodSummary component (Task 02-05b)", () => {
  it("renders overview fields (Node, Pod IP, QoS class, Service account, Controlled by, Priority)", () => {
    render(<PodSummary detail={sampleDetail} row={sampleRow} />);

    expect(screen.getByText("Node")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "fleetglass-dev-worker" })).toBeInTheDocument();

    expect(screen.getByText("Pod IP")).toBeInTheDocument();
    expect(screen.getByText("10.244.1.12")).toBeInTheDocument();

    expect(screen.getByText("QoS class")).toBeInTheDocument();
    expect(screen.getByText("Burstable")).toBeInTheDocument();

    expect(screen.getByText("Service account")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "web-sa" })).toBeInTheDocument();

    expect(screen.getByText("Controlled by")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "ReplicaSet web-7d9f8c6b5" })).toBeInTheDocument();

    expect(screen.getByText("Priority")).toBeInTheDocument();
    expect(screen.getByText("high-priority")).toBeInTheDocument();
  });

  it("renders conditions as chips with appropriate status classes and tooltips", () => {
    render(<PodSummary detail={sampleDetail} row={sampleRow} />);

    const scheduledChip = screen.getByTestId("condition-PodScheduled");
    expect(scheduledChip).toHaveClass("chip", "ok");
    expect(scheduledChip).toHaveAttribute("title");
    expect(scheduledChip.getAttribute("title")).toContain("Reason: Scheduled");
    expect(scheduledChip.getAttribute("title")).toContain("Message: Successfully assigned");

    const containersReadyChip = screen.getByTestId("condition-ContainersReady");
    expect(containersReadyChip).toHaveClass("chip", "err");
    expect(containersReadyChip.getAttribute("title")).toContain("Reason: ContainersNotReady");

    const readyChip = screen.getByTestId("condition-Ready");
    expect(readyChip).toHaveClass("chip", "warn");
    expect(readyChip.getAttribute("title")).toContain("Reason: NodeLost");
  });

  it("renders container cards with state, image, ports, restarts, init label, and last termination", () => {
    render(<PodSummary detail={sampleDetail} row={sampleRow} />);

    // Init container
    const initContainer = screen.getByTestId("container-init-setup");
    expect(within(initContainer).getByText("Init: init-setup")).toBeInTheDocument();
    expect(within(initContainer).getByText("busybox:latest")).toBeInTheDocument();

    // Web container
    const webContainer = screen.getByTestId("container-web");
    expect(within(webContainer).getByText("web")).toBeInTheDocument();
    expect(within(webContainer).getByText("Running")).toBeInTheDocument();
    expect(within(webContainer).getByText("nginx:1.27-alpine")).toBeInTheDocument();
    expect(within(webContainer).getByText("80/TCP")).toBeInTheDocument();
    expect(within(webContainer).getByText(/1\/1 · 0 restarts/)).toBeInTheDocument();
    expect(within(webContainer).getByText("100m / 500m")).toBeInTheDocument();
    expect(within(webContainer).getByText("128Mi / 512Mi")).toBeInTheDocument();

    // Crash container
    const crashContainer = screen.getByTestId("container-crash");
    expect(within(crashContainer).getByText("crash")).toBeInTheDocument();
    expect(within(crashContainer).getByText("CrashLoopBackOff", { exact: false })).toBeInTheDocument();
    expect(within(crashContainer).getByText("broken-app:v1")).toBeInTheDocument();
    expect(within(crashContainer).getByText(/0\/1 · 14 restarts/)).toBeInTheDocument();
    expect(within(crashContainer).getByText("Error (1)")).toBeInTheDocument();
  });

  it("renders uses chips with kind and name", () => {
    render(<PodSummary detail={sampleDetail} row={sampleRow} />);

    const cmChip = screen.getByTestId("uses-chip-ConfigMap-web-config");
    expect(cmChip).toBeInTheDocument();
    expect(cmChip).toHaveTextContent("ConfigMap");
    expect(cmChip).toHaveTextContent("web-config");

    const secChip = screen.getByTestId("uses-chip-Secret-web-secret");
    expect(secChip).toBeInTheDocument();
    expect(secChip).toHaveTextContent("Secret");
    expect(secChip).toHaveTextContent("web-secret");
  });

  it("clicking uses chip triggers onOpenRelated callback", async () => {
    const user = userEvent.setup();
    const onOpenRelated = vi.fn();

    render(<PodSummary detail={sampleDetail} row={sampleRow} onOpenRelated={onOpenRelated} />);

    const cmChip = screen.getByTestId("uses-chip-ConfigMap-web-config");
    await user.click(cmChip);

    expect(onOpenRelated).toHaveBeenCalledTimes(1);
    expect(onOpenRelated).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "ConfigMap",
        name: "web-config",
      }),
    );
  });

  it("action buttons (Logs, Shell, Port-forward) switch tab or invoke handlers with container name", async () => {
    const user = userEvent.setup();
    const onSelectTab = vi.fn();
    const onPortForward = vi.fn();

    render(
      <PodSummary
        detail={sampleDetail}
        row={sampleRow}
        onSelectTab={onSelectTab}
        onPortForward={onPortForward}
      />,
    );

    const webCard = screen.getByTestId("container-web");
    const logsBtn = within(webCard).getByRole("button", { name: /logs/i });
    const shellBtn = within(webCard).getByRole("button", { name: /shell/i });
    const pfBtn = within(webCard).getByRole("button", { name: /port-forward/i });

    expect(logsBtn).toHaveTextContent("Logs");
    expect(shellBtn).toHaveTextContent("Shell");
    expect(pfBtn).toHaveTextContent("Port-forward");

    await user.click(logsBtn);
    expect(onSelectTab).toHaveBeenCalledWith("Logs", "web");

    await user.click(shellBtn);
    expect(onSelectTab).toHaveBeenCalledWith("Terminal", "web");

    await user.click(pfBtn);
    expect(onPortForward).toHaveBeenCalledWith("web", 80);
    expect(screen.getByTestId("port-forward-section-web")).toBeInTheDocument();
  });

  it("renders labels card as chips", () => {
    render(<PodSummary detail={sampleDetail} row={sampleRow} />);

    const labelsCard = screen.getByTestId("pod-labels-card");
    expect(within(labelsCard).getByText("LABELS")).toBeInTheDocument();
    expect(within(labelsCard).getByText("app=")).toBeInTheDocument();
    expect(within(labelsCard).getByText("web")).toBeInTheDocument();
    expect(within(labelsCard).getByText("tier=")).toBeInTheDocument();
    expect(within(labelsCard).getByText("frontend")).toBeInTheDocument();
  });
});

describe("ResourceDrawer integration with PodSummary", () => {
  it("renders PodSummary when row.kind === 'Pod' and detail.pod is present", () => {
    render(
      <ResourceDrawer
        row={sampleRow}
        detail={sampleDetail}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByTestId("pod-summary")).toBeInTheDocument();
    expect(screen.getByTestId("pod-overview-card")).toBeInTheDocument();
    expect(screen.getByTestId("pod-conditions-card")).toBeInTheDocument();
    expect(screen.getByTestId("pod-containers-card")).toBeInTheDocument();
  });

  it("retains generic summary view when detail.pod is absent or row is not a Pod", () => {
    const podWithoutSummary: ResourceDetail = {
      ...sampleDetail,
      pod: null,
    };

    render(
      <ResourceDrawer
        row={sampleRow}
        detail={podWithoutSummary}
        onClose={vi.fn()}
      />,
    );

    expect(screen.queryByTestId("pod-summary")).not.toBeInTheDocument();
    expect(screen.getByText("Cluster")).toBeInTheDocument();
    expect(screen.getByText("kind-fleetglass-dev")).toBeInTheDocument();
  });
});
