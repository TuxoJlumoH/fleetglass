import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  HelmReleasePage,
  HelmReleaseSummary,
  HelmRevisionDiff,
  PluginManifest,
} from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import type { Row } from "../stores/pods";
import { CommandPalette } from "./CommandPalette";
import { HelmView } from "./HelmView";
import { PluginsView } from "./PluginsView";
import { ResourceDrawer } from "./ResourceDrawer";
import { TabsArea } from "./TabsArea";

describe("Task R-13: Mount HelmView, SecretEditor, PluginSandboxView", () => {
  beforeEach(() => {
    useLayoutStore.setState({
      ...initialLayoutState,
      tabs: [{ id: "tab-1", title: "Welcome", view: "resources", kind: "Pod" }],
      activeTabId: "tab-1",
    });
    useContextsStore.setState({
      contexts: [
        {
          name: "kind-dev",
          cluster: "kind-dev",
          user: "admin",
          namespace: "default",
          server: "https://127.0.0.1:6443",
          auth: { type: "clientCert" },
          isCurrent: true,
        },
      ],
      selectedContext: "kind-dev",
      activeClusterSelection: ["kind-dev"],
      settingsByContext: {
        "kind-dev": {
          colour: null,
          tags: [],
          readOnly: false,
          protected: false,
          pinned: false,
        },
      },
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe("1. HelmView in TabsArea", () => {
    const sampleReleases: HelmReleaseSummary[] = [
      {
        name: "cert-manager",
        namespace: "cert-manager",
        revision: 3,
        updated: "2026-10-31 10:00:00",
        status: "deployed",
        chart: "cert-manager-v1.16.0",
        appVersion: "v1.16.0",
      },
      {
        name: "prometheus",
        namespace: "monitoring",
        revision: 1,
        updated: "2026-10-30 08:30:00",
        status: "deployed",
        chart: "kube-prometheus-stack-65.0.0",
        appVersion: "v0.77.0",
      },
    ];

    it("renders honest empty state when no Helm releases are found", async () => {
      const fetchHelmReleases = vi.fn().mockResolvedValue({
        total: 0,
        offset: 0,
        releases: [],
      } satisfies HelmReleasePage);

      useLayoutStore.setState({
        tabs: [{ id: "helm-tab", title: "Helm", view: "helm" }],
        activeTabId: "helm-tab",
      });

      render(
        <TabsArea
          fetchHelmReleases={fetchHelmReleases}
        />,
      );

      expect(await screen.findByTestId("helm-empty-state")).toHaveTextContent(
        "No Helm releases found in kind-dev/all",
      );
      expect(fetchHelmReleases).toHaveBeenCalledWith("kind-dev", null, 0, 100);
    });

    it("renders honest loading and error states for HelmView", async () => {
      let rejectPromise!: (err: Error) => void;
      const fetchHelmReleases = vi.fn(
        () =>
          new Promise<HelmReleasePage>((_, reject) => {
            rejectPromise = reject;
          }),
      );

      render(
        <HelmView
          cluster="test-cluster"
          namespace="kube-system"
          fetchReleases={fetchHelmReleases}
        />,
      );

      expect(screen.getByTestId("helm-loading")).toHaveTextContent("Loading Helm releases…");

      act(() => {
        rejectPromise(new Error("RBAC: forbidden to list secrets"));
      });

      expect(await screen.findByTestId("helm-error")).toHaveTextContent(
        "Error loading Helm releases: RBAC: forbidden to list secrets",
      );
    });

    it("renders releases table, opens detail with history, diff, and rollback preview", async () => {
      const user = userEvent.setup();

      const fetchHelmReleases = vi.fn().mockResolvedValue({
        total: 2,
        offset: 0,
        releases: sampleReleases,
      });

      const firstRelease = sampleReleases[0] ?? {
        name: "cert-manager",
        namespace: "cert-manager",
        revision: 3,
        updated: "2026-10-31 10:00:00",
        status: "deployed",
        chart: "cert-manager-v1.16.0",
        appVersion: "v1.16.0",
      };

      const historyData: HelmReleaseSummary[] = [
        firstRelease,
        {
          name: "cert-manager",
          namespace: "cert-manager",
          revision: 2,
          updated: "2026-10-25 10:00:00",
          status: "superseded",
          chart: "cert-manager-v1.15.0",
          appVersion: "v1.15.0",
        },
        {
          name: "cert-manager",
          namespace: "cert-manager",
          revision: 1,
          updated: "2026-10-20 10:00:00",
          status: "superseded",
          chart: "cert-manager-v1.14.0",
          appVersion: "v1.14.0",
        },
      ];

      const fetchHelmHistory = vi.fn().mockResolvedValue(historyData);
      const fetchHelmDiff = vi.fn().mockResolvedValue({
        valuesDiff: "--- rev 2\n+++ rev 3\n- replicas: 1\n+ replicas: 2",
        manifestDiff: "--- rev 2\n+++ rev 3\n- imageTag: v1.15.0\n+ imageTag: v1.16.0",
      } satisfies HelmRevisionDiff);
      const previewHelmRollback = vi.fn().mockResolvedValue("helm rollback cert-manager 2 -n cert-manager");

      useLayoutStore.setState({
        tabs: [{ id: "helm-tab", title: "Helm", view: "helm" }],
        activeTabId: "helm-tab",
      });

      render(
        <TabsArea
          fetchHelmReleases={fetchHelmReleases}
          fetchHelmHistory={fetchHelmHistory}
          fetchHelmDiff={fetchHelmDiff}
          previewHelmRollback={previewHelmRollback}
        />,
      );

      // Verify release rows rendered
      const certManagerCells = await screen.findAllByText("cert-manager");
      expect(certManagerCells.length).toBeGreaterThan(0);
      expect(screen.getByText("prometheus")).toBeInTheDocument();

      // Click on cert-manager row
      const targetCell = certManagerCells[0];
      if (targetCell) {
        await user.click(targetCell);
      }
      expect(screen.getByTestId("helm-detail-panel")).toBeInTheDocument();

      // Switch to History tab
      const historyTab = screen.getByRole("tab", { name: "History" });
      await user.click(historyTab);

      expect(fetchHelmHistory).toHaveBeenCalledWith("kind-dev", "cert-manager", "cert-manager");
      expect(await screen.findByText("rev 2")).toBeInTheDocument();

      // Click Preview Rollback on rev 2
      const rollbackBtn = screen.getByRole("button", { name: "Preview rollback to rev 2" });
      await user.click(rollbackBtn);

      expect(previewHelmRollback).toHaveBeenCalledWith("cert-manager", "cert-manager", 2);
      expect(await screen.findByTestId("helm-rollback-preview")).toHaveTextContent(
        "helm rollback cert-manager 2 -n cert-manager",
      );

      // Switch to Diff tab
      const diffTab = screen.getByRole("tab", { name: "Diff" });
      await user.click(diffTab);

      const diffBtn = screen.getByRole("button", { name: "Diff Revisions" });
      await user.click(diffBtn);

      expect(fetchHelmDiff).toHaveBeenCalledWith("kind-dev", "cert-manager", "cert-manager", 2, 3);
      expect(await screen.findByTestId("helm-diff-values")).toHaveTextContent("+ replicas: 2");
      expect(screen.getByTestId("helm-diff-manifest")).toHaveTextContent("+ imageTag: v1.16.0");
    });
  });

  describe("2. SecretEditor in ResourceDrawer", () => {
    const podRow: Row = {
      uid: "pod-1",
      name: "frontend-pod",
      kind: "Pod",
      namespace: "default",
      cluster: "kind-dev",
      status: "healthy",
      statusReason: "Running",
      labels: [["app", "frontend"]],
      cells: [],
      custom: [],
    };

    const secretRow: Row = {
      uid: "secret-1",
      name: "app-credentials",
      kind: "Secret",
      namespace: "production",
      cluster: "kind-dev",
      status: "healthy",
      statusReason: "Active",
      labels: [["app", "web"]],
      cells: [],
      custom: [],
    };

    it("does not show Secret Data tab for Pods", () => {
      render(<ResourceDrawer row={podRow} onClose={vi.fn()} />);
      expect(screen.queryByRole("tab", { name: /secret/i })).toBeNull();
      expect(screen.queryByRole("tab", { name: /data/i })).toBeNull();
      expect(screen.queryByTestId("open-secret-editor-btn")).toBeNull();
    });

    it("shows Secret Data tab and launches SecretEditor for Secrets", async () => {
      const user = userEvent.setup();
      const decodeSecret = vi.fn().mockResolvedValue({
        DATABASE_USER: "postgres",
        DATABASE_PASS: "super-secret-123",
      });
      const encodeSecret = vi.fn().mockResolvedValue({
        DATABASE_USER: "cG9zdGdyZXM=",
        DATABASE_PASS: "bmV3LXBhc3N3b3Jk",
      });

      const secretYaml = `apiVersion: v1
kind: Secret
metadata:
  name: app-credentials
  namespace: production
data:
  DATABASE_USER: cG9zdGdyZXM=
  DATABASE_PASS: c3VwZXItc2VjcmV0LTEyMw==
`;

      render(
        <ResourceDrawer
          row={secretRow}
          yamlText={secretYaml}
          decodeSecret={decodeSecret}
          encodeSecret={encodeSecret}
          onClose={vi.fn()}
        />,
      );

      // Secret Data tab is present
      const secretTab = screen.getByRole("tab", { name: /secret data/i });
      expect(secretTab).toBeInTheDocument();

      // Summary tab also has the open secret button
      const openBtn = screen.getByTestId("open-secret-editor-btn");
      expect(openBtn).toBeInTheDocument();

      // Open SecretEditor
      await user.click(secretTab);

      expect(screen.getByTestId("drawer-secret-editor")).toBeInTheDocument();
      expect(decodeSecret).toHaveBeenCalledWith({
        DATABASE_USER: "cG9zdGdyZXM=",
        DATABASE_PASS: "c3VwZXItc2VjcmV0LTEyMw==",
      });

      // Values masked initially
      expect(await screen.findByText("Reveal / Decode")).toBeInTheDocument();
      expect(screen.queryByText("super-secret-123")).toBeNull();

      // Click Reveal
      await user.click(screen.getByText("Reveal / Decode"));
      expect(screen.getByDisplayValue("super-secret-123")).toBeInTheDocument();

      // Edit value and save
      const passInput = screen.getByDisplayValue("super-secret-123");
      await user.clear(passInput);
      await user.type(passInput, "new-password");

      const saveBtn = screen.getByRole("button", { name: /save/i });
      await user.click(saveBtn);

      expect(encodeSecret).toHaveBeenCalled();
    });

    it("respects read-only contexts by disabling editing and hiding Save button", async () => {
      const user = userEvent.setup();
      const decodeSecret = vi.fn().mockResolvedValue({
        API_KEY: "secret-token",
      });

      // Set context to read-only in store
      useContextsStore.setState({
        settingsByContext: {
          "kind-dev": {
            colour: null,
            tags: [],
            readOnly: true,
            protected: false,
            pinned: false,
          },
        },
      });

      render(
        <ResourceDrawer
          row={secretRow}
          yamlText="apiVersion: v1\nkind: Secret\ndata:\n  API_KEY: c2VjcmV0\n"
          decodeSecret={decodeSecret}
          onClose={vi.fn()}
        />,
      );

      const secretTab = screen.getByRole("tab", { name: /secret data/i });
      await user.click(secretTab);

      // Wait for decode
      expect(await screen.findByText("Reveal / Decode")).toBeInTheDocument();

      // Save button must not be present in read-only mode
      expect(screen.queryByRole("button", { name: /save/i })).toBeNull();
      // Readonly notice is displayed
      expect(screen.getByTestId("secret-readonly-notice")).toHaveTextContent("Secret is read-only");
    });
  });

  describe("3. PluginSandboxView in UI", () => {
    const samplePlugins: PluginManifest[] = [
      {
        id: "cost-analyzer",
        name: "cost-analyzer",
        version: "0.2.0",
        description: "Cloud and node cost estimation for workloads",
        entrypoint: "plugins/cost-analyzer/index.html",
        permissions: ["read:resources", "read:contexts"],
      },
      {
        id: "net-top",
        name: "net-top",
        version: "1.0.1",
        description: "Network topology graph viewer",
        entrypoint: "plugins/net-top/index.html",
        permissions: ["read:resources"],
      },
    ];

    it("opens Plugins view via CommandPalette", async () => {
      const user = userEvent.setup();

      render(
        <>
          <CommandPalette />
          <TabsArea />
        </>,
      );

      // Open palette via Ctrl+K
      await user.keyboard("{Control>}k{/Control}");
      expect(screen.getByRole("dialog", { name: "Command palette" })).toBeInTheDocument();

      // Click "Open Plugins view"
      const pluginOption = screen.getByRole("option", { name: "Open Plugins view" });
      await user.click(pluginOption);

      expect(useLayoutStore.getState().activeTabId).toBe("view-plugins");
      expect(screen.getByRole("tab", { name: "Plugins" })).toHaveAttribute("aria-selected", "true");
    });

    it("renders honest loading, error, and empty states in PluginsView", async () => {
      const fetchEmpty = vi.fn().mockResolvedValue([]);
      const { rerender } = render(<PluginsView fetchPlugins={fetchEmpty} />);

      expect(await screen.findByTestId("plugins-empty")).toHaveTextContent("No plugins installed");

      const fetchError = vi.fn().mockRejectedValue(new Error("Failed to scan plugin directory"));
      rerender(<PluginsView fetchPlugins={fetchError} />);

      expect(await screen.findByTestId("plugins-error")).toHaveTextContent(
        "Failed to load plugins: Failed to scan plugin directory",
      );
    });

    it("renders installed plugins list and mounts PluginSandboxView on Open", async () => {
      const user = userEvent.setup();
      const fetchPlugins = vi.fn().mockResolvedValue(samplePlugins);

      render(<PluginsView fetchPlugins={fetchPlugins} htmlContent="<div>Cost Content</div>" />);

      // Lists installed plugins
      expect(await screen.findByText("cost-analyzer")).toBeInTheDocument();
      expect(screen.getByText("v0.2.0")).toBeInTheDocument();
      expect(screen.getByText("Cloud and node cost estimation for workloads")).toBeInTheDocument();
      expect(screen.getByText("net-top")).toBeInTheDocument();

      // Click Open Plugin on cost-analyzer
      const openBtn = screen.getByRole("button", { name: "Open cost-analyzer" });
      await user.click(openBtn);

      // PluginSandboxView mounted
      expect(screen.getByTestId("plugin-sandbox-view")).toBeInTheDocument();
      expect(screen.getByTestId("plugin-iframe")).toHaveAttribute("sandbox", "allow-scripts");
      expect(screen.getByText("read:resources")).toBeInTheDocument();
      expect(screen.getByText("read:contexts")).toBeInTheDocument();

      // Close button unmounts sandbox and returns to plugin list
      const closeBtn = screen.getByTestId("plugin-close-btn");
      await user.click(closeBtn);

      expect(screen.queryByTestId("plugin-sandbox-view")).toBeNull();
      expect(screen.getByText("cost-analyzer")).toBeInTheDocument();
    });
  });
});
