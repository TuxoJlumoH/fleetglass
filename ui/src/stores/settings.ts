import { create } from "zustand";

/** Table row density: 34 px rows (mockup default) or 28 px "Compact" rows. */
export type Density = "comfortable" | "compact";

/** Theme preference: explicit dark/light, or follow system prefers-color-scheme. */
export type ThemePreference = "dark" | "light" | "system";

/** Resolved active theme applied to the document and editor. */
export type ResolvedTheme = "dark" | "light";

export interface Settings {
  density: Density;
  theme: ThemePreference;
}

export const SETTINGS_STORAGE_KEY = "fleetglass.settings";

export const DEFAULT_SETTINGS: Settings = {
  density: "comfortable",
  theme: "dark",
};

/** Row heights in px; must match `--row-h` / `--row-h-compact` in styles.css. */
export const ROW_HEIGHTS: Record<Density, number> = { comfortable: 34, compact: 28 };

/** Parses stored settings; anything missing, unknown or malformed falls back to the default. */
export function parseSettings(raw: string | null): Settings {
  if (raw === null) return { ...DEFAULT_SETTINGS };
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
  if (typeof value !== "object" || value === null) return { ...DEFAULT_SETTINGS };
  const obj = value as Record<string, unknown>;
  const density = obj.density;
  const theme = obj.theme;
  return {
    density: density === "compact" || density === "comfortable" ? density : DEFAULT_SETTINGS.density,
    theme: theme === "dark" || theme === "light" || theme === "system" ? theme : DEFAULT_SETTINGS.theme,
  };
}

/** Resolves the effective "dark" or "light" theme from preference and system environment. */
export function resolveTheme(theme: ThemePreference): ResolvedTheme {
  if (theme === "dark" || theme === "light") {
    return theme;
  }
  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    try {
      return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
    } catch {
      return "dark";
    }
  }
  return "dark";
}

/** Applies `data-theme` attribute and `.light` class to document root and body. */
export function applyThemeToDom(resolved: ResolvedTheme): void {
  if (typeof document === "undefined") return;
  document.documentElement.setAttribute("data-theme", resolved);
  document.documentElement.classList.toggle("light", resolved === "light");
  document.body.setAttribute("data-theme", resolved);
  document.body.classList.toggle("light", resolved === "light");
}

function storage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    // Access can throw (e.g. storage disabled); settings then live for the session only.
    return null;
  }
}

function load(): Settings {
  try {
    return parseSettings(storage()?.getItem(SETTINGS_STORAGE_KEY) ?? null);
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

function save(settings: Settings): void {
  try {
    storage()?.setItem(SETTINGS_STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // Quota or disabled storage: keep the in-memory value.
  }
}

export interface SettingsState extends Settings {
  resolvedTheme: ResolvedTheme;
  setDensity: (density: Density) => void;
  toggleDensity: () => void;
  setTheme: (theme: ThemePreference) => void;
  cycleTheme: () => void;
}

function subscribeToSystemTheme(onChange: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  try {
    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => {
      onChange();
    };
    if (typeof mql.addEventListener === "function") {
      mql.addEventListener("change", handler);
      return () => {
        mql.removeEventListener("change", handler);
      };
    }
    const legacyMql = mql as unknown as {
      addListener?: (cb: () => void) => void;
      removeListener?: (cb: () => void) => void;
    };
    if (typeof legacyMql.addListener === "function") {
      legacyMql.addListener(handler);
      return () => {
        legacyMql.removeListener?.(handler);
      };
    }
  } catch {
    // In partially mocked test environments, matchMedia might fail silently.
  }
  return () => {};
}

let cleanupListener: (() => void) | null = null;

export function initThemeListener(): void {
  if (cleanupListener) {
    cleanupListener();
    cleanupListener = null;
  }
  cleanupListener = subscribeToSystemTheme(() => {
    const { theme } = useSettingsStore.getState();
    if (theme === "system") {
      const resolved = resolveTheme("system");
      applyThemeToDom(resolved);
      useSettingsStore.setState({ resolvedTheme: resolved });
    }
  });
}

const initialSettings = load();
const initialResolvedTheme = resolveTheme(initialSettings.theme);
applyThemeToDom(initialResolvedTheme);
initThemeListener();

export const useSettingsStore = create<SettingsState>((set, get) => ({
  ...initialSettings,
  resolvedTheme: initialResolvedTheme,
  setDensity: (density) => {
    set({ density });
    save({ density, theme: get().theme });
  },
  toggleDensity: () => {
    get().setDensity(get().density === "compact" ? "comfortable" : "compact");
  },
  setTheme: (theme) => {
    const resolvedTheme = resolveTheme(theme);
    applyThemeToDom(resolvedTheme);
    set({ theme, resolvedTheme });
    save({ density: get().density, theme });
  },
  cycleTheme: () => {
    const current = get().theme;
    const next: ThemePreference =
      current === "dark" ? "light" : current === "light" ? "system" : "dark";
    get().setTheme(next);
  },
}));

/** Test helper: re-reads storage into the store and updates DOM/listeners. */
export function reloadSettingsStore(): void {
  const settings = load();
  const resolvedTheme = resolveTheme(settings.theme);
  applyThemeToDom(resolvedTheme);
  initThemeListener();
  useSettingsStore.setState({ ...settings, resolvedTheme });
}
