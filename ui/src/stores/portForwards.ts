import { isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import {
  commands,
  type PortForwardInfo_Serialize,
  type SavedPortForward,
} from "../generated/bindings";

export interface PortForwardsState {
  forwards: PortForwardInfo_Serialize[];
  saved: SavedPortForward[];
  loading: boolean;
  error: string | null;
  loadForwards: () => Promise<void>;
  loadSaved: () => Promise<void>;
  stopForward: (id: number) => Promise<void>;
  startForward: (
    context: string,
    namespace: string,
    pod: string,
    remotePort: number,
    localPort: number,
  ) => Promise<PortForwardInfo_Serialize | null>;
  startForwardTarget: (
    context: string,
    namespace: string,
    targetKind: string,
    targetName: string,
    containerPort: number,
    localPort: number,
  ) => Promise<PortForwardInfo_Serialize | null>;
  startSavedForward: (
    saved: SavedPortForward,
  ) => Promise<PortForwardInfo_Serialize | null>;
  saveBookmark: (forward: SavedPortForward) => Promise<boolean>;
  deleteBookmark: (id: string) => Promise<boolean>;
}

export const initialPortForwardsState: {
  forwards: PortForwardInfo_Serialize[];
  saved: SavedPortForward[];
  loading: boolean;
  error: string | null;
} = {
  forwards: [],
  saved: [],
  loading: false,
  error: null,
};

export const usePortForwardsStore = create<PortForwardsState>()((set, get) => ({
  ...initialPortForwardsState,
  loadForwards: async () => {
    if (!isTauri()) return;
    set({ loading: true, error: null });
    try {
      const [res, savedRes] = await Promise.all([
        commands.listPortForwards(),
        commands.listSavedPortForwards(),
      ]);
      if (res.status === "ok") {
        set({
          forwards: Array.isArray(res.data) ? res.data : [],
          saved: savedRes.status === "ok" && Array.isArray(savedRes.data) ? savedRes.data : get().saved,
          loading: false,
        });
      } else {
        set({ error: res.error.message, loading: false });
      }
    } catch (err: unknown) {
      set({
        error: err instanceof Error ? err.message : String(err),
        loading: false,
      });
    }
  },
  loadSaved: async () => {
    if (!isTauri()) return;
    try {
      const res = await commands.listSavedPortForwards();
      if (res.status === "ok") {
        set({ saved: Array.isArray(res.data) ? res.data : [] });
      } else {
        set({ error: res.error.message });
      }
    } catch (err: unknown) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },
  stopForward: async (id: number) => {
    if (!isTauri()) return;
    try {
      const res = await commands.stopPortForward(id);
      if (res.status === "ok") {
        set((state) => ({
          forwards: state.forwards.filter((f) => f.id !== id),
        }));
      } else {
        set({ error: res.error.message });
      }
    } catch (err: unknown) {
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },
  startForward: async (
    context: string,
    namespace: string,
    pod: string,
    remotePort: number,
    localPort: number,
  ) => {
    if (!isTauri()) return null;
    try {
      const res = await commands.startPortForward(
        context,
        namespace,
        pod,
        remotePort,
        localPort,
      );
      if (res.status === "ok") {
        const item = res.data;
        set((state) => ({
          forwards: state.forwards.some((f) => f.id === item.id)
            ? state.forwards
            : [...state.forwards, item],
        }));
        return item;
      } else {
        set({ error: res.error.message });
        return null;
      }
    } catch (err: unknown) {
      set({ error: err instanceof Error ? err.message : String(err) });
      return null;
    }
  },
  startForwardTarget: async (
    context: string,
    namespace: string,
    targetKind: string,
    targetName: string,
    containerPort: number,
    localPort: number,
  ) => {
    if (!isTauri()) return null;
    try {
      const res = await commands.startPortForwardTarget(
        context,
        namespace,
        targetKind,
        targetName,
        containerPort,
        localPort,
      );
      if (res.status === "ok") {
        const item = res.data;
        set((state) => ({
          forwards: state.forwards.some((f) => f.id === item.id)
            ? state.forwards
            : [...state.forwards, item],
        }));
        return item;
      } else {
        set({ error: res.error.message });
        return null;
      }
    } catch (err: unknown) {
      set({ error: err instanceof Error ? err.message : String(err) });
      return null;
    }
  },
  startSavedForward: async (saved: SavedPortForward) => {
    return get().startForwardTarget(
      saved.context,
      saved.namespace,
      saved.targetKind,
      saved.targetName,
      saved.containerPort,
      saved.localPort,
    );
  },
  saveBookmark: async (forward: SavedPortForward) => {
    if (!isTauri()) return false;
    try {
      const res = await commands.savePortForward(forward);
      if (res.status === "ok") {
        set((state) => ({
          saved: [
            ...state.saved.filter((s) => s.id !== forward.id),
            forward,
          ],
        }));
        return true;
      } else {
        set({ error: res.error.message });
        return false;
      }
    } catch (err: unknown) {
      set({ error: err instanceof Error ? err.message : String(err) });
      return false;
    }
  },
  deleteBookmark: async (id: string) => {
    if (!isTauri()) return false;
    try {
      const res = await commands.deleteSavedPortForward(id);
      if (res.status === "ok") {
        set((state) => ({
          saved: state.saved.filter((s) => s.id !== id),
        }));
        return true;
      } else {
        set({ error: res.error.message });
        return false;
      }
    } catch (err: unknown) {
      set({ error: err instanceof Error ? err.message : String(err) });
      return false;
    }
  },
}));

export function resetPortForwardsStore(): void {
  usePortForwardsStore.setState(initialPortForwardsState);
}
