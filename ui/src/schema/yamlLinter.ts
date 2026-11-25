import { syntaxTree } from "@codemirror/language";
import { linter, type Diagnostic } from "@codemirror/lint";
import type { EditorState } from "@codemirror/state";
import type { SyntaxNode } from "@lezer/common";
import type { K8sJsonSchema, SchemaLookup } from "./types";
import {
  documentInfo,
  documentNodes,
  mergeAllOf,
  pairKey,
  pairValue,
  preservesUnknown,
  rootMapping,
  unquote,
} from "./yamlAst";

/** Fields never linted: server-written state, not something a user edits. */
const SKIPPED_ROOT_FIELDS = new Set(["status"]);
const SKIPPED_METADATA_FIELDS = new Set(["managedFields"]);

type ValueKind = "mapping" | "sequence" | "string" | "integer" | "number" | "boolean" | "null" | "unknown";

const MAPPING = new Set(["BlockMapping", "FlowMapping"]);
const SEQUENCE = new Set(["BlockSequence", "FlowSequence"]);

/** YAML 1.2 core-schema type of a scalar or collection node. */
function valueKind(node: SyntaxNode, docText: string): ValueKind {
  if (MAPPING.has(node.name)) return "mapping";
  if (SEQUENCE.has(node.name)) return "sequence";
  if (node.name === "QuotedLiteral" || node.name === "BlockLiteral") return "string";
  if (node.name !== "Literal") return "unknown"; // anchors, aliases, tags, errors
  const text = docText.slice(node.from, node.to).trim();
  if (text === "" || text === "~" || text === "null" || text === "Null" || text === "NULL") {
    return "null";
  }
  if (/^(true|True|TRUE|false|False|FALSE)$/.test(text)) return "boolean";
  // YAML 1.1 booleans: decoded as boolean or string depending on the client, so unchecked.
  if (/^(y|Y|yes|Yes|YES|n|N|no|No|NO|on|On|ON|off|Off|OFF)$/.test(text)) return "unknown";
  if (/^[-+]?(0|[1-9][0-9]*)$/.test(text) || /^0o[0-7]+$/.test(text) || /^0x[0-9a-fA-F]+$/.test(text)) {
    return "integer";
  }
  if (/^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$/.test(text) || /^[-+]?\.(inf|Inf|INF)$/.test(text) || /^\.(nan|NaN|NAN)$/.test(text)) {
    return "number";
  }
  return "string";
}

function kindMatchesType(kind: ValueKind, type: string): boolean {
  switch (type) {
    case "object":
      return kind === "mapping";
    case "array":
      return kind === "sequence";
    case "string":
      return kind === "string";
    case "integer":
      return kind === "integer";
    case "number":
      return kind === "integer" || kind === "number";
    case "boolean":
      return kind === "boolean";
    default:
      return true;
  }
}

const KIND_LABEL: Record<ValueKind, string> = {
  mapping: "mapping",
  sequence: "sequence",
  string: "string",
  integer: "integer",
  number: "number",
  boolean: "boolean",
  null: "null",
  unknown: "value",
};

/** Item value nodes of a sequence; `-` and `,` siblings and empty items are skipped. */
export function sequenceItems(seq: SyntaxNode): Array<SyntaxNode | null> {
  const items: Array<SyntaxNode | null> = [];
  for (let child = seq.firstChild; child; child = child.nextSibling) {
    if (child.name === "Item") {
      let inner = child.firstChild;
      while (inner && inner.name === "Comment") inner = inner.nextSibling;
      items.push(inner);
    }
  }
  return items;
}

/** Whether a value structurally matches a branch, for picking `oneOf`/`anyOf` alternatives. */
function matchesBranch(node: SyntaxNode | null, branch: K8sJsonSchema, docText: string): boolean {
  const schema = mergeAllOf(branch);
  if (!node) return true;
  const kind = valueKind(node, docText);
  if (kind === "unknown") return true;
  if (kind === "null" && schema.nullable) return true;
  if (schema.type && !kindMatchesType(kind, schema.type)) return false;
  if (Array.isArray(schema.enum) && schema.enum.length > 0 && kind !== "mapping" && kind !== "sequence") {
    const text = unquote(docText.slice(node.from, node.to));
    if (!schema.enum.some((e) => String(e) === text)) return false;
  }
  return true;
}

/**
 * Validates every document of the editor against its kind's schema. A document whose
 * schema is not available produces no diagnostics.
 */
