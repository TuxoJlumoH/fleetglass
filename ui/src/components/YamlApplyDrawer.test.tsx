import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import type { Row } from "../stores/pods";
import { resetResourceDetailStore, useResourceDetailStore } from "../stores/resourceDetail";
import { DiffModal } from "./DiffModal";
import { ResourceDrawer } from "./ResourceDrawer";

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

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

const originalYaml = `apiVersion: v1
kind: Pod
metadata:
  name: nginx-0
  namespace: default
spec:
  containers:
  - name: nginx
    image: nginx:1.27
`;

const modifiedYaml = `apiVersion: v1
kind: Pod
metadata:
  name: nginx-0
  namespace: default
spec:
  containers:
  - name: nginx
    image: nginx:1.28
`;

const appliedYaml = `apiVersion: v1
kind: Pod
metadata:
  name: nginx-0
  namespace: default
  generation: 2
spec:
  containers:
  - name: nginx
    image: nginx:1.28
`;

describe("R-05 Edit YAML: dry-run, diff, apply", () => {
  let calls: Call[];

  beforeEach(() => {
    calls = [];
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    resetResourceDetailStore();
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
    clearMocks();
    resetResourceDetailStore();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  function setupMockBackend(overrides: {
    dryRunError?: { kind: string; message: string };
    applyError?: { kind: string; message: string };
    diff?: { identical: boolean; unifiedDiff: string };
    settings?: { readOnly: boolean; protected: boolean };
  } = {}) {
    mockIPC((cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args: a });

      switch (cmd) {
        case "dry_run_apply":
          if (overrides.dryRunError) {
            throw Object.assign(new Error(overrides.dryRunError.message), overrides.dryRunError);
          }
          return modifiedYaml;

        case "apply_yaml":
          if (overrides.applyError) {
            throw Object.assign(new Error(overrides.applyError.message), overrides.applyError);
          }
          return appliedYaml;

        case "compare_resources":
          return (
            overrides.diff ?? {
              identical: false,
              unifiedDiff: "--- left\n+++ right\n-    image: nginx:1.27\n+    image: nginx:1.28",
            }
          );

        case "get_context_settings":
          return {
            colour: "#4299e1",
            tags: ["dev"],
            readOnly: overrides.settings?.readOnly ?? false,
            protected: overrides.settings?.protected ?? false,
            pinned: false,
          };

        default:
          return undefined;
      }
    });
  }

  it("Test 1: modifies YAML in drawer and calls dry-run with modified YAML", async () => {
    setupMockBackend();
    const user = userEvent.setup();

    render(
      <ResourceDrawer
        row={sampleRow}
        yamlText={originalYaml}
        onClose={() => {}}
      />,
    );

    // Switch to YAML tab
    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const editor = screen.getByLabelText("Resource YAML");
    expect(editor).toHaveValue(originalYaml);

    // Edit the YAML in the textarea
    fireEvent.change(editor, { target: { value: modifiedYaml } });
    expect(editor).toHaveValue(modifiedYaml);

    // Click "Review & Apply"
    const reviewBtn = screen.getByRole("button", { name: "Review & Apply" });
    await user.click(reviewBtn);

    // Verify dry_run_apply was called with the cluster and modified YAML
    const dryRunCall = calls.find((c) => c.cmd === "dry_run_apply");
    expect(dryRunCall).toBeDefined();
    expect(dryRunCall?.args).toEqual({
      context: "kind-dev",
      yaml: modifiedYaml,
    });
  });

  it("Test 2: opens DiffModal on successful dry-run showing diff between original and edited YAML", async () => {
    setupMockBackend();
    const user = userEvent.setup();

    render(
      <ResourceDrawer
        row={sampleRow}
        yamlText={originalYaml}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const editor = screen.getByLabelText("Resource YAML");
    fireEvent.change(editor, { target: { value: modifiedYaml } });

    await user.click(screen.getByRole("button", { name: "Review & Apply" }));

    // Wait for DiffModal dialog to open
    const modal = await screen.findByRole("dialog", { name: "Resource comparison diff" });
    expect(modal).toBeInTheDocument();

    // Verify left and right targets
    expect(screen.getByTestId("diff-left-resource")).toHaveTextContent("kind-dev/default/nginx-0");
    expect(screen.getByTestId("diff-right-resource")).toHaveTextContent("kind-dev/default/nginx-0");

    // Verify diff content from compare_resources
    expect(await screen.findByText("image: nginx:1.27")).toBeInTheDocument();
    expect(await screen.findByText("image: nginx:1.28")).toBeInTheDocument();

    // Verify compare_resources IPC call received both manifests
    const compareCall = calls.find((c) => c.cmd === "compare_resources");
    expect(compareCall).toBeDefined();
    expect(compareCall?.args).toEqual({
      leftYaml: originalYaml,
      rightYaml: modifiedYaml,
    });
  });

  it("Test 3: read-only context dry-run error is displayed honestly without opening DiffModal", async () => {
    setupMockBackend({
      dryRunError: {
        kind: "readOnlyContext",
        message: "context 'kind-dev' is read-only",
      },
    });
    const user = userEvent.setup();

    render(
      <ResourceDrawer
        row={sampleRow}
        yamlText={originalYaml}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const editor = screen.getByLabelText("Resource YAML");
    fireEvent.change(editor, { target: { value: modifiedYaml } });

    await user.click(screen.getByRole("button", { name: "Review & Apply" }));

    // DiffModal should NOT open
    expect(screen.queryByRole("dialog", { name: "Resource comparison diff" })).not.toBeInTheDocument();

    // An honest alert should appear in the drawer
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("context 'kind-dev' is read-only");
  });

  it("Test 4: applies YAML on unprotected context and updates drawer YAML and status", async () => {
    setupMockBackend();
    const user = userEvent.setup();

    useResourceDetailStore.setState({
      target: sampleRow,
      detail: {
        uid: "uid-pod-0",
        cluster: "kind-dev",
        kind: "Pod",
        namespace: "default",
        name: "nginx-0",
        yaml: originalYaml,
        related: [],
        events: [],
      },
      loading: false,
      error: null,
    });

    render(
      <ResourceDrawer
        row={sampleRow}
        yamlText={originalYaml}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const editor = screen.getByLabelText("Resource YAML");
    fireEvent.change(editor, { target: { value: modifiedYaml } });

    await user.click(screen.getByRole("button", { name: "Review & Apply" }));

    // Modal opens
    await screen.findByRole("dialog", { name: "Resource comparison diff" });

    // Apply button is present and enabled for unprotected context
    const applyBtn = screen.getByRole("button", { name: "Apply" });
    expect(applyBtn).toBeEnabled();

    // Click Apply
    await user.click(applyBtn);

    // Verify apply_yaml was called with confirmedName = null
    await waitFor(() => {
      const applyCall = calls.find((c) => c.cmd === "apply_yaml");
      expect(applyCall).toBeDefined();
      expect(applyCall?.args).toEqual({
        context: "kind-dev",
        yaml: modifiedYaml,
        confirmedName: null,
      });
    });

    // DiffModal is closed
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Resource comparison diff" })).not.toBeInTheDocument();
    });

    // Success status is displayed
    const successStatus = await screen.findByTestId("apply-success-status");
    expect(successStatus).toHaveTextContent("Applied changes to Pod default/nginx-0 on kind-dev");

    // Editor textarea now reflects the applied YAML
    expect(screen.getByLabelText("Resource YAML")).toHaveValue(appliedYaml);

    // ResourceDetail store was also updated
    expect(useResourceDetailStore.getState().detail?.yaml).toBe(appliedYaml);
  });

  it("Test 5: protected context requires typing resource name before Apply is enabled", async () => {
    useContextsStore.setState({
      ...initialContextsState,
      selectedContext: "kind-dev",
      settingsByContext: {
        "kind-dev": {
          colour: "#e53e3e",
          tags: ["prod"],
          readOnly: false,
          protected: true,
          pinned: false,
        },
      },
    });

    setupMockBackend({
      settings: { readOnly: false, protected: true },
    });
    const user = userEvent.setup();

    render(
      <ResourceDrawer
        row={sampleRow}
        yamlText={originalYaml}
        onClose={() => {}}
      />,
    );

    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const editor = screen.getByLabelText("Resource YAML");
    fireEvent.change(editor, { target: { value: modifiedYaml } });

    await user.click(screen.getByRole("button", { name: "Review & Apply" }));

    // Wait for DiffModal
    await screen.findByRole("dialog", { name: "Resource comparison diff" });

    // Guard prompt is shown
    const guard = screen.getByTestId("diff-protected-guard");
    expect(guard).toBeInTheDocument();
    expect(within(guard).getByText("nginx-0")).toBeInTheDocument();

    const applyBtn = screen.getByRole("button", { name: "Apply" });
    const nameInput = screen.getByLabelText("Confirm resource name");

    // Apply is initially disabled
    expect(applyBtn).toBeDisabled();

    // Type incorrect name
    await user.type(nameInput, "wrong-pod");
    expect(applyBtn).toBeDisabled();

    // Clear and type exact resource name
    await user.clear(nameInput);
    await user.type(nameInput, "nginx-0");

    // Apply is now enabled
    expect(applyBtn).toBeEnabled();

    // Click Apply
    await user.click(applyBtn);

    // Verify apply_yaml was called with confirmedName = "nginx-0"
    await waitFor(() => {
      const applyCall = calls.find((c) => c.cmd === "apply_yaml");
      expect(applyCall).toBeDefined();
      expect(applyCall?.args).toEqual({
        context: "kind-dev",
        yaml: modifiedYaml,
        confirmedName: "nginx-0",
      });
    });

    // DiffModal is closed and success status is displayed
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Resource comparison diff" })).not.toBeInTheDocument();
    });
    expect(await screen.findByTestId("apply-success-status")).toHaveTextContent(
      "Applied changes to Pod default/nginx-0 on kind-dev",
    );
  });

  it("Test 6: DiffModal in applyMode disables Apply on read-only context and explains why", async () => {
    setupMockBackend({
      settings: { readOnly: true, protected: false },
    });

    render(
      <DiffModal
        open={true}
        applyMode={true}
        context="kind-dev"
        resourceName="nginx-0"
        readOnly={true}
        left={{ name: "nginx-0", manifest: originalYaml }}
        right={{ name: "nginx-0", manifest: modifiedYaml }}
        compare={() => Promise.resolve({ identical: false, unifiedDiff: "+line" })}
        onClose={() => {}}
      />,
    );

    // Warning alert explains read-only
    const alert = await screen.findByTestId("diff-readonly-alert");
    expect(alert).toHaveTextContent("Blocked: context ‘kind-dev’ is read-only. Changes cannot be applied.");

    // Apply button is disabled
    const applyBtn = screen.getByRole("button", { name: "Apply" });
    expect(applyBtn).toBeDisabled();
  });

  it("Test 7: DiffModal displays backend apply error honestly", async () => {
    setupMockBackend({
      applyError: {
        kind: "kube",
        message: "admission webhook denied: immutable field spec.clusterIP",
      },
    });
    const user = userEvent.setup();

    render(
      <DiffModal
        open={true}
        applyMode={true}
        context="kind-dev"
        resourceName="nginx-0"
        left={{ name: "nginx-0", manifest: originalYaml }}
        right={{ name: "nginx-0", manifest: modifiedYaml }}
        compare={() => Promise.resolve({ identical: false, unifiedDiff: "+line" })}
        onClose={() => {}}
      />,
    );

    const applyBtn = screen.getByRole("button", { name: "Apply" });
    await user.click(applyBtn);

    // An honest error is shown in the modal, and the modal remains open
    const errorAlert = await screen.findByTestId("diff-apply-error");
    expect(errorAlert).toHaveTextContent("admission webhook denied: immutable field spec.clusterIP");
    expect(screen.getByRole("dialog", { name: "Resource comparison diff" })).toBeInTheDocument();
  });
});
