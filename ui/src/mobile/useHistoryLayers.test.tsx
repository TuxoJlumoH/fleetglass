import { act, renderHook, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { historyDepthOf } from "./mobileNav";
import { GO_SETTLE_MS, historyLayersBusy, useHistoryLayers } from "./useHistoryLayers";

const depthNow = () => historyDepthOf(window.history.state);

/** History back as the Android gesture does it; resolves after the popstate is handled. */
async function back() {
  await act(async () => {
    const popped = new Promise<void>((resolve) => {
      window.addEventListener(
        "popstate",
        () => {
          resolve();
        },
        { once: true },
      );
    });
    window.history.back();
    await popped;
  });
}

async function settled() {
  await waitFor(() => {
    expect(historyLayersBusy()).toBe(false);
  });
}

beforeEach(async () => {
  await settled();
  // A foreign entry under ours, as the app's start page is on Android.
  window.history.pushState({ start: true }, "");
});

afterEach(() => {
  vi.restoreAllMocks();
});

function mount(depth: number, onBack = vi.fn(), strict = false) {
  const hook = renderHook(({ d }: { d: number }) => { useHistoryLayers(d, onBack); }, {
    initialProps: { d: depth },
    ...(strict ? { wrapper: StrictMode } : {}),
  });
  return { ...hook, onBack };
}

describe("useHistoryLayers", () => {
  it("pushes one entry per layer and calls onBack once per popped entry", async () => {
    const { rerender, onBack, unmount } = mount(0);
    rerender({ d: 2 });
    expect(depthNow()).toBe(2);
    await back();
    expect(onBack).toHaveBeenCalledTimes(1);
    unmount();
    await settled();
  });

  it("pushes once under StrictMode", async () => {
    const before = window.history.length;
    const { unmount } = mount(1, vi.fn(), true);
    await settled();
    expect(depthNow()).toBe(1);
    expect(window.history.length).toBe(before + 1);
    unmount();
    await settled();
    expect(depthNow()).toBe(0);
  });

  it("a UI rewind (scrim / bottom nav) lands on the start entry, so the next back exits", async () => {
    const { rerender, onBack, unmount } = mount(2);
    rerender({ d: 0 });
    await settled();
    expect(depthNow()).toBe(0);
    expect(window.history.state).toEqual({ start: true });
    expect(onBack).not.toHaveBeenCalled();
    // The next back leaves our entries: nothing of ours is closed.
    await back();
    expect(onBack).not.toHaveBeenCalled();
    unmount();
  });

  it("fast taps while a rewind is in flight end at the last depth", async () => {
    const { rerender, onBack, unmount } = mount(2);
    rerender({ d: 0 });
    expect(historyLayersBusy()).toBe(true);
    rerender({ d: 1 });
    rerender({ d: 3 });
    rerender({ d: 1 });
    await settled();
    expect(depthNow()).toBe(1);
    expect(onBack).not.toHaveBeenCalled();
    unmount();
    await settled();
  });

  it("clamps a rewind to the depth recorded in history.state", async () => {
    // Only one of our entries is really there (e.g. the user navigated back outside the hook).
    const { rerender, unmount } = mount(1);
    const go = vi.spyOn(window.history, "go");
    window.history.replaceState({ fgMobile: 1 }, "");
    rerender({ d: 1 });
    rerender({ d: 0 });
    expect(go).toHaveBeenCalledWith(-1);
    await settled();
    unmount();
    await settled();
  });

  it("stops waiting for a lost popstate after the settle timeout", async () => {
    const { rerender, unmount } = mount(2);
    const go = vi.spyOn(window.history, "go").mockImplementation(() => undefined);
    rerender({ d: 0 });
    expect(historyLayersBusy()).toBe(true);
    // No popstate comes; after GO_SETTLE_MS the depth is re-read and the rewind retried.
    await waitFor(
      () => {
        expect(go.mock.calls.length).toBeGreaterThanOrEqual(2);
      },
      { timeout: GO_SETTLE_MS * 3 },
    );
    go.mockRestore();
    await waitFor(
      () => {
        expect(depthNow()).toBe(0);
      },
      { timeout: GO_SETTLE_MS * 3 },
    );
    unmount();
    await settled();
  });

  it("rewinds its entries on unmount, and a remount starts from history.state", async () => {
    const first = mount(3);
    expect(depthNow()).toBe(3);
    first.unmount();
    await settled();
    expect(depthNow()).toBe(0);
    expect(window.history.state).toEqual({ start: true });

    // Remount right after an unmount (rotation back): no rewind, no extra entries.
    const second = mount(2);
    expect(depthNow()).toBe(2);
    second.unmount();
    const push = vi.spyOn(window.history, "pushState");
    const go = vi.spyOn(window.history, "go");
    const third = mount(2);
    await settled();
    expect(depthNow()).toBe(2);
    expect(push).not.toHaveBeenCalled();
    expect(go).not.toHaveBeenCalled();
    third.unmount();
    await settled();
    expect(depthNow()).toBe(0);
  });

  it("re-pushes the entry when the top layer refuses to close", async () => {
    const { onBack, unmount } = mount(1);
    await back();
    expect(onBack).toHaveBeenCalledTimes(1);
    await settled();
    // Depth is still 1 (nothing closed), so the entry is back.
    expect(depthNow()).toBe(1);
    unmount();
    await settled();
  });
});
