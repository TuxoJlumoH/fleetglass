import { Channel } from "@tauri-apps/api/core";
import { commands, type DebugNodeRequest_Deserialize, type ResourceDetail } from "../generated/bindings";
import type { Row } from "../stores/pods";

export interface ExecClient {
  createChannel?: (onOutput: (chunk: string) => void) => Channel<string>;
  startExecSession: (
    context: string,
    namespace: string,
    pod: string,
    container: string | null,
    command: string[] | null,
    confirmedName: string | null,
    onOutput: Channel<string>,
  ) => ReturnType<typeof commands.startExecSession>;
  startAttachSession?: (
    context: string,
    namespace: string,
    pod: string,
    container: string | null,
    confirmedName: string | null,
    onOutput: Channel<string>,
  ) => ReturnType<typeof commands.startAttachSession>;
  startNodeDebugSession?: (
    request: DebugNodeRequest_Deserialize,
    onOutput: Channel<string>,
  ) => ReturnType<typeof commands.startNodeDebugSession>;
  writeExecStdin: (id: number, data: string) => ReturnType<typeof commands.writeExecStdin>;
  stopExecSession: (id: number) => ReturnType<typeof commands.stopExecSession>;
}

export const defaultExecClient: ExecClient = {
  createChannel: (onOutput) => new Channel<string>(onOutput),
  startExecSession: (context, namespace, pod, container, command, confirmedName, onOutput) =>
    commands.startExecSession(context, namespace, pod, container, command, confirmedName, onOutput),
  startAttachSession: (context, namespace, pod, container, confirmedName, onOutput) =>
    commands.startAttachSession(context, namespace, pod, container, confirmedName, onOutput),
  startNodeDebugSession: (request, onOutput) =>
    commands.startNodeDebugSession(request, onOutput),
  writeExecStdin: (id, data) => commands.writeExecStdin(id, data),
  stopExecSession: (id) => commands.stopExecSession(id),
};

let globalExecClient: ExecClient = defaultExecClient;

export function setExecClient(client?: ExecClient): void {
  globalExecClient = client ?? defaultExecClient;
}

export function getExecClient(): ExecClient {
  return globalExecClient;
}

export function stripAnsi(str: string): string {
  // eslint-disable-next-line no-control-regex
  return str.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, "");
}

export function extractContainersFromYaml(yaml: string): string[] {
  if (!yaml) return [];
  const containers: string[] = [];
  const lines = yaml.split("\n");
  let inContainersSection = false;
  let sectionIndent = 0;
  let itemIndent = -1;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!line || !line.trim() || line.trim().startsWith("#")) continue;

    const matchSection = line.match(/^(\s*)(containers|initContainers):\s*$/);
    if (matchSection) {
      inContainersSection = true;
      sectionIndent = matchSection[1]?.length ?? 0;
      itemIndent = -1;
      continue;
    }

    if (inContainersSection) {
      const curIndent = line.search(/\S/);
      const isItemDash = line.trimStart().startsWith("- ");

      if (curIndent < sectionIndent || (curIndent === sectionIndent && !isItemDash)) {
        inContainersSection = false;
        continue;
      }

      // Match item start with "- "
      const matchDash = line.match(/^(\s*)-\s*(.*)$/);
      if (matchDash) {
        const dashIndent = matchDash[1]?.length ?? 0;
        if (itemIndent === -1) {
          itemIndent = dashIndent;
        }
        if (dashIndent === itemIndent) {
          const rest = matchDash[2] ?? "";
          const nameMatch = rest.match(/^name:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
          if (nameMatch && nameMatch[1] && !containers.includes(nameMatch[1])) {
            containers.push(nameMatch[1]);
          }
          continue;
        }
      }

      // Match property `name: ...` at container property indentation level
      if (itemIndent !== -1) {
        const propIndent = itemIndent + 2;
        const propMatch = line.match(/^\s*name:\s*['"]?([a-zA-Z0-9._-]+)['"]?/);
        if (propMatch && propMatch[1] && curIndent === propIndent && !line.trimStart().startsWith("-")) {
          if (!containers.includes(propMatch[1])) {
            containers.push(propMatch[1]);
          }
        }
      }
    }
  }

  return containers;
}

export function extractPodContainers(
  row: Row | null,
  detail: ResourceDetail | null,
  yamlText?: string,
): string[] {
  const result: string[] = [];

  const yaml = detail?.yaml || yamlText || "";
  if (yaml) {
    for (const name of extractContainersFromYaml(yaml)) {
      if (!result.includes(name)) {
        result.push(name);
      }
    }
  }

  if (detail && typeof detail === "object") {
    const anyDetail = detail as {
      resource?: { spec?: { containers?: Array<{ name?: string }> } };
      spec?: { containers?: Array<{ name?: string }> };
    };
    const containersList =
      anyDetail.resource?.spec?.containers ?? anyDetail.spec?.containers;
    if (Array.isArray(containersList)) {
      for (const c of containersList) {
        if (c.name && typeof c.name === "string" && !result.includes(c.name)) {
          result.push(c.name);
        }
      }
    }
  }

  if (result.length === 0 && row && row.name) {
    result.push(row.name);
  }

  return result;
}
