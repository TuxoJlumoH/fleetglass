/**
 * RBAC manifest parsing and cross-link data extraction (`04-05a`).
 *
 * Extracts subject→role links for RoleBinding / ClusterRoleBinding,
 * and role→rule permissions for Role / ClusterRole.
 */

export interface RbacRoleRef {
  kind: string;
  name: string;
}

export interface RbacSubject {
  kind: string;
  name: string;
  namespace?: string;
}

export interface RbacRule {
  apiGroups: string[];
  resources: string[];
  verbs: string[];
}

export interface RbacInfo {
  roleRef?: RbacRoleRef;
  subjects?: RbacSubject[];
  rules?: RbacRule[];
}

/** Parses flow list `["a", "b"]` or returns single value. */
function parseYamlStringArray(raw: string): string[] {
  const trimmed = raw.trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    const inner = trimmed.slice(1, -1).trim();
    if (inner === "") return [];
    return inner
      .split(",")
      .map((item) => item.trim().replace(/^['"](.*)['"]$/, "$1"))
      .filter((s) => s.length > 0 || inner.includes('""') || inner.includes("''"));
  }
  return [trimmed.replace(/^['"](.*)['"]$/, "$1")];
}

export function parseRbacFromYaml(yamlText: string): RbacInfo {
  const result: RbacInfo = {};
  if (!yamlText) return result;

  const lines = yamlText.split("\n");
  let currentSection: "none" | "roleRef" | "subjects" | "rules" = "none";
  let currentSubject: Partial<RbacSubject> | null = null;
  let currentRule: Partial<RbacRule> | null = null;
  let ruleListProperty: "apiGroups" | "resources" | "verbs" | null = null;

  for (const rawLine of lines) {
    const line = rawLine.replace(/#.*$/, ""); // Strip comments
    const trimmed = line.trim();
    if (!trimmed) continue;

    const indent = rawLine.search(/\S/);

    const isTopLevelKey =
      indent === 0 &&
      !trimmed.startsWith("-") &&
      /^[a-zA-Z0-9_-]+:/.test(trimmed);

    if (isTopLevelKey) {
      if (currentSubject && currentSubject.kind && currentSubject.name) {
        result.subjects = result.subjects ?? [];
        result.subjects.push({
          kind: currentSubject.kind,
          name: currentSubject.name,
          ...(currentSubject.namespace ? { namespace: currentSubject.namespace } : {}),
        });
        currentSubject = null;
      }
      if (currentRule) {
        result.rules = result.rules ?? [];
        result.rules.push({
          apiGroups: currentRule.apiGroups ?? [],
          resources: currentRule.resources ?? [],
          verbs: currentRule.verbs ?? [],
        });
        currentRule = null;
      }

      if (trimmed.startsWith("roleRef:")) {
        currentSection = "roleRef";
        continue;
      } else if (trimmed.startsWith("subjects:")) {
        currentSection = "subjects";
        continue;
      } else if (trimmed.startsWith("rules:")) {
        currentSection = "rules";
        continue;
      } else {
        currentSection = "none";
        continue;
      }
    }

    if (currentSection === "roleRef") {
      const matchKind = trimmed.match(/^kind:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
      if (matchKind && matchKind[1]) {
        result.roleRef = result.roleRef ?? { kind: "", name: "" };
        result.roleRef.kind = matchKind[1];
      }
      const matchName = trimmed.match(/^name:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
      if (matchName && matchName[1]) {
        result.roleRef = result.roleRef ?? { kind: "", name: "" };
        result.roleRef.name = matchName[1];
      }
    } else if (currentSection === "subjects") {
      if (trimmed.startsWith("-")) {
        if (currentSubject && currentSubject.kind && currentSubject.name) {
          result.subjects = result.subjects ?? [];
          result.subjects.push({
            kind: currentSubject.kind,
            name: currentSubject.name,
            ...(currentSubject.namespace ? { namespace: currentSubject.namespace } : {}),
          });
        }
        currentSubject = {};
        const content = trimmed.slice(1).trim();
        const kMatch = content.match(/^kind:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
        if (kMatch && kMatch[1]) currentSubject.kind = kMatch[1];
        const nMatch = content.match(/^name:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
        if (nMatch && nMatch[1]) currentSubject.name = nMatch[1];
      } else if (currentSubject) {
        const kMatch = trimmed.match(/^kind:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
        if (kMatch && kMatch[1]) currentSubject.kind = kMatch[1];
        const nMatch = trimmed.match(/^name:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
        if (nMatch && nMatch[1]) currentSubject.name = nMatch[1];
        const nsMatch = trimmed.match(/^namespace:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
        if (nsMatch && nsMatch[1]) currentSubject.namespace = nsMatch[1];
      }
    } else if (currentSection === "rules") {
      if (trimmed.startsWith("-")) {
        if (currentRule) {
          result.rules = result.rules ?? [];
          result.rules.push({
            apiGroups: currentRule.apiGroups ?? [],
            resources: currentRule.resources ?? [],
            verbs: currentRule.verbs ?? [],
          });
        }
        currentRule = { apiGroups: [], resources: [], verbs: [] };
        ruleListProperty = null;

        const content = trimmed.slice(1).trim();
        if (content.startsWith("apiGroups:")) {
          currentRule.apiGroups = parseYamlStringArray(content.slice("apiGroups:".length));
        } else if (content.startsWith("resources:")) {
          currentRule.resources = parseYamlStringArray(content.slice("resources:".length));
        } else if (content.startsWith("verbs:")) {
          currentRule.verbs = parseYamlStringArray(content.slice("verbs:".length));
        }
      } else if (currentRule) {
        if (trimmed.startsWith("apiGroups:")) {
          const rest = trimmed.slice("apiGroups:".length).trim();
          if (rest) {
            currentRule.apiGroups = parseYamlStringArray(rest);
            ruleListProperty = null;
          } else {
            currentRule.apiGroups = [];
            ruleListProperty = "apiGroups";
          }
        } else if (trimmed.startsWith("resources:")) {
          const rest = trimmed.slice("resources:".length).trim();
          if (rest) {
            currentRule.resources = parseYamlStringArray(rest);
            ruleListProperty = null;
          } else {
            currentRule.resources = [];
            ruleListProperty = "resources";
          }
        } else if (trimmed.startsWith("verbs:")) {
          const rest = trimmed.slice("verbs:".length).trim();
          if (rest) {
            currentRule.verbs = parseYamlStringArray(rest);
            ruleListProperty = null;
          } else {
            currentRule.verbs = [];
            ruleListProperty = "verbs";
          }
        } else if (trimmed.startsWith("-") && ruleListProperty && currentRule[ruleListProperty]) {
          const val = trimmed.slice(1).trim().replace(/^['"](.*)['"]$/, "$1");
          currentRule[ruleListProperty]?.push(val);
        }
      }
    }
  }

  if (currentSubject && currentSubject.kind && currentSubject.name) {
    result.subjects = result.subjects ?? [];
    result.subjects.push({
      kind: currentSubject.kind,
      name: currentSubject.name,
      ...(currentSubject.namespace ? { namespace: currentSubject.namespace } : {}),
    });
  }
  if (currentRule) {
    result.rules = result.rules ?? [];
    result.rules.push({
      apiGroups: currentRule.apiGroups ?? [],
      resources: currentRule.resources ?? [],
      verbs: currentRule.verbs ?? [],
    });
  }

  return result;
}
