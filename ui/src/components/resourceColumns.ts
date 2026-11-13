import type { Row, SortField } from "../stores/pods";
import { RESTARTS_HOT } from "./statusTone";

/**
 * Column headers per kind. The backend sends typed `cells` but no header metadata, so this
 * map mirrors the cell order of the core projections (`crates/core/src/projection.rs`,
 * `cluster.rs`, `detail.rs`). Kinds without an entry (CRDs, ecosystem kinds) get columns
 * derived from the cell types of the rows the backend returned.
 */

export type ColumnSource =
  | { type: "cluster" }
  | { type: "namespace" }
  | { type: "name" }
  | { type: "cell"; index: number };

export interface ResourceColumn {
  key: string;
  label: string;
  source: ColumnSource;
  sortField: SortField | null;
  /** CSS grid track size. */
  width: string;
  mono: boolean;
  /** Int cells at or above this value are highlighted (pod restarts, mockup `.restarts.hot`). */
  hotAt?: number;
  /** Always shown: the column picker cannot hide it (name and status). */
  pinned?: boolean;
}

const CLUSTER: ResourceColumn = {
  key: "cluster",
  label: "Cluster",
  source: { type: "cluster" },
  sortField: { type: "cluster" },
  width: "110px",
  mono: false,
};
const NAMESPACE: ResourceColumn = {
  key: "namespace",
  label: "Namespace",
  source: { type: "namespace" },
  sortField: { type: "namespace" },
  width: "120px",
  mono: false,
};
const NAME: ResourceColumn = {
  key: "name",
  label: "Name",
  source: { type: "name" },
  sortField: { type: "name" },
  width: "minmax(180px, 2fr)",
  mono: false,
  pinned: true,
};

type CellKind = "ready" | "status" | "int" | "text" | "mono" | "age" | "usage";

interface CellColumn {
  label: string;
  type: CellKind;
  width: number | string;
  /** Overrides the default (sortable for status and int cells). */
  sortable?: boolean;
  /**
   * Sort a status cell by its own label instead of the row status. Needed when the cell label
   * differs from the row's `status_reason` (Event: cell = type, status_reason = reason).
   */
  sortByCell?: boolean;
  hotAt?: number;
}

function cellColumn(index: number, col: CellColumn): ResourceColumn {
  const sortable = col.sortable ?? (col.type === "status" || col.type === "int");
  return {
    key: `cell-${String(index)}`,
    label: col.label,
    source: { type: "cell", index },
    // A status cell normally shows the row's status_reason, which the backend sorts as a plain
    // string (SortField "status"). Cells whose label differs from status_reason sort by the cell.
    sortField: !sortable
      ? null
      : col.type === "status" && col.sortByCell !== true
        ? { type: "status" }
        : { type: "cell", index },
    width: typeof col.width === "number" ? `${String(col.width)}px` : col.width,
    mono: col.type !== "text" && col.type !== "status" && col.type !== "usage",
    ...(col.hotAt === undefined ? {} : { hotAt: col.hotAt }),
    ...(col.type === "status" ? { pinned: true } : {}),
  };
}

const ready = (label = "Ready"): CellColumn => ({ label, type: "ready", width: 72 });
const status = (label = "Status"): CellColumn => ({ label, type: "status", width: 150 });
const age: CellColumn = { label: "Age", type: "age", width: 170 };
// Usage cells show a value, a %-of-request/limit hint and a bar (mockup columns are 120 px).
const cpu: CellColumn = { label: "CPU", type: "usage", width: 120, sortable: true };
const memory: CellColumn = { label: "Memory", type: "usage", width: 120, sortable: true };

interface KindLayout {
  clusterScoped?: boolean;
  cells: CellColumn[];
}

