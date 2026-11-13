import {
  EditorState,
  Compartment,
  type Extension,
} from "@codemirror/state";
import {
  EditorView,
  lineNumbers,
  highlightActiveLineGutter,
  highlightSpecialChars,
  drawSelection,
  dropCursor,
  rectangularSelection,
  crosshairCursor,
  highlightActiveLine,
  keymap,
} from "@codemirror/view";
import {
  foldGutter,
  foldKeymap,
  indentOnInput,
  syntaxHighlighting,
  HighlightStyle,
  bracketMatching,
} from "@codemirror/language";
import { tags } from "@lezer/highlight";
import {
  highlightSelectionMatches,
  searchKeymap,
} from "@codemirror/search";
import { yaml } from "@codemirror/lang-yaml";

export interface ExtensionOptions {
  readOnlyCompartment?: Compartment;
  readOnly?: boolean;
  themeCompartment?: Compartment;
  theme?: "dark" | "light";
}

export const fleetglassDarkTheme = EditorView.theme({
  "&": {
    color: "var(--fg, #d7dae0)",
    backgroundColor: "var(--bg-card, var(--bg-app, var(--bg, #111317)))",
    height: "100%",
    fontSize: "12px",
    fontFamily: "var(--font-mono, monospace)",
  },
  ".cm-content": {
    caretColor: "var(--accent, #4c8dff)",
    fontFamily: "var(--font-mono, monospace)",
  },
  ".cm-cursor, .cm-dropCursor": {
    borderLeftColor: "var(--accent, #4c8dff)",
    borderLeftWidth: "2px",
  },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--accent-subtle, rgba(76, 141, 255, 0.25)) !important",
  },
  ".cm-activeLine": {
    backgroundColor: "var(--bg-hover, rgba(255, 255, 255, 0.04))",
  },
  ".cm-gutters": {
    backgroundColor: "var(--bg-elev, #181b21)",
    color: "var(--fg-muted, #8a91a0)",
    borderRight: "1px solid var(--border, #2a2f3a)",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "var(--bg-hover, #222630)",
    color: "var(--fg, #d7dae0)",
  },
  ".cm-lineNumbers .cm-gutterElement": {
    padding: "0 8px 0 6px",
    minWidth: "28px",
    textAlign: "right",
  },
  ".cm-foldPlaceholder": {
    backgroundColor: "var(--bg-elev, #181b21)",
    border: "1px solid var(--border, #2a2f3a)",
    color: "var(--fg-muted, #8a91a0)",
    borderRadius: "2px",
    padding: "0 4px",
    margin: "0 2px",
  },
  ".cm-tooltip": {
    border: "1px solid var(--border, #2a2f3a)",
    backgroundColor: "var(--bg-elev, #181b21)",
    color: "var(--fg, #d7dae0)",
  },
  ".cm-panels": {
    backgroundColor: "var(--bg-elev, #181b21)",
    color: "var(--fg, #d7dae0)",
    borderTop: "1px solid var(--border, #2a2f3a)",
  },
  ".cm-searchMatch": {
    backgroundColor: "rgba(210, 153, 34, 0.3)",
    outline: "1px solid var(--warn, #d29922)",
  },
  ".cm-searchMatch.cm-searchMatch-selected": {
    backgroundColor: "rgba(76, 141, 255, 0.4)",
  },
}, { dark: true });

