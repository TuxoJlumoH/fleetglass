import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KindCount } from "../generated/bindings";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";
import {
  SIDEBAR_COLLAPSED_STORAGE_KEY,
  parseCollapsedGroups,
  reloadSidebarStore,
} from "../stores/sidebar";
import { Sidebar } from "./Sidebar";
import {
  KIND_COUNTS_POLL_MS,
  aggregateKindCounts,
  sameKindCounts,
  useKindCounts,
} from "./useKindCounts";

type CountsByCluster = Record<string, KindCount[] | Error>;

/** Pretends to be inside Tauri; answers `kind_counts` from `byCluster` and records the calls. */
function mockCounts(byCluster: CountsByCluster): string[] {
  (globalThis as { isTauri?: boolean }).isTauri = true;
  mockWindows("main");
  const calls: string[] = [];
  mockIPC((cmd, args) => {
    if (cmd !== "kind_counts") return undefined;
    const cluster = (args as { cluster: string }).cluster;
    calls.push(cluster);
    const res = byCluster[cluster] ?? [];
    if (res instanceof Error) throw res;
    return res;
  });
  return calls;
}

/** Lets the mocked IPC promises and React updates settle without advancing the poll timer. */
async function flush() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

function count(kind: string, total: number, failing = 0, ready = true): KindCount {
  return { kind, total, failing, ready };
}

