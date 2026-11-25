import { describe, it, expect } from "vitest";
import { EditorState } from "@codemirror/state";
import { yaml } from "@codemirror/lang-yaml";
import { validateYamlDocument } from "./yamlLinter";
import { fixtureLookup } from "./__fixtures__/schemas";
import type { SchemaLookup } from "./types";

function lint(doc: string, lookup: SchemaLookup = fixtureLookup) {
  const state = EditorState.create({ doc, extensions: [yaml()] });
  return validateYamlDocument(state, lookup);
}

function messages(doc: string, lookup?: SchemaLookup): string[] {
  return lint(doc, lookup).map((d) => d.message);
}

const VALID_DEPLOYMENT = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: nginx
  labels:
    app: nginx
    anything-goes: "yes"
spec:
  replicas: 3
  selector:
    matchLabels:
      app: nginx
  strategy:
    rollingUpdate:
      maxSurge: 25%
      maxUnavailable: 1
  template:
    metadata:
      labels:
        app: nginx
    spec:
      containers:
        - name: nginx
          image: nginx:1.25.0
          imagePullPolicy: IfNotPresent
          ports:
            - containerPort: 80
`;

describe("yamlLinter", () => {
  it("returns no diagnostics for a valid Deployment", () => {
    expect(messages(VALID_DEPLOYMENT)).toEqual([]);
  });

  it("returns no diagnostics without a schema, for any content", () => {
    const none: SchemaLookup = () => undefined;
    expect(messages(VALID_DEPLOYMENT.replace("replicas: 3", "replicas: lots\n  bogus: 1"), none)).toEqual(
      [],
    );
    expect(messages("apiVersion: v9\nkind: Unknown\nfoo: bar\n")).toEqual([]);
    expect(messages("")).toEqual([]);
    expect(messages("# only a comment\n")).toEqual([]);
  });

  it("flags unknown fields with the key's exact range", () => {
    const doc = VALID_DEPLOYMENT.replace("  replicas: 3\n", "  replicas: 3\n  unknownSpecField: x\n");
    const diags = lint(doc);
    expect(diags).toHaveLength(1);
    const d = diags[0];
    expect(d?.severity).toBe("warning");
    expect(d?.message).toBe('Unknown field "unknownSpecField" in spec');
    expect(doc.slice(d?.from, d?.to)).toBe("unknownSpecField");
  });

  it("flags type mismatches, including flow collections", () => {
    const doc = VALID_DEPLOYMENT.replace("replicas: 3", "replicas: three")
      .replace("  selector:\n    matchLabels:\n      app: nginx", "  selector: [a]\n  paused: \"true\"")
      .replace("containerPort: 80", "containerPort: 8.5");
    expect(messages(doc)).toEqual([
      'Invalid type for "replicas": expected integer, got string',
      'Invalid type for "selector": expected object, got sequence',
      'Invalid type for "paused": expected boolean, got string',
      'Invalid type for "containerPort": expected integer, got number',
    ]);
  });

  it("reports missing required fields, also for an empty object key", () => {
    const doc = `apiVersion: apps/v1
kind: Deployment
metadata:
  name: x
spec:
  template:
    spec:
      containers:
        - image: nginx
`;
    expect(messages(doc)).toEqual([
      'Missing required field "name" in containers[]',
      'Missing required field "selector" in spec',
    ]);
    expect(messages("apiVersion: apps/v1\nkind: Deployment\nspec:\n")).toEqual([
      'Missing required field "selector" in spec',
      'Missing required field "template" in spec',
    ]);
  });

  it("checks enums case-sensitively", () => {
    const doc = VALID_DEPLOYMENT.replace("IfNotPresent", "ifnotpresent");
    expect(messages(doc)).toEqual([
      'Value "ifnotpresent" is not one of: "Always", "IfNotPresent", "Never"',
    ]);
  });

  it("never lints status and metadata.managedFields", () => {
    const doc = `${VALID_DEPLOYMENT}status:
  replicas: lots
  whatever: true
`.replace(
      "  name: nginx\n",
      "  name: nginx\n  managedFields:\n    - manager: x\n      bogus: 1\n",
    );
    expect(messages(doc)).toEqual([]);
    expect(messages("apiVersion: example.fleetglass.dev/v1\nkind: Widget\nspec:\n  size: 1\nstatus:\n  ready: maybe\n")).toEqual([]);
  });

  it("does not flag Object.prototype names as known fields", () => {
    const doc = VALID_DEPLOYMENT.replace("  replicas: 3\n", "  replicas: 3\n  toString: x\n");
    expect(messages(doc)).toEqual(['Unknown field "toString" in spec']);
  });

  describe("block sequences (reviewer: '-' siblings)", () => {
    it("validates each item exactly once and ignores empty items", () => {
      const doc = `apiVersion: apps/v1