export function validateYamlDocument(state: EditorState, lookup: SchemaLookup): Diagnostic[] {
  const docText = state.doc.toString();
  if (!docText.trim()) return [];
  const tree = syntaxTree(state);
  const diagnostics: Diagnostic[] = [];

  for (const doc of documentNodes(tree)) {
    const mapping = rootMapping(doc);
    if (!mapping) continue;
    const { kind, apiVersion } = documentInfo(doc, docText);
    if (!kind || !apiVersion) continue;
    const schema = lookup(apiVersion, kind);
    if (!schema) continue;
    validateMapping(mapping, schema, null, kind, []);
  }
  return diagnostics;

  function report(node: { from: number; to: number }, severity: Diagnostic["severity"], message: string): void {
    diagnostics.push({ from: node.from, to: Math.max(node.to, node.from + 1), severity, message });
  }

  function validateMapping(
    mapping: SyntaxNode,
    raw: K8sJsonSchema,
    ownerKey: SyntaxNode | null,
    label: string,
    path: string[],
  ): void {
    const schema = mergeAllOf(raw);
    const props = schema.properties ?? {};
    const additional = schema.additionalProperties;
    // Unknown fields are only reported when the schema says the object is closed.
    const closed =
      !preservesUnknown(schema) &&
      additional !== true &&
      (Object.keys(props).length > 0 || additional === false);
    const present = new Set<string>();

    for (let child = mapping.firstChild; child; child = child.nextSibling) {
      if (child.name !== "Pair") continue;
      const key = pairKey(child, docText);
      if (!key) continue;
      present.add(key.text);
      if (path.length === 0 && SKIPPED_ROOT_FIELDS.has(key.text)) continue;
      if (path.length === 1 && path[0] === "metadata" && SKIPPED_METADATA_FIELDS.has(key.text)) {
        continue;
      }
      const value = pairValue(child);
      const childPath = [...path, key.text];

      if (Object.hasOwn(props, key.text)) {
        const propSchema = props[key.text];
        if (propSchema) validateValue(value, propSchema, key.node, key.text, childPath);
      } else if (typeof additional === "object") {
        validateValue(value, additional, key.node, key.text, childPath);
      } else if (closed) {
        report(key.node, "warning", `Unknown field "${key.text}" in ${label}`);
      }
    }

    for (const req of schema.required ?? []) {
      if (!present.has(req)) {
        const range = ownerKey ?? {
          from: mapping.from,
          to: Math.min(state.doc.lineAt(mapping.from).to, mapping.to),
        };
        report(range, "error", `Missing required field "${req}" in ${label}`);
      }
    }
  }

  function validateValue(
    value: SyntaxNode | null,
    raw: K8sJsonSchema,
    keyNode: SyntaxNode,
    label: string,
    path: string[],
  ): void {
    const schema = mergeAllOf(raw);
    // `&anchor value` is checked as `value`; a `!!tag` changes the type, so it is not checked.
    let node = value;
    while (node && node.name === "Anchored") {
      const inner: SyntaxNode | null = node.lastChild;
      node = inner && inner.name !== "Anchor" ? inner : null;
    }
    if (!node) {
      // Empty value: null, i.e. unset. Only an object that needs fields is worth a note.
      if (value === null && schema.type === "object") {
        for (const req of schema.required ?? []) {
          report(keyNode, "error", `Missing required field "${req}" in ${label}`);
        }
      }
      return;
    }
    const kind = valueKind(node, docText);
    if (kind === "unknown" || (kind === "null" && schema.nullable !== false)) return;

    const intOrString = schema["x-kubernetes-int-or-string"] === true || schema.format === "int-or-string";
    if (intOrString) {
      if (kind !== "integer" && kind !== "string") {
        report(node, "error", `Invalid type for "${label}": expected integer or string, got ${KIND_LABEL[kind]}`);
      }
      return;
    }

    if (schema.type && !kindMatchesType(kind, schema.type)) {
      // A quoted number where a string is wanted is fine; the reverse is a real error.
      report(node, "error", `Invalid type for "${label}": expected ${schema.type}, got ${KIND_LABEL[kind]}`);
      return;
    }

    const alternatives = [schema.oneOf, schema.anyOf].filter(
      (a): a is K8sJsonSchema[] => Array.isArray(a) && a.length > 0,
    );
    for (const branches of alternatives) {
      if (!branches.some((b) => matchesBranch(node, b, docText))) {
        const expected = [...new Set(branches.map((b) => mergeAllOf(b).type ?? "value"))].join(" or ");
        report(node, "error", `Invalid type for "${label}": expected ${expected}, got ${KIND_LABEL[kind]}`);
        return;
      }
    }

    if (
      Array.isArray(schema.enum) &&
      schema.enum.length > 0 &&
      kind !== "mapping" &&
      kind !== "sequence"
    ) {
      const text = unquote(docText.slice(node.from, node.to));
      if (!schema.enum.some((e) => String(e) === text)) {
        report(
          node,
          "warning",
          `Value "${text}" is not one of: ${schema.enum.map((e) => `"${String(e)}"`).join(", ")}`,
        );
      }
    }

    if (kind === "mapping") {
      validateMapping(node, schema, keyNode, label, path);
    } else if (kind === "sequence" && schema.items) {
      for (const item of sequenceItems(node)) {
        // An empty `-` (still being typed) is not checked.
        if (!item) continue;
        validateValue(item, schema.items, keyNode, `${label}[]`, [...path, "[]"]);
      }
    }
  }
}

/** CodeMirror linter that validates against schemas returned by `lookup`. */
export function k8sYamlLinter(lookup: SchemaLookup) {
  return linter((view) => validateYamlDocument(view.state, lookup), { delay: 300 });
}
