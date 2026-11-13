import { isTauri } from "@tauri-apps/api/core";
import { commands } from "./generated/bindings";
import { useAppInfoStore } from "./stores/appInfo";

/**
 * If `app_info` has not answered after this long, the platform is settled as "unknown" so the
 * shell choice (03-03a "pending" layout, a blank render) never waits forever. A late answer
 * still fills in the real platform.
 */
export const APP_INFO_TIMEOUT_MS = 3000;

function settleUnknownPlatform(): void {
  if (useAppInfoStore.getState().platform === null) useAppInfoStore.getState().setPlatform("unknown");
}

/**
 * Fetch build info from the Rust backend and put the version and platform in the store.
 * Outside Tauri (plain browser, vitest) this is a no-op and the status bar
 * keeps showing "version: —".
 */
export async function loadAppInfo(): Promise<void> {
  if (!isTauri()) return;
  const timer = setTimeout(settleUnknownPlatform, APP_INFO_TIMEOUT_MS);
  try {
    const info = await commands.appInfo();
    useAppInfoStore.getState().setVersion(info.version);
    useAppInfoStore.getState().setPlatform(info.platform);
  } catch (err: unknown) {
    console.error("app_info failed", err);
    // Settle the platform so the shell choice (03-03a) does not wait for it forever.
    settleUnknownPlatform();
  } finally {
    clearTimeout(timer);
  }
}
