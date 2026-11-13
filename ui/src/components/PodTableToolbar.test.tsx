import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { commands } from "../generated/bindings";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import {
  type Page,
  type QuerySpec,
  type Row,
  type RowStatus,
  type StatusCounts,
  type StoreAccess,
  resetPodsStore,
  setQueryFetcher,
  usePodsStore,
} from "../stores/pods";
import { useResourceDetailStore } from "../stores/resourceDetail";
import { HIDDEN_COLUMNS_STORAGE_KEY, useTableColumnsStore } from "../stores/tableColumns";
import { useActionsStore } from "../stores/actions";
import { PodTable } from "./PodTable";
import { STATUS_TONE } from "./statusTone";

const LABELS: Record<RowStatus, string> = {
  healthy: "Running",
  pending: "Unschedulable",
  degraded: "BackOff",
  failed: "CrashLoopBackOff",
  completed: "Completed",
  terminating: "Terminating",
  unknown: "Unknown",
};

function makeRow(name: string, status: RowStatus, restarts = 0): Row {
  return {
    uid: `uid-${name}`,
    cluster: "prod-eu",
    kind: "Pod",
    namespace: "default",
    name,
    status,
    statusReason: LABELS[status],
    cells: [
      { type: "ready", value: { ready: 1, total: 1 } },
      { type: "status", value: { label: LABELS[status], phase: status } },
      { type: "int", value: restarts },
      { type: "timestamp", value: "" },
      { type: "text", value: "" },
      { type: "text", value: "node-0" },
      { type: "text", value: "10.244.0.1" },
      { type: "timestamp", value: "2026-01-01T00:00:00Z" },
      { type: "usage", value: { value: 3, formatted: "3m", request: 100, limit: null } },
      { type: "text", value: "" },
    ],
    labels: [],
    custom: [],
  };
}

const ALL_ROWS: Row[] = [
  makeRow("web-0", "healthy"),
  makeRow("crashloop-0", "failed", 14),
  makeRow("pending-0", "pending"),
  makeRow("job-backoff-0", "degraded", 5),
  makeRow("migrate-0", "completed", 4),
  makeRow("old-0", "terminating"),
  makeRow("lost-0", "unknown"),
  makeRow("web-1", "healthy"),
];

function countsOf(rows: Row[]): StatusCounts {
  const c: StatusCounts = {
    healthy: 0,
    pending: 0,
    degraded: 0,
    failed: 0,
    completed: 0,
    terminating: 0,
    unknown: 0,
  };
  for (const r of rows) c[r.status] += 1;
  return c;
}

const READY: StoreAccess[] = [
  { cluster: "prod-eu", kind: "Pod", access: { state: "ready" }, rows: 8, metrics: null },
];

/** A fake query engine: applies the status filter, counts statuses without it. */
function fakeEngine(specs: QuerySpec[], access: StoreAccess[] = READY) {
  return (spec: QuerySpec): Promise<Page> => {
    specs.push(spec);
    const rows =
      spec.statuses.length === 0 ? ALL_ROWS : ALL_ROWS.filter((r) => spec.statuses.includes(r.status));
    return Promise.resolve({
      total: rows.length,
      offset: spec.offset,
      limit: spec.limit,
      rows: rows.slice(spec.offset, spec.offset + spec.limit),
      version: 1,
      access,
      statusCounts: countsOf(ALL_ROWS),
    });
  };
}

function rowOf(name: string): HTMLElement {
  const el = screen.getByText(name).closest<HTMLElement>('[role="row"]');
  if (!el) throw new Error(`row ${name} not rendered`);
  return el;
}

beforeEach(() => {
  resetPodsStore();
  localStorage.clear();
  useTableColumnsStore.setState({ hidden: {} });
  useContextsStore.setState({ ...initialContextsState, selectedContext: "prod-eu" });
});

afterEach(() => {
  useResourceDetailStore.getState().close();
  resetPodsStore();
});

