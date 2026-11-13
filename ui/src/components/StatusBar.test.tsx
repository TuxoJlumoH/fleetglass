import type { Channel } from "@tauri-apps/api/core";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PageDelta_Deserialize } from "../generated/bindings";
import {
  defaultContextSettings,
  initialContextsState,
  setConnectHandler,
  useContextsStore,
  type ConnectionStatus,
} from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { podsLayoutState } from "../test/layout";
import {
  resetPodsStore,
  usePodsStore,
  type Page,
  type StoreAccess,
  type StoreAccessState,
} from "../stores/pods";
import { useAppInfoStore } from "../stores/appInfo";
import { StatusBar } from "./StatusBar";

const EMOJI = /[\u{1F7E0}-\u{1F7EB}\u{1F534}-\u{1F535}○●]/u;

function selectContext(name: string, status?: ConnectionStatus) {
  useContextsStore.setState((s) => ({
    selectedContext: name,
    connectionStatus: status ? { ...s.connectionStatus, [name]: status } : s.connectionStatus,
  }));
}

function podAccess(
  cluster: string,
  metrics: StoreAccess["metrics"] = null,
  access: StoreAccessState = { state: "ready" },
): StoreAccess {
  return { cluster, kind: "Pod", access, rows: 3, metrics };
}

function page(version: number, access: StoreAccess[], offset = 0): Page {
  return {
    offset,
    limit: 100,
    total: 0,
    version,
    rows: [],
    access,
    statusCounts: null,
  } as unknown as Page;
}

/** Puts the store in "page loaded, pull-only" state with `access`, as a table would. */
function loaded(access: StoreAccess[], lastUpdatedAt = Date.now()) {
  usePodsStore.setState({ access, pageLoaded: true, lastUpdatedAt, liveSubscribed: false });
}

function openNonResourceTab() {
  act(() => {
    useLayoutStore.getState().openTab({ id: "view-helm", title: "Helm releases", view: "helm" });
  });
}

