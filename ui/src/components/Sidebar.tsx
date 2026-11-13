import {
  Activity,
  Anchor,
  ArrowRightToLine,
  Box,
  Boxes,
  Check,
  ChevronDown,
  Clock,
  Copy,
  Cpu,
  Database,
  Disc,
  DoorOpen,
  Download,
  FileCheck,
  FileText,
  Gauge,
  GitBranch,
  HardDrive,
  House,
  Key,
  LayoutGrid,
  Layers,
  Lock,
  type LucideIcon,
  Monitor,
  Network,
  Radio,
  RefreshCw,
  Route,
  Scale,
  ServerCog,
  Shield,
  ShieldCheck,
  ShipWheel,
  Sliders,
  UserCheck,
  Workflow,
  Zap,
} from "lucide-react";
import {
  type CSSProperties,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import { commands, type DiagnosticReport } from "../generated/bindings";
import {
  clustersForQuery,
  type ConnectionStatus,
  type Context,
  useContextsStore,
} from "../stores/contexts";
import { useIsAndroid } from "../stores/appInfo";
import { IMPORT_TAB, type TabView, useLayoutStore } from "../stores/layout";
import { useSidebarStore } from "../stores/sidebar";
import { authLabel } from "./authLabel";
import { clusterLight, UNKNOWN_LIGHT } from "./connectionStatus";
import { useAccessLights } from "./useConnectionLights";
import { DiagnosticsModal } from "./DiagnosticsModal";
import { type KindTotals, useKindCounts } from "./useKindCounts";

interface DiagnosticsState {
  context: string | null;
  loading: boolean;
  report: DiagnosticReport | null;
  error: string | null;
}

const closedDiagnostics: DiagnosticsState = {
  context: null,
  loading: false,
  report: null,
  error: null,
};

/**
 * Runs `run_connection_diagnostics` for a context on demand, and once automatically
 * whenever a context's connection state transitions into `"error"`.
 */
function useContextDiagnostics(connectionStatus: Record<string, ConnectionStatus>) {
  const [state, setState] = useState<DiagnosticsState>(closedDiagnostics);
  // Ignores responses for a run that was superseded or closed.
  const runId = useRef(0);
  // Element focused before the dialog opened; re-focused on close.
  const returnFocus = useRef<HTMLElement | null>(null);
  const isOpen = useRef(false);

  /** `focusOnClose` defaults to the element focused right now. */
  const open = useCallback((context: string, focusOnClose?: HTMLElement | null) => {
    const id = ++runId.current;
    // A re-open while the dialog is up keeps the original return target, not the Close button.
    if (!isOpen.current) {
      const active = document.activeElement;
      returnFocus.current =
        focusOnClose ?? (active instanceof HTMLElement && active !== document.body ? active : null);
    }
    isOpen.current = true;
    setState({ context, loading: true, report: null, error: null });
    void (async () => {
      let next: DiagnosticsState;
      try {
        const res = await commands.runConnectionDiagnostics(context);
        next =
          res.status === "ok"
            ? { context, loading: false, report: res.data, error: null }
            : { context, loading: false, report: null, error: res.error.message };
      } catch (e) {
        next = {
          context,
          loading: false,
          report: null,
          error: e instanceof Error ? e.message : String(e),
        };
      }
      if (runId.current === id) setState(next);
    })();
  }, []);

  const close = useCallback(() => {
    runId.current++;
    isOpen.current = false;
    const target = returnFocus.current;
    returnFocus.current = null;
    setState(closedDiagnostics);
    if (target?.isConnected) target.focus();
  }, []);

  // Seeded with the statuses at mount so an error that already existed does not re-open.
  const previous = useRef(connectionStatus);
  useEffect(() => {
    const prev = previous.current;
    previous.current = connectionStatus;
    // Only the first context that transitions to error in one update auto-opens; the dialog
    // shows one report at a time and the others can be opened from their rows.
    const failed = Object.keys(connectionStatus).find(
      (name) => connectionStatus[name]?.state === "error" && prev[name]?.state !== "error",
    );
    if (failed !== undefined) open(failed);
  }, [connectionStatus, open]);

  return { state, open, close };
}

/** Shift+F10, the ContextMenu key, or a bare "d" on a context row opens diagnostics. */
function isDiagnosticsKey(e: KeyboardEvent<HTMLElement>): boolean {
  if (e.ctrlKey || e.metaKey || e.altKey) return false;
  if (e.key === "ContextMenu") return true;
  if (e.key === "F10") return e.shiftKey;
  return e.key === "d" && !e.shiftKey;
}

/** Focusable list with a roving tabindex. */
function ContextList({ contexts, onNavigate }: { contexts: Context[]; onNavigate?: SidebarNavigate | undefined }) {
  const [active, setActive] = useState(() => Math.max(0, contexts.findIndex((c) => c.isCurrent)));
  const itemRefs = useRef<(HTMLLIElement | null)[]>([]);
  const connectionStatus = useContextsStore((s) => s.connectionStatus);
  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const selectContext = useContextsStore((s) => s.selectContext);
  const activeClusterSelection = useContextsStore((s) => s.activeClusterSelection);
  const toggleClusterSelection = useContextsStore((s) => s.toggleClusterSelection);
  const setActiveClusterSelection = useContextsStore((s) => s.setActiveClusterSelection);
  const diagnostics = useContextDiagnostics(connectionStatus);
  // Queried clusters show what their watch stores report; the rest only an explicit status.
  const clustersKey = useContextsStore((s) => clustersForQuery(s).join("\n"));
  const queried = useMemo(() => (clustersKey === "" ? [] : clustersKey.split("\n")), [clustersKey]);
  const accessLights = useAccessLights(queried);

  const onKeyDown = (e: KeyboardEvent<HTMLUListElement>) => {
    const targets: Record<string, number> = {
      ArrowDown: active + 1,
      ArrowUp: active - 1,
      Home: 0,
      End: contexts.length - 1,
    };
    const to = targets[e.key];
    // Ignore keys bubbling (via the React tree) from the portalled diagnostics modal.
    if (to === undefined || !e.currentTarget.contains(e.target as Node)) return;
    e.preventDefault();
    const next = Math.max(0, Math.min(contexts.length - 1, to));
    setActive(next);
    itemRefs.current[next]?.focus();
  };

  return (
    <ul className="ctx-list" aria-label="Kubeconfig contexts" onKeyDown={onKeyDown}>
      {contexts.map((ctx, i) => {
        const light =
          clusterLight(ctx.name, accessLights, connectionStatus[ctx.name]) ?? UNKNOWN_LIGHT;
        const settings = settingsByContext[ctx.name];
        const isSelected =
          activeClusterSelection.length > 0
            ? activeClusterSelection.includes(ctx.name)
            : selectedContext === ctx.name;

        return (
          <li
            key={ctx.name}
            ref={(el) => {
              itemRefs.current[i] = el;
            }}
            className="ctx"
            data-selected={isSelected ? "true" : undefined}
            // The swatch reads `--ctx`; without a per-context colour it inherits the theme's.
            style={settings?.colour ? ({ "--ctx": settings.colour } as CSSProperties) : undefined}
            tabIndex={i === active ? 0 : -1}
            aria-current={ctx.isCurrent ? "true" : undefined}
            title={ctx.server ?? undefined}
            onFocus={() => {
              setActive(i);
            }}
            onClick={(e) => {
              if (e.ctrlKey || e.metaKey) {
                toggleClusterSelection(ctx.name);
              } else {
                setActiveClusterSelection([]);
                void selectContext(ctx.name);
                onNavigate?.({ type: "context" });
              }
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              // Ignore menus bubbling (via the React tree) from the portalled modal.
              if (!e.currentTarget.contains(e.target as Node)) return;
              diagnostics.open(ctx.name, e.currentTarget);
            }}
            onKeyDown={(e) => {
              // A prevented key was consumed elsewhere (e.g. `d` finishing the `g d` chord).
              if (e.target !== e.currentTarget || e.defaultPrevented) return;
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                void selectContext(ctx.name);
                onNavigate?.({ type: "context" });
              } else if (isDiagnosticsKey(e)) {
                // Keyboard equivalent of right-click; stop so global "d" shortcuts don't fire.
                e.preventDefault();
                e.stopPropagation();
                diagnostics.open(ctx.name, e.currentTarget);
              }
            }}
          >
            <span className="ctx__sw" aria-hidden="true" />
            <span className="ctx__main">
              <span className="ctx__name-row">
                <span className="ctx__name">{ctx.name}</span>
                {ctx.isCurrent && (
                  <span className="ctx__marker" aria-hidden="true" title="kubeconfig current-context">
                    ●
                  </span>
                )}
                {ctx.isCurrent && <span className="visually-hidden"> (current)</span>}
                <span
                  role="img"
                  className={`dot ${light.tone} ctx__status-light`}
                  data-testid={`status-light-${ctx.name}`}
                  data-tone={light.tone}
                  aria-label={light.label}
                  title={light.label}
                />
              </span>
              {ctx.namespace && <span className="ctx__ns">{ctx.namespace}</span>}
              {settings?.tags && settings.tags.length > 0 && (
                <span className="ctx__tags">
                  {settings.tags.map((tag) => (
                    <span key={tag} className="badge badge--tag">
                      {tag}
                    </span>
                  ))}
                </span>
              )}
            </span>
            {settings?.readOnly && (
              <span
                className="badge badge--readonly"
                data-testid={`badge-readonly-${ctx.name}`}
                title="Read-only context"
              >
                RO
              </span>
            )}
            {settings?.protected && (
              <span
                className="badge badge--protected"
                data-testid={`badge-protected-${ctx.name}`}
                title="Protected context (type name to confirm writes)"
              >
                PROTECTED
              </span>
            )}
            <span className="badge">{authLabel(ctx.auth)}</span>
            <button
              type="button"
              className="ctx__diag-btn"
              aria-label={`Run diagnostics for ${ctx.name}`}
              title="Run connection diagnostics"
              tabIndex={-1}
              onClick={(e) => {
                e.stopPropagation();
                // Clicking focuses this tabIndex=-1 button; return focus to the row instead.
                diagnostics.open(ctx.name, e.currentTarget.closest("li"));
              }}
              // Keep Enter/Space from reaching the row, which would select the context.
              onKeyDown={(e) => {
                e.stopPropagation();
              }}
            >
              ⚕
            </button>
            {isSelected && (
              <ChevronDown
                className="ctx__chev"
                size={ICON_SIZE}
                strokeWidth={ICON_STROKE}
                aria-hidden="true"
              />
            )}
          </li>
        );
      })}
      {/* Portalled to <body>, so no DOM node is added inside the list. */}
      {createPortal(
        <DiagnosticsModal
          open={diagnostics.state.context !== null}
          context={diagnostics.state.context}
          loading={diagnostics.state.loading}
          report={diagnostics.state.report}
          error={diagnostics.state.error}
          onClose={diagnostics.close}
        />,
        document.body,
      )}
    </ul>
  );
}

