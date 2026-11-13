import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_SETTINGS,
  ROW_HEIGHTS,
  SETTINGS_STORAGE_KEY,
  parseSettings,
  reloadSettingsStore,
  useSettingsStore,
} from "./settings";

describe("settings store", () => {
  // eslint-disable-next-line @typescript-eslint/unbound-method
  const originalMatchMedia = window.matchMedia;

  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    document.documentElement.classList.remove("light");
    document.body.removeAttribute("data-theme");
    document.body.classList.remove("light");
    reloadSettingsStore();
  });

  afterEach(() => {
    localStorage.clear();
    window.matchMedia = originalMatchMedia;
    reloadSettingsStore();
  });

  it("defaults to comfortable (34 px) rows and dark theme", () => {
    expect(useSettingsStore.getState().density).toBe("comfortable");
    expect(useSettingsStore.getState().theme).toBe("dark");
    expect(useSettingsStore.getState().resolvedTheme).toBe("dark");
    expect(ROW_HEIGHTS.comfortable).toBe(34);
    expect(ROW_HEIGHTS.compact).toBe(28);
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    expect(document.documentElement.classList.contains("light")).toBe(false);
  });

  it("persists density and theme and reloads them", () => {
    useSettingsStore.getState().setDensity("compact");
    expect(JSON.parse(localStorage.getItem(SETTINGS_STORAGE_KEY) ?? "null")).toEqual({
      density: "compact",
      theme: "dark",
    });

    useSettingsStore.getState().setTheme("light");
    expect(JSON.parse(localStorage.getItem(SETTINGS_STORAGE_KEY) ?? "null")).toEqual({
      density: "compact",
      theme: "light",
    });
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(document.documentElement.classList.contains("light")).toBe(true);

    useSettingsStore.setState({ density: "comfortable", theme: "dark", resolvedTheme: "dark" });
    reloadSettingsStore();
    expect(useSettingsStore.getState().density).toBe("compact");
    expect(useSettingsStore.getState().theme).toBe("light");
    expect(useSettingsStore.getState().resolvedTheme).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(document.documentElement.classList.contains("light")).toBe(true);
  });

  it("toggles between comfortable and compact", () => {
    useSettingsStore.getState().toggleDensity();
    expect(useSettingsStore.getState().density).toBe("compact");
    useSettingsStore.getState().toggleDensity();
    expect(useSettingsStore.getState().density).toBe("comfortable");
  });

  it("cycles through dark -> light -> system -> dark", () => {
    expect(useSettingsStore.getState().theme).toBe("dark");

    useSettingsStore.getState().cycleTheme();
    expect(useSettingsStore.getState().theme).toBe("light");
    expect(useSettingsStore.getState().resolvedTheme).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");

    useSettingsStore.getState().cycleTheme();
    expect(useSettingsStore.getState().theme).toBe("system");

    useSettingsStore.getState().cycleTheme();
    expect(useSettingsStore.getState().theme).toBe("dark");
    expect(useSettingsStore.getState().resolvedTheme).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  it("resolves system preference using matchMedia", () => {
    let changeHandler: (() => void) | null = null;
    let prefersDark = false;

    window.matchMedia = vi.fn().mockImplementation((query: string) => ({
      matches: prefersDark,
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn((event: string, handler: () => void) => {
        if (event === "change") changeHandler = handler;
      }),
      removeEventListener: vi.fn((event: string, handler: () => void) => {
        if (event === "change" && changeHandler === handler) changeHandler = null;
      }),
      dispatchEvent: vi.fn(),
    }));

    reloadSettingsStore();
    useSettingsStore.getState().setTheme("system");
    expect(useSettingsStore.getState().theme).toBe("system");
    // prefersDark is false -> resolvedTheme should be light
    expect(useSettingsStore.getState().resolvedTheme).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(document.documentElement.classList.contains("light")).toBe(true);

    // Simulate system preference change to dark
    prefersDark = true;
    if (typeof changeHandler === "function") {
      (changeHandler as () => void)();
    }

    expect(useSettingsStore.getState().resolvedTheme).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    expect(document.documentElement.classList.contains("light")).toBe(false);
  });

  it("falls back to defaults for malformed or unknown stored values", () => {
    for (const raw of [
      null,
      "",
      "not json",
      "null",
      "42",
      '"compact"',
      "[]",
      '{"density":"huge"}',
      '{"theme":"neon"}',
      '{"density":"huge","theme":"invalid"}',
    ]) {
      expect(parseSettings(raw)).toEqual(DEFAULT_SETTINGS);
    }
    expect(parseSettings('{"density":"compact","extra":1}')).toEqual({
      density: "compact",
      theme: "dark",
    });
    expect(parseSettings('{"density":"comfortable","theme":"light"}')).toEqual({
      density: "comfortable",
      theme: "light",
    });
    expect(parseSettings('{"theme":"system"}')).toEqual({
      density: "comfortable",
      theme: "system",
    });
  });

  it("loads defaults when storage holds garbage", () => {
    localStorage.setItem(SETTINGS_STORAGE_KEY, "{oops");
    reloadSettingsStore();
    expect(useSettingsStore.getState().density).toBe("comfortable");
    expect(useSettingsStore.getState().theme).toBe("dark");
    expect(useSettingsStore.getState().resolvedTheme).toBe("dark");
  });
});