export const fleetglassLightTheme = EditorView.theme({
  "&": {
    color: "var(--fg, #1f2328)",
    backgroundColor: "var(--bg, #ffffff)",
    height: "100%",
    fontSize: "12px",
    fontFamily: "var(--font-mono, monospace)",
  },
  ".cm-content": {
    caretColor: "var(--accent, #2f6fec)",
    fontFamily: "var(--font-mono, monospace)",
  },
  ".cm-cursor, .cm-dropCursor": {
    borderLeftColor: "var(--accent, #2f6fec)",
    borderLeftWidth: "2px",
  },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--bg-sel, #e8f0fe) !important",
  },
  ".cm-activeLine": {
    backgroundColor: "var(--bg-hover, #eceff3)",
  },
  ".cm-gutters": {
    backgroundColor: "var(--bg-elev, #f6f7f9)",
    color: "var(--fg-muted, #5c6370)",
    borderRight: "1px solid var(--border, #e3e6eb)",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "var(--bg-hover, #eceff3)",
    color: "var(--fg, #1f2328)",
  },
  ".cm-lineNumbers .cm-gutterElement": {
    padding: "0 8px 0 6px",
    minWidth: "28px",
    textAlign: "right",
  },
  ".cm-foldPlaceholder": {
    backgroundColor: "var(--bg-elev, #f6f7f9)",
    border: "1px solid var(--border, #e3e6eb)",
    color: "var(--fg-muted, #5c6370)",
    borderRadius: "2px",
    padding: "0 4px",
    margin: "0 2px",
  },
  ".cm-tooltip": {
    border: "1px solid var(--border, #e3e6eb)",
    backgroundColor: "var(--bg-elev, #f6f7f9)",
    color: "var(--fg, #1f2328)",
  },
  ".cm-panels": {
    backgroundColor: "var(--bg-elev, #f6f7f9)",
    color: "var(--fg, #1f2328)",
    borderTop: "1px solid var(--border, #e3e6eb)",
  },
  ".cm-searchMatch": {
    backgroundColor: "rgba(154, 103, 0, 0.2)",
    outline: "1px solid var(--warn, #9a6700)",
  },
  ".cm-searchMatch.cm-searchMatch-selected": {
    backgroundColor: "rgba(47, 111, 236, 0.3)",
  },
}, { dark: false });

export const fleetglassDarkHighlightStyle = HighlightStyle.define([
  { tag: tags.propertyName, color: "#7ee787" },
  { tag: tags.string, color: "#a5d6ff" },
  { tag: [tags.number, tags.bool], color: "#79c0ff" },
  { tag: tags.keyword, color: "#ff7b72" },
  { tag: tags.comment, color: "#8b949e" },
  { tag: [tags.punctuation, tags.separator], color: "#c9d1d9" },
  { tag: tags.bracket, color: "#c9d1d9" },
]);

export const fleetglassLightHighlightStyle = HighlightStyle.define([
  { tag: tags.propertyName, color: "#0550ae" },
  { tag: tags.string, color: "#0a3069" },
  { tag: [tags.number, tags.bool], color: "#0550ae" },
  { tag: tags.keyword, color: "#cf222e" },
  { tag: tags.comment, color: "#6e7781" },
  { tag: [tags.punctuation, tags.separator], color: "#24292f" },
  { tag: tags.bracket, color: "#24292f" },
]);

export function getThemeExtension(theme: "dark" | "light" = "dark"): Extension {
  return theme === "light"
    ? [fleetglassLightTheme, syntaxHighlighting(fleetglassLightHighlightStyle, { fallback: true })]
    : [fleetglassDarkTheme, syntaxHighlighting(fleetglassDarkHighlightStyle, { fallback: true })];
}

export function getYamlEditorExtensions(options: ExtensionOptions = {}): Extension[] {
  const {
    readOnlyCompartment,
    readOnly = false,
    themeCompartment,
    theme = "dark",
  } = options;

  const readOnlyExtension = readOnlyCompartment
    ? readOnlyCompartment.of(EditorState.readOnly.of(readOnly))
    : EditorState.readOnly.of(readOnly);

  const themeExtension = themeCompartment
    ? themeCompartment.of(getThemeExtension(theme))
    : getThemeExtension(theme);

  return [
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    foldGutter(),
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    indentOnInput(),
    bracketMatching(),
    rectangularSelection(),
    crosshairCursor(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    keymap.of([...searchKeymap, ...foldKeymap]),
    yaml(),
    EditorView.lineWrapping,
    EditorView.cspNonce.of("fleetglass"),
    readOnlyExtension,
    themeExtension,
  ];
}
