import { yamlLanguage } from "@codemirror/lang-yaml";
import type { EditorState } from "@codemirror/state";
import type { SyntaxNode, Tree } from "@lezer/common";
import type { DocumentInfo, K8sJsonSchema } from "./types";

export interface YamlContext {
  path: string[];
  isKey: boolean;
  isValue: boolean;
  keyPrefix?: string;
  valueKey?: string;
  valuePrefix?: string;
  from: number;
  to: number;
}

/** Strips one pair of matching surrounding quotes. */
export function unquote(text: string): string {
  const t = text.trim();
  if (t.length >= 2 && (t[0] === '"' || t[0] === "'") && t[t.length - 1] === t[0]) {
    return t.slice(1, -1);
  }
  return t;
}

/** Top-level `Document` nodes of a parsed stream, in order. */
export function documentNodes(tree: Tree): SyntaxNode[] {
  const top = tree.topNode;
  if (top.name === "Document") return [top];
  const docs: SyntaxNode[] = [];
  for (let child = top.firstChild; child; child = child.nextSibling) {
    if (child.name === "Document") docs.push(child);
  }
  return docs;
}

/** The document containing `pos` (the last one starting at or before it). */
export function documentAt(tree: Tree, pos: number): SyntaxNode | null {
  let found: SyntaxNode | null = null;
  for (const doc of documentNodes(tree)) {
    if (doc.from <= pos) found = doc;
    else break;
  }
  return found;
}

/** Root mapping of a document, if the document is a mapping. */
export function rootMapping(doc: SyntaxNode): SyntaxNode | null {
  for (let child = doc.firstChild; child; child = child.nextSibling) {
    if (child.name === "BlockMapping" || child.name === "FlowMapping") return child;
  }
  return null;
}

/** Value node of a `Pair`, or `null` when the value is empty (YAML null). */
export function pairValue(pair: SyntaxNode): SyntaxNode | null {
  let afterColon = false;
  for (let child = pair.firstChild; child; child = child.nextSibling) {
    if (child.name === ":") {
      afterColon = true;
      continue;
    }
    if (afterColon && child.name !== "Comment") return child;
  }
  return null;
}

/** Unquoted text of a `Pair`'s key. */
export function pairKey(pair: SyntaxNode, docText: string): { node: SyntaxNode; text: string } | null {
  const key = pair.getChild("Key");
  if (!key) return null;
  return { node: key, text: unquote(docText.slice(key.from, key.to)) };
}

function docInfoOfMapping(mapping: SyntaxNode, docText: string): DocumentInfo {
  const info: DocumentInfo = {};
  for (let child = mapping.firstChild; child; child = child.nextSibling) {
    if (child.name !== "Pair") continue;
    const key = pairKey(child, docText);
    const value = pairValue(child);
    if (!key || !value) continue;
    if (value.name !== "Literal" && value.name !== "QuotedLiteral") continue;
    const text = unquote(docText.slice(value.from, value.to));
    if (key.text === "kind") info.kind = text;
    if (key.text === "apiVersion") info.apiVersion = text;
  }
  return info;
}

/** kind/apiVersion of one document node. */
export function documentInfo(doc: SyntaxNode, docText: string): DocumentInfo {
  const mapping = rootMapping(doc);
  return mapping ? docInfoOfMapping(mapping, docText) : {};
}

/**
 * kind/apiVersion of the document containing `pos` (default: the first document).
 * Only that document is read, never a neighbour separated by `---`.
 */
export function findDocInfo(docText: string, tree: Tree, pos = 0): DocumentInfo {
  const doc = documentAt(tree, pos) ?? documentNodes(tree)[0];
  return doc ? documentInfo(doc, docText) : {};
}

/** kind/apiVersion of every document in `text`, in order. */
export function listDocInfos(text: string): DocumentInfo[] {
  if (!text.trim()) return [];
  const tree = yamlLanguage.parser.parse(text);
  return documentNodes(tree).map((doc) => documentInfo(doc, text));
}

/** `true` when the schema accepts fields it does not declare. */
export function preservesUnknown(schema: K8sJsonSchema): boolean {
  return schema["x-kubernetes-preserve-unknown-fields"] === true;
}

/**
 * Folds `allOf` branches into one schema: properties and required are merged, the
 * first declared type/format/description wins. `oneOf`/`anyOf` are left as they are.
 */
export function mergeAllOf(schema: K8sJsonSchema): K8sJsonSchema {
  if (!Array.isArray(schema.allOf) || schema.allOf.length === 0) return schema;
  const merged: K8sJsonSchema = { ...schema };
  delete merged.allOf;
  for (const raw of schema.allOf) {
    const branch = mergeAllOf(raw);
    if (merged.type === undefined && branch.type !== undefined) merged.type = branch.type;
    if (merged.format === undefined && branch.format !== undefined) merged.format = branch.format;
    if (merged.description === undefined && branch.description !== undefined) {
      merged.description = branch.description;
    }
    if (merged.items === undefined && branch.items !== undefined) merged.items = branch.items;
    if (merged.enum === undefined && branch.enum !== undefined) merged.enum = branch.enum;
    if (merged.additionalProperties === undefined && branch.additionalProperties !== undefined) {
      merged.additionalProperties = branch.additionalProperties;
    }
    if (branch.properties) {
      // Outer and earlier declarations win; order follows declaration.
      const props = { ...merged.properties };
      for (const [name, prop] of Object.entries(branch.properties)) {
        if (!Object.hasOwn(props, name)) props[name] = prop;
      }
      merged.properties = props;
    }
    if (branch.required) merged.required = [...(merged.required ?? []), ...branch.required];
    if (preservesUnknown(branch)) merged["x-kubernetes-preserve-unknown-fields"] = true;
    if (branch["x-kubernetes-int-or-string"]) merged["x-kubernetes-int-or-string"] = true;
    if (branch.oneOf && !merged.oneOf) merged.oneOf = branch.oneOf;
    if (branch.anyOf && !merged.anyOf) merged.anyOf = branch.anyOf;
  }
  return merged;
}

