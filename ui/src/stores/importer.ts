import { create } from "zustand";
import {
  commands,
  type ErrorKind,
  type ImportCandidate_Serialize,
  type ImportPreview_Serialize,
  type ImportProblem,
} from "../generated/bindings";
import { useContextsStore } from "./contexts";

/** Kubeconfig import on Android (03-02c). The backend keeps the previewed file until commit. */

export type ImportCandidate = ImportCandidate_Serialize;
export type ImportPreview = ImportPreview_Serialize;

export interface ImportError {
  kind: ErrorKind | null;
  message: string;
}

const ALTERNATIVE = "create a token or client-certificate context for the phone";

/**
 * User-facing explanation of why a context cannot be imported, phrased to follow
 * "Context "x" ...". Mirrors `ImportProblem::describe()` in crates/core/src/api/import.rs.
 */
export function describeImportProblem(p: ImportProblem): string {
  switch (p.type) {
    case "execPlugin":
      return p.command
        ? `uses the exec credential plugin \`${p.command}\`, which cannot run on Android; ${ALTERNATIVE}`
        : `uses an exec credential plugin, which cannot run on Android; ${ALTERNATIVE}`;
    case "authProvider":
      return `uses the \`${p.name}\` auth-provider, which cannot run on Android (only \`oidc\` with an id-token is supported); ${ALTERNATIVE}`;
    case "oidcWithoutIdToken":
      return `uses the \`oidc\` auth-provider without an id-token; on Android only an id-token already in the kubeconfig is used and it is not refreshed automatically (log in on a computer, import the kubeconfig with the current id-token and re-import it when it expires), or ${ALTERNATIVE}`;
    case "incompleteClientCertificate":
      return "has a client certificate without its key, or a key without its certificate; both client-certificate-data and client-key-data are needed";
    case "fileReference":
      return `refers to a file in \`${p.field}\`, which only exists on the machine the kubeconfig came from; export it with the data embedded (\`kubectl config view --raw --flatten --minify\`) and import that`;
    case "noCertificateAuthority":
      return "has no certificate-authority-data for its cluster, so the server certificate cannot be verified on Android; add the cluster's CA certificate (or, for test clusters only, insecure-skip-tls-verify: true)";
    case "missingCluster":
      return "refers to a cluster that is not defined in the file";
    case "missingUser":
      return "refers to a user that is not defined in the file";
    case "noServer":
      return "has no server URL for its cluster";
  }
}

/** Same normalisation as the contexts store: `IpcError` results, thrown errors, plain strings. */
function toImportError(raw: unknown): ImportError {
  if (raw instanceof Error) return { kind: null, message: raw.message };
  if (typeof raw === "object" && raw !== null && "message" in raw) {
    const { kind, message } = raw as { kind?: unknown; message: unknown };
    return {
      kind: typeof kind === "string" ? (kind as ErrorKind) : null,
      message: String(message),
    };
  }
  return { kind: null, message: String(raw) };
}

type Result<T> = { status: "ok"; data: T } | { status: "error"; error: unknown };

/** Runs a command; errors (returned or thrown) land in `error`, `null` is returned. */
async function run<T>(
  call: () => Promise<Result<T>>,
  setError: (e: ImportError) => void,
): Promise<{ data: T } | null> {
  try {
    const res = await call();
    if (res.status === "ok") return { data: res.data };
    setError(toImportError(res.error));
  } catch (err: unknown) {
    setError(toImportError(err));
  }
  return null;
}

export interface TokenForm {
  name: string;
  server: string;
  token: string;
  caPem: string;
  insecureSkipTlsVerify: boolean;
}

const emptyTokenForm: TokenForm = {
  name: "",
  server: "",
  token: "",
  caPem: "",
  insecureSkipTlsVerify: false,
};

interface ImporterState {
  /** A command is in flight; buttons are disabled meanwhile. */
  busy: boolean;
  preview: ImportPreview | null;
  /** Names of the candidates ticked for import. */
  selected: string[];
  replaceExisting: boolean;
  pasted: string;
  token: TokenForm;
  imported: string[] | null;
  importedStatus: "idle" | "loading" | "ready" | "error";
  /** Why the imported list could not be loaded (shown in the "Imported contexts" section). */
  importedError: ImportError | null;
  /** Success line, e.g. "Imported: a, b". */
  message: string | null;
  error: ImportError | null;
  /**
   * The sealed store cannot be read (e.g. the Android Keystore key is gone), so imports keep
   * failing until it is deleted with `resetStore`.
   */
  storeBroken: boolean;
  pickFile: () => Promise<void>;
  setPasted: (text: string) => void;
  previewPasted: () => Promise<void>;
  toggleSelected: (name: string) => void;
  setReplaceExisting: (replace: boolean) => void;
  commit: () => Promise<void>;
  cancel: () => Promise<void>;
  setToken: (patch: Partial<TokenForm>) => void;
  submitToken: () => Promise<void>;
  loadImported: () => Promise<void>;
  remove: (name: string) => Promise<void>;
  /** Deletes all stored imported contexts (the way out when the store cannot be read). */
  resetStore: () => Promise<void>;
}

