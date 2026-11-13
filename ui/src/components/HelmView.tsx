import { useEffect, useState, type KeyboardEvent } from "react";
import {
  commands,
  type HelmReleasePage,
  type HelmReleaseSummary,
  type HelmRevisionDiff,
} from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";

export interface HelmRelease {
  name: string;
  namespace: string;
  revision: number | string;
  status: string;
  chart: string;
  appVersion: string;
  updated: string;
  values?: string | Record<string, unknown>;
  manifest?: string;
  notes?: string;
}

export interface HelmViewProps {
  cluster?: string | null;
  namespace?: string | null;
  releases?: HelmRelease[];
  selectedRelease?: HelmRelease | null;
  onSelectRelease?: (release: HelmRelease | null) => void;
  fetchReleases?: (
    cluster: string,
    namespace: string | null,
    offset: number,
    limit: number,
  ) => Promise<HelmReleasePage | HelmReleaseSummary[]>;
  fetchHistory?: (
    cluster: string,
    namespace: string,
    name: string,
  ) => Promise<HelmReleaseSummary[]>;
  fetchDiff?: (
    cluster: string,
    namespace: string,
    name: string,
    revA: number,
    revB: number,
  ) => Promise<HelmRevisionDiff>;
  previewRollback?: (
    namespace: string,
    name: string,
    revision: number,
  ) => Promise<string>;
}

function toYamlString(val: unknown, indent = 0): string {
  if (typeof val === "string") return val;
  if (typeof val === "number" || typeof val === "boolean") return String(val);
  if (val === null || val === undefined) return "";
  if (typeof val !== "object") return "";

  const spaces = " ".repeat(indent);
  if (Array.isArray(val)) {
    return val
      .map((item) => {
        if (typeof item === "object" && item !== null) {
          const inner = toYamlString(item, indent + 2).trimStart();
          return `${spaces}- ${inner}`;
        }
        if (typeof item === "string") {
          return `${spaces}- ${item}`;
        }
        if (typeof item === "number" || typeof item === "boolean") {
          return `${spaces}- ${String(item)}`;
        }
        return `${spaces}- `;
      })
      .join("\n");
  }

  return Object.entries(val as Record<string, unknown>)
    .map(([k, v]) => {
      if (typeof v === "object" && v !== null) {
        return `${spaces}${k}:\n${toYamlString(v, indent + 2)}`;
      }
      if (typeof v === "string") {
        return `${spaces}${k}: ${v}`;
      }
      if (typeof v === "number" || typeof v === "boolean") {
        return `${spaces}${k}: ${String(v)}`;
      }
      return `${spaces}${k}: `;
    })
    .join("\n");
}

type DetailTab = "values" | "manifest" | "notes" | "history" | "diff";

interface ReleasesState {
  key: string | null;
  releases: HelmRelease[];
  error: string | null;
}

interface HistoryState {
  key: string | null;
  items: HelmReleaseSummary[];
  error: string | null;
}

const defaultFetchReleases = async (
  cluster: string,
  namespace: string | null,
  offset: number,
  limit: number,
): Promise<HelmReleasePage> => {
  const res = await commands.listHelmReleases(cluster, namespace, offset, limit);
  if (res.status === "error") {
    throw new Error(res.error.message);
  }
  return res.data;
};

const defaultFetchHistory = async (
  cluster: string,
  namespace: string,
  name: string,
): Promise<HelmReleaseSummary[]> => {
  const res = await commands.getHelmReleaseHistory(cluster, namespace, name);
  if (res.status === "error") {
    throw new Error(res.error.message);
  }
  return res.data;
};

const defaultFetchDiff = async (
  cluster: string,
  namespace: string,
  name: string,
  revA: number,
  revB: number,
): Promise<HelmRevisionDiff> => {
  const res = await commands.diffHelmRevisions(cluster, namespace, name, revA, revB);
  if (res.status === "error") {
    throw new Error(res.error.message);
  }
  return res.data;
};

