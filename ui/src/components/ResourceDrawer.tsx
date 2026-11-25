import {
  ArrowLeft,
  ArrowRightLeft,
  BugPlay,
  ChevronsUpDown,
  FastForward,
  type LucideIcon,
  Pause,
  Play,
  RefreshCw,
  RotateCcw,
  ScrollText,
  SquareTerminal,
  Terminal,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { commands, type RelatedRef, type ResourceDetail } from "../generated/bindings";
import { useCompareStore, type CompareTarget } from "../stores/compare";
import {
  argoAppSyncActionFor,
  argoRolloutAbortActionFor,
  argoRolloutPromoteActionFor,
  argoRolloutRetryActionFor,
  canArgoAppSync,
  canArgoRollout,
  canCertRenew,
  canExternalSecretForceSync,
  canFluxReconcile,
  canRestart,
  canScale,
  certRenewActionFor,
  deleteActionFor,
  externalSecretForceSyncActionFor,
  fluxReconcileActionFor,
  fluxSuspendActionFor,
  restartActionFor,
  scaleActionFor,
  useActionsStore,
} from "../stores/actions";
import { useContextsStore } from "../stores/contexts";
import type { Cell, Row } from "../stores/pods";
import { useResourceDetailStore } from "../stores/resourceDetail";
import { formatAge } from "./age";
import { ActivePortForwards } from "./ActivePortForwards";
import { DebugContainerModal } from "./DebugContainerModal";
import { DiffModal } from "./DiffModal";
import { extractPodContainers, type ExecClient } from "./execTerminal";
import { ICON_SIZE, ICON_STROKE, KindIcon } from "./KindIcon";
import { NodeDebugModal } from "./NodeDebugModal";
import { CopyUrlButton, ExecShellPanel, OpenInBrowserButton } from "./OpsPanels";
import { PodSummary } from "./PodSummary";
import { PortForwardForm } from "./PortForwardForm";
import { parseRbacFromYaml } from "./rbacSummary";
import { SecretEditor, type SecretCodec } from "./SecretEditor";
import { STATUS_TONE } from "./statusTone";
import { WorkloadLogsView } from "./WorkloadLogsView";
import { YamlEditor } from "./YamlEditor";

export type DrawerTab = "Summary" | "Secret" | "YAML" | "Logs" | "Terminal" | "Events";

export interface LogLine {
  seq: number;
  text: string;
}

export interface DryRunResult {
  diff: string;
  kubectlPreview: string;
}

export interface ResourceDrawerProps {
  row: Row | null;
  yamlText?: string;
  logLines?: LogLine[];
  detail?: ResourceDetail | null;
  loading?: boolean;
  /** Error from fetching `detail`; shown instead of empty YAML/events. */
  error?: string | null;
  readOnly?: boolean;
  decodeSecret?: SecretCodec;
  encodeSecret?: SecretCodec;
  onClose: () => void;
  onDryRunApply?: (yaml: string) => Promise<DryRunResult>;
  execClient?: ExecClient;
  onOpenRelated?: (ref: RelatedRef) => void;
  /**
   * `screen`: the phone's full-screen detail (03-03b, mockup v0.3 `3-pod-detail`): an app bar
   * with a back arrow instead of the close button, and no keyboard hints on the actions.
   */
  variant?: "drawer" | "screen";
  /**
   * Exec shell and port-forward. False hides the Terminal tab, the Shell / Forward actions and
   * the port-forward forms (Android 0.3 ships neither, docs/TASKS.md).
   */
  remoteOps?: boolean;
  onOpenInBrowser?: ((url: string) => void | Promise<void>) | undefined;
}

const DEFAULT_TABS: readonly DrawerTab[] = ["Summary", "YAML", "Logs", "Events"];
const SECRET_TABS: readonly DrawerTab[] = ["Summary", "Secret", "YAML", "Logs", "Events"];
const POD_TABS: readonly DrawerTab[] = ["Summary", "YAML", "Logs", "Terminal", "Events"];

/** `get_resource_detail` returns at most this many events (newest first, `crates/core/src/detail.rs`). */
const EVENT_LIMIT = 50;

/** How often the header's relative age is recomputed (same as the table). */
const AGE_TICK_MS = 30_000;

/**
 * Creation time of a row: the last timestamp cell. Every projection puts "Age" after any other
 * timestamp (pods: "Last restart" comes first), see `resourceColumns.ts`. Rows built from a
 * related ref have no cells, so no age is shown for them.
 */
function creationTimestamp(row: Row): string | null {
  for (let i = row.cells.length - 1; i >= 0; i -= 1) {
    const cell = row.cells[i];
    if (cell?.type === "timestamp") return cell.value || null;
  }
  return null;
}

/** Pod statuses whose container keeps dying (kubectl's STATUS column). */
const CRASH_REASON = /CrashLoopBackOff|Error|OOMKilled|ContainerCannotRun/;

/**
 * True when the pod's (selected) container is not running and has run before, so a previous
 * instance may have logs: from the detail's container states, else from the row's status.
 */
function containerNotRunning(
  row: Row,
  detail: ResourceDetail | null,
  container: string | undefined,
): boolean {
  const containers = detail?.pod?.containers.filter(
    (c) => !c.init && (container === undefined || c.name === container),
  );
  if (containers !== undefined && containers.length > 0) {
    return containers.some((c) => c.state !== "Running" && c.restarts > 0);
  }
  return row.status === "failed" && CRASH_REASON.test(row.statusReason);
}

interface HeaderAction {
  id: string;
  label: string;
  icon: LucideIcon;
  onClick: () => void;
  /** Key that triggers the same action while focus is in the drawer (see `handleActionKey`). */
  kbd?: string;
  disabledReason?: string | null;
  danger?: boolean;
}

/**
 * Targets where a key is text input, never a drawer shortcut: form fields, contenteditable
 * (CodeMirror's content is one), the YAML editor and the xterm terminal (its helper textarea
 * and its container, so typing `r` in a shell never restarts anything).
 */
function isDrawerTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable) {
    return true;
  }
  return target.closest(".cm-editor, .xterm, .exec-panel__terminal") !== null;
}

// Event rows are projected by `project_event` as [Status(type), Text(reason), Text(message), Timestamp].
function extractEventInfo(ev: { cells: readonly Cell[] }) {
  const [typeCell, reasonCell, messageCell, timeCell] = ev.cells;
  return {
    type: typeCell?.type === "status" ? typeCell.value.label : "",
    reason: reasonCell?.type === "text" ? reasonCell.value : "",
    message: messageCell?.type === "text" ? messageCell.value : "",
    timestamp: timeCell?.type === "timestamp" ? timeCell.value : "",
  };
}

