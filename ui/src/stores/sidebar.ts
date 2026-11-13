import { create } from "zustand";

/** Titles of the sidebar resource groups the user collapsed (02-07c). */
export const SIDEBAR_COLLAPSED_STORAGE_KEY = "fleetglass.sidebar.collapsed";

/** More entries than the sidebar has groups means the value is not ours; ignore the rest. */
const MAX_STORED_GROUPS = 32;

/** Parses the stored group titles; anything malformed yields "nothing collapsed". */
export function parseCollapsedGroups(raw: string | null): string[] {
  if (raw === null) return [];
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(value)) return [];
  const titles = value.filter((v): v is string => typeof v === "string" && v.length > 0);
  return [...new Set(titles)].slice(0, MAX_STORED_GROUPS);
}

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    // Access can throw (e.g. storage disabled); the state then lives for the session only.
    return null;
  }
}

function load(): string[] {
  try {
    return parseCollapsedGroups(storage()?.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) ?? null);
  } catch {
    return [];
  }
}

function save(collapsed: readonly string[]) {
  try {
    storage()?.setItem(SIDEBAR_COLLAPSED_STORAGE_KEY, JSON.stringify(collapsed));
  } catch {
    // Quota or disabled storage: keep the in-memory value.
  }
}

interface SidebarState {
  collapsedGroups: string[];
  toggleGroup: (title: string) => void;
}

export const useSidebarStore = create<SidebarState>((set, get) => ({
  collapsedGroups: load(),
  toggleGroup: (title) => {
    const current = get().collapsedGroups;
    const next = current.includes(title)
      ? current.filter((t) => t !== title)
      : [...current, title];
    set({ collapsedGroups: next });
    save(next);
  },
}));

/** Test helper: re-reads storage into the store. */
export function reloadSidebarStore() {
  useSidebarStore.setState({ collapsedGroups: load() });
}
