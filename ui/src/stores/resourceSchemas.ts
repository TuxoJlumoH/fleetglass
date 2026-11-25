import { create } from "zustand";
import { commands, type SchemaUnavailableReason } from "../generated/bindings";
import type { K8sJsonSchema } from "../schema/types";

/** Schema of one (context, apiVersion, kind), fetched from the cluster (`get_resource_schema`). */
export type SchemaEntry =
  | { state: "loading" }
  | { state: "ready"; schema: K8sJsonSchema; truncated: boolean }
  | { state: "unavailable"; reason: SchemaUnavailableReason | "error"; message: string; at: number };

/** Schemas kept in memory; a Deployment schema is about 350 KB. */
export const MAX_CACHED_SCHEMAS = 24;
/** An unreachable cluster or failed call is asked again after this long. */
export const RETRY_UNAVAILABLE_MS = 30_000;

export interface ResourceSchemasState {
  entries: Record<string, SchemaEntry>;
  /** Fetches the schema unless it is cached (or a fetch is in flight). */
  ensure: (context: string, apiVersion: string, kind: string) => void;
  /** Drops the cached schemas of `context` (after a reconnect). */
  invalidateContext: (context: string) => void;
}

export function schemaKey(context: string, apiVersion: string, kind: string): string {
  return `${context}\u0000${apiVersion}\u0000${kind}`;
}

// Least recently requested first; bounds `entries`.
let order: string[] = [];

function touch(key: string): string[] {
  order = [...order.filter((k) => k !== key), key];
  const evicted = order.length > MAX_CACHED_SCHEMAS ? order.slice(0, order.length - MAX_CACHED_SCHEMAS) : [];
  order = order.slice(evicted.length);
  return evicted;
}

function isRetryable(entry: SchemaEntry): boolean {
  return (
    entry.state === "unavailable" &&
    (entry.reason === "unreachable" || entry.reason === "error") &&
    Date.now() - entry.at > RETRY_UNAVAILABLE_MS
  );
}

function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "object" && err !== null && "message" in err) return String(err.message);
  return String(err);
}

export function resetResourceSchemasStore(): void {
  order = [];
  useResourceSchemasStore.setState({ entries: {} });
}

export const useResourceSchemasStore = create<ResourceSchemasState>()((set, get) => ({
  entries: {},
  ensure: (context, apiVersion, kind) => {
    if (!context || !apiVersion || !kind) return;
    const key = schemaKey(context, apiVersion, kind);
    const existing = get().entries[key];
    if (existing && !isRetryable(existing)) {
      touch(key);
      return;
    }
    const evicted = touch(key);
    set((s) => {
      const gone = new Set(evicted);
      const kept = Object.entries(s.entries).filter(([k]) => !gone.has(k));
      return { entries: { ...Object.fromEntries(kept), [key]: { state: "loading" } } };
    });

    const settle = (entry: SchemaEntry) => {
      set((s) => (s.entries[key] ? { entries: { ...s.entries, [key]: entry } } : s));
    };
    void (async () => {
      try {
        const res = await commands.getResourceSchema(context, apiVersion, kind);
        if (res.status === "error") {
          settle({ state: "unavailable", reason: "error", message: res.error.message, at: Date.now() });
          return;
        }
        const data = res.data;
        if (data.state === "unavailable") {
          settle({ state: "unavailable", reason: data.reason, message: data.message, at: Date.now() });
          return;
        }
        const schema = JSON.parse(data.schemaJson) as K8sJsonSchema;
        settle({ state: "ready", schema, truncated: data.truncated });
      } catch (err: unknown) {
        settle({ state: "unavailable", reason: "error", message: errorMessage(err), at: Date.now() });
      }
    })();
  },
  invalidateContext: (context) => {
    const prefix = `${context}\u0000`;
    order = order.filter((k) => !k.startsWith(prefix));
    set((s) => ({
      entries: Object.fromEntries(Object.entries(s.entries).filter(([k]) => !k.startsWith(prefix))),
    }));
  },
}));
