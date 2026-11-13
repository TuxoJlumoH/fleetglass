import { create } from "zustand";

/**
 * Columns the user hid with the toolbar's column picker, persisted. Keyed by lower-case kind
 * (tabs carry no API group), then by column LABEL, not position, so a changed cell layout can
 * never hide a different column.
 */
export const HIDDEN_COLUMNS_STORAGE_KEY = "fleetglass.hiddenColumns";

/** Stored kinds are capped; the oldest (insertion order) are dropped first. */
export const MAX_STORED_KINDS = 50;
/** Hidden labels kept per kind; anything beyond is not ours. */
export const MAX_HIDDEN_PER_KIND = 64;

type HiddenByKind = Record<string, string[]>;

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/** Keeps the newest {@link MAX_STORED_KINDS} non-empty kinds (object insertion order). */
function capped(hidden: HiddenByKind): HiddenByKind {
  const entries = Object.entries(hidden).filter(([, keys]) => keys.length > 0);
  return Object.fromEntries(entries.slice(Math.max(0, entries.length - MAX_STORED_KINDS)));
}

export function parseHiddenColumns(raw: string | null): HiddenByKind {
  if (raw === null) return {};
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== "object" || value === null || Array.isArray(value)) return {};
    const out: HiddenByKind = {};
    for (const [kind, keys] of Object.entries(value as Record<string, unknown>)) {
      if (!Array.isArray(keys)) continue;
      const labels = [
        ...new Set(keys.filter((k): k is string => typeof k === "string" && k.length > 0)),
      ].slice(0, MAX_HIDDEN_PER_KIND);
      if (labels.length > 0) out[kind.toLowerCase()] = labels;
    }
    return capped(out);
  } catch {
    return {};
  }
}

function load(): HiddenByKind {
  try {
    return parseHiddenColumns(storage()?.getItem(HIDDEN_COLUMNS_STORAGE_KEY) ?? null);
  } catch {
    return {};
  }
}

function save(hidden: HiddenByKind) {
  try {
    storage()?.setItem(HIDDEN_COLUMNS_STORAGE_KEY, JSON.stringify(hidden));
  } catch {
    // Quota or disabled storage: keep the in-memory value.
  }
}

const NONE: readonly string[] = [];

interface TableColumnsState {
  hidden: HiddenByKind;
  /** Hides or shows the column labelled `label` of `kind`. */
  toggleColumn: (kind: string, label: string) => void;
  showAll: (kind: string) => void;
}

function without(hidden: HiddenByKind, kind: string): HiddenByKind {
  return Object.fromEntries(Object.entries(hidden).filter(([k]) => k !== kind));
}

export const useTableColumnsStore = create<TableColumnsState>((set, get) => ({
  hidden: load(),
  toggleColumn: (kind, label) => {
    const k = kind.toLowerCase();
    const current = get().hidden[k] ?? [];
    const next = current.includes(label)
      ? current.filter((c) => c !== label)
      : [...current, label].slice(0, MAX_HIDDEN_PER_KIND);
    // Re-inserting moves the kind to the end, so the cap drops the least recently changed.
    const rest = without(get().hidden, k);
    const hidden = capped(next.length > 0 ? { ...rest, [k]: next } : rest);
    set({ hidden });
    save(hidden);
  },
  showAll: (kind) => {
    const hidden = without(get().hidden, kind.toLowerCase());
    set({ hidden });
    save(hidden);
  },
}));

/** Hidden column labels of `kind` (stable empty array when none). */
export function hiddenColumnsOf(state: TableColumnsState, kind: string): readonly string[] {
  return state.hidden[kind.toLowerCase()] ?? NONE;
}

/** Test helper: re-reads storage into the store. */
export function reloadTableColumnsStore() {
  useTableColumnsStore.setState({ hidden: load() });
}
