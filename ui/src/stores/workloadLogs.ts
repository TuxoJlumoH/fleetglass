import { Channel, isTauri } from "@tauri-apps/api/core";
import { createStore, type StoreApi } from "zustand/vanilla";
import {
  commands,
  type ErrorKind,
  type MuxLogEvent,
  type MuxLogLine,
  type WorkloadLogTarget,
} from "../generated/bindings";

/** Default number of lines kept client-side; the oldest are dropped beyond this. */
export const DEFAULT_MAX_LINES = 50_000;
/** Most recent errors kept for the banner (the total count is tracked separately). */
const MAX_KEPT_ERRORS = 20;

/** Backend calls used for workload logs; injectable for tests via `setWorkloadLogsClient`. */
export interface WorkloadLogsClient {
  /** Creates the channel passed to `startWorkloadLogs`; `onEvent` receives every event. */
  createChannel: (onEvent: (event: MuxLogEvent) => void) => Channel<MuxLogEvent>;
  startWorkloadLogs: typeof commands.startWorkloadLogs;
  stopWorkloadLogs: typeof commands.stopWorkloadLogs;
  openDetachedLogs: typeof commands.openDetachedLogs;
  /** Writes a `.log` file via the backend. Absent ⇒ the view falls back to a Blob download. */
  saveLogFile?: typeof commands.saveLogFile;
  /** Android share sheet (03-04a). Absent ⇒ sharing reports "not available". */
  shareText?: typeof commands.shareText;
}

const defaultClient: WorkloadLogsClient = {
  createChannel: (onEvent) => new Channel<MuxLogEvent>(onEvent),
  startWorkloadLogs: (target, onEvent) => commands.startWorkloadLogs(target, onEvent),
  stopWorkloadLogs: (id) => commands.stopWorkloadLogs(id),
  openDetachedLogs: (target) => commands.openDetachedLogs(target),
  saveLogFile: (fileName, contents) => commands.saveLogFile(fileName, contents),
  shareText: (title, fileName, text) => commands.shareText(title, fileName, text),
};
let client: WorkloadLogsClient = defaultClient;

export function setWorkloadLogsClient(next?: WorkloadLogsClient): void {
  client = next ?? defaultClient;
}

/** The generated commands need the Tauri runtime; injected clients do not. */
function backendAvailable(): boolean {
  return client !== defaultClient || isTauri();
}

/** Outcome of {@link saveLogFile}: `null` when no backend can save (use a Blob download). */
export type SaveLogResult = { ok: true; path: string } | { ok: false; message: string };

/**
 * Saves `contents` through the backend (`<a download>` does nothing in WebKitGTK).
 * Returns `null` outside the desktop app or when the client cannot save files.
 */
export async function saveLogFile(
  fileName: string,
  contents: string,
): Promise<SaveLogResult | null> {
  const save = client.saveLogFile;
  if (save === undefined || !backendAvailable()) return null;
  try {
    const res = await save(fileName, contents);
    if (res.status === "ok") return { ok: true, path: res.data.path };
    return { ok: false, message: toError("save", res.error).message };
  } catch (err: unknown) {
    return { ok: false, message: toError("save", err).message };
  }
}

/** Outcome of {@link shareLogText}. */
export type ShareLogResult = { ok: true } | { ok: false; message: string };

/**
 * Hands `text` to the Android share sheet as the file `fileName` (only call it on Android:
 * desktop backends answer "not supported").
 */
export async function shareLogText(
  title: string,
  fileName: string,
  text: string,
): Promise<ShareLogResult> {
  const share = client.shareText;
  if (share === undefined || !backendAvailable()) {
    return { ok: false, message: "Not running in the Fleetglass app" };
  }
  try {
    const res = await share(title, fileName, text);
    if (res.status === "ok") return { ok: true };
    return { ok: false, message: toError("save", res.error).message };
  } catch (err: unknown) {
    return { ok: false, message: toError("save", err).message };
  }
}

export type WorkloadLogsErrorSource = "start" | "stream" | "stop" | "popOut" | "save";

