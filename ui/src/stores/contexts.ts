import { isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import {
  commands,
  type ClusterGroup,
  type ContextInfo_Serialize,
  type ErrorKind,
  type TeamSettingsSummary,
} from "../generated/bindings";

export type { ClusterGroup, TeamSettingsSummary };

/** A context as sent by the backend (`list_contexts`). */
export type Context = ContextInfo_Serialize;

export type ContextsStatus = "idle" | "loading" | "ready" | "error";

export type ConnectionStatus =
  | { state: "disconnected" }
  | { state: "connecting" }
  | { state: "connected" }
  | { state: "degraded"; message: string }
  | { state: "error"; message: string };

export interface ContextSettings {
  colour: string | null;
  tags: string[];
  readOnly: boolean;
  protected: boolean;
  pinned: boolean;
  unlockedUntil?: number | null;
}

export const defaultContextSettings: ContextSettings = {
  colour: null,
  tags: [],
  readOnly: false,
  protected: false,
  pinned: false,
  unlockedUntil: null,
};

export type ConnectHandlerResult = ConnectionStatus | "connected";
export type ConnectHandler = (name: string) => Promise<ConnectHandlerResult>;

/**
 * Optional connection probe run on selection. None is installed in production: the real
 * connection state comes from the watch stores' access states (`usePodsStore.access`), so
 * without a handler `selectContext` makes no claim about the connection.
 */
let connectHandler: ConnectHandler | null = null;

export function setConnectHandler(handler?: ConnectHandler): void {
  connectHandler = handler ?? null;
}

/** Above this many contexts, settings are fetched only for the selected / queried ones. */
export const ENSURE_SETTINGS_ALL_LIMIT = 200;

/** Contexts whose `get_context_settings` call is in flight (dedupes `ensureSettings`). */
const settingsInFlight = new Set<string>();

export interface ContextsError {
  /** `null` when the failure did not come from the backend as an `IpcError`. */
  kind: ErrorKind | null;
  message: string;
}

/** Backend calls used for cluster groups; injectable for tests via `setClusterGroupClient`. */
export type ClusterGroupClient = Pick<
  typeof commands,
  "getClusterGroups" | "setClusterGroup" | "deleteClusterGroup"
> & {
  reloadTeamSettings?: (
    path: string,
  ) => ReturnType<typeof commands.reloadTeamSettings>;
};

const defaultClusterGroupClient: ClusterGroupClient = {
  getClusterGroups: () => commands.getClusterGroups(),
  setClusterGroup: (group) => commands.setClusterGroup(group),
  deleteClusterGroup: (name) => commands.deleteClusterGroup(name),
  reloadTeamSettings: (path) => commands.reloadTeamSettings(path),
};
let clusterGroupClient: ClusterGroupClient = defaultClusterGroupClient;

export function setClusterGroupClient(client?: Partial<ClusterGroupClient>): void {
  clusterGroupClient = client
    ? {
        ...defaultClusterGroupClient,
        ...client,
      }
    : defaultClusterGroupClient;
}

/** The generated commands need the Tauri runtime; injected clients do not. */
function clusterGroupBackendAvailable(): boolean {
  return clusterGroupClient !== defaultClusterGroupClient || isTauri();
}

/**
 * The last context and cluster group the user picked, restored on the next start (03-03b).
 * Imported contexts (Android) have no kubeconfig current-context, so without this the app
 * opened on "No context" every time.
 */
export const LAST_SELECTION_STORAGE_KEY = "fleetglass.lastSelection";

export interface LastSelection {
  context: string | null;
  /** Cluster group name ("All Clusters" included); null when no group was active. */
  group: string | null;
  /**
   * The kubeconfig current-context when the selection was saved. If the kubeconfig now names
   * another one, the user switched outside the app (`kubectl config use-context`) and that wins.
   */
  kubeCurrent?: string | null;
}

/** Parses the stored selection; anything malformed yields null (nothing to restore). */
export function parseLastSelection(raw: string | null): LastSelection | null {
  if (raw === null) return null;
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const { context, group, kubeCurrent } = value as {
    context?: unknown;
    group?: unknown;
    kubeCurrent?: unknown;
  };
  const name = (v: unknown) => (typeof v === "string" && v.length > 0 ? v : null);
  const parsed = { context: name(context), group: name(group), kubeCurrent: name(kubeCurrent) };
  return parsed.context === null && parsed.group === null ? null : parsed;
}

/**
 * Which stored selection applies at start-up, given the contexts that exist and the kubeconfig
 * current-context. The last selection wins on every platform while its context still exists,
 * unless the kubeconfig current-context changed since it was saved (an explicit switch outside
 * the app, e.g. `kubectl config use-context`), which then wins. A record saved while there was
 * no current-context (always the case on Android, where imported contexts have none) keeps
 * winning when a current-context appears later; that is deliberate, the app choice is newer.
 */
export function restoredSelection(
  stored: LastSelection | null,
  names: readonly string[],
  current: string | null,
): { context: string | null; group: string | null } {
  if (stored === null) return { context: current, group: null };
  const switchedOutside =
    current !== null && stored.kubeCurrent != null && stored.kubeCurrent !== current;
  if (switchedOutside) return { context: current, group: null };
  const context = stored.context !== null && names.includes(stored.context) ? stored.context : current;
  return { context, group: stored.group };
}

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    // Access can throw (storage disabled); the selection then lives for the session only.
    return null;
  }
}

