import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ContextList_Serialize, DiagnosticReport, IpcError } from "../generated/bindings";
import { initialContextsState, setConnectHandler, useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import { Sidebar } from "./Sidebar";

const LIST: ContextList_Serialize = {
  contexts: [
    {
      name: "dev",
      cluster: "dev-cluster",
      user: "dev-user",
      namespace: null,
      server: "https://dev.example.test:6443",
      auth: { type: "token" },
      isCurrent: true,
    },
    {
      name: "broken",
      cluster: "broken-cluster",
      user: "broken-user",
      namespace: null,
      server: "https://127.0.0.1:1",
      auth: { type: "token" },
      isCurrent: false,
    },
  ],
  current: "dev",
  warnings: [],
};

function reportFor(context: string): DiagnosticReport {
  return {
    context,
    steps: [
      { name: "kubeconfig", status: "pass", detail: "Context found" },
      { name: "tcp", status: "fail", detail: "Connection refused" },
    ],
    rbac: [{ resource: "pods", verb: "list", allowed: false }],
  };
}

interface Call {
  cmd: string;
  args: unknown;
}

/** Pretend to be inside Tauri; `diagnostics` answers `run_connection_diagnostics`. */
function mockBackend(diagnostics: (context: string) => unknown): Call[] {
  (globalThis as { isTauri?: boolean }).isTauri = true;
  mockWindows("main");
  const calls: Call[] = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === "list_contexts") return LIST;
    if (cmd === "run_connection_diagnostics") {
      return diagnostics((args as { context: string }).context);
    }
    return undefined;
  });
  return calls;
}

interface Deferred {
  promise: Promise<void>;
  resolve: () => void;
}

