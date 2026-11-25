import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { PodTable } from "./PodTable";
import { SplitPaneContainer } from "./SplitPaneContainer";
import { TabsArea } from "./TabsArea";
import { deleteActionFor, resetActionsStore, useActionsStore } from "../stores/actions";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { useLayoutStore } from "../stores/layout";
import { resetOverviewStore, retainOverview, startOverview } from "../stores/overview";
import {
  resetPodsStore,
  setQueryFetcher,
  usePodsStore,
  useSecondaryPodsStore,
  type Page,
  type QueryFetcher,
  type Row,
} from "../stores/pods";
import { resetResourceDetailStore, useResourceDetailStore } from "../stores/resourceDetail";
import {
  HIDDEN_COLUMNS_STORAGE_KEY,
  hiddenColumnsOf,
  reloadTableColumnsStore,
  useTableColumnsStore,
} from "../stores/tableColumns";
import { podsLayoutState } from "../test/layout";

function podRow(name: string, cluster = "kind-a"): Row {
  return {
    uid: `uid-${name}`,
    cluster,
    kind: "Pod",
    namespace: "default",
    name,
    status: "healthy",
    statusReason: "Running",
    cells: [
      { type: "text", value: name },
      { type: "text", value: "1/1" },
      { type: "status", value: { label: "Running", phase: "healthy" } },
    ],
    labels: [],
    custom: [],
  };
}

const A1 = podRow("a-one");
const A2 = podRow("a-two");
const B1 = podRow("b-one", "kind-b");

function fakePage(rows: Row[]): Page {
  return {
    rows,
    total: rows.length,
    offset: 0,
    limit: 100,
    version: 1,
    access: [],
    statusCounts: {
      healthy: rows.length,
      pending: 0,
      degraded: 0,
      failed: 0,
      completed: 0,
      terminating: 0,
      unknown: 0,
    },
  };
}

const pageOf =
  (rows: Row[]): QueryFetcher =>
  () =>
    Promise.resolve(fakePage(rows));

/** Live `subscribe_query` ids the backend would still be streaming to. */
let openQuerySubs: Set<number>;
let overviewSubs: { opened: number; closed: number };
let yamlFetches: string[];

function mockBackend() {
  let nextId = 1;
  mockIPC((cmd, args) => {
    const a = args as Record<string, unknown>;
    switch (cmd) {
      case "query_page":
        return fakePage([A1, A2]);
      case "subscribe_query": {
        const id = nextId++;
        openQuerySubs.add(id);
        return id;
      }
      case "unsubscribe_query":
        openQuerySubs.delete(a.subscriptionId as number);
        return true;
      case "subscribe_overview":
        overviewSubs.opened += 1;
        return nextId++;
      case "unsubscribe_overview":
        overviewSubs.closed += 1;
        return true;
      case "get_resource_detail":
        yamlFetches.push(a.name as string);
        return { yaml: `kind: Pod\nmetadata:\n  name: ${String(a.name)}\n`, events: [] };
      default:
        return undefined;
    }
  });
}