function loadLastSelection(): LastSelection | null {
  try {
    return parseLastSelection(storage()?.getItem(LAST_SELECTION_STORAGE_KEY) ?? null);
  } catch {
    return null;
  }
}

function saveLastSelection(selection: Omit<LastSelection, "kubeCurrent">): void {
  const record: LastSelection = { ...selection, kubeCurrent: useContextsStore.getState().current };
  try {
    storage()?.setItem(LAST_SELECTION_STORAGE_KEY, JSON.stringify(record));
  } catch {
    // Quota or disabled storage: keep the in-memory selection.
  }
}

/** Group the user picked last (null when the selection is not a named group). */
let activeGroupName: string | null = null;
/** Stored group waiting for the contexts and cluster groups to load before it is re-applied. */
let pendingGroupRestore: string | null = null;

/** Test helper: forgets the in-memory group bookkeeping (storage is left alone). */
export function resetLastSelectionState(): void {
  activeGroupName = null;
  pendingGroupRestore = null;
}

export type ClusterGroupsStatus = "idle" | "loading" | "ready" | "error";
export type TeamSettingsStatus = "idle" | "loading" | "ready" | "error";

interface ContextsState {
  status: ContextsStatus;
  /** False outside the desktop app (plain browser, tests without the Tauri global). */
  available: boolean;
  contexts: Context[];
  current: string | null;
  selectedContext: string | null;
  connectionStatus: Record<string, ConnectionStatus>;
  settingsByContext: Record<string, ContextSettings>;
  warnings: string[];
  error: ContextsError | null;
  clusterGroups: ClusterGroup[];
  clusterGroupsStatus: ClusterGroupsStatus;
  /** Last failure of a cluster group load/save/delete; cleared by the next success. */
  clusterGroupsError: ContextsError | null;
  activeClusterSelection: string[];
  toggleClusterSelection: (name: string) => void;
  teamSettingsSummary: TeamSettingsSummary | null;
  teamSettingsStatus: TeamSettingsStatus;
  teamSettingsError: ContextsError | null;
  loadContexts: () => Promise<void>;
  selectContext: (name: string) => Promise<void>;
  setContextSettings: (name: string, patch: Partial<ContextSettings>) => void;
  unlockContext: (context: string, confirmedName?: string | null) => Promise<ContextSettings>;
  relockContext: (context: string) => Promise<ContextSettings>;
  /**
   * Loads the persisted settings (colour, read-only, ...) of every name not cached yet: one
   * small struct per context from local settings, no cluster I/O. In-flight calls are deduped;
   * failures leave the context unknown.
   */
  ensureSettings: (names: readonly string[]) => Promise<void>;
  setConnectionStatus: (name: string, status: ConnectionStatus) => void;
  setActiveClusterSelection: (clusters: string[]) => void;
  selectClusterGroup: (groupName: string) => void;
  setClusterGroups: (groups: ClusterGroup[]) => void;
  loadClusterGroups: () => Promise<void>;
  /** Adds or replaces a group by name. Resolves `false` (error in state) on failure. */
  saveClusterGroup: (group: ClusterGroup) => Promise<boolean>;
  /** Resolves `false` (error in state) on failure. */
  deleteClusterGroup: (name: string) => Promise<boolean>;
  reloadTeamSettings: (path: string) => Promise<boolean>;
}