function deferred(): Deferred {
  let resolve: () => void = () => undefined;
  const promise = new Promise<void>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/** Each context's diagnostics response waits for its own gate. */
function gatedBackend(contexts: string[]) {
  const gates = new Map(contexts.map((c) => [c, deferred()]));
  const calls = mockBackend(async (context) => {
    await gates.get(context)?.promise;
    return reportFor(context);
  });
  const release = async (context: string) => {
    const gate = gates.get(context);
    await act(async () => {
      gate?.resolve();
      await gate?.promise;
      // Let the IPC mock's promise chain and the hook's continuation settle.
      await new Promise((r) => setTimeout(r, 0));
    });
  };
  return { calls, release };
}

const rowOf = (name: string): HTMLElement => {
  const li = screen.getByText(name).closest("li");
  if (!li) throw new Error(`no row for ${name}`);
  return li;
};

const diagCalls = (calls: Call[]) => calls.filter((c) => c.cmd === "run_connection_diagnostics");

async function renderLoaded() {
  render(<Sidebar />);
  await act(() => useContextsStore.getState().loadContexts());
}

describe("Sidebar connection diagnostics (R-12)", () => {
  beforeEach(() => {
    useLayoutStore.setState(initialLayoutState);
    useContextsStore.setState(initialContextsState);
    setConnectHandler();
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("the per-context button opens the dialog with steps without selecting, and closes", async () => {
    const user = userEvent.setup();
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const calls = mockBackend(async (context) => {
      await gate;
      return reportFor(context);
    });
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Run diagnostics for broken" }));

    const dialog = screen.getByRole("dialog", { name: "Connection diagnostics" });
    expect(within(dialog).getByRole("status")).toHaveTextContent("Running diagnostics…");
    expect(diagCalls(calls)).toEqual([
      { cmd: "run_connection_diagnostics", args: { context: "broken" } },
    ]);
    // stopPropagation: the row click handler did not select the context.
    expect(useContextsStore.getState().selectedContext).toBe("dev");

    await act(async () => {
      release();
      await gate;
    });

    expect(await within(dialog).findByTestId("diag-step-tcp")).toHaveTextContent(
      "FAILtcpConnection refused",
    );
    expect(within(dialog).getByTestId("diag-step-kubeconfig")).toHaveTextContent("PASS");
    expect(within(dialog).getByText("Denied")).toBeInTheDocument();
    expect(dialog).toHaveTextContent("Connection diagnostics: broken");
    expect(within(dialog).queryByRole("status")).toBeNull();

    await user.click(within(dialog).getByRole("button", { name: "Close diagnostics" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("Escape closes the dialog without moving the context list focus", async () => {
    const user = userEvent.setup();
    mockBackend((context) => reportFor(context));
    await renderLoaded();

    const devRow = rowOf("dev");
    act(() => {
      devRow.focus();
    });
    await user.keyboard("d");
    await screen.findByTestId("diag-step-tcp");
    // Focus starts on the close button; arrow keys must not reach the roving list handler.
    const closeBtn = screen.getByRole("button", { name: "Close diagnostics" });
    expect(closeBtn).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(closeBtn).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    // Focus returns to the row the dialog was opened from, not the next row.
    expect(devRow).toHaveFocus();
  });

  it("02-06b: a d already consumed (e.g. by the `g d` chord) does not open diagnostics", async () => {
    mockBackend((context) => reportFor(context));
    await renderLoaded();
    const devRow = rowOf("dev");
    act(() => {
      devRow.focus();
    });
    // Same as the capture-phase chord listener: the second key arrives prevented.
    const consume = (e: KeyboardEvent) => {
      e.preventDefault();
    };
    window.addEventListener("keydown", consume, true);
    try {
      fireEvent.keyDown(devRow, { key: "d" });
    } finally {
      window.removeEventListener("keydown", consume, true);
    }
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("restores focus to the row after closing via the button", async () => {
    const user = userEvent.setup();
    mockBackend((context) => reportFor(context));
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Run diagnostics for broken" }));
    await screen.findByTestId("diag-step-tcp");
    await user.click(screen.getByRole("button", { name: "Close diagnostics" }));
    expect(rowOf("broken")).toHaveFocus();
  });

  it("traps Tab and Shift+Tab inside the dialog", async () => {
    const user = userEvent.setup();
    mockBackend((context) => reportFor(context));
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Run diagnostics for dev" }));
    await screen.findByTestId("diag-step-tcp");
    const closeBtn = screen.getByRole("button", { name: "Close diagnostics" });
    expect(closeBtn).toHaveFocus();
    await user.tab();
    expect(closeBtn).toHaveFocus();
    await user.tab({ shift: true });
    expect(closeBtn).toHaveFocus();
  });

  it("keeps global shortcuts from reaching the app while open", async () => {
    const user = userEvent.setup();
    mockBackend((context) => reportFor(context));
    await renderLoaded();
    const globalKey = vi.fn();
    window.addEventListener("keydown", globalKey);
    document.addEventListener("keydown", globalKey);
    try {
      await user.click(screen.getByRole("button", { name: "Run diagnostics for dev" }));
      await screen.findByTestId("diag-step-tcp");
      await user.keyboard("{Control>}k{/Control}{Control>}t{/Control}{Control>}w{/Control}");
      await user.keyboard("{Control>}b{/Control}jk");
      await user.keyboard("{Escape}");
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(globalKey).not.toHaveBeenCalled();

      // Once closed, keys flow to global handlers again.
      fireEvent.keyDown(document.body, { key: "j" });
      expect(globalKey).toHaveBeenCalled();
    } finally {
      window.removeEventListener("keydown", globalKey);
      document.removeEventListener("keydown", globalKey);
    }
  });

  it("Escape is consumed by the dialog (preventDefault)", async () => {
    const user = userEvent.setup();
    mockBackend((context) => reportFor(context));
    await renderLoaded();
    await user.click(screen.getByRole("button", { name: "Run diagnostics for dev" }));
    await screen.findByTestId("diag-step-tcp");
    const closeBtn = screen.getByRole("button", { name: "Close diagnostics" });
    const notCancelled = fireEvent.keyDown(closeBtn, { key: "Escape" });
    expect(notCancelled).toBe(false);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it.each([
    ["Shift+F10", "{Shift>}{F10}{/Shift}"],
    ["ContextMenu", "{ContextMenu}"],
    ["d", "d"],
  ])("%s on a focused context row opens diagnostics for it", async (_label, keys) => {
    const user = userEvent.setup();
    const calls = mockBackend((context) => reportFor(context));
    await renderLoaded();

    const row = rowOf("broken");
    act(() => {
      row.focus();
    });
    await user.keyboard(keys);

    const dialog = await screen.findByRole("dialog", { name: "Connection diagnostics" });
    expect(await within(dialog).findByTestId("diag-step-tcp")).toBeInTheDocument();
    expect(dialog).toHaveTextContent("Connection diagnostics: broken");
    expect(diagCalls(calls)).toEqual([
      { cmd: "run_connection_diagnostics", args: { context: "broken" } },
    ]);
    // Opening diagnostics does not select the context.
    expect(useContextsStore.getState().selectedContext).toBe("dev");
  });

  it("a response arriving after close does not re-open the dialog", async () => {
    const user = userEvent.setup();
    const { calls, release } = gatedBackend(["broken"]);
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Run diagnostics for broken" }));
    expect(screen.getByRole("dialog")).toHaveTextContent("Running diagnostics…");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();

    await release("broken");
    expect(diagCalls(calls)).toHaveLength(1);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByTestId("diag-step-tcp")).toBeNull();
  });

  it("a superseded run resolving last does not replace the newer report", async () => {
    const user = userEvent.setup();
    const { calls, release } = gatedBackend(["dev", "broken"]);
    await renderLoaded();

    act(() => {
      rowOf("dev").focus();
    });
    await user.keyboard("d");
    // Re-open for "broken" while "dev" is still running (right-click works through the modal).
    fireEvent.contextMenu(rowOf("broken"));
    const dialog = screen.getByRole("dialog", { name: "Connection diagnostics" });
    expect(dialog).toHaveTextContent("Connection diagnostics: broken");

    await release("broken");
    expect(await within(dialog).findByTestId("diag-step-tcp")).toBeInTheDocument();
    await release("dev");

    expect(diagCalls(calls).map((c) => c.args)).toEqual([
      { context: "dev" },
      { context: "broken" },
    ]);
    expect(dialog).toHaveTextContent("Connection diagnostics: broken");
    expect(dialog).not.toHaveTextContent("Connection diagnostics: dev");
    expect(within(dialog).queryByRole("status")).toBeNull();

    // Focus returns to the row of the first open, not the dialog's Close button.
    await user.keyboard("{Escape}");
    expect(rowOf("dev")).toHaveFocus();
  });

  it("right-click on a context opens the dialog and runs diagnostics", async () => {
    const calls = mockBackend((context) => reportFor(context));
    await renderLoaded();

    const item = screen.getByText("broken").closest("li");
    expect(item).not.toBeNull();
    if (item) fireEvent.contextMenu(item);

    const dialog = await screen.findByRole("dialog", { name: "Connection diagnostics" });
    expect(await within(dialog).findByTestId("diag-step-tcp")).toBeInTheDocument();
    expect(diagCalls(calls)).toHaveLength(1);
    expect(diagCalls(calls)[0]?.args).toEqual({ context: "broken" });
  });

  it("auto-opens once when a context transitions to error", async () => {
    const user = userEvent.setup();
    const calls = mockBackend((context) => reportFor(context));
    await renderLoaded();
    expect(screen.queryByRole("dialog")).toBeNull();

    setConnectHandler(() => Promise.resolve({ state: "error", message: "connection refused" }));
    await user.click(screen.getByText("broken"));

    const dialog = await screen.findByRole("dialog", { name: "Connection diagnostics" });
    expect(await within(dialog).findByTestId("diag-step-tcp")).toBeInTheDocument();
    expect(dialog).toHaveTextContent("Connection diagnostics: broken");
    expect(diagCalls(calls)).toEqual([
      { cmd: "run_connection_diagnostics", args: { context: "broken" } },
    ]);

    await user.click(within(dialog).getByRole("button", { name: "Close diagnostics" }));
    expect(screen.queryByRole("dialog")).toBeNull();

    // Unrelated status updates while "broken" stays in error must not re-open it.
    setConnectHandler(() => Promise.resolve("connected"));
    await user.click(screen.getByText("dev"));
    expect(screen.getByTestId("status-light-dev")).toHaveAttribute("aria-label", "Connected");
    act(() => {
      useContextsStore.setState((s) => ({
        connectionStatus: {
          ...s.connectionStatus,
          broken: { state: "error", message: "connection refused" },
        },
      }));
    });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(diagCalls(calls)).toHaveLength(1);
  });

  it("shows the backend error as an alert", async () => {
    const user = userEvent.setup();
    const err: IpcError = { kind: "invalidInput", message: "context `broken` not found" };
    mockBackend(() => {
      // Tauri rejects with the serialized `IpcError` object, not an `Error`.
      // eslint-disable-next-line @typescript-eslint/only-throw-error
      throw err;
    });
    await renderLoaded();

    await user.click(screen.getByRole("button", { name: "Run diagnostics for broken" }));

    const dialog = screen.getByRole("dialog", { name: "Connection diagnostics" });
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Cannot run diagnostics: context `broken` not found",
    );
    expect(within(dialog).queryByTestId("diag-rbac-table")).toBeNull();
  });
});