describe("PodTable 02-07b table styling", () => {
  it("renders a status pill with the mockup tone for every status", async () => {
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");

    const expected: [string, RowStatus][] = [
      ["web-0", "healthy"],
      ["crashloop-0", "failed"],
      ["pending-0", "pending"],
      ["job-backoff-0", "degraded"],
      ["migrate-0", "completed"],
      ["old-0", "terminating"],
      ["lost-0", "unknown"],
    ];
    for (const [name, status] of expected) {
      const pill = rowOf(name).querySelector(".pill");
      expect(pill, name).not.toBeNull();
      expect(pill).toHaveTextContent(LABELS[status]);
      expect(pill).toHaveClass(STATUS_TONE[status]);
    }
    expect(STATUS_TONE).toEqual({
      healthy: "ok",
      pending: "warn",
      degraded: "warn",
      failed: "err",
      completed: "off",
      terminating: "warn",
      unknown: "off",
    });
  });

  it("tints failed rows only, and the selected row uses the selection style", async () => {
    const user = userEvent.setup();
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");

    expect(rowOf("crashloop-0")).toHaveClass("pod-table__row--failed");
    expect(rowOf("web-0")).not.toHaveClass("pod-table__row--failed");
    expect(rowOf("job-backoff-0")).not.toHaveClass("pod-table__row--failed");
    expect(rowOf("pending-0")).not.toHaveClass("pod-table__row--failed");

    await user.click(rowOf("crashloop-0"));
    expect(rowOf("crashloop-0")).toHaveClass("pod-table__row--selected");
  });

  it("shows restart counts of 5 or more in red", async () => {
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");

    expect(within(rowOf("crashloop-0")).getByText("14")).toHaveClass("restarts", "hot");
    expect(within(rowOf("job-backoff-0")).getByText("5")).toHaveClass("restarts", "hot");
    const four = within(rowOf("migrate-0")).getByText("4");
    expect(four).not.toHaveClass("hot");
    expect(rowOf("migrate-0").querySelector(".restarts.hot")).toBeNull();
  });

  it("renders usage cells as value + bar", async () => {
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");
    const row = rowOf("web-0");
    expect(row.querySelector(".use .bar")).not.toBeNull();
    expect(within(row).getByText("3% req")).toBeInTheDocument();
    // An unmeasured memory cell (core sends Text("")) stays a dash, never a zero bar.
    expect(row.querySelectorAll(".use")).toHaveLength(1);
    const cells = within(row).getAllByRole("cell");
    expect(cells[cells.length - 1]).toHaveTextContent(/^—$/);
  });
});

