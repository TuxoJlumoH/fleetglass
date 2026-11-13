import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ContextList_Serialize,
  ImportPreview_Serialize,
  ImportProblem,
  IpcError,
} from "../generated/bindings";
import { useAppInfoStore } from "../stores/appInfo";
import { initialContextsState, setConnectHandler, useContextsStore } from "../stores/contexts";
import { describeImportProblem, resetImporterStore, useImporterStore } from "../stores/importer";
import { IMPORT_TAB, useLayoutStore } from "../stores/layout";
import { podsLayoutState } from "../test/layout";
import { CommandPalette } from "./CommandPalette";
import { ImportView } from "./ImportView";
import { Sidebar } from "./Sidebar";

const PREVIEW: ImportPreview_Serialize = {
  contexts: [
    {
      name: "kind",
      cluster: "kind-c",
      server: "https://127.0.0.1:7443",
      auth: { type: "clientCert" },
      exists: false,
      problem: null,
    },
    {
      name: "eks",
      cluster: "eks-c",
      server: "https://eks.example.test",
      auth: { type: "exec", command: "aws" },
      exists: false,
      problem: { type: "execPlugin", command: "aws" },
    },
    {
      name: "old",
      cluster: "old-c",
      server: "https://old.example.test",
      auth: { type: "token" },
      exists: true,
      problem: null,
    },
  ],
  warnings: ["Context \"kind\" skips TLS verification"],
};

const EMPTY_CONTEXTS: ContextList_Serialize = { contexts: [], current: null, warnings: [] };

type Handler = (args: Record<string, unknown>) => unknown;

/** Pretends to be inside Tauri; `handlers` answer commands, the rest return defaults. */
function mockBackend(handlers: Record<string, Handler> = {}) {
  (globalThis as { isTauri?: boolean }).isTauri = true;
  mockWindows("main");
  const calls: { cmd: string; args: Record<string, unknown> }[] = [];
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    calls.push({ cmd, args: a });
    const h = handlers[cmd];
    if (h) return h(a);
    if (cmd === "list_contexts") return EMPTY_CONTEXTS;
    if (cmd === "list_imported_contexts") return [];
    if (cmd === "get_cluster_groups") return [];
    if (cmd === "kind_counts") return [];
    return null;
  });
  return calls;
}

function names(calls: { cmd: string }[]): string[] {
  return calls.map((c) => c.cmd);
}

describe("describeImportProblem", () => {
  const cases: [ImportProblem, string][] = [
    [
      { type: "execPlugin", command: "aws" },
      "uses the exec credential plugin `aws`, which cannot run on Android; create a token or client-certificate context for the phone",
    ],
    [
      { type: "execPlugin", command: null },
      "uses an exec credential plugin, which cannot run on Android; create a token or client-certificate context for the phone",
    ],
    [
      { type: "authProvider", name: "gcp" },
      "uses the `gcp` auth-provider, which cannot run on Android (only `oidc` with an id-token is supported); create a token or client-certificate context for the phone",
    ],
    [
      { type: "oidcWithoutIdToken" },
      "uses the `oidc` auth-provider without an id-token; on Android only an id-token already in the kubeconfig is used and it is not refreshed automatically (log in on a computer, import the kubeconfig with the current id-token and re-import it when it expires), or create a token or client-certificate context for the phone",
    ],
    [
      { type: "incompleteClientCertificate" },
      "has a client certificate without its key, or a key without its certificate; both client-certificate-data and client-key-data are needed",
    ],
    [
      { type: "fileReference", field: "client-key" },
      "refers to a file in `client-key`, which only exists on the machine the kubeconfig came from; export it with the data embedded (`kubectl config view --raw --flatten --minify`) and import that",
    ],
    [
      { type: "noCertificateAuthority" },
      "has no certificate-authority-data for its cluster, so the server certificate cannot be verified on Android; add the cluster's CA certificate (or, for test clusters only, insecure-skip-tls-verify: true)",
    ],
    [{ type: "missingCluster" }, "refers to a cluster that is not defined in the file"],
    [{ type: "missingUser" }, "refers to a user that is not defined in the file"],
    [{ type: "noServer" }, "has no server URL for its cluster"],
  ];

  it.each(cases)("describes %j like the Rust describe()", (problem, text) => {
    expect(describeImportProblem(problem)).toBe(text);
  });
});

