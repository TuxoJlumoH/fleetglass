import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { commands } from "../generated/bindings";
import { App } from "../App";
import { useActionsStore } from "../stores/actions";
import { createGoChordHandler, isTypingTarget } from "../lib/goChords";
import { initialContextsState, setConnectHandler, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { podsLayoutState } from "../test/layout";
import { type Page, type Row, resetPodsStore, setQueryFetcher } from "../stores/pods";
import { SETTINGS_STORAGE_KEY, useSettingsStore } from "../stores/settings";
import { CommandPalette } from "./CommandPalette";
import { PodTable } from "./PodTable";

function makePodRow(index: number): Row {
  return {
    uid: `uid-${String(index)}`,
    cluster: "dev",
    kind: "Pod",
    namespace: "default",
    name: `pod-${String(index).padStart(5, "0")}`,
    status: "healthy",
    statusReason: "Running",
    cells: [
      { type: "ready", value: { ready: 1, total: 1 } },
      { type: "status", value: { label: "Running", phase: "healthy" } },
      { type: "int", value: 0 },
      { type: "text", value: "node-0" },
      { type: "text", value: "10.0.0.1" },
      { type: "timestamp", value: "2026-01-01T00:00:00Z" },
    ],
    labels: [],
    custom: [],
  };
}

function makePodPage(offset: number, limit: number, total: number): Page {
  const count = Math.max(0, Math.min(limit, total - offset));
  const rows: Row[] = [];
  for (let i = 0; i < count; i += 1) {
    rows.push(makePodRow(offset + i));
  }
  return {
    total,
    offset,
    limit,
    rows,
    version: 1,
    access: [{ cluster: "dev", kind: "Pod", access: { state: "ready" }, rows: 0, metrics: null }],
    statusCounts: { healthy: 0, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
  };
}

describe("P1-11 Keyboard navigation & CommandPalette and P1-12 Tabs", () => {
  beforeEach(() => {
    useLayoutStore.setState(podsLayoutState);
    useContextsStore.setState(initialContextsState);
    setConnectHandler();
    resetPodsStore();
  });

  afterEach(() => {
    resetPodsStore();
    useSettingsStore.setState({ density: "comfortable" });
    localStorage.removeItem(SETTINGS_STORAGE_KEY);
  });

  it("opens Command Palette on Ctrl+K, opens a Deployments tab when selecting Deployments, and closes the palette", async () => {
    const user = userEvent.setup();
    render(<App />);

    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();

    // Press Ctrl+K to open palette
    await user.keyboard("{Control>}k{/Control}");
    const dialog = screen.getByRole("dialog", { name: "Command palette" });
    expect(dialog).toBeInTheDocument();
    expect(
      screen.getByPlaceholderText("Type a command, resource, or context…"),
    ).toBeInTheDocument();

    // Select Deployments resource item
    const deploymentsOption = screen.getByRole("option", { name: "Deployments" });
    await user.click(deploymentsOption);

    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
    expect(useLayoutStore.getState().paletteOpen).toBe(false);

    const deploymentsTab = screen.getByRole("tab", { name: "Deployments" });
    expect(deploymentsTab).toHaveAttribute("aria-selected", "true");

    const activeTab = useLayoutStore
      .getState()
      .tabs.find((t) => t.id === useLayoutStore.getState().activeTabId);
    expect(activeTab?.kind).toBe("Deployment");
  });

  it("goes to the Overview tab from the palette and reuses it", async () => {
    const user = userEvent.setup();
    render(<App />);
    for (let i = 0; i < 2; i += 1) {
      await user.keyboard("{Control>}k{/Control}");
      await user.click(screen.getByRole("option", { name: "Go to Overview" }));
      expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute("aria-selected", "true");
    }
    expect(useLayoutStore.getState().tabs.filter((t) => t.view === "overview")).toHaveLength(1);
    // Outside the desktop app the page says so.
    expect(screen.getByText("Not running in the Fleetglass app")).toBeInTheDocument();
  });

  it("opens Audit log tab when selecting Open audit log from Views in the command palette", async () => {
    const user = userEvent.setup();
    render(<App />);

    // Press Ctrl+K to open palette
    await user.keyboard("{Control>}k{/Control}");
    const auditOption = screen.getByRole("option", { name: "Open audit log" });
    await user.click(auditOption);

    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
    expect(useLayoutStore.getState().paletteOpen).toBe(false);

    const auditTab = screen.getByRole("tab", { name: "Audit log" });
    expect(auditTab).toHaveAttribute("aria-selected", "true");

    const activeTab = useLayoutStore
      .getState()
      .tabs.find((t) => t.id === useLayoutStore.getState().activeTabId);
    expect(activeTab?.view).toBe("audit");
  });

  it("opens an ecosystem tab from Resources in Command Palette", async () => {
    const user = userEvent.setup();
    render(<App />);

    const ecoActions = [
      { action: "Open Gateways", title: "Gateways", kind: "Gateway" },
      { action: "Open HTTPRoutes", title: "HTTPRoutes", kind: "HTTPRoute" },
      { action: "Open Argo Applications", title: "Argo Apps", kind: "Application" },
      { action: "Open Flux Kustomizations", title: "Flux Kustomize", kind: "Kustomization" },
      { action: "Open Karpenter NodePools", title: "Karpenter Pools", kind: "NodePool" },
    ];

    for (const item of ecoActions) {
      await user.keyboard("{Control>}k{/Control}");
      expect(screen.getByRole("dialog", { name: "Command palette" })).toBeInTheDocument();

      const option = screen.getByRole("option", { name: item.action });
      expect(option).toBeInTheDocument();
      await user.click(option);

      expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
      expect(useLayoutStore.getState().paletteOpen).toBe(false);

      const activeTab = useLayoutStore
        .getState()
        .tabs.find((t) => t.id === useLayoutStore.getState().activeTabId);
      expect(activeTab).toBeDefined();
      expect(activeTab?.title).toBe(item.title);
      expect(activeTab?.kind).toBe(item.kind);
      expect(activeTab?.view).toBe("resources");
    }
  });

  it("reloads team settings from Settings group in Command Palette", async () => {
    const user = userEvent.setup();
    const reloadMock = vi.fn(() => Promise.resolve(true));
    useContextsStore.setState({ reloadTeamSettings: reloadMock });
    render(<App />);

    await user.keyboard("{Control>}k{/Control}");
    const reloadOption = screen.getByRole("option", { name: "Reload team settings" });
    expect(reloadOption).toBeInTheDocument();
    await user.click(reloadOption);

    expect(reloadMock).toHaveBeenCalledWith("team-settings.yaml");
    expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
  });

  it("opens multiple tabs, navigates back/forward across history, and closes tabs while preserving each tab's state", async () => {
    const user = userEvent.setup();
    render(<App />);

    let podsTabId = "";
    let servicesTabId = "";
    act(() => {
      const store = useLayoutStore.getState();
      podsTabId = store.openTab({
        title: "Pods (prod)",
        kind: "Pod",
        cluster: "prod",
        namespace: "kube-system",
        textFilter: "coredns",
      });
      servicesTabId = store.openTab({
        title: "Services (staging)",
        kind: "Service",
        cluster: "staging",
        namespace: "default",
        textFilter: "api",
      });
    });

    expect(useLayoutStore.getState().activeTabId).toBe(servicesTabId);
    expect(useLayoutStore.getState().tabs).toHaveLength(3);

    // Update first tab state independently
    act(() => {
      useLayoutStore.getState().updateTabState(podsTabId, { textFilter: "coredns-updated" });
    });

    // Navigate back -> podsTabId -> pods (initial tab)
    act(() => {
      useLayoutStore.getState().goBack();
    });
    expect(useLayoutStore.getState().activeTabId).toBe(podsTabId);
    const podsTab = useLayoutStore.getState().tabs.find((t) => t.id === podsTabId);
    expect(podsTab?.textFilter).toBe("coredns-updated");
    expect(podsTab?.namespace).toBe("kube-system");

    act(() => {
      useLayoutStore.getState().goBack();
    });
    expect(useLayoutStore.getState().activeTabId).toBe("pods");

    // Navigate forward -> podsTabId -> servicesTabId
    act(() => {
      useLayoutStore.getState().goForward();
    });
    expect(useLayoutStore.getState().activeTabId).toBe(podsTabId);

    act(() => {
      useLayoutStore.getState().goForward();
    });
    expect(useLayoutStore.getState().activeTabId).toBe(servicesTabId);
    const servicesTab = useLayoutStore.getState().tabs.find((t) => t.id === servicesTabId);
    expect(servicesTab?.textFilter).toBe("api");
    expect(servicesTab?.kind).toBe("Service");

    // Close active tab via UI close button and verify neighbour becomes active
    await user.click(screen.getByRole("button", { name: "Close Services (staging)" }));
    expect(useLayoutStore.getState().tabs).toHaveLength(2);
    expect(useLayoutStore.getState().activeTabId).toBe(podsTabId);
  });

  it("toggles compact rows from the palette; table rows follow the density", async () => {
    const user = userEvent.setup();
    useSettingsStore.setState({ density: "comfortable" });
    setQueryFetcher((spec) => Promise.resolve(makePodPage(spec.offset, spec.limit, 10)));
    useContextsStore.setState({ selectedContext: "dev" });
    render(
      <>
        <PodTable />
        <CommandPalette />
      </>,
    );
    await screen.findByText("pod-00000");
    const firstRow = () => screen.getAllByTestId("pod-row")[0];
    expect(firstRow()?.style.height).toBe("34px");

    await user.keyboard("{Control>}k{/Control}");
    await user.click(screen.getByRole("option", { name: /Toggle compact rows/ }));
    expect(useSettingsStore.getState().density).toBe("compact");
    expect(screen.getByRole("table")).toHaveAttribute("data-density", "compact");
    expect(firstRow()?.style.height).toBe("28px");
    expect(screen.getAllByTestId("pod-row")[1]?.style.transform).toBe("translateY(28px)");

    // Table keys keep working after the re-measure.
    await user.keyboard("jj");
    expect(
      screen.getAllByTestId("pod-row").find((el) => el.dataset.index === "2"),
    ).toHaveAttribute("data-selected", "true");
  });

  it("switches theme from the command palette", async () => {
    const user = userEvent.setup();
    useSettingsStore.getState().setTheme("dark");
    render(<App />);

    await user.keyboard("{Control>}k{/Control}");
    const lightOption = screen.getByRole("option", { name: /Theme: Light/ });
    await user.click(lightOption);

    expect(useSettingsStore.getState().theme).toBe("light");
    expect(useSettingsStore.getState().resolvedTheme).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(document.documentElement.classList.contains("light")).toBe(true);

    await user.keyboard("{Control>}k{/Control}");
    const systemOption = screen.getByRole("option", { name: /Theme: Follow system/ });
    await user.click(systemOption);

    expect(useSettingsStore.getState().theme).toBe("system");

    await user.keyboard("{Control>}k{/Control}");
    const darkOption = screen.getByRole("option", { name: /Theme: Dark/ });
    await user.click(darkOption);

    expect(useSettingsStore.getState().theme).toBe("dark");
    expect(useSettingsStore.getState().resolvedTheme).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    expect(document.documentElement.classList.contains("light")).toBe(false);
  });

  it("supports k9s keymap (j, k, G, g, /) in PodTable to move row selection and focus the filter input", async () => {
    const user = userEvent.setup();
    setQueryFetcher((spec) => Promise.resolve(makePodPage(spec.offset, spec.limit, 10)));
    useContextsStore.setState({ selectedContext: "dev" });

    render(<PodTable />);
    await screen.findByText("pod-00000");

    const getRowAt = (idx: number) =>
      screen.getAllByTestId("pod-row").find((el) => el.getAttribute("data-index") === String(idx));

    // Initial selection is row 0
    expect(getRowAt(0)).toHaveAttribute("data-selected", "true");
    expect(getRowAt(1)).not.toHaveAttribute("data-selected");

    // j moves down to row 1, then row 2
    await user.keyboard("jj");
    expect(getRowAt(0)).not.toHaveAttribute("data-selected");
    expect(getRowAt(2)).toHaveAttribute("data-selected", "true");

    // k moves up to row 1
    await user.keyboard("k");
    expect(getRowAt(1)).toHaveAttribute("data-selected", "true");

    // G jumps to bottom (row 9)
    await user.keyboard("G");
    expect(getRowAt(9)).toHaveAttribute("data-selected", "true");

    // g jumps to top (row 0)
    await user.keyboard("g");
    expect(getRowAt(0)).toHaveAttribute("data-selected", "true");

    // / focuses the Filter pods input without inserting "/" into its value
    const filterInput = screen.getByLabelText("Filter pods");
    expect(filterInput).not.toHaveFocus();
    await user.keyboard("/");
    expect(filterInput).toHaveFocus();
    expect(filterInput).toHaveValue("");
  });
});

describe("02-06b go-to chords", () => {
  function key(k: string, target: EventTarget = document.body): KeyboardEvent {
    const e = new KeyboardEvent("keydown", { key: k, cancelable: true });
    Object.defineProperty(e, "target", { value: target });
    return e;
  }

  it("g then p within 1s runs Pods", () => {
    let t = 0;
    const run = vi.fn();
    const h = createGoChordHandler(run, () => t);
    h(key("g"));
    t = 500;
    const p = key("p");
    h(p);
    expect(run).toHaveBeenCalledWith(expect.objectContaining({ kind: "Pod" }));
    expect(p.defaultPrevented).toBe(true);
  });

  it("g then h opens Helm", () => {
    let t = 0;
    const run = vi.fn();
    const h = createGoChordHandler(run, () => t);
    h(key("g"));
    t = 10;
    h(key("h"));
    expect(run).toHaveBeenCalledWith(expect.objectContaining({ view: "helm" }));
  });

  it("second key after the 1s timeout does nothing", () => {
    let t = 0;
    const run = vi.fn();
    const h = createGoChordHandler(run, () => t);
    h(key("g"));
    t = 1500;
    h(key("p"));
    expect(run).not.toHaveBeenCalled();
  });

  it("a lone g is not prevented, so the table keeps g = top", () => {
    const run = vi.fn();
    const h = createGoChordHandler(run, () => 0);
    const e = key("g");
    h(e);
    expect(e.defaultPrevented).toBe(false);
    expect(run).not.toHaveBeenCalled();
  });

  it("is ignored while a modal dialog is open", () => {
    let t = 0;
    const run = vi.fn();
    const h = createGoChordHandler(run, () => t);
    const dlg = document.createElement("div");
    dlg.setAttribute("role", "dialog");
    dlg.setAttribute("aria-modal", "true");
    document.body.appendChild(dlg);
    try {
      h(key("g"));
      t = 10;
      h(key("d"));
      expect(run).not.toHaveBeenCalled();
    } finally {
      dlg.remove();
    }
  });

  it("is ignored while typing in an input", () => {
    let t = 0;
    const run = vi.fn();
    const h = createGoChordHandler(run, () => t);
    const input = document.createElement("input");
    h(key("g", input));
    t = 10;
    h(key("p", input));
    expect(run).not.toHaveBeenCalled();
    expect(isTypingTarget(input)).toBe(true);
    expect(isTypingTarget(document.body)).toBe(false);
  });

  it("g d in the app switches the active tab to Deployments; palette lists the chords", async () => {
    render(<App />);
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "g" }));
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "d" }));
    });
    const { tabs, activeTabId } = useLayoutStore.getState();
    expect(tabs.find((tab) => tab.id === activeTabId)?.kind).toBe("Deployment");

    const user = userEvent.setup();
    await user.keyboard("{Control>}k{/Control}");
    expect(await screen.findByLabelText("Go-to shortcuts")).toHaveTextContent("Helm releases");
  });

  it("in the app with a pod table: lone g still goes to top, g s switches tab without a table action", async () => {
    useLayoutStore.setState(podsLayoutState);
    useContextsStore.setState({ ...initialContextsState, selectedContext: "dev" });
    setConnectHandler();
    resetPodsStore();
    // Deployment rows, so a leaked `s` would request a real Scale action.
    setQueryFetcher((spec) => {
      const page = makePodPage(spec.offset, spec.limit, 10);
      return Promise.resolve({ ...page, rows: page.rows.map((r) => ({ ...r, kind: "Deployment" })) });
    });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("pod-00000");
    const rowAt = (idx: number) =>
      screen.getAllByTestId("pod-row").find((el) => el.getAttribute("data-index") === String(idx));

    await user.keyboard("jjj");
    expect(rowAt(3)).toHaveAttribute("data-selected", "true");
    await user.keyboard("g");
    expect(rowAt(0)).toHaveAttribute("data-selected", "true");

    await user.keyboard("s");
    // `g s` is a chord: tab switches to Services, and no Scale/confirm dialog opens.
    const { tabs, activeTabId } = useLayoutStore.getState();
    expect(tabs.find((tab) => tab.id === activeTabId)?.kind).toBe("Service");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("alertdialog")).toBeNull();
    // Without the capture-phase chord listener, PodTable would also see `s` and post a
    // "Scale ..." notice for the selected pod.
    expect(screen.queryByText(/Scale/)).toBeNull();
    expect(useActionsStore.getState().pending).toBeNull();
    resetPodsStore();
  });

  it("04-10: palette offers Open context shell in external terminal", async () => {
    useContextsStore.setState({
      ...initialContextsState,
      selectedContext: "kind-prod",
    });
    useLayoutStore.getState().openPalette();
    const openExtTermSpy = vi.spyOn(commands, "openExternalTerminal").mockResolvedValue({ status: "ok", data: null } as never);

    render(<CommandPalette />);
    const item = await screen.findByText("Open context shell in external terminal (kind-prod)");
    expect(item).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(item);

    expect(openExtTermSpy).toHaveBeenCalledWith({
      target: {
        kind: "contextShell",
        cluster: "kind-prod",
        namespace: null,
      },
      confirmedName: null,
    });
  });

  it("04-10: palette offers Unlock context for 15 minutes for read-only context", async () => {
    useContextsStore.setState({
      ...initialContextsState,
      contexts: [{ name: "prod-ro", cluster: "prod-ro", user: "admin", namespace: "default", server: null, auth: { type: "token" }, isCurrent: false }],
      settingsByContext: {
        "prod-ro": {
          colour: null,
          tags: [],
          readOnly: true,
          protected: false,
          pinned: false,
          unlockedUntil: null,
        },
      },
    });
    useLayoutStore.getState().openPalette();
    const unlockSpy = vi.spyOn(commands, "unlockContext").mockResolvedValue({
      status: "ok",
      data: {
        colour: null,
        tags: [],
        readOnly: true,
        protected: false,
        pinned: false,
        unlockedUntil: Math.floor(Date.now() / 1000) + 900,
      },
    } as never);

    render(<CommandPalette />);
    const item = await screen.findByText("Unlock context prod-ro for 15 minutes");
    expect(item).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(item);

    // Modal opens, click Unlock
    const unlockBtn = screen.getByTestId("unlock-confirm-execute-btn");
    await user.click(unlockBtn);

    expect(unlockSpy).toHaveBeenCalledWith("prod-ro", null);
  });
});