function parseSecretFromYaml(yaml: string): {
  data: Record<string, string>;
  stringData: Record<string, string>;
} {
  const data: Record<string, string> = {};
  const stringData: Record<string, string> = {};
  if (!yaml) return { data, stringData };

  const lines = yaml.split("\n");
  let currentSection: "data" | "stringData" | null = null;
  let sectionIndent = 0;

  for (const line of lines) {
    if (!line.trim() || line.trim().startsWith("#")) continue;

    const dataMatch = line.match(/^(\s*)data:\s*$/);
    if (dataMatch) {
      currentSection = "data";
      sectionIndent = dataMatch[1]?.length ?? 0;
      continue;
    }
    const stringDataMatch = line.match(/^(\s*)stringData:\s*$/);
    if (stringDataMatch) {
      currentSection = "stringData";
      sectionIndent = stringDataMatch[1]?.length ?? 0;
      continue;
    }

    if (currentSection) {
      const lineIndent = line.search(/\S/);
      if (lineIndent <= sectionIndent) {
        currentSection = null;
      } else {
        const kvMatch = line.trim().match(/^['"]?([^:'"]+)['"]?\s*:\s*(.*)$/);
        if (kvMatch) {
          const key = kvMatch[1]?.trim() ?? "";
          let val = kvMatch[2]?.replace(/#.*$/, "").trim() ?? "";
          if (
            (val.startsWith('"') && val.endsWith('"')) ||
            (val.startsWith("'") && val.endsWith("'"))
          ) {
            val = val.slice(1, -1);
          }
          if (key) {
            if (currentSection === "data") {
              data[key] = val;
            } else {
              stringData[key] = val;
            }
          }
        }
      }
    }
  }

  return { data, stringData };
}

function updateYamlWithSecretData(
  existingYaml: string,
  base64Data: Record<string, string>,
): string {
  const newEntries = Object.entries(base64Data).map(([k, v]) => `  ${k}: ${v}`);
  if (!existingYaml.trim()) {
    return `apiVersion: v1\nkind: Secret\ndata:\n${newEntries.join("\n")}\n`;
  }

  const lines = existingYaml.split("\n");
  let dataIndex = -1;
  let dataIndent = 0;
  for (let i = 0; i < lines.length; i++) {
    const match = lines[i]?.match(/^(\s*)data:\s*$/);
    if (match) {
      dataIndex = i;
      dataIndent = match[1]?.length ?? 0;
      break;
    }
  }

  const indentedEntries = Object.entries(base64Data).map(
    ([k, v]) => `${" ".repeat(dataIndent + 2)}${k}: ${v}`,
  );

  if (dataIndex === -1) {
    return `${existingYaml.trimEnd()}\ndata:\n${indentedEntries.join("\n")}\n`;
  }

  let endIndex = dataIndex + 1;
  while (endIndex < lines.length) {
    const line = lines[endIndex];
    if (line !== undefined && !line.trim()) {
      endIndex++;
      continue;
    }
    const indent = line ? line.search(/\S/) : -1;
    if (indent !== -1 && indent <= dataIndent) {
      break;
    }
    endIndex++;
  }

  const before = lines.slice(0, dataIndex + 1);
  const after = lines.slice(endIndex);
  return [...before, ...indentedEntries, ...after].join("\n");
}

/** Render with `key={row.uid}` so edits and dry-run results never carry over to another resource. */
export function ResourceDrawer({
  row,
  yamlText = "",
  logLines,
  detail = null,
  loading = false,
  error = null,
  readOnly: readOnlyProp,
  decodeSecret,
  encodeSecret,
  onClose,
  onDryRunApply,
  execClient,
  onOpenRelated: onOpenRelatedProp,
  variant = "drawer",
  remoteOps = true,
  onOpenInBrowser,
}: ResourceDrawerProps) {
  const screen = variant === "screen";
  const asideRef = useRef<HTMLElement | null>(null);
  const [activeTab, setActiveTab] = useState<DrawerTab>("Summary");
  const [terminalMode, setTerminalMode] = useState<"exec" | "attach">("exec");
  const [selectedContainer, setSelectedContainer] = useState<string | undefined>(undefined);
  const [editedYaml, setEditedYaml] = useState<string | null>(null);
  const [dryRunResult, setDryRunResult] = useState<DryRunResult | null>(null);
  const [dryRunning, setDryRunning] = useState(false);
  const [showDebug, setShowDebug] = useState(false);
  const [showNodeDebug, setShowNodeDebug] = useState(false);
  const [dryRunError, setDryRunError] = useState<string | null>(null);
  const [applySuccess, setApplySuccess] = useState<string | null>(null);
  const [diffModalOpen, setDiffModalOpen] = useState(false);
  const [crossCompareTarget, setCrossCompareTarget] = useState<CompareTarget | null>(null);
  const selectedForCompare = useCompareStore((s) => s.selectedForCompare);
  const isSelfSelected =
    Boolean(selectedForCompare && row) &&
    (selectedForCompare?.uid
      ? selectedForCompare.uid === row?.uid
      : selectedForCompare?.name === row?.name &&
        selectedForCompare?.cluster === row?.cluster &&
        selectedForCompare?.kind === row?.kind);
  const [logRegex, setLogRegex] = useState("");
  const [followLogs, setFollowLogs] = useState(true);
  const [wrapLines, setWrapLines] = useState(false);
  const [previousContainer, setPreviousContainer] = useState(false);
  /** Container whose port-forward form is open in the pod summary. */
  const [pfContainer, setPfContainer] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [prevRowUid, setPrevRowUid] = useState<string | undefined>(row?.uid);
  if (row?.uid !== prevRowUid) {
    setPrevRowUid(row?.uid);
    setSelectedContainer(undefined);
    setPfContainer(null);
  }
  const eventsCountId = useId();
  const tabIdPrefix = useId();
  const panelId = `${tabIdPrefix}-panel`;
  const tabId = (tab: DrawerTab) => `${tabIdPrefix}-tab-${tab}`;

  useEffect(() => {
    const id = setInterval(() => {
      setNow(Date.now());
    }, AGE_TICK_MS);
    return () => {
      clearInterval(id);
    };
  }, []);

  const isSecret = useMemo(() => {
    if (!row) return false;
    if (row.kind === "Secret") return true;
    if (detail?.kind === "Secret") return true;
    const detailObj = detail as unknown as { resource?: { kind?: string } } | null;
    return detailObj?.resource?.kind === "Secret";
  }, [row, detail]);

  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const clusterKey = row?.cluster || selectedContext || "";
  const isReadOnly =
    readOnlyProp !== undefined
      ? readOnlyProp
      : clusterKey && settingsByContext[clusterKey]
        ? settingsByContext[clusterKey].readOnly
        : false;

  // Move focus into the drawer when it opens so Escape (handled on the drawer itself, not on
  // window) closes it without stealing Escape from other components.
  const hasRow = row !== null;
  useEffect(() => {
    if (hasRow) asideRef.current?.focus();
  }, [hasRow]);

  const originalYaml = detail?.yaml ?? yamlText;
  const currentYaml = editedYaml ?? originalYaml;
  const detailMissing = loading || error !== null;

  const parsedSecret = useMemo(() => {
    if (!isSecret) return { data: {}, stringData: {} };

    const detailObj = detail as unknown as {
      resource?: { data?: Record<string, string>; stringData?: Record<string, string> };
      data?: Record<string, string>;
      stringData?: Record<string, string>;
    } | null;

    const rawData = detailObj?.resource?.data ?? detailObj?.data;
    const rawStringData = detailObj?.resource?.stringData ?? detailObj?.stringData;
    const fromYaml = parseSecretFromYaml(currentYaml);

    return {
      data: rawData ?? fromYaml.data,
      stringData: rawStringData ?? fromYaml.stringData,
    };
  }, [isSecret, detail, currentYaml]);

  const rbacInfo = useMemo(() => {
    if (
      !row ||
      !["role", "clusterrole", "rolebinding", "clusterrolebinding"].includes(
        row.kind.toLowerCase(),
      )
    ) {
      return null;
    }
    return parseRbacFromYaml(currentYaml);
  }, [row, currentYaml]);

  const filteredLogs = useMemo(() => {
    if (!logLines) return [];
    if (!logRegex) {
      return logLines;
    }
    try {
      const regex = new RegExp(logRegex);
      return logLines.filter((line) => regex.test(line.text));
    } catch {
      return [];
    }
  }, [logLines, logRegex]);

  const isPod = row?.kind === "Pod";
  const tabs = isSecret ? SECRET_TABS : isPod && remoteOps ? POD_TABS : DEFAULT_TABS;
  const activeDrawerTab = tabs.includes(activeTab) ? activeTab : "Summary";

  const availableContainers = useMemo(() => {
    const list = extractPodContainers(row, detail, currentYaml);
    if (detail?.pod?.containers) {
      for (const c of detail.pod.containers) {
        if (!list.includes(c.name)) list.push(c.name);
      }
    }
    return list;
  }, [row, detail, currentYaml]);

  const endpoints = detail?.endpoints ?? [];

  if (!row) {
    return null;
  }

  const handleSelectTab = (tab: DrawerTab, containerName?: string) => {
    if (containerName) {
      setSelectedContainer(containerName);
    }
    setActiveTab(tab);
  };

  const handleOpenRelated = (ref: RelatedRef) => {
    if (onOpenRelatedProp) {
      onOpenRelatedProp(ref);
      return;
    }
    const targetRow: Row = {
      uid: ref.uid || `${ref.kind}/${ref.namespace ?? row.namespace ?? ""}/${ref.name}`,
      cluster: row.cluster,
      kind: ref.kind,
      namespace: ref.namespace ?? (ref.kind === "Node" ? null : row.namespace),
      name: ref.name,
      status: "unknown",
      statusReason: "",
      cells: [],
      labels: [],
      custom: [],
    };
    void useResourceDetailStore.getState().open(targetRow);
  };

  const openPortForward = () => {
    const containers = detail?.pod?.containers ?? [];
    const target =
      containers.find((c) => !c.init && c.ports.length > 0) ?? containers.find((c) => !c.init);
    if (target) {
      setPfContainer(target.name);
      setSelectedContainer(target.name);
    }
    // Without a pod summary the generic Summary shows the port-forward form directly.
    setActiveTab("Summary");
  };

  const readOnlyReason = isReadOnly
    ? `Context ${clusterKey} is read-only: write actions are disabled.`
    : null;
  const scaleAction = canScale(row) ? scaleActionFor(row) : null;
  const restartAction = canRestart(row) ? restartActionFor(row) : null;
  const requestRestart = () => {
    if (restartAction) useActionsStore.getState().request(restartAction);
  };
  const requestScale = () => {
    if (scaleAction) useActionsStore.getState().request(scaleAction);
  };
  const requestDelete = () => {
    useActionsStore.getState().request(deleteActionFor(row));
  };

  /**
   * r / s / Ctrl+D inside the drawer act on the drawer's resource, through the same request
   * path as the header buttons and only when that button is shown and enabled. Handled here
   * because focus is in the drawer and the table ignores keys from inside it.
   */
  const handleActionKey = (e: KeyboardEvent<HTMLElement>): boolean => {
    if (e.altKey || e.metaKey || isDrawerTypingTarget(e.target)) return false;
    if (document.querySelector('[role="dialog"][aria-modal="true"]')) return false;
    const key = e.key.toLowerCase();
    let run: (() => void) | null = null;
    if (e.ctrlKey) {
      if (key === "d" && !e.shiftKey && !isReadOnly) run = requestDelete;
    } else if (!e.shiftKey && !isReadOnly) {
      if (key === "r" && restartAction) run = requestRestart;
      else if (key === "s" && scaleAction) run = requestScale;
    }
    if (!run) return false;
    e.preventDefault();
    e.stopPropagation();
    run();
    return true;
  };

  const headerActions: HeaderAction[] = [
    ...(isPod
      ? [
          {
            id: "logs",
            label: "Logs",
            icon: ScrollText,
            onClick: () => {
              setActiveTab("Logs");
            },
          },
          ...(remoteOps
            ? [
                {
                  id: "shell",
                  label: "Shell",
                  icon: SquareTerminal,
                  onClick: () => {
                    setTerminalMode("exec");
                    setActiveTab("Terminal");
                  },
                },
                {
                  id: "attach",
                  label: "Attach",
                  icon: Terminal,
                  onClick: () => {
                    setTerminalMode("attach");
                    setActiveTab("Terminal");
                  },
                },
              ]
            : []),
          ...(remoteOps && row.namespace !== null
            ? [
                { id: "forward", label: "Forward", icon: ArrowRightLeft, onClick: openPortForward },
                {
                  id: "debug",
                  label: "Debug",
                  icon: BugPlay,
                  onClick: () => {
                    setShowDebug(true);
                  },
                },
              ]
            : []),
        ]
      : []),
    ...(row.kind === "Node" && remoteOps
      ? [
          {
            id: "debug-node",
            label: "Debug",
            icon: BugPlay,
            onClick: () => {
              setShowNodeDebug(true);
            },
          },
        ]
      : []),
    ...(restartAction
      ? [
          {
            id: "restart",
            label: "Restart",
            icon: RotateCcw,
            onClick: requestRestart,
            disabledReason: readOnlyReason,
            kbd: "r",
          },
        ]
      : []),
    ...(scaleAction
      ? [
          {
            id: "scale",
            label: "Scale",
            icon: ChevronsUpDown,
            onClick: requestScale,
            disabledReason: readOnlyReason,
            kbd: "s",
          },
        ]
      : []),
    ...(canFluxReconcile(row)
      ? [
          {
            id: "flux-reconcile",
            label: "Reconcile",
            icon: RefreshCw,
            onClick: () => {
              useActionsStore.getState().request(fluxReconcileActionFor(row));
            },
            disabledReason: readOnlyReason,
          },
          {
            id: "flux-suspend",
            label: "Suspend",
            icon: Pause,
            onClick: () => {
              useActionsStore.getState().request(fluxSuspendActionFor(row, true));
            },
            disabledReason: readOnlyReason,
          },
          {
            id: "flux-resume",
            label: "Resume",
            icon: Play,
            onClick: () => {
              useActionsStore.getState().request(fluxSuspendActionFor(row, false));
            },
            disabledReason: readOnlyReason,
          },
        ]
      : []),
    ...(canArgoRollout(row)
      ? [
          {
            id: "rollout-promote",
            label: "Promote",
            icon: Play,
            onClick: () => {
              useActionsStore.getState().request(argoRolloutPromoteActionFor(row, false));
            },
            disabledReason: readOnlyReason,
          },
          {
            id: "rollout-promote-full",
            label: "Promote Full",
            icon: FastForward,
            onClick: () => {
              useActionsStore.getState().request(argoRolloutPromoteActionFor(row, true));
            },
            disabledReason: readOnlyReason,
          },
          {
            id: "rollout-abort",
            label: "Abort",
            icon: X,
            onClick: () => {
              useActionsStore.getState().request(argoRolloutAbortActionFor(row));
            },
            disabledReason: readOnlyReason,
          },
          {
            id: "rollout-retry",
            label: "Retry",
            icon: RotateCcw,
            onClick: () => {
              useActionsStore.getState().request(argoRolloutRetryActionFor(row));
            },
            disabledReason: readOnlyReason,
          },
        ]
      : []),
    ...(canArgoAppSync(row)
      ? [
          {
            id: "argo-sync",
            label: "Sync",
            icon: RefreshCw,
            onClick: () => {
              useActionsStore.getState().request(argoAppSyncActionFor(row));
            },
            disabledReason: readOnlyReason,
          },
        ]
      : []),
    ...(canCertRenew(row)
      ? [
          {
            id: "cert-renew",
            label: "Renew",
            icon: RefreshCw,
            onClick: () => {
              useActionsStore.getState().request(certRenewActionFor(row));
            },
            disabledReason: readOnlyReason,
          },
        ]
      : []),
    ...(canExternalSecretForceSync(row)
      ? [
          {
            id: "externalsecret-sync",
            label: "Force Sync",
            icon: RefreshCw,
            onClick: () => {
              useActionsStore.getState().request(externalSecretForceSyncActionFor(row));
            },
            disabledReason: readOnlyReason,
          },
        ]
      : []),
    {
      id: "compare",
      label: isSelfSelected
        ? "Clear compare selection"
        : selectedForCompare
          ? `Compare with ${selectedForCompare.name}`
          : "Select for compare",
      icon: ArrowRightLeft,
      onClick: () => {
        if (isSelfSelected) {
          useCompareStore.getState().clearCompare();
        } else if (selectedForCompare) {
          setCrossCompareTarget(selectedForCompare);
        } else {
          useCompareStore.getState().selectForCompare({
            cluster: row.cluster,
            kind: row.kind,
            namespace: row.namespace,
            name: row.name,
            uid: row.uid,
            manifest: originalYaml || undefined,
          });
        }
      },
    },
    {
      id: "delete",
      label: "Delete",
      icon: Trash2,
      onClick: requestDelete,
      disabledReason: readOnlyReason,
      danger: true,
      kbd: "Ctrl+D",
    },
  ];

  const createdAt = creationTimestamp(row);
  const metaLine = [
    row.kind,
    ...(row.namespace !== null ? [row.namespace] : []),
    row.cluster,
    ...(createdAt ? [formatAge(createdAt, now)] : []),
  ].join(" · ");

  // Known only once the detail has loaded; `events` is optional on the wire.
  const eventCount =
    !detailMissing && detail?.events !== undefined ? detail.events.length : null;
  // Core sets `eventsTruncated` only when more than EVENT_LIMIT events exist; the length check
  // is a fallback for an older backend that does not send the field.
  const eventsTruncated =
    detail?.eventsTruncated ?? (detail?.events?.length ?? 0) >= EVENT_LIMIT;
  const eventsCapped = eventCount !== null && eventsTruncated;

  const onTabKeyDown = (e: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let next: number;
    if (e.key === "ArrowRight") next = (index + 1) % tabs.length;
    else if (e.key === "ArrowLeft") next = (index - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = tabs.length - 1;
    else return;
    e.preventDefault();
    const tab = tabs[next];
    if (tab === undefined) return;
    setActiveTab(tab);
    const buttons = e.currentTarget.parentElement?.querySelectorAll<HTMLElement>('[role="tab"]');
    buttons?.[next]?.focus();
  };

  const handleDryRun = async () => {
    if (!onDryRunApply) return;
    const res = await onDryRunApply(currentYaml);
    setDryRunResult(res);
  };

  const handleReviewAndApply = async () => {
    setDryRunning(true);
    setDryRunError(null);
    setApplySuccess(null);
    try {
      const res = await commands.dryRunApply(clusterKey, currentYaml);
      setDryRunning(false);
      if (res.status === "error") {
        setDryRunError(res.error.message || `Dry-run failed: ${res.error.kind}`);
        return;
      }
      setDiffModalOpen(true);
    } catch (err: unknown) {
      setDryRunning(false);
      setDryRunError(
        typeof err === "object" && err !== null && "message" in err
          ? String(err.message)
          : String(err),
      );
    }
  };

  return (
    <aside
      role="region"
      aria-label="Resource details"
      className={screen ? "resource-drawer resource-drawer--screen" : "resource-drawer"}
      data-variant={variant}
      ref={asideRef}
      data-loading={loading ? "true" : undefined}
      tabIndex={-1}
      onKeyDown={(e) => {
        if (e.defaultPrevented) return;
        if (e.key !== "Escape") {
          handleActionKey(e);
          return;
        }
        // A modal opened on top of the drawer owns Escape.
        if (document.querySelector('[role="dialog"][aria-modal="true"]')) return;
        // Escape in the edited YAML only leaves the editor; a second Escape closes the drawer,
        // so an edit is never discarded by a single keypress.
        if (
          e.target instanceof HTMLElement &&
          (e.target.closest(".cm-editor") || e.target instanceof HTMLTextAreaElement) &&
          editedYaml !== null
        ) {
          e.preventDefault();
          e.stopPropagation();
          asideRef.current?.focus();
          return;
        }
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }}
    >
      {screen && (
        <div className="m-appbar resource-screen__appbar" data-testid="detail-appbar">
          <button
            type="button"
            className="m-appbar__btn"
            aria-label="Back"
            data-testid="detail-back"
            onClick={onClose}
          >
            <ArrowLeft size={20} strokeWidth={ICON_STROKE} aria-hidden="true" />
          </button>
          <div className="resource-screen__titles">
            <h2 className="resource-screen__name" title={row.name}>
              {row.name}
            </h2>
            <div className="resource-screen__meta" data-testid="drawer-meta">
              {metaLine}
            </div>
          </div>
        </div>
      )}
      <header className="resource-drawer__header" data-testid="drawer-header">
        {screen ? (
          row.statusReason ? (
            <span
              className={`pill ${STATUS_TONE[row.status]} resource-drawer__reason`}
              data-testid="drawer-status-reason"
              title={row.statusReason}
            >
              <span className="resource-drawer__reason-text">{row.statusReason}</span>
            </span>
          ) : null
        ) : (
        <>
        <div className="resource-drawer__title-row">
          <span className="resource-drawer__kind-icon">
            <KindIcon kind={row.kind} />
          </span>
          <h2 className="resource-drawer__title" title={row.name}>
            {row.name}
          </h2>
          {row.statusReason ? (
            <span
              className={`pill ${STATUS_TONE[row.status]} resource-drawer__reason`}
              data-testid="drawer-status-reason"
              title={row.statusReason}
            >
              <span className="resource-drawer__reason-text">{row.statusReason}</span>
            </span>
          ) : null}
          <button
            type="button"
            aria-label="Close drawer"
            title="Close (Esc)"
            className="resource-drawer__close"
            onClick={onClose}
          >
            <X size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
          </button>
        </div>
        <div className="resource-drawer__meta" data-testid="drawer-meta">
          {metaLine}
        </div>
        </>
        )}
        <div className="resource-drawer__actions" role="toolbar" aria-label="Resource actions">
          {headerActions.map((a) => {
            const Icon = a.icon;
            const disabled = a.disabledReason != null;
            return (
              <button
                key={a.id}
                type="button"
                className={a.danger ? "btn sm danger resource-drawer__action--end" : "btn sm"}
                data-testid={`drawer-action-${a.id}`}
                disabled={disabled}
                {...(disabled && a.disabledReason ? { title: a.disabledReason } : {})}
                {...(a.kbd && !screen
                  ? { "aria-keyshortcuts": a.kbd === "Ctrl+D" ? "Control+D" : a.kbd }
                  : {})}
                onClick={a.onClick}
              >
                <Icon size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
                {a.label}
                {a.kbd && !screen && <kbd aria-hidden="true">{a.kbd}</kbd>}
              </button>
            );
          })}
        </div>
      </header>

      <DebugContainerModal
        open={showDebug}
        onClose={() => {
          setShowDebug(false);
        }}
        row={row}
      />

      <NodeDebugModal
        open={showNodeDebug}
        onClose={() => {
          setShowNodeDebug(false);
        }}
        row={row}
        {...(execClient ? { execClient } : {})}
      />

      <div className="resource-drawer__tabs" role="tablist" aria-label="Resource detail tabs">
        {tabs.map((tab, index) => {
          const selected = activeDrawerTab === tab;
          const label = tab === "Secret" ? "Data" : tab;
          // The count badge is a description, so the tab's name stays "Events".
          const showCount = tab === "Events" && eventCount !== null;
          const accessibleName =
            tab === "Secret" ? "Secret Data" : showCount ? "Events" : undefined;
          return (
            <button
              key={tab}
              type="button"
              role="tab"
              id={tabId(tab)}
              aria-controls={panelId}
              aria-label={accessibleName}
              aria-selected={selected}
              {...(showCount ? { "aria-describedby": eventsCountId } : {})}
              tabIndex={selected ? 0 : -1}
              className="resource-drawer__tab"
              onClick={() => {
                setActiveTab(tab);
              }}
              onKeyDown={(e) => {
                onTabKeyDown(e, index);
              }}
            >
              {label}
              {showCount && (
                <span
                  id={eventsCountId}
                  className="resource-drawer__tab-count"
                  data-testid="events-count"
                  title={
                    eventsCapped
                      ? `Latest ${String(EVENT_LIMIT)} events loaded`
                      : `${String(eventCount)} events`
                  }
                >
                  {eventsCapped ? `${String(EVENT_LIMIT)}+` : eventCount}
                </span>
              )}
            </button>
          );
        })}
      </div>

      {loading && (
        <p role="status" className="resource-drawer__state" data-testid="drawer-loading">
          Loading {row.kind} {row.name}…
        </p>
      )}
      {error !== null && (
        <p role="alert" className="error-state resource-drawer__state" data-testid="drawer-error">
          Cannot load {row.kind} {row.name}: {error}
        </p>
      )}

      <div
        id={panelId}
        role="tabpanel"
        aria-labelledby={tabId(activeDrawerTab)}
        className={[
          "resource-drawer__body",
          activeDrawerTab === "Logs" && logLines === undefined && row.kind === "Pod"
            ? "resource-drawer__body--logs"
            : "",
          activeDrawerTab === "Terminal" ? "resource-drawer__body--terminal" : "",
        ]
          .filter(Boolean)
          .join(" ")}
      >
        {activeDrawerTab === "Summary" && (
          isPod && detail?.pod ? (
            <PodSummary
              detail={detail}
              row={row}
              onOpenRelated={handleOpenRelated}
              onSelectTab={handleSelectTab}
              portForwardContainer={pfContainer}
              remoteOps={remoteOps}
              onOpenInBrowser={onOpenInBrowser}
              onPortForward={(containerName) => {
                setSelectedContainer(containerName);
                setPfContainer((prev) => (prev === containerName ? null : containerName));
              }}
            />
          ) : (
            <div className="resource-drawer__summary" data-testid="resource-summary">
              <div className="card" data-testid="summary-overview-card">
                <h4>OVERVIEW</h4>
                <div className="body">
                  <dl className="kv">
                    <dt>Name</dt>
                    <dd title={row.name}>{row.name}</dd>
                    <dt>Namespace</dt>
                    <dd>{row.namespace ?? "—"}</dd>
                    <dt>Cluster</dt>
                    <dd title={row.cluster}>{row.cluster}</dd>
                    <dt>Status</dt>
                    <dd title={row.statusReason}>{row.statusReason || "—"}</dd>
                  </dl>
                  {isSecret && (
                    <button
                      type="button"
                      className="btn sm resource-drawer__summary-action"
                      data-testid="open-secret-editor-btn"
                      onClick={() => {
                        setActiveTab("Secret");
                      }}
                    >
                      <KindIcon kind="Secret" />
                      View / Edit Secret
                    </button>
                  )}
                </div>
              </div>
              {endpoints.length > 0 && (
                <div className="card" data-testid="summary-endpoints-card">
                  <h4>
                    ENDPOINTS
                    <span className="r">
                      {detail?.endpointsTruncated ? `first ${String(endpoints.length)}` : endpoints.length}
                    </span>
                  </h4>
                  <div className="body endpoints-card__list">
                    {endpoints.map((ep, idx) => (
                      <div
                        key={ep.url}
                        className="endpoints-card__item"
                        data-testid={`endpoint-item-${String(idx)}`}
                        title={ep.source}
                      >
                        <code className="endpoints-card__url">{ep.url}</code>
                        <div className="endpoints-card__actions">
                          {ep.openable ? (
                            <>
                              <OpenInBrowserButton url={ep.url} onOpen={onOpenInBrowser} />
                              <CopyUrlButton url={ep.url} showUrl={false} />
                            </>
                          ) : (
                            <span
                              className="resource-drawer__empty"
                              data-testid={`endpoint-not-openable-${String(idx)}`}
                            >
                              Wildcard or non-hostname: cannot open
                            </span>
                          )}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              )}
              {detail?.certificates && detail.certificates.length > 0 && (
                <div className="card" data-testid="summary-certificates-card">
                  <h4>
                    CERTIFICATES<span className="r">{detail.certificates.length}</span>
                  </h4>
                  <div className="body cert-card__list">
                    {detail.certificates.map((cert, idx) => (
                      <div key={idx} className="cert-card__item" data-testid={`cert-item-${String(idx)}`}>
                        <div className="cert-card__header">
                          <span className="cert-card__source">{cert.source}</span>
                          <span className={`pill ${cert.validityTone}`} data-testid={`cert-validity-${String(idx)}`}>
                            {cert.validityLabel}
                          </span>
                        </div>
                        <dl className="kv cert-card__kv">
                          <dt>Subject</dt>
                          <dd className="mono" title={cert.subject}>{cert.subject || "—"}</dd>
                          <dt>Issuer</dt>
                          <dd className="mono" title={cert.issuer}>{cert.issuer || "—"}</dd>
                          <dt>SANs</dt>
                          <dd>
                            {cert.sans.length > 0 ? (
                              <div className="cert-card__sans">
                                {cert.sans.map((san) => (
                                  <span key={san} className="chip sm mono">{san}</span>
                                ))}
                              </div>
                            ) : (
                              "—"
                            )}
                          </dd>
                          <dt>Validity</dt>
                          <dd title={`Valid from ${cert.notBefore} to ${cert.notAfter}`}>
                            {cert.notBefore} &rarr; {cert.notAfter}
                          </dd>
                          <dt>Serial</dt>
                          <dd className="mono" title={cert.serialNumber}>{cert.serialNumber || "—"}</dd>
                          <dt>Algorithm</dt>
                          <dd className="mono">{cert.signatureAlgorithm || "—"}</dd>
                        </dl>
                      </div>
                    ))}
                  </div>
                </div>
              )}
              {rbacInfo?.roleRef && (
                <div className="card" data-testid="summary-rbac-role-card">
                  <h4>ROLE REFERENCE</h4>
                  <div className="body resource-drawer__chips">
                    <button
                      type="button"
                      className="chip link"
                      data-testid="rbac-role-ref-btn"
                      onClick={() => {
                        const targetRole = rbacInfo.roleRef;
                        if (!targetRole) return;
                        handleOpenRelated({
                          kind: targetRole.kind,
                          name: targetRole.name,
                          namespace:
                            targetRole.kind === "Role"
                              ? (row.namespace ?? null)
                              : null,
                          uid: "",
                          relationship: "roleRef",
                        });
                      }}
                    >
                      <KindIcon kind={rbacInfo.roleRef.kind} />
                      {`${rbacInfo.roleRef.kind}/${rbacInfo.roleRef.name}`}
                    </button>
                  </div>
                </div>
              )}
              {rbacInfo?.subjects && rbacInfo.subjects.length > 0 && (
                <div className="card" data-testid="summary-rbac-subjects-card">
                  <h4>
                    SUBJECTS<span className="r">{rbacInfo.subjects.length}</span>
                  </h4>
                  <div className="body resource-drawer__chips">
                    {rbacInfo.subjects.map((sub, i) => (
                      <button
                        key={`${sub.kind}-${sub.name}-${String(i)}`}
                        type="button"
                        className={`chip ${sub.kind === "ServiceAccount" ? "link" : ""}`}
                        data-testid={`rbac-subject-${String(i)}`}
                        onClick={() => {
                          if (sub.kind === "ServiceAccount") {
                            handleOpenRelated({
                              kind: "ServiceAccount",
                              name: sub.name,
                              namespace: sub.namespace ?? row.namespace ?? null,
                              uid: "",
                              relationship: "subject",
                            });
                          }
                        }}
                      >
                        <KindIcon kind={sub.kind} />
                        {`${sub.kind}/${sub.name}${sub.namespace ? ` (${sub.namespace})` : ""}`}
                      </button>
                    ))}
                  </div>
                </div>
              )}
              {rbacInfo?.rules && rbacInfo.rules.length > 0 && (
                <div className="card" data-testid="summary-rbac-rules-card">
                  <h4>
                    RULES<span className="r">{rbacInfo.rules.length}</span>
                  </h4>
                  <div className="body">
                    <table className="resource-drawer__rules-table">
                      <thead>
                        <tr>
                          <th>Resources</th>
                          <th>Verbs</th>
                          <th>API Groups</th>
                        </tr>
                      </thead>
                      <tbody>
                        {rbacInfo.rules.map((rule, idx) => (
                          <tr key={idx} data-testid={`rbac-rule-row-${String(idx)}`}>
                            <td className="mono">{rule.resources.join(", ") || "*"}</td>
                            <td>{rule.verbs.join(", ")}</td>
                            <td className="mono">
                              {rule.apiGroups.map((g) => (g === "" ? '"" (core)' : g)).join(", ") || "*"}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </div>
              )}
              {detail?.related && detail.related.length > 0 && (
                <div className="card" data-testid="summary-related-card">
                  <h4>
                    OWNER REFERENCES<span className="r">{detail.related.length}</span>
                  </h4>
                  <div className="body resource-drawer__chips" data-testid="owner-references">
                    {detail.related.map((ref) => (
                      <span key={ref.uid || `${ref.kind}/${ref.name}`} className="chip">
                        <KindIcon kind={ref.kind} />
                        {`${ref.kind}/${ref.name}`}
                      </span>
                    ))}
                  </div>
                </div>
              )}
              <div className="card" data-testid="summary-labels-card">
                <h4>LABELS</h4>
                <div className="body resource-drawer__chips">
                  {row.labels.length === 0 ? (
                    <span className="resource-drawer__empty">None</span>
                  ) : (
                    row.labels.map(([k, v]) => (
                      <span key={`${k}=${v}`} className="chip">
                        {`${k}=${v}`}
                      </span>
                    ))
                  )}
                </div>
              </div>
              {isPod && remoteOps && row.namespace && (
                <div className="card" data-testid="summary-port-forward-card">
                  <h4>PORT-FORWARD</h4>
                  <div className="body">
                    <ActivePortForwards
                      context={row.cluster}
                      namespace={row.namespace}
                      pod={row.name}
                      onOpenInBrowser={onOpenInBrowser}
                    />
                    <PortForwardForm
                      context={row.cluster}
                      namespace={row.namespace}
                      pod={row.name}
                      yaml={originalYaml}
                    />
                  </div>
                </div>
              )}
            </div>
          )
        )}

        {activeDrawerTab === "Secret" && isSecret && (
          <div className="resource-drawer__secret-body" data-testid="drawer-secret-editor">
            <SecretEditor
              name={row.name}
              {...(row.namespace ? { namespace: row.namespace } : {})}
              secret={{
                metadata: {
                  name: row.name,
                  ...(row.namespace ? { namespace: row.namespace } : {}),
                },
                data: parsedSecret.data,
                stringData: parsedSecret.stringData,
              }}
              data={parsedSecret.data}
              readOnly={isReadOnly}
              {...(decodeSecret ? { decode: decodeSecret } : {})}
              {...(encodeSecret ? { encode: encodeSecret } : {})}
              onSave={(_decoded, base64) => {
                const updated = updateYamlWithSecretData(currentYaml, base64);
                setEditedYaml(updated);
              }}
            />
          </div>
        )}

        {activeDrawerTab === "YAML" && (
          <div className="resource-drawer__yaml">
            <div className="resource-drawer__yaml-toolbar">
              {!detailMissing && !!currentYaml && (
                <button
                  type="button"
                  className="resource-drawer__btn"
                  disabled={dryRunning}
                  data-testid="yaml-apply-btn"
                  aria-label={onDryRunApply ? "Dry-run & Diff" : "Review & Apply"}
                  onClick={() => {
                    if (onDryRunApply) {
                      void handleDryRun();
                    } else {
                      void handleReviewAndApply();
                    }
                  }}
                >
                  {onDryRunApply
                    ? "Dry-run & Diff"
                    : dryRunning
                      ? "Dry-running…"
                      : "Review & Apply"}
                </button>
              )}
            </div>
            {dryRunError && (
              <div
                role="alert"
                className="error-state resource-drawer__state"
                data-testid="yaml-apply-error"
              >
                {dryRunError}
              </div>
            )}
            {applySuccess && (
              <div
                role="status"
                className="resource-drawer__status-success resource-drawer__state"
                data-testid="apply-success-status"
              >
                {applySuccess}
              </div>
            )}
            {detailMissing ? (
              <p className={error !== null ? "error-state" : "empty-state"}>
                {error !== null
                  ? "YAML unavailable: the resource could not be loaded."
                  : "Loading YAML…"}
              </p>
            ) : (
              <YamlEditor
                aria-label="Resource YAML"
                className="resource-drawer__yaml-editor"
                value={currentYaml}
                readOnly={isReadOnly}
                onChange={(value) => {
                  setEditedYaml(value);
                  setDryRunError(null);
                  setApplySuccess(null);
                }}
              />
            )}
            {dryRunResult && (
              <div className="resource-drawer__dryrun">
                <pre
                  data-testid="yaml-kubectl-preview"
                  className="resource-drawer__kubectl-preview"
                >
                  {dryRunResult.kubectlPreview}
                </pre>
                <pre data-testid="yaml-diff-preview" className="resource-drawer__diff-preview">
                  {dryRunResult.diff}
                </pre>
              </div>
            )}
          </div>
        )}

        {activeDrawerTab === "Logs" && logLines !== undefined && (
          <div className="resource-drawer__logs">
            <div className="resource-drawer__logs-toolbar">
              <input
                type="search"
                aria-label="Search logs (regex)"
                placeholder="Regex filter…"
                className="resource-drawer__logs-search"
                value={logRegex}
                onChange={(e) => {
                  setLogRegex(e.target.value);
                }}
              />
              <label className="resource-drawer__toggle">
                <input
                  type="checkbox"
                  aria-label="Follow logs"
                  checked={followLogs}
                  onChange={(e) => {
                    setFollowLogs(e.target.checked);
                  }}
                />
                <span>Follow</span>
              </label>
              <label className="resource-drawer__toggle">
                <input
                  type="checkbox"
                  aria-label="Wrap log lines"
                  checked={wrapLines}
                  onChange={(e) => {
                    setWrapLines(e.target.checked);
                  }}
                />
                <span>Wrap lines</span>
              </label>
              <label className="resource-drawer__toggle">
                <input
                  type="checkbox"
                  aria-label="Previous container logs"
                  checked={previousContainer}
                  onChange={(e) => {
                    setPreviousContainer(e.target.checked);
                  }}
                />
                <span>Previous container</span>
              </label>
            </div>
            <div
              data-testid="log-lines"
              data-follow={followLogs ? "true" : "false"}
              data-wrap={wrapLines ? "true" : "false"}
              data-previous={previousContainer ? "true" : "false"}
              className={[
                "resource-drawer__log-lines",
                wrapLines ? "resource-drawer__log-lines--wrap" : "",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              {filteredLogs.map((line) => (
                <div key={line.seq} className="resource-drawer__log-line">
                  {line.text}
                </div>
              ))}
            </div>
          </div>
        )}

        {activeDrawerTab === "Logs" && logLines === undefined && (
          row.kind === "Pod" ? (
            <div className="resource-drawer__logs resource-drawer__logs--workload">
              <WorkloadLogsView
                key={`${clusterKey}-${row.namespace ?? "default"}-${row.name}-${selectedContainer ?? ""}`}
                target={{
                  cluster: row.cluster,
                  namespace: row.namespace ?? "default",
                  selector: { type: "pod", name: row.name },
                  container: selectedContainer ?? null,
                  tailLines: 500,
                }}
                phone={screen}
                notRunning={containerNotRunning(row, detail, selectedContainer)}
              />
            </div>
          ) : (
            <div className="resource-drawer__logs">
              <p className="empty-state" data-testid="logs-unavailable">
                Logs are available for Pods and Workloads.
              </p>
            </div>
          )
        )}

        {activeDrawerTab === "Terminal" && (
          isPod ? (
            <ExecShellPanel
              key={`${clusterKey}-${row.namespace ?? "default"}-${row.name}-${selectedContainer ?? ""}-${terminalMode}`}
              pod={row.name}
              cluster={clusterKey}
              namespace={row.namespace ?? "default"}
              containers={
                selectedContainer && availableContainers.includes(selectedContainer)
                  ? [
                      selectedContainer,
                      ...availableContainers.filter((c) => c !== selectedContainer),
                    ]
                  : availableContainers
              }
              readOnly={isReadOnly}
              mode={terminalMode}
              {...(execClient ? { execClient } : {})}
            />
          ) : (
            <div className="resource-drawer__terminal-empty">
              <p className="empty-state" data-testid="terminal-unavailable">
                Exec is only available for Pods.
              </p>
            </div>
          )
        )}

        {activeDrawerTab === "Events" && (
          <div className="resource-drawer__events">
            {detailMissing ? (
              <p className={error !== null ? "error-state" : "empty-state"}>
                {error !== null
                  ? "Events unavailable: the resource could not be loaded."
                  : "Loading events…"}
              </p>
            ) : detail === null ? (
              <p className="empty-state">Events are not loaded in this view.</p>
            ) : detail.events && detail.events.length > 0 ? (
              <>
                {eventsTruncated && (
                  <p className="resource-drawer__state" data-testid="events-limit">
                    Showing latest {EVENT_LIMIT} events
                  </p>
                )}
                <ul className="resource-drawer__events-list" data-testid="events-list">
                  {detail.events.map((ev, idx) => {
                    const info = extractEventInfo(ev);
                    const key = ev.uid || `${info.timestamp}-${info.reason}-${String(idx)}`;
                    return (
                      <li
                        key={key}
                        className="resource-drawer__event-item"
                        data-testid="event-item"
                      >
                        <div className="resource-drawer__event-header">
                          {info.timestamp && (
                            <span className="resource-drawer__event-timestamp">
                              {info.timestamp}
                            </span>
                          )}
                          <span
                            className={`badge ${info.type === "Warning" ? "badge--warning" : ""}`}
                          >
                            {info.type}
                          </span>
                          <span className="resource-drawer__event-reason">{info.reason}</span>
                        </div>
                        {info.message && (
                          <div className="resource-drawer__event-message">{info.message}</div>
                        )}
                      </li>
                    );
                  })}
                </ul>
              </>
            ) : (
              <p className="empty-state">No events recorded for {row.name}.</p>
            )}
          </div>
        )}
      </div>

      {diffModalOpen && (
        <DiffModal
          open={diffModalOpen}
          applyMode={true}
          context={clusterKey}
          resourceName={row.name}
          resourceKind={row.kind}
          resourceNamespace={row.namespace ?? null}
          readOnly={isReadOnly}
          isProtected={
            clusterKey && settingsByContext[clusterKey]
              ? settingsByContext[clusterKey].protected
              : undefined
          }
          left={{
            ...(clusterKey ? { context: clusterKey } : {}),
            ...(row.namespace ? { namespace: row.namespace } : {}),
            name: row.name,
            manifest: originalYaml,
          }}
          right={{
            ...(clusterKey ? { context: clusterKey } : {}),
            ...(row.namespace ? { namespace: row.namespace } : {}),
            name: row.name,
            manifest: currentYaml,
          }}
          onClose={() => {
            setDiffModalOpen(false);
          }}
          onApplySuccess={(newYaml) => {
            setDiffModalOpen(false);
            setEditedYaml(newYaml);
            setDryRunError(null);
            const targetDesc = row.namespace ? `${row.namespace}/${row.name}` : row.name;
            setApplySuccess(`Applied changes to ${row.kind} ${targetDesc} on ${clusterKey}`);
            const curState = useResourceDetailStore.getState();
            if (curState.target?.uid === row.uid && curState.detail) {
              useResourceDetailStore.setState({
                detail: { ...curState.detail, yaml: newYaml },
              });
            }
          }}
        />
      )}

      {crossCompareTarget && (
        <DiffModal
          open={true}
          left={{
            context: crossCompareTarget.cluster,
            namespace: crossCompareTarget.namespace ?? undefined,
            name: crossCompareTarget.name,
            kind: crossCompareTarget.kind,
            manifest: crossCompareTarget.manifest,
          }}
          right={{
            context: clusterKey,
            namespace: row.namespace ?? undefined,
            name: row.name,
            kind: row.kind,
            manifest: originalYaml || undefined,
          }}
          resourceKind={row.kind}
          onClose={() => {
            setCrossCompareTarget(null);
          }}
        />
      )}
    </aside>
  );
}
