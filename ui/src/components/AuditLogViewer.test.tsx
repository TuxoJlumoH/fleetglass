import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { AuditEntry, AuditLogPage } from "../generated/bindings";
import { AuditLogViewer, type AuditLogViewerProps } from "./AuditLogViewer";

function makeEntry(overrides: Partial<AuditEntry> = {}): AuditEntry {
  return {
    id: 1,
    timestamp: "2026-04-16T12:00:00Z",
    cluster: "prod-us-east",
    user: "alice",
    action: "delete",
    kind: "Pod",
    namespace: "default",
    name: "web-app-0",
    kubectl: "kubectl --context prod-us-east -n default delete pod web-app-0",
    success: true,
    error: null,
    ...overrides,
  };
}

describe("<AuditLogViewer />", () => {
  it("renders audit entries with cluster, user, action, resource, status and kubectl copy button", async () => {
    const entries: AuditEntry[] = [
      makeEntry({
        id: 1,
        cluster: "prod-cluster",
        user: "alice",
        action: "delete",
        kind: "Pod",
        namespace: "default",
        name: "web-app-0",
        kubectl: "kubectl --context prod-cluster -n default delete pod web-app-0",
        success: true,
      }),
      makeEntry({
        id: 2,
        cluster: "staging-cluster",
        user: null,
        action: "restart",
        kind: "Deployment",
        namespace: "staging",
        name: "api-gateway",
        kubectl: "kubectl --context staging-cluster -n staging rollout restart deployment api-gateway",
        success: false,
        error: "Forbidden: user cannot patch deployment",
      }),
    ];

    const initialPage: AuditLogPage = {
      total: 2,
      offset: 0,
      entries,
    };
    const fetchAuditLog = vi.fn().mockResolvedValue(initialPage);

    render(<AuditLogViewer fetchAuditLog={fetchAuditLog} />);

    // Accessible container
    const container = screen.getByRole("region", { name: "Audit log" });
    expect(container).toBeInTheDocument();

    // Loading then data
    expect(await screen.findByText("prod-cluster")).toBeInTheDocument();
    expect(screen.getByText("staging-cluster")).toBeInTheDocument();

    // User display: alice and "-" for null user
    expect(screen.getByText("alice")).toBeInTheDocument();
    expect(screen.getByText("-")).toBeInTheDocument();

    // Action pills
    expect(screen.getByText("delete")).toHaveClass("audit-pill--delete");
    expect(screen.getByText("restart")).toHaveClass("audit-pill--restart");

    // Resource formatting
    expect(screen.getByText("Pod/web-app-0")).toBeInTheDocument();
    expect(screen.getByText("(default)")).toBeInTheDocument();
    expect(screen.getByText("Deployment/api-gateway")).toBeInTheDocument();
    expect(screen.getByText("(staging)")).toBeInTheDocument();

    // Status badges: Success and Failed with error tooltip
    expect(screen.getByText("Success")).toHaveClass("audit-status--success");
    const failedBadge = screen.getByText("Failed");
    expect(failedBadge).toHaveClass("audit-status--failed");
    expect(failedBadge).toHaveAttribute("title", "Forbidden: user cannot patch deployment");

    // Kubectl command and Copy buttons
    expect(
      screen.getByText("kubectl --context prod-cluster -n default delete pod web-app-0"),
    ).toBeInTheDocument();
    const copyButtons = screen.getAllByTestId("copy-kubectl");
    expect(copyButtons).toHaveLength(2);

    // Initial fetch was called with offset 0 and null filters
    expect(fetchAuditLog).toHaveBeenCalledWith(0, 100, null, null);
  });

  it("filters by cluster and action, and supports refresh", async () => {
    const user = userEvent.setup();
    const fetchAuditLog = vi
      .fn<NonNullable<AuditLogViewerProps["fetchAuditLog"]>>()
      .mockImplementation(
        (offset: number, _limit: number, cluster: string | null, action: string | null) => {
          let filtered = [
            makeEntry({ id: 1, cluster: "prod", action: "delete" }),
            makeEntry({ id: 2, cluster: "staging", action: "restart" }),
          ];
          if (cluster) {
            filtered = filtered.filter((e) => e.cluster.includes(cluster));
          }
          if (action) {
            filtered = filtered.filter((e) => e.action.includes(action));
          }
          const page: AuditLogPage = {
            total: filtered.length,
            offset,
            entries: filtered,
          };
          return Promise.resolve(page);
        },
      );

    render(<AuditLogViewer fetchAuditLog={fetchAuditLog} />);

    await screen.findByText("prod");
    expect(screen.getByText("staging")).toBeInTheDocument();

    // Filter by cluster
    const clusterInput = screen.getByLabelText("Filter by cluster");
    await user.type(clusterInput, "prod");

    await waitFor(() => {
      expect(fetchAuditLog).toHaveBeenLastCalledWith(0, 100, "prod", null);
    });

    // Filter by action
    const actionInput = screen.getByLabelText("Filter by action");
    await user.type(actionInput, "delete");

    await waitFor(() => {
      expect(fetchAuditLog).toHaveBeenLastCalledWith(0, 100, "prod", "delete");
    });

    // Refresh button triggers refetch
    const callCountBefore = fetchAuditLog.mock.calls.length;
    const refreshBtn = screen.getByRole("button", { name: "Refresh audit log" });
    await user.click(refreshBtn);

    await waitFor(() => {
      expect(fetchAuditLog.mock.calls.length).toBeGreaterThan(callCountBefore);
    });
  });

  it("copies the kubectl command to clipboard and indicates copied status", async () => {
    const user = userEvent.setup();
    const entry = makeEntry({
      id: 42,
      kubectl: "kubectl delete pod nginx-prod",
    });

    const page: AuditLogPage = {
      total: 1,
      offset: 0,
      entries: [entry],
    };
    const fetchAuditLog = vi.fn().mockResolvedValue(page);

    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();

    render(<AuditLogViewer fetchAuditLog={fetchAuditLog} />);

    await screen.findByText("kubectl delete pod nginx-prod");

    const copyBtn = screen.getByTestId("copy-kubectl");
    expect(copyBtn).toHaveTextContent("Copy");

    await user.click(copyBtn);

    expect(writeText).toHaveBeenCalledWith("kubectl delete pod nginx-prod");
    expect(copyBtn).toHaveTextContent("Copied");
  });

  it("shows empty state when no audit records are returned", async () => {
    const page: AuditLogPage = {
      total: 0,
      offset: 0,
      entries: [],
    };
    const fetchAuditLog = vi.fn().mockResolvedValue(page);

    render(<AuditLogViewer fetchAuditLog={fetchAuditLog} />);

    expect(await screen.findByText("No audit events recorded")).toBeInTheDocument();
    expect(screen.getByTestId("audit-empty")).toBeInTheDocument();
  });

  it("shows error alert state when fetching fails", async () => {
    const fetchAuditLog = vi.fn().mockRejectedValue(new Error("Database connection lost"));

    render(<AuditLogViewer fetchAuditLog={fetchAuditLog} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Failed to load audit log: Database connection lost");
    expect(screen.getByTestId("audit-error")).toBeInTheDocument();
  });

  it("requests the next page when 'Load more' is clicked", async () => {
    const user = userEvent.setup();
    const firstPageEntries: AuditEntry[] = Array.from({ length: 100 }, (_, i) =>
      makeEntry({ id: i + 1, name: `pod-${String(i)}` }),
    );
    const secondPageEntries: AuditEntry[] = Array.from({ length: 5 }, (_, i) =>
      makeEntry({ id: 100 + i + 1, name: `pod-${String(100 + i)}` }),
    );

    const fetchAuditLog = vi
      .fn<NonNullable<AuditLogViewerProps["fetchAuditLog"]>>()
      .mockImplementation((offset: number) => {
        if (offset === 0) {
          const page: AuditLogPage = {
            total: 105,
            offset: 0,
            entries: firstPageEntries,
          };
          return Promise.resolve(page);
        }
        const page: AuditLogPage = {
          total: 105,
          offset: 100,
          entries: secondPageEntries,
        };
        return Promise.resolve(page);
      });

    render(<AuditLogViewer fetchAuditLog={fetchAuditLog} />);

    await screen.findByText("Pod/pod-0");
    expect(screen.getByText("100 of 105 events")).toBeInTheDocument();

    const loadMoreBtn = screen.getByRole("button", { name: "Load more" });
    expect(loadMoreBtn).toBeInTheDocument();

    await user.click(loadMoreBtn);

    expect(fetchAuditLog).toHaveBeenCalledWith(100, 100, null, null);
    expect(await screen.findByText("Pod/pod-104")).toBeInTheDocument();
    expect(screen.getByText("105 of 105 events")).toBeInTheDocument();

    // When all rows are loaded, "Load more" button is removed
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("calls onClose when the close button is clicked", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const page: AuditLogPage = {
      total: 0,
      offset: 0,
      entries: [],
    };
    const fetchAuditLog = vi.fn().mockResolvedValue(page);

    render(<AuditLogViewer fetchAuditLog={fetchAuditLog} onClose={onClose} />);

    const closeBtn = screen.getByRole("button", { name: "Close audit log" });
    await user.click(closeBtn);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