export const initialContextsState = {
  status: "idle" as ContextsStatus,
  available: true,
  contexts: [] as Context[],
  current: null as string | null,
  selectedContext: null as string | null,
  connectionStatus: {} as Record<string, ConnectionStatus>,
  settingsByContext: {} as Record<string, ContextSettings>,
  warnings: [] as string[],
  error: null as ContextsError | null,
  clusterGroups: [] as ClusterGroup[],
  clusterGroupsStatus: "idle" as ClusterGroupsStatus,
  clusterGroupsError: null as ContextsError | null,
  activeClusterSelection: [] as string[],
  teamSettingsSummary: null as TeamSettingsSummary | null,
  teamSettingsStatus: "idle" as TeamSettingsStatus,
  teamSettingsError: null as ContextsError | null,
};

/**
 * The generated wrapper types every non-`Error` rejection as `IpcError`, but Tauri
 * itself rejects with plain strings (e.g. "Command list_contexts not allowed by ACL").
 */
function toContextsError(raw: unknown): ContextsError {
  if (typeof raw === "object" && raw !== null && "message" in raw) {
    const { kind, message } = raw as { kind?: unknown; message: unknown };
    return {
      kind: typeof kind === "string" ? (kind as ErrorKind) : null,
      message: String(message),
    };
  }
  return { kind: null, message: String(raw) };
}

/** Normalises both `Result` errors and thrown transport errors. */
function toCaughtError(err: unknown): ContextsError {
  return toContextsError(err instanceof Error ? { message: err.message } : err);
}

const notInDesktopApp: ContextsError = { kind: null, message: "Not running in the Fleetglass app" };

function byName(a: ClusterGroup, b: ClusterGroup): number {
  return a.name.localeCompare(b.name);
}

/**
 * Clusters (kubeconfig context names) the resource tables query: the active cluster
 * group / multi-selection when one is set, else the selected context, else none.
 * The backend only watches clusters named in the query (lazy connect).
 */
export function clustersForQuery(state: {
  activeClusterSelection: readonly string[];
  selectedContext: string | null;
}): string[] {
  if (state.activeClusterSelection.length > 0) {
    return [...new Set(state.activeClusterSelection)];
  }
  return state.selectedContext === null ? [] : [state.selectedContext];
}

