import { useEffect, useState, type DragEvent, type KeyboardEvent } from "react";
import {
  commands,
  type WorkloadLogTarget,
  type WorkloadSelector,
} from "../generated/bindings";
import { useActionsStore } from "../stores/actions";
import { useContextsStore } from "../stores/contexts";
import { type PaneId, useLayoutStore, type TabView } from "../stores/layout";
import type { Row } from "../stores/pods";
import { AppView, type AppViewProps } from "./AppView";
import { AuditLogViewer, type AuditLogViewerProps } from "./AuditLogViewer";
import { DiffModal } from "./DiffModal";
import { HelmView, type HelmViewProps } from "./HelmView";
import { ImportView } from "./ImportView";
import { OverviewPage } from "./OverviewPage";
import { focusPane } from "./paneFocus";
import { PluginsView, type PluginsViewProps } from "./PluginsView";
import { PodTable } from "./PodTable";
import { SplitPaneContainer } from "./SplitPaneContainer";
import { WorkloadLogsView } from "./WorkloadLogsView";
import { YamlEditor } from "./YamlEditor";

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

const SECONDARY_VIEWS: readonly { view: TabView; label: string }[] = [
  { view: "apps", label: "Applications" },
  { view: "helm", label: "Helm releases" },
  { view: "logs", label: "Workload Logs" },
  { view: "yaml", label: "YAML Editor" },
  { view: "resources", label: "Resources" },
  { view: "audit", label: "Audit log" },
  { view: "plugins", label: "Plugins" },
  { view: "overview", label: "Overview" },
];

const SECONDARY_KINDS: readonly { kind: string; label: string }[] = [
  { kind: "Pod", label: "Pods" },
  { kind: "Deployment", label: "Deployments" },
  { kind: "StatefulSet", label: "StatefulSets" },
  { kind: "DaemonSet", label: "DaemonSets" },
  { kind: "ReplicaSet", label: "ReplicaSets" },
  { kind: "Job", label: "Jobs" },
  { kind: "CronJob", label: "CronJobs" },
  { kind: "Service", label: "Services" },
  { kind: "Ingress", label: "Ingresses" },
  { kind: "ConfigMap", label: "ConfigMaps" },
  { kind: "Secret", label: "Secrets" },
  { kind: "Node", label: "Nodes" },
  { kind: "Namespace", label: "Namespaces" },
  { kind: "PersistentVolume", label: "PersistentVolumes" },
  { kind: "PersistentVolumeClaim", label: "PersistentVolumeClaims" },
  { kind: "ServiceAccount", label: "ServiceAccounts" },
  { kind: "Role", label: "Roles" },
  { kind: "ClusterRole", label: "ClusterRoles" },
  { kind: "RoleBinding", label: "RoleBindings" },
  { kind: "ClusterRoleBinding", label: "ClusterRoleBindings" },
  { kind: "Event", label: "Events" },
];

