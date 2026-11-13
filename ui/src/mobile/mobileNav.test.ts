import { describe, expect, it } from "vitest";
import {
  historyDepthOf,
  historyPlan,
  initialMobileNav,
  mobileNavReducer,
  type MobileNavState,
  navDepth,
  topScreen,
} from "./mobileNav";
import { pickMobileLayout } from "./useMobileLayout";

function run(state: MobileNavState, ...actions: Parameters<typeof mobileNavReducer>[1][]): MobileNavState {
  return actions.reduce(mobileNavReducer, state);
}

describe("mobile nav stack", () => {
  it("starts on Overview with nothing to close", () => {
    const s = initialMobileNav();
    expect(topScreen(s)).toEqual({ kind: "overview" });
    expect(navDepth(s)).toBe(0);
  });

  it("starts above Overview when a non-overview tab is active", () => {
    const s = initialMobileNav({ id: "pods", view: "resources" });
    expect(topScreen(s)).toEqual({ kind: "view", tabId: "pods" });
    expect(navDepth(s)).toBe(1);
    expect(navDepth(initialMobileNav({ id: "overview", view: "overview" }))).toBe(0);
  });

  it("switching top-level destinations keeps one screen above Overview", () => {
    const s = run(
      initialMobileNav(),
      { type: "view", tabId: "pods" },
      { type: "view", tabId: "events" },
      { type: "top", screen: { kind: "settings" } },
    );
    expect(s.screens).toEqual([{ kind: "overview" }, { kind: "settings" }]);
    expect(navDepth(s)).toBe(1);
    expect(run(s, { type: "top", screen: { kind: "overview" } }).screens).toEqual([{ kind: "overview" }]);
  });

  it("sub-screens push above Settings and pop back to it", () => {
    const s = run(
      initialMobileNav(),
      { type: "top", screen: { kind: "settings" } },
      { type: "view", tabId: "view-audit", sub: true },
    );
    expect(s.screens).toHaveLength(3);
    const back = run(s, { type: "back" });
    expect(topScreen(back)).toEqual({ kind: "settings" });
    expect(topScreen(run(back, { type: "back" }))).toEqual({ kind: "overview" });
  });

  it("`sub` outside Settings replaces the screen above Overview", () => {
    const s = run(initialMobileNav(), { type: "view", tabId: "pods" }, { type: "view", tabId: "x", sub: true });
    expect(s.screens).toEqual([{ kind: "overview" }, { kind: "view", tabId: "x" }]);
  });

  it("back closes the slide-over before popping a screen, and is a no-op at the root", () => {
    const s = run(initialMobileNav(), { type: "view", tabId: "pods" }, { type: "slideOver", open: true });
    expect(navDepth(s)).toBe(2);
    const a = run(s, { type: "back" });
    expect(a.slideOver).toBe(false);
    expect(topScreen(a)).toEqual({ kind: "view", tabId: "pods" });
    const b = run(a, { type: "back" });
    expect(navDepth(b)).toBe(0);
    expect(run(b, { type: "back" })).toBe(b);
  });

  it("navigating closes the slide-over; re-selecting the shown screen only closes it", () => {
    const open = run(initialMobileNav(), { type: "view", tabId: "pods" }, { type: "slideOver", open: true });
    const same = run(open, { type: "view", tabId: "pods" });
    expect(same.slideOver).toBe(false);
    expect(same.screens).toBe(open.screens);
    const other = run(open, { type: "view", tabId: "deploy" });
    expect(other).toEqual({ screens: [{ kind: "overview" }, { kind: "view", tabId: "deploy" }], slideOver: false });
  });

  it("unchanged navigation returns the same state object", () => {
    const s = run(initialMobileNav(), { type: "view", tabId: "pods" });
    expect(run(s, { type: "view", tabId: "pods" })).toBe(s);
    expect(run(s, { type: "slideOver", open: false })).toBe(s);
  });

  it("counts extra layers (palette, detail) in the depth", () => {
    expect(navDepth(initialMobileNav(), 2)).toBe(2);
  });
});

describe("history sync", () => {
  it("plans pushes and rewinds", () => {
    expect(historyPlan(0, 2)).toEqual({ push: 2 });
    expect(historyPlan(3, 1)).toEqual({ go: -2 });
    expect(historyPlan(1, 1)).toBeNull();
  });

  it("reads only its own history state", () => {
    expect(historyDepthOf({ fgMobile: 3 })).toBe(3);
    expect(historyDepthOf(null)).toBe(0);
    expect(historyDepthOf({ other: 1 })).toBe(0);
    expect(historyDepthOf({ fgMobile: -1 })).toBe(0);
    expect(historyDepthOf({ fgMobile: "2" })).toBe(0);
  });
});

describe("shell selection rule", () => {
  it("phone width anywhere, Android below tablet width, desktop otherwise", () => {
    expect(pickMobileLayout("linux", true, true)).toBe("mobile");
    expect(pickMobileLayout("android", false, true)).toBe("mobile");
    expect(pickMobileLayout("android", false, false)).toBe("desktop");
    expect(pickMobileLayout("linux", false, true)).toBe("desktop");
  });

  it("waits for the platform only where it decides the layout (600–899 px)", () => {
    expect(pickMobileLayout(null, false, true)).toBe("pending");
    expect(pickMobileLayout(null, true, true)).toBe("mobile");
    expect(pickMobileLayout(null, false, false)).toBe("desktop");
  });
});
