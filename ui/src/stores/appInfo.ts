import { create } from "zustand";

// Filled by P0-05 from the Tauri `app_info()` command (using generated types).
interface AppInfoState {
  version: string | null;
  /** Backend OS (`std::env::consts::OS`, e.g. "linux", "android"); null until loaded. */
  platform: string | null;
  setVersion: (version: string) => void;
  setPlatform: (platform: string) => void;
}

export const useAppInfoStore = create<AppInfoState>()((set) => ({
  version: null,
  platform: null,
  setVersion: (version) => {
    set({ version });
  },
  setPlatform: (platform) => {
    set({ platform });
  },
}));

/** True in the Android app (kubeconfig import and other mobile-only features). */
export function useIsAndroid(): boolean {
  return useAppInfoStore((s) => s.platform === "android");
}
