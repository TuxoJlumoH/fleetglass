import { useEffect, type KeyboardEvent } from "react";
import { useContextsStore } from "../stores/contexts";
import { useLayoutStore, type TabView } from "../stores/layout";
import { AppView, type AppViewProps } from "./AppView";
import { AuditLogViewer, type AuditLogViewerProps } from "./AuditLogViewer";
import { HelmView, type HelmViewProps } from "./HelmView";
import { ImportView } from "./ImportView";
import { OverviewPage } from "./OverviewPage";
import { PluginsView, type PluginsViewProps } from "./PluginsView";
import { PodTable } from "./PodTable";
import { SplitPaneContainer } from "./SplitPaneContainer";

export interface TabsAreaProps {
  /** Injected into every AppView; defaults to the `query_apps` IPC. */
  fetchApps?: AppViewProps["fetchApps"];
  /** Injected into every AuditLogViewer; defaults to the `get_audit_log` IPC. */
  fetchAuditLog?: AuditLogViewerProps["fetchAuditLog"];
  /** Injected into every HelmView; defaults to `list_helm_releases` IPC. */
  fetchHelmReleases?: HelmViewProps["fetchReleases"];
  /** Injected into every HelmView; defaults to `get_helm_release_history` IPC. */
  fetchHelmHistory?: HelmViewProps["fetchHistory"];
  /** Injected into every HelmView; defaults to `diff_helm_revisions` IPC. */
  fetchHelmDiff?: HelmViewProps["fetchDiff"];
  /** Injected into every HelmView; defaults to `preview_helm_rollback` IPC. */
  previewHelmRollback?: HelmViewProps["previewRollback"];
  /** Injected into every PluginsView; defaults to `list_plugins` IPC. */
  fetchPlugins?: PluginsViewProps["fetchPlugins"];
}

/** Options for the secondary pane. "resources" is left out: PodTable is bound to the single
 *  global pods store and window key bindings, so a second instance would not be independent.
 *  "overview" is left out because it shares the resource drawer store with PodTable. */
const SECONDARY_VIEWS: readonly { view: TabView; label: string }[] = [
  { view: "apps", label: "Applications" },
  { view: "helm", label: "Helm releases" },
  { view: "audit", label: "Audit log" },
  { view: "plugins", label: "Plugins" },
];

export function ViewContent({
  view,
  kind,
  textFilter,
  fetchApps,
  fetchAuditLog,
  fetchHelmReleases,
  fetchHelmHistory,
  fetchHelmDiff,
  previewHelmRollback,
  fetchPlugins,
}: {
  view: TabView;
  /** Resource kind of a "resources" tab; defaults to Pods. */
  kind?: string;
  textFilter?: string;
  fetchApps?: AppViewProps["fetchApps"];
  fetchAuditLog?: AuditLogViewerProps["fetchAuditLog"];
  fetchHelmReleases?: HelmViewProps["fetchReleases"];
  fetchHelmHistory?: HelmViewProps["fetchHistory"];
  fetchHelmDiff?: HelmViewProps["fetchDiff"];
  previewHelmRollback?: HelmViewProps["previewRollback"];
  fetchPlugins?: PluginsViewProps["fetchPlugins"];
}) {
  const clusters = useContextsStore((s) => s.activeClusterSelection);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const activeCluster = clusters[0] ?? selectedContext;

  switch (view) {
    case "overview":
      return <OverviewPage />;
    case "resources":
      return (
        <PodTable
          kind={kind ?? "Pod"}
          {...(textFilter !== undefined ? { initialTextFilter: textFilter } : {})}
        />
      );
    case "apps":
      return (
        <AppView clusters={clusters} namespaces={[]} {...(fetchApps ? { fetchApps } : {})} />
      );
    case "helm":
      return (
        <HelmView
          cluster={activeCluster}
          {...(fetchHelmReleases ? { fetchReleases: fetchHelmReleases } : {})}
          {...(fetchHelmHistory ? { fetchHistory: fetchHelmHistory } : {})}
          {...(fetchHelmDiff ? { fetchDiff: fetchHelmDiff } : {})}
          {...(previewHelmRollback ? { previewRollback: previewHelmRollback } : {})}
        />
      );
    case "audit":
      return <AuditLogViewer {...(fetchAuditLog ? { fetchAuditLog } : {})} />;
    case "plugins":
      return <PluginsView {...(fetchPlugins ? { fetchPlugins } : {})} />;
    case "import":
      return <ImportView />;
  }
}

