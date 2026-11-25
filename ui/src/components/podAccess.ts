import type { StoreAccess as AnyStoreAccess } from "../generated/bindings";
import type { StoreAccess } from "../stores/pods";
import { kindNoun } from "./resourceColumns";

/** One per-cluster notice shown instead of (or above) resource rows. */
export interface AccessNotice {
  cluster: string;
  /** `alert` for failures the user must act on, `status` for informational states. */
  role: "status" | "alert";
  text: string;
}

const formatCount = (n: number) => n.toLocaleString("en-US");

/**
 * Label for rows kept after a store's first initial list failed partway, so the
 * partial list is never shown unlabelled. Empty when the rows are not partial.
 */
export function partialSuffix(partialRows: number | null | undefined): string {
  if (partialRows === null || partialRows === undefined || partialRows <= 0) return "";
  const rows = partialRows === 1 ? "row" : "rows";
  return ` — showing ${formatCount(partialRows)} ${rows} received before the error (incomplete)`;
}

/** Notice for a synced store whose watcher restarted and is re-listing. */
export function reconnectingText(cluster: string, rows: number | undefined): string {
  return (rows ?? 0) > 0
    ? `Reconnecting to ${cluster}… (showing last known rows)`
    : `Reconnecting to ${cluster}…`;
}

/**
 * Label for a failure notice shown over rows the store still holds (a synced store
 * keeps its last list when it later fails). Empty when there are no rows, or when
 * they are a partial first list (labelled by {@link partialSuffix} instead).
 */
export function staleSuffix(rows: number | undefined, partialRows?: number | null): string {
  if ((rows ?? 0) <= 0 || (partialRows ?? 0) > 0) return "";
  return " — showing last known rows (may be outdated)";
}

/** Honest message for one store; `null` when the store is Ready (its rows are shown). */
export function accessNotice(entry: StoreAccess): AccessNotice | null {
  const { cluster, kind, access, rows } = entry;
  // In the background (mobile) the watcher is stopped on purpose: a connecting or re-listing
  // state from before the pause is not a connection problem, so say it is paused instead.
  if (entry.paused === true && (access.state === "loading" || access.state === "reconnecting")) {
    return { cluster, role: "status", text: `Paused in the background (${cluster}) — showing last known rows` };
  }
  switch (access.state) {
    case "ready":
      return null;
    case "loading":
      // Rows already arrived are shown under the `loadingProgress` banner instead.
      if (access.rowsSoFar > 0) return null;
      return { cluster, role: "status", text: `Connecting to ${cluster}…` };
    case "reconnecting":
      // The rows are the last complete list, kept while the watcher re-lists.
      return { cluster, role: "status", text: reconnectingText(cluster, rows) };
    case "forbidden": {
      const scope = access.namespace === null ? "" : ` (namespace ${access.namespace})`;
      return {
        cluster,
        role: "alert",
        text: `No permission to list ${access.resource} in ${cluster}${scope}${staleSuffix(rows)}`,
      };
    }
    case "notFound":
      return {
        cluster,
        role: "status",
        text: `The ${access.resource} API is not installed in ${cluster}${staleSuffix(rows)}`,
      };
    case "unsupported":
      return {
        cluster,
        role: "status",
        text: `${access.message} (${cluster})${staleSuffix(rows)}`,
      };
    case "authExpired":
      return {
        cluster,
        role: "alert",
        text: `Authentication expired for ${cluster}: ${access.hint}${staleSuffix(rows, access.partialRows)}`,
      };
    case "error":
      return {
        cluster,
        role: "alert",
        text: `Cannot list ${kindNoun(kind)} in ${cluster}: ${access.message}${partialSuffix(access.partialRows)}${staleSuffix(rows, access.partialRows)}`,
      };
  }
}

/**
 * Store watched namespace by namespace because its cluster-wide list/watch is forbidden
 * (05-01). Shown as a status pill, not an error: the rows are real, just limited.
 */
export interface RestrictedScope {
  cluster: string;
  /** Short pill text, e.g. "Restricted: 3 namespaces (cluster-wide watch forbidden)". */
  label: string;
  /** Longer explanation for the tooltip / screen readers. */
  detail: string;
}

const SOURCE_DETAIL = {
  configured: "from this context's namespace settings",
  discovered: "every namespace you may list",
  contextNamespace: "the kubeconfig context's namespace",
} as const;

export function restrictedScope(entry: StoreAccess): RestrictedScope | null {
  const { scope } = entry;
  if (scope === undefined) return null;
  const watched = scope.namespaces.map((n) => n.namespace);
  const count = watched.length;
  const label = `Restricted: ${count === 1 ? `namespace ${watched[0] ?? ""}` : `${formatCount(count)} namespaces`} (cluster-wide watch forbidden)`;
  const parts = [
    `${entry.cluster}: no permission to watch ${kindNoun(entry.kind)} cluster-wide, so only ${count === 1 ? "this namespace is" : "these namespaces are"} watched (${SOURCE_DETAIL[scope.source]}): ${watched.join(", ")}.`,
  ];
  if (scope.denied.length > 0) parts.push(`Not permitted: ${scope.denied.join(", ")}.`);
  if (scope.truncated) parts.push("More namespaces exist than are checked; set the namespaces to watch in the context settings.");
  return {
    cluster: entry.cluster,
    label,
    detail: parts.join(" "),
  };
}

/** Restricted-scope pills of the queried clusters, one per restricted store. */
export function restrictedScopes(
  clusters: readonly string[],
  access: readonly StoreAccess[],
): RestrictedScope[] {
  return access
    .filter((a) => clusters.includes(a.cluster))
    .map(restrictedScope)
    .filter((s): s is RestrictedScope => s !== null);
}

/**
 * Notices for every queried cluster that is not Ready. A cluster without an access
 * entry yet (first page still pending) counts as connecting.
 */
export function accessNotices(
  clusters: readonly string[],
  access: readonly StoreAccess[],
): AccessNotice[] {
  const notices: AccessNotice[] = [];
  for (const cluster of clusters) {
    const entries = access.filter((a) => a.cluster === cluster);
    if (entries.length === 0) {
      notices.push({ cluster, role: "status", text: `Connecting to ${cluster}…` });
      continue;
    }
    for (const entry of entries) {
      const notice = accessNotice(entry);
      if (notice !== null) notices.push(notice);
    }
  }
  return notices;
}

/**
 * Banner text while stores are still in their initial list but already show rows,
 * so a partial list is never shown unlabelled: "Loading… 1,200 rows so far (dev)",
 * with a per-cluster breakdown when several clusters are loading. When the loading
 * stores span several kinds (e.g. the Applications view) the count is of "objects".
 * `null` when no queried store is loading with rows.
 */
export function loadingProgress(access: readonly AnyStoreAccess[]): string | null {
  const perCluster = new Map<string, number>();
  const kinds = new Set<string>();
  for (const { cluster, kind, access: state } of access) {
    if (state.state === "loading" && state.rowsSoFar > 0) {
      perCluster.set(cluster, (perCluster.get(cluster) ?? 0) + state.rowsSoFar);
      kinds.add(kind);
    }
  }
  if (perCluster.size === 0) return null;
  let sum = 0;
  for (const n of perCluster.values()) sum += n;
  const clusters =
    perCluster.size === 1
      ? [...perCluster.keys()].join("")
      : [...perCluster].map(([cluster, n]) => `${cluster}: ${formatCount(n)}`).join(", ");
  const unit = kinds.size > 1 ? "object" : "row";
  return `Loading… ${formatCount(sum)} ${unit}${sum === 1 ? "" : "s"} so far (${clusters})`;
}