describe("ImportView", () => {
  beforeEach(() => {
    resetImporterStore();
    useContextsStore.setState(initialContextsState);
    useLayoutStore.setState(podsLayoutState);
    setConnectHandler();
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
    vi.restoreAllMocks();
  });

  async function renderView() {
    render(<ImportView />);
    await waitFor(() => {
      expect(useImporterStore.getState().importedStatus).toBe("ready");
    });
  }

  it("previews a picked file, pre-selects importable new contexts and commits them", async () => {
    const user = userEvent.setup();
    const calls = mockBackend({
      import_pick_file: () => PREVIEW,
      import_commit: (a) => a.names,
      list_imported_contexts: () =>
        useImporterStore.getState().message === null ? [] : ["kind"],
    });
    await renderView();

    await user.click(screen.getByRole("button", { name: "Choose kubeconfig file" }));
    const kind = await screen.findByRole("checkbox", { name: /kind/ });
    expect(kind).toBeChecked();
    // Existing contexts are not pre-selected; a problem disables the box.
    const old = screen.getByRole("checkbox", { name: /old/ });
    expect(old).not.toBeChecked();
    expect(within(screen.getByTestId("candidate-old")).getByText("already imported")).toBeVisible();
    expect(screen.getByText(PREVIEW.warnings[0] ?? "")).toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: "Replace existing" })).not.toBeInTheDocument();

    // Selecting an existing context offers "Replace existing".
    await user.click(old);
    await user.click(screen.getByRole("checkbox", { name: "Replace existing" }));
    await user.click(screen.getByRole("button", { name: "Import 2 contexts" }));

    await screen.findByText("Imported: kind, old");
    const commit = calls.find((c) => c.cmd === "import_commit");
    expect(commit?.args).toEqual({ names: ["kind", "old"], replaceExisting: true });
    // The contexts list is reloaded after the import.
    expect(names(calls).slice(names(calls).indexOf("import_commit"))).toContain("list_contexts");
    expect(useImporterStore.getState().preview).toBeNull();
  });

  it("does not send replaceExisting when no selected context exists", async () => {
    const user = userEvent.setup();
    const calls = mockBackend({
      import_pick_file: () => PREVIEW,
      import_commit: (a) => a.names,
    });
    await renderView();
    await user.click(screen.getByRole("button", { name: "Choose kubeconfig file" }));
    await user.click(await screen.findByRole("button", { name: "Import 1 context" }));
    await screen.findByText("Imported: kind");
    expect(calls.find((c) => c.cmd === "import_commit")?.args).toEqual({
      names: ["kind"],
      replaceExisting: false,
    });
  });

  it("disables an exec-plugin candidate and shows why", async () => {
    const user = userEvent.setup();
    mockBackend({ import_pick_file: () => PREVIEW });
    await renderView();
    await user.click(screen.getByRole("button", { name: "Choose kubeconfig file" }));

    const eks = await screen.findByRole("checkbox", { name: /eks/ });
    expect(eks).toBeDisabled();
    expect(eks).not.toBeChecked();
    expect(eks).toHaveAccessibleDescription(
      /Context “eks” uses the exec credential plugin `aws`, which cannot run on Android/,
    );
  });

  it("keeps the screen unchanged when the picker is cancelled", async () => {
    const user = userEvent.setup();
    const calls = mockBackend({ import_pick_file: () => null });
    await renderView();
    await user.click(screen.getByRole("button", { name: "Choose kubeconfig file" }));
    await waitFor(() => {
      expect(names(calls)).toContain("import_pick_file");
    });
    await waitFor(() => {
      expect(useImporterStore.getState().busy).toBe(false);
    });
    expect(useImporterStore.getState().preview).toBeNull();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Import \d/ })).not.toBeInTheDocument();
  });

  it("previews pasted text and cancels the preview", async () => {
    const user = userEvent.setup();
    const calls = mockBackend({ import_preview_text: () => PREVIEW });
    await renderView();

    expect(screen.getByRole("button", { name: "Preview" })).toBeDisabled();
    await user.type(screen.getByLabelText("Paste kubeconfig"), "apiVersion: v1");
    await user.click(screen.getByRole("button", { name: "Preview" }));
    expect(await screen.findByRole("checkbox", { name: /kind/ })).toBeChecked();
    expect(calls.find((c) => c.cmd === "import_preview_text")?.args).toEqual({
      yaml: "apiVersion: v1",
    });

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => {
      expect(screen.queryByRole("checkbox", { name: /kind/ })).not.toBeInTheDocument();
    });
    expect(names(calls)).toContain("import_cancel");
  });

  it("shows backend errors inline", async () => {
    const user = userEvent.setup();
    const err: IpcError = { kind: "invalidInput", message: "only available in the Android app" };
    mockBackend({
      import_pick_file: () => {
        // Tauri rejects with the serialized `IpcError` object, not an `Error`.
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw err;
      },
    });
    await renderView();
    await user.click(screen.getByRole("button", { name: "Choose kubeconfig file" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("only available in the Android app");
  });

  it("imports a token context, clears the token and never logs it", async () => {
    const user = userEvent.setup();
    const spies = (["log", "info", "warn", "error", "debug"] as const).map((m) =>
      vi.spyOn(console, m).mockImplementation(() => undefined),
    );
    const calls = mockBackend({ import_token: (a) => [a.name] });
    await renderView();

    const submit = screen.getByRole("button", { name: "Import token context" });
    expect(submit).toBeDisabled();
    await user.type(screen.getByLabelText("Context name"), "phone");
    await user.type(screen.getByLabelText("API server URL"), "https://k8s.example.test");
    const tokenInput = screen.getByLabelText("Token");
    expect(tokenInput).toHaveAttribute("type", "password");
    expect(tokenInput).toHaveAttribute("autocomplete", "off");
    await user.type(tokenInput, "s3cr3t-token");
    await user.click(screen.getByRole("checkbox", { name: "Skip TLS verification (test clusters only)" }));
    await user.click(submit);

    await screen.findByText("Imported: phone");
    expect(calls.find((c) => c.cmd === "import_token")?.args).toEqual({
      name: "phone",
      server: "https://k8s.example.test",
      token: "s3cr3t-token",
      caPem: null,
      insecureSkipTlsVerify: true,
    });
    expect(screen.getByLabelText("Token")).toHaveValue("");
    expect(useImporterStore.getState().token.token).toBe("");
    expect(names(calls).slice(names(calls).indexOf("import_token"))).toContain("list_contexts");
    for (const spy of spies) {
      for (const args of spy.mock.calls) {
        expect(JSON.stringify(args)).not.toContain("s3cr3t-token");
      }
    }
  });

  it("removes an imported context after an inline confirmation", async () => {
    const user = userEvent.setup();
    let imported = ["kind", "old"];
    const calls = mockBackend({
      list_imported_contexts: () => imported,
      remove_imported_context: (a) => {
        imported = imported.filter((n) => n !== a.name);
        return true;
      },
    });
    await renderView();

    await user.click(await screen.findByRole("button", { name: "Remove old" }));
    // "No" gets focus and keeps the context.
    expect(screen.getByRole("button", { name: "No, keep old" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "No, keep old" }));
    expect(names(calls)).not.toContain("remove_imported_context");

    await user.click(screen.getByRole("button", { name: "Remove old" }));
    await user.click(screen.getByRole("button", { name: "Yes, remove old" }));
    await screen.findByText("Removed: old");
    expect(calls.find((c) => c.cmd === "remove_imported_context")?.args).toEqual({ name: "old" });
    expect(screen.queryByRole("button", { name: "Remove old" })).not.toBeInTheDocument();
    expect(names(calls).slice(names(calls).indexOf("remove_imported_context"))).toContain(
      "list_contexts",
    );
  });

  it("deletes an unreadable store after confirmation and reloads the lists", async () => {
    const user = userEvent.setup();
    const broken: IpcError = {
      kind: "internal",
      message:
        "open failed: the credential key is missing (app data restored or key deleted); re-import the contexts",
    };
    let reset = false;
    const calls = mockBackend({
      list_imported_contexts: () => {
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        if (!reset) throw broken;
        return [];
      },
      reset_imported_contexts: () => {
        reset = true;
        return null;
      },
    });
    render(<ImportView />);

    expect(await screen.findByRole("alert")).toHaveTextContent("re-import the contexts");
    await user.click(screen.getByRole("button", { name: "Delete stored contexts" }));
    expect(screen.getByRole("button", { name: "No, keep stored contexts" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "No, keep stored contexts" }));
    expect(names(calls)).not.toContain("reset_imported_contexts");

    await user.click(screen.getByRole("button", { name: "Delete stored contexts" }));
    await user.click(screen.getByRole("button", { name: "Yes, delete stored contexts" }));
    await screen.findByText("Stored contexts deleted; import them again.");
    expect(await screen.findByText("No imported contexts yet.")).toBeInTheDocument();
    const after = names(calls).slice(names(calls).indexOf("reset_imported_contexts"));
    expect(after).toContain("list_imported_contexts");
    expect(after).toContain("list_contexts");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete stored contexts" })).not.toBeInTheDocument();
  });

  it("offers the reset when an import fails with a credential error", async () => {
    const user = userEvent.setup();
    const calls = mockBackend({
      import_token: () => {
        const err: IpcError = {
          kind: "internal",
          message: "open failed: authentication failed (the credential key changed); re-import the contexts",
        };
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw err;
      },
    });
    await renderView();
    expect(screen.queryByRole("button", { name: "Delete stored contexts" })).not.toBeInTheDocument();

    await user.type(screen.getByLabelText("Context name"), "phone");
    await user.type(screen.getByLabelText("API server URL"), "https://k8s.example.test");
    await user.type(screen.getByLabelText("Token"), "t");
    await user.click(screen.getByRole("button", { name: "Import token context" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("credential key changed");

    await user.click(screen.getByRole("button", { name: "Delete stored contexts" }));
    await user.click(screen.getByRole("button", { name: "Yes, delete stored contexts" }));
    await screen.findByText("Stored contexts deleted; import them again.");
    expect(names(calls)).toContain("reset_imported_contexts");
  });

  it("does not offer the reset for ordinary import errors", async () => {
    const user = userEvent.setup();
    mockBackend({
      import_token: () => {
        const err: IpcError = { kind: "invalidInput", message: "bad server URL" };
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw err;
      },
    });
    await renderView();
    await user.type(screen.getByLabelText("Context name"), "phone");
    await user.type(screen.getByLabelText("API server URL"), "https://k8s.example.test");
    await user.type(screen.getByLabelText("Token"), "t");
    await user.click(screen.getByRole("button", { name: "Import token context" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("bad server URL");
    expect(screen.queryByRole("button", { name: "Delete stored contexts" })).not.toBeInTheDocument();
  });
});

describe("Import entry points", () => {
  beforeEach(() => {
    resetImporterStore();
    useContextsStore.setState(initialContextsState);
    useLayoutStore.setState(podsLayoutState);
    setConnectHandler();
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
    useAppInfoStore.setState({ version: null, platform: null });
  });

  async function renderSidebar() {
    mockBackend();
    render(<Sidebar />);
    await act(() => useContextsStore.getState().loadContexts());
  }

  it("is hidden on desktop", async () => {
    useAppInfoStore.setState({ platform: "linux" });
    await renderSidebar();
    expect(screen.getByText("No contexts in kubeconfig")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Import a kubeconfig" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Import contexts" })).not.toBeInTheDocument();

    render(<CommandPalette />);
    act(() => {
      useLayoutStore.getState().openPalette();
    });
    expect(screen.queryByRole("option", { name: "Import contexts" })).not.toBeInTheDocument();
  });

  it("shows a sidebar item, an empty-state call to action and a palette entry on Android", async () => {
    const user = userEvent.setup();
    useAppInfoStore.setState({ platform: "android" });
    await renderSidebar();

    await user.click(screen.getByRole("button", { name: "Import a kubeconfig" }));
    expect(useLayoutStore.getState().activeTabId).toBe(IMPORT_TAB.id);

    useLayoutStore.setState(podsLayoutState);
    await user.click(screen.getByRole("button", { name: "Import contexts" }));
    expect(useLayoutStore.getState().activeTabId).toBe(IMPORT_TAB.id);
    expect(useLayoutStore.getState().tabs.filter((t) => t.id === IMPORT_TAB.id)).toHaveLength(1);

    useLayoutStore.setState(podsLayoutState);
    render(<CommandPalette />);
    act(() => {
      useLayoutStore.getState().openPalette();
    });
    await user.click(await screen.findByRole("option", { name: "Import contexts" }));
    expect(useLayoutStore.getState().activeTabId).toBe(IMPORT_TAB.id);
  });
});
