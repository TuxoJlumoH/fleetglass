import type { WorkloadLogTarget_Deserialize, WorkloadSelector } from "./generated/bindings";

/**
 * Result of parsing `location.hash` for the detached logs route that the Rust side
 * opens as `index.html#/logs?target=<url-encoded JSON WorkloadLogTarget>`.
 */
export type LogsRoute =
  | { kind: "notLogs" }
  | { kind: "logs"; target: WorkloadLogTarget_Deserialize }
  | { kind: "invalid"; message: string };

const ROUTE_PREFIX = "#/logs";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function parseSelector(raw: unknown): WorkloadSelector | string {
  if (!isRecord(raw)) return "selector must be an object";
  if (raw.type === "pod") {
    if (!nonEmptyString(raw.name)) return "pod selector needs a non-empty `name`";
    return { type: "pod", name: raw.name };
  }
  if (raw.type === "labels") {
    if (typeof raw.selector !== "string") return "labels selector needs a string `selector`";
    return { type: "labels", selector: raw.selector };
  }
  if (raw.type === "owner") {
    if (!nonEmptyString(raw.kind) || !nonEmptyString(raw.name)) {
      return "owner selector needs non-empty `kind` and `name`";
    }
    return { type: "owner", kind: raw.kind, name: raw.name };
  }
  return 'selector.type must be "pod", "labels" or "owner"';
}

/** Validates a decoded JSON value as a `WorkloadLogTarget`; returns an error message otherwise. */
export function validateWorkloadLogTarget(raw: unknown): WorkloadLogTarget_Deserialize | string {
  if (!isRecord(raw)) return "target must be a JSON object";
  if (!nonEmptyString(raw.cluster)) return "target.cluster must be a non-empty string";
  if (!nonEmptyString(raw.namespace)) return "target.namespace must be a non-empty string";
  const selector = parseSelector(raw.selector);
  if (typeof selector === "string") return `target.${selector}`;

  const container = raw.container ?? null;
  if (container !== null && !nonEmptyString(container)) {
    return "target.container must be a non-empty string or null";
  }
  const tailLines = raw.tailLines ?? null;
  if (
    tailLines !== null &&
    (typeof tailLines !== "number" || !Number.isSafeInteger(tailLines) || tailLines < 0)
  ) {
    return "target.tailLines must be a non-negative integer or null";
  }
  return { cluster: raw.cluster, namespace: raw.namespace, selector, container, tailLines };
}

/** Pure parser for `location.hash`; anything not under `#/logs` keeps the main app. */
export function parseLogsRoute(hash: string): LogsRoute {
  if (!hash.startsWith(ROUTE_PREFIX)) return { kind: "notLogs" };
  const rest = hash.slice(ROUTE_PREFIX.length);
  if (rest !== "" && !rest.startsWith("?") && !rest.startsWith("/")) return { kind: "notLogs" };

  const queryStart = rest.indexOf("?");
  const params = new URLSearchParams(queryStart === -1 ? "" : rest.slice(queryStart + 1));
  const encoded = params.get("target");
  if (encoded === null || encoded === "") {
    return { kind: "invalid", message: "The logs window URL has no target parameter." };
  }

  let decoded: unknown;
  try {
    decoded = JSON.parse(encoded);
  } catch {
    return { kind: "invalid", message: "The logs window target is not valid JSON." };
  }
  const target = validateWorkloadLogTarget(decoded);
  if (typeof target === "string") {
    return { kind: "invalid", message: `The logs window target is invalid: ${target}.` };
  }
  return { kind: "logs", target };
}
