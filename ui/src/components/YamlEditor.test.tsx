import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
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
import { CompletionContext } from "@codemirror/autocomplete";
import { useSettingsStore } from "../stores/settings";
import { validateYamlDocument } from "../schema/yamlLinter";
import { createK8sCompletionSource } from "../schema/yamlAutocomplete";
import { getK8sHoverTooltip } from "../schema/yamlHover";
import { forEachDiagnostic } from "@codemirror/lint";
import { fixtureLookup } from "../schema/__fixtures__/schemas";

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

  it("flags unknown fields when editing a deployment YAML (05-04)", () => {
    const deploymentYaml = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
spec:
  replicas: 3
  unknownSpecField: "not-valid"
  selector:
    matchLabels:
      app: nginx
  template:
    metadata:
      labels:
        app: nginx
    spec:
      containers:
        - name: nginx
          image: nginx:latest
`;
    const state = EditorState.create({
      doc: deploymentYaml,
      extensions: getYamlEditorExtensions({ schemaLookup: fixtureLookup }),
    });
    const diagnostics = validateYamlDocument(state, fixtureLookup);
    expect(diagnostics.map((d) => d.message)).toEqual(['Unknown field "unknownSpecField" in spec']);
    expect(diagnostics[0]?.severity).toBe("warning");
  });

  it("offers autocomplete for spec properties when editing a deployment YAML (05-04)", async () => {
    const deploymentYaml = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
spec:
  `;
    const state = EditorState.create({
      doc: deploymentYaml,
      extensions: getYamlEditorExtensions({ schemaLookup: fixtureLookup }),
    });
    const completionSource = createK8sCompletionSource(fixtureLookup);
    const result = await Promise.resolve(
      completionSource(new CompletionContext(state, deploymentYaml.length, true)),
    );
    const labels = (result ? result.options : []).map((o) => o.label);
    expect(labels).toEqual(
      expect.arrayContaining(["replicas", "selector", "template", "strategy", "minReadySeconds"]),
    );
  });

  it("provides hover documentation for spec properties in YamlEditor (05-04)", () => {
    const deploymentYaml = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
spec:
  replicas: 3
`;
    const view = new EditorView({
      state: EditorState.create({
        doc: deploymentYaml,
        extensions: getYamlEditorExtensions({ schemaLookup: fixtureLookup }),
      }),
    });
    const tooltip = getK8sHoverTooltip(view, deploymentYaml.indexOf("replicas") + 1, fixtureLookup);
    if (!tooltip) throw new Error("expected tooltip");
    const dom = tooltip.create(view).dom;
    expect(dom.querySelector(".k8s-hover-title")?.textContent).toBe("replicas");
    expect(dom.querySelector(".k8s-hover-type")?.textContent).toBe("integer");
    expect(dom.querySelector(".k8s-hover-desc")?.textContent).toContain("Number of desired pods");
  });

  it("lints only once a schema lookup is passed, and stops when it is removed", async () => {
    const doc = "apiVersion: apps/v1\nkind: Deployment\nspec:\n  bogus: 1\n  selector: {}\n  template: {}\n";
    const { container, rerender } = render(<YamlEditor value={doc} forceCodeMirror={true} />);
    const view = EditorView.findFromDOM(container.querySelector(".cm-editor") as HTMLElement);
    if (!view) throw new Error("expected an editor view");
    const count = () => {
      let n = 0;
      forEachDiagnostic(view.state, () => {
        n += 1;
      });
      return n;
    };
    await new Promise((r) => setTimeout(r, 400));
    expect(count()).toBe(0);

    rerender(<YamlEditor value={doc} forceCodeMirror={true} schemaLookup={fixtureLookup} />);
    await waitFor(
      () => {
        expect(count()).toBe(1);
      },
      { timeout: 2000 },
    );

    rerender(<YamlEditor value={doc} forceCodeMirror={true} />);
    await waitFor(
      () => {
        expect(count()).toBe(0);
      },
      { timeout: 2000 },
    );
  });
});