const KIND_LAYOUTS: Record<string, KindLayout> = {
  pod: {
    cells: [
      { label: "Ready", type: "ready", width: 64 },
      { label: "Status", type: "status", width: 130 },
      { label: "Restarts", type: "int", width: 76, hotAt: RESTARTS_HOT },
      { label: "Last restart", type: "age", width: 96, sortable: true },
      { label: "Last reason", type: "text", width: 130 },
      { label: "Node", type: "text", width: 120 },
      { label: "IP", type: "mono", width: 110 },
      { label: "Age", type: "age", width: 140 },
      cpu,
      memory,
    ],
  },
  deployment: {
    cells: [
      ready(),
      { label: "Up-to-date", type: "int", width: 90 },
      { label: "Available", type: "int", width: 90 },
      status(),
      age,
    ],
  },
  statefulset: { cells: [ready(), status(), age] },
  daemonset: { cells: [ready(), status(), age] },
  job: { cells: [ready("Completions"), status(), age] },
  cronjob: { cells: [{ label: "Schedule", type: "mono", width: 130 }, status(), age] },
  ingress: { cells: [{ label: "Class", type: "text", width: 120 }, status(), age] },
  persistentvolumeclaim: {
    cells: [
      status(),
      { label: "Volume", type: "mono", width: 200 },
      { label: "Storage class", type: "text", width: 130 },
      age,
    ],
  },
  service: {
    cells: [
      { label: "Type", type: "text", width: 110 },
      { label: "Cluster-IP", type: "mono", width: 130 },
      age,
    ],
  },
  node: { clusterScoped: true, cells: [status(), age, cpu, memory] },
  namespace: { clusterScoped: true, cells: [status(), age] },
  configmap: { cells: [{ label: "Data", type: "int", width: 64 }, age] },
  secret: {
    cells: [
      { label: "Type", type: "text", width: 200 },
      { label: "Data", type: "int", width: 64 },
      age,
    ],
  },
  event: {
    cells: [
      { ...status("Type"), sortByCell: true },
      { label: "Reason", type: "text", width: 140 },
      { label: "Message", type: "text", width: "minmax(200px, 3fr)" },
      age,
    ],
  },
  // Workloads & Discovery (04-05a)
  replicaset: {
    cells: [ready(), { label: "Current", type: "int", width: 80 }, status(), age],
  },
  endpointslice: {
    cells: [
      { label: "Address type", type: "text", width: 110 },
      { label: "Ports", type: "text", width: 140 },
      { label: "Endpoints", type: "text", width: 140 },
      status(),
      age,
    ],
  },
  horizontalpodautoscaler: {
    cells: [
      { label: "Reference", type: "text", width: 180 },
      { label: "Min pods", type: "int", width: 80 },
      { label: "Max pods", type: "int", width: 80 },
      { label: "Replicas", type: "int", width: 80 },
      status(),
      age,
    ],
  },
  poddisruptionbudget: {
    cells: [
      { label: "Min available", type: "text", width: 100 },
      { label: "Max unavailable", type: "text", width: 110 },
      { label: "Allowed", type: "int", width: 80 },
      { label: "Current", type: "int", width: 80 },
      { label: "Desired", type: "int", width: 80 },
      status(),
      age,
    ],
  },
  // Quotas & Scheduling (04-05a)
  resourcequota: { cells: [status(), age] },
  limitrange: { cells: [status(), age] },
  priorityclass: {
    clusterScoped: true,
    cells: [
      { label: "Value", type: "int", width: 90 },
      { label: "Global default", type: "text", width: 110 },
      { label: "Preemption policy", type: "text", width: 160 },
      status(),
      age,
    ],
  },
  runtimeclass: {
    clusterScoped: true,
    cells: [{ label: "Handler", type: "text", width: 160 }, status(), age],
  },
  lease: {
    cells: [
      { label: "Holder", type: "text", width: 220 },
      { label: "Duration", type: "int", width: 90 },
      status(),
      age,
    ],
  },
  // Storage (04-05a)
  persistentvolume: {
    clusterScoped: true,
    cells: [
      { label: "Capacity", type: "text", width: 90 },
      { label: "Access modes", type: "text", width: 120 },
      { label: "Reclaim policy", type: "text", width: 120 },
      status(),
      { label: "Claim", type: "text", width: 180 },
      { label: "Storage class", type: "text", width: 130 },
      age,
    ],
  },
  storageclass: {
    clusterScoped: true,
    cells: [
      { label: "Provisioner", type: "text", width: 200 },
      { label: "Reclaim", type: "text", width: 110 },
      { label: "Binding mode", type: "text", width: 140 },
      { label: "Allow expansion", type: "text", width: 120 },
      status(),
      age,
    ],
  },
  volumeattachment: {
    clusterScoped: true,
    cells: [
      { label: "Attacher", type: "text", width: 180 },
      { label: "Node", type: "text", width: 140 },
      { label: "PV", type: "text", width: 180 },
      { label: "Attached", type: "text", width: 90 },
      status(),
      age,
    ],
  },
  // Access Control (RBAC) (04-05a)
  serviceaccount: {
    cells: [{ label: "Secrets", type: "int", width: 70 }, status(), age],
  },
  role: { cells: [status(), age] },
  clusterrole: { clusterScoped: true, cells: [status(), age] },
  rolebinding: {
    cells: [
      { label: "Role", type: "text", width: 180 },
      { label: "Subjects", type: "text", width: 220 },
      status(),
      age,
    ],
  },
  clusterrolebinding: {
    clusterScoped: true,
    cells: [
      { label: "Role", type: "text", width: 180 },
      { label: "Subjects", type: "text", width: 220 },
      status(),
      age,
    ],
  },
  // Policy & Admission (04-05a)
  networkpolicy: {
    cells: [{ label: "Pod selector", type: "text", width: 200 }, status(), age],
  },
  mutatingwebhookconfiguration: {
    clusterScoped: true,
    cells: [{ label: "Webhooks", type: "int", width: 90 }, status(), age],
  },
  validatingwebhookconfiguration: {
    clusterScoped: true,
    cells: [{ label: "Webhooks", type: "int", width: 90 }, status(), age],
  },
  validatingadmissionpolicy: {
    clusterScoped: true,
    cells: [status(), age],
  },
  validatingadmissionpolicybinding: {
    clusterScoped: true,
    cells: [{ label: "Policy", type: "text", width: 200 }, status(), age],
  },
  // Ecosystem resources (04-05b)
  gateway: {
    cells: [status(), { label: "Addresses", type: "text", width: 220 }, age],
  },
  httproute: {
    cells: [status(), { label: "Parents", type: "text", width: 220 }, age],
  },
  grpcroute: {
    cells: [status(), { label: "Parents", type: "text", width: 220 }, age],
  },
  gatewayclass: {
    clusterScoped: true,
    cells: [status(), { label: "Controller", type: "text", width: 220 }, age],
  },
  application: {
    cells: [status("Health"), { label: "Sync", type: "text", width: 120 }, age],
  },
  appproject: {
    cells: [status(), age],
  },
  kustomization: {
    cells: [
      status(),
      { label: "Reason", type: "text", width: 200 },
      { label: "Suspended", type: "text", width: 90 },
      age,
    ],
  },
  helmrelease: {
    cells: [
      status(),
      { label: "Reason", type: "text", width: 200 },
      { label: "Suspended", type: "text", width: 90 },
      age,
    ],
  },
  nodepool: {
    clusterScoped: true,
    cells: [status(), { label: "Reason", type: "text", width: 180 }, age],
  },
  nodeclaim: {
    clusterScoped: true,
    cells: [status(), { label: "Reason", type: "text", width: 180 }, age],
  },
  certificate: {
    cells: [
      status(),
      { label: "Ready", type: "text", width: 70 },
      { label: "Secret", type: "mono", width: 160 },
      { label: "Issuer", type: "text", width: 160 },
      { label: "Expiry", type: "text", width: 140 },
      age,
    ],
  },
  certificaterequest: {
    cells: [
      status(),
      { label: "Approved", type: "text", width: 80 },
      { label: "Ready", type: "text", width: 70 },
      { label: "Issuer", type: "text", width: 160 },
      age,
    ],
  },
  issuer: {
    cells: [status(), { label: "Ready", type: "text", width: 70 }, { label: "Type", type: "text", width: 120 }, age],
  },
  clusterissuer: {
    clusterScoped: true,
    cells: [status(), { label: "Ready", type: "text", width: 70 }, { label: "Type", type: "text", width: 120 }, age],
  },
  externalsecret: {
    cells: [
      status(),
      { label: "Store", type: "text", width: 150 },
      { label: "Target", type: "mono", width: 150 },
      { label: "Refresh", type: "text", width: 140 },
      age,
    ],
  },
  secretstore: {
    cells: [status(), { label: "Provider", type: "text", width: 120 }, age],
  },
  clustersecretstore: {
    clusterScoped: true,
    cells: [status(), { label: "Provider", type: "text", width: 120 }, age],
  },
  rollout: {
    cells: [
      status(),
      { label: "Strategy", type: "text", width: 100 },
      { label: "Step", type: "text", width: 80 },
      { label: "Replicas", type: "text", width: 90 },
      age,
    ],
  },
  analysisrun: {
    cells: [status(), { label: "Phase", type: "text", width: 110 }, age],
  },
};