function SecondaryPane({
  fetchApps,
  fetchAuditLog,
  fetchHelmReleases,
  fetchHelmHistory,
  fetchHelmDiff,
  previewHelmRollback,
  fetchPlugins,
}: {
  fetchApps?: AppViewProps["fetchApps"];
  fetchAuditLog?: AuditLogViewerProps["fetchAuditLog"];
  fetchHelmReleases?: HelmViewProps["fetchReleases"];
  fetchHelmHistory?: HelmViewProps["fetchHistory"];
  fetchHelmDiff?: HelmViewProps["fetchDiff"];
  previewHelmRollback?: HelmViewProps["previewRollback"];
  fetchPlugins?: PluginsViewProps["fetchPlugins"];
}) {
  const view = useLayoutStore((s) => s.secondaryView);
  const setSecondaryView = useLayoutStore((s) => s.setSecondaryView);
  return (
    <div className="shell-secondary" data-testid="secondary-pane">
      <div className="shell-secondary__toolbar">
        <label className="shell-secondary__label">
          View
          <select
            className="shell-secondary__select"
            aria-label="Secondary pane view"
            value={view}
            onChange={(e) => {
              setSecondaryView(e.target.value as TabView);
            }}
          >
            {SECONDARY_VIEWS.map((o) => (
              <option key={o.view} value={o.view}>
                {o.label}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="shell-secondary__body">
        {/* Keyed by view so switching views resets that pane's own query state. */}
        <ViewContent
          key={view}
          view={view}
          {...(fetchApps ? { fetchApps } : {})}
          {...(fetchAuditLog ? { fetchAuditLog } : {})}
          {...(fetchHelmReleases ? { fetchHelmReleases } : {})}
          {...(fetchHelmHistory ? { fetchHelmHistory } : {})}
          {...(fetchHelmDiff ? { fetchHelmDiff } : {})}
          {...(previewHelmRollback ? { previewHelmRollback } : {})}
          {...(fetchPlugins ? { fetchPlugins } : {})}
        />
      </div>
    </div>
  );
}

export function TabsArea({
  fetchApps,
  fetchAuditLog,
  fetchHelmReleases,
  fetchHelmHistory,
  fetchHelmDiff,
  previewHelmRollback,
  fetchPlugins,
}: TabsAreaProps = {}) {
  const tabs = useLayoutStore((s) => s.tabs);
  const activeTabId = useLayoutStore((s) => s.activeTabId);
  const setActiveTab = useLayoutStore((s) => s.setActiveTab);
  const cycleTab = useLayoutStore((s) => s.cycleTab);
  const openTab = useLayoutStore((s) => s.openTab);
  const closeTab = useLayoutStore((s) => s.closeTab);

  useEffect(() => {
    const onWindowKeyDown = (e: globalThis.KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey) {
        const key = e.key.toLowerCase();
        if (key === "t") {
          e.preventDefault();
          openTab({ title: "Pods", kind: "Pod" });
        } else if (key === "w") {
          e.preventDefault();
          closeTab(useLayoutStore.getState().activeTabId);
        }
      }
    };
    window.addEventListener("keydown", onWindowKeyDown);
    return () => {
      window.removeEventListener("keydown", onWindowKeyDown);
    };
  }, [openTab, closeTab]);

  // WAI-ARIA tabs pattern: arrows move between tabs, only the active tab is in the tab order.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      e.preventDefault();
      cycleTab(e.key === "ArrowRight" ? 1 : -1);
      const id = useLayoutStore.getState().activeTabId;
      document.getElementById(`tab-${id}`)?.focus();
    }
  };

  const panels = tabs.map((tab) => (
    <section
      key={tab.id}
      id={`panel-${tab.id}`}
      role="tabpanel"
      aria-labelledby={`tab-${tab.id}`}
      className="tabs__panel"
      data-view={tab.view ?? "resources"}
      hidden={tab.id !== activeTabId}
    >
      {tab.id === activeTabId ? (
        <ViewContent
          view={tab.view ?? "resources"}
          kind={tab.kind ?? "Pod"}
          {...(tab.textFilter !== undefined ? { textFilter: tab.textFilter } : {})}
          {...(fetchApps ? { fetchApps } : {})}
          {...(fetchAuditLog ? { fetchAuditLog } : {})}
          {...(fetchHelmReleases ? { fetchHelmReleases } : {})}
          {...(fetchHelmHistory ? { fetchHelmHistory } : {})}
          {...(fetchHelmDiff ? { fetchHelmDiff } : {})}
          {...(previewHelmRollback ? { previewHelmRollback } : {})}
          {...(fetchPlugins ? { fetchPlugins } : {})}
        />
      ) : null}
    </section>
  ));

  return (
    <main className="main">
      <div className="tabs" role="tablist" aria-label="Open views" onKeyDown={onKeyDown}>
        {tabs.map((tab) => {
          const active = tab.id === activeTabId;
          return (
            <div key={tab.id} className="tabs__item">
              <button
                id={`tab-${tab.id}`}
                type="button"
                role="tab"
                className="tabs__tab"
                aria-selected={active}
                aria-controls={`panel-${tab.id}`}
                tabIndex={active ? 0 : -1}
                onClick={() => {
                  setActiveTab(tab.id);
                }}
              >
                {tab.title}
              </button>
              {tabs.length > 1 && (
                <button
                  type="button"
                  className="tabs__close"
                  aria-label={`Close ${tab.title}`}
                  onClick={() => {
                    closeTab(tab.id);
                  }}
                >
                  ×
                </button>
              )}
            </div>
          );
        })}
        <button
          type="button"
          className="tabs__new"
          aria-label="New tab"
          onClick={() => {
            openTab({ title: "Pods", kind: "Pod" });
          }}
        >
          + New Tab
        </button>
      </div>
      <SplitPaneContainer
        className="tabs__split"
        primary={<div className="tabs__panels">{panels}</div>}
        secondary={
          <SecondaryPane
            {...(fetchApps ? { fetchApps } : {})}
            {...(fetchAuditLog ? { fetchAuditLog } : {})}
            {...(fetchHelmReleases ? { fetchHelmReleases } : {})}
            {...(fetchHelmHistory ? { fetchHelmHistory } : {})}
            {...(fetchHelmDiff ? { fetchHelmDiff } : {})}
            {...(previewHelmRollback ? { previewHelmRollback } : {})}
            {...(fetchPlugins ? { fetchPlugins } : {})}
          />
        }
      />
    </main>
  );
}
