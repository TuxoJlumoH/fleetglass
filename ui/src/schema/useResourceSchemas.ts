import { useEffect, useMemo } from "react";
import { schemaKey, useResourceSchemasStore } from "../stores/resourceSchemas";
import type { SchemaLookup } from "./types";
import { listDocInfos } from "./yamlAst";

export interface YamlSchemas {
  /** Lookup for the editor, `undefined` without a context. */
  lookup: SchemaLookup | undefined;
  /** "Schema unavailable: …" notes, one per kind that has no schema. */
  notes: string[];
}

/**
 * Fetches (via the store) the cluster schema of every kind in `yaml` on `context`.
 * Documents whose schema is missing or still loading are not validated.
 */
export function useResourceSchemas(context: string, yaml: string): YamlSchemas {
  const gvks = useMemo(() => {
    const seen = new Map<string, { apiVersion: string; kind: string }>();
    for (const info of listDocInfos(yaml)) {
      if (info.apiVersion && info.kind) {
        seen.set(`${info.apiVersion} ${info.kind}`, { apiVersion: info.apiVersion, kind: info.kind });
      }
    }
    return [...seen.values()];
  }, [yaml]);

  const ensure = useResourceSchemasStore((s) => s.ensure);
  const entries = useResourceSchemasStore((s) => s.entries);

  useEffect(() => {
    for (const { apiVersion, kind } of gvks) ensure(context, apiVersion, kind);
  }, [context, gvks, ensure]);

  const lookup = useMemo<SchemaLookup | undefined>(() => {
    if (!context) return undefined;
    return (apiVersion, kind) => {
      const entry = entries[schemaKey(context, apiVersion, kind)];
      return entry?.state === "ready" ? entry.schema : undefined;
    };
  }, [context, entries]);

  const notes = useMemo(() => {
    const out: string[] = [];
    for (const { apiVersion, kind } of gvks) {
      const entry = entries[schemaKey(context, apiVersion, kind)];
      if (entry?.state === "unavailable") {
        out.push(`Schema unavailable: ${entry.message} (${apiVersion} ${kind})`);
      } else if (entry?.state === "ready" && entry.truncated) {
        out.push(`Schema for ${kind} (${apiVersion}) is incomplete: it was cut to stay small.`);
      }
    }
    return out;
  }, [context, entries, gvks]);

  return { lookup, notes };
}