const defaultPreviewRollback = async (
  namespace: string,
  name: string,
  revision: number,
): Promise<string> => {
  const res = await commands.previewHelmRollback(namespace, name, revision);
  if (res.status === "error") {
    throw new Error(res.error.message);
  }
  return res.data;
};

export function HelmView({
  cluster: clusterProp,
  namespace: namespaceProp,
  releases: controlledReleases,
  selectedRelease: controlledSelected,
  onSelectRelease,
  fetchReleases = defaultFetchReleases,
  fetchHistory = defaultFetchHistory,
  fetchDiff = defaultFetchDiff,
  previewRollback = defaultPreviewRollback,
}: HelmViewProps) {
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const activeClusters = useContextsStore((s) => s.activeClusterSelection);
  const activeCluster = clusterProp ?? activeClusters[0] ?? selectedContext ?? null;
  const effectiveNamespace = namespaceProp ?? null;

  const isControlled = controlledReleases !== undefined;
  const releasesQueryKey = isControlled ? null : `${activeCluster ?? ""}:${effectiveNamespace ?? ""}`;

  const [releasesState, setReleasesState] = useState<ReleasesState>({
    key: null,
    releases: [],
    error: null,
  });

  const [internalSelected, setInternalSelected] = useState<HelmRelease | null>(null);
  const [activeTab, setActiveTab] = useState<DetailTab>("values");

  // History state
  const selected = controlledSelected !== undefined ? controlledSelected : internalSelected;
  const historyKey =
    selected && (activeTab === "history" || activeTab === "diff")
      ? `${activeCluster ?? ""}:${selected.namespace}:${selected.name}`
      : null;

  const [historyState, setHistoryState] = useState<HistoryState>({
    key: null,
    items: [],
    error: null,
  });
  const [rollbackPreview, setRollbackPreview] = useState<string | null>(null);

  // Diff state
  const [diffRevA, setDiffRevA] = useState<number | null>(null);
  const [diffRevB, setDiffRevB] = useState<number | null>(null);
  const [diffResult, setDiffResult] = useState<HelmRevisionDiff | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);
  const [diffError, setDiffError] = useState<string | null>(null);

  const loading = !isControlled && Boolean(activeCluster) && releasesState.key !== releasesQueryKey;
  const error = releasesState.key === releasesQueryKey ? releasesState.error : null;
  const fetchedReleases = releasesState.key === releasesQueryKey ? releasesState.releases : [];
  const releases = isControlled ? controlledReleases : fetchedReleases;

  const historyLoading = historyKey !== null && historyState.key !== historyKey;
  const historyError = historyState.key === historyKey ? historyState.error : null;
  const historyList = historyState.key === historyKey ? historyState.items : [];

  // Load releases when cluster / namespace changes if not controlled
  useEffect(() => {
    if (isControlled || !activeCluster) return;
    let cancelled = false;

    fetchReleases(activeCluster, effectiveNamespace, 0, 100)
      .then((data) => {
        if (cancelled) return;
        const list: HelmReleaseSummary[] = Array.isArray(data) ? data : data.releases;
        setReleasesState({
          key: releasesQueryKey,
          releases: list.map((r) => ({
            name: r.name,
            namespace: r.namespace,
            revision: r.revision,
            status: r.status,
            chart: r.chart,
            appVersion: r.appVersion,
            updated: r.updated,
          })),
          error: null,
        });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setReleasesState({
          key: releasesQueryKey,
          releases: [],
          error: err instanceof Error ? err.message : String(err),
        });
      });

    return () => {
      cancelled = true;
    };
  }, [isControlled, activeCluster, effectiveNamespace, fetchReleases, releasesQueryKey]);

  // Load history when tab is 'history' or 'diff'
  useEffect(() => {
    if (!selected || (activeTab !== "history" && activeTab !== "diff")) return;
    const cluster = activeCluster ?? selectedContext ?? "";
    if (!cluster) return;

    let cancelled = false;
    fetchHistory(cluster, selected.namespace, selected.name)
      .then((items) => {
        if (cancelled) return;
        setHistoryState({
          key: historyKey,
          items,
          error: null,
        });
        if (items.length >= 2 && diffRevA === null && diffRevB === null) {
          setDiffRevA(items[1]?.revision ?? null);
          setDiffRevB(items[0]?.revision ?? null);
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setHistoryState({
          key: historyKey,
          items: [],
          error: err instanceof Error ? err.message : String(err),
        });
      });

    return () => {
      cancelled = true;
    };
  }, [selected, activeTab, activeCluster, selectedContext, fetchHistory, historyKey, diffRevA, diffRevB]);

  const handleSelect = (rel: HelmRelease) => {
    setInternalSelected(rel);
    setRollbackPreview(null);
    setDiffResult(null);
    setDiffRevA(null);
    setDiffRevB(null);
    onSelectRelease?.(rel);
  };

  const handleRowKeyDown = (e: KeyboardEvent<HTMLTableRowElement>, rel: HelmRelease) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      handleSelect(rel);
    }
  };

  const handlePreviewRollback = async (rev: number) => {
    if (!selected) return;
    try {
      const preview = await previewRollback(selected.namespace, selected.name, rev);
      setRollbackPreview(preview);
    } catch (err: unknown) {
      setRollbackPreview(`Error: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handleRunDiff = async () => {
    if (!selected || diffRevA === null || diffRevB === null) return;
    const cluster = activeCluster ?? selectedContext ?? "";
    if (!cluster) return;

    setDiffLoading(true);
    setDiffError(null);
    try {
      const res = await fetchDiff(cluster, selected.namespace, selected.name, diffRevA, diffRevB);
      setDiffResult(res);
    } catch (err: unknown) {
      setDiffError(err instanceof Error ? err.message : String(err));
    } finally {
      setDiffLoading(false);
    }
  };

  const emptyText = activeCluster
    ? `No Helm releases found in ${activeCluster}/${effectiveNamespace || "all"}`
    : "No Helm releases found";

  return (
    <div className="helm-view" aria-label="Helm Releases" data-testid="helm-view">
      {loading && (
        <div role="status" className="helm-view__loading" data-testid="helm-loading">
          Loading Helm releases…
        </div>
      )}
      {error !== null && (
        <div role="alert" className="error-state helm-view__error" data-testid="helm-error">
          Error loading Helm releases: {error}
        </div>
      )}

      <div className="helm-view__table-container">
        <table className="helm-table" aria-label="Helm Releases List">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Namespace</th>
              <th scope="col">Revision</th>
              <th scope="col">Status</th>
              <th scope="col">Chart</th>
              <th scope="col">App Version</th>
              <th scope="col">Updated</th>
            </tr>
          </thead>
          <tbody>
            {!loading && releases.length === 0 ? (
              <tr>
                <td colSpan={7} className="empty-state" data-testid="helm-empty-state">
                  {emptyText}
                </td>
              </tr>
            ) : (
              releases.map((rel) => {
                const isSelected =
                  selected !== null &&
                  selected.name === rel.name &&
                  selected.namespace === rel.namespace;
                return (
                  <tr
                    key={`${rel.namespace}/${rel.name}`}
                    className="helm-table__row"
                    data-selected={isSelected ? "true" : undefined}
                    aria-selected={isSelected}
                    tabIndex={0}
                    onClick={() => {
                      handleSelect(rel);
                    }}
                    onKeyDown={(e) => {
                      handleRowKeyDown(e, rel);
                    }}
                  >
                    <td className="helm-table__cell helm-table__cell--name">{rel.name}</td>
                    <td className="helm-table__cell">{rel.namespace}</td>
                    <td className="helm-table__cell">{rel.revision}</td>
                    <td className="helm-table__cell">
                      <span className={`badge badge--status badge--status-${rel.status.toLowerCase()}`}>
                        {rel.status}
                      </span>
                    </td>
                    <td className="helm-table__cell">{rel.chart}</td>
                    <td className="helm-table__cell">{rel.appVersion}</td>
                    <td className="helm-table__cell">{rel.updated}</td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>

      {selected && (
        <section
          className="helm-detail"
          aria-label={`Details for ${selected.name}`}
          data-testid="helm-detail-panel"
        >
          <div className="helm-detail__header">
            <div className="helm-detail__title-group">
              <h3 className="helm-detail__title">
                {selected.name}
                <span className="helm-detail__revision"> (rev {selected.revision})</span>
              </h3>
              <span className="badge badge--tag">{selected.namespace}</span>
              <span className={`badge badge--status badge--status-${selected.status.toLowerCase()}`}>
                {selected.status}
              </span>
            </div>
            <div className="helm-detail__meta">
              <span>Chart: {selected.chart}</span>
              <span>App: {selected.appVersion}</span>
              <span>Updated: {selected.updated}</span>
            </div>
          </div>

          <div role="tablist" className="helm-detail__tabs" aria-label="Release details">
            <button
              type="button"
              role="tab"
              id="tab-values"
              aria-controls="panel-values"
              aria-selected={activeTab === "values"}
              className="helm-detail__tab"
              onClick={() => {
                setActiveTab("values");
              }}
            >
              Values
            </button>
            <button
              type="button"
              role="tab"
              id="tab-manifest"
              aria-controls="panel-manifest"
              aria-selected={activeTab === "manifest"}
              className="helm-detail__tab"
              onClick={() => {
                setActiveTab("manifest");
              }}
            >
              Manifest
            </button>
            <button
              type="button"
              role="tab"
              id="tab-notes"
              aria-controls="panel-notes"
              aria-selected={activeTab === "notes"}
              className="helm-detail__tab"
              onClick={() => {
                setActiveTab("notes");
              }}
            >
              Notes
            </button>
            <button
              type="button"
              role="tab"
              id="tab-history"
              aria-controls="panel-history"
              aria-selected={activeTab === "history"}
              className="helm-detail__tab"
              onClick={() => {
                setActiveTab("history");
              }}
            >
              History
            </button>
            <button
              type="button"
              role="tab"
              id="tab-diff"
              aria-controls="panel-diff"
              aria-selected={activeTab === "diff"}
              className="helm-detail__tab"
              onClick={() => {
                setActiveTab("diff");
              }}
            >
              Diff
            </button>
          </div>

          <div
            role="tabpanel"
            id="panel-values"
            aria-labelledby="tab-values"
            className="helm-detail__panel"
            hidden={activeTab !== "values"}
            data-testid="helm-panel-values"
          >
            <pre className="helm-detail__code" data-testid="helm-values-content">
              {toYamlString(selected.values) || "(No values recorded)"}
            </pre>
          </div>

          <div
            role="tabpanel"
            id="panel-manifest"
            aria-labelledby="tab-manifest"
            className="helm-detail__panel"
            hidden={activeTab !== "manifest"}
            data-testid="helm-panel-manifest"
          >
            <pre className="helm-detail__code" data-testid="helm-manifest-content">
              {selected.manifest || "(No manifest recorded)"}
            </pre>
          </div>

          <div
            role="tabpanel"
            id="panel-notes"
            aria-labelledby="tab-notes"
            className="helm-detail__panel"
            hidden={activeTab !== "notes"}
            data-testid="helm-panel-notes"
          >
            <pre className="helm-detail__code" data-testid="helm-notes-content">
              {selected.notes || "(No notes recorded)"}
            </pre>
          </div>

          <div
            role="tabpanel"
            id="panel-history"
            aria-labelledby="tab-history"
            className="helm-detail__panel"
            hidden={activeTab !== "history"}
            data-testid="helm-panel-history"
          >
            {historyLoading && <p className="loading-state">Loading release history…</p>}
            {historyError && <p className="error-state">{historyError}</p>}
            {!historyLoading && !historyError && historyList.length === 0 && (
              <p className="empty-state">No revision history found.</p>
            )}
            {!historyLoading && historyList.length > 0 && (
              <div className="helm-history">
                <table className="helm-table helm-table--history" aria-label="Revision History">
                  <thead>
                    <tr>
                      <th scope="col">Revision</th>
                      <th scope="col">Status</th>
                      <th scope="col">Chart</th>
                      <th scope="col">App Version</th>
                      <th scope="col">Updated</th>
                      <th scope="col">Action</th>
                    </tr>
                  </thead>
                  <tbody>
                    {historyList.map((item) => (
                      <tr key={item.revision} className="helm-table__row">
                        <td className="helm-table__cell">rev {item.revision}</td>
                        <td className="helm-table__cell">
                          <span
                            className={`badge badge--status badge--status-${item.status.toLowerCase()}`}
                          >
                            {item.status}
                          </span>
                        </td>
                        <td className="helm-table__cell">{item.chart}</td>
                        <td className="helm-table__cell">{item.appVersion}</td>
                        <td className="helm-table__cell">{item.updated}</td>
                        <td className="helm-table__cell">
                          <button
                            type="button"
                            className="btn btn--sm"
                            aria-label={`Preview rollback to rev ${String(item.revision)}`}
                            onClick={() => {
                              void handlePreviewRollback(item.revision);
                            }}
                          >
                            Preview Rollback
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
            {rollbackPreview && (
              <div className="helm-rollback-preview">
                <h4>Rollback Command Preview</h4>
                <pre className="helm-detail__code" data-testid="helm-rollback-preview">
                  {rollbackPreview}
                </pre>
              </div>
            )}
          </div>

          <div
            role="tabpanel"
            id="panel-diff"
            aria-labelledby="tab-diff"
            className="helm-detail__panel"
            hidden={activeTab !== "diff"}
            data-testid="helm-panel-diff"
          >
            <div className="helm-diff-controls">
              <label>
                Revision A:
                <select
                  aria-label="Revision A"
                  value={diffRevA ?? ""}
                  onChange={(e) => {
                    setDiffRevA(Number(e.target.value));
                  }}
                >
                  <option value="">Select rev</option>
                  {historyList.map((h) => (
                    <option key={`a-${String(h.revision)}`} value={h.revision}>
                      rev {h.revision} ({h.chart})
                    </option>
                  ))}
                </select>
              </label>
              <label>
                Revision B:
                <select
                  aria-label="Revision B"
                  value={diffRevB ?? ""}
                  onChange={(e) => {
                    setDiffRevB(Number(e.target.value));
                  }}
                >
                  <option value="">Select rev</option>
                  {historyList.map((h) => (
                    <option key={`b-${String(h.revision)}`} value={h.revision}>
                      rev {h.revision} ({h.chart})
                    </option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                className="btn btn--sm btn--primary"
                disabled={diffRevA === null || diffRevB === null || diffLoading}
                onClick={() => {
                  void handleRunDiff();
                }}
              >
                {diffLoading ? "Diffing…" : "Diff Revisions"}
              </button>
            </div>
            {diffError && <p className="error-state">{diffError}</p>}
            {diffResult && (
              <div className="helm-diff-results" data-testid="helm-diff-results">
                <h4>Values Diff</h4>
                <pre className="helm-detail__code" data-testid="helm-diff-values">
                  {diffResult.valuesDiff || "(No values diff)"}
                </pre>
                <h4>Manifest Diff</h4>
                <pre className="helm-detail__code" data-testid="helm-diff-manifest">
                  {diffResult.manifestDiff || "(No manifest diff)"}
                </pre>
              </div>
            )}
          </div>
        </section>
      )}
    </div>
  );
}
