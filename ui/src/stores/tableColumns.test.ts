import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  HIDDEN_COLUMNS_STORAGE_KEY,
  MAX_HIDDEN_PER_KIND,
  MAX_STORED_KINDS,
  hiddenColumnsOf,
  parseHiddenColumns,
  reloadTableColumnsStore,
  useTableColumnsStore,
} from "./tableColumns";

const stored = () => JSON.parse(localStorage.getItem(HIDDEN_COLUMNS_STORAGE_KEY) ?? "null") as unknown;
const hiddenOf = (kind: string) => hiddenColumnsOf(useTableColumnsStore.getState(), kind);

beforeEach(() => {
  localStorage.clear();
  reloadTableColumnsStore();
});

afterEach(() => {
  vi.restoreAllMocks();
  localStorage.clear();
  reloadTableColumnsStore();
});

describe("parseHiddenColumns", () => {
  it("ignores corrupt or non-object JSON", () => {
    expect(parseHiddenColumns(null)).toEqual({});
    expect(parseHiddenColumns("{oops")).toEqual({});
    expect(parseHiddenColumns("42")).toEqual({});
    expect(parseHiddenColumns('"pod"')).toEqual({});
    expect(parseHiddenColumns("null")).toEqual({});
    expect(parseHiddenColumns('[["IP"]]')).toEqual({});
  });

  it("keeps string labels, drops empty kinds and lower-cases kinds", () => {
    expect(
      parseHiddenColumns(
        JSON.stringify({ Pod: ["IP", 3, null, "", "IP", "Node"], service: [], node: "IP" }),
      ),
    ).toEqual({ pod: ["IP", "Node"] });
  });

  it("caps labels per kind and the number of kinds", () => {
    const many = Array.from({ length: MAX_HIDDEN_PER_KIND + 10 }, (_, i) => `c${String(i)}`);
    expect(parseHiddenColumns(JSON.stringify({ pod: many })).pod).toHaveLength(
      MAX_HIDDEN_PER_KIND,
    );
    const kinds = Object.fromEntries(
      Array.from({ length: MAX_STORED_KINDS + 5 }, (_, i) => [`k${String(i)}`, ["A"]]),
    );
    const parsed = parseHiddenColumns(JSON.stringify(kinds));
    expect(Object.keys(parsed)).toHaveLength(MAX_STORED_KINDS);
    // The oldest (first inserted) are dropped.
    expect(parsed.k0).toBeUndefined();
    expect(parsed[`k${String(MAX_STORED_KINDS + 4)}`]).toEqual(["A"]);
  });

  it("a corrupt stored value loads as nothing hidden", () => {
    localStorage.setItem(HIDDEN_COLUMNS_STORAGE_KEY, "{oops");
    reloadTableColumnsStore();
    expect(useTableColumnsStore.getState().hidden).toEqual({});
  });
});

describe("useTableColumnsStore", () => {
  it("toggles a column by label and persists it", () => {
    useTableColumnsStore.getState().toggleColumn("Pod", "IP");
    expect(hiddenOf("pod")).toEqual(["IP"]);
    expect(stored()).toEqual({ pod: ["IP"] });
    reloadTableColumnsStore();
    expect(hiddenOf("Pod")).toEqual(["IP"]);
  });

  it("deletes a kind's key when its hidden list becomes empty", () => {
    const { toggleColumn } = useTableColumnsStore.getState();
    toggleColumn("Pod", "IP");
    toggleColumn("Service", "Type");
    toggleColumn("Pod", "IP");
    expect(useTableColumnsStore.getState().hidden).toEqual({ service: ["Type"] });
    expect(stored()).toEqual({ service: ["Type"] });
  });

  it("showAll removes only that kind", () => {
    const { toggleColumn, showAll } = useTableColumnsStore.getState();
    toggleColumn("Pod", "IP");
    toggleColumn("Pod", "Node");
    toggleColumn("Service", "Type");
    showAll("POD");
    expect(hiddenOf("Pod")).toEqual([]);
    expect(stored()).toEqual({ service: ["Type"] });
  });

  it("caps stored kinds, dropping the least recently changed", () => {
    const { toggleColumn } = useTableColumnsStore.getState();
    for (let i = 0; i < MAX_STORED_KINDS; i++) toggleColumn(`k${String(i)}`, "A");
    // Touching k0 again makes it the newest; k1 is now the oldest.
    toggleColumn("k0", "B");
    toggleColumn("extra", "A");
    const hidden = useTableColumnsStore.getState().hidden;
    expect(Object.keys(hidden)).toHaveLength(MAX_STORED_KINDS);
    expect(hidden.k1).toBeUndefined();
    expect(hidden.k0).toEqual(["A", "B"]);
    expect(hidden.extra).toEqual(["A"]);
  });

  it("keeps the in-memory value when setItem throws", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("quota", "QuotaExceededError");
    });
    expect(() => {
      useTableColumnsStore.getState().toggleColumn("Pod", "IP");
    }).not.toThrow();
    expect(hiddenOf("Pod")).toEqual(["IP"]);
    expect(localStorage.getItem(HIDDEN_COLUMNS_STORAGE_KEY)).toBeNull();
  });
});
