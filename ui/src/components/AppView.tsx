import { useEffect, useState } from "react";
import {
  commands,
  type AppPage,
  type AppQuerySpec_Deserialize,
  type AppResource,
  type AppSummary,
  type RowStatus,
  type StoreAccess,
} from "../generated/bindings";
import {
  loadingProgress,
  partialSuffix,
  reconnectingText,
  restrictedScope,
  staleSuffix,
} from "./podAccess";

const PAGE_LIMIT = 100;
const FILTER_DEBOUNCE_MS = 150;
/** Re-query interval while a store is still in its initial list (rows arrive progressively). */
export const LOADING_POLL_MS = 500;

export interface AppViewProps {
  clusters: string[];
  namespaces: string[];
  fetchApps?: (spec: AppQuerySpec_Deserialize) => Promise<AppPage>;
}

async function defaultFetchApps(spec: AppQuerySpec_Deserialize): Promise<AppPage> {
  const res = await commands.queryApps(spec);
  if (res.status === "error") throw new Error(res.error.message, { cause: res.error });
  return res.data;
}

interface WindowState {
  /** Query key the loaded window belongs to; differs from the current key while loading. */
  key: string | null;
  apps: AppSummary[];
  total: number;
  access: StoreAccess[];
  error: string | null;
}
const EMPTY_WINDOW: WindowState = { key: null, apps: [], total: 0, access: [], error: null };

function specFor(queryKey: string, offset: number): AppQuerySpec_Deserialize {
  const [clusters, namespaces, textFilter] = JSON.parse(queryKey) as [string[], string[], string];
  return { clusters, namespaces, textFilter: textFilter || null, offset, limit: PAGE_LIMIT };
}

function errorMessage(err: unknown): string {
  if (typeof err === "object" && err !== null && "message" in err) return String(err.message);
  return String(err);
}

// Display-only badge: worst of failed > degraded > pending ("progressing") > healthy.
// Rust owns each component's status; this only picks which one to show.
function overallBadge(app: AppSummary): string {
  const seen = new Set<RowStatus>(app.components.map((c) => c.status));
  if (seen.has("failed")) return "failed";
  if (seen.has("degraded")) return "degraded";
  return seen.has("pending") ? "progressing" : "healthy";
}

function groupByKind(components: AppResource[]): [string, AppResource[]][] {
  const groups = new Map<string, AppResource[]>();
  for (const c of components) groups.set(c.kind, [...(groups.get(c.kind) ?? []), c]);
  return [...groups];
}

function renderBanners(accessList: StoreAccess[]) {
  const banners: React.ReactNode[] = [];
  accessList.forEach((item, index) => {
    const restricted = restrictedScope(item);
    if (restricted !== null) {
      banners.push(
        <div
          key={`restricted-${item.cluster}-${item.kind}-${String(index)}`}
          role="status"
          className="app-view__banner app-view__banner--scope"
          data-testid="restricted-scope"
          title={restricted.detail}
        >
          <span className="pill warn">{restricted.label}</span>
          <span className="visually-hidden"> {restricted.detail}</span>
        </div>,
      );
    }
    if (item.access.state === "forbidden") {
      const resource = item.access.resource;
      const message = item.access.message;
      banners.push(
        <div
          key={`forbidden-${item.cluster}-${resource}-${String(index)}`}
          role="alert"
          className="app-view__banner app-view__banner--forbidden"
        >
          No permission to list {resource} in {item.cluster}: {message}
          {staleSuffix(item.rows)}
        </div>,
      );
    } else if (item.access.state === "error") {
      const message = item.access.message;
      banners.push(
        <div
          key={`error-${item.cluster}-${String(index)}`}
          role="alert"
          className="app-view__banner app-view__banner--error"
        >
          Connection error for {item.cluster}: {message}
          {partialSuffix(item.access.partialRows)}
          {staleSuffix(item.rows, item.access.partialRows)}
        </div>,
      );
    } else if (item.access.state === "authExpired") {
      banners.push(
        <div
          key={`auth-expired-${item.cluster}-${String(index)}`}
          role="alert"
          className="app-view__banner app-view__banner--error"
        >
          Authentication expired for {item.cluster}: {item.access.hint}
          {staleSuffix(item.rows, item.access.partialRows)}
        </div>,
      );
    } else if (item.access.state === "reconnecting") {
      banners.push(
        <div
          key={`reconnecting-${item.cluster}-${String(index)}`}
          role="status"
          className="app-view__banner app-view__banner--loading"
        >
          {reconnectingText(item.cluster, item.rows)}
        </div>,
      );
    }
  });
  const progress = loadingProgress(accessList);
  if (progress !== null) {
    banners.push(
      <div
        key="loading-progress"
        role="status"
        className="app-view__banner app-view__banner--loading"
        data-testid="app-view-loading-progress"
      >
        {progress}
      </div>,
    );
  }
  return banners;
}