kind: Deployment
spec:
  selector: {}
  template:
    spec:
      containers:
        - name: a
          bogus: 1
        -
        - name: c
          ports:
            - containerPort: x
`;
      expect(messages(doc)).toEqual([
        'Unknown field "bogus" in containers[]',
        'Invalid type for "containerPort": expected integer, got string',
      ]);
    });

    it("validates kubectl-style sequences at the key's indent", () => {
      const doc = `apiVersion: apps/v1
kind: Deployment
spec:
  selector: {}
  template:
    spec:
      containers:
      - name: a
        imagePullPolicy: Sometimes
`;
      expect(messages(doc)).toEqual([
        'Value "Sometimes" is not one of: "Always", "IfNotPresent", "Never"',
      ]);
    });
  });

  describe("multi-document streams", () => {
    it("validates each document against its own kind", () => {
      const doc = `apiVersion: v1
kind: ConfigMap
metadata:
  name: c
data:
  key: value
---
apiVersion: apps/v1
kind: "Deployment"
spec:
  replicas: x
  selector: {}
  template: {}
---
apiVersion: v1
kind: Secret
spec:
  replicas: not-checked
`;
      expect(messages(doc)).toEqual(['Invalid type for "replicas": expected integer, got string']);
    });

    it("does not borrow the kind of an earlier document", () => {
      const doc = `apiVersion: apps/v1
kind: Deployment
spec:
  selector: {}
  template: {}
---
spec:
  replicas: x
  nonsense: 1
`;
      expect(messages(doc)).toEqual([]);
    });

    it("handles a leading --- separator", () => {
      const doc = `---\napiVersion: apps/v1\nkind: Deployment\nspec:\n  bogus: 1\n  selector: {}\n  template: {}\n`;
      expect(messages(doc)).toEqual(['Unknown field "bogus" in spec']);
    });
  });

  describe("schema keywords", () => {
    const widget = (spec: string) =>
      `apiVersion: example.fleetglass.dev/v1\nkind: Widget\nspec:\n${spec}`;

    it("accepts int-or-string values and rejects other types", () => {
      expect(messages(widget("  size: 3\n"))).toEqual([]);
      expect(messages(widget("  size: 50%\n"))).toEqual([]);
      expect(messages(widget("  size: true\n"))).toEqual([
        'Invalid type for "size": expected integer or string, got boolean',
      ]);
    });

    it("respects x-kubernetes-preserve-unknown-fields", () => {
      expect(messages(widget("  size: 1\n  config:\n    anything:\n      nested: [1]\n"))).toEqual([]);
    });

    it("checks additionalProperties even when properties is empty", () => {
      expect(messages(widget("  size: 1\n  tags:\n    a: b\n    c:\n      d: e\n"))).toEqual([
        'Invalid type for "c": expected string, got mapping',
      ]);
      expect(messages(widget("  size: 1\n  closed:\n    x: 1\n"))).toEqual([
        'Unknown field "x" in closed',
      ]);
    });

    it("merges allOf branches", () => {
      expect(messages(widget("  size: 1\n  merged:\n    a: x\n    b: 2\n"))).toEqual([]);
      expect(messages(widget("  size: 1\n  merged:\n    b: x\n    c: 1\n"))).toEqual([
        'Invalid type for "b": expected integer, got string',
        'Unknown field "c" in merged',
        'Missing required field "a" in merged',
      ]);
    });

    it("still validates properties and required next to oneOf", () => {
      expect(messages(widget("  size: 1\n  choice:\n    y: a\n    z: 1\n"))).toEqual([
        'Unknown field "z" in choice',
        'Missing required field "x" in choice',
      ]);
    });

    it("treats unknown combinator branches as permissive", () => {
      expect(messages(widget("  size: 1\n  opaque:\n    any: thing\n"))).toEqual([]);
      expect(messages(widget("  size: 1\n  opaque: text\n"))).toEqual([]);
    });

    it("does not check tagged, aliased or YAML 1.1 boolean values", () => {
      const doc = VALID_DEPLOYMENT.replace("replicas: 3", "replicas: !!str 3").replace(
        "  selector:",
        "  paused: yes\n  selector:",
      );
      expect(messages(doc)).toEqual([]);
    });
  });
});