describe("PodTable 02-07b toolbar", () => {
  it("shows live counts: total, failing (failed + degraded) and pending", async () => {
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");

    expect(screen.getByTestId("pod-summary")).toHaveTextContent("8 pods2 failing1 pending");
    expect(screen.getByTestId("pod-total")).toHaveTextContent("8 pods");
    expect(screen.getByTestId("pod-failing")).toHaveTextContent("2 failing");
    expect(screen.getByTestId("pod-failing").querySelector("b")).toHaveClass("e");
    expect(screen.getByTestId("pod-pending")).toHaveTextContent("1 pending");
  });

  it("does not claim failing/pending counts while a store is not Ready", async () => {
    setQueryFetcher(
      fakeEngine(
        [],
        [
          {
            cluster: "prod-eu",
            kind: "Pod",
            access: { state: "forbidden", resource: "pods", namespace: null, message: "denied" },
            rows: 0,
            metrics: null,
          },
        ],
      ),
    );
    render(<PodTable />);
    expect(await screen.findByText(/No permission to list pods/)).toBeInTheDocument();
    expect(screen.queryByTestId("pod-failing")).toBeNull();
    expect(screen.queryByTestId("pod-pending")).toBeNull();
  });

  it("shows the / hint and / focuses the filter", async () => {
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");
    const filter = screen.getByLabelText("Filter pods");
    expect(filter.parentElement?.querySelector("kbd")).toHaveTextContent("/");
    fireEvent.keyDown(rowOf("web-0"), { key: "/" });
    expect(filter).toHaveFocus();
  });

  it("the status picker sends statuses to the backend and lists per-status counts", async () => {
    const user = userEvent.setup();
    const specs: QuerySpec[] = [];
    setQueryFetcher(fakeEngine(specs));
    render(<PodTable />);
    await screen.findByText("web-0");
    expect(specs[specs.length - 1]?.statuses).toEqual([]);

    // The visible text is the button's name (label-in-name).
    const button = screen.getByRole("button", { name: /^Status: any/ });
    expect(button).toHaveAttribute("aria-haspopup", "menu");
    await user.click(button);
    expect(button).toHaveAttribute("aria-expanded", "true");
    const menu = screen.getByRole("menu", { name: "Status filter" });
    const failed = within(menu).getByRole("menuitemcheckbox", { name: /Failed/ });
    expect(failed).toHaveTextContent("Failed1");
    expect(failed).toHaveAttribute("aria-checked", "false");
    expect(within(menu).getByRole("menuitemcheckbox", { name: /Healthy/ })).toHaveTextContent(
      "Healthy2",
    );

    await user.click(failed);
    await waitFor(() => {
      expect(specs[specs.length - 1]?.statuses).toEqual(["failed"]);
    });
    expect(specs[specs.length - 1]?.offset).toBe(0);
    expect(await screen.findByText("crashloop-0")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.queryByText("web-0")).toBeNull();
    });
    expect(button).toHaveTextContent("Status: Failed");
    expect(button).toHaveAccessibleName(/^Status: Failed/);
    expect(failed).toHaveAttribute("aria-checked", "true");
    expect(screen.getByTestId("pod-summary")).toHaveTextContent("1 pod1 failing0 pending");

    await user.click(within(menu).getByRole("menuitemcheckbox", { name: /Degraded/ }));
    await waitFor(() => {
      expect(specs[specs.length - 1]?.statuses).toEqual(["degraded", "failed"]);
    });

    await user.click(within(menu).getByRole("menuitem", { name: "Any status" }));
    await waitFor(() => {
      expect(specs[specs.length - 1]?.statuses).toEqual([]);
    });
    expect(await screen.findByText("web-0")).toBeInTheDocument();
  });

  it("the status picker is keyboard operable; Escape closes only the picker", async () => {
    const user = userEvent.setup();
    const specs: QuerySpec[] = [];
    setQueryFetcher(fakeEngine(specs));
    render(<PodTable />);
    await screen.findByText("web-0");

    // Open the drawer first: Escape in the picker must not close it.
    fireEvent.keyDown(rowOf("web-0"), { key: "Enter" });
    expect(useResourceDetailStore.getState().target?.uid).toBe("uid-web-0");

    const button = screen.getByRole("button", { name: /^Status: any/ });
    button.focus();
    await user.keyboard("{Enter}");
    expect(button).toHaveAttribute("aria-expanded", "true");
    const menu = screen.getByRole("menu", { name: "Status filter" });
    const item = (name: RegExp) => within(menu).getByRole("menuitemcheckbox", { name });
    expect(item(/Healthy/)).toHaveFocus();

    // j/k/g inside the picker never move the table selection.
    const selectedBefore = usePodsStore.getState().querySpec;
    await user.keyboard("{ArrowDown}");
    expect(item(/Pending/)).toHaveFocus();
    await user.keyboard(" ");
    await waitFor(() => {
      expect(specs[specs.length - 1]?.statuses).toEqual(["pending"]);
    });
    // One press of Space toggles exactly once (a double toggle would end unchecked).
    expect(item(/Pending/)).toHaveAttribute("aria-checked", "true");
    expect(specs.some((s) => s.statuses.length === 0 && specs.indexOf(s) > 0)).toBe(false);
    await user.keyboard("{ArrowDown}{ArrowDown}{Enter}");
    await waitFor(() => {
      expect(specs[specs.length - 1]?.statuses).toEqual(["pending", "failed"]);
    });
    expect(item(/Failed/)).toHaveAttribute("aria-checked", "true");
    expect(item(/Pending/)).toHaveAttribute("aria-checked", "true");
    expect(selectedBefore.kinds).toEqual(["Pod"]);

    await user.keyboard("{End}");
    expect(within(menu).getByRole("menuitem", { name: "Any status" })).toHaveFocus();
    await user.keyboard("{Home}");
    expect(item(/Healthy/)).toHaveFocus();

    await user.keyboard("{Escape}");
    expect(button).toHaveAttribute("aria-expanded", "false");
    expect(menu).not.toBeVisible();
    expect(button).toHaveFocus();
    // The drawer is still open.
    expect(useResourceDetailStore.getState().target?.uid).toBe("uid-web-0");
  });

  it("the column picker hides and restores columns", async () => {
    const user = userEvent.setup();
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");
    expect(screen.getByRole("columnheader", { name: /^IP$/i })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /^Columns/ }));
    const menu = screen.getByRole("menu", { name: "Columns" });
    // Name and Status are pinned.
    expect(within(menu).queryByRole("menuitemcheckbox", { name: "Name" })).toBeNull();
    expect(within(menu).queryByRole("menuitemcheckbox", { name: "Status" })).toBeNull();
    await user.click(within(menu).getByRole("menuitemcheckbox", { name: "IP" }));
    expect(screen.queryByRole("columnheader", { name: /^IP$/i })).toBeNull();
    expect(screen.queryByText("10.244.0.1")).toBeNull();
    // Stored by label, not by cell position.
    expect(JSON.parse(localStorage.getItem(HIDDEN_COLUMNS_STORAGE_KEY) ?? "null")).toEqual({
      pod: ["IP"],
    });

    await user.click(within(menu).getByRole("menuitem", { name: "Show all" }));
    expect(screen.getByRole("columnheader", { name: /^IP$/i })).toBeInTheDocument();
  });

  it("table keys still work: j/k/g/G, Enter, Escape, Ctrl+D", async () => {
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");
    const container = document.querySelector<HTMLElement>(".pod-table-container");
    if (!container) throw new Error("no container");
    const selected = () =>
      screen.getAllByTestId("pod-row").find((el) => el.dataset.selected === "true");

    act(() => {
      container.focus();
    });
    fireEvent.keyDown(container, { key: "j" });
    expect(selected()).toHaveTextContent("crashloop-0");
    fireEvent.keyDown(container, { key: "G" });
    expect(selected()).toHaveTextContent("web-1");
    fireEvent.keyDown(container, { key: "k" });
    expect(selected()).toHaveTextContent("lost-0");
    fireEvent.keyDown(container, { key: "g" });
    expect(selected()).toHaveTextContent("web-0");

    fireEvent.keyDown(container, { key: "Enter" });
    expect(useResourceDetailStore.getState().target?.uid).toBe("uid-web-0");
    fireEvent.keyDown(container, { key: "Escape" });
    expect(useResourceDetailStore.getState().target).toBeNull();

    fireEvent.keyDown(container, { key: "d", ctrlKey: true });
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
  });
});