describe("StatusBar", () => {
  beforeEach(() => {
    useContextsStore.setState(initialContextsState);
    useLayoutStore.setState(podsLayoutState);
    useAppInfoStore.setState({ version: null });
    setConnectHandler();
    resetPodsStore();
  });

  afterEach(() => {
    vi.useRealTimers();
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("shows nothing about the connection when no context is selected", () => {
    render(<StatusBar />);
    expect(screen.queryByTestId("status-connection")).toBeNull();
    expect(screen.queryByTestId("status-context")).toBeNull();
    expect(screen.queryByTestId("status-metrics")).toBeNull();
    expect(screen.queryByTestId("status-updated")).toBeNull();
    expect(screen.queryByTestId("status-access-mode")).toBeNull();
  });

  it("makes no connection claim for a selected context with nothing reliable known", () => {
    selectContext("kind-dev");
    render(<StatusBar />);
    expect(screen.queryByTestId("status-connection")).toBeNull();
    expect(screen.getByTestId("status-context")).toHaveTextContent("kind-dev");
  });

  it.each([
    [{ state: "ready" }, "Connected", "ok", "Connected"],
    [{ state: "loading", rowsSoFar: 4 }, "Loading…", "warn", "Loading (initial list)"],
    [{ state: "reconnecting" }, "Reconnecting…", "warn", "Reconnecting (re-listing)"],
    [
      { state: "forbidden", resource: "pods", namespace: null, message: "403" },
      "No permission",
      "err",
      "No permission: 403",
    ],
    [{ state: "error", message: "TLS", partialRows: null }, "Error", "err", "Error: TLS"],
    [{ state: "notFound", resource: "x", message: "404" }, "API not found", "err", "API not found: 404"],
    [{ state: "unsupported", kind: "X", message: "n/a" }, "Not supported", "err", "Not supported: n/a"],
    [
      {
        state: "authExpired",
        provider: "aws-sso",
        hint: "login required",
        loginCommand: "aws sso login",
        deviceCode: null,
        partialRows: null,
      },
      "Auth Expired",
      "err",
      "Auth Expired: login required",
    ],
  ] as const)("derives the connection from access %o", (access, text, tone, title) => {
    // A stale contexts-store claim must not win over what the watch store reports.
    selectContext("kind-dev", { state: "disconnected" });
    loaded([podAccess("kind-dev", null, access)]);
    render(<StatusBar />);
    const item = screen.getByTestId("status-connection");
    expect(item).toHaveTextContent(text);
    expect(item).toHaveAttribute("title", title);
    expect(item.querySelector(".dot")).toHaveClass(tone);
    expect(screen.getByRole("contentinfo").textContent).not.toMatch(EMOJI);
  });

  it("falls back to an explicit connection status for a cluster without access entries", () => {
    selectContext("kind-dev", { state: "error", message: "refused" });
    render(<StatusBar />);
    const item = screen.getByTestId("status-connection");
    expect(item).toHaveTextContent("Error");
    expect(item).toHaveAttribute("title", "Error: refused");
  });

  it("summarises several selected clusters from their access states", () => {
    useContextsStore.setState({
      activeClusterSelection: ["a", "b", "c"],
      // Stale claims must not show up as "disconnected" while the stores report.
      connectionStatus: { a: { state: "disconnected" }, b: { state: "disconnected" } },
    });
    loaded([
      podAccess("a"),
      podAccess("b", null, { state: "error", message: "refused", partialRows: null }),
      podAccess("c", null, { state: "reconnecting" }),
    ]);
    render(<StatusBar />);
    const item = screen.getByTestId("status-connection");
    expect(item).toHaveTextContent("3 clusters · 1 error · 1 reconnecting");
    expect(item).not.toHaveTextContent("disconnected");
    expect(item.querySelector(".dot")).toHaveClass("err");
    expect(screen.queryByTestId("status-context")).toBeNull();
  });

  it("says 'all connected' when every queried cluster is ready", () => {
    useContextsStore.setState({ activeClusterSelection: ["a", "b"] });
    loaded([podAccess("a"), podAccess("b")]);
    render(<StatusBar />);
    const item = screen.getByTestId("status-connection");
    expect(item).toHaveTextContent("2 clusters · all connected");
    expect(item.querySelector(".dot")).toHaveClass("ok");
  });

  it.each([
    [{ state: "available" }, "metrics-server available"],
    [{ state: "notInstalled", message: "404" }, "metrics-server not installed"],
    [{ state: "forbidden", message: "403" }, "metrics: no permission"],
    [{ state: "error", message: "timeout" }, "metrics unavailable"],
  ] as const)("shows metrics state %o", (metrics, text) => {
    selectContext("kind-dev");
    loaded([podAccess("kind-dev", metrics)]);
    render(<StatusBar />);
    expect(screen.getByTestId("status-metrics")).toHaveTextContent(text);
  });

  it("shows no metrics text while unknown (pending or a kind without usage)", () => {
    selectContext("kind-dev");
    loaded([podAccess("kind-dev", { state: "pending" })]);
    const { unmount } = render(<StatusBar />);
    expect(screen.queryByTestId("status-metrics")).toBeNull();
    unmount();
    loaded([podAccess("kind-dev", null)]);
    render(<StatusBar />);
    expect(screen.queryByTestId("status-metrics")).toBeNull();
  });

  it("ticks 'Updated Ns ago' for a pull-only table and clears the interval on unmount", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-10T12:00:00Z"));
    selectContext("kind-dev");
    loaded([podAccess("kind-dev")], Date.now() - 2000);
    const clearSpy = vi.spyOn(window, "clearInterval");
    const { unmount } = render(<StatusBar />);
    expect(screen.getByTestId("status-updated")).toHaveTextContent("Updated 2s ago");
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(screen.getByTestId("status-updated")).toHaveTextContent("Updated 5s ago");
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByTestId("status-updated")).toHaveTextContent("Updated 1m ago");

    // New data resets the age without remounting the ticker.
    act(() => {
      usePodsStore.setState({ lastUpdatedAt: Date.now() });
    });
    expect(screen.getByTestId("status-updated")).toHaveTextContent("Updated 0s ago");

    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(clearSpy).toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("says 'Live' instead of an age while a live subscription is open and all stores are ready", () => {
    selectContext("kind-dev");
    loaded([podAccess("kind-dev")], Date.now() - 600_000);
    usePodsStore.setState({ liveSubscribed: true });
    render(<StatusBar />);
    const item = screen.getByTestId("status-updated");
    expect(item).toHaveTextContent(/^Live$/);
    expect(item).toHaveAttribute("data-live", "true");
    expect(screen.queryByText(/Updated/)).toBeNull();
  });

  it.each([
    [{ state: "loading", rowsSoFar: 1 }],
    [{ state: "reconnecting" }],
    [{ state: "error", message: "x", partialRows: null }],
    [{ state: "forbidden", resource: "pods", namespace: null, message: "403" }],
  ] as const)("hides Updated / Live while a store is %o", (access) => {
    useContextsStore.setState({ activeClusterSelection: ["a", "b"] });
    loaded([podAccess("a"), podAccess("b", null, access)]);
    const { unmount } = render(<StatusBar />);
    expect(screen.queryByTestId("status-updated")).toBeNull();
    unmount();
    usePodsStore.setState({ liveSubscribed: true });
    render(<StatusBar />);
    expect(screen.queryByTestId("status-updated")).toBeNull();
  });

  it("hides Updated / Live while a queried cluster has no store yet", () => {
    useContextsStore.setState({ activeClusterSelection: ["a", "b"] });
    loaded([podAccess("a")]);
    render(<StatusBar />);
    expect(screen.queryByTestId("status-updated")).toBeNull();
  });

  it("hides Updated, metrics and access-derived connection on a non-resource tab", () => {
    selectContext("kind-dev");
    loaded([podAccess("kind-dev", { state: "available" })]);
    render(<StatusBar />);
    expect(screen.getByTestId("status-updated")).toBeInTheDocument();
    expect(screen.getByTestId("status-metrics")).toBeInTheDocument();
    expect(screen.getByTestId("status-connection")).toHaveTextContent("Connected");

    openNonResourceTab();
    expect(screen.queryByTestId("status-updated")).toBeNull();
    expect(screen.queryByTestId("status-metrics")).toBeNull();
    expect(screen.queryByTestId("status-connection")).toBeNull();
    // The context itself is still named.
    expect(screen.getByTestId("status-context")).toHaveTextContent("kind-dev");

    act(() => {
      useLayoutStore.getState().setActiveTab("pods");
    });
    expect(screen.getByTestId("status-updated")).toBeInTheDocument();
  });

  it("shows the read-only / read-write / mixed badge only when settings are known", () => {
    selectContext("kind-dev");
    const { unmount } = render(<StatusBar />);
    expect(screen.queryByTestId("status-access-mode")).toBeNull();
    act(() => {
      useContextsStore.getState().setContextSettings("kind-dev", { readOnly: false });
    });
    expect(screen.getByTestId("status-access-mode")).toHaveTextContent("Read-write");
    act(() => {
      useContextsStore.getState().setContextSettings("kind-dev", { readOnly: true });
    });
    expect(screen.getByTestId("status-access-mode")).toHaveTextContent("Read-only");
    unmount();

    useContextsStore.setState({
      activeClusterSelection: ["a", "b"],
      settingsByContext: {
        a: { ...defaultContextSettings, readOnly: true },
        b: { ...defaultContextSettings },
      },
    });
    render(<StatusBar />);
    expect(screen.getByTestId("status-access-mode")).toHaveTextContent("Mixed");
  });

  it("opens the command palette from the Ctrl K hint and has no '?' hint", async () => {
    const user = userEvent.setup();
    useAppInfoStore.setState({ version: "0.2.0" });
    render(<StatusBar />);
    const commands = screen.getByTestId("status-commands");
    expect(commands).toHaveTextContent("Ctrl K Commands");
    expect(commands).toHaveAttribute("aria-keyshortcuts", "Control+K");
    await user.click(commands);
    expect(useLayoutStore.getState().paletteOpen).toBe(true);
    // No shortcuts help exists in the app yet, so the footer does not advertise one.
    expect(screen.queryByText("Shortcuts")).toBeNull();
    expect(screen.getByTestId("app-version")).toHaveTextContent("v0.2.0");
  });
});

describe("StatusBar settings loading (ensureSettings)", () => {
  let settingsCalls: string[];
  let resolvers: (() => void)[];

  beforeEach(() => {
    useContextsStore.setState(initialContextsState);
    useLayoutStore.setState(podsLayoutState);
    setConnectHandler();
    resetPodsStore();
    settingsCalls = [];
    resolvers = [];
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    mockIPC((cmd, args) => {
      if (cmd === "get_context_settings") {
        const context = (args as { context: string }).context;
        settingsCalls.push(context);
        return new Promise((resolve) => {
          resolvers.push(() => {
            resolve({
              colour: context === "prod" ? "#ef4444" : null,
              tags: [],
              readOnly: context === "prod",
              protected: false,
              pinned: false,
            });
          });
        });
      }
      if (cmd === "list_port_forwards") return [];
      return undefined;
    });
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  async function flush() {
    await act(async () => {
      for (const r of resolvers.splice(0)) r();
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  it("loads the selected context's settings so the badge appears after select", async () => {
    render(<StatusBar />);
    await act(() => useContextsStore.getState().selectContext("prod"));
    await waitFor(() => {
      expect(settingsCalls).toEqual(["prod"]);
    });
    await flush();
    await waitFor(() => {
      expect(screen.getByTestId("status-access-mode")).toHaveTextContent("Read-only");
    });
    expect(useContextsStore.getState().settingsByContext.prod?.colour).toBe("#ef4444");
    // selectContext and the status bar effect both asked; only one call went out.
    expect(settingsCalls).toEqual(["prod"]);
  });

  it("dedupes in-flight calls and skips cached contexts", async () => {
    const { ensureSettings } = useContextsStore.getState();
    const first = ensureSettings(["a", "b", "a"]);
    const second = ensureSettings(["a", "b"]);
    await waitFor(() => {
      expect(settingsCalls).toEqual(["a", "b"]);
    });
    await flush();
    await act(() => Promise.all([first, second]));
    await ensureSettings(["a", "b"]);
    expect(settingsCalls).toEqual(["a", "b"]);
    expect(useContextsStore.getState().settingsByContext.a?.readOnly).toBe(false);
  });

  it("ignores errors and leaves the context unknown", async () => {
    clearMocks();
    mockWindows("main");
    mockIPC((cmd) => {
      if (cmd === "get_context_settings") {
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw { kind: "internal", message: "boom" };
      }
      return undefined;
    });
    await act(() => useContextsStore.getState().ensureSettings(["x"]));
    expect(useContextsStore.getState().settingsByContext.x).toBeUndefined();
  });

  it("04-10: displays countdown pill and clicking Lock now relocks context", async () => {
    clearMocks();
    mockWindows("main");
    let relocked = false;
    mockIPC((cmd) => {
      if (cmd === "relock_context") {
        relocked = true;
        return { ...defaultContextSettings, readOnly: true, unlockedUntil: null };
      }
      return undefined;
    });

    const now = Math.floor(Date.now() / 1000);
    useContextsStore.setState({
      selectedContext: "prod",
      activeClusterSelection: ["prod"],
      settingsByContext: {
        prod: {
          ...defaultContextSettings,
          readOnly: true,
          unlockedUntil: now + 872, // 14:32 left
        },
      },
    });

    render(<StatusBar />);
    const pill = screen.getByTestId("status-unlocked-pill");
    expect(pill).toHaveTextContent("Unlocked · 14:32 left · Lock now");

    const lockBtn = screen.getByTestId("status-lock-now-btn");
    await act(async () => {
      await userEvent.click(lockBtn);
    });

    expect(relocked).toBe(true);
  });

  it("04-10: clicking Read-only badge opens ContextUnlockModal and typing confirmation unlocks", async () => {
    clearMocks();
    mockWindows("main");
    let unlockedWith: string | null = null;
    mockIPC((cmd, args) => {
      if (cmd === "unlock_context") {
        unlockedWith = (args as { confirmedName: string | null }).confirmedName;
        return { ...defaultContextSettings, readOnly: true, unlockedUntil: Math.floor(Date.now() / 1000) + 900 };
      }
      return undefined;
    });

    useContextsStore.setState({
      selectedContext: "prod-locked",
      activeClusterSelection: ["prod-locked"],
      settingsByContext: {
        "prod-locked": {
          ...defaultContextSettings,
          readOnly: true,
          protected: true,
        },
      },
    });

    render(<StatusBar />);
    const modeBadge = screen.getByTestId("status-access-mode");
    expect(modeBadge).toHaveTextContent("Read-only");

    await act(async () => {
      await userEvent.click(modeBadge);
    });

    const input = screen.getByTestId("unlock-confirm-input");
    expect(input).toBeInTheDocument();

    const execBtn = screen.getByTestId("unlock-confirm-execute-btn");
    expect(execBtn).toBeDisabled();

    await act(async () => {
      await userEvent.type(input, "prod-locked");
    });

    expect(execBtn).toBeEnabled();

    await act(async () => {
      await userEvent.click(execBtn);
    });

    expect(unlockedWith).toBe("prod-locked");
  });
});

describe("pods store lastUpdatedAt", () => {
  beforeEach(() => {
    resetPodsStore();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("is set by the first page and only bumped by a new version, not same-version windows", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-10T12:00:00Z"));
    const t0 = Date.now();
    expect(usePodsStore.getState().lastUpdatedAt).toBeNull();
    usePodsStore.getState().applyPage(page(1, [podAccess("a")]));
    expect(usePodsStore.getState().lastUpdatedAt).toBe(t0);

    // Scrolling fetches another window of the same version.
    vi.advanceTimersByTime(30_000);
    usePodsStore.getState().applyPage(page(1, [podAccess("a")], 100));
    expect(usePodsStore.getState().lastUpdatedAt).toBe(t0);

    vi.advanceTimersByTime(30_000);
    usePodsStore.getState().applyPage(page(2, [podAccess("a")]));
    expect(usePodsStore.getState().lastUpdatedAt).toBe(t0 + 60_000);
  });

  it("is not bumped by live deltas that only change access (same version)", async () => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    const channels: Channel<PageDelta_Deserialize>[] = [];
    mockIPC((cmd, args) => {
      const a = args as Record<string, unknown>;
      if (cmd === "query_page") return page(1, [podAccess("a")]);
      if (cmd === "subscribe_query") {
        channels.push(a.onDelta as Channel<PageDelta_Deserialize>);
        return channels.length;
      }
      if (cmd === "unsubscribe_query") return true;
      return undefined;
    });
    await act(() => usePodsStore.getState().setClusters(["a"]));
    await waitFor(() => {
      expect(channels).toHaveLength(1);
    });
    expect(usePodsStore.getState().liveSubscribed).toBe(true);
    const t0 = usePodsStore.getState().lastUpdatedAt;
    if (t0 === null) throw new Error("no first page");

    const channel = channels[0];
    if (channel === undefined) throw new Error("no channel");
    let index = 0;
    const push = (p: Page) => {
      act(() => {
        const internals = (
          window as unknown as {
            __TAURI_INTERNALS__: { runCallback: (id: number, data: unknown) => void };
          }
        ).__TAURI_INTERNALS__;
        internals.runCallback(channel.id, { index, message: { subscriptionId: 1, page: p } });
        index += 1;
      });
    };

    vi.spyOn(Date, "now").mockReturnValue(t0 + 5_000);
    push(page(1, [podAccess("a", null, { state: "reconnecting" })]));
    expect(usePodsStore.getState().access[0]?.access.state).toBe("reconnecting");
    expect(usePodsStore.getState().lastUpdatedAt).toBe(t0);

    push(page(2, [podAccess("a")]));
    expect(usePodsStore.getState().lastUpdatedAt).toBe(t0 + 5_000);

    resetPodsStore();
    expect(usePodsStore.getState().liveSubscribed).toBe(false);
  });
});
