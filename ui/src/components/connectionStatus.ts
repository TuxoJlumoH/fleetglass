import type { ConnectionStatus, ContextSettings } from "../stores/contexts";
import type { StoreAccess, StoreAccessState } from "../stores/pods";

/** Dot colour classes from the v0.2 mockup (`.dot.ok` … plus `.off` for "no claim"). */
export type DotTone = "ok" | "warn" | "err" | "off";

export interface ConnectionLight {
  tone: DotTone;
  /** Short visible text: "Connected", "Reconnecting…", "No permission", ... */
  text: string;
  /** Full accessible label, including the backend message where there is one. */
  label: string;
}

/** Neutral dot for a context we know nothing reliable about; never claims "Disconnected". */
export const UNKNOWN_LIGHT: ConnectionLight = {
  tone: "off",
  text: "",
  label: "No connection status",
};

/**
 * Light from the contexts store's `connectionStatus`. Only a real connect handler or diagnostics
 * write it (the store no longer seeds defaults), so a missing entry is unknown: `null`.
 */
export function connectionLight(status: ConnectionStatus | undefined): ConnectionLight | null {
  if (!status) return null;
  switch (status.state) {
    case "disconnected":
      return { tone: "off", text: "Disconnected", label: "Disconnected" };
    case "connected":
      return { tone: "ok", text: "Connected", label: "Connected" };
    case "connecting":
      return { tone: "warn", text: "Connecting…", label: "Connecting" };
    case "degraded":
      return { tone: "warn", text: "Degraded", label: `Degraded: ${status.message}` };
    case "error":
      return { tone: "err", text: "Error", label: `Error: ${status.message}` };
  }
}

/** Light for one watch store's access state (what the backend actually reports). */
export function accessLight(access: StoreAccessState): ConnectionLight {
  switch (access.state) {
    case "ready":
      return { tone: "ok", text: "Connected", label: "Connected" };
    case "loading":
      return { tone: "warn", text: "Loading…", label: "Loading (initial list)" };
    case "reconnecting":
      return { tone: "warn", text: "Reconnecting…", label: "Reconnecting (re-listing)" };
    case "error":
      return { tone: "err", text: "Error", label: `Error: ${access.message}` };
    case "forbidden":
      return { tone: "err", text: "No permission", label: `No permission: ${access.message}` };
    case "notFound":
      return { tone: "err", text: "API not found", label: `API not found: ${access.message}` };
    case "unsupported":
      return { tone: "err", text: "Not supported", label: `Not supported: ${access.message}` };
    case "authExpired":
      return { tone: "err", text: "Auth Expired", label: `Auth Expired: ${access.hint}` };
  }
}

const TONE_RANK: Record<DotTone, number> = { off: 0, ok: 1, warn: 2, err: 3 };

/** Worst access light per cluster over the active table's stores. */
export function accessLightsByCluster(
  access: readonly StoreAccess[],
): Record<string, ConnectionLight> {
  const out: Record<string, ConnectionLight> = {};
  for (const a of access) {
    const light = accessLight(a.access);
    const prev = out[a.cluster];
    if (prev === undefined || TONE_RANK[light.tone] > TONE_RANK[prev.tone]) out[a.cluster] = light;
  }
  return out;
}

/**
 * Light for `cluster`: its access-derived light when the active table has stores there, else
 * the contexts store's status, else `null` (unknown: make no claim).
 */
export function clusterLight(
  cluster: string,
  accessLights: Readonly<Record<string, ConnectionLight>>,
  status: ConnectionStatus | undefined,
): ConnectionLight | null {
  return accessLights[cluster] ?? connectionLight(status);
}

function plural(n: number, word: string): string {
  return `${String(n)} ${word}${n === 1 ? "" : "s"}`;
}

/**
 * Connection summary for the queried clusters, derived like `clusterLight`. One cluster reads
 * like its own light; several read like "3 clusters · 1 error · 1 reconnecting". `null` when
 * nothing is selected or nothing reliable is known.
 */