export const useContextsStore = create<ContextsState>()((set, get) => ({
  ...initialContextsState,
  loadContexts: async () => {
    if (!isTauri()) {
      set({
        available: false,
        status: "idle",
        contexts: [],
        current: null,
        selectedContext: null,
        warnings: [],
        error: null,
      });
      return;
    }
    set({ status: "loading", available: true, error: null });
    try {
      const res = await commands.listContexts();
      if (res.status === "ok") {
        const names = res.data.contexts.map((c) => c.name);
        // The context picked last wins while it still exists; else the kubeconfig's current.
        const restored = restoredSelection(loadLastSelection(), names, res.data.current);
        const selected = restored.context;
        // No connection status is seeded: nothing is connected yet (lazy connect), and an
        // unknown context shows a neutral dot rather than claiming "Disconnected".
        set({
          status: "ready",
          contexts: res.data.contexts,
          current: res.data.current,
          selectedContext: selected,
          warnings: res.data.warnings,
          error: null,
        });
        if (restored.group !== null && get().activeClusterSelection.length === 0) {
          pendingGroupRestore = restored.group;
          restorePendingGroup();
        }
        void get().ensureSettings(
          names.length <= ENSURE_SETTINGS_ALL_LIMIT ? names : selected === null ? [] : [selected],
        );
      } else {
        set({
          status: "error",
          contexts: [],
          current: null,
          selectedContext: null,
          warnings: [],
          error: toContextsError(res.error),
        });
      }
    } catch (err: unknown) {
      // Transport failures (e.g. command not allowed by ACL) are thrown as Error.
      set({
        status: "error",
        contexts: [],
        current: null,
        selectedContext: null,
        warnings: [],
        error: toContextsError(err instanceof Error ? { message: err.message } : err),
      });
    }
  },
  selectContext: async (name: string) => {
    saveLastSelection({ context: name, group: activeGroupName });
    void get().ensureSettings([name]);
    const handler = connectHandler;
    if (handler === null) {
      set({ selectedContext: name });
      return;
    }
    set((state) => ({
      selectedContext: name,
      connectionStatus: {
        ...state.connectionStatus,
        [name]: { state: "connecting" },
      },
    }));
    try {
      const result = await handler(name);
      const nextStatus: ConnectionStatus =
        result === "connected" ? { state: "connected" } : result;
      set((state) => ({
        connectionStatus: {
          ...state.connectionStatus,
          [name]: nextStatus,
        },
      }));
    } catch (err: unknown) {
      const message =
        err instanceof Error
          ? err.message
          : typeof err === "object" && err !== null && "message" in err
            ? String(err.message)
            : String(err);
      set((state) => ({
        connectionStatus: {
          ...state.connectionStatus,
          [name]: { state: "error", message },
        },
      }));
    }
  },
  setContextSettings: (name: string, patch: Partial<ContextSettings>) => {
    set((state) => ({
      settingsByContext: {
        ...state.settingsByContext,
        [name]: {
          ...(state.settingsByContext[name] ?? defaultContextSettings),
          ...patch,
        },
      },
    }));
  },
  ensureSettings: async (names: readonly string[]) => {
    if (!isTauri()) return;
    const cached = get().settingsByContext;
    const todo = [...new Set(names)].filter(
      (n) => cached[n] === undefined && !settingsInFlight.has(n),
    );
    await Promise.all(
      todo.map(async (name) => {
        settingsInFlight.add(name);
        try {
          const res = await commands.getContextSettings(name);
          // A write action may have stored fresher settings meanwhile; keep those.
          // A malformed reply throws below and is ignored like an error.
          if (res.status === "ok") {
            if (get().settingsByContext[name] === undefined) {
              get().setContextSettings(name, {
                colour: res.data.colour ?? null,
                tags: res.data.tags,
                readOnly: res.data.readOnly,
                protected: res.data.protected,
                pinned: res.data.pinned,
                unlockedUntil: res.data.unlockedUntil ?? null,
              });
            }
          }
        } catch {
          // Unknown settings: the badge and swatch stay hidden; a later call may retry.
        } finally {
          settingsInFlight.delete(name);
        }
      }),
    );
  },
  unlockContext: async (context: string, confirmedName?: string | null) => {
    const res = await commands.unlockContext(context, confirmedName ?? null);
    if (res.status === "ok") {
      const s: ContextSettings = {
        colour: res.data.colour ?? null,
        tags: res.data.tags,
        readOnly: res.data.readOnly,
        protected: res.data.protected,
        pinned: res.data.pinned,
        unlockedUntil: res.data.unlockedUntil ?? null,
      };
      get().setContextSettings(context, s);
      return s;
    }
    throw new Error(res.error.message);
  },
  relockContext: async (context: string) => {
    const res = await commands.relockContext(context);
    if (res.status === "ok") {
      const s: ContextSettings = {
        colour: res.data.colour ?? null,
        tags: res.data.tags,
        readOnly: res.data.readOnly,
        protected: res.data.protected,
        pinned: res.data.pinned,
        unlockedUntil: null,
      };
      get().setContextSettings(context, s);
      return s;
    }
    throw new Error(res.error.message);
  },
  setConnectionStatus: (name: string, status: ConnectionStatus) => {
    set((state) => ({
      connectionStatus: {
        ...state.connectionStatus,
        [name]: status,
      },
    }));
  },
  setActiveClusterSelection: (clusters: string[]) => {
    // An ad-hoc selection is not a group; the next start restores the context only.
    pendingGroupRestore = null;
    activeGroupName = null;
    saveLastSelection({ context: get().selectedContext, group: null });
    set({ activeClusterSelection: clusters });
  },
  toggleClusterSelection: (name: string) => {
    pendingGroupRestore = null;
    activeGroupName = null;
    const { activeClusterSelection, selectedContext } = get();
    let nextSelection: string[];
    let nextSelected = selectedContext;

    if (activeClusterSelection.length === 0) {
      if (selectedContext === name) {
        nextSelection = [];
      } else if (selectedContext !== null) {
        nextSelection = [selectedContext, name];
        void get().ensureSettings([name]);
      } else {
        nextSelection = [name];
        void get().ensureSettings([name]);
      }
    } else {
      if (activeClusterSelection.includes(name)) {
        nextSelection = activeClusterSelection.filter((c) => c !== name);
        if (nextSelection.length === 0) {
          nextSelected = selectedContext ?? name;
        }
      } else {
        nextSelection = [...activeClusterSelection, name];
        void get().ensureSettings([name]);
      }
    }

    set({
      activeClusterSelection: nextSelection,
      selectedContext: nextSelected,
    });
    saveLastSelection({ context: get().selectedContext, group: null });
  },
  selectClusterGroup: (groupName: string) => {
    pendingGroupRestore = null;
    const selection = groupSelection(get(), groupName);
    activeGroupName = selection.length > 0 ? groupName : null;
    saveLastSelection({ context: get().selectedContext, group: activeGroupName });
    set({ activeClusterSelection: selection });
  },
  setClusterGroups: (groups: ClusterGroup[]) => {
    set({ clusterGroups: groups });
    restorePendingGroup();
  },
  loadClusterGroups: async () => {
    if (!clusterGroupBackendAvailable()) return;
    set({ clusterGroupsStatus: "loading", clusterGroupsError: null });
    try {
      const res = await clusterGroupClient.getClusterGroups();
      if (res.status === "ok") {
        set({ clusterGroups: res.data, clusterGroupsStatus: "ready", clusterGroupsError: null });
        restorePendingGroup();
      } else {
        set({ clusterGroupsStatus: "error", clusterGroupsError: toContextsError(res.error) });
        // The stored group cannot be resolved this session; keep the restored context only.
        pendingGroupRestore = null;
      }
    } catch (err: unknown) {
      set({ clusterGroupsStatus: "error", clusterGroupsError: toCaughtError(err) });
      pendingGroupRestore = null;
    }
  },
  saveClusterGroup: async (group: ClusterGroup) => {
    if (!clusterGroupBackendAvailable()) {
      set({ clusterGroupsError: notInDesktopApp });
      return false;
    }
    try {
      const res = await clusterGroupClient.setClusterGroup(group);
      if (res.status === "error") {
        set({ clusterGroupsError: toContextsError(res.error) });
        return false;
      }
      set((state) => ({
        clusterGroups: [
          ...state.clusterGroups.filter((g) => g.name !== group.name),
          { name: group.name, contexts: [...group.contexts] },
        ].sort(byName),
        clusterGroupsError: null,
      }));
      return true;
    } catch (err: unknown) {
      set({ clusterGroupsError: toCaughtError(err) });
      return false;
    }
  },
  deleteClusterGroup: async (name: string) => {
    if (!clusterGroupBackendAvailable()) {
      set({ clusterGroupsError: notInDesktopApp });
      return false;
    }
    try {
      const res = await clusterGroupClient.deleteClusterGroup(name);
      if (res.status === "error") {
        set({ clusterGroupsError: toContextsError(res.error) });
        return false;
      }
      set((state) => ({
        clusterGroups: state.clusterGroups.filter((g) => g.name !== name),
        clusterGroupsError: null,
      }));
      return true;
    } catch (err: unknown) {
      set({ clusterGroupsError: toCaughtError(err) });
      return false;
    }
  },
  reloadTeamSettings: async (path: string) => {
    if (!clusterGroupBackendAvailable()) {
      set({ teamSettingsStatus: "error", teamSettingsError: notInDesktopApp });
      return false;
    }
    set({ teamSettingsStatus: "loading", teamSettingsError: null });
    try {
      const runner = clusterGroupClient.reloadTeamSettings ?? commands.reloadTeamSettings;
      const res = await runner(path);
      if (res.status === "ok") {
        set({
          teamSettingsSummary: res.data,
          teamSettingsStatus: "ready",
          teamSettingsError: null,
        });
        await get().loadClusterGroups();
        await get().loadContexts();
        return true;
      } else {
        set({
          teamSettingsStatus: "error",
          teamSettingsError: toContextsError(res.error),
        });
        return false;
      }
    } catch (err: unknown) {
      set({
        teamSettingsStatus: "error",
        teamSettingsError: toCaughtError(err),
      });
      return false;
    }
  },
}));

