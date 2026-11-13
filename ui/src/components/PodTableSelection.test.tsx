import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { useActionsStore } from "../stores/actions";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { type Page, type Row, resetPodsStore, setQueryFetcher, usePodsStore } from "../stores/pods";
import { useResourceDetailStore } from "../stores/resourceDetail";
import { ROW_HEIGHTS } from "../stores/settings";
import { PodTable } from "./PodTable";

function makeRow(name: string): Row {
  return {
    uid: `uid-${name}`,
    cluster: "prod-eu",
    kind: "Pod",
    namespace: "default",
    name,
    status: "healthy",
    statusReason: "Running",
    cells: [
      { type: "ready", value: { ready: 1, total: 1 } },
      { type: "status", value: { label: "Running", phase: "healthy" } },
      { type: "int", value: 0 },
      { type: "text", value: "node-0" },
      { type: "text", value: "10.244.0.1" },
      { type: "timestamp", value: "2026-01-01T00:00:00Z" },
    ],
    labels: [],
    custom: [],
  };
}

function pageOf(names: string[], version: number): Page {
  return {
    total: names.length,
    offset: 0,
    limit: 100,
    rows: names.map(makeRow),
    version,
    access: [{ cluster: "prod-eu", kind: "Pod", access: { state: "ready" }, rows: 0, metrics: null }],
    statusCounts: { healthy: 0, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
  };
}

/** Replaces the loaded rows as a backend update would (rows shift under fixed indexes). */
function publish(names: string[], version: number) {
  act(() => {
    usePodsStore.setState({ rowsByIndex: {} });
    usePodsStore.getState().applyPage(pageOf(names, version));
  });
}

function selectedRow(): HTMLElement | undefined {
  return screen.getAllByTestId("pod-row").find((el) => el.dataset.selected === "true");
}

function rowElement(name: string): HTMLElement {
  const el = screen.getByText(name).closest<HTMLElement>('[role="row"]');
  if (!el) throw new Error(`row ${name} not rendered`);
  return el;
}

const INITIAL = ["pod-a", "pod-c", "pod-e", "pod-g"];

describe("PodTable selection anchored by row uid", () => {
  beforeEach(() => {
    resetPodsStore();
    useContextsStore.setState({ ...initialContextsState, selectedContext: "prod-eu" });
    setQueryFetcher(() => Promise.resolve(pageOf(INITIAL, 1)));
  });

  afterEach(() => {
    useResourceDetailStore.getState().close();
    resetPodsStore();
  });

  it("stays on the same pod when a row is inserted above it", async () => {
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("pod-g");

    await user.click(rowElement("pod-c"));
    expect(selectedRow()).toHaveTextContent("pod-c");
    expect(selectedRow()).toHaveAttribute("data-index", "1");

    publish(["pod-a", "pod-b", "pod-c", "pod-e", "pod-g"], 2);

    expect(selectedRow()).toHaveTextContent("pod-c");
    expect(selectedRow()).toHaveAttribute("data-index", "2");
    expect(useActionsStore.getState().selection?.uid).toBe("uid-pod-c");

    // Actions and the drawer use the anchored row, not the row now at the old index.
    fireEvent.keyDown(rowElement("pod-c"), { key: "Enter" });
    expect(useResourceDetailStore.getState().target?.uid).toBe("uid-pod-c");
  });

  it("keeps the index (clamped) and adopts the row there when the selected pod is deleted", async () => {
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("pod-g");

    await user.click(rowElement("pod-e"));
    expect(selectedRow()).toHaveAttribute("data-index", "2");

    // pod-e deleted: index 2 now holds pod-g.
    publish(["pod-a", "pod-c", "pod-g"], 2);
    expect(selectedRow()).toHaveTextContent("pod-g");
    expect(selectedRow()).toHaveAttribute("data-index", "2");

    // The adopted row is the new anchor: an insert above moves the selection with pod-g.
    publish(["pod-a", "pod-b", "pod-c", "pod-g"], 3);
    expect(selectedRow()).toHaveTextContent("pod-g");
    expect(selectedRow()).toHaveAttribute("data-index", "3");

    // pod-g (last row) deleted: the index clamps to the new last row.
    publish(["pod-a", "pod-b"], 4);
    expect(selectedRow()).toHaveTextContent("pod-b");
    expect(selectedRow()).toHaveAttribute("data-index", "1");
  });

  it("moves relative to the anchored row after a re-order", async () => {
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("pod-g");

    const container = rowElement("pod-a").closest<HTMLElement>(".pod-table-container");
    if (!container) throw new Error("container not rendered");
    container.focus();
    await user.keyboard("j");
    expect(selectedRow()).toHaveTextContent("pod-c");

    // Two rows inserted above pod-c and the order changes; pod-c is now at index 3.
    publish(["pod-0", "pod-a", "pod-b", "pod-c", "pod-e", "pod-g"], 2);
    expect(selectedRow()).toHaveTextContent("pod-c");

    await user.keyboard("j");
    expect(selectedRow()).toHaveTextContent("pod-e");
    expect(selectedRow()).toHaveAttribute("data-index", "4");

    // An update right before a key press: the handler reads the re-anchored row synchronously.
    publish(["pod-0", "pod-1", "pod-a", "pod-b", "pod-c", "pod-e", "pod-g"], 3);
    await user.keyboard("k");
    expect(selectedRow()).toHaveTextContent("pod-c");
    expect(selectedRow()).toHaveAttribute("data-index", "4");
  });

  it("R-14d: focus follows the anchored row when it moves on screen", async () => {
    render(<PodTable />);
    await screen.findByText("pod-g");
    act(() => {
      rowElement("pod-c").focus();
    });
    fireEvent.click(rowElement("pod-c"));
    expect(rowElement("pod-c")).toHaveFocus();

    publish(["pod-0", "pod-a", "pod-c", "pod-e", "pod-g"], 2);
    expect(selectedRow()).toHaveTextContent("pod-c");
    await waitFor(() => {
      expect(rowElement("pod-c")).toHaveFocus();
    });
  });

  it("R-14d: a filter/sort/kind change (new query) drops the anchor", async () => {
    const user = userEvent.setup();
    render(<PodTable />);
    await screen.findByText("pod-g");
    await user.click(rowElement("pod-e"));
    expect(selectedRow()).toHaveTextContent("pod-e");

    // A new filter starts a new query: the anchor is dropped and only the index (2,
    // clamped) is kept, so the selection does not chase pod-e to index 0 in the result.
    setQueryFetcher(() => Promise.resolve(pageOf(["pod-e", "pod-x", "pod-y", "pod-z"], 5)));
    await user.click(screen.getByLabelText("Filter pods"));
    await user.keyboard("pod");
    await waitFor(() => {
      expect(screen.queryByText("pod-x")).not.toBeNull();
    });
    await waitFor(() => {
      expect(selectedRow()).toHaveTextContent("pod-y");
    });
    expect(selectedRow()).toHaveAttribute("data-index", "2");
  });

  it("R-14d: a pending focus for a row moved off screen is given up, not applied later", async () => {
    const many = Array.from({ length: 200 }, (_, i) => `pod-${String(i).padStart(3, "0")}`);
    setQueryFetcher(() => Promise.resolve(pageOf(many, 1)));
    render(<PodTable />);
    await screen.findByText("pod-000");
    act(() => {
      rowElement("pod-001").focus();
    });
    fireEvent.click(rowElement("pod-001"));

    // pod-001 moves to index 150 (far below the rendered window).
    const moved = [...many.slice(0, 1), ...many.slice(2, 151), "pod-001", ...many.slice(151)];
    publish(moved, 2);
    const container = document.querySelector<HTMLElement>(".pod-table-container");
    act(() => {
      container?.focus();
    });
    // Let the give-up frames run.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 100));
    });
    // Scrolling there later must not yank focus onto the row.
    const viewport = document.querySelector<HTMLElement>(".pod-table__viewport");
    act(() => {
      if (viewport) {
        viewport.scrollTop = 150 * ROW_HEIGHTS.comfortable;
        viewport.dispatchEvent(new Event("scroll"));
      }
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });
    expect(container).toHaveFocus();
  });

  it("R-14d: works under StrictMode (selection and keyboard move)", async () => {
    const user = userEvent.setup();
    render(
      <StrictMode>
        <PodTable />
      </StrictMode>,
    );
    await screen.findByText("pod-g");
    await user.click(rowElement("pod-c"));
    publish(["pod-a", "pod-b", "pod-c", "pod-e", "pod-g"], 2);
    expect(selectedRow()).toHaveTextContent("pod-c");
    await user.keyboard("j");
    expect(selectedRow()).toHaveTextContent("pod-e");
  });
});
