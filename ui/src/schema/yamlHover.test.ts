import { describe, it, expect } from "vitest";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { yaml } from "@codemirror/lang-yaml";
import { getK8sHoverTooltip } from "./yamlHover";
import { fixtureLookup } from "./__fixtures__/schemas";

function viewOf(doc: string): EditorView {
  return new EditorView({ state: EditorState.create({ doc, extensions: [yaml()] }) });
}

function hover(doc: string, needle: string, nth = 0) {
  const view = viewOf(doc);
  let idx = -1;
  for (let i = 0; i <= nth; i++) idx = doc.indexOf(needle, idx + 1);
  const tooltip = getK8sHoverTooltip(view, idx + 1, fixtureLookup);
  return { tooltip, idx, dom: tooltip?.create(view).dom };
}

const DEPLOYMENT = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
spec:
  replicas: 3
  selector:
    matchLabels:
      app: nginx
  strategy:
    rollingUpdate:
      maxSurge: 1
  template:
    spec:
      containers:
      - name: a
        imagePullPolicy: Always
`;

describe("yamlHover", () => {
  it("shows type, description and range for a field", () => {
    const { tooltip, idx, dom } = hover(DEPLOYMENT, "replicas");
    expect(tooltip?.pos).toBe(idx);
    expect(tooltip?.end).toBe(idx + "replicas".length);
    expect(dom?.className).toBe("k8s-hover-tooltip");
    expect(dom?.querySelector(".k8s-hover-title")?.textContent).toBe("replicas");
    expect(dom?.querySelector(".k8s-hover-type")?.textContent).toBe("integer");
    expect(dom?.querySelector(".k8s-hover-desc")?.textContent).toContain("Number of desired pods");
  });

  it("shows required, enum and int-or-string", () => {
    expect(hover(DEPLOYMENT, "selector").dom?.querySelector(".k8s-hover-badge.required")?.textContent).toBe(
      "required",
    );
    expect(hover(DEPLOYMENT, "maxSurge").dom?.querySelector(".k8s-hover-type")?.textContent).toBe(
      "int-or-string",
    );
    // kubectl-style sequence: `- name` sits at the indent of `containers:`.
    const pull = hover(DEPLOYMENT, "imagePullPolicy").dom;
    expect(pull?.querySelector(".k8s-hover-enum")?.textContent).toBe(
      'Allowed values: "Always", "IfNotPresent", "Never"',
    );
  });

  it("shows defaults of CRD fields", () => {
    const doc = "apiVersion: example.fleetglass.dev/v1\nkind: Widget\nspec:\n  mode: fast\n";
    expect(hover(doc, "mode").dom?.querySelector(".k8s-hover-badge.default")?.textContent).toBe(
      "default: safe",
    );
  });

  it("returns null on whitespace, unknown fields and kinds without a schema", () => {
    const doc = "apiVersion: apps/v1\nkind: Deployment\nspec:\n  nonExistentField: 123\n";
    expect(getK8sHoverTooltip(viewOf(doc), doc.length, fixtureLookup)).toBeNull();
    expect(hover(doc, "nonExistentField").tooltip).toBeNull();
    expect(hover("apiVersion: v1\nkind: Nope\nspec:\n  replicas: 1\n", "replicas").tooltip).toBeNull();
  });

  it("uses the kind of the document under the cursor", () => {
    const doc = `apiVersion: v1\nkind: ConfigMap\ndata:\n  a: b\n---\n${DEPLOYMENT}`;
    expect(hover(doc, "replicas").dom?.querySelector(".k8s-hover-type")?.textContent).toBe("integer");
    expect(hover(doc, "data").dom?.querySelector(".k8s-hover-type")?.textContent).toBe("object");
  });
});