/** Columns for a kind the map does not know, derived from the cells of a returned row. */
function derivedCells(sample: Row | undefined): CellColumn[] {
  // Generic projection (`project_dynamic_object`): status, then creation timestamp.
  if (sample === undefined || sample.cells.length === 0) return [status(), age];
  return sample.cells.map((cell, i) => {
    switch (cell.type) {
      case "status":
        return status();
      case "ready":
        return ready();
      case "timestamp":
        return age;
      case "int":
        return { label: `Value ${String(i + 1)}`, type: "int", width: 90 };
      case "text":
        return { label: `Value ${String(i + 1)}`, type: "text", width: 160 };
      case "usage":
        return { label: `Usage ${String(i + 1)}`, type: "usage", width: 90, sortable: true };
    }
  });
}

/** Whether `kind` has a known column layout (otherwise columns follow the returned cells). */
export function hasKnownLayout(kind: string): boolean {
  return KIND_LAYOUTS[kind.toLowerCase()] !== undefined;
}

export function columnsForKind(kind: string, sample?: Row): ResourceColumn[] {
  const layout = KIND_LAYOUTS[kind.toLowerCase()];
  const cells = layout?.cells ?? derivedCells(sample);
  const base = layout?.clusterScoped ? [CLUSTER, NAME] : [CLUSTER, NAMESPACE, NAME];
  return [...base, ...cells.map((c, i) => cellColumn(i, c))];
}

export function gridTemplate(columns: readonly ResourceColumn[]): string {
  return columns.map((c) => c.width).join(" ");
}

/** English plural of a kind name: `Deployment` → `Deployments`, `Ingress` → `Ingresses`. */
export function kindPlural(kind: string): string {
  if (/(s|x|z|ch|sh)$/i.test(kind)) return `${kind}es`;
  if (/[^aeiou]y$/i.test(kind)) return `${kind.slice(0, -1)}ies`;
  return `${kind}s`;
}

/** Lower-case plural noun for UI copy, kubectl-style: `deployments`, `statefulsets`. */
export function kindNoun(kind: string): string {
  return kindPlural(kind).toLowerCase();
}
