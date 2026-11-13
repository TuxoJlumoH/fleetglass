import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AppView, LOADING_POLL_MS } from "./AppView";
import type { AppPage, AppQuerySpec_Deserialize, AppSummary, IpcError, StoreAccess } from "../generated/bindings";
import { DiffModal, type CompareResourcesFn } from "./DiffModal";
import { SplitPaneContainer } from "./SplitPaneContainer";
import { initialLayoutState, useLayoutStore } from "../stores/layout";

describe("Phase 2 Wave 2 UI Components", () => {
  beforeEach(() => {
    useLayoutStore.setState(initialLayoutState);
  });

  // Test 1: <DiffModal /> renders the backend-computed diff (compare_resources IPC).
  describe("<DiffModal />", () => {
    const leftManifest = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: order-service
  namespace: staging
spec:
  replicas: 2`;

    const rightManifest = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: order-service
  namespace: production
spec:
  replicas: 5`;

    // Shape of the Rust `compute_unified_diff` output, plus headers to check their styling.
    const backendDiff = [
      "--- staging-us/staging/order-service",
      "+++ prod-us/production/order-service",
      "@@ -1,7 +1,7 @@",
      " apiVersion: apps/v1",
      " kind: Deployment",
      " metadata:",
      "   name: order-service",
      "-  namespace: staging",
      "+  namespace: production",
      " spec:",
      "-  replicas: 2",
      "+  replicas: 5",
    ].join("\n");

    function renderModal(compare: CompareResourcesFn, onClose = vi.fn()) {
      render(
        <DiffModal
          open={true}
          left={{
            context: "staging-us",
            namespace: "staging",
            name: "order-service",
            manifest: leftManifest,
          }}
          right={{
            context: "prod-us",
            namespace: "production",
            name: "order-service",
            manifest: rightManifest,
          }}
          compare={compare}
          onClose={onClose}
        />,
      );
      return onClose;
    }

    it("shows loading, then renders the backend diff side-by-side and unified", async () => {
      const user = userEvent.setup();
      let resolve!: (r: { unifiedDiff: string; identical: boolean }) => void;
      const compare = vi.fn<CompareResourcesFn>(
        () =>
          new Promise((r) => {
            resolve = r;
          }),
      );
      const onClose = renderModal(compare);

      // Raw manifests are sent to Rust; the UI computes nothing itself.
      expect(compare).toHaveBeenCalledWith(leftManifest, rightManifest);

      const dialog = screen.getByRole("dialog", { name: "Resource comparison diff" });
      expect(dialog).toHaveAttribute("aria-modal", "true");
      expect(screen.getByTestId("diff-left-resource")).toHaveTextContent(
        "staging-us/staging/order-service",
      );
      expect(screen.getByTestId("diff-right-resource")).toHaveTextContent(
        "prod-us/production/order-service",
      );

      // Loading state while the IPC call is pending, never an empty view.
      expect(screen.getByTestId("diff-loading")).toHaveTextContent(/comparing resources/i);
      expect(screen.queryByTestId("diff-side-by-side")).not.toBeInTheDocument();

      await act(async () => {
        resolve({ unifiedDiff: backendDiff, identical: false });
        await Promise.resolve();
      });

      expect(screen.queryByTestId("diff-loading")).not.toBeInTheDocument();

      // Side-by-side (default): `-` lines left, `+` lines right, context on both.
      const leftPane = screen.getByTestId("diff-left-pane");
      const rightPane = screen.getByTestId("diff-right-pane");
      expect(within(leftPane).getByText("replicas: 2").parentElement).toHaveClass(
        "diff-line--deleted",
      );
      expect(within(leftPane).queryByText("replicas: 5")).not.toBeInTheDocument();
      expect(within(rightPane).getByText("replicas: 5").parentElement).toHaveClass(
        "diff-line--added",
      );
      expect(within(rightPane).queryByText("replicas: 2")).not.toBeInTheDocument();
      expect(within(leftPane).getByText("kind: Deployment")).toBeInTheDocument();
      expect(within(rightPane).getByText("kind: Deployment")).toBeInTheDocument();

      // Unified view renders the backend lines with highlighted changes and styled headers.
      await user.click(screen.getByRole("tab", { name: /unified diff/i }));
      expect(screen.getByTestId("diff-unified-viewer")).toBeInTheDocument();

      const added = screen.getAllByTestId("diff-line-added");
      expect(added.map((l) => l.textContent)).toEqual(["+  namespace: production", "+  replicas: 5"]);
      added.forEach((l) => {
        expect(l).toHaveClass("diff-line--added");
      });

      const deleted = screen.getAllByTestId("diff-line-deleted");
      expect(deleted.map((l) => l.textContent)).toEqual(["-  namespace: staging", "-  replicas: 2"]);
      deleted.forEach((l) => {
        expect(l).toHaveClass("diff-line--deleted");
      });

      expect(screen.getByTestId("diff-line-hunk")).toHaveTextContent("@@ -1,7 +1,7 @@");
      const fileHeaders = screen.getAllByTestId("diff-line-file-header");
      expect(fileHeaders).toHaveLength(2);
      expect(fileHeaders[0]).toHaveClass("diff-unified__header-left");
      expect(fileHeaders[1]).toHaveClass("diff-unified__header-right");

      // Copy patch copies the backend unified diff verbatim.
      const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
      await user.click(screen.getByRole("button", { name: /copy diff/i }));
      expect(writeText).toHaveBeenCalledWith(backendDiff);
      expect(screen.getByRole("button", { name: /copied/i })).toBeInTheDocument();

      // Escape and the close button both close the modal.
      fireEvent.keyDown(window, { key: "Escape" });
      expect(onClose).toHaveBeenCalledTimes(1);
      await user.click(screen.getByRole("button", { name: /close diff modal/i }));
      expect(onClose).toHaveBeenCalledTimes(2);
    });

    it("shows the identical message when the backend reports no differences", async () => {
      const compare = vi.fn<CompareResourcesFn>(() =>
        Promise.resolve({ unifiedDiff: " apiVersion: apps/v1\n kind: Deployment", identical: true }),
      );
      renderModal(compare);

      expect(await screen.findByTestId("diff-identical")).toHaveTextContent(
        "Resources are identical (ignoring runtime metadata)",
      );
      expect(screen.queryByTestId("diff-side-by-side")).not.toBeInTheDocument();
      expect(screen.queryByTestId("diff-loading")).not.toBeInTheDocument();
    });

    it("shows a typed error message when the comparison fails", async () => {
      const ipcError = { kind: "invalidInput", message: "left resource is not valid YAML" };
      // Tauri rejects with the serialized `IpcError` object, not an `Error`.
      // eslint-disable-next-line @typescript-eslint/prefer-promise-reject-errors
      const compare = vi.fn<CompareResourcesFn>(() => Promise.reject(ipcError));
      renderModal(compare);

      const alert = await screen.findByRole("alert");
      expect(alert).toHaveAttribute("data-testid", "diff-error");
      expect(alert).toHaveTextContent("invalidInput");
      expect(alert).toHaveTextContent("left resource is not valid YAML");
      expect(screen.queryByTestId("diff-loading")).not.toBeInTheDocument();
      expect(screen.queryByTestId("diff-side-by-side")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: /copy diff/i })).toBeDisabled();
    });

    it("04-04: toggles metadata and only differences options and re-runs compare", async () => {
      const user = userEvent.setup();
      const compare = vi.fn<CompareResourcesFn>(() =>
        Promise.resolve({ unifiedDiff: backendDiff, identical: false }),
      );
      renderModal(compare);

      await screen.findByRole("dialog");
      expect(compare).toHaveBeenCalledWith(leftManifest, rightManifest);

      const metaToggle = screen.getByTestId("toggle-metadata-status");
      await user.click(metaToggle);
      expect(compare).toHaveBeenLastCalledWith(leftManifest, rightManifest, {
        includeMetadataAndStatus: true,
        onlyDifferences: false,
        revealSecrets: false,
      });

      const diffToggle = screen.getByTestId("toggle-only-differences");
      await user.click(diffToggle);
      expect(compare).toHaveBeenLastCalledWith(leftManifest, rightManifest, {
        includeMetadataAndStatus: true,
        onlyDifferences: true,
        revealSecrets: false,
      });
    });

    it("04-04: Secret resources show hold-to-reveal button that decodes and diffs secret values", () => {
      vi.useFakeTimers();
      try {
        const compare = vi.fn<CompareResourcesFn>(() =>
          Promise.resolve({ unifiedDiff: " password: <redacted>", identical: true }),
        );

        render(
          <DiffModal
            open={true}
            resourceKind="Secret"
            leftManifest={"kind: Secret\ndata:\n  k: v1"}
            rightManifest={"kind: Secret\ndata:\n  k: v2"}
            compare={compare}
          />,
        );

        const revealBtn = screen.getByTestId("reveal-secrets-btn");
        expect(revealBtn).toHaveTextContent("Hold 1s to decode & diff secret values");

        // Press and hold
        fireEvent.mouseDown(revealBtn);
        act(() => {
          vi.advanceTimersByTime(1000);
        });
        fireEvent.mouseUp(revealBtn);

        expect(revealBtn).toHaveTextContent("Secret values revealed");
        expect(compare).toHaveBeenLastCalledWith(
          "kind: Secret\ndata:\n  k: v1",
          "kind: Secret\ndata:\n  k: v2",
          {
            includeMetadataAndStatus: false,
            onlyDifferences: false,
            revealSecrets: true,
          },
        );
      } finally {
        vi.useRealTimers();
      }
    });
  });

  // Test 2: <SplitPaneContainer /> renders two independent panes when splitMode is active and toggles via Ctrl+\.
  it("<SplitPaneContainer /> renders two independent panes when splitMode is active and toggles via Ctrl+\\", () => {
    render(
      <SplitPaneContainer
        primary={<div data-testid="primary-pane-view">Primary Workloads Table</div>}
        secondary={<div data-testid="secondary-pane-view">Secondary Pod Logs</div>}
      />,
    );

    // Initially splitMode is "none": only primary pane is visible
    expect(screen.getByTestId("split-pane-primary")).toBeInTheDocument();
    expect(screen.getByTestId("primary-pane-view")).toBeInTheDocument();
    expect(screen.queryByTestId("split-pane-secondary")).not.toBeInTheDocument();
    expect(screen.queryByTestId("secondary-pane-view")).not.toBeInTheDocument();
    expect(useLayoutStore.getState().splitMode).toBe("none");

    // Toggle split via Ctrl+\ shortcut
    fireEvent.keyDown(window, { key: "\\", ctrlKey: true });

    // Store splitMode updated to "horizontal"
    expect(useLayoutStore.getState().splitMode).toBe("horizontal");

    // Now both panes are rendered independently
    expect(screen.getByTestId("split-pane-primary")).toBeInTheDocument();
    expect(screen.getByTestId("primary-pane-view")).toBeInTheDocument();
    expect(screen.getByTestId("split-pane-secondary")).toBeInTheDocument();
    expect(screen.getByTestId("secondary-pane-view")).toBeInTheDocument();

    // Divider handle is rendered with 50/50 default ratio
    const divider = screen.getByRole("separator", { name: /split pane resize handle/i });
    expect(divider).toBeInTheDocument();
    expect(divider).toHaveAttribute("aria-valuenow", "50");

    // Press Ctrl+\ again to toggle split off
    fireEvent.keyDown(window, { key: "\\", ctrlKey: true });

    expect(useLayoutStore.getState().splitMode).toBe("none");
    expect(screen.getByTestId("split-pane-primary")).toBeInTheDocument();
    expect(screen.queryByTestId("split-pane-secondary")).not.toBeInTheDocument();

    // Setting splitMode to "vertical" via setSplitMode renders stacked panels
    act(() => {
      useLayoutStore.getState().setSplitMode("vertical");
    });

    expect(screen.getByTestId("split-pane-primary")).toBeInTheDocument();
    expect(screen.getByTestId("split-pane-secondary")).toBeInTheDocument();
    const container = screen.getByTestId("split-pane-container");
    expect(container).toHaveAttribute("data-split-mode", "vertical");
  });

  // Test 3: <AppView /> renders backend-grouped app pages and forwards the text filter to Rust.
  it("<AppView /> renders one card per AppSummary from a windowed query_apps page", async () => {
    const user = userEvent.setup();

    const page: AppPage = {
      access: [],
      total: 2,
      offset: 0,
      apps: [
        {
          name: "checkout-app",
          namespace: "shop",
          cluster: "kind-a",
          components: [
            { cluster: "kind-a", kind: "Deployment", namespace: "shop", name: "checkout-api", status: "healthy", statusReason: "3/3" },
            { cluster: "kind-a", kind: "Pod", namespace: "shop", name: "checkout-api-7b8c9-1", status: "healthy", statusReason: "Running" },
            { cluster: "kind-a", kind: "Service", namespace: "shop", name: "checkout-service", status: "healthy", statusReason: "" },
          ],
        },
        {
          name: "inventory-app",
          namespace: "warehouse",
          cluster: "kind-a",
          components: [
            { cluster: "kind-a", kind: "Deployment", namespace: "warehouse", name: "inventory-api", status: "degraded", statusReason: "1/3" },
            { cluster: "kind-a", kind: "Pod", namespace: "warehouse", name: "inventory-api-x9z2-1", status: "failed", statusReason: "CrashLoopBackOff" },
            { cluster: "kind-a", kind: "Pod", namespace: "warehouse", name: "inventory-api-x9z2-2", status: "pending", statusReason: "Pending" },
          ],
        },
      ],
    };
    const fetchApps = vi.fn((spec: AppQuerySpec_Deserialize) =>
      Promise.resolve<AppPage>(spec.textFilter ? { access: [], total: 1, offset: 0, apps: page.apps.slice(0, 1) } : page),
    );

    render(<AppView clusters={["kind-a"]} namespaces={["shop", "warehouse"]} fetchApps={fetchApps} />);

    expect(screen.getByTestId("app-view-loading")).toBeInTheDocument();
    expect(await screen.findByTestId("app-card-checkout-app")).toBeInTheDocument();
    expect(fetchApps).toHaveBeenCalledTimes(1);
    expect(fetchApps).toHaveBeenLastCalledWith({
      clusters: ["kind-a"],
      namespaces: ["shop", "warehouse"],
      textFilter: null,
      offset: 0,
      limit: 100,
    });

    // Badges: worst component status wins (failed > degraded > pending > healthy).
    expect(screen.getByTestId("app-status-checkout-app")).toHaveTextContent(/healthy/i);
    expect(screen.getByTestId("app-status-inventory-app")).toHaveTextContent(/failed/i);

    // Components are grouped by kind for display.
    const checkoutDeployments = screen.getByTestId("app-kind-checkout-app-Deployment");
    expect(within(checkoutDeployments).getByText("checkout-api")).toBeInTheDocument();
    const checkoutServices = screen.getByTestId("app-kind-checkout-app-Service");
    expect(within(checkoutServices).getByText("checkout-service")).toBeInTheDocument();
    const inventoryPods = screen.getByTestId("app-kind-inventory-app-Pod");
    expect(within(inventoryPods).getAllByRole("listitem")).toHaveLength(2);
    expect(within(inventoryPods).getByText("CrashLoopBackOff")).toBeInTheDocument();

    // Everything is loaded: no "Load more".
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();

    // The search box sends textFilter to Rust (debounced) instead of filtering locally.
    await user.type(screen.getByRole("searchbox", { name: "Filter applications" }), "checkout");
    // Wait for the filtered page itself, not just the intermediate loading state
    // (cards also disappear while the new query is in flight).
    await waitFor(() => {
      expect(fetchApps).toHaveBeenLastCalledWith({
        clusters: ["kind-a"],
        namespaces: ["shop", "warehouse"],
        textFilter: "checkout",
        offset: 0,
        limit: 100,
      });
    });
    expect(await screen.findByTestId("app-card-checkout-app")).toBeInTheDocument();
    expect(screen.queryByTestId("app-card-inventory-app")).not.toBeInTheDocument();
    // Debounced: typing "checkout" must not issue one request per keystroke.
    expect(fetchApps.mock.calls.length).toBeLessThan(1 + "checkout".length);
  });

  it("<AppView /> requests the next window with Load more when total exceeds loaded apps", async () => {
    const user = userEvent.setup();
    const app = (name: string): AppSummary => ({ name, namespace: "ns", cluster: "c", components: [] });
    const fetchApps = vi.fn((spec: AppQuerySpec_Deserialize) =>
      Promise.resolve<AppPage>({
        access: [],
        total: 101,
        offset: spec.offset,
        apps: [app(spec.offset === 0 ? "first" : "second")],
      }),
    );

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(await screen.findByTestId("app-card-first")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(await screen.findByTestId("app-card-second")).toBeInTheDocument();
    expect(screen.getByTestId("app-card-first")).toBeInTheDocument();
    expect(fetchApps).toHaveBeenLastCalledWith({
      clusters: ["kind-a"],
      namespaces: [],
      textFilter: null,
      offset: 1,
      limit: 100,
    });
  });

  it("<AppView /> shows an honest empty state when no applications match", async () => {
    const fetchApps = vi.fn(() => Promise.resolve<AppPage>({ access: [], total: 0, offset: 0, apps: [] }));

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(await screen.findByTestId("app-view-empty")).toHaveTextContent(
      "No applications found in the selected clusters/namespaces",
    );
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("<AppView /> renders 'Select a context to view applications.' when clusters is empty", () => {
    const fetchApps = vi.fn(() => Promise.resolve<AppPage>({ access: [], total: 0, offset: 0, apps: [] }));

    render(<AppView clusters={[]} namespaces={[]} fetchApps={fetchApps} />);

    expect(screen.getByText("Select a context to view applications.")).toBeInTheDocument();
    expect(fetchApps).not.toHaveBeenCalled();
  });

  it("<AppView /> renders honest banner for Forbidden access state instead of an empty list", async () => {
    const access: StoreAccess[] = [
      {
        cluster: "kind-a",
        kind: "Deployment",
        access: {
          state: "forbidden",
          resource: "deployments",
          namespace: null,
          message: "User alice cannot list resource deployments",
        },
      },
    ];
    const fetchApps = vi.fn(() => Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }));

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveClass("app-view__banner", "app-view__banner--forbidden");
    expect(alert).toHaveTextContent(
      "No permission to list deployments in kind-a: User alice cannot list resource deployments",
    );
    expect(screen.queryByTestId("app-view-empty")).not.toBeInTheDocument();
  });

  it("<AppView /> renders honest banner for Error access state instead of an empty list", async () => {
    const access: StoreAccess[] = [
      {
        cluster: "cluster-b",
        kind: "Pod",
        access: {
          state: "error",
          message: "dial tcp 10.0.0.1:6443: connect: connection refused",
        },
      },
    ];
    const fetchApps = vi.fn(() => Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }));

    render(<AppView clusters={["cluster-b"]} namespaces={[]} fetchApps={fetchApps} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveClass("app-view__banner", "app-view__banner--error");
    expect(alert).toHaveTextContent(
      "Connection error for cluster-b: dial tcp 10.0.0.1:6443: connect: connection refused",
    );
    expect(screen.queryByTestId("app-view-empty")).not.toBeInTheDocument();
  });

  it("<AppView /> renders 'Loading applications…' when apps.length === 0 and access has loading state", async () => {
    const access: StoreAccess[] = [
      {
        cluster: "kind-a",
        kind: "Pod",
        access: {
          state: "loading",
          rowsSoFar: 0,
        },
      },
    ];
    const fetchApps = vi.fn(() => Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }));

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(await screen.findByTestId("app-view-loading")).toHaveTextContent("Loading applications…");
    expect(screen.queryByTestId("app-view-empty")).not.toBeInTheDocument();
  });

  it("<AppView /> labels applications grouped from a partial initial list", async () => {
    const access: StoreAccess[] = [
      { cluster: "kind-a", kind: "Pod", access: { state: "loading", rowsSoFar: 2500 } },
      { cluster: "kind-a", kind: "Deployment", access: { state: "ready" } },
    ];
    const fetchApps = vi.fn(() =>
      Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }),
    );

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(await screen.findByTestId("app-view-loading-progress")).toHaveTextContent(
      "Loading… 2,500 rows so far (kind-a)",
    );
  });

  it("<AppView /> counts 'objects' when the loading stores span several kinds", async () => {
    const access: StoreAccess[] = [
      { cluster: "kind-a", kind: "Pod", access: { state: "loading", rowsSoFar: 2000 } },
      { cluster: "kind-a", kind: "Deployment", access: { state: "loading", rowsSoFar: 500 } },
    ];
    const fetchApps = vi.fn(() =>
      Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }),
    );

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(await screen.findByTestId("app-view-loading-progress")).toHaveTextContent(
      "Loading… 2,500 objects so far (kind-a)",
    );
  });

  it("<AppView /> labels rows kept after a failed initial list as incomplete", async () => {
    const access: StoreAccess[] = [
      {
        cluster: "kind-a",
        kind: "Pod",
        access: { state: "error", message: "list timed out", partialRows: 1200 },
      },
    ];
    const fetchApps = vi.fn(() =>
      Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }),
    );

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Connection error for kind-a: list timed out — showing 1,200 rows received before the error (incomplete)",
    );
  });

  it("<AppView /> labels a reconnecting store as showing last known rows", async () => {
    const access: StoreAccess[] = [
      { cluster: "kind-a", kind: "Pod", access: { state: "reconnecting" }, rows: 2 },
    ];
    const fetchApps = vi.fn(() =>
      Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }),
    );

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(
      await screen.findByText("Reconnecting to kind-a… (showing last known rows)"),
    ).toHaveAttribute("role", "status");
    expect(screen.queryByTestId("app-view-loading-progress")).not.toBeInTheDocument();
  });

  it("<AppView /> R-14d: a failure over kept rows says they are last known", async () => {
    const access: StoreAccess[] = [
      {
        cluster: "kind-a",
        kind: "Pod",
        access: { state: "error", message: "timeout", partialRows: null },
        rows: 3,
      },
      { cluster: "kind-a", kind: "Deployment", access: { state: "reconnecting" }, rows: 0 },
    ];
    const fetchApps = vi.fn(() =>
      Promise.resolve<AppPage>({ access, total: 0, offset: 0, apps: [] }),
    );

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(
      await screen.findByText(
        "Connection error for kind-a: timeout — showing last known rows (may be outdated)",
      ),
    ).toHaveAttribute("role", "alert");
    expect(screen.getByText("Reconnecting to kind-a…")).toHaveAttribute("role", "status");
  });

  describe("<AppView /> re-queries while a store is loading", () => {
    afterEach(() => {
      vi.useRealTimers();
    });

    const loadingPage = (rowsSoFar: number): AppPage => ({
      access: [{ cluster: "kind-a", kind: "Pod", access: { state: "loading", rowsSoFar } }],
      total: 0,
      offset: 0,
      apps: [],
    });
    const readyPage: AppPage = {
      access: [{ cluster: "kind-a", kind: "Pod", access: { state: "ready" } }],
      total: 0,
      offset: 0,
      apps: [],
    };
    const flush = async (ms: number) => {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(ms);
      });
    };

    it("polls every LOADING_POLL_MS until no store is loading", async () => {
      vi.useFakeTimers();
      const pages = [loadingPage(500), loadingPage(1500), readyPage];
      const fetchApps = vi.fn(() => Promise.resolve(pages.shift() ?? readyPage));

      render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);
      await flush(0);
      expect(fetchApps).toHaveBeenCalledTimes(1);
      expect(screen.getByTestId("app-view-loading-progress")).toHaveTextContent("Loading… 500 rows");

      await flush(LOADING_POLL_MS - 1);
      expect(fetchApps).toHaveBeenCalledTimes(1);
      await flush(1);
      expect(fetchApps).toHaveBeenCalledTimes(2);
      expect(screen.getByTestId("app-view-loading-progress")).toHaveTextContent("Loading… 1,500 rows");

      await flush(LOADING_POLL_MS);
      expect(fetchApps).toHaveBeenCalledTimes(3);
      expect(screen.queryByTestId("app-view-loading-progress")).toBeNull();

      // Ready: polling stops.
      await flush(LOADING_POLL_MS * 4);
      expect(fetchApps).toHaveBeenCalledTimes(3);
    });

    it("stops polling on unmount", async () => {
      vi.useFakeTimers();
      const fetchApps = vi.fn(() => Promise.resolve(loadingPage(10)));

      const { unmount } = render(
        <AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />,
      );
      await flush(0);
      expect(fetchApps).toHaveBeenCalledTimes(1);
      unmount();
      await flush(LOADING_POLL_MS * 4);
      expect(fetchApps).toHaveBeenCalledTimes(1);
    });

    it("restarts the poll for a new filter instead of polling the old one", async () => {
      vi.useFakeTimers();
      const fetchApps = vi.fn((spec: AppQuerySpec_Deserialize) =>
        Promise.resolve({ ...loadingPage(10), offset: spec.offset }),
      );

      const { rerender } = render(
        <AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />,
      );
      await flush(0);
      rerender(<AppView clusters={["kind-a"]} namespaces={["team-b"]} fetchApps={fetchApps} />);
      await flush(0);
      expect(fetchApps).toHaveBeenCalledTimes(2);
      await flush(LOADING_POLL_MS);
      expect(fetchApps).toHaveBeenCalledTimes(3);
      expect(fetchApps.mock.calls.every(([spec], i) => i === 0 || spec.namespaces.includes("team-b"))).toBe(
        true,
      );
    });
  });

  it("<AppView /> renders honest banners alongside loaded applications on partial failure", async () => {
    const access: StoreAccess[] = [
      {
        cluster: "kind-b",
        kind: "Deployment",
        access: {
          state: "forbidden",
          resource: "deployments",
          namespace: null,
          message: "RBAC denied",
        },
      },
      {
        cluster: "kind-a",
        kind: "Deployment",
        access: {
          state: "ready",
        },
      },
    ];
    const apps: AppSummary[] = [
      {
        name: "shop-app",
        namespace: "shop",
        cluster: "kind-a",
        components: [
          {
            cluster: "kind-a",
            kind: "Deployment",
            namespace: "shop",
            name: "shop-web",
            status: "healthy",
            statusReason: "1/1",
          },
        ],
      },
    ];
    const fetchApps = vi.fn(() => Promise.resolve<AppPage>({ access, total: 1, offset: 0, apps }));

    render(<AppView clusters={["kind-a", "kind-b"]} namespaces={[]} fetchApps={fetchApps} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveClass("app-view__banner", "app-view__banner--forbidden");
    expect(alert).toHaveTextContent("No permission to list deployments in kind-b: RBAC denied");
    expect(await screen.findByTestId("app-card-shop-app")).toBeInTheDocument();
  });

  it("<AppView /> shows the typed IpcError message when query_apps fails", async () => {
    const ipcError: IpcError = { kind: "kubernetes", message: "No permission to list deployments" };
    const fetchApps = vi.fn(() => Promise.reject(ipcError as unknown as Error));

    render(<AppView clusters={["kind-a"]} namespaces={[]} fetchApps={fetchApps} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("No permission to list deployments");
    expect(screen.queryByTestId("app-view-empty")).not.toBeInTheDocument();
  });
});
