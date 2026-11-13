import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  type Page,
  type QuerySpec,
  type Row,
  resetPodsStore,
  setQueryFetcher,
  usePodsStore,
} from "../stores/pods";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { PodTable } from "./PodTable";

function makeSyntheticRow(index: number): Row {
  return {
    uid: `uid-${String(index)}`,
    cluster: "prod-eu",
    kind: "Pod",
    namespace: "default",
    name: `pod-${String(index).padStart(5, "0")}`,
    status: "healthy",
    statusReason: "Running",
    cells: [
      { type: "ready", value: { ready: 1, total: 1 } },
      { type: "status", value: { label: "Running", phase: "healthy" } },
      { type: "int", value: index % 5 },
      { type: "text", value: `node-${String(index % 10)}` },
      { type: "text", value: `10.244.0.${String((index % 250) + 1)}` },
      { type: "timestamp", value: "2026-01-01T00:00:00Z" },
    ],
    labels: [["app", "web"]],
    custom: [],
  };
}

function makePage(offset: number, limit: number, total = 50_000): Page {
  const count = Math.max(0, Math.min(limit, total - offset));
  const rows: Row[] = [];
  for (let i = 0; i < count; i += 1) {
    rows.push(makeSyntheticRow(offset + i));
  }
  return {
    total,
    offset,
    limit,
    rows,
    version: 1,
    access: [{ cluster: "prod-eu", kind: "Pod", access: { state: "ready" }, rows: 0, metrics: null }],
    statusCounts: { healthy: 0, pending: 0, degraded: 0, failed: 0, completed: 0, terminating: 0, unknown: 0 },
  };
}

describe("PodTable virtualised table and pods store", () => {
  beforeEach(() => {
    resetPodsStore();
    useContextsStore.setState({ ...initialContextsState, selectedContext: "prod-eu" });
  });

  afterEach(() => {
    resetPodsStore();
  });

  it("renders 50,000 pods total badge and mounts only visible virtual rows", async () => {
    const fetcher = vi.fn((spec: QuerySpec) =>
      Promise.resolve(makePage(spec.offset, spec.limit, 50_000)),
    );
    setQueryFetcher(fetcher);

    render(<PodTable />);

    expect(await screen.findByText("pod-00000")).toBeInTheDocument();
    expect(screen.getByTestId("pod-total")).toHaveTextContent("50,000 pods");

    const mountedRows = screen.getAllByTestId("pod-row");
    expect(mountedRows.length).toBeGreaterThan(0);
    expect(mountedRows.length).toBeLessThan(100);
    expect(screen.queryByText("pod-00099")).toBeNull();

    // Only the first 100-row page is held in the sparse cache.
    expect(Object.keys(usePodsStore.getState().rowsByIndex)).toHaveLength(100);
  });

  it("calls the backend fetcher with textFilter and offset 0 when typing in Filter pods", async () => {
    const user = userEvent.setup();
    const specs: QuerySpec[] = [];
    setQueryFetcher((spec) => {
      specs.push(spec);
      return Promise.resolve(makePage(spec.offset, spec.limit, 12));
    });

    render(<PodTable />);
    await screen.findByText("pod-00000");

    const filterInput = screen.getByLabelText("Filter pods");
    await user.type(filterInput, "nginx");

    const lastSpec = specs[specs.length - 1];
    expect(lastSpec).toBeDefined();
    expect(lastSpec?.textFilter).toBe("nginx");
    expect(lastSpec?.offset).toBe(0);
  });

  it("calls the backend fetcher with updated sort spec when clicking Status or Restarts headers", async () => {
    const user = userEvent.setup();
    const specs: QuerySpec[] = [];
    setQueryFetcher((spec) => {
      specs.push(spec);
      return Promise.resolve(makePage(spec.offset, spec.limit, 500));
    });

    render(<PodTable />);
    await screen.findByText("pod-00000");

    // Click Status column header -> sort by status asc, then click again -> desc
    const statusHeader = screen.getByRole("columnheader", { name: /Status/i });
    await user.click(statusHeader);
    expect(specs[specs.length - 1]?.sort).toEqual({
      field: { type: "status" },
      direction: "asc",
    });
    expect(specs[specs.length - 1]?.offset).toBe(0);
    expect(statusHeader).toHaveAttribute("aria-sort", "ascending");
    expect(statusHeader).toHaveTextContent("▲");

    await user.click(statusHeader);
    expect(specs[specs.length - 1]?.sort).toEqual({
      field: { type: "status" },
      direction: "desc",
    });
    expect(statusHeader).toHaveAttribute("aria-sort", "descending");
    expect(statusHeader).toHaveTextContent("▼");

    // Click Restarts column header -> sort by cell index 2 asc
    const restartsHeader = screen.getByRole("columnheader", { name: /Restarts/i });
    await user.click(restartsHeader);
    expect(specs[specs.length - 1]?.sort).toEqual({
      field: { type: "cell", index: 2 },
      direction: "asc",
    });
    expect(specs[specs.length - 1]?.offset).toBe(0);
    expect(restartsHeader).toHaveAttribute("aria-sort", "ascending");
  });

  it("fetches aligned 100-row pages via ensureRange when scrolling to unloaded indices", async () => {
    const specs: QuerySpec[] = [];
    setQueryFetcher((spec) => {
      specs.push(spec);
      return Promise.resolve(makePage(spec.offset, spec.limit, 50_000));
    });

    render(<PodTable />);
    await screen.findByText("pod-00000");

    await act(async () => {
      await usePodsStore.getState().ensureRange(240, 265);
    });

    const pageSpec = specs.find((s) => s.offset === 200);
    expect(pageSpec).toBeDefined();
    expect(pageSpec?.limit).toBe(100);
    expect(usePodsStore.getState().rowsByIndex[250]?.name).toBe("pod-00250");
  });
});