export function summariseConnections(
  clusters: readonly string[],
  accessLights: Readonly<Record<string, ConnectionLight>>,
  statusByContext: Readonly<Record<string, ConnectionStatus>>,
): ConnectionLight | null {
  const [only] = clusters;
  if (only === undefined) return null;
  if (clusters.length === 1) return clusterLight(only, accessLights, statusByContext[only]);

  const lights = clusters.map((c) => ({
    name: c,
    light: clusterLight(c, accessLights, statusByContext[c]),
  }));
  const known = lights.flatMap((l) => (l.light === null ? [] : [l.light]));
  if (known.length === 0) return null;

  const errors = known.filter((l) => l.tone === "err").length;
  const connected = known.filter((l) => l.tone === "ok").length;
  // Warn states keep their own word: "1 reconnecting · 2 loading".
  const warnCounts = new Map<string, number>();
  for (const l of known) {
    if (l.tone !== "warn") continue;
    const word = l.text.replace("…", "").toLowerCase();
    warnCounts.set(word, (warnCounts.get(word) ?? 0) + 1);
  }

  const parts = [plural(clusters.length, "cluster")];
  if (connected === clusters.length) parts.push("all connected");
  if (errors > 0) parts.push(plural(errors, "error"));
  for (const [word, n] of warnCounts) parts.push(`${String(n)} ${word}`);

  const tone: DotTone =
    errors > 0 ? "err" : warnCounts.size > 0 ? "warn" : connected === clusters.length ? "ok" : "off";
  return {
    tone,
    text: parts.join(" · "),
    label: lights.map((l) => `${l.name}: ${(l.light ?? UNKNOWN_LIGHT).label}`).join("\n"),
  };
}

/**
 * True when every queried cluster has at least one store in `access` and all of them are
 * ready, i.e. the table's rows reflect the clusters right now.
 */
export function allReady(access: readonly StoreAccess[], clusters: readonly string[]): boolean {
  if (clusters.length === 0) return false;
  const stores = access.filter((a) => clusters.includes(a.cluster));
  return (
    clusters.every((c) => stores.some((a) => a.cluster === c)) &&
    stores.every((a) => a.access.state === "ready")
  );
}

/**
 * True when a store of the queried clusters is paused because the app is in the background
 * (mobile, 03-04a): its rows are the last known ones and nothing is watched.
 */
export function anyPaused(access: readonly StoreAccess[], clusters: readonly string[]): boolean {
  return access.some((a) => a.paused === true && clusters.includes(a.cluster));
}

/** Status text while the app is (or just was) in the background. */
export const PAUSED_TEXT = "Paused — showing last known data";

export interface MetricsSummary {
  text: string;
  title?: string;
}

/**
 * `metrics.k8s.io` state of the active table's stores on the queried clusters (only kinds with
 * usage columns carry one). `null` when unknown: no such store yet, or the first poll pending.
 */
export function metricsSummary(
  access: readonly StoreAccess[],
  clusters: readonly string[],
): MetricsSummary | null {
  const states = access.filter((a) => a.metrics !== null && clusters.includes(a.cluster));
  if (states.length === 0) return null;
  // The first non-available store explains the gap; with several clusters, name it.
  const problem = states.find((a) => a.metrics?.state !== "available" && a.metrics?.state !== "pending");
  if (problem?.metrics) {
    const suffix = clusters.length > 1 ? ` (${problem.cluster})` : "";
    const m = problem.metrics;
    switch (m.state) {
      case "notInstalled":
        return { text: `metrics-server not installed${suffix}`, title: m.message };
      case "forbidden":
        return { text: `metrics: no permission${suffix}`, title: m.message };
      case "error":
        return { text: `metrics unavailable${suffix}`, title: m.message };
      default:
        break;
    }
  }
  if (states.every((a) => a.metrics?.state === "available")) {
    return { text: "metrics-server available" };
  }
  return null;
}

export type AccessMode = "read-only" | "read-write" | "mixed";

/** Read-only flag of the queried clusters; `null` when any cluster's settings are not loaded. */
export function accessMode(
  clusters: readonly string[],
  settingsByContext: Readonly<Record<string, ContextSettings>>,
): AccessMode | null {
  if (clusters.length === 0) return null;
  const flags: boolean[] = [];
  for (const c of clusters) {
    const s = settingsByContext[c];
    if (s === undefined) return null;
    flags.push(s.readOnly);
  }
  if (flags.every(Boolean)) return "read-only";
  if (flags.every((f) => !f)) return "read-write";
  return "mixed";
}

/** "Updated 2s ago" / "Updated 3m ago" / "Updated 1h ago". */
export function formatUpdatedAgo(updatedAt: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - updatedAt) / 1000));
  if (seconds < 60) return `Updated ${String(seconds)}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `Updated ${String(minutes)}m ago`;
  return `Updated ${String(Math.floor(minutes / 60))}h ago`;
}