export const initialImporterState = {
  busy: false,
  preview: null as ImportPreview | null,
  selected: [] as string[],
  replaceExisting: false,
  pasted: "",
  token: emptyTokenForm,
  imported: null as string[] | null,
  importedStatus: "idle" as ImporterState["importedStatus"],
  importedError: null as ImportError | null,
  message: null as string | null,
  error: null as ImportError | null,
  storeBroken: false,
};

/** An import failed because the stored credentials cannot be decrypted (vault error). */
function isStoreFailure(error: ImportError): boolean {
  return error.kind === "internal" && error.message.includes("credential");
}

/** Importable candidates that are not imported yet are ticked by default. */
function defaultSelection(preview: ImportPreview): string[] {
  return preview.contexts.filter((c) => c.problem === null && !c.exists).map((c) => c.name);
}

export const useImporterStore = create<ImporterState>()((set, get) => {
  const setError = (error: ImportError) => {
    set({ error, message: null });
  };

  /** For commands that write the store: a vault failure means the store is unreadable. */
  const setWriteError = (error: ImportError) => {
    set({ error, message: null, ...(isStoreFailure(error) ? { storeBroken: true } : {}) });
  };

  const showPreview = (preview: ImportPreview) => {
    set({
      preview,
      selected: defaultSelection(preview),
      replaceExisting: false,
      error: null,
      message: null,
    });
  };

  /** After a change: refresh the imported list and the app's contexts. */
  const afterImportChange = async (message: string | null) => {
    set({ message, error: null });
    await Promise.all([get().loadImported(), useContextsStore.getState().loadContexts()]);
  };

  return {
    ...initialImporterState,
    pickFile: async () => {
      set({ busy: true, error: null, message: null });
      const res = await run(() => commands.importPickFile(), setWriteError);
      set({ busy: false });
      // `null` = the picker was cancelled: keep whatever was shown before.
      if (res?.data) showPreview(res.data);
    },
    setPasted: (pasted) => {
      set({ pasted });
    },
    previewPasted: async () => {
      const text = get().pasted;
      if (text.trim() === "") return;
      set({ busy: true, error: null, message: null });
      const res = await run(() => commands.importPreviewText(text), setWriteError);
      set({ busy: false });
      if (res) showPreview(res.data);
    },
    toggleSelected: (name) => {
      const { preview, selected } = get();
      const candidate = preview?.contexts.find((c) => c.name === name);
      if (!candidate || candidate.problem !== null) return;
      set({
        selected: selected.includes(name)
          ? selected.filter((n) => n !== name)
          : [...selected, name],
      });
    },
    setReplaceExisting: (replaceExisting) => {
      set({ replaceExisting });
    },
    commit: async () => {
      const { preview, selected, replaceExisting } = get();
      if (!preview || selected.length === 0) return;
      // Keep file order, whatever order the boxes were ticked in.
      const names = preview.contexts.map((c) => c.name).filter((n) => selected.includes(n));
      const anyExisting = preview.contexts.some((c) => c.exists && names.includes(c.name));
      set({ busy: true, error: null, message: null });
      const res = await run(
        () => commands.importCommit(names, anyExisting && replaceExisting),
        setWriteError,
      );
      if (res) {
        set({ preview: null, selected: [], replaceExisting: false, pasted: "" });
        await afterImportChange(`Imported: ${res.data.join(", ")}`);
      }
      set({ busy: false });
    },
    cancel: async () => {
      set({ busy: true });
      const res = await run(() => commands.importCancel(), setError);
      set({ busy: false });
      if (res) set({ preview: null, selected: [], replaceExisting: false, error: null });
    },
    setToken: (patch) => {
      set((s) => ({ token: { ...s.token, ...patch } }));
    },
    submitToken: async () => {
      const { name, server, token, caPem, insecureSkipTlsVerify } = get().token;
      set({ busy: true, error: null, message: null });
      const ca = caPem.trim() === "" ? null : caPem;
      const res = await run(
        () => commands.importToken(name.trim(), server.trim(), token, ca, insecureSkipTlsVerify),
        setWriteError,
      );
      if (res) {
        set({ token: emptyTokenForm });
        await afterImportChange(`Imported: ${res.data.join(", ")}`);
      }
      set({ busy: false });
    },
    loadImported: async () => {
      set({ importedStatus: "loading" });
      let error: ImportError;
      try {
        const res = await commands.listImportedContexts();
        if (res.status === "ok") {
          set({ imported: res.data, importedStatus: "ready", importedError: null });
          return;
        }
        error = toImportError(res.error);
      } catch (err: unknown) {
        error = toImportError(err);
      }
      // Listing only fails when the store cannot be read (desktop returns an empty list).
      set({ imported: null, importedStatus: "error", importedError: error, storeBroken: true });
    },
    remove: async (name) => {
      set({ busy: true, error: null, message: null });
      const res = await run(() => commands.removeImportedContext(name), setWriteError);
      if (res) {
        await afterImportChange(res.data ? `Removed: ${name}` : `${name} was not imported`);
      }
      set({ busy: false });
    },
    resetStore: async () => {
      set({ busy: true, error: null, message: null });
      const res = await run(() => commands.resetImportedContexts(), setError);
      if (res) {
        set({ storeBroken: false, importedError: null });
        await afterImportChange("Stored contexts deleted; import them again.");
      }
      set({ busy: false });
    },
  };
});

/** Test helper: back to the initial state. */
export function resetImporterStore(): void {
  useImporterStore.setState({ ...initialImporterState });
}
