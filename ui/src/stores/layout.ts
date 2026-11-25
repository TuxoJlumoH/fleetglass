import { create } from "zustand";

/** What a tab (or the secondary split pane) shows. */
export type TabView =
  | "overview"
  | "resources"
  | "apps"
  | "helm"
  | "audit"
  | "plugins"
  | "import"
  | "logs"
  | "yaml";

/** The Android "Import contexts" view (03-02c); one tab, reopened by this id. */
export const IMPORT_TAB: Tab = { id: "view-import", title: "Import contexts", view: "import" };
/** The audit log view (02-03); one tab, reopened by this id. */
export const AUDIT_TAB: Tab = { id: "view-audit", title: "Audit log", view: "audit" };

export interface Tab {
  id: string;
  title: string;
  /** Missing means "resources" (tabs created before views existed). */
  view?: TabView;
  kind?: string;
  cluster?: string | null;
  namespace?: string | null;
  textFilter?: string;
}

export type SplitMode = "none" | "horizontal" | "vertical";

/** One of the two panes of the split view; "primary" is the only pane while the split is closed. */
export type PaneId = "primary" | "secondary";

interface LayoutState {
  sidebarCollapsed: boolean;
  tabs: Tab[];
  activeTabId: string;
  paletteOpen: boolean;
  historyBack: string[];
  historyForward: string[];
  splitMode: SplitMode;
  /** View shown in the secondary split pane; independent of the active tab. */
  secondaryView: TabView;
  /** Query state of the secondary pane's resource table (the primary keeps it per tab). */
  secondaryCluster: string | null;
  /** Comma-separated namespace filter, like `Tab.namespace`. */
  secondaryNamespace: string | null;
  secondaryKind: string;
  secondaryTextFilter: string;
  /** Bumped when a tab is dropped onto the secondary pane; remounts its view with the new state. */
  secondaryResetToken: number;
  /** Pane that receives keyboard shortcuts and drives the palette's selection. */
  activePane: PaneId;
  toggleSidebar: () => void;
  setActiveTab: (id: string) => void;
  /** Activate the tab `delta` positions away from the active one, wrapping around. */
  cycleTab: (delta: number) => void;
  openPalette: () => void;
  closePalette: () => void;
  togglePalette: () => void;
  openTab: (tab: Omit<Tab, "id"> & { id?: string }) => string;
  /** Activate an existing resources tab for `kind`, or open one when none exists. */
  showKind: (title: string, kind: string) => string;
  closeTab: (id: string) => void;
  updateTabState: (id: string, patch: Partial<Tab>) => void;
  reorderTabs: (sourceId: string, targetId: string) => void;
  goBack: () => void;
  goForward: () => void;
  setSplitMode: (mode: SplitMode) => void;
  toggleSplit: (mode?: "horizontal" | "vertical") => void;
  /** Swaps horizontal and vertical; does nothing while the split is closed. */
  toggleSplitOrientation: () => void;
  setSecondaryView: (view: TabView) => void;
  setSecondaryCluster: (cluster: string | null) => void;
  setSecondaryNamespace: (namespace: string | null) => void;
  setSecondaryKind: (kind: string) => void;
  setSecondaryTextFilter: (filter: string) => void;
  /** Shows `tab`'s view and query state in the secondary pane (a tab dropped onto it). */
  loadTabIntoSecondary: (tab: Tab) => void;
  setActivePane: (pane: PaneId) => void;
  switchPane: () => void;
}

/** The initial tab (02-08b); reopened with this id by the sidebar / palette "Overview". */
export const OVERVIEW_TAB: Tab = {
  id: "overview",
  title: "Overview",
  view: "overview",
};

let tabCounter = 1;

function nextTabId(existing: Tab[]): string {
  let candidate = `tab-${String(tabCounter)}`;
  tabCounter += 1;
  while (existing.some((t) => t.id === candidate)) {
    candidate = `tab-${String(tabCounter)}`;
    tabCounter += 1;
  }
  return candidate;
}