export function AppView({ clusters, namespaces, fetchApps = defaultFetchApps }: AppViewProps) {
  const [filterText, setFilterText] = useState("");
  const [textFilter, setTextFilter] = useState("");
  const [result, setResult] = useState<WindowState>(EMPTY_WINDOW);
  const [loadingMore, setLoadingMore] = useState(false);

  // String key so callers passing fresh array literals do not refetch on every render.
  const queryKey = JSON.stringify([clusters, namespaces, textFilter]);
  const loading = clusters.length > 0 && result.key !== queryKey;
  const { apps, total, access, error } = clusters.length === 0 || loading ? EMPTY_WINDOW : result;

  useEffect(() => {
    const timer = setTimeout(() => {
      setTextFilter(filterText.trim());
    }, FILTER_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [filterText]);

  useEffect(() => {
    if (clusters.length === 0) return;
    let cancelled = false;
    fetchApps(specFor(queryKey, 0)).then(
      (page) => {
        if (!cancelled) {
          setResult({
            key: queryKey,
            apps: page.apps,
            total: page.total,
            access: page.access,
            error: null,
          });
        }
      },
      (err: unknown) => {
        if (!cancelled) {
          setResult({ ...EMPTY_WINDOW, key: queryKey, error: errorMessage(err) });
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [queryKey, clusters.length, fetchApps]);

  // The page above is a snapshot: while a store is still loading, re-query the
  // loaded window so grouped applications and the progress banner stay current.
  const pollKey =
    clusters.length > 0 &&
    result.key === queryKey &&
    result.error === null &&
    result.access.some(
      (item) => item.access.state === "loading" || item.access.state === "reconnecting",
    )
      ? queryKey
      : null;
  useEffect(() => {
    if (pollKey === null) return;
    let cancelled = false;
    const limit = Math.max(PAGE_LIMIT, result.apps.length);
    const timer = setTimeout(() => {
      fetchApps({ ...specFor(pollKey, 0), limit }).then(
        (page) => {
          if (cancelled) return;
          setResult((prev) =>
            prev.key === pollKey
              ? { key: pollKey, apps: page.apps, total: page.total, access: page.access, error: null }
              : prev,
          );
        },
        (err: unknown) => {
          if (cancelled) return;
          setResult((prev) => (prev.key === pollKey ? { ...prev, error: errorMessage(err) } : prev));
        },
      );
    }, LOADING_POLL_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // `result` re-arms the timer after each response.
  }, [pollKey, fetchApps, result]);

  const loadMore = () => {
    setLoadingMore(true);
    fetchApps(specFor(queryKey, apps.length)).then(
      (page) => {
        setResult((prev) =>
          prev.key === queryKey
            ? {
                ...prev,
                apps: [...prev.apps, ...page.apps],
                total: page.total,
                access: page.access,
              }
            : prev,
        );
        setLoadingMore(false);
      },
      (err: unknown) => {
        setResult((prev) => (prev.key === queryKey ? { ...prev, error: errorMessage(err) } : prev));
        setLoadingMore(false);
      },
    );
  };

  const banners = renderBanners(access);
  const hasLoadingAccess = access.some((item) => item.access.state === "loading");
  const isLoading = loading || (apps.length === 0 && hasLoadingAccess);

  return (
    <div className="app-view" data-testid="app-view">
      <div className="app-view__toolbar">
        <input
          type="search"
          aria-label="Filter applications"
          placeholder="Filter applications by name…"
          className="app-view__search"
          value={filterText}
          onChange={(e) => {
            setFilterText(e.target.value);
          }}
        />
        <div className="app-view__stats">
          <span>
            {String(apps.length)} of {String(total)} applications
          </span>
        </div>
      </div>

      {clusters.length === 0 ? (
        <div className="app-view__empty" role="status" data-testid="app-view-empty">
          <p>Select a context to view applications.</p>
        </div>
      ) : error !== null ? (
        <p className="error-state" role="alert" data-testid="app-view-error">
          Failed to load applications: {error}
        </p>
      ) : (
        <>
          {banners.length > 0 && (
            <div className="app-view__banners" data-testid="app-view-banners">
              {banners}
            </div>
          )}
          {isLoading ? (
            <div className="app-view__empty" role="status" data-testid="app-view-loading">
              <p>Loading applications…</p>
            </div>
          ) : apps.length === 0 ? (
            banners.length === 0 ? (
              <div className="app-view__empty" role="status" data-testid="app-view-empty">
                <p>No applications found in the selected clusters/namespaces</p>
              </div>
            ) : null
          ) : (
            <div className="app-view__grid" data-testid="app-view-grid">
              {apps.map((app) => {
                const badge = overallBadge(app);
                return (
                  <article
                    key={`${app.cluster}/${app.namespace}/${app.name}`}
                    className="app-card"
                    data-testid={`app-card-${app.name}`}
                  >
                    <div className="app-card__header">
                      <div className="app-card__title-group">
                        <h3 className="app-card__title">{app.name}</h3>
                        <span className="app-card__meta" title={`Namespace: ${app.namespace}`}>
                          {app.namespace}
                        </span>
                        <span className="app-card__meta-cluster" title={`Cluster: ${app.cluster}`}>
                          {app.cluster}
                        </span>
                      </div>
                      <span
                        className={`app-status-badge app-status-badge--${badge}`}
                        data-testid={`app-status-${app.name}`}
                      >
                        {badge}
                      </span>
                    </div>

                    <div className="app-card__body">
                      {groupByKind(app.components).map(([kind, items]) => (
                        <section
                          key={kind}
                          className="app-card__section"
                          data-testid={`app-kind-${app.name}-${kind}`}
                          aria-label={kind}
                        >
                          <h4 className="app-card__section-title">
                            {kind} <span className="app-card__badge-count">{String(items.length)}</span>
                          </h4>
                          <ul className="app-card__resource-list">
                            {items.map((c) => (
                              <li
                                key={`${c.cluster}/${c.namespace}/${c.name}`}
                                className="app-card__resource-item"
                              >
                                <span className="app-card__resource-name">{c.name}</span>
                                <span className="app-card__resource-status" title={c.status}>
                                  {c.statusReason || c.status}
                                </span>
                              </li>
                            ))}
                          </ul>
                        </section>
                      ))}
                    </div>
                  </article>
                );
              })}
            </div>
          )}
        </>
      )}

      {error === null && clusters.length > 0 && apps.length < total && (
        <div className="app-view__toolbar">
          <button type="button" disabled={loadingMore} onClick={loadMore}>
            {loadingMore ? "Loading…" : "Load more"}
          </button>
        </div>
      )}
    </div>
  );
}
