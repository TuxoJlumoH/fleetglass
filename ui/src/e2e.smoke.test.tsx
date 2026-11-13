import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ExecShellPanel } from "./components/OpsPanels";
import { PodTable } from "./components/PodTable";
import { ResourceDrawer } from "./components/ResourceDrawer";
import { Sidebar } from "./components/Sidebar";
import { deleteActionFor, resetActionsStore, useActionsStore } from "./stores/actions";
import { useContextsStore } from "./stores/contexts";
import { resetPodsStore, setQueryFetcher, type Page, type Row } from "./stores/pods";

const SAMPLE_ROW: Row = {
  uid: "uid-web-0",
  cluster: "prod",
  kind: "Pod",
  namespace: "default",
  name: "web-0",
  status: "healthy",
  statusReason: "Running",
  cells: [
    { type: "ready", value: { ready: 1, total: 1 } },
    { type: "status", value: { label: "Running", phase: "healthy" } },
    { type: "int", value: 0 },
    { type: "text", value: "node-1" },
    { type: "text", value: "10.244.0.12" },
    { type: "timestamp", value: "2026-10-31T18:00:00Z" },
  ],
  labels: [["app", "web"]],
  custom: [],
};

describe("P1-15 E2E smoke suite (list, logs, exec, delete-with-confirm)", () => {
  afterEach(() => {
    clearMocks();
    resetActionsStore();
  });

  beforeEach(() => {
    resetPodsStore();
    resetActionsStore();
    useContextsStore.setState({
      status: "ready",
      available: true,
      contexts: [
        {
          name: "prod",
          cluster: "prod-cluster",
          user: "admin",
          namespace: "default",
          server: "https://127.0.0.1:7443",
          auth: { type: "token" },
          isCurrent: true,
        },
      ],
      current: "prod",
      warnings: [],
      error: null,
      selectedContext: "prod",
      connectionStatus: { prod: { state: "connected" } },
      settingsByContext: {
        prod: {
          colour: "#ef4444",
          tags: ["production"],
          readOnly: false,
          protected: true,
          pinned: true,
        },
      },
    });
  });

  it("exercises context list, pod table filter/sort, detail YAML dry-run, logs search, exec shell, and protected delete-with-confirm", async () => {
    const user = userEvent.setup();
    const fetcher = vi.fn((spec: { offset: number; limit: number }): Promise<Page> =>
      Promise.resolve({
        total: 1,
        offset: spec.offset,
        limit: spec.limit,
        version: 1,
        rows: [SAMPLE_ROW],
        access: [{ cluster: "prod", kind: "Pod", access: { state: "ready" }, rows: 0, metrics: null }],
        statusCounts: { healthy: 0, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
      }),
    );
    setQueryFetcher(fetcher);

    const onStartExec = vi.fn();
    const executed: unknown[] = [];
    mockIPC((cmd, args) => {
      if (cmd === "preview_kubectl_action") return "kubectl --context prod -n default delete pod web-0";
      if (cmd === "get_context_settings") {
        return { colour: "#ef4444", tags: ["production"], readOnly: false, protected: true, pinned: true };
      }
      if (cmd === "execute_action") {
        executed.push(args);
        return null;
      }
      return undefined;
    });

    render(
      <div>
        <Sidebar />
        <PodTable />
        <ResourceDrawer
          row={SAMPLE_ROW}
          yamlText={"apiVersion: v1\nkind: Pod\nmetadata:\n  name: web-0\n"}
          logLines={[
            { seq: 1, text: "INFO server listening on :80" },
            { seq: 2, text: "ERROR upstream timeout on /api" },
          ]}
          onClose={() => undefined}
          onDryRunApply={() =>
            Promise.resolve({
              diff: "-replicas: 1\n+replicas: 2",
              kubectlPreview: "kubectl --context prod apply --server-side -f -",
            })
          }
        />
        <ExecShellPanel
          pod="web-0"
          containers={["nginx", "sidecar"]}
          readOnly={false}
          onStartExec={onStartExec}
        />
      </div>,
    );

    // 1. Context list & status light + protected badge
    expect(screen.getByTestId("status-light-prod")).toHaveAttribute("aria-label", "Connected");
    expect(screen.getByTestId("badge-protected-prod")).toHaveTextContent("PROTECTED");

    // 2. PodTable loads initial page and dispatches filter
    await waitFor(() => {
      expect(screen.getByTestId("pod-total")).toHaveTextContent(/^1 pod$/);
    });
    const filterInput = screen.getByLabelText("Filter pods");
    await user.type(filterInput, "web");
    await waitFor(() => {
      expect(fetcher).toHaveBeenCalledWith(expect.objectContaining({ textFilter: "web" }));
    });

    // 3. Detail YAML dry-run & diff
    await user.click(screen.getByRole("tab", { name: "YAML" }));
    await user.click(screen.getByRole("button", { name: "Dry-run & Diff" }));
    expect(await screen.findByTestId("yaml-diff-preview")).toHaveTextContent("+replicas: 2");

    // 4. Logs regex search
    await user.click(screen.getByRole("tab", { name: "Logs" }));
    await user.type(screen.getByLabelText("Search logs (regex)"), "ERROR.*timeout");
    expect(screen.getByTestId("log-lines")).toHaveTextContent("ERROR upstream timeout");
    expect(screen.getByTestId("log-lines")).not.toHaveTextContent("INFO server listening");

    // 5. Exec shell container picker
    await user.selectOptions(screen.getByLabelText("Select container"), "sidecar");
    await user.click(screen.getByTestId("exec-connect-btn"));
    expect(onStartExec).toHaveBeenCalledWith("sidecar", "/bin/sh");

    // 6. Delete with mandatory type-the-name confirmation on protected context
    act(() => {
      useActionsStore.getState().request(deleteActionFor(SAMPLE_ROW));
    });
    expect(await screen.findByTestId("kubectl-preview")).toHaveTextContent(
      "kubectl --context prod -n default delete pod web-0",
    );
    const confirmBtn = screen.getByTestId("confirm-action-btn");
    expect(confirmBtn).toBeDisabled();
    await user.type(await screen.findByLabelText("Type resource name to confirm"), "web-0");
    expect(confirmBtn).toBeEnabled();
    await user.click(confirmBtn);
    await waitFor(() => {
      expect(executed).toEqual([
        {
          context: "prod",
          action: { type: "delete", kind: "Pod", namespace: "default", name: "web-0" },
          confirmedName: "web-0",
        },
      ]);
    });
  });
});