export const initialLayoutState = {
  sidebarCollapsed: false,
  tabs: [OVERVIEW_TAB],
  activeTabId: OVERVIEW_TAB.id,
  paletteOpen: false,
  historyBack: [] as string[],
  historyForward: [] as string[],
  splitMode: "none" as SplitMode,
  secondaryView: "apps" as TabView,
  secondaryCluster: null as string | null,
  secondaryNamespace: null as string | null,
  secondaryKind: "Pod",
  secondaryTextFilter: "",
  secondaryResetToken: 0,
  activePane: "primary" as PaneId,
};

export const useLayoutStore = create<LayoutState>()((set, get) => ({
  ...initialLayoutState,
  setSplitMode: (mode) => {
    set(mode === "none" ? { splitMode: mode, activePane: "primary" } : { splitMode: mode });
  },
  toggleSplit: (mode = "horizontal") => {
    set((s) =>
      s.splitMode === "none" ? { splitMode: mode } : { splitMode: "none", activePane: "primary" },
    );
  },
  toggleSplitOrientation: () => {
    set((s) => {
      if (s.splitMode === "none") return {};
      return { splitMode: s.splitMode === "horizontal" ? "vertical" : "horizontal" };
    });
  },
  setSecondaryView: (view) => {
    set({ secondaryView: view });
  },
  setSecondaryCluster: (cluster) => {
    set({ secondaryCluster: cluster });
  },
  setSecondaryNamespace: (namespace) => {
    set({ secondaryNamespace: namespace });
  },
  setSecondaryKind: (kind) => {
    set({ secondaryKind: kind });
  },
  setSecondaryTextFilter: (filter) => {
    set({ secondaryTextFilter: filter });
  },
  loadTabIntoSecondary: (tab) => {
    set((s) => ({
      secondaryView: tab.view ?? "resources",
      secondaryKind: tab.kind ?? s.secondaryKind,
      secondaryCluster: tab.cluster ?? null,
      secondaryTextFilter: tab.textFilter ?? "",
      secondaryNamespace: tab.namespace ?? null,
      secondaryResetToken: s.secondaryResetToken + 1,
    }));
  },
  setActivePane: (pane) => {
    set({ activePane: pane });
  },
  switchPane: () => {
    set((s) => ({ activePane: s.activePane === "primary" ? "secondary" : "primary" }));
  },
  reorderTabs: (sourceId, targetId) => {
    const { tabs } = get();
    const sourceIndex = tabs.findIndex((t) => t.id === sourceId);
    const targetIndex = tabs.findIndex((t) => t.id === targetId);
    if (sourceIndex === -1 || targetIndex === -1 || sourceIndex === targetIndex) return;
    const nextTabs = [...tabs];
    const [moved] = nextTabs.splice(sourceIndex, 1);
    if (moved) {
      nextTabs.splice(targetIndex, 0, moved);
      set({ tabs: nextTabs });
    }
  },
  toggleSidebar: () => {
    set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed }));
  },
  setActiveTab: (id) => {
    const { tabs, activeTabId, historyBack } = get();
    if (!tabs.some((t) => t.id === id)) return;
    if (id === activeTabId) return;
    set({
      activeTabId: id,
      historyBack: [...historyBack, activeTabId],
      historyForward: [],
    });
  },
  cycleTab: (delta) => {
    const { tabs, activeTabId, historyBack } = get();
    if (tabs.length === 0) return;
    const current = Math.max(
      0,
      tabs.findIndex((t) => t.id === activeTabId),
    );
    const next = tabs[(((current + delta) % tabs.length) + tabs.length) % tabs.length];
    if (next && next.id !== activeTabId) {
      set({
        activeTabId: next.id,
        historyBack: [...historyBack, activeTabId],
        historyForward: [],
      });
    }
  },
  openPalette: () => {
    set({ paletteOpen: true });
  },
  closePalette: () => {
    set({ paletteOpen: false });
  },
  togglePalette: () => {
    set((s) => ({ paletteOpen: !s.paletteOpen }));
  },
  openTab: (tabInput) => {
    const { tabs, activeTabId, historyBack } = get();
    const resolvedId = tabInput.id ?? nextTabId(tabs);
    const existingIndex = tabs.findIndex((t) => t.id === resolvedId);

    if (existingIndex !== -1) {
      const nextTabs = tabs.map((t, idx) =>
        idx === existingIndex ? { ...t, ...tabInput, id: resolvedId } : t,
      );
      set({
        tabs: nextTabs,
        activeTabId: resolvedId,
        historyBack:
          activeTabId !== resolvedId ? [...historyBack, activeTabId] : historyBack,
        historyForward: activeTabId !== resolvedId ? [] : get().historyForward,
      });
      return resolvedId;
    }

    const newTab: Tab = {
      view: "resources",
      kind: "Pod",
      cluster: null,
      namespace: null,
      textFilter: "",
      ...tabInput,
      id: resolvedId,
    };

    set({
      tabs: [...tabs, newTab],
      activeTabId: resolvedId,
      historyBack:
        activeTabId !== resolvedId ? [...historyBack, activeTabId] : historyBack,
      historyForward: [],
    });
    return resolvedId;
  },
  showKind: (title, kind) => {
    const { tabs, activeTabId, openTab } = get();
    const isKindTab = (t: Tab) => (t.view ?? "resources") === "resources" && (t.kind ?? "Pod") === kind;
    const active = tabs.find((t) => t.id === activeTabId);
    const existing = active !== undefined && isKindTab(active) ? active : tabs.find(isKindTab);
    if (existing !== undefined) {
      get().setActiveTab(existing.id);
      return existing.id;
    }
    return openTab({ title, kind, view: "resources" });
  },
  closeTab: (id) => {
    const { tabs, activeTabId, historyBack, historyForward } = get();
    if (tabs.length <= 1) return;
    const idx = tabs.findIndex((t) => t.id === id);
    if (idx === -1) return;

    const nextTabs = tabs.filter((t) => t.id !== id);
    const nextBack = historyBack.filter((h) => h !== id);
    const nextForward = historyForward.filter((h) => h !== id);

    if (activeTabId === id) {
      const neighbour = nextTabs[Math.min(idx, nextTabs.length - 1)] ?? nextTabs[0];
      set({
        tabs: nextTabs,
        activeTabId: neighbour ? neighbour.id : activeTabId,
        historyBack: nextBack,
        historyForward: nextForward,
      });
    } else {
      set({
        tabs: nextTabs,
        historyBack: nextBack,
        historyForward: nextForward,
      });
    }
  },
  updateTabState: (id, patch) => {
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === id ? { ...t, ...patch, id: t.id } : t)),
    }));
  },
  goBack: () => {
    const { tabs, activeTabId, historyBack, historyForward } = get();
    const nextBack = [...historyBack];
    while (nextBack.length > 0) {
      const candidate = nextBack.pop();
      if (
        candidate !== undefined &&
        candidate !== activeTabId &&
        tabs.some((t) => t.id === candidate)
      ) {
        set({
          activeTabId: candidate,
          historyBack: nextBack,
          historyForward: [...historyForward, activeTabId],
        });
        return;
      }
    }
  },
  goForward: () => {
    const { tabs, activeTabId, historyBack, historyForward } = get();
    const nextForward = [...historyForward];
    while (nextForward.length > 0) {
      const candidate = nextForward.pop();
      if (
        candidate !== undefined &&
        candidate !== activeTabId &&
        tabs.some((t) => t.id === candidate)
      ) {
        set({
          activeTabId: candidate,
          historyBack: [...historyBack, activeTabId],
          historyForward: nextForward,
        });
        return;
      }
    }
  },
}));