export interface WorkloadLogsError {
  /** Where the problem came from: a failed command, or an `error` event on the stream. */
  source: WorkloadLogsErrorSource;
  /** `null` for stream events and failures that did not come from the backend as an `IpcError`. */
  kind: ErrorKind | null;
  message: string;
  /** `Date.now()` when the UI received the error. */
  receivedAt: number;
}

/** A received line plus UI-only metadata. */
export interface LogEntry extends MuxLogLine {
  /** Client-side, never-wrapping id (the backend `seq` wraps); stable React key. */
  id: number;
  /** `Date.now()` when this window received the line. Lines carry no Kubernetes timestamp. */
  receivedAt: number;
}

/** Fixed-capacity FIFO: pushing beyond `capacity` overwrites the oldest entry. */
export class LogRing<T> {
  readonly capacity: number;
  private readonly buf: (T | undefined)[] = [];
  private head = 0;
  private len = 0;
  /** Entries overwritten since creation. */
  dropped = 0;

  constructor(capacity: number) {
    if (!Number.isFinite(capacity) || capacity < 1) {
      throw new RangeError(`LogRing capacity must be >= 1, got ${String(capacity)}`);
    }
    this.capacity = Math.floor(capacity);
  }

  get size(): number {
    return this.len;
  }

  push(item: T): void {
    if (this.len < this.capacity) {
      this.buf[(this.head + this.len) % this.capacity] = item;
      this.len += 1;
    } else {
      this.buf[this.head] = item;
      this.head = (this.head + 1) % this.capacity;
      this.dropped += 1;
    }
  }

  /** `i` = 0 is the oldest entry kept. */
  at(i: number): T | undefined {
    if (i < 0 || i >= this.len) return undefined;
    return this.buf[(this.head + i) % this.capacity];
  }

  toArray(): T[] {
    const out: T[] = [];
    for (let i = 0; i < this.len; i += 1) {
      const item = this.at(i);
      if (item !== undefined) out.push(item);
    }
    return out;
  }
}

export type PodPresence = "present" | "left";

export type WorkloadLogsStatus = "idle" | "starting" | "streaming" | "stopped" | "failed";

export interface WorkloadLogsState {
  status: WorkloadLogsStatus;
  /** Mutated in place for speed; `version` changes whenever its contents do. */
  ring: LogRing<LogEntry>;
  version: number;
  /** Pods seen on this stream: from `podJoined`/`podLeft`, or from lines of unannounced pods. */
  pods: Record<string, PodPresence>;
  /** Containers seen in received lines, in first-seen order. */
  containers: string[];
  /** Most recent errors, oldest first. */
  errors: WorkloadLogsError[];
  errorCount: number;
  start: (target: WorkloadLogTarget) => Promise<void>;
  stop: () => Promise<void>;
  popOut: (target: WorkloadLogTarget) => Promise<boolean>;
  dismissErrors: () => void;
  /** Feeds one channel event; used by the channel and directly by tests. */
  applyEvent: (event: MuxLogEvent) => void;
}

export interface WorkloadLogsStoreOptions {
  /** Client-side line cap (default 50,000). */
  maxLines?: number;
}

export type WorkloadLogsStore = StoreApi<WorkloadLogsState>;

/**
 * The generated wrapper types every non-`Error` rejection as `IpcError`, but Tauri
 * itself rejects with plain strings (e.g. "Command ... not allowed by ACL").
 */
function toError(source: WorkloadLogsErrorSource, raw: unknown): WorkloadLogsError {
  const value = raw instanceof Error ? { message: raw.message } : raw;
  if (typeof value === "object" && value !== null && "message" in value) {
    const { kind, message } = value as { kind?: unknown; message: unknown };
    return {
      source,
      kind: typeof kind === "string" ? (kind as ErrorKind) : null,
      message: String(message),
      receivedAt: Date.now(),
    };
  }
  return { source, kind: null, message: String(value), receivedAt: Date.now() };
}

let nextEntryId = 1;