/** Schema of the field `key` inside the object schema `schema`, if it is declared. */
export function propertySchema(schema: K8sJsonSchema, key: string): K8sJsonSchema | undefined {
  const s = mergeAllOf(schema);
  const props = s.properties;
  if (props && Object.hasOwn(props, key)) return props[key];
  if (typeof s.additionalProperties === "object") return s.additionalProperties;
  return undefined;
}

/**
 * Navigates a schema down a property path; `[]` (or a numeric index) steps into
 * array items. Returns `undefined` when the path leaves the declared schema.
 */
export function findSchemaForPath(
  rootSchema: K8sJsonSchema,
  path: string[],
): K8sJsonSchema | undefined {
  let current: K8sJsonSchema | undefined = mergeAllOf(rootSchema);
  for (const segment of path) {
    if (!current) return undefined;
    if (segment === "[]" || /^\d+$/.test(segment)) {
      current = current.items ? mergeAllOf(current.items) : undefined;
      continue;
    }
    const next = propertySchema(current, segment);
    current = next ? mergeAllOf(next) : undefined;
  }
  return current;
}

const DASH_PREFIX = /^-(\s+|$)/;

function stripComment(value: string): string {
  return value.startsWith("#") ? "" : value;
}

/**
 * Parent property path of the line at `pos`, from indentation. Array items appear
 * as `[]`. The scan stops at the document separator `---`.
 */
export function getParentPathAtPos(state: EditorState, pos: number): string[] {
  const line = state.doc.lineAt(pos);
  const textBeforePos = line.text.slice(0, pos - line.from);
  const firstNonSpace = textBeforePos.search(/\S/);
  const currentIndent = firstNonSpace === -1 ? textBeforePos.length : firstNonSpace;

  const path: string[] = [];
  let neededIndent = currentIndent;
  // After an item `- …` at indent N, the owning key may sit at the same indent N
  // (kubectl style: `containers:\n- name: x`).
  let afterDash = DASH_PREFIX.test(line.text.trimStart());

  for (let l = line.number - 1; l >= 1; l--) {
    const prevLine = state.doc.line(l);
    const trimmed = prevLine.text.trim();
    if (trimmed.startsWith("---") || trimmed.startsWith("...")) break;
    if (!trimmed || trimmed.startsWith("#")) continue;

    const lineIndent = prevLine.text.search(/\S/);
    const isArrayItem = DASH_PREFIX.test(trimmed);
    const isParentCandidate =
      lineIndent < neededIndent || (afterDash && !isArrayItem && lineIndent === neededIndent);
    if (!isParentCandidate) continue;

    const content = isArrayItem ? trimmed.replace(DASH_PREFIX, "") : trimmed;
    const colonMatch = /:(\s|$)/.exec(content);
    if (colonMatch) {
      const key = unquote(content.slice(0, colonMatch.index));
      const valPart = stripComment(content.slice(colonMatch.index + 1).trim());
      if (isArrayItem) {
        // `- resources:` opens a mapping inside the item; `- name: x` is a sibling.
        if (!valPart) path.unshift(key);
        path.unshift("[]");
      } else {
        // A key with an inline scalar cannot be a parent.
        if (valPart && !/^[|>&!]/.test(valPart)) continue;
        path.unshift(key);
      }
    } else if (isArrayItem) {
      path.unshift("[]");
    } else {
      continue;
    }
    neededIndent = lineIndent;
    afterDash = isArrayItem;
    if (lineIndent === 0 && !isArrayItem) break;
  }

  if (DASH_PREFIX.test(line.text.trimStart())) path.push("[]");
  return path;
}

/**
 * Resolves context (path, isKey, isValue, word prefixes) at a given cursor position.
 */
export function getYamlContextAtPos(state: EditorState, pos: number): YamlContext {
  const line = state.doc.lineAt(pos);
  const col = pos - line.from;
  const lineText = line.text;
  const colonMatch = /:(\s|$)/.exec(lineText);
  const colonIdx = colonMatch ? colonMatch.index : -1;

  const path = getParentPathAtPos(state, pos);

  if (colonIdx !== -1 && col > colonIdx) {
    const keyPart = unquote(lineText.slice(0, colonIdx).trim().replace(DASH_PREFIX, ""));
    const valBeforeCursor = lineText.slice(colonIdx + 1, col).trimStart();
    const valMatch = valBeforeCursor.match(/[\w./-]*$/);
    const valuePrefix = valMatch ? valMatch[0] : "";
    return {
      path,
      isKey: false,
      isValue: true,
      valueKey: keyPart,
      valuePrefix,
      from: pos - valuePrefix.length,
      to: pos,
    };
  }

  const keyMatch = lineText.slice(0, col).match(/[\w.-]*$/);
  const keyPrefix = keyMatch ? keyMatch[0] : "";
  return {
    path,
    isKey: true,
    isValue: false,
    keyPrefix,
    from: pos - keyPrefix.length,
    to: pos,
  };
}

/**
 * Finds a Key syntax node at a specific position.
 */
export function findKeyNodeAtPos(tree: Tree, pos: number): SyntaxNode | null {
  let node: SyntaxNode | null = tree.resolveInner(pos, -1);
  while (node && node.name !== "Key") {
    if (node.name === "Pair") {
      const k = node.getChild("Key");
      if (k && k.from <= pos && pos <= k.to) return k;
    }
    node = node.parent;
  }
  return node?.name === "Key" ? node : null;
}
