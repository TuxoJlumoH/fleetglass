import { create } from "zustand";
import { commands, type ResourceDetail } from "../generated/bindings";
import type { PaneId } from "./layout";
import type { Row } from "./pods";

/** Fetch state for the resource detail drawer (`get_resource_detail`, R-04). */
export interface ResourceDetailState {
  /** Row the drawer is open for; `null` means the drawer is closed. */
  target: Row | null;
  /** Split pane whose view shows the drawer; only that pane renders or closes it. */
  ownerPane: PaneId;
  detail: ResourceDetail | null;
  loading: boolean;
  error: string | null;
  /**
   * Opens the drawer for `row` in `pane` and fetches its detail. Supersedes any in-flight
   * request. Without `pane`, an open drawer stays in its pane (a related link), else "primary".
   */
  open: (row: Row, pane?: PaneId) => Promise<void>;
  close: () => void;
}

export const initialResourceDetailState = {
  target: null as Row | null,
  ownerPane: "primary" as PaneId,
  detail: null as ResourceDetail | null,
  loading: false,
  error: null as string | null,
};

// Bumped on every open/close; a response from an older generation is dropped so a slow
// fetch for a previously opened row never replaces the detail of the current one.
let generation = 0;

function errorMessage(err: unknown): string {
  return err instanceof Error
    ? err.message
    : typeof err === "object" && err !== null && "message" in err
      ? String(err.message)
      : String(err);
}

export function resetResourceDetailStore(): void {
  generation += 1;
  useResourceDetailStore.setState({ ...initialResourceDetailState });
}

export const useResourceDetailStore = create<ResourceDetailState>()((set, get) => ({
  ...initialResourceDetailState,
  open: async (row, pane) => {
    generation += 1;
    const gen = generation;
    const current = get();
    const ownerPane = pane ?? (current.target !== null ? current.ownerPane : "primary");
    set({ target: row, ownerPane, detail: null, loading: true, error: null });
    try {
      const res = await commands.getResourceDetail(
        row.cluster,
        row.kind,
        row.namespace ?? null,
        row.name,
      );
      if (gen !== generation) return;
      if (res.status === "error") {
        set({ loading: false, error: res.error.message });
        return;
      }
      set({ loading: false, detail: res.data });
    } catch (err: unknown) {
      if (gen !== generation) return;
      set({ loading: false, error: errorMessage(err) });
    }
  },
  close: () => {
    generation += 1;
    set({ ...initialResourceDetailState });
  },
}));
