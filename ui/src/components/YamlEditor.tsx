import { useEffect, useRef, type JSX } from "react";
import { EditorState, Compartment } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { getThemeExtension, getYamlEditorExtensions } from "./yamlEditorExtensions";
import { useSettingsStore } from "../stores/settings";

export interface YamlEditorProps {
  value: string;
  onChange?: (value: string) => void;
  readOnly?: boolean;
  "aria-label"?: string;
  className?: string;
  forceCodeMirror?: boolean;
  theme?: "dark" | "light";
}

export function YamlEditor({
  value,
  onChange,
  readOnly = false,
  "aria-label": ariaLabel = "Resource YAML",
  className = "resource-drawer__yaml-editor",
  forceCodeMirror = false,
  theme: themeProp,
}: YamlEditorProps): JSX.Element {
  const isJsdom =
    typeof navigator !== "undefined" &&
    navigator.userAgent.includes("jsdom");

  const resolvedThemeFromStore = useSettingsStore((s) => s.resolvedTheme);
  const activeTheme = themeProp ?? resolvedThemeFromStore;

  const containerRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const onChangeRef = useRef(onChange);

  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);

  const readOnlyCompartmentRef = useRef(new Compartment());
  const themeCompartmentRef = useRef(new Compartment());
  const initialReadOnlyRef = useRef(readOnly);
  const initialThemeRef = useRef(activeTheme);

  useEffect(() => {
    if (isJsdom && !forceCodeMirror) return;
    if (!containerRef.current) return;

    const state = EditorState.create({
      doc: value,
      extensions: [
        ...getYamlEditorExtensions({
          readOnlyCompartment: readOnlyCompartmentRef.current,
          readOnly: initialReadOnlyRef.current,
          themeCompartment: themeCompartmentRef.current,
          theme: initialThemeRef.current,
        }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            const nextVal = update.state.doc.toString();
            onChangeRef.current?.(nextVal);
          }
        }),
      ],
    });

    const view = new EditorView({
      state,
      parent: containerRef.current,
    });

    viewRef.current = view;

    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- value is used only for initial document; dynamic updates handled in separate effect
  }, [forceCodeMirror, isJsdom]);

  useEffect(() => {
    if (isJsdom && !forceCodeMirror) return;
    const view = viewRef.current;
    if (!view) return;
    const currentDoc = view.state.doc.toString();
    if (currentDoc !== value) {
      view.dispatch({
        changes: {
          from: 0,
          to: currentDoc.length,
          insert: value,
        },
      });
    }
  }, [value, isJsdom, forceCodeMirror]);

  useEffect(() => {
    if (isJsdom && !forceCodeMirror) return;
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({
      effects: readOnlyCompartmentRef.current.reconfigure(
        EditorState.readOnly.of(readOnly),
      ),
    });
  }, [readOnly, isJsdom, forceCodeMirror]);

  useEffect(() => {
    if (isJsdom && !forceCodeMirror) return;
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({
      effects: themeCompartmentRef.current.reconfigure(
        getThemeExtension(activeTheme),
      ),
    });
  }, [activeTheme, isJsdom, forceCodeMirror]);

  if (isJsdom && !forceCodeMirror) {
    return (
      <textarea
        aria-label={ariaLabel}
        className={className}
        value={value}
        readOnly={readOnly}
        onChange={(e) => onChange?.(e.target.value)}
        spellCheck={false}
      />
    );
  }

  return (
    <div
      ref={containerRef}
      aria-label={ariaLabel}
      className={className}
      tabIndex={-1}
    />
  );
}