/** Contexts `groupName` selects: a saved group, the implicit "All Clusters", else none. */
function groupSelection(
  state: Pick<ContextsState, "clusterGroups" | "contexts">,
  groupName: string,
): string[] {
  const group = state.clusterGroups.find((g) => g.name === groupName);
  if (group) return [...group.contexts];
  if (groupName === "All Clusters") return state.contexts.map((c) => c.name);
  return [];
}

/**
 * Re-applies the stored cluster group once both the contexts and the groups are known (they
 * load separately). A group that no longer exists, or whose contexts are all gone, is dropped
 * and the restored context stays selected. The user picking anything first cancels it.
 */
function restorePendingGroup(): void {
  const name = pendingGroupRestore;
  if (name === null) return;
  const state = useContextsStore.getState();
  if (state.status !== "ready") return;
  const known = state.clusterGroups.some((g) => g.name === name);
  // "All Clusters" needs only the contexts; a saved group waits for the groups to load.
  if (!known && name !== "All Clusters" && state.clusterGroupsStatus !== "ready") return;
  pendingGroupRestore = null;
  const names = new Set(state.contexts.map((c) => c.name));
  const selection = groupSelection(state, name).filter((c) => names.has(c));
  if (selection.length === 0) return;
  activeGroupName = name;
  useContextsStore.setState({ activeClusterSelection: selection });
}