function SidebarBody({ onNavigate }: { onNavigate?: SidebarNavigate | undefined }) {
  const { status, available, contexts, error } = useContextsStore();
  const isAndroid = useIsAndroid();
  const openTab = useLayoutStore((s) => s.openTab);

  if (!available) {
    return (
      <p className="empty-state" role="status">
        Not running in the Fleetglass app
      </p>
    );
  }
  switch (status) {
    case "idle":
      return (
        <p className="empty-state" role="status">
          No contexts loaded
        </p>
      );
    case "loading":
      return (
        <p className="empty-state" role="status">
          Loading contexts…
        </p>
      );
    case "error":
      return (
        <p className="error-state" role="alert">
          Cannot load contexts: {error?.message ?? "unknown error"}
        </p>
      );
    case "ready":
      if (contexts.length === 0) {
        if (isAndroid) {
          return (
            <div className="empty-state" role="status">
              <p className="sidebar__empty-text">No contexts yet.</p>
              <button
                type="button"
                className="btn sidebar__import-cta"
                data-testid="import-cta"
                onClick={() => {
                  openTab({ ...IMPORT_TAB });
                  onNavigate?.({ type: "view" });
                }}
              >
                Import a kubeconfig
              </button>
            </div>
          );
        }
        return (
          <p className="empty-state" role="status">
            No contexts in kubeconfig
          </p>
        );
      }
      // Remount when the names change so the roving index starts at the current context.
      return (
        <ContextList
          key={contexts.map((c) => c.name).join("\n")}
          contexts={contexts}
          onNavigate={onNavigate}
        />
      );
  }
}

