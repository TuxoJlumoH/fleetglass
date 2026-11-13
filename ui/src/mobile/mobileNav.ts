/**
 * Mobile navigation state (03-03a): a stack of screens plus the slide-over, so the Android back
 * gesture (history back in the Tauri WebView) closes the top layer. Pure; the shell keeps the
 * browser history in step with `navDepth` (see `useHistoryLayers`).
 *
 * The command palette and the resource detail are layers too, but their open state lives in
 * their own stores (layout `paletteOpen`, resourceDetail `target`); the shell closes those first.
 */

/** A full screen. The first one is always Overview (the start destination). */
export type MobileScreen =
  | { kind: "overview" }
  | { kind: "settings" }
  /** A layout-store tab (Pods, Events, Helm, Audit log, …) shown in the mobile content area. */
  | { kind: "view"; tabId: string };

export interface MobileNavState {
  screens: MobileScreen[];
  slideOver: boolean;
}

export type MobileNavAction =
  /** Bottom nav "Overview" / "Settings": a top-level destination; Overview clears the stack. */
  | { type: "top"; screen: { kind: "overview" } | { kind: "settings" } }
  /**
   * A tab became the one to show (bottom nav, sidebar, palette). `sub` pushes it above Settings
   * (Audit log / Import opened from Settings); otherwise it replaces whatever is above Overview.
   */
  | { type: "view"; tabId: string; sub?: boolean }
  | { type: "slideOver"; open: boolean }
  /** Close the slide-over, else pop a screen; a no-op on Overview alone. */
  | { type: "back" };

export const OVERVIEW_SCREEN: MobileScreen = { kind: "overview" };

export function initialMobileNav(activeTab?: { id: string; view?: string }): MobileNavState {
  const screens: MobileScreen[] = [OVERVIEW_SCREEN];
  if (activeTab !== undefined && activeTab.view !== "overview") {
    screens.push({ kind: "view", tabId: activeTab.id });
  }
  return { screens, slideOver: false };
}

export function topScreen(state: MobileNavState): MobileScreen {
  return state.screens[state.screens.length - 1] ?? OVERVIEW_SCREEN;
}

/**
 * Number of layers back can close, one history entry each: screens above Overview, the
 * slide-over, and `extraLayers` (palette / detail). 0 means back leaves the app.
 */
export function navDepth(state: MobileNavState, extraLayers = 0): number {
  return state.screens.length - 1 + (state.slideOver ? 1 : 0) + extraLayers;
}

export function sameScreen(a: MobileScreen, b: MobileScreen): boolean {
  if (a.kind !== b.kind) return false;
  return a.kind !== "view" || (b.kind === "view" && a.tabId === b.tabId);
}

/** Navigating always dismisses the slide-over (a selection closes it). */
function navigate(state: MobileNavState, screens: MobileScreen[]): MobileNavState {
  const same =
    screens.length === state.screens.length &&
    screens.every((s, i) => {
      const prev = state.screens[i];
      return prev !== undefined && sameScreen(s, prev);
    });
  if (same && !state.slideOver) return state;
  return { screens: same ? state.screens : screens, slideOver: false };
}

export function mobileNavReducer(state: MobileNavState, action: MobileNavAction): MobileNavState {
  switch (action.type) {
    case "top":
      return navigate(
        state,
        action.screen.kind === "overview" ? [OVERVIEW_SCREEN] : [OVERVIEW_SCREEN, action.screen],
      );
    case "view": {
      const screen: MobileScreen = { kind: "view", tabId: action.tabId };
      const top = topScreen(state);
      if (sameScreen(top, screen)) return navigate(state, state.screens);
      if (action.sub === true && top.kind === "settings") return navigate(state, [...state.screens, screen]);
      return navigate(state, [OVERVIEW_SCREEN, screen]);
    }
    case "slideOver":
      return state.slideOver === action.open ? state : { ...state, slideOver: action.open };
    case "back":
      if (state.slideOver) return { ...state, slideOver: false };
      if (state.screens.length > 1) return { ...state, screens: state.screens.slice(0, -1) };
      return state;
  }
}

/** How to move the history from `current` pushed entries to `target`; null when in step. */
export type HistoryStep = { push: number } | { go: number } | null;

export function historyPlan(current: number, target: number): HistoryStep {
  if (target > current) return { push: target - current };
  if (target < current) return { go: target - current };
  return null;
}

/** Depth stored in a history entry's state by `useHistoryLayers`; 0 for foreign entries. */
export function historyDepthOf(state: unknown): number {
  if (typeof state === "object" && state !== null && "fgMobile" in state) {
    const n = state.fgMobile;
    if (typeof n === "number" && Number.isInteger(n) && n >= 0) return n;
  }
  return 0;
}