/** Render with `key={selection.uid}`: loaded and edited YAML never carry over to another resource. */
function SecondaryYamlEditor({ selection }: { selection: Row }) {
  const cluster = selection.cluster;
  const kind = selection.kind;
  const namespace = selection.namespace ?? null;
  const name = selection.name;

  const [yaml, setYaml] = useState<string | null>(null);
  const [editedYaml, setEditedYaml] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [dryRunning, setDryRunning] = useState(false);
  const [dryRunError, setDryRunError] = useState<string | null>(null);
  const [diffModalOpen, setDiffModalOpen] = useState(false);
  const [applySuccess, setApplySuccess] = useState<string | null>(null);

  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const isReadOnly = cluster !== "" ? (settingsByContext[cluster]?.readOnly ?? false) : false;
  const isProtected = cluster !== "" ? (settingsByContext[cluster]?.protected ?? false) : false;

  useEffect(() => {
    let cancelled = false;
    commands
      .getResourceDetail(cluster, kind, namespace, name)
      .then((res) => {
        if (cancelled) return;
        setLoading(false);
        if (res.status === "error") {
          setError(res.error.message);
        } else {
          setYaml(res.data.yaml);
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setLoading(false);
        setError(
          typeof err === "object" && err !== null && "message" in err
            ? String(err.message)
            : String(err),
        );
      });

    return () => {
      cancelled = true;
    };
  }, [cluster, kind, namespace, name]);

  if (loading) {
    return (
      <div className="secondary-empty-state" data-testid="secondary-yaml-loading" role="status">
        <p>Loading {kind} {name}…</p>
      </div>
    );
  }

  if (error) {
    return (
      <div className="secondary-empty-state" data-testid="secondary-yaml-error" role="alert">
        <p className="error-state">{error}</p>
      </div>
    );
  }

  const currentYaml = editedYaml ?? yaml ?? "";

  const handleReviewAndApply = async () => {
    setDryRunning(true);
    setDryRunError(null);
    setApplySuccess(null);
    try {
      const res = await commands.dryRunApply(cluster, currentYaml);
      setDryRunning(false);
      if (res.status === "error") {
        setDryRunError(res.error.message || `Dry-run failed: ${res.error.kind}`);
        return;
      }
      setDiffModalOpen(true);
    } catch (err: unknown) {
      setDryRunning(false);
      setDryRunError(
        typeof err === "object" && err !== null && "message" in err
          ? String(err.message)
          : String(err),
      );
    }
  };

  return (
    <div
      className="secondary-yaml-container"
      data-testid="secondary-yaml-editor"
      style={{ height: "100%", width: "100%", display: "flex", flexDirection: "column" }}
    >
      <div
        className="secondary-yaml-toolbar"
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "6px 12px",
          borderBottom: "1px solid var(--border)",
          flex: "none",
        }}
      >
        <span className="secondary-yaml-title" style={{ fontSize: "12px", color: "var(--fg-muted)" }}>
          <strong style={{ color: "var(--fg)" }}>{kind}</strong>: {name}
        </span>
        <button
          type="button"
          className="btn sm"
          data-testid="yaml-apply-btn"
          disabled={dryRunning || isReadOnly || !currentYaml}
          onClick={() => {
            void handleReviewAndApply();
          }}
        >
          {dryRunning ? "Dry-running…" : "Review & Apply"}
        </button>
      </div>
      {dryRunError && (
        <div role="alert" className="error-state" data-testid="secondary-yaml-apply-error" style={{ padding: "4px 12px" }}>
          {dryRunError}
        </div>
      )}
      {applySuccess && (
        <div role="status" className="resource-drawer__status-success" data-testid="secondary-yaml-apply-success" style={{ padding: "4px 12px" }}>
          {applySuccess}
        </div>
      )}
      <div style={{ flex: 1, minHeight: 0 }}>
        <YamlEditor
          value={currentYaml}
          onChange={(val) => {
            setEditedYaml(val);
          }}
          readOnly={isReadOnly}
        />
      </div>
      {diffModalOpen && (
        <DiffModal
          open={true}
          applyMode={true}
          context={cluster}
          resourceName={name}
          resourceKind={kind}
          resourceNamespace={namespace}
          readOnly={isReadOnly}
          isProtected={isProtected}
          left={{
            context: cluster,
            namespace: namespace ?? undefined,
            name,
            manifest: yaml ?? "",
          }}
          right={{
            context: cluster,
            namespace: namespace ?? undefined,
            name,
            manifest: currentYaml,
          }}
          onClose={() => {
            setDiffModalOpen(false);
          }}
          onApplySuccess={(newYaml) => {
            setDiffModalOpen(false);
            setYaml(newYaml);
            setEditedYaml(null);
            setApplySuccess(`Applied changes to ${kind} ${name}`);
          }}
        />
      )}
    </div>
  );
}

