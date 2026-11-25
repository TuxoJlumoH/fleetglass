import { syntaxTree } from "@codemirror/language";
import { hoverTooltip, type EditorView, type Tooltip } from "@codemirror/view";
import type { SchemaLookup } from "./types";
import {
  findDocInfo,
  findKeyNodeAtPos,
  findSchemaForPath,
  getParentPathAtPos,
  mergeAllOf,
  unquote,
} from "./yamlAst";

/** Hover documentation for the schema field whose key is at `pos`. */
export function getK8sHoverTooltip(
  view: EditorView,
  pos: number,
  lookup: SchemaLookup,
): Tooltip | null {
  const state = view.state;
  const docText = state.doc.toString();
  const tree = syntaxTree(state);

  const keyNode = findKeyNodeAtPos(tree, pos);
  if (!keyNode) return null;
  const keyName = unquote(docText.slice(keyNode.from, keyNode.to));
  if (!keyName) return null;

  const { kind, apiVersion } = findDocInfo(docText, tree, keyNode.from);
  if (!kind || !apiVersion) return null;
  const rootSchema = lookup(apiVersion, kind);
  if (!rootSchema) return null;

  const targetSchema = findSchemaForPath(rootSchema, getParentPathAtPos(state, keyNode.from));
  const raw = targetSchema?.properties?.[keyName];
  if (!targetSchema || !raw) return null;
  const propSchema = mergeAllOf(raw);
  const isRequired = targetSchema.required?.includes(keyName) ?? false;
  const intOrString =
    propSchema["x-kubernetes-int-or-string"] === true || propSchema.format === "int-or-string";

  return {
    pos: keyNode.from,
    end: keyNode.to,
    above: true,
    create() {
      const dom = document.createElement("div");
      dom.className = "k8s-hover-tooltip";

      const header = document.createElement("div");
      header.className = "k8s-hover-header";
      const title = document.createElement("span");
      title.className = "k8s-hover-title";
      title.textContent = keyName;
      header.appendChild(title);

      const typeBadge = document.createElement("span");
      typeBadge.className = "k8s-hover-type";
      typeBadge.textContent = intOrString ? "int-or-string" : (propSchema.type ?? "any");
      header.appendChild(typeBadge);

      if (isRequired) {
        const reqBadge = document.createElement("span");
        reqBadge.className = "k8s-hover-badge required";
        reqBadge.textContent = "required";
        header.appendChild(reqBadge);
      }
      if (propSchema.default !== undefined) {
        const defBadge = document.createElement("span");
        defBadge.className = "k8s-hover-badge default";
        const d = propSchema.default;
        defBadge.textContent = `default: ${typeof d === "string" ? d : JSON.stringify(d)}`;
        header.appendChild(defBadge);
      }
      dom.appendChild(header);

      if (propSchema.description) {
        const desc = document.createElement("div");
        desc.className = "k8s-hover-desc";
        desc.textContent = propSchema.description;
        dom.appendChild(desc);
      }
      if (Array.isArray(propSchema.enum) && propSchema.enum.length > 0) {
        const enumBox = document.createElement("div");
        enumBox.className = "k8s-hover-enum";
        enumBox.textContent = `Allowed values: ${propSchema.enum.map((e) => `"${String(e)}"`).join(", ")}`;
        dom.appendChild(enumBox);
      }
      return { dom };
    },
  };
}

export function k8sYamlHover(lookup: SchemaLookup) {
  return hoverTooltip((view, pos) => getK8sHoverTooltip(view, pos, lookup));
}