/** One store per log view, so several views (e.g. main window and a detached one) can coexist. */
export function createWorkloadLogsStore(options: WorkloadLogsStoreOptions = {}): WorkloadLogsStore {
  const maxLines = options.maxLines ?? DEFAULT_MAX_LINES;
  // Incremented on every start/stop; events and results of older generations are ignored.
  let generation = 0;
  let activeId: number | null = null;

  return createStore<WorkloadLogsState>()((set, get) => {
    const pushError = (error: WorkloadLogsError) => {
      set((state) => ({
        errors: [...state.errors, error].slice(-MAX_KEPT_ERRORS),
        errorCount: state.errorCount + 1,
      }));
    };

    const stopId = async (id: number) => {
      try {
        const res = await client.stopWorkloadLogs(id);
        if (res.status === "error") pushError(toError("stop", res.error));
      } catch (err: unknown) {
        pushError(toError("stop", err));
      }
    };

    return {
      status: "idle",
      ring: new LogRing<LogEntry>(maxLines),
      version: 0,
      pods: {},
      containers: [],
      errors: [],
      errorCount: 0,

      applyEvent: (event) => {
        switch (event.type) {
          case "lines": {
            if (event.lines.length === 0) return;
            const { ring, pods, containers } = get();
            const receivedAt = Date.now();
            let nextPods: Record<string, PodPresence> | null = null;
            let nextContainers: string[] | null = null;
            for (const line of event.lines) {
              ring.push({ ...line, id: nextEntryId, receivedAt });
              nextEntryId += 1;
              if (!(line.pod in (nextPods ?? pods))) {
                nextPods ??= { ...pods };
                nextPods[line.pod] = "present";
              }
              if (!(nextContainers ?? containers).includes(line.container)) {
                nextContainers ??= [...containers];
                nextContainers.push(line.container);
              }
            }
            set((state) => ({
              version: state.version + 1,
              ...(nextPods !== null ? { pods: nextPods } : {}),
              ...(nextContainers !== null ? { containers: nextContainers } : {}),
            }));
            return;
          }
          case "podJoined":
            set((state) => ({ pods: { ...state.pods, [event.pod]: "present" } }));
            return;
          case "podLeft":
            set((state) => ({ pods: { ...state.pods, [event.pod]: "left" } }));
            return;
          case "error":
            pushError({ source: "stream", kind: null, message: event.message, receivedAt: Date.now() });
            return;
        }
      },

      start: async (target) => {
        generation += 1;
        const gen = generation;
        const previous = activeId;
        activeId = null;
        if (previous !== null) void stopId(previous);
        set({
          status: "starting",
          ring: new LogRing<LogEntry>(maxLines),
          version: get().version + 1,
          pods: {},
          containers: [],
          errors: [],
          errorCount: 0,
        });
        if (!backendAvailable()) {
          set({ status: "failed" });
          pushError({
            source: "start",
            kind: null,
            message: "Not running in the Fleetglass app",
            receivedAt: Date.now(),
          });
          return;
        }
        const channel = client.createChannel((event) => {
          if (gen === generation) get().applyEvent(event);
        });
        try {
          const res = await client.startWorkloadLogs(target, channel);
          if (res.status === "ok") {
            if (gen !== generation) {
              // Stopped or restarted while the start was in flight.
              void stopId(res.data);
              return;
            }
            activeId = res.data;
            set({ status: "streaming" });
          } else if (gen === generation) {
            set({ status: "failed" });
            pushError(toError("start", res.error));
          }
        } catch (err: unknown) {
          if (gen !== generation) return;
          set({ status: "failed" });
          pushError(toError("start", err));
        }
      },

      stop: async () => {
        generation += 1;
        const id = activeId;
        activeId = null;
        if (get().status === "starting" || get().status === "streaming") {
          set({ status: "stopped" });
        }
        if (id !== null) await stopId(id);
      },

      popOut: async (target) => {
        if (!backendAvailable()) {
          pushError({
            source: "popOut",
            kind: null,
            message: "Not running in the Fleetglass app",
            receivedAt: Date.now(),
          });
          return false;
        }
        try {
          const res = await client.openDetachedLogs(target);
          if (res.status === "error") {
            pushError(toError("popOut", res.error));
            return false;
          }
          return true;
        } catch (err: unknown) {
          pushError(toError("popOut", err));
          return false;
        }
      },

      dismissErrors: () => {
        set({ errors: [] });
      },
    };
  });
}
