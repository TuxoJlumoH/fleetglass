import { type ITheme } from "@xterm/xterm";

/** ANSI colours readable on a dark terminal background. */
const ANSI_ON_DARK = {
  black: "#111317",
  red: "#f85149",
  green: "#3fb950",
  yellow: "#d29922",
  blue: "#4c8dff",
  magenta: "#bc8cff",
  cyan: "#39c5cf",
  white: "#d7dae0",
  brightBlack: "#6b7280",
  brightRed: "#ff7b72",
  brightGreen: "#56d364",
  brightYellow: "#e3b341",
  brightBlue: "#79c0ff",
  brightMagenta: "#d2a8ff",
  brightCyan: "#56d4dd",
  brightWhite: "#ffffff",
} as const;

/** ANSI colours readable on a light terminal background. */
const ANSI_ON_LIGHT = {
  black: "#1f2328",
  red: "#cf222e",
  green: "#1a7f37",
  yellow: "#9a6700",
  blue: "#2f6fec",
  magenta: "#8250df",
  cyan: "#1b7c83",
  white: "#6e7781",
  brightBlack: "#57606a",
  brightRed: "#a40e26",
  brightGreen: "#116329",
  brightYellow: "#7d4e00",
  brightBlue: "#0550ae",
  brightMagenta: "#6639ba",
  brightCyan: "#136061",
  brightWhite: "#8c959f",
} as const;

const FALLBACK = {
  background: "#0b0d10",
  foreground: "#d7dae0",
  accent: "#4c8dff",
  fontFamily: "monospace",
  fontSize: 12,
};

function cssVar(style: CSSStyleDeclaration, name: string): string {
  return style.getPropertyValue(name).trim();
}

/** Parses `#rgb` / `#rrggbb`; returns null for anything else. */
function parseHex(color: string): [number, number, number] | null {
  const digits = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(color)?.[1];
  if (digits === undefined) return null;
  const hex = digits.length === 3 ? digits.replace(/./g, (c) => c + c) : digits;
  return [
    parseInt(hex.slice(0, 2), 16),
    parseInt(hex.slice(2, 4), 16),
    parseInt(hex.slice(4, 6), 16),
  ];
}

function isLightColor(color: string): boolean {
  const rgb = parseHex(color);
  if (!rgb) return false;
  const [r, g, b] = rgb;
  return (0.299 * r + 0.587 * g + 0.114 * b) / 255 > 0.5;
}

function withAlpha(color: string, alpha: number): string {
  const rgb = parseHex(color);
  return rgb ? `rgba(${[...rgb, alpha].map(String).join(", ")})` : color;
}

/**
 * Builds the xterm theme from the active palette tokens (`--bg-terminal`, `--fg-terminal`,
 * `--accent`) on `root` (`05-02`). The ANSI set follows the terminal background's lightness,
 * so it stays readable whichever palette keeps the terminal dark or light.
 */
export function resolveTerminalTheme(root: Element = document.documentElement): ITheme {
  const style = getComputedStyle(root);
  const background = cssVar(style, "--bg-terminal") || FALLBACK.background;
  const foreground = cssVar(style, "--fg-terminal") || FALLBACK.foreground;
  const accent = cssVar(style, "--accent") || FALLBACK.accent;
  return {
    background,
    foreground,
    cursor: accent,
    cursorAccent: background,
    selectionBackground: withAlpha(accent, 0.35),
    ...(isLightColor(background) ? ANSI_ON_LIGHT : ANSI_ON_DARK),
  };
}

/** Terminal font from the `--font-mono` and `--fs-s` tokens. */
export function resolveTerminalFont(root: Element = document.documentElement): {
  fontFamily: string;
  fontSize: number;
} {
  const style = getComputedStyle(root);
  const size = Number.parseFloat(cssVar(style, "--fs-s"));
  return {
    fontFamily: cssVar(style, "--font-mono") || FALLBACK.fontFamily,
    fontSize: Number.isFinite(size) && size > 0 ? size : FALLBACK.fontSize,
  };
}
