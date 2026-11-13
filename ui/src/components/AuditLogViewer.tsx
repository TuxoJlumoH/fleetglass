import { useEffect, useState } from "react";
import {
  commands,
  type AuditEntry,
  type AuditLogPage,
} from "../generated/bindings";

const PAGE_LIMIT = 100;
const FILTER_DEBOUNCE_MS = 150;

export interface AuditLogViewerProps {
  fetchAuditLog?: (
    offset: number,
    limit: number,
    cluster: string | null,
    action: string | null,
  ) => Promise<AuditLogPage>;
  onClose?: () => void;
  role?: "region" | "dialog";
}

async function defaultFetchAuditLog(
  offset: number,
  limit: number,
  cluster: string | null,
  action: string | null,
): Promise<AuditLogPage> {
  const res = await commands.getAuditLog(offset, limit, cluster, action);
  if (res.status === "error") {
    throw new Error(res.error.message || "Failed to fetch audit log", { cause: res.error });
  }
  return res.data;
}

interface WindowState {
  key: string | null;
  entries: AuditEntry[];
  total: number;
  error: string | null;
}

const EMPTY_WINDOW: WindowState = { key: null, entries: [], total: 0, error: null };

function errorMessage(err: unknown): string {
  if (typeof err === "object" && err !== null && "message" in err) {
    return String(err.message);
  }
  return String(err);
}

function formatLocalTime(isoString: string): string {
  try {
    const d = new Date(isoString);
    if (Number.isNaN(d.getTime())) return isoString;
    return d.toLocaleString();
  } catch {
    return isoString;
  }
}

