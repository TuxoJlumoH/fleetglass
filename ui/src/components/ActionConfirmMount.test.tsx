import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ContextSettings_Serialize } from "../generated/bindings";
import {
  deleteActionFor,
  resetActionsStore,
  scaleActionFor,
  useActionsStore,
  type PendingAction,
} from "../stores/actions";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import { resetPodsStore, type Row } from "../stores/pods";
import { resetResourceDetailStore } from "../stores/resourceDetail";
import { ActionConfirmDialog } from "./ActionConfirmDialog";
import { CommandPalette } from "./CommandPalette";
import { PodTable } from "./PodTable";

function podRow(i: number): Row {
  return {
    uid: `uid-pod-${String(i)}`,
    cluster: "kind-dev",
    kind: "Pod",
    namespace: "default",
    name: `nginx-${String(i)}`,
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
    labels: [],
    custom: [],
  };
}

const ROWS = [podRow(0), podRow(1), podRow(2)];

const UNPROTECTED: ContextSettings_Serialize = {
  colour: null,
  tags: [],
  readOnly: false,
  protected: false,
  pinned: false,
};

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

type Handler = (args: Record<string, unknown>) => unknown;

function previewFor(args: Record<string, unknown>): string {
  const a = args.action as { type: string; kind: string; namespace: string; name: string };
  return `kubectl --context ${String(args.context)} -n ${a.namespace} ${a.type} ${a.kind.toLowerCase()} ${a.name}`;
}

function mockBackend(calls: Call[], over: Partial<Record<string, Handler>> = {}) {
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    calls.push({ cmd, args: a });
    const handler = over[cmd];
    if (handler) return handler(a);
    switch (cmd) {
      case "query_page":
        return {
          total: ROWS.length,
          offset: 0,
          limit: 100,
          rows: ROWS,
          version: 1,
          access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
        };
      case "subscribe_query":
        return 1;
      case "unsubscribe_query":
        return true;
      case "preview_kubectl_action":
        return previewFor(a);
      case "get_context_settings":
        return UNPROTECTED;
      case "execute_action":
        return null;
      default:
        return undefined;
    }
  });
}

function rowElement(name: string): HTMLElement {
  const table = screen.getByRole("table", { name: "Pods" });
  const el = within(table).getByText(name).closest<HTMLElement>('[role="row"]');
  if (!el) throw new Error(`row ${name} not rendered`);
  return el;
}

function executeCalls(calls: Call[]): Record<string, unknown>[] {
  return calls.filter((c) => c.cmd === "execute_action").map((c) => c.args);
}