async function flush() {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

function twoTables() {
  return render(
    <div style={{ height: "600px", width: "1000px" }}>
      <SplitPaneContainer
        splitMode="horizontal"
        primary={<PodTable paneId="primary" kind="Pod" clusters={["kind-a"]} />}
        secondary={<PodTable paneId="secondary" kind="Pod" clusters={["kind-b"]} />}
      />
    </div>,
  );
}

beforeEach(() => {
  openQuerySubs = new Set();
  overviewSubs = { opened: 0, closed: 0 };
  yamlFetches = [];
  useContextsStore.setState({ ...initialContextsState, selectedContext: "kind-a" });
  useLayoutStore.setState({
    ...podsLayoutState,
    tabs: [
      { id: "tab-1", title: "Pods A", view: "resources", kind: "Pod", cluster: "kind-a" },
      {
        id: "tab-2",
        title: "Deploys",
        view: "resources",
        kind: "Deployment",
        cluster: "kind-b",
        namespace: "web",
      },
    ],
    activeTabId: "tab-1",
    splitMode: "horizontal",
    activePane: "primary",
  });
  (globalThis as { isTauri?: boolean }).isTauri = true;
  mockBackend();
  resetActionsStore();
  resetResourceDetailStore();
  resetPodsStore();
  localStorage.removeItem(HIDDEN_COLUMNS_STORAGE_KEY);
  reloadTableColumnsStore();
});

afterEach(() => {
  clearMocks();
  delete (globalThis as { isTauri?: boolean }).isTauri;
  resetActionsStore();
  resetResourceDetailStore();
  resetPodsStore();
  resetOverviewStore();
});

describe("05-03 review: actions and selection per pane", () => {
  it("only the active pane drives the global selection, and switching panes re-syncs it", () => {
    const actions = () => useActionsStore.getState();
    actions().setSelection(A1, "primary");
    actions().setSelection(B1, "secondary");
    expect(actions().selection).toBe(A1);
    expect(actions().selectionPane).toBe("primary");

    act(() => {
      useLayoutStore.getState().setActivePane("secondary");
    });
    expect(actions().selection).toBe(B1);
    expect(actions().selectionPane).toBe("secondary");

    // The secondary table unmounting drops its row; the primary's comes back on switch.
    actions().clearSelection("secondary");
    expect(actions().selection).toBeNull();
    act(() => {
      useLayoutStore.getState().setActivePane("primary");
    });
    expect(actions().selection).toBe(A1);
  });

  it("renders exactly one confirm dialog, in the pane that requested the action", async () => {
    setQueryFetcher(pageOf([A1]), usePodsStore);
    setQueryFetcher(pageOf([B1]), useSecondaryPodsStore);
    twoTables();
    await flush();

    act(() => {
      useActionsStore.getState().request(deleteActionFor(B1), "secondary");
    });
    const dialogs = screen.getAllByRole("dialog");
    expect(dialogs).toHaveLength(1);
    expect(screen.getByTestId("split-pane-secondary")).toContainElement(dialogs[0] ?? null);
    // The safety gate is unchanged: the confirm button needs the typed name on protected
    // contexts and the preview still loads; here we only check the pane isolation.
    expect(useActionsStore.getState().pendingPane).toBe("secondary");
  });

  it("an unmounting inactive table never cancels the other pane's pending action", async () => {
    setQueryFetcher(pageOf([A1]), usePodsStore);
    setQueryFetcher(pageOf([B1]), useSecondaryPodsStore);
    const { rerender } = twoTables();
    await flush();
    act(() => {
      useActionsStore.getState().request(deleteActionFor(A1), "primary");
    });
    rerender(
      <div style={{ height: "600px", width: "1000px" }}>
        <SplitPaneContainer
          splitMode="horizontal"
          primary={<PodTable paneId="primary" kind="Pod" clusters={["kind-a"]} />}
          secondary={null}
        />
      </div>,
    );
    expect(useActionsStore.getState().pending).not.toBeNull();
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
  });
});

describe("05-03 review: secondary pane state", () => {
  it("reloads the secondary YAML editor when the selection changes", async () => {
    useLayoutStore.setState({ secondaryView: "yaml" });
    render(<TabsArea />);
    act(() => {
      useActionsStore.getState().setSelection(A1, "primary");
    });
    const secondary = screen.getByTestId("split-pane-secondary");
    expect(await within(secondary).findByTestId("secondary-yaml-editor")).toBeInTheDocument();

    // An unsaved edit for a-one must not be applied to (or shown for) a-two.
    fireEvent.change(within(secondary).getByLabelText("Resource YAML"), {
      target: { value: "kind: Pod\nmetadata:\n  name: edited-a-one\n" },
    });

    act(() => {
      useActionsStore.getState().setSelection(A2, "primary");
    });
    await flush();
    expect(yamlFetches).toEqual(["a-one", "a-two"]);
    const editor = within(secondary).getByLabelText("Resource YAML");
    expect(editor).toHaveValue("kind: Pod\nmetadata:\n  name: a-two\n");
  });

  it("typing a namespace filter keeps the secondary table mounted and requeries once", async () => {
    const specs: string[][] = [];
    setQueryFetcher((spec) => {
      specs.push([...spec.namespaces]);
      return Promise.resolve(fakePage([B1]));
    }, useSecondaryPodsStore);
    useLayoutStore.setState({ secondaryView: "resources" });
    render(<TabsArea />);
    await flush();
    const secondary = screen.getByTestId("split-pane-secondary");
    const input = within(secondary).getByLabelText("Namespace filter");
    specs.length = 0;

    fireEvent.change(input, { target: { value: "kube-system" } });
    await flush();

    expect(input.isConnected).toBe(true);
    expect(useLayoutStore.getState().secondaryNamespace).toBe("kube-system");
    expect(specs).toEqual([["kube-system"]]);
  });

  it("a tab dropped on the secondary pane remounts it with the tab's kind and filters in one query", async () => {
    const specs: { kinds: string[]; namespaces: string[] }[] = [];
    setQueryFetcher((spec) => {
      specs.push({ kinds: [...spec.kinds], namespaces: [...spec.namespaces] });
      return Promise.resolve(fakePage([]));
    }, useSecondaryPodsStore);
    useLayoutStore.setState({ secondaryView: "resources" });
    render(<TabsArea />);
    await flush();
    specs.length = 0;
    const token = useLayoutStore.getState().secondaryResetToken;

    const tab = useLayoutStore.getState().tabs[1];
    if (!tab) throw new Error("missing tab");
    act(() => {
      useLayoutStore.getState().loadTabIntoSecondary(tab);
    });
    await flush();

    expect(useLayoutStore.getState().secondaryResetToken).toBe(token + 1);
    expect(specs[0]).toEqual({ kinds: ["Deployment"], namespaces: ["web"] });
    expect(specs.some((s) => s.kinds[0] === "Deployment" && s.namespaces.length === 0)).toBe(false);
  });

  it("drag-leave onto a child keeps the drop highlight", () => {
    useLayoutStore.setState({ secondaryView: "resources" });
    render(<TabsArea />);
    const pane = screen.getByTestId("secondary-pane");
    const child = within(pane).getByLabelText("Secondary pane view");
    fireEvent.dragOver(pane, { dataTransfer: { dropEffect: "none" } });
    expect(pane).toHaveAttribute("data-dragover", "true");

    // jsdom has no DragEvent, which would drop `relatedTarget`.
    const leave = (to: Element) =>
      fireEvent(pane, new MouseEvent("dragleave", { bubbles: true, relatedTarget: to }));
    leave(child);
    expect(pane).toHaveAttribute("data-dragover", "true");
    leave(document.body);
    expect(pane).not.toHaveAttribute("data-dragover", "true");
  });

  it("closing the split resets the active pane to primary", () => {
    useLayoutStore.getState().setActivePane("secondary");
    useLayoutStore.getState().toggleSplit();
    expect(useLayoutStore.getState().activePane).toBe("primary");
    useLayoutStore.setState({ splitMode: "vertical", activePane: "secondary" });
    useLayoutStore.getState().setSplitMode("none");
    expect(useLayoutStore.getState().activePane).toBe("primary");
  });
});

describe("05-03 review: live subscriptions", () => {
  it("closes the secondary pane's subscription when its view changes or the split closes", async () => {
    useLayoutStore.setState({ secondaryView: "resources", secondaryCluster: "kind-b" });
    render(<TabsArea />);
    await flush();
    expect(openQuerySubs.size).toBe(2);
    expect(useSecondaryPodsStore.getState().liveSubscribed).toBe(true);

    act(() => {
      useLayoutStore.getState().setSecondaryView("logs");
    });
    await flush();
    expect(openQuerySubs.size).toBe(1);
    expect(useSecondaryPodsStore.getState().liveSubscribed).toBe(false);

    act(() => {
      useLayoutStore.getState().setSecondaryView("resources");
    });
    await flush();
    expect(openQuerySubs.size).toBe(2);

    act(() => {
      useLayoutStore.getState().setSplitMode("none");
    });
    await flush();
    expect(openQuerySubs.size).toBe(1);
    expect(usePodsStore.getState().liveSubscribed).toBe(true);
  });

  it("resetPodsStore clears both panes' stores and subscriptions", async () => {
    useLayoutStore.setState({ secondaryView: "resources", secondaryCluster: "kind-b" });
    const { unmount } = render(<TabsArea />);
    await flush();
    unmount();
    await useSecondaryPodsStore.getState().setTextFilter("x");
    resetPodsStore();
    await flush();
    expect(openQuerySubs.size).toBe(0);
    expect(useSecondaryPodsStore.getState().querySpec.textFilter).toBeNull();
    expect(useSecondaryPodsStore.getState().querySpec.clusters).toEqual([]);
    expect(usePodsStore.getState().rowsByIndex).toEqual({});
  });

  it("two overview users share one subscription until the last releases it", async () => {
    const releaseA = retainOverview();
    startOverview(["kind-a"]);
    const releaseB = retainOverview();
    startOverview(["kind-a"]);
    await flush();
    expect(overviewSubs.opened).toBe(1);

    releaseA();
    releaseA();
    await flush();
    expect(overviewSubs.closed).toBe(0);

    releaseB();
    await flush();
    expect(overviewSubs.closed).toBe(1);
  });
});

describe("05-03 review: keyboard ownership", () => {
  it("keys on split toolbar buttons never reach the active table", async () => {
    setQueryFetcher(pageOf([A1, A2]), usePodsStore);
    setQueryFetcher(pageOf([B1]), useSecondaryPodsStore);
    twoTables();
    await flush();
    expect(useActionsStore.getState().selection?.uid).toBe(A1.uid);

    const button = screen.getByRole("button", { name: "Split horizontally" });
    fireEvent.keyDown(button, { key: "j" });
    expect(useActionsStore.getState().selection?.uid).toBe(A1.uid);

    fireEvent.keyDown(document.body, { key: "j" });
    expect(useActionsStore.getState().selection?.uid).toBe(A2.uid);
  });

  it("Escape in one pane leaves the other pane's drawer open", async () => {
    setQueryFetcher(pageOf([A1]), usePodsStore);
    setQueryFetcher(pageOf([B1]), useSecondaryPodsStore);
    twoTables();
    await flush();
    await act(async () => {
      await useResourceDetailStore.getState().open(A1, "primary");
    });

    const secondaryTable = document.querySelector<HTMLElement>(
      '.pod-table-container[data-pane-id="secondary"]',
    );
    if (!secondaryTable) throw new Error("no secondary table");
    fireEvent.keyDown(secondaryTable, { key: "Escape" });
    expect(useResourceDetailStore.getState().target).toBe(A1);
  });

  it("Ctrl+W switches panes even while a CodeMirror editor has focus", async () => {
    useLayoutStore.setState({ secondaryView: "resources" });
    render(<TabsArea />);
    await flush();
    const editor = document.createElement("div");
    editor.className = "cm-content";
    editor.contentEditable = "true";
    editor.tabIndex = 0;
    screen.getByTestId("split-pane-primary").appendChild(editor);
    editor.focus();

    fireEvent.keyDown(editor, { key: "w", ctrlKey: true });

    expect(useLayoutStore.getState().activePane).toBe("secondary");
    expect(document.activeElement).toBe(
      document.querySelector('.pod-table-container[data-pane-id="secondary"]'),
    );
  });
});

describe("05-03 review: columns per pane", () => {
  it("keeps the primary pane's persisted shape and loads saved columns unchanged", () => {
    localStorage.setItem(HIDDEN_COLUMNS_STORAGE_KEY, JSON.stringify({ pod: ["IP"] }));
    reloadTableColumnsStore();
    expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod")).toEqual(["IP"]);
    expect(hiddenColumnsOf(useTableColumnsStore.getState(), "Pod", "secondary")).toEqual([]);

    useTableColumnsStore.getState().toggleColumn("Pod", "Node", "secondary");
    useTableColumnsStore.getState().toggleColumn("Pod", "Age", "primary");
    expect(JSON.parse(localStorage.getItem(HIDDEN_COLUMNS_STORAGE_KEY) ?? "null")).toEqual({
      "secondary:pod": ["Node"],
      pod: ["IP", "Age"],
    });

    useTableColumnsStore.getState().showAll("Pod", "secondary");
    expect(JSON.parse(localStorage.getItem(HIDDEN_COLUMNS_STORAGE_KEY) ?? "null")).toEqual({
      pod: ["IP", "Age"],
    });
  });
});