export function ViewContent({
  view,
  kind,
  textFilter,
  namespace,
  paneId = "primary",
  cluster,
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
  kind?: string | undefined;
  textFilter?: string | undefined;
  namespace?: string | null | undefined;
  paneId?: PaneId | undefined;
  cluster?: string | null | undefined;
  fetchApps?: AppViewProps["fetchApps"] | undefined;
  fetchAuditLog?: AuditLogViewerProps["fetchAuditLog"] | undefined;
  fetchHelmReleases?: HelmViewProps["fetchReleases"] | undefined;
  fetchHelmHistory?: HelmViewProps["fetchHistory"] | undefined;
  fetchHelmDiff?: HelmViewProps["fetchDiff"] | undefined;
  previewHelmRollback?: HelmViewProps["previewRollback"] | undefined;
  fetchPlugins?: PluginsViewProps["fetchPlugins"] | undefined;
}) {
  const clusters = useContextsStore((s) => s.activeClusterSelection);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const activeCluster = cluster ?? clusters[0] ?? selectedContext;
  const activeSelection = useActionsStore((s) => s.selection);

  switch (view) {
    case "overview":
      return <OverviewPage paneId={paneId} />;
    case "resources":
      return (
        <PodTable
          paneId={paneId}
          kind={kind ?? "Pod"}
          {...(cluster ? { clusters: [cluster] } : {})}
          {...(textFilter !== undefined ? { initialTextFilter: textFilter } : {})}
          {...(namespace !== undefined ? { initialNamespace: namespace } : {})}
        />
      );
    case "apps":
      return (
        <AppView
          clusters={cluster ? [cluster] : clusters}
          namespaces={[]}
          {...(fetchApps ? { fetchApps } : {})}
        />
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
    case "logs": {
      if (!activeSelection) {
        return (
          <div className="secondary-empty-state" data-testid="secondary-logs-empty" role="status">
            <p>Select a pod or workload to view logs</p>
          </div>
        );
      }
      const isPod = activeSelection.kind === "Pod";
      const isWorkload = [
        "Deployment",
        "StatefulSet",
        "DaemonSet",
        "ReplicaSet",
        "Job",
        "CronJob",
      ].includes(activeSelection.kind);

      if (!isPod && !isWorkload) {
        return (
          <div className="secondary-empty-state" data-testid="secondary-logs-empty" role="status">
            <p>Select a pod or workload to view logs</p>
          </div>
        );
      }

      const selector: WorkloadSelector = isPod
        ? { type: "pod", name: activeSelection.name }
        : { type: "owner", kind: activeSelection.kind, name: activeSelection.name };

      const logTarget: WorkloadLogTarget = {
        cluster: activeSelection.cluster,
        namespace: activeSelection.namespace ?? "default",
        selector,
        container: null,
        tailLines: 500,
        previous: false,
      };
      return <WorkloadLogsView target={logTarget} />;
    }
    case "yaml": {
      if (!activeSelection) {
        return (
          <div className="secondary-empty-state" data-testid="secondary-yaml-empty" role="status">
            <p>Select a resource to view or edit YAML</p>
          </div>
        );
      }
      return <SecondaryYamlEditor key={activeSelection.uid} selection={activeSelection} />;
    }
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
  const secondaryKind = useLayoutStore((s) => s.secondaryKind);
  const setSecondaryKind = useLayoutStore((s) => s.setSecondaryKind);
  const secondaryCluster = useLayoutStore((s) => s.secondaryCluster);
  const setSecondaryCluster = useLayoutStore((s) => s.setSecondaryCluster);
  const resetToken = useLayoutStore((s) => s.secondaryResetToken);
  const loadTabIntoSecondary = useLayoutStore((s) => s.loadTabIntoSecondary);
  // The filters only seed a (re)mounted table; the table itself requeries as they change, so
  // they are read once per mount instead of keying (and remounting) the view on every keystroke.
  const { secondaryTextFilter, secondaryNamespace } = useLayoutStore.getState();
  const contexts = useContextsStore((s) => s.contexts);
  const [isDragOver, setIsDragOver] = useState(false);

  const handleDragOver = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    if (!isDragOver) setIsDragOver(true);
  };

  const handleDragLeave = (e: DragEvent<HTMLDivElement>) => {
    // Moving onto a child of the pane is not leaving it.
    if (e.relatedTarget instanceof Node && e.currentTarget.contains(e.relatedTarget)) return;
    setIsDragOver(false);
  };

  const handleDrop = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setIsDragOver(false);
    const tabId =
      e.dataTransfer.getData("application/x-fleetglass-tab") ||
      e.dataTransfer.getData("text/plain");
    if (!tabId) return;
    const tab = useLayoutStore.getState().tabs.find((t) => t.id === tabId);
    if (tab) loadTabIntoSecondary(tab);
  };

  return (
    <div
      className={`shell-secondary ${isDragOver ? "shell-secondary--dropzone" : ""}`}
      data-testid="secondary-pane"
      data-dragover={isDragOver}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
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
        {view === "resources" && (
          <label className="shell-secondary__label" style={{ marginLeft: 8 }}>
            Kind
            <select
              className="shell-secondary__select"
              aria-label="Secondary pane kind"
              value={secondaryKind}
              onChange={(e) => {
                setSecondaryKind(e.target.value);
              }}
            >
              {(SECONDARY_KINDS.some((k) => k.kind === secondaryKind)
                ? SECONDARY_KINDS
                : [{ kind: secondaryKind, label: secondaryKind }, ...SECONDARY_KINDS]
              ).map((k) => (
                <option key={k.kind} value={k.kind}>
                  {k.label}
                </option>
              ))}
            </select>
          </label>
        )}
        {contexts.length > 1 && (
          <label className="shell-secondary__label" style={{ marginLeft: 8 }}>
            Cluster
            <select
              className="shell-secondary__select"
              aria-label="Secondary pane cluster"
              value={secondaryCluster ?? ""}
              onChange={(e) => {
                setSecondaryCluster(e.target.value === "" ? null : e.target.value);
              }}
            >
              <option value="">Default (Active Context)</option>
              {contexts.map((c) => (
                <option key={c.name} value={c.name}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      <div className="shell-secondary__body">
        {/* A new view, kind or cluster, or a dropped tab, remounts the pane with its saved state. */}
        <ViewContent
          key={`${view}\n${secondaryKind}\n${secondaryCluster ?? ""}\n${String(resetToken)}`}
          view={view}
          paneId="secondary"
          kind={secondaryKind}
          cluster={secondaryCluster}
          textFilter={secondaryTextFilter}
          namespace={secondaryNamespace}
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
  const [draggingTabId, setDraggingTabId] = useState<string | null>(null);
  const [dragOverTabId, setDragOverTabId] = useState<string | null>(null);

  useEffect(() => {
    const onWindowKeyDown = (e: globalThis.KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey) {
        const key = e.key.toLowerCase();
        if (key === "t") {
          e.preventDefault();
          openTab({ title: "Pods", kind: "Pod" });
        } else if (key === "w") {
          // Split open: Ctrl+W switches panes, from anywhere (CodeMirror included). Closed: it
          // closes the active tab, as before split view.
          e.preventDefault();
          const layout = useLayoutStore.getState();
          if (layout.splitMode === "none") {
            closeTab(layout.activeTabId);
            return;
          }
          layout.switchPane();
          focusPane(useLayoutStore.getState().activePane);
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
          paneId="primary"
          kind={tab.kind ?? "Pod"}
          cluster={tab.cluster}
          {...(tab.textFilter !== undefined ? { textFilter: tab.textFilter } : {})}
          {...(tab.namespace !== undefined ? { namespace: tab.namespace } : {})}
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
          const isDragging = draggingTabId === tab.id;
          const isDragOver = dragOverTabId === tab.id;
          return (
            <div
              key={tab.id}
              className={`tabs__item ${isDragging ? "tabs__item--dragging" : ""} ${
                isDragOver ? "tabs__item--dragover" : ""
              }`}
              draggable={true}
              onDragStart={(e) => {
                e.dataTransfer.setData("text/plain", tab.id);
                e.dataTransfer.setData("application/x-fleetglass-tab", tab.id);
                e.dataTransfer.effectAllowed = "move";
                setDraggingTabId(tab.id);
              }}
              onDragEnd={() => {
                setDraggingTabId(null);
                setDragOverTabId(null);
              }}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (dragOverTabId !== tab.id) setDragOverTabId(tab.id);
              }}
              onDragLeave={() => {
                if (dragOverTabId === tab.id) setDragOverTabId(null);
              }}
              onDrop={(e) => {
                e.preventDefault();
                const sourceId =
                  e.dataTransfer.getData("application/x-fleetglass-tab") ||
                  e.dataTransfer.getData("text/plain");
                if (sourceId && sourceId !== tab.id) {
                  useLayoutStore.getState().reorderTabs(sourceId, tab.id);
                }
                setDraggingTabId(null);
                setDragOverTabId(null);
              }}
            >
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