describe("R-07 ActionConfirmDialog mounted in PodTable and the palette", () => {
  let calls: Call[];

  beforeEach(() => {
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
    useLayoutStore.setState(initialLayoutState);
    useContextsStore.setState({ ...initialContextsState, selectedContext: "kind-dev" });
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    calls = [];
  });

  afterEach(() => {
    clearMocks();
    resetPodsStore();
    resetResourceDetailStore();
    resetActionsStore();
    useLayoutStore.setState(initialLayoutState);
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("ctrl-d on the selected row opens the dialog for that row with the backend preview", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    await user.click(rowElement("nginx-0"));
    await user.keyboard("j");
    await waitFor(() => {
      expect(rowElement("nginx-1")).toHaveFocus();
    });
    await user.keyboard("{Control>}d{/Control}");

    const dialog = await screen.findByRole("dialog", { name: "Delete pod default/nginx-1" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(await within(dialog).findByTestId("kubectl-preview")).toHaveTextContent(
      "kubectl --context kind-dev -n default delete pod nginx-1",
    );
    expect(calls.find((c) => c.cmd === "preview_kubectl_action")?.args).toEqual({
      context: "kind-dev",
      action: { type: "delete", kind: "Pod", namespace: "default", name: "nginx-1" },
    });
    // Unprotected: no typed-name field; confirm sends no name.
    expect(screen.queryByLabelText("Type resource name to confirm")).toBeNull();
    const confirm = within(dialog).getByTestId("confirm-action-btn");
    await waitFor(() => {
      expect(confirm).toBeEnabled();
    });
    await user.click(confirm);

    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(executeCalls(calls)).toEqual([
      {
        context: "kind-dev",
        action: { type: "delete", kind: "Pod", namespace: "default", name: "nginx-1" },
        confirmedName: null,
      },
    ]);
    expect(screen.getByTestId("action-status")).toHaveTextContent(
      "Delete requested for pod default/nginx-1 on kind-dev",
    );
    // No local removal: the row stays until the live watch drops it.
    expect(rowElement("nginx-1")).toBeInTheDocument();
    await waitFor(() => {
      expect(rowElement("nginx-1")).toHaveFocus();
    });
  });

  it("table keys are blocked while the dialog is open; Escape closes it and restores focus", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => {
      expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
    });

    // j inside the dialog and on the body does not move the selection.
    fireEvent.keyDown(dialog, { key: "j" });
    fireEvent.keyDown(document.body, { key: "j" });
    expect(rowElement("nginx-0")).toHaveAttribute("data-selected", "true");

    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(rowElement("nginx-0")).toHaveFocus();
    expect(executeCalls(calls)).toEqual([]);
  });

  it("Cancel closes the dialog without running the action and restores focus", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    await user.click(rowElement("nginx-2"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

    expect(screen.queryByRole("dialog")).toBeNull();
    await waitFor(() => {
      expect(rowElement("nginx-2")).toHaveFocus();
    });
    expect(executeCalls(calls)).toEqual([]);
  });

  it("protected context: confirm stays disabled until the exact name is typed; confirmedName is sent", async () => {
    mockBackend(calls, { get_context_settings: () => ({ ...UNPROTECTED, protected: true }) });
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");
    const input = await within(dialog).findByLabelText("Type resource name to confirm");
    const confirm = within(dialog).getByTestId("confirm-action-btn");
    await within(dialog).findByTestId("kubectl-preview");

    expect(confirm).toBeDisabled();
    await user.type(input, "nginx-");
    expect(confirm).toBeDisabled();
    await user.type(input, "00");
    expect(confirm).toBeDisabled();
    await user.clear(input);
    await user.type(input, "NGINX-0");
    expect(confirm).toBeDisabled();
    // Enter in the input with a wrong name does not submit.
    await user.keyboard("{Enter}");
    expect(executeCalls(calls)).toEqual([]);

    await user.clear(input);
    await user.type(input, "nginx-0");
    expect(confirm).toBeEnabled();
    await user.click(confirm);

    await waitFor(() => {
      expect(executeCalls(calls)).toEqual([
        {
          context: "kind-dev",
          action: { type: "delete", kind: "Pod", namespace: "default", name: "nginx-0" },
          confirmedName: "nginx-0",
        },
      ]);
    });
  });

  it("read-only context: confirm is disabled and the dialog says why", async () => {
    mockBackend(calls, {
      get_context_settings: () => ({ ...UNPROTECTED, readOnly: true, protected: true }),
    });
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");

    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");

    expect(await within(dialog).findByTestId("readonly-blocked")).toHaveTextContent(
      "Context kind-dev is read-only: this action is disabled.",
    );
    expect(await within(dialog).findByTestId("kubectl-preview")).toHaveTextContent(
      "delete pod nginx-0",
    );
    expect(within(dialog).getByTestId("confirm-action-btn")).toBeDisabled();
    expect(within(dialog).queryByLabelText("Type resource name to confirm")).toBeNull();
    expect(executeCalls(calls)).toEqual([]);
  });

  it.each([
    ["readOnlyContext", "context kind-dev is read-only", "Blocked: the context is read-only."],
    [
      "confirmationRequired",
      "type nginx-0 to confirm on protected context kind-dev",
      "Blocked: this context is protected and needs the exact name.",
    ],
    ["kubernetes", 'pods "nginx-0" is forbidden', "Action failed:"],
  ] as const)(
    "shows the backend's %s error in the dialog",
    async (kind, message, prefix) => {
      // Settings changed on disk after the dialog loaded them: the backend still refuses.
      mockBackend(calls, {
        execute_action: () => {
          throw { kind, message } as unknown as Error;
        },
      });
      const user = userEvent.setup();
      render(<PodTable />);
      await screen.findByText("nginx-2");

      await user.click(rowElement("nginx-0"));
      await user.keyboard("{Control>}d{/Control}");
      const dialog = await screen.findByRole("dialog");
      const confirm = within(dialog).getByTestId("confirm-action-btn");
      await waitFor(() => {
        expect(confirm).toBeEnabled();
      });
      await user.click(confirm);

      const alert = await within(dialog).findByTestId("action-error");
      expect(alert).toHaveTextContent(prefix);
      expect(alert).toHaveTextContent(message);
      expect(screen.getByRole("dialog")).toBeInTheDocument();
      expect(screen.queryByTestId("action-status")).toBeNull();
    },
  );

  it("shows the preview failure instead of a made-up command", async () => {
    mockBackend(calls, {
      preview_kubectl_action: () => {
        throw new Error("preview unavailable");
      },
    });
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");
    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");

    expect(await screen.findByTestId("kubectl-preview-error")).toHaveTextContent(
      "kubectl preview unavailable: preview unavailable",
    );
    expect(screen.queryByTestId("kubectl-preview")).toBeNull();
    expect(screen.getByTestId("confirm-action-btn")).toBeDisabled();
  });

  it("s and r on a pod explain that they apply to workloads instead of opening a dialog", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");
    await user.click(rowElement("nginx-0"));

    await user.keyboard("s");
    expect(screen.getByTestId("action-status")).toHaveTextContent(
      "Scale works on Deployments and StatefulSets; Pod nginx-0 cannot be scaled directly.",
    );
    await user.keyboard("r");
    expect(screen.getByTestId("action-status")).toHaveTextContent(
      "Restart works on Deployments, StatefulSets and DaemonSets; Pod nginx-0 cannot be restarted directly.",
    );
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(calls.some((c) => c.cmd === "preview_kubectl_action")).toBe(false);
  });

  it("palette offers delete for the selected row and opens the dialog for it", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    render(
      <>
        <PodTable />
        <CommandPalette />
      </>,
    );
    await screen.findByText("nginx-2");
    await user.click(rowElement("nginx-2"));

    await user.keyboard("{Control>}k{/Control}");
    const palette = await screen.findByRole("dialog", { name: "Command palette" });
    // Pods are not workloads: no scale / restart entries.
    expect(within(palette).queryByText(/^Scale /)).toBeNull();
    expect(within(palette).queryByText(/^Restart /)).toBeNull();
    await user.type(within(palette).getByLabelText("Command palette search"), "delete");
    await user.click(within(palette).getByText("Delete pod nginx-2"));

    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
    const dialog = await screen.findByRole("dialog", { name: "Delete pod default/nginx-2" });
    expect(await within(dialog).findByTestId("kubectl-preview")).toHaveTextContent(
      "kubectl --context kind-dev -n default delete pod nginx-2",
    );
  });

  it("palette offers scale and restart for a selected workload; scale edits replicas", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    act(() => {
      useActionsStore.getState().setSelection({
        ...podRow(0),
        uid: "uid-deploy",
        kind: "Deployment",
        name: "web",
        cells: [{ type: "ready", value: { ready: 3, total: 3 } }],
      });
    });
    render(<CommandPalette />);
    act(() => {
      useLayoutStore.getState().openPalette();
    });

    const palette = await screen.findByRole("dialog", { name: "Command palette" });
    expect(within(palette).getByText("Restart deployment web")).toBeInTheDocument();
    await user.click(within(palette).getByText("Scale deployment web"));

    // The dialog is mounted by PodTable; render it standalone here.
    render(<ActionConfirmDialog />);
    const dialog = await screen.findByRole("dialog", { name: "Scale deployment default/web" });
    const replicas = within(dialog).getByLabelText("Replicas");
    expect(replicas).toHaveValue("3");
    fireEvent.change(replicas, { target: { value: "1" } });
    await waitFor(() => {
      expect(useActionsStore.getState().pending?.action).toMatchObject({ replicas: 1 });
    });
    await waitFor(() => {
      expect(within(dialog).getByTestId("confirm-action-btn")).toBeEnabled();
    });
    await user.click(within(dialog).getByTestId("confirm-action-btn"));
    await waitFor(() => {
      expect(executeCalls(calls)).toEqual([
        {
          context: "kind-dev",
          action: { type: "scale", kind: "Deployment", namespace: "default", name: "web", replicas: 1 },
          confirmedName: null,
        },
      ]);
    });
    expect(useActionsStore.getState().lastSuccess).toBe(
      "Scale to 1 replica requested for deployment default/web on kind-dev",
    );
  });

  it("an in-flight delete cannot be replaced from the palette: one execute, first result shown", async () => {
    let resolveExec: (v: null) => void = () => undefined;
    mockBackend(calls, {
      execute_action: () =>
        new Promise<null>((res) => {
          resolveExec = res;
        }),
    });
    const user = userEvent.setup();
    render(
      <>
        <PodTable />
        <CommandPalette />
      </>,
    );
    await screen.findByText("nginx-2");
    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog", { name: "Delete pod default/nginx-0" });
    const confirm = within(dialog).getByTestId("confirm-action-btn");
    await waitFor(() => {
      expect(confirm).toBeEnabled();
    });
    await user.click(confirm);
    await waitFor(() => {
      expect(useActionsStore.getState().running).toBe(true);
    });

    // ctrl-k does not open the palette over the action dialog.
    await user.keyboard("{Control>}k{/Control}");
    expect(useLayoutStore.getState().paletteOpen).toBe(false);
    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();

    // Even if the palette were open, its actions are hidden and request() refuses.
    act(() => {
      useLayoutStore.getState().openPalette();
    });
    const palette = await screen.findByRole("dialog", { name: "Command palette" });
    expect(within(palette).queryByText(/^Actions:/)).toBeNull();
    expect(within(palette).queryByText(/^Delete pod/)).toBeNull();
    act(() => {
      useLayoutStore.getState().closePalette();
    });
    let accepted = true;
    act(() => {
      accepted = useActionsStore.getState().request(deleteActionFor(ROWS[1] as Row));
    });
    expect(accepted).toBe(false);

    expect(useActionsStore.getState().running).toBe(true);
    expect(within(dialog).getByTestId("confirm-action-btn")).toBeDisabled();
    expect(within(dialog).getByTestId("confirm-action-btn")).toHaveTextContent("Running…");
    expect(executeCalls(calls)).toHaveLength(1);

    act(() => {
      resolveExec(null);
    });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(screen.getByTestId("action-status")).toHaveTextContent(
      "Delete requested for pod default/nginx-0 on kind-dev",
    );
    expect(executeCalls(calls)).toHaveLength(1);
  });

  it("palette hides the Actions group while an action is pending", async () => {
    mockBackend(calls);
    act(() => {
      useActionsStore.getState().setSelection(podRow(1));
    });
    render(<CommandPalette />);
    act(() => {
      useLayoutStore.getState().openPalette();
    });
    let palette = await screen.findByRole("dialog", { name: "Command palette" });
    expect(within(palette).getByText("Delete pod nginx-1")).toBeInTheDocument();

    act(() => {
      useActionsStore.getState().request(deleteActionFor(podRow(0)));
    });
    palette = screen.getByRole("dialog", { name: "Command palette" });
    expect(within(palette).queryByText("Delete pod nginx-1")).toBeNull();
    expect(within(palette).queryByText(/^Actions:/)).toBeNull();
  });

  it("Tab and Shift-Tab stay inside the dialog", async () => {
    mockBackend(calls, { get_context_settings: () => ({ ...UNPROTECTED, protected: true }) });
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");
    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");
    const input = await within(dialog).findByLabelText("Type resource name to confirm");
    await within(dialog).findByTestId("kubectl-preview");
    await user.type(input, "nginx-0");
    const confirm = within(dialog).getByTestId("confirm-action-btn");
    await waitFor(() => {
      expect(confirm).toBeEnabled();
    });
    const copy = within(dialog).getByRole("button", { name: "Copy as kubectl" });

    confirm.focus();
    await user.tab();
    expect(copy).toHaveFocus();
    await user.tab({ shift: true });
    expect(confirm).toHaveFocus();
    for (let i = 0; i < 6; i += 1) {
      await user.tab();
      expect(dialog.contains(document.activeElement)).toBe(true);
    }
    for (let i = 0; i < 6; i += 1) {
      await user.tab({ shift: true });
      expect(dialog.contains(document.activeElement)).toBe(true);
    }
  });

  it("backdrop closes only for a press that starts and ends on it", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("nginx-2");
    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");
    const backdrop = screen.getByTestId("action-confirm-backdrop");
    const cancel = within(dialog).getByRole("button", { name: "Cancel" });

    // Drag from inside the dialog, release over the backdrop.
    fireEvent.mouseDown(cancel);
    fireEvent.mouseUp(backdrop);
    fireEvent.click(backdrop);
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    // Press on the backdrop, release inside the dialog (click lands on the common ancestor).
    fireEvent.mouseDown(backdrop);
    fireEvent.click(cancel.closest("form") as HTMLElement);
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    fireEvent.mouseDown(backdrop);
    fireEvent.click(backdrop);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(executeCalls(calls)).toEqual([]);
  });

  it("Copy as kubectl copies the backend preview and resets after ~2 s", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    render(<PodTable />);
    await screen.findByText("nginx-2");
    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByTestId("kubectl-preview");

    await user.click(within(dialog).getByRole("button", { name: "Copy as kubectl" }));
    expect(writeText).toHaveBeenCalledWith("kubectl --context kind-dev -n default delete pod nginx-0");
    expect(within(dialog).getByRole("button", { name: "Copied" })).toBeInTheDocument();
    await waitFor(
      () => {
        expect(within(dialog).getByRole("button", { name: "Copy as kubectl" })).toBeInTheDocument();
      },
      { timeout: 3000 },
    );
    expect(within(dialog).queryByTestId("copy-error")).toBeNull();
    writeText.mockRestore();
  });

  it("Copy as kubectl reports a clipboard failure instead of claiming success", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    const writeText = vi
      .spyOn(navigator.clipboard, "writeText")
      .mockRejectedValue(new Error("Clipboard write denied"));
    render(<PodTable />);
    await screen.findByText("nginx-0");
    await user.click(rowElement("nginx-0"));
    await user.keyboard("{Control>}d{/Control}");
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByTestId("kubectl-preview");

    await user.click(within(dialog).getByRole("button", { name: "Copy as kubectl" }));
    expect(await within(dialog).findByTestId("copy-error")).toHaveTextContent(
      "Could not copy to the clipboard: Clipboard write denied",
    );
    expect(within(dialog).queryByRole("button", { name: "Copied" })).toBeNull();
    writeText.mockRestore();
  });

  it("replicas: the field can be cleared; empty, invalid and too-large values block confirm", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    act(() => {
      const deployment: Row = {
        ...podRow(0),
        uid: "uid-deploy",
        kind: "Deployment",
        name: "web",
        cells: [{ type: "ready", value: { ready: 3, total: 3 } }],
      };
      useActionsStore.getState().request(scaleActionFor(deployment) as PendingAction);
    });
    render(<ActionConfirmDialog />);
    const dialog = await screen.findByRole("dialog", { name: "Scale deployment default/web" });
    const replicas = within(dialog).getByLabelText("Replicas");
    const confirm = within(dialog).getByTestId("confirm-action-btn");
    await waitFor(() => {
      expect(confirm).toBeEnabled();
    });

    await user.clear(replicas);
    expect(replicas).toHaveValue("");
    expect(within(dialog).getByTestId("replicas-error")).toHaveTextContent("Enter the number of replicas.");
    expect(confirm).toBeDisabled();
    expect(within(dialog).queryByTestId("kubectl-preview")).toBeNull();

    await user.type(replicas, "1");
    await waitFor(() => {
      expect(within(dialog).getByTestId("kubectl-preview")).toHaveTextContent("scale deployment web");
    });
    expect(within(dialog).getByText("Scale to 1 replica")).toBeInTheDocument();
    await waitFor(() => {
      expect(confirm).toBeEnabled();
    });

    await user.type(replicas, "x");
    expect(within(dialog).getByTestId("replicas-error")).toHaveTextContent(
      "Replicas must be a whole number of 0 or more.",
    );
    expect(confirm).toBeDisabled();

    await user.clear(replicas);
    await user.type(replicas, "2147483648");
    expect(within(dialog).getByTestId("replicas-error")).toHaveTextContent(
      "Replicas must be at most 2147483647.",
    );
    expect(confirm).toBeDisabled();
    await user.keyboard("{Enter}");
    expect(executeCalls(calls)).toEqual([]);
    // The invalid value never reached the store as a count.
    expect(useActionsStore.getState().pending?.replicasUnset).toBe(true);
    expect(useActionsStore.getState().preview).toBeNull();
  });

  it("replicas: without a known desired count the field starts empty and must be filled", async () => {
    mockBackend(calls);
    const user = userEvent.setup();
    act(() => {
      const statefulSet: Row = { ...podRow(0), uid: "uid-sts", kind: "StatefulSet", name: "db", cells: [] };
      useActionsStore.getState().request(scaleActionFor(statefulSet) as PendingAction);
    });
    render(<ActionConfirmDialog />);
    const dialog = await screen.findByRole("dialog", { name: "Scale statefulset default/db" });
    const replicas = within(dialog).getByLabelText("Replicas");
    expect(replicas).toHaveValue("");
    expect(within(dialog).getByTestId("kubectl-preview-waiting")).toBeInTheDocument();
    expect(within(dialog).getByTestId("confirm-action-btn")).toBeDisabled();
    expect(calls.some((c) => c.cmd === "preview_kubectl_action")).toBe(false);

    await user.type(replicas, "2");
    await waitFor(() => {
      expect(within(dialog).getByTestId("confirm-action-btn")).toBeEnabled();
    });
    expect(calls.find((c) => c.cmd === "preview_kubectl_action")?.args).toMatchObject({
      action: { type: "scale", kind: "StatefulSet", name: "db", replicas: 2 },
    });
  });

  it("ctrl-d is swallowed even when no row is selected", async () => {
    mockBackend(calls, {
      query_page: () => ({
        total: 0,
        offset: 0,
        limit: 100,
        rows: [],
        version: 1,
        access: [{ cluster: "kind-dev", kind: "Pod", access: { state: "ready" } }],
      }),
    });
    render(<PodTable />);
    await waitFor(() => {
      expect(calls.some((c) => c.cmd === "query_page")).toBe(true);
    });
    const notPrevented = fireEvent.keyDown(document.body, { key: "d", ctrlKey: true });
    expect(notPrevented).toBe(false);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(calls.some((c) => c.cmd === "preview_kubectl_action")).toBe(false);
  });
});
