import {
  autocompletion,
  type Completion,
  type CompletionContext,
  type CompletionResult,
  type CompletionSource,
} from "@codemirror/autocomplete";
import { syntaxTree } from "@codemirror/language";
import type { K8sJsonSchema, SchemaLookup } from "./types";
import { findDocInfo, findSchemaForPath, getYamlContextAtPos, mergeAllOf } from "./yamlAst";

const STARTER_KEYS: Completion[] = [
  { label: "apiVersion", type: "property", detail: "string", boost: 3, apply: "apiVersion: " },
  { label: "kind", type: "property", detail: "string", boost: 3, apply: "kind: " },
  { label: "metadata", type: "property", detail: "object", boost: 2, apply: "metadata:\n  " },
];

function typeLabel(schema: K8sJsonSchema): string {
  const s = mergeAllOf(schema);
  if (s["x-kubernetes-int-or-string"] || s.format === "int-or-string") return "int-or-string";
  if (s.type) return s.type;
  const branches = s.oneOf ?? s.anyOf;
  if (branches) {
    return [...new Set(branches.map((b) => mergeAllOf(b).type ?? "any"))].join(" | ");
  }
  return "any";
}

/** Completion source for keys and values of the document under the cursor. */
export function createK8sCompletionSource(lookup: SchemaLookup): CompletionSource {
  return (context: CompletionContext): CompletionResult | null => {
    const { state, pos } = context;
    const ctx = getYamlContextAtPos(state, pos);
    const { kind, apiVersion } = findDocInfo(state.doc.toString(), syntaxTree(state), pos);
    const rootSchema = kind && apiVersion ? lookup(apiVersion, kind) : undefined;

    if (ctx.isValue) {
      if (!rootSchema || !ctx.valueKey) return null;
      const propSchema = findSchemaForPath(rootSchema, [...ctx.path, ctx.valueKey]);
      if (!propSchema) return null;
      const options: Completion[] = [];
      if (Array.isArray(propSchema.enum)) {
        for (const e of propSchema.enum) {
          if (typeof e !== "string" && typeof e !== "number" && typeof e !== "boolean") continue;
          options.push({ label: String(e), type: "enum", detail: "enum value" });
        }
      } else if (propSchema.type === "boolean") {
        options.push(
          { label: "true", type: "keyword", detail: "boolean" },
          { label: "false", type: "keyword", detail: "boolean" },
        );
      }
      if (options.length === 0) return null;
      const match = context.matchBefore(/[\w./-]*$/);
      return { from: match ? match.from : pos, options, validFor: /^[\w./-]*$/ };
    }

    const match = context.matchBefore(/[\w.-]*$/);
    if (!context.explicit && (!match || match.from === match.to)) return null;

    const options: Completion[] = [];
    const targetSchema = rootSchema ? findSchemaForPath(rootSchema, ctx.path) : undefined;
    if (targetSchema?.properties) {
      const required = new Set(targetSchema.required ?? []);
      const indent = /^\s*/.exec(state.doc.lineAt(pos).text)?.[0] ?? "";
      const childIndent = `${indent.replace(/-/g, " ")}  `;
      for (const [name, raw] of Object.entries(targetSchema.properties)) {
        if (ctx.path.length === 0 && name === "status") continue;
        const prop = mergeAllOf(raw);
        const isRequired = required.has(name);
        let apply = `${name}: `;
        if (prop.type === "object" && prop.properties) apply = `${name}:\n${childIndent}`;
        else if (prop.type === "array") apply = `${name}:\n${childIndent}- `;
        const item: Completion = {
          label: name,
          type: "property",
          detail: `${typeLabel(prop)}${isRequired ? " (required)" : ""}`,
          boost: isRequired ? 3 : 1,
          apply,
        };
        if (prop.description) item.info = prop.description;
        options.push(item);
      }
    } else if (ctx.path.length === 0 && !rootSchema) {
      options.push(...STARTER_KEYS);
    }

    if (options.length === 0) return null;
    return { from: match ? match.from : pos, options, validFor: /^[\w.-]*$/ };
  };
}

export function k8sYamlAutocomplete(lookup: SchemaLookup) {
  return autocompletion({ override: [createK8sCompletionSource(lookup)], defaultKeymap: true });
}
