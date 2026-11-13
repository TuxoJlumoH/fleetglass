import { isTauri } from "@tauri-apps/api/core";
import { useCallback, useSyncExternalStore } from "react";
import { useAppInfoStore } from "../stores/appInfo";

/** Any platform below this width gets the phone layout (so it can be checked in a browser). */
export const PHONE_QUERY = "(max-width: 599.98px)";
/** Android below this width gets the phone layout; tablets at or above keep the desktop one. */
export const TABLET_QUERY = "(max-width: 899.98px)";

function useMediaQuery(query: string): boolean {
  const subscribe = useCallback(
    (onChange: () => void) => {
      if (typeof window.matchMedia !== "function") return () => undefined;
      const mql = window.matchMedia(query);
      mql.addEventListener("change", onChange);
      return () => {
        mql.removeEventListener("change", onChange);
      };
    },
    [query],
  );
  const get = useCallback(
    // jsdom and very old WebViews have no matchMedia: keep the desktop layout.
    () => (typeof window.matchMedia === "function" ? window.matchMedia(query).matches : false),
    [query],
  );
  return useSyncExternalStore(subscribe, get, () => false);
}

export type ShellLayout = "mobile" | "desktop" | "pending";

/**
 * Pure rule behind `useMobileLayout`, for tests. `platform` is null until `app_info` answered:
 * at 600–899 px that could still be an Android phone in landscape, so nothing is picked yet
 * (no desktop flash); at ≥ 900 px the answer cannot change the layout, so desktop goes first.
 */
export function pickMobileLayout(platform: string | null, phoneWidth: boolean, belowTablet: boolean): ShellLayout {
  if (phoneWidth) return "mobile";
  if (!belowTablet) return "desktop";
  if (platform === null) return "pending";
  return platform === "android" ? "mobile" : "desktop";
}

/** Which shell to show; follows resizes and orientation changes. */
export function useMobileLayout(): ShellLayout {
  // Outside the app (browser dev, tests) `app_info` never answers: treat it as not Android.
  const platform = useAppInfoStore((s) => s.platform ?? (isTauri() ? null : "browser"));
  const phone = useMediaQuery(PHONE_QUERY);
  const belowTablet = useMediaQuery(TABLET_QUERY);
  return pickMobileLayout(platform, phone, belowTablet);
}
