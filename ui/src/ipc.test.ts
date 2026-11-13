import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { APP_INFO_TIMEOUT_MS, loadAppInfo } from "./ipc";
import { useAppInfoStore } from "./stores/appInfo";

describe("loadAppInfo", () => {
  beforeEach(() => {
    useAppInfoStore.setState({ version: null, platform: null });
  });

  afterEach(() => {
    clearMocks();
    delete (globalThis as { isTauri?: boolean }).isTauri;
  });

  it("is a no-op outside Tauri", async () => {
    await loadAppInfo();
    expect(useAppInfoStore.getState().version).toBeNull();
  });

  it("stores the version and platform returned by the app_info command", async () => {
    // The Tauri runtime sets this global; mockIPC does not.
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    const calls: string[] = [];
    mockIPC((cmd) => {
      calls.push(cmd);
      return cmd === "app_info" ? { version: "1.2.3", platform: "android" } : undefined;
    });

    await loadAppInfo();

    expect(calls).toEqual(["app_info"]);
    expect(useAppInfoStore.getState().version).toBe("1.2.3");
    expect(useAppInfoStore.getState().platform).toBe("android");
  });

  it("settles the platform as unknown when app_info fails", async () => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    mockIPC(() => {
      throw new Error("Command app_info not allowed by ACL");
    });
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);

    await loadAppInfo();

    expect(useAppInfoStore.getState().platform).toBe("unknown");
    expect(useAppInfoStore.getState().version).toBeNull();
    error.mockRestore();
  });

  it("keeps a known platform when a later app_info call fails", async () => {
    (globalThis as { isTauri?: boolean }).isTauri = true;
    mockWindows("main");
    useAppInfoStore.setState({ platform: "android" });
    mockIPC(() => {
      throw new Error("boom");
    });
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);

    await loadAppInfo();

    expect(useAppInfoStore.getState().platform).toBe("android");
    error.mockRestore();
  });

  it("settles the platform as unknown when app_info hangs, then takes a late answer", async () => {
    vi.useFakeTimers();
    try {
      (globalThis as { isTauri?: boolean }).isTauri = true;
      mockWindows("main");
      let answer: (v: unknown) => void = () => undefined;
      mockIPC((cmd) =>
        cmd === "app_info"
          ? new Promise((resolve) => {
              answer = resolve;
            })
          : undefined,
      );

      const done = loadAppInfo();
      await vi.advanceTimersByTimeAsync(APP_INFO_TIMEOUT_MS - 1);
      expect(useAppInfoStore.getState().platform).toBeNull();
      await vi.advanceTimersByTimeAsync(1);
      expect(useAppInfoStore.getState().platform).toBe("unknown");

      answer({ version: "1.2.3", platform: "android" });
      await done;
      expect(useAppInfoStore.getState().platform).toBe("android");
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not override the platform after a timely answer", async () => {
    vi.useFakeTimers();
    try {
      (globalThis as { isTauri?: boolean }).isTauri = true;
      mockWindows("main");
      mockIPC((cmd) => (cmd === "app_info" ? { version: "1", platform: "linux" } : undefined));
      await loadAppInfo();
      await vi.advanceTimersByTimeAsync(APP_INFO_TIMEOUT_MS * 2);
      expect(useAppInfoStore.getState().platform).toBe("linux");
    } finally {
      vi.useRealTimers();
    }
  });
});