describe("Sidebar kind counts", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    localStorage.clear();
    reloadSidebarStore();
    useLayoutStore.setState(initialLayoutState);
    useContextsStore.setState({ ...initialContextsState, selectedContext: "dev" });
  });

  afterEach(() => {
    vi.useRealTimers();
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
    localStorage.clear();
    reloadSidebarStore();
  });

  it("renders right-aligned counts and a red dot for failing kinds", async () => {
    mockCounts({ dev: [count("Pod", 42, 3), count("Deployment", 8)] });
    render(<Sidebar />);
    await flush();

    const pods = screen.getByTestId("nav-item-Pod");
    expect(within(pods).getByText("42")).toHaveClass("nav-item-btn__count");
    const dot = pods.querySelector(".nav-item-btn__fail");
    expect(dot).toHaveAttribute("title", "3 failing");
    // Mockup order: label, dot right after it, then the count pushed right.
    const label = pods.querySelector(".nav-item-btn__label");
    expect(label?.nextElementSibling).toBe(dot);
    expect(pods.querySelector(".nav-item-btn__meta .nav-item-btn__fail")).toBeNull();
    // The count describes the button; it is not part of its name.
    expect(pods).toHaveAccessibleName("Pods");
    expect(pods).toHaveAccessibleDescription("42, 3 failing");

    const deployments = screen.getByTestId("nav-item-Deployment");
    expect(within(deployments).getByText("8")).toBeInTheDocument();
    expect(deployments.querySelector(".nav-item-btn__fail")).toBeNull();
  });

  it("shows no count for a kind that is not watched (no entry, not 0)", async () => {
    mockCounts({ dev: [count("Pod", 5)] });
    render(<Sidebar />);
    await flush();

    const services = screen.getByTestId("nav-item-Service");
    expect(services.querySelector(".nav-item-btn__meta")).toBeNull();
    expect(services).not.toHaveTextContent("0");
    expect(services).not.toHaveAttribute("aria-describedby");
  });

  it("dims a not-ready count and explains it in a title", async () => {
    mockCounts({ dev: [count("Secret", 0, 0, false), count("Pod", 2)] });
    render(<Sidebar />);
    await flush();

    const meta = screen.getByTestId("nav-item-Secret").querySelector(".nav-item-btn__meta");
    expect(meta).not.toBeNull();
    expect(meta).toHaveAttribute("data-stale", "true");
    expect(meta?.getAttribute("title")).toBe(
      "Not up to date: still loading, reconnecting, not permitted, or not loaded on every selected cluster",
    );
    expect(meta).toHaveTextContent("(not up to date)");

    const podMeta = screen.getByTestId("nav-item-Pod").querySelector(".nav-item-btn__meta");
    expect(podMeta).not.toHaveAttribute("data-stale");
    expect(podMeta).not.toHaveAttribute("title");
  });

  it("sums over the selected clusters; ready only when every cluster is ready", async () => {
    mockCounts({
      a: [count("Pod", 10, 1), count("Node", 3)],
      b: [count("Pod", 5, 2), count("Node", 2, 0, false), count("Service", 4)],
    });
    useContextsStore.setState({ activeClusterSelection: ["a", "b"] });
    render(<Sidebar />);
    await flush();

    expect(screen.getByTestId("nav-item-Pod")).toHaveTextContent("15");
    expect(
      screen.getByTestId("nav-item-Pod").querySelector(".nav-item-btn__fail"),
    ).toHaveAttribute("title", "3 failing");
    expect(
      screen.getByTestId("nav-item-Node").querySelector(".nav-item-btn__meta"),
    ).toHaveAttribute("data-stale", "true");
    // Only cluster b watches Services: the sum is partial, so it is not ready.
    expect(
      screen.getByTestId("nav-item-Service").querySelector(".nav-item-btn__meta"),
    ).toHaveAttribute("data-stale", "true");
  });

  it("polls on an interval, refetches on cluster change and stops on unmount", async () => {
    const calls = mockCounts({ dev: [count("Pod", 1)], prod: [count("Pod", 7)] });
    const { unmount } = render(<Sidebar />);
    await flush();
    expect(calls).toEqual(["dev"]);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(KIND_COUNTS_POLL_MS);
    });
    expect(calls).toEqual(["dev", "dev"]);

    // A cluster change fetches immediately and never shows the old cluster's numbers.
    act(() => {
      useContextsStore.setState({ selectedContext: "prod" });
    });
    expect(screen.getByTestId("nav-item-Pod")).not.toHaveTextContent("1");
    await flush();
    expect(calls).toEqual(["dev", "dev", "prod"]);
    expect(screen.getByTestId("nav-item-Pod")).toHaveTextContent("7");

    unmount();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(KIND_COUNTS_POLL_MS * 4);
    });
    expect(calls).toEqual(["dev", "dev", "prod"]);
  });

  it("does not poll while the document is hidden and resumes when visible", async () => {
    const calls = mockCounts({ dev: [count("Pod", 1)] });
    let state: DocumentVisibilityState = "visible";
    const spy = vi.spyOn(document, "visibilityState", "get").mockImplementation(() => state);
    try {
      render(<Sidebar />);
      await flush();
      expect(calls).toHaveLength(1);

      state = "hidden";
      document.dispatchEvent(new Event("visibilitychange"));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(KIND_COUNTS_POLL_MS * 3);
      });
      expect(calls).toHaveLength(1);

      state = "visible";
      document.dispatchEvent(new Event("visibilitychange"));
      await flush();
      expect(calls).toHaveLength(2);
    } finally {
      spy.mockRestore();
    }
  });

  it("never overlaps requests", async () => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    let started = 0;
    let release: (() => void) | undefined;
    mockIPC((cmd) => {
      if (cmd !== "kind_counts") return undefined;
      started++;
      return new Promise<KindCount[]>((resolve) => {
        release = () => {
          resolve([count("Pod", 1)]);
        };
      });
    });
    render(<Sidebar />);
    await flush();
    expect(started).toBe(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(KIND_COUNTS_POLL_MS * 5);
    });
    expect(started).toBe(1);
    release?.();
    await flush();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(KIND_COUNTS_POLL_MS);
    });
    expect(started).toBe(2);
  });

  it("shows no counts when no cluster is selected and does not invoke", async () => {
    const calls = mockCounts({});
    useContextsStore.setState({ selectedContext: null });
    render(<Sidebar />);
    await flush();
    expect(calls).toEqual([]);
    expect(document.querySelector(".nav-item-btn__meta")).toBeNull();
  });

  it("an unchanged poll keeps the state, so the sidebar does not re-render", async () => {
    const counts = { dev: [count("Pod", 42, 3)] as KindCount[] | Error };
    mockCounts(counts);
    let renders = 0;
    function Probe() {
      const map = useKindCounts(["dev"]);
      renders++;
      seen.push(map);
      return null;
    }
    const seen: ReadonlyMap<string, unknown>[] = [];
    render(<Probe />);
    await flush();
    const afterFirst = renders;
    const first = seen[seen.length - 1];
    expect(first?.get("Pod")).toEqual({ total: 42, failing: 3, ready: true });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(KIND_COUNTS_POLL_MS * 3);
    });
    expect(renders).toBe(afterFirst);

    counts.dev = [count("Pod", 43, 3)];
    await act(async () => {
      await vi.advanceTimersByTimeAsync(KIND_COUNTS_POLL_MS);
    });
    expect(renders).toBe(afterFirst + 1);
    expect(seen[seen.length - 1]?.get("Pod")).toEqual({ total: 43, failing: 3, ready: true });
  });

  it("treats a failed call as unknown for that cluster", async () => {
    mockCounts({ a: [count("Pod", 4)], b: new Error("boom") });
    useContextsStore.setState({ activeClusterSelection: ["a", "b"] });
    render(<Sidebar />);
    await flush();
    const meta = screen.getByTestId("nav-item-Pod").querySelector(".nav-item-btn__meta");
    expect(meta).toHaveTextContent("4");
    expect(meta).toHaveAttribute("data-stale", "true");
  });
});