describe("PodTable 02-07b fix round", () => {
  it("clicking an option's label text (not just the checkbox) toggles it", async () => {
    const user = userEvent.setup();
    const specs: QuerySpec[] = [];
    setQueryFetcher(fakeEngine(specs));
    render(<PodTable />);
    await screen.findByText("web-0");

    await user.click(screen.getByRole("button", { name: /^Status: any/ }));
    const menu = screen.getByRole("menu", { name: "Status filter" });
    const failed = within(menu).getByRole("menuitemcheckbox", { name: /Failed/ });
    const labelText = failed.querySelector(".tb-menu__label");
    if (!labelText) throw new Error("no label span");
    await user.click(labelText);
    await waitFor(() => {
      expect(specs[specs.length - 1]?.statuses).toEqual(["failed"]);
    });
    expect(failed).toHaveAttribute("aria-checked", "true");
    // The count and the row padding toggle too.
    const count = failed.querySelector(".tb-menu__count");
    if (!count) throw new Error("no count span");
    await user.click(count);
    await waitFor(() => {
      expect(specs[specs.length - 1]?.statuses).toEqual([]);
    });
    // The menu stays open for further toggles.
    expect(menu).toBeVisible();
  });

  it("table keys do nothing while focus is in the toolbar (picker button or menu)", async () => {
    const user = userEvent.setup();
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");
    const selected = () =>
      screen.getAllByTestId("pod-row").find((el) => el.dataset.selected === "true");
    expect(selected()).toHaveTextContent("web-0");

    const button = screen.getByRole("button", { name: /^Status: any/ });
    button.focus();
    await user.keyboard("s");
    await user.keyboard("r");
    await user.keyboard("{Control>}d{/Control}");
    await user.keyboard("jjG");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(useActionsStore.getState().pending).toBeNull();
    expect(screen.queryByText(/cannot be scaled|cannot be restarted/)).toBeNull();
    expect(selected()).toHaveTextContent("web-0");
    expect(useResourceDetailStore.getState().target).toBeNull();

    // Same from inside the open menu.
    await user.keyboard("{Enter}");
    const menu = screen.getByRole("menu", { name: "Status filter" });
    expect(within(menu).getByRole("menuitemcheckbox", { name: /Healthy/ })).toHaveFocus();
    await user.keyboard("sjG{Control>}d{/Control}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(useActionsStore.getState().pending).toBeNull();
    expect(selected()).toHaveTextContent("web-0");
  });

  it.each([
    ["loading", { state: "loading", rowsSoFar: 3 }],
    ["reconnecting", { state: "reconnecting" }],
  ] as const)("hides failing/pending counts while a store is %s", async (_name, accessState) => {
    setQueryFetcher(
      fakeEngine(
        [],
        [{ cluster: "prod-eu", kind: "Pod", access: accessState, rows: 8, metrics: null }],
      ),
    );
    render(<PodTable />);
    await screen.findByText("web-0");
    expect(screen.getByTestId("pod-total")).toHaveTextContent("8 pods");
    expect(screen.queryByTestId("pod-failing")).toBeNull();
    expect(screen.queryByTestId("pod-pending")).toBeNull();
    // The picker lists statuses without claiming counts.
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /^Status: any/ }));
    const failed = within(screen.getByRole("menu", { name: "Status filter" })).getByRole(
      "menuitemcheckbox",
      { name: /Failed/ },
    );
    expect(failed.querySelector(".tb-menu__count")).toBeNull();
  });

  it("the summary is plain text, not a live region", async () => {
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");
    expect(screen.getByTestId("pod-summary")).not.toHaveAttribute("aria-live");
  });

  it("a status-filter change drops the selection anchor (new query)", async () => {
    const user = userEvent.setup();
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");
    const selected = () =>
      screen.getAllByTestId("pod-row").find((el) => el.dataset.selected === "true");
    await user.click(rowOf("lost-0"));
    expect(selected()).toHaveTextContent("lost-0");
    expect(selected()).toHaveAttribute("data-index", "6");

    // Result: web-0, crashloop-0, lost-0, web-1. Following the uid would select lost-0 (2);
    // a new query keeps only the clamped index (3).
    act(() => {
      void usePodsStore.getState().setStatusFilter(["healthy", "failed", "unknown"]);
    });
    await waitFor(() => {
      expect(screen.queryByText("pending-0")).toBeNull();
    });
    await waitFor(() => {
      expect(selected()).toHaveTextContent("web-1");
    });
    expect(selected()).toHaveAttribute("data-index", "3");
  });

  it("setKind clears the status filter", async () => {
    setQueryFetcher(fakeEngine([]));
    await act(async () => {
      await usePodsStore.getState().setStatusFilter(["failed"]);
    });
    expect(usePodsStore.getState().querySpec.statuses).toEqual(["failed"]);
    await act(async () => {
      await usePodsStore.getState().setKind("Deployment");
    });
    expect(usePodsStore.getState().querySpec.kinds).toEqual(["Deployment"]);
    expect(usePodsStore.getState().querySpec.statuses).toEqual([]);
  });

  it("shows filter query error badge when search syntax is invalid", async () => {
    vi.spyOn(commands, "validateFilterQuery").mockResolvedValue(
      "unclosed regular expression: missing closing '/'"
    );
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");

    const input = screen.getByLabelText("Filter pods");
    fireEvent.change(input, { target: { value: "/invalid" } });

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent(
        "unclosed regular expression: missing closing '/'"
      );
    });

    const searchBox = input.closest(".tb-search");
    expect(searchBox).toHaveClass("tb-search--invalid");
  });

  it("toggles the filter syntax help popover", async () => {
    const user = userEvent.setup();
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");

    const hintBtn = screen.getByRole("button", { name: "Search syntax help" });
    expect(screen.queryByRole("tooltip")).toBeNull();

    await user.click(hintBtn);
    expect(screen.getByRole("tooltip")).toBeInTheDocument();
    expect(screen.getByText("Search Syntax")).toBeInTheDocument();
    expect(screen.getByText("web AND api")).toBeInTheDocument();

    const closeBtn = screen.getByRole("button", { name: "Close syntax help" });
    await user.click(closeBtn);
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("toggles quick resource efficiency filter pills", async () => {
    const user = userEvent.setup();
    setQueryFetcher(fakeEngine([]));
    render(<PodTable />);
    await screen.findByText("web-0");

    const nearLimitBtn = screen.getByRole("button", { name: "Near limit" });
    expect(nearLimitBtn).toHaveAttribute("aria-pressed", "false");

    await user.click(nearLimitBtn);
    expect(usePodsStore.getState().querySpec.textFilter).toBe("is:near-limit");
    expect(nearLimitBtn).toHaveAttribute("aria-pressed", "true");

    // Clicking again toggles off
    await user.click(nearLimitBtn);
    expect(usePodsStore.getState().querySpec.textFilter).toBeNull();
    expect(nearLimitBtn).toHaveAttribute("aria-pressed", "false");
  });
});