export function AuditLogViewer({
  fetchAuditLog = defaultFetchAuditLog,
  onClose,
  role = "region",
}: AuditLogViewerProps) {
  const [clusterInput, setClusterInput] = useState("");
  const [actionInput, setActionInput] = useState("");
  const [clusterFilter, setClusterFilter] = useState("");
  const [actionFilter, setActionFilter] = useState("");
  const [refreshKey, setRefreshKey] = useState(0);

  const [result, setResult] = useState<WindowState>(EMPTY_WINDOW);
  const [loadingMore, setLoadingMore] = useState(false);
  const [copiedId, setCopiedId] = useState<number | null>(null);

  const queryKey = JSON.stringify([clusterFilter, actionFilter, refreshKey]);
  const loading = result.key !== queryKey;
  const { entries, total, error } = loading ? EMPTY_WINDOW : result;

  useEffect(() => {
    const timer = setTimeout(() => {
      setClusterFilter(clusterInput.trim());
    }, FILTER_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [clusterInput]);

  useEffect(() => {
    const timer = setTimeout(() => {
      setActionFilter(actionInput.trim());
    }, FILTER_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [actionInput]);

  useEffect(() => {
    let cancelled = false;
    const clusterArg = clusterFilter || null;
    const actionArg = actionFilter || null;

    fetchAuditLog(0, PAGE_LIMIT, clusterArg, actionArg).then(
      (page) => {
        if (!cancelled) {
          setResult({
            key: queryKey,
            entries: page.entries,
            total: page.total,
            error: null,
          });
        }
      },
      (err: unknown) => {
        if (!cancelled) {
          setResult({
            ...EMPTY_WINDOW,
            key: queryKey,
            error: errorMessage(err),
          });
        }
      },
    );

    return () => {
      cancelled = true;
    };
  }, [fetchAuditLog, queryKey, clusterFilter, actionFilter]);

  useEffect(() => {
    if (copiedId === null) return;
    const timer = setTimeout(() => {
      setCopiedId(null);
    }, 2000);
    return () => {
      clearTimeout(timer);
    };
  }, [copiedId]);

  const handleRefresh = () => {
    setRefreshKey((k) => k + 1);
  };

  const handleLoadMore = () => {
    if (loadingMore || entries.length >= total) return;
    setLoadingMore(true);

    const clusterArg = clusterFilter || null;
    const actionArg = actionFilter || null;

    fetchAuditLog(entries.length, PAGE_LIMIT, clusterArg, actionArg).then(
      (page) => {
        setResult((prev) =>
          prev.key === queryKey
            ? { ...prev, entries: [...prev.entries, ...page.entries], total: page.total }
            : prev,
        );
        setLoadingMore(false);
      },
      (err: unknown) => {
        setResult((prev) =>
          prev.key === queryKey ? { ...prev, error: errorMessage(err) } : prev,
        );
        setLoadingMore(false);
      },
    );
  };

  const handleCopy = async (kubectl: string, id: number) => {
    try {
      await navigator.clipboard.writeText(kubectl);
    } catch {
      // Fallback / ignore failure in unprivileged or testing environment
    }
    setCopiedId(id);
  };

  return (
    <div
      className="audit-log-viewer"
      role={role}
      aria-label="Audit log"
      data-testid="audit-log-viewer"
    >
      <div className="audit-log__toolbar">
        <input
          type="search"
          aria-label="Filter by cluster"
          placeholder="Filter by cluster…"
          className="audit-log__input"
          value={clusterInput}
          onChange={(e) => {
            setClusterInput(e.target.value);
          }}
        />
        <input
          type="search"
          aria-label="Filter by action"
          placeholder="Filter by action…"
          className="audit-log__input"
          value={actionInput}
          onChange={(e) => {
            setActionInput(e.target.value);
          }}
        />
        <button
          type="button"
          aria-label="Refresh audit log"
          className="audit-log__btn audit-log__btn--refresh"
          onClick={handleRefresh}
          disabled={loading}
        >
          Refresh
        </button>
        <div className="audit-log__stats">
          <span>
            {String(entries.length)} of {String(total)} events
          </span>
        </div>
        {onClose && (
          <button
            type="button"
            aria-label="Close audit log"
            className="audit-log__close-btn"
            onClick={onClose}
          >
            ×
          </button>
        )}
      </div>

      <div className="audit-log__content">
        {error !== null ? (
          <div className="audit-log__error" role="alert" data-testid="audit-error">
            <p>Failed to load audit log: {error}</p>
          </div>
        ) : loading ? (
          <div className="audit-log__status" role="status" data-testid="audit-loading">
            <p>Loading audit log…</p>
          </div>
        ) : entries.length === 0 ? (
          <div className="audit-log__empty" role="status" data-testid="audit-empty">
            <p>No audit events recorded</p>
          </div>
        ) : (
          <div className="audit-log__table-container">
            <table className="audit-log__table" aria-label="Audit log records">
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">Cluster</th>
                  <th scope="col">User</th>
                  <th scope="col">Action</th>
                  <th scope="col">Resource</th>
                  <th scope="col">Status</th>
                  <th scope="col">Kubectl command</th>
                </tr>
              </thead>
              <tbody>
                {entries.map((entry) => (
                  <tr key={entry.id} className="audit-log__row" data-testid={`audit-row-${String(entry.id)}`}>
                    <td className="audit-log__cell-time">{formatLocalTime(entry.timestamp)}</td>
                    <td className="audit-log__cell-cluster">
                      <span className="audit-badge audit-badge--cluster">{entry.cluster}</span>
                    </td>
                    <td className="audit-log__cell-user">{entry.user || "-"}</td>
                    <td className="audit-log__cell-action">
                      <span className={`audit-pill audit-pill--action audit-pill--${entry.action}`}>
                        {entry.action}
                      </span>
                    </td>
                    <td className="audit-log__cell-resource">
                      <span className="audit-resource">
                        <span className="audit-resource__name">
                          {`${entry.kind}/${entry.name}`}
                        </span>
                        {entry.namespace ? (
                          <span
                            className="audit-resource__namespace"
                            title={`Namespace: ${entry.namespace}`}
                          >
                            {" "}({entry.namespace})
                          </span>
                        ) : null}
                      </span>
                    </td>
                    <td className="audit-log__cell-status">
                      {entry.success ? (
                        <span className="audit-status audit-status--success">Success</span>
                      ) : (
                        <span
                          className="audit-status audit-status--failed"
                          title={entry.error || "Failed"}
                        >
                          Failed
                        </span>
                      )}
                    </td>
                    <td className="audit-log__cell-kubectl">
                      <div className="audit-kubectl">
                        <code className="audit-kubectl__code" title={entry.kubectl}>
                          {entry.kubectl}
                        </code>
                        <button
                          type="button"
                          className="audit-kubectl__copy-btn"
                          data-testid="copy-kubectl"
                          aria-label="Copy kubectl command"
                          onClick={() => {
                            void handleCopy(entry.kubectl, entry.id);
                          }}
                        >
                          {copiedId === entry.id ? "Copied" : "Copy"}
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {!loading && error === null && entries.length < total && (
        <div className="audit-log__pagination">
          <button
            type="button"
            className="audit-log__btn audit-log__btn--load-more"
            onClick={handleLoadMore}
            disabled={loadingMore}
          >
            {loadingMore ? "Loading…" : "Load more"}
          </button>
        </div>
      )}
    </div>
  );
}