describe("Sidebar group collapse", () => {
  beforeEach(() => {
    localStorage.clear();
    reloadSidebarStore();
    useLayoutStore.setState(initialLayoutState);
    useContextsStore.setState(initialContextsState);
  });

  afterEach(() => {
    localStorage.clear();
    reloadSidebarStore();
  });

  it("toggles a group with a keyboard-accessible button and persists the state", () => {
    const { unmount } = render(<Sidebar />);
    const toggle = screen.getByRole("button", { name: "Network" });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("list", { name: "Network" })).toBeVisible();

    act(() => {
      toggle.click();
    });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("list", { name: "Network" })).toBeNull();
    expect(screen.getByTestId("nav-item-Service")).not.toBeVisible();
    expect(JSON.parse(localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) ?? "null")).toEqual([
      "Network",
    ]);

    // Survives a reload.
    unmount();
    reloadSidebarStore();
    render(<Sidebar />);
    expect(screen.getByRole("button", { name: "Network" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByRole("list", { name: "Workloads" })).toBeVisible();

    act(() => {
      screen.getByRole("button", { name: "Network" }).click();
    });
    expect(screen.getByRole("list", { name: "Network" })).toBeVisible();
    expect(JSON.parse(localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) ?? "null")).toEqual([]);
  });

  it("reveals a collapsed group when its kind becomes active, without persisting it", () => {
    localStorage.setItem(SIDEBAR_COLLAPSED_STORAGE_KEY, JSON.stringify(["Network"]));
    reloadSidebarStore();
    render(<Sidebar />);
    const toggle = screen.getByRole("button", { name: "Network" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByTestId("nav-item-Service")).not.toBeVisible();

    // `g s`, the palette or a tab switch makes Services active: its group is shown.
    act(() => {
      useLayoutStore.getState().showKind("Services", "Service");
    });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByTestId("nav-item-Service")).toBeVisible();
    expect(screen.getByTestId("nav-item-Service")).toHaveAttribute("aria-pressed", "true");
    // Not persisted.
    expect(JSON.parse(localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) ?? "null")).toEqual([
      "Network",
    ]);

    // The user can still collapse it; it stays collapsed while the kind stays active.
    act(() => {
      toggle.click();
    });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByTestId("nav-item-Service")).not.toBeVisible();
    expect(JSON.parse(localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) ?? "null")).toEqual([
      "Network",
    ]);

    // Moving to another kind and back reveals it again.
    act(() => {
      useLayoutStore.getState().showKind("Pods", "Pod");
    });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    act(() => {
      useLayoutStore.getState().showKind("Services", "Service");
    });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
  });

  it("a revealed group collapsed while active, then expanded by the user, is persisted", () => {
    localStorage.setItem(SIDEBAR_COLLAPSED_STORAGE_KEY, JSON.stringify(["Network"]));
    reloadSidebarStore();
    render(<Sidebar />);
    act(() => {
      useLayoutStore.getState().showKind("Services", "Service");
    });
    const toggle = screen.getByRole("button", { name: "Network" });
    act(() => {
      toggle.click();
    });
    act(() => {
      toggle.click();
    });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(JSON.parse(localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) ?? "null")).toEqual([]);
  });

  it("ignores malformed stored state", () => {
    expect(parseCollapsedGroups(null)).toEqual([]);
    expect(parseCollapsedGroups("not json")).toEqual([]);
    expect(parseCollapsedGroups('{"Network":true}')).toEqual([]);
    expect(parseCollapsedGroups('["Network", 3, null, "", "Network", "Config"]')).toEqual([
      "Network",
      "Config",
    ]);

    localStorage.setItem(SIDEBAR_COLLAPSED_STORAGE_KEY, "{oops");
    reloadSidebarStore();
    render(<Sidebar />);
    expect(screen.getByRole("button", { name: "Workloads" })).toHaveAttribute("aria-expanded", "true");
  });
});

describe("aggregateKindCounts", () => {
  it("sums by kind and keeps kinds no cluster reported absent", () => {
    const out = aggregateKindCounts([
      [count("Pod", 2, 1)],
      [count("Pod", 3, 0)],
    ]);
    expect(out.get("Pod")).toEqual({ total: 5, failing: 1, ready: true });
    expect(out.has("Service")).toBe(false);
  });
});

describe("sameKindCounts", () => {
  it("compares total, failing and ready per kind", () => {
    const a = aggregateKindCounts([[count("Pod", 2, 1), count("Node", 1)]]);
    expect(sameKindCounts(a, aggregateKindCounts([[count("Node", 1), count("Pod", 2, 1)]]))).toBe(
      true,
    );
    expect(sameKindCounts(a, aggregateKindCounts([[count("Pod", 2, 1)]]))).toBe(false);
    expect(sameKindCounts(a, aggregateKindCounts([[count("Pod", 2, 0), count("Node", 1)]]))).toBe(
      false,
    );
    expect(
      sameKindCounts(a, aggregateKindCounts([[count("Pod", 2, 1), count("Node", 1, 0, false)]])),
    ).toBe(false);
  });
});
