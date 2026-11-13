import { type CSSProperties, useCallback, useEffect, useReducer, useRef, useState } from "react";
import { CommandPalette } from "../components/CommandPalette";
import { Sidebar, type SidebarNavigate } from "../components/Sidebar";
import { closeTopBackLayer, hasBackLayer, useBackLayerCount } from "../lib/backLayers";
import { type TabsAreaProps, ViewContent } from "../components/TabsArea";
import { useContextsStore } from "../stores/contexts";
import { AUDIT_TAB, IMPORT_TAB, OVERVIEW_TAB, type Tab, useLayoutStore } from "../stores/layout";
import { useIsAndroid } from "../stores/appInfo";
import { useOverviewStore } from "../stores/overview";
import { useResourceDetailStore } from "../stores/resourceDetail";
import { BottomNav, type BottomNavItem } from "./BottomNav";
import { MobileAppBar } from "./MobileAppBar";
import { MobileOverview } from "./MobileOverview";
import { MobileResourceList } from "./MobileResourceList";
import { MobileSettings } from "./MobileSettings";
import { MobileStatusStrip } from "./MobileStatusStrip";
import {
  initialMobileNav,
  mobileNavReducer,
  type MobileScreen,
  navDepth,
  topScreen,
} from "./mobileNav";
import { useHistoryLayers } from "./useHistoryLayers";

/** How long the last overview's warning count is trusted for the Events dot. */
export const EVENTS_HINT_MAX_AGE_MS = 2 * 60_000;

/**
 * Red dot on Events: some cluster counted warning events in the last overview update. The
 * overview subscription only runs while Overview is shown, so the count is dropped once it is
 * older than `EVENTS_HINT_MAX_AGE_MS` rather than kept as a stale hint.
 */
function useEventsWarning(): boolean {
  const warnedAt = useOverviewStore((s) =>
    s.data?.clusters.some((c) => (c.warningEvents1h ?? 0) > 0) === true ? s.lastUpdatedAt : null,
  );
  // The `warnedAt` whose hint has expired; a newer update shows the dot again.
  const [expired, setExpired] = useState<number | null>(null);
  useEffect(() => {
    if (warnedAt === null) return;
    const id = setTimeout(
      () => {
        setExpired(warnedAt);
      },
      Math.max(0, warnedAt + EVENTS_HINT_MAX_AGE_MS - Date.now()),
    );
    return () => {
      clearTimeout(id);
    };
  }, [warnedAt]);
  return warnedAt !== null && expired !== warnedAt;
}

/** Which bottom-nav item the screen belongs to; null for other sidebar views. */
function bottomNavItem(screen: MobileScreen, kind: string | undefined, view: string | undefined): BottomNavItem | null {
  if (screen.kind === "overview") return "overview";
  if (screen.kind === "settings") return "settings";
  if (view === "audit" || view === "import") return "settings";
  if ((view ?? "resources") !== "resources") return null;
  if (kind === "Event") return "events";
  if ((kind ?? "Pod") === "Pod") return "workloads";
  return null;
}

/**
 * Phone layout (03-03a, mockups `docs/mockups/v0.3/`): app bar, one screen at a time, short
 * status strip and bottom nav; the desktop Sidebar as a slide-over. Same stores as desktop.
 */
