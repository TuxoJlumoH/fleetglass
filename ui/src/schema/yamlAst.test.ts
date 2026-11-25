import { describe, it, expect } from "vitest";
import { EditorState } from "@codemirror/state";
import { yaml } from "@codemirror/lang-yaml";
import { syntaxTree } from "@codemirror/language";
import {
  findDocInfo,
  findSchemaForPath,
  getParentPathAtPos,
  listDocInfos,
  mergeAllOf,
  unquote,
} from "./yamlAst";
import { deploymentSchema, widgetSchema } from "./__fixtures__/schemas";

function pathAtEnd(doc: string): string[] {
  const state = EditorState.create({ doc, extensions: [yaml()] });
  return getParentPathAtPos(state, doc.length);
}

describe("yamlAst", () => {
  it("computes parent paths for nested mappings and indented sequences", () => {
    expect(pathAtEnd("spec:\n  template:\n    spec:\n      ")).toEqual(["spec", "template", "spec"]);
    expect(pathAtEnd("spec:\n  containers:\n    - name: a\n      ")).toEqual(["spec", "containers", "[]"]);
    expect(pathAtEnd("spec:\n  containers:\n    - resources:\n        ")).toEqual([
      "spec",
      "containers",
      "[]",
      "resources",
    ]);
  });

  it("treats a key at the dash's indent as the owner of a kubectl-style sequence", () => {
    expect(pathAtEnd("spec:\n  containers:\n  - name: a\n    ")).toEqual(["spec", "containers", "[]"]);
    expect(pathAtEnd("spec:\n  containers:\n  - name: a\n    ports:\n    - containerPort: 1\n      ")).toEqual([
      "spec",
      "containers",
      "[]",
      "ports",
      "[]",
    ]);
    // A sibling key at the same indent as a mapping key is not a parent.
    expect(pathAtEnd("spec:\n  replicas: 1\n  ")).toEqual(["spec"]);
  });

  it("stops at the document separator", () => {
    expect(pathAtEnd("spec:\n  template:\n---\n")).toEqual([]);
    expect(pathAtEnd("spec:\n  template:\n---\nmetadata:\n  ")).toEqual(["metadata"]);
  });

  it("reads kind/apiVersion of the document under the cursor, quoted or not", () => {
    const doc = 'apiVersion: v1\nkind: ConfigMap\n---\napiVersion: "apps/v1"\nkind: \'Deployment\'\n';
    const state = EditorState.create({ doc, extensions: [yaml()] });
    const tree = syntaxTree(state);
    expect(findDocInfo(doc, tree, 0)).toEqual({ apiVersion: "v1", kind: "ConfigMap" });
    expect(findDocInfo(doc, tree, doc.length)).toEqual({ apiVersion: "apps/v1", kind: "Deployment" });
    expect(listDocInfos(doc)).toEqual([
      { apiVersion: "v1", kind: "ConfigMap" },
      { apiVersion: "apps/v1", kind: "Deployment" },
    ]);
    expect(listDocInfos("spec: {}\n")).toEqual([{}]);
    expect(unquote('"a"')).toBe("a");
    expect(unquote("'a")).toBe("'a");
  });

  it("navigates schemas through items, additionalProperties and allOf", () => {
    expect(findSchemaForPath(deploymentSchema, ["spec", "template", "spec", "containers", "[]"])?.required).toEqual(
      ["name"],
    );
    expect(findSchemaForPath(deploymentSchema, ["metadata", "labels", "app"])?.type).toBe("string");
    expect(findSchemaForPath(deploymentSchema, ["spec", "nope"])).toBeUndefined();
    const merged = findSchemaForPath(widgetSchema, ["spec", "merged"]);
    expect(Object.keys(merged?.properties ?? {})).toEqual(["a", "b"]);
    expect(merged?.required).toEqual(["a"]);
    expect(mergeAllOf({ type: "string" })).toEqual({ type: "string" });
  });
});
