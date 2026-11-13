import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { YamlEditor } from "./YamlEditor";
import {
  getYamlEditorExtensions,
  fleetglassDarkTheme,
  fleetglassDarkHighlightStyle,
  fleetglassLightTheme,
  fleetglassLightHighlightStyle,
} from "./yamlEditorExtensions";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { useSettingsStore } from "../stores/settings";

describe("YamlEditor", () => {
  it("renders an accessible textarea in jsdom environment", () => {
    const yaml = "apiVersion: v1\nkind: ConfigMap";
    render(<YamlEditor value={yaml} />);

    const editor = screen.getByLabelText("Resource YAML");
    expect(editor).toBeInTheDocument();
    expect(editor.tagName).toBe("TEXTAREA");
    expect(editor).toHaveValue(yaml);
    expect(editor).toHaveClass("resource-drawer__yaml-editor");
  });

  it("supports custom aria-label and className", () => {
    render(
      <YamlEditor
        value="foo: bar"
        aria-label="Custom YAML Editor"
        className="custom-editor-class"
      />,
    );

    const editor = screen.getByLabelText("Custom YAML Editor");
    expect(editor).toBeInTheDocument();
    expect(editor).toHaveClass("custom-editor-class");
  });

  it("calls onChange when text changes in jsdom textarea", () => {
    const handleChange = vi.fn();
    render(<YamlEditor value="initial: true" onChange={handleChange} />);

    const editor = screen.getByLabelText("Resource YAML");
    fireEvent.change(editor, { target: { value: "initial: false" } });

    expect(handleChange).toHaveBeenCalledWith("initial: false");
  });

  it("marks textarea as readOnly when readOnly prop is true", () => {
    render(<YamlEditor value="immutable: true" readOnly={true} />);

    const editor = screen.getByLabelText("Resource YAML");
    expect(editor).toHaveProperty("readOnly", true);
  });

  it("renders CodeMirror container when forceCodeMirror is set", () => {
    const { container } = render(
      <YamlEditor
        value="apiVersion: v1\nkind: Service"
        forceCodeMirror={true}
        aria-label="CM Editor"
      />,
    );

    const div = container.querySelector(".resource-drawer__yaml-editor");
    expect(div).toBeInTheDocument();
    expect(div?.tagName).toBe("DIV");
    expect(div?.getAttribute("aria-label")).toBe("CM Editor");
  });

  it("creates valid CodeMirror EditorState with getYamlEditorExtensions for dark and light themes", () => {
    const darkExtensions = getYamlEditorExtensions({ readOnly: true, theme: "dark" });
    expect(Array.isArray(darkExtensions)).toBe(true);
    expect(darkExtensions.length).toBeGreaterThan(0);

    const darkState = EditorState.create({
      doc: "apiVersion: apps/v1\nkind: Deployment",
      extensions: darkExtensions,
    });

    expect(darkState.doc.toString()).toBe("apiVersion: apps/v1\nkind: Deployment");
    expect(darkState.readOnly).toBe(true);
    expect(darkState.facet(EditorView.cspNonce)).toBe("fleetglass");
    expect(fleetglassDarkTheme).toBeDefined();
    expect(fleetglassDarkHighlightStyle).toBeDefined();

    const lightExtensions = getYamlEditorExtensions({ readOnly: false, theme: "light" });
    const lightState = EditorState.create({
      doc: "apiVersion: v1\nkind: Pod",
      extensions: lightExtensions,
    });
    expect(lightState.doc.toString()).toBe("apiVersion: v1\nkind: Pod");
    expect(lightState.readOnly).toBe(false);
    expect(lightState.facet(EditorView.cspNonce)).toBe("fleetglass");
    expect(fleetglassLightTheme).toBeDefined();
    expect(fleetglassLightHighlightStyle).toBeDefined();
  });

  it("reconfigures theme when store theme changes with forceCodeMirror", () => {
    useSettingsStore.setState({ resolvedTheme: "dark" });
    const { rerender } = render(
      <YamlEditor
        value="kind: Namespace"
        forceCodeMirror={true}
      />,
    );

    useSettingsStore.setState({ resolvedTheme: "light" });
    rerender(
      <YamlEditor
        value="kind: Namespace"
        forceCodeMirror={true}
      />,
    );
    expect(screen.getByLabelText("Resource YAML")).toBeInTheDocument();
  });
});