export function MobileShell(props: TabsAreaProps) {
  const [nav, dispatch] = useReducer(mobileNavReducer, undefined, () => {
    const { tabs, activeTabId } = useLayoutStore.getState();
    return initialMobileNav(tabs.find((t) => t.id === activeTabId));
  });
  const tabs = useLayoutStore((s) => s.tabs);
  const activeTabId = useLayoutStore((s) => s.activeTabId);
  const paletteOpen = useLayoutStore((s) => s.paletteOpen);
  const detailOpen = useResourceDetailStore((s) => s.target !== null);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const settings = useContextsStore((s) => (selectedContext ? s.settingsByContext[selectedContext] : undefined));
  const modalLayers = useBackLayerCount();
  const eventsWarning = useEventsWarning();
  // Exec shell and port-forward are not in Android 0.3 (docs/TASKS.md); a narrow desktop
  // window keeps them.
  const isAndroid = useIsAndroid();
  const top = topScreen(nav);

  // A tab activated elsewhere (command palette, a "Connect" button) becomes the shown screen.
  const firstSync = useRef(true);
  useEffect(() => {
    if (firstSync.current) {
      firstSync.current = false;
      return;
    }
    const tab = useLayoutStore.getState().tabs.find((t) => t.id === activeTabId);
    if (tab === undefined) return;
    if (tab.view === "overview") dispatch({ type: "top", screen: { kind: "overview" } });
    else dispatch({ type: "view", tabId: tab.id });
  }, [activeTabId]);

  const showOverview = useCallback(() => {
    useLayoutStore.getState().openTab({ ...OVERVIEW_TAB });
    dispatch({ type: "top", screen: { kind: "overview" } });
  }, []);

  const onBottomNav = useCallback(
    (item: BottomNavItem) => {
      const layout = useLayoutStore.getState();
      switch (item) {
        case "overview":
          showOverview();
          break;
        case "workloads":
          dispatch({ type: "view", tabId: layout.showKind("Pods", "Pod") });
          break;
        case "events":
          dispatch({ type: "view", tabId: layout.showKind("Events", "Event") });
          break;
        case "settings":
          dispatch({ type: "top", screen: { kind: "settings" } });
          break;
      }
    },
    [showOverview],
  );

  const openSub = useCallback((tab: Tab) => {
    const id = useLayoutStore.getState().openTab({ ...tab });
    dispatch({ type: "view", tabId: id, sub: true });
  }, []);

  const openSlideOver = useCallback(() => {
    // The slide-over is the desktop Sidebar, which hides itself while collapsed (Ctrl+B).
    if (useLayoutStore.getState().sidebarCollapsed) useLayoutStore.setState({ sidebarCollapsed: false });
    dispatch({ type: "slideOver", open: true });
  }, []);
  const closeSlideOver = useCallback(() => {
    dispatch({ type: "slideOver", open: false });
  }, []);

  // Called after the Sidebar updated the stores, so the layout store holds the selection.
  const onSidebarNavigate = useCallback<SidebarNavigate>(({ type }) => {
    if (type === "view") {
      const layout = useLayoutStore.getState();
      const tab = layout.tabs.find((t) => t.id === layout.activeTabId);
      if (tab?.view === "overview") dispatch({ type: "top", screen: { kind: "overview" } });
      else if (tab) dispatch({ type: "view", tabId: tab.id });
      // Re-selecting the shown view changes nothing above; still close the slide-over.
      dispatch({ type: "slideOver", open: false });
      return;
    }
    dispatch({ type: "slideOver", open: false });
  }, []);

  // Back: a modal dialog (confirm, diagnostics, diff), the palette, the resource detail, then
  // the slide-over / screen stack. A modal that refuses (a running action) keeps back here:
  // nothing below it closes and the history entry is pushed again.
  const onBack = useCallback(() => {
    if (hasBackLayer()) {
      closeTopBackLayer();
      return;
    }
    const layout = useLayoutStore.getState();
    if (layout.paletteOpen) {
      layout.closePalette();
      return;
    }
    const detail = useResourceDetailStore.getState();
    if (detail.target !== null) {
      detail.close();
      return;
    }
    dispatch({ type: "back" });
  }, []);
  useHistoryLayers(navDepth(nav, modalLayers + (paletteOpen ? 1 : 0) + (detailOpen ? 1 : 0)), onBack);

  // Keep the layout store's active tab on the shown screen, so the sidebar highlights it and
  // re-selecting it later is a change.
  useEffect(() => {
    const layout = useLayoutStore.getState();
    if (top.kind === "view") layout.setActiveTab(top.tabId);
    else if (top.kind === "overview" && layout.tabs.find((t) => t.id === layout.activeTabId)?.view !== "overview") {
      layout.openTab({ ...OVERVIEW_TAB });
    }
  }, [top]);

  useEffect(() => {
    if (!nav.slideOver) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeSlideOver();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [nav.slideOver, closeSlideOver]);

  const topTab = top.kind === "view" ? tabs.find((t) => t.id === top.tabId) : undefined;
  let screen;
  if (top.kind === "overview") {
    screen = (
      <MobileOverview
        onOpenPods={() => {
          onBottomNav("workloads");
        }}
      />
    );
  } else if (top.kind === "settings") {
    screen = (
      <MobileSettings
        onOpenImport={() => {
          openSub(IMPORT_TAB);
        }}
        onOpenAudit={() => {
          openSub(AUDIT_TAB);
        }}
      />
    );
  } else if (topTab === undefined) {
    screen = (
      <p className="empty-state" role="status">
        This view was closed.
      </p>
    );
  } else if ((topTab.view ?? "resources") === "resources") {
    screen = (
      <div className="m-screen m-view" data-testid="m-view" data-view="resources">
        <MobileResourceList
          key={topTab.id}
          kind={topTab.kind ?? "Pod"}
          remoteOps={!isAndroid}
          {...(topTab.textFilter !== undefined ? { initialTextFilter: topTab.textFilter } : {})}
        />
      </div>
    );
  } else {
    screen = (
      <div className="m-screen m-view" data-testid="m-view" data-view={topTab.view ?? "resources"}>
        <ViewContent
          key={topTab.id}
          view={topTab.view ?? "resources"}
          kind={topTab.kind ?? "Pod"}
          {...(topTab.textFilter !== undefined ? { textFilter: topTab.textFilter } : {})}
          {...props}
        />
      </div>
    );
  }

  return (
    <div
      className="m-shell"
      data-testid="mobile-shell"
      style={settings?.colour ? ({ "--context-accent": settings.colour } as CSSProperties) : undefined}
    >
      {settings?.readOnly && (
        <div className="context-readonly-banner" data-testid="active-context-readonly">
          Read-only context ({selectedContext})
        </div>
      )}
      <MobileAppBar
        menuOpen={nav.slideOver}
        onMenu={openSlideOver}
        onSearch={() => {
          useLayoutStore.getState().openPalette();
        }}
      />
      <main className="m-content">{screen}</main>
      <MobileStatusStrip
        freshness={
          top.kind === "overview"
            ? "overview"
            : (topTab?.view ?? (top.kind === "view" ? "resources" : null)) === "resources"
              ? "table"
              : "none"
        }
      />
      <BottomNav
        active={bottomNavItem(top, topTab?.kind, topTab?.view)}
        eventsWarning={eventsWarning}
        onSelect={onBottomNav}
      />
      {nav.slideOver && (
        <>
          <div className="m-scrim" data-testid="m-scrim" aria-hidden="true" onClick={closeSlideOver} />
          <div
            className="m-slide"
            role="dialog"
            aria-modal="true"
            aria-label="Navigation"
            data-testid="m-slide"
          >
            <Sidebar onNavigate={onSidebarNavigate} />
          </div>
        </>
      )}
      <CommandPalette />
    </div>
  );
}