function ClusterGroupsList({ onNavigate }: { onNavigate?: SidebarNavigate | undefined }) {
  const contexts = useContextsStore((s) => s.contexts);
  const clusterGroups = useContextsStore((s) => s.clusterGroups);
  const activeClusterSelection = useContextsStore((s) => s.activeClusterSelection);
  const selectClusterGroup = useContextsStore((s) => s.selectClusterGroup);

  const groups = [
    ...(!clusterGroups.some((g) => g.name === "All Clusters")
      ? [{ name: "All Clusters", contexts: contexts.map((c) => c.name) }]
      : []),
    ...clusterGroups,
  ];

  return (
    <div className="sidebar__section" aria-label="Cluster Groups">
      <h2 className="sidebar__title">Cluster Groups</h2>
      <ul className="cluster-group-list" aria-label="Cluster Groups">
        {groups.map((group) => {
          const isAll = group.name === "All Clusters";
          const isSelected = isAll
            ? activeClusterSelection.length === 0 ||
              (contexts.length > 0 &&
                activeClusterSelection.length === contexts.length &&
                contexts.every((c) => activeClusterSelection.includes(c.name)))
            : group.contexts.length > 0 &&
              group.contexts.length === activeClusterSelection.length &&
              group.contexts.every((c) => activeClusterSelection.includes(c));

          return (
            <li key={group.name} className="cluster-group-item">
              <button
                type="button"
                className="cluster-group-btn"
                data-selected={isSelected ? "true" : undefined}
                aria-pressed={isSelected}
                onClick={() => {
                  selectClusterGroup(group.name);
                  onNavigate?.({ type: "context" });
                }}
              >
                <span className="cluster-group-btn__name">{group.name}</span>
                <span className="badge cluster-group-btn__count">
                  {isAll ? contexts.length : group.contexts.length}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

interface NavResourceItem {
  label: string;
  icon: LucideIcon;
  kind?: string;
  view?: TabView;
  id?: string;
  badge?: string;
}

interface NavResourceGroup {
  title: string;
  items: readonly NavResourceItem[];
}

/** Items above the resource groups (mockup 2: "Overview" first). */
const TOP_ITEMS: readonly NavResourceItem[] = [
  { label: "Overview", icon: House, view: "overview", id: "overview" },
];

/** Android only (03-02c): kubeconfig import lives in the app's encrypted store. */
const ANDROID_TOP_ITEMS: readonly NavResourceItem[] = [
  ...TOP_ITEMS,
  { label: IMPORT_TAB.title, icon: Download, view: "import", id: IMPORT_TAB.id },
];

const RESOURCE_GROUPS: readonly NavResourceGroup[] = [
  {
    title: "Workloads",
    items: [
      { label: "Pods", icon: Box, kind: "Pod" },
      { label: "Deployments", icon: LayoutGrid, kind: "Deployment" },
      { label: "StatefulSets", icon: Database, kind: "StatefulSet" },
      { label: "DaemonSets", icon: Boxes, kind: "DaemonSet" },
      { label: "ReplicaSets", icon: Copy, kind: "ReplicaSet" },
      { label: "Jobs", icon: Check, kind: "Job" },
      { label: "CronJobs", icon: Clock, kind: "CronJob" },
      { label: "EndpointSlices", icon: Radio, kind: "EndpointSlice" },
      { label: "HPAs", icon: Gauge, kind: "HorizontalPodAutoscaler" },
      { label: "PDBs", icon: Sliders, kind: "PodDisruptionBudget" },
    ],
  },
  {
    title: "Network",
    items: [
      { label: "Services", icon: Network, kind: "Service" },
      { label: "Ingresses", icon: ArrowRightToLine, kind: "Ingress" },
      { label: "NetworkPolicies", icon: Shield, kind: "NetworkPolicy" },
    ],
  },
  {
    title: "Config",
    items: [
      { label: "ConfigMaps", icon: FileText, kind: "ConfigMap" },
      { label: "Secrets", icon: Lock, kind: "Secret" },
      { label: "ResourceQuotas", icon: Scale, kind: "ResourceQuota" },
      { label: "LimitRanges", icon: Sliders, kind: "LimitRange" },
      { label: "Leases", icon: Key, kind: "Lease" },
    ],
  },
  {
    title: "Storage",
    items: [
      { label: "PersistentVolumes", icon: Disc, kind: "PersistentVolume" },
      { label: "PersistentVolumeClaims", icon: HardDrive, kind: "PersistentVolumeClaim" },
      { label: "StorageClasses", icon: Layers, kind: "StorageClass" },
      { label: "VolumeAttachments", icon: Anchor, kind: "VolumeAttachment" },
    ],
  },
  {
    title: "Access Control",
    items: [
      { label: "ServiceAccounts", icon: UserCheck, kind: "ServiceAccount" },
      { label: "Roles", icon: Key, kind: "Role" },
      { label: "ClusterRoles", icon: Shield, kind: "ClusterRole" },
      { label: "RoleBindings", icon: Workflow, kind: "RoleBinding" },
      { label: "ClusterRoleBindings", icon: Workflow, kind: "ClusterRoleBinding" },
    ],
  },
  {
    title: "Cluster",
    items: [
      { label: "Nodes", icon: Monitor, kind: "Node" },
      { label: "Namespaces", icon: Layers, kind: "Namespace" },
      { label: "PriorityClasses", icon: Activity, kind: "PriorityClass" },
      { label: "RuntimeClasses", icon: Cpu, kind: "RuntimeClass" },
      { label: "Events", icon: Zap, kind: "Event" },
      { label: "MutatingWebhooks", icon: FileCheck, kind: "MutatingWebhookConfiguration" },
      { label: "ValidatingWebhooks", icon: FileCheck, kind: "ValidatingWebhookConfiguration" },
      { label: "AdmissionPolicies", icon: ShieldCheck, kind: "ValidatingAdmissionPolicy" },
      { label: "Helm releases", icon: ShipWheel, view: "helm", id: "view-helm" },
    ],
  },
  {
    title: "Ecosystem",
    items: [
      { label: "Gateways", icon: DoorOpen, kind: "Gateway", badge: "Gateway API" },
      { label: "HTTPRoutes", icon: Route, kind: "HTTPRoute", badge: "Gateway API" },
      { label: "Argo Apps", icon: GitBranch, kind: "Application", badge: "Argo CD" },
      { label: "Flux Kustomize", icon: RefreshCw, kind: "Kustomization", badge: "Flux" },
      { label: "Karpenter Pools", icon: ServerCog, kind: "NodePool", badge: "Karpenter" },
    ],
  },
];

/** Mockup icon size and stroke (`svg.i` in docs/mockups/v0.2/mock.css). */
const ICON_SIZE = 15;
const ICON_STROKE = 1.7;

const NO_CLUSTERS: readonly string[] = [];

const NOT_READY_TITLE =
  "Not up to date: still loading, reconnecting, not permitted, or not loaded on every selected cluster";

function failingText(counts: KindTotals | undefined): string | null {
  return counts && counts.failing > 0 ? `${String(counts.failing)} failing` : null;
}

/** Red dot right after the label (mockup `.nav .dot`) when the kind has failing objects. */
function NavFailDot({ counts }: { counts: KindTotals | undefined }) {
  const failing = failingText(counts);
  return failing ? <span className="nav-item-btn__fail" title={failing} aria-hidden="true" /> : null;
}

/** Right-aligned count of a nav item; nothing when the kind is not watched yet. */
function NavCount({ id, counts }: { id: string; counts: KindTotals | undefined }) {
  if (!counts) return null;
  const failing = failingText(counts);
  return (
    // Hidden from the name computation but referenced by the button's aria-describedby, so the
    // button's name stays the kind label and screen readers hear the count as its description.
    <span
      id={id}
      aria-hidden="true"
      className="nav-item-btn__meta"
      data-stale={counts.ready ? undefined : "true"}
      title={counts.ready ? undefined : NOT_READY_TITLE}
    >
      <span className="nav-item-btn__count">
        {counts.total}
        {failing && <span className="visually-hidden">, {failing}</span>}
        {!counts.ready && <span className="visually-hidden"> (not up to date)</span>}
      </span>
    </span>
  );
}

function NavigationSection({ onNavigate }: { onNavigate?: SidebarNavigate | undefined }) {
  const tabs = useLayoutStore((s) => s.tabs);
  const activeTabId = useLayoutStore((s) => s.activeTabId);
  const showKind = useLayoutStore((s) => s.showKind);
  const openTab = useLayoutStore((s) => s.openTab);
  const collapsedGroups = useSidebarStore((s) => s.collapsedGroups);
  const toggleGroup = useSidebarStore((s) => s.toggleGroup);
  const activeClusterSelection = useContextsStore((s) => s.activeClusterSelection);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  // Same clusters the resource tables query, so the counts describe what a click would show.
  const clusters = useMemo(
    () => clustersForQuery({ activeClusterSelection, selectedContext }),
    [activeClusterSelection, selectedContext],
  );
  // No polling while the whole sidebar is hidden (it stays mounted).
  const sidebarHidden = useLayoutStore((s) => s.sidebarCollapsed);
  const counts = useKindCounts(sidebarHidden ? NO_CLUSTERS : clusters);
  const isAndroid = useIsAndroid();

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const activeKind = activeTab?.kind;
  const activeView = activeTab?.view ?? "resources";

  const isItemActive = (item: NavResourceItem): boolean => {
    if (item.view !== undefined) {
      return activeTab?.view === item.view || (item.id !== undefined && activeTab?.id === item.id);
    }
    return activeView === "resources" && activeKind === item.kind;
  };

  // The group holding the active item is shown even if it was collapsed, each time the active
  // item changes (a tab switch, `g p`, the palette). This is not persisted: the stored collapse
  // state only changes when the user toggles the group, and toggling hides it again.
  const activeGroup =
    RESOURCE_GROUPS.find((g) => g.items.some((item) => isItemActive(item)))?.title ?? null;
  const activeKey = `${activeView}\n${activeKind ?? ""}\n${activeTab?.id ?? ""}`;
  const [reveal, setReveal] = useState<{ key: string; group: string | null }>({
    key: activeKey,
    group: activeGroup,
  });
  if (reveal.key !== activeKey) {
    setReveal({ key: activeKey, group: activeGroup });
  }

  const handleItemClick = (item: NavResourceItem) => {
    if (item.view === "helm") {
      openTab({ id: "view-helm", title: "Helm releases", view: "helm" });
    } else if (item.kind) {
      showKind(item.label, item.kind);
    } else if (item.view) {
      openTab({ id: item.id ?? `view-${item.view}`, title: item.label, view: item.view });
    }
    onNavigate?.({ type: "view" });
  };

  const renderNavList = (items: readonly NavResourceItem[]) =>
    items.map((item) => {
      const isActive = isItemActive(item);
      const testId = item.kind ? `nav-item-${item.kind}` : `nav-item-${item.view ?? item.id ?? item.label}`;
      const itemCounts = item.kind === undefined ? undefined : counts.get(item.kind);
      const metaId = `${testId}-count`;
      const Icon = item.icon;
      return (
        <li key={item.kind ?? item.id ?? item.label} className="nav-item">
          <button
            type="button"
            className="nav-item-btn"
            data-selected={isActive ? "true" : undefined}
            data-active={isActive ? "true" : undefined}
            aria-pressed={isActive}
            aria-describedby={itemCounts ? metaId : undefined}
            data-testid={testId}
            onClick={() => {
              handleItemClick(item);
            }}
          >
            <Icon
              className="nav-item-btn__icon"
              size={ICON_SIZE}
              strokeWidth={ICON_STROKE}
              aria-hidden="true"
            />
            <span className="nav-item-btn__label">{item.label}</span>
            <NavFailDot counts={itemCounts} />
            {item.badge && <span className="badge badge--ecosystem">{item.badge}</span>}
            <NavCount id={metaId} counts={itemCounts} />
          </button>
        </li>
      );
    });

  return (
    <>
      <div className="sidebar__section sidebar__top-nav">
        <ul className="nav-list" aria-label="Views">
          {renderNavList(isAndroid ? ANDROID_TOP_ITEMS : TOP_ITEMS)}
        </ul>
      </div>
      {RESOURCE_GROUPS.map((group) => {
        const storedCollapsed = collapsedGroups.includes(group.title);
        const revealed = storedCollapsed && reveal.group === group.title;
        const collapsed = storedCollapsed && !revealed;
        const listId = `sidebar-group-${group.title.toLowerCase()}`;
        return (
          <div
            key={group.title}
            className="sidebar__section sidebar__group"
            aria-label={group.title}
            data-collapsed={collapsed ? "true" : undefined}
          >
            <h2 className="sidebar__title">
              <button
                type="button"
                className="sidebar__group-toggle"
                aria-expanded={!collapsed}
                aria-controls={listId}
                data-testid={`sidebar-group-toggle-${group.title}`}
                onClick={() => {
                  // Any toggle of the revealed group ends the reveal; a group shown only because
                  // it holds the active item just falls back to its stored (collapsed) state.
                  if (reveal.group === group.title) setReveal({ key: activeKey, group: null });
                  if (!revealed) toggleGroup(group.title);
                }}
              >
                <span>{group.title}</span>
                <ChevronDown
                  className="sidebar__group-chevron"
                  size={ICON_SIZE}
                  strokeWidth={ICON_STROKE}
                  aria-hidden="true"
                />
              </button>
            </h2>
            <ul id={listId} className="nav-list" aria-label={group.title} hidden={collapsed}>
              {renderNavList(group.items)}
            </ul>
          </div>
        );
      })}
    </>
  );
}

/**
 * A selection in the sidebar: a context or cluster group (`context`), or a view / nav item that
 * is now the layout store's active tab (`view`). Called after the store was updated.
 */
export type SidebarNavigate = (selection: { type: "context" | "view" }) => void;

/** `onNavigate`: the phone shell closes its slide-over on a selection; desktop passes nothing. */
export function Sidebar({ onNavigate }: { onNavigate?: SidebarNavigate | undefined } = {}) {
  const collapsed = useLayoutStore((s) => s.sidebarCollapsed);
  const status = useContextsStore((s) => s.status);
  const available = useContextsStore((s) => s.available);
  const showWarnings = useContextsStore((s) => s.status === "ready");
  const warnings = useContextsStore((s) => s.warnings);
  const teamSettingsSummary = useContextsStore((s) => s.teamSettingsSummary);
  const reloadTeamSettings = useContextsStore((s) => s.reloadTeamSettings);

  return (
    <nav className="sidebar" aria-label="Contexts" data-collapsed={collapsed} hidden={collapsed}>
      <h2 className="sidebar__title">Contexts</h2>
      <SidebarBody onNavigate={onNavigate} />
      {available && status === "ready" && <ClusterGroupsList onNavigate={onNavigate} />}
      <NavigationSection onNavigate={onNavigate} />
      {showWarnings && warnings.length > 0 && (
        <ul className="notice" aria-label="Kubeconfig warnings">
          {warnings.map((w) => (
            <li key={w}>{w}</li>
          ))}
        </ul>
      )}
      {teamSettingsSummary && (
        <div className="sidebar__footer">
          <button
            type="button"
            className="badge badge--team"
            data-testid="team-settings-badge"
            title={`Team settings: ${teamSettingsSummary.loadedPath} (${String(teamSettingsSummary.appliedGroups)} groups, ${String(teamSettingsSummary.appliedContexts)} contexts)${
              teamSettingsSummary.warnings.length > 0
                ? `\nWarnings:\n${teamSettingsSummary.warnings.join("\n")}`
                : ""
            }`}
            onClick={() => {
              void reloadTeamSettings(teamSettingsSummary.loadedPath);
            }}
          >
            team: {String(teamSettingsSummary.appliedGroups)} group{teamSettingsSummary.appliedGroups === 1 ? "" : "s"}
            {teamSettingsSummary.warnings.length > 0 &&
              ` (${String(teamSettingsSummary.warnings.length)} warning${teamSettingsSummary.warnings.length === 1 ? "" : "s"})`}
          </button>
        </div>
      )}
    </nav>
  );
}
