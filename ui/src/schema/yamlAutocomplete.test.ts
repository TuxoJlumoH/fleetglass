import { describe, it, expect } from "vitest";
import { EditorState } from "@codemirror/state";
import { CompletionContext, type Completion, type CompletionResult } from "@codemirror/autocomplete";
import { yaml } from "@codemirror/lang-yaml";
import { createK8sCompletionSource } from "./yamlAutocomplete";
import { fixtureLookup } from "./__fixtures__/schemas";

async function complete(doc: string, pos = doc.length, explicit = true): Promise<CompletionResult | null> {
  const state = EditorState.create({ doc, extensions: [yaml()] });
  const source = createK8sCompletionSource(fixtureLookup);
  return Promise.resolve(source(new CompletionContext(state, pos, explicit)));
}

async function requireCompletions(doc: string, pos = doc.length, explicit = true): Promise<CompletionResult> {
  const res = await complete(doc, pos, explicit);
  if (!res) throw new Error("expected completions");
  return res;
}

function labels(res: CompletionResult): string[] {
  return res.options.map((o) => o.label);
}

function option(res: CompletionResult, label: string): Completion | undefined {
  return res.options.find((o) => o.label === label);
}

const HEAD = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: nginx\n";

describe("yamlAutocomplete", () => {
  it("offers spec properties of a Deployment with type, required and docs", async () => {
    const res = await requireCompletions(`${HEAD}spec:\n  `);
    expect(labels(res)).toEqual(
      expect.arrayContaining(["replicas", "selector", "template", "strategy", "minReadySeconds"]),
    );
    expect(option(res, "replicas")?.detail).toBe("integer");
    expect(option(res, "replicas")?.info).toContain("Number of desired pods");
    expect(option(res, "selector")?.detail).toBe("object (required)");
    expect(option(res, "selector")?.boost).toBe(3);
  });

  it("offers nothing implicitly on an empty prefix, and filters by typed prefix", async () => {
    expect(await complete(`${HEAD}spec:\n  `, undefined, false)).toBeNull();
    const res = await requireCompletions(`${HEAD}spec:\n  rep`, undefined, false);
    expect(res.from).toBe(`${HEAD}spec:\n  `.length);
    expect(labels(res)).toContain("replicas");
  });

  it("offers container properties inside a sequence item, in both indent styles", async () => {
    const nested = `${HEAD}spec:\n  template:\n    spec:\n      containers:\n        - name: a\n          `;
    expect(labels(await requireCompletions(nested))).toEqual(
      expect.arrayContaining(["image", "imagePullPolicy", "ports"]),
    );
    const kubectl = `${HEAD}spec:\n  template:\n    spec:\n      containers:\n      - name: a\n        `;
    expect(labels(await requireCompletions(kubectl))).toEqual(
      expect.arrayContaining(["image", "imagePullPolicy", "ports"]),
    );
  });

  it("offers enum and boolean values", async () => {
    const enumDoc = `${HEAD}spec:\n  template:\n    spec:\n      restartPolicy: `;
    expect(labels(await requireCompletions(enumDoc))).toEqual(["Always", "Never", "OnFailure"]);
    expect(labels(await requireCompletions(`${HEAD}spec:\n  paused: `))).toEqual(["true", "false"]);
  });

  it("keeps the current indentation for nested object and array completions", async () => {
    const res = await requireCompletions(`${HEAD}spec:\n  template:\n    spec:\n      `);
    expect(option(res, "containers")?.apply).toBe("containers:\n        - ");
    const top = await requireCompletions(`${HEAD}spec:\n  `);
    expect(option(top, "template")?.apply).toBe("template:\n    ");
  });

  it("does not offer status, and offers no spec for a ConfigMap", async () => {
    expect(labels(await requireCompletions(HEAD))).not.toContain("status");
    const res = await requireCompletions("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n");
    expect(labels(res)).toContain("data");
    expect(labels(res)).not.toContain("spec");
  });

  it("offers starter keys when the document has no schema", async () => {
    const res = await requireCompletions("");
    expect(labels(res)).toEqual(["apiVersion", "kind", "metadata"]);
    expect(await complete("apiVersion: v9\nkind: Nope\nspec:\n  ")).toBeNull();
  });

  it("uses the document under the cursor in a multi-document stream", async () => {
    const doc = `apiVersion: v1\nkind: ConfigMap\ndata:\n  a: b\n---\n${HEAD}spec:\n  `;
    expect(labels(await requireCompletions(doc))).toContain("replicas");
    const first = "apiVersion: v1\nkind: ConfigMap\n";
    const res = await requireCompletions(`${first}\n---\n${HEAD}`, first.length);
    expect(labels(res)).toContain("data");
    expect(labels(res)).not.toContain("spec");
  });

  it("does not walk past --- into the previous document", async () => {
    const doc = `${HEAD}spec:\n  template:\n---\napiVersion: v1\nkind: ConfigMap\n`;
    const res = await requireCompletions(doc);
    expect(labels(res)).toContain("data");
  });

  it("accepts a quoted kind", async () => {
    const res = await requireCompletions('apiVersion: "apps/v1"\nkind: "Deployment"\nspec:\n  ');
    expect(labels(res)).toContain("replicas");
  });

  it("offers int-or-string and CRD fields", async () => {
    const res = await requireCompletions("apiVersion: example.fleetglass.dev/v1\nkind: Widget\nspec:\n  ");
    expect(option(res, "size")?.detail).toBe("int-or-string (required)");
    expect(option(res, "mode")?.info).toBe("How to run.");
    expect(labels(await requireCompletions("apiVersion: example.fleetglass.dev/v1\nkind: Widget\nspec:\n  mode: "))).toEqual(["fast", "safe"]);
  });
});
