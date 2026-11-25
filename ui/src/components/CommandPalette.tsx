import { Command } from "cmdk";
import { useEffect, useState } from "react";
import { commands } from "../generated/bindings";
import { ContextUnlockModal } from "./StatusBar";
import { focusPane } from "./paneFocus";
import {
  canRestart,
  canScale,
  deleteActionFor,
  restartActionFor,
  scaleActionFor,
  useActionsStore,
  type PendingAction,
} from "../stores/actions";
import { createGoChordHandler, GO_CHORDS } from "../lib/goChords";
import { useContextsStore } from "../stores/contexts";
import { useIsAndroid } from "../stores/appInfo";
import { AUDIT_TAB, IMPORT_TAB, OVERVIEW_TAB, useLayoutStore } from "../stores/layout";
import { useSettingsStore } from "../stores/settings";
import { useCompareStore } from "../stores/compare";

if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class ResizeObserver {
    observe() {
      // no-op in jsdom
    }
    unobserve() {
      // no-op in jsdom
    }
    disconnect() {
      // no-op in jsdom
    }
  };
}

if (typeof Element !== "undefined" && typeof Element.prototype.scrollIntoView === "undefined") {
  Element.prototype.scrollIntoView = () => {
    // no-op in jsdom
  };
}

interface ResourceOption {
  label: string;
  kind: string;
}

const RESOURCES: readonly ResourceOption[] = [
  { label: "Pods", kind: "Pod" },
  { label: "Deployments", kind: "Deployment" },
  { label: "Jobs", kind: "Job" },
  { label: "Services", kind: "Service" },
  { label: "Nodes", kind: "Node" },
  { label: "Namespaces", kind: "Namespace" },
  { label: "ConfigMaps", kind: "ConfigMap" },
  { label: "Secrets", kind: "Secret" },
];

interface EcosystemOption {
  actionLabel: string;
  title: string;
  kind: string;
  badge: string;
  keywords?: string[];
}

const ECOSYSTEM_ACTIONS: readonly EcosystemOption[] = [
  {
    actionLabel: "Open Gateways",
    title: "Gateways",
    kind: "Gateway",
    badge: "Gateway API",
    keywords: ["gateway", "gateways", "gateway-api", "networking"],
  },
  {
    actionLabel: "Open HTTPRoutes",
    title: "HTTPRoutes",
    kind: "HTTPRoute",
    badge: "Gateway API",
    keywords: ["httproute", "httproutes", "routes", "gateway-api", "networking"],
  },
  {
    actionLabel: "Open Argo Applications",
    title: "Argo Apps",
    kind: "Application",
    badge: "Argo CD",
    keywords: ["argo", "argocd", "application", "applications", "apps", "gitops"],
  },
  {
    actionLabel: "Open Flux Kustomizations",
    title: "Flux Kustomize",
    kind: "Kustomization",
    badge: "Flux",
    keywords: ["flux", "fluxcd", "kustomization", "kustomize", "gitops"],
  },
  {
    actionLabel: "Open Karpenter NodePools",
    title: "Karpenter Pools",
    kind: "NodePool",
    badge: "Karpenter",
    keywords: ["karpenter", "nodepool", "nodepools", "pools", "autoscaling", "nodes"],
  },
];

// Like PodTable's modal guard, limited to the action confirm dialog (R-07): a pending
// action, or its aria-modal dialog still in the DOM.
function isActionDialogOpen(): boolean {
  if (useActionsStore.getState().pending !== null) return true;
  return document.querySelector('.action-confirm-dialog[role="dialog"][aria-modal="true"]') !== null;
}

export function CommandPalette() {
  const paletteOpen = useLayoutStore((s) => s.paletteOpen);
  const closePalette = useLayoutStore((s) => s.closePalette);
  const togglePalette = useLayoutStore((s) => s.togglePalette);
  const openTab = useLayoutStore((s) => s.openTab);
  const showKind = useLayoutStore((s) => s.showKind);
  const toggleSidebar = useLayoutStore((s) => s.toggleSidebar);
  const goBack = useLayoutStore((s) => s.goBack);
  const goForward = useLayoutStore((s) => s.goForward);
  const toggleSplit = useLayoutStore((s) => s.toggleSplit);
  const toggleSplitOrientation = useLayoutStore((s) => s.toggleSplitOrientation);
  const setSplitMode = useLayoutStore((s) => s.setSplitMode);
  const compactRows = useSettingsStore((s) => s.density === "compact");
  const toggleDensity = useSettingsStore((s) => s.toggleDensity);
  const theme = useSettingsStore((s) => s.theme);
  const setTheme = useSettingsStore((s) => s.setTheme);

  const isAndroid = useIsAndroid();
  const contexts = useContextsStore((s) => s.contexts);
  const selectContext = useContextsStore((s) => s.selectContext);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const unlockContext = useContextsStore((s) => s.unlockContext);
  const relockContext = useContextsStore((s) => s.relockContext);
  const reloadTeamSettings = useContextsStore((s) => s.reloadTeamSettings);
  const selection = useActionsStore((s) => s.selection);
  const selectedForCompare = useCompareStore((s) => s.selectedForCompare);
  const [unlockPromptContext, setUnlockPromptContext] = useState<{
    name: string;
    isProtected: boolean;
  } | null>(null);
  const [nowSecs] = useState(() => Math.floor(Date.now() / 1000));
  // While an action is pending (or running) no other action can be started from here.
  const actionPending = useActionsStore((s) => s.pending !== null);

  const runAction = (pending: PendingAction | null) => {
    closePalette();
    // `request` refuses while another action is pending or running.
    // The dialog opens in the pane the selection came from (the active one).
    const pane = useActionsStore.getState().selectionPane ?? useLayoutStore.getState().activePane;
    if (pending) useActionsStore.getState().request(pending, pane);
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
        e.preventDefault();
        // An action confirm dialog owns the keyboard; the palette waits until it closes.
        if (!useLayoutStore.getState().paletteOpen && isActionDialogOpen()) return;
        togglePalette();
        return;
      }
      if (e.key === "Escape" && useLayoutStore.getState().paletteOpen) {
        e.preventDefault();
        closePalette();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [togglePalette, closePalette]);

  // Go-to chords (`g p`, `g d`, ...). A lone `g` is left alone so the table keeps "g = top".
  useEffect(() => {
    const onChord = createGoChordHandler((chord) => {
      const layout = useLayoutStore.getState();
      if (chord.view === "helm") {
        layout.openTab({ id: "view-helm", title: "Helm releases", view: "helm" });
      } else if (chord.kind) {
        layout.showKind(chord.label, chord.kind);
      }
    });
    // Capture phase: runs before PodTable's handlers, which skip prevented events, so the
    // second key of a chord (e.g. `s` in `g s`) never also triggers a table action.
    window.addEventListener("keydown", onChord, true);
    return () => {
      window.removeEventListener("keydown", onChord, true);
    };
  }, []);

  if (!paletteOpen) {
    if (unlockPromptContext) {
      return (
        <ContextUnlockModal
          context={unlockPromptContext.name}
          isProtected={unlockPromptContext.isProtected}
          onClose={() => {
            setUnlockPromptContext(null);
          }}
          onUnlock={(ctx, confirmed) => {
            void unlockContext(ctx, confirmed);
          }}
        />
      );
    }
    return null;
  }

  return (
    <div
      className="cmdk-backdrop"
      onClick={() => {
        closePalette();
      }}
    >
      <div
        role="dialog"
        aria-label="Command palette"
        aria-modal="true"
        className="cmdk-dialog"
        onClick={(e) => {
          e.stopPropagation();
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            closePalette();
          }
        }}
      >
        <Command label="Command palette" className="cmdk-root">
          <Command.Input
            autoFocus
            className="cmdk-input"
            aria-label="Command palette search"
            placeholder="Type a command, resource, or context…"
          />
          <Command.List className="cmdk-list">
            <Command.Empty className="cmdk-empty">No matching commands.</Command.Empty>
            {selection && !actionPending && (
              <Command.Group heading={`Actions: ${selection.kind} ${selection.name}`} className="cmdk-group">
                <Command.Item
                  value={`Delete ${selection.kind} ${selection.name}`}
                  keywords={["delete", "remove", "ctrl+d", "action"]}
                  className="cmdk-item"
                  onSelect={() => {
                    runAction(deleteActionFor(selection));
                  }}
                >
                  Delete {selection.kind.toLowerCase()} {selection.name}
                  <kbd className="cmdk-item__shortcut">ctrl+d</kbd>
                </Command.Item>
                {canScale(selection) && (
                  <Command.Item
                    value={`Scale ${selection.kind} ${selection.name}`}
                    keywords={["scale", "replicas", "action"]}
                    className="cmdk-item"
                    onSelect={() => {
                      runAction(scaleActionFor(selection));
                    }}
                  >
                    Scale {selection.kind.toLowerCase()} {selection.name}
                    <kbd className="cmdk-item__shortcut">s</kbd>
                  </Command.Item>
                )}
                {canRestart(selection) && (
                  <Command.Item
                    value={`Restart ${selection.kind} ${selection.name}`}
                    keywords={["restart", "rollout", "action"]}
                    className="cmdk-item"
                    onSelect={() => {
                      runAction(restartActionFor(selection));
                    }}
                  >
                    Restart {selection.kind.toLowerCase()} {selection.name}
                    <kbd className="cmdk-item__shortcut">r</kbd>
                  </Command.Item>
                )}
                <Command.Item
                  value={`Select ${selection.kind} ${selection.name} for compare`}
                  keywords={["compare", "diff", "select", "action"]}
                  className="cmdk-item"
                  onSelect={() => {
                    useCompareStore.getState().selectForCompare({
                      cluster: selection.cluster,
                      kind: selection.kind,
                      namespace: selection.namespace,
                      name: selection.name,
                      uid: selection.uid,
                    });
                    closePalette();
                  }}
                >
                  Select {selection.kind.toLowerCase()} {selection.name} for compare
                </Command.Item>
                {selectedForCompare && selectedForCompare.uid !== selection.uid && (
                  <Command.Item
                    value={`Compare with ${selectedForCompare.name}`}
                    keywords={["compare", "diff", "action"]}
                    className="cmdk-item"
                    onSelect={() => {
                      useCompareStore.getState().openComparison(selectedForCompare, {
                        cluster: selection.cluster,
                        kind: selection.kind,
                        namespace: selection.namespace,
                        name: selection.name,
                        uid: selection.uid,
                      });
                      closePalette();
                    }}
                  >
                    Compare with {selectedForCompare.name} ({selectedForCompare.cluster})
                  </Command.Item>
                )}
              </Command.Group>
            )}
            <Command.Group heading="Resources" className="cmdk-group">
              {RESOURCES.map((res) => (
                <Command.Item
                  key={res.kind}
                  value={res.label}
                  keywords={[res.kind, "resource"]}
                  className="cmdk-item"
                  onSelect={() => {
                    showKind(res.label, res.kind);
                    closePalette();
                  }}
                >
                  {res.label}
                </Command.Item>
              ))}
              {ECOSYSTEM_ACTIONS.map((action) => (
                <Command.Item
                  key={action.kind}
                  value={action.actionLabel}
                  keywords={[action.title, action.kind, action.badge, ...(action.keywords ?? []), "resource", "ecosystem"]}
                  className="cmdk-item"
                  onSelect={() => {
                    showKind(action.title, action.kind);
                    closePalette();
                  }}
                >
                  {action.actionLabel}
                </Command.Item>
              ))}
            </Command.Group>
            <Command.Group heading="Views" className="cmdk-group">
              <Command.Item
                value="Go to Overview"
                keywords={["overview", "home", "dashboard", "clusters", "attention", "view"]}
                className="cmdk-item"
                onSelect={() => {
                  openTab({ ...OVERVIEW_TAB });
                  closePalette();
                }}
              >
                Go to Overview
              </Command.Item>
              {isAndroid && (
                <Command.Item
                  value="Import contexts"
                  keywords={["import", "kubeconfig", "token", "add", "context", "view"]}
                  className="cmdk-item"
                  onSelect={() => {
                    openTab({ ...IMPORT_TAB });
                    closePalette();
                  }}
                >
                  Import contexts
                </Command.Item>
              )}
              <Command.Item
                value="Open Apps view"
                keywords={["applications", "apps", "view"]}
                className="cmdk-item"
                onSelect={() => {
                  // Fixed id: re-selecting focuses the existing Apps tab instead of adding another.
                  openTab({ id: "view-apps", title: "Applications", view: "apps" });
                  closePalette();
                }}
              >
                Open Apps view
              </Command.Item>
              <Command.Item
                value="Open Helm releases"
                keywords={["helm", "charts", "releases", "view"]}
                className="cmdk-item"
                onSelect={() => {
                  openTab({ id: "view-helm", title: "Helm releases", view: "helm" });
                  closePalette();
                }}
              >
                Open Helm releases
              </Command.Item>
              <Command.Item
                value="Open audit log"
                keywords={["audit", "log", "history", "actions", "view"]}
                className="cmdk-item"
                onSelect={() => {
                  openTab({ ...AUDIT_TAB });
                  closePalette();
                }}
              >
                Open audit log
              </Command.Item>
              <Command.Item
                value="Open Plugins view"
                keywords={["plugins", "extensions", "sandbox", "view"]}
                className="cmdk-item"
                onSelect={() => {
                  openTab({ id: "view-plugins", title: "Plugins", view: "plugins" });
                  closePalette();
                }}
              >
                Open Plugins view
              </Command.Item>
              <Command.Item
                value="Toggle split pane"
                keywords={["split", "pane", "ctrl+\\"]}
                className="cmdk-item"
                onSelect={() => {
                  toggleSplit();
                  closePalette();
                }}
              >
                Toggle split pane
              </Command.Item>
              <Command.Item
                value="Switch split orientation"
                keywords={["split", "orientation", "horizontal", "vertical", "stacked"]}
                className="cmdk-item"
                onSelect={() => {
                  // With no split open, open one stacked: the non-default orientation.
                  if (useLayoutStore.getState().splitMode === "none") setSplitMode("vertical");
                  else toggleSplitOrientation();
                  closePalette();
                }}
              >
                Switch split orientation
              </Command.Item>
              <Command.Item
                value="Switch split pane focus"
                keywords={["split", "pane", "focus", "switch", "ctrl+w"]}
                className="cmdk-item"
                onSelect={() => {
                  const { splitMode, switchPane } = useLayoutStore.getState();
                  if (splitMode === "none") {
                    useLayoutStore.getState().setSplitMode("horizontal");
                  }
                  switchPane();
                  closePalette();
                  focusPane(useLayoutStore.getState().activePane);
                }}
              >
                Switch split pane focus
              </Command.Item>
            </Command.Group>
            {contexts.length > 0 && (
              <Command.Group heading="Contexts" className="cmdk-group">
                {contexts.map((ctx) => (
                  <Command.Item
                    key={ctx.name}
                    value={`context:${ctx.name}`}
                    keywords={[ctx.name, ctx.cluster, "context"]}
                    className="cmdk-item"
                    onSelect={() => {
                      void selectContext(ctx.name);
                      closePalette();
                    }}
                  >
                    {ctx.name}
                  </Command.Item>
                ))}
              </Command.Group>
            )}
            {selectedContext && (
              <Command.Group heading="Terminal & External Handoff" className="cmdk-group">
                <Command.Item
                  value={`Open context shell in external terminal (${selectedContext})`}
                  keywords={["terminal", "external", "shell", "exec", "tmux", "context", selectedContext]}
                  className="cmdk-item"
                  onSelect={() => {
                    void commands.openExternalTerminal({
                      target: { kind: "contextShell", cluster: selectedContext, namespace: null },
                      confirmedName: null,
                    });
                    closePalette();
                  }}
                >
                  Open context shell in external terminal ({selectedContext})
                </Command.Item>
              </Command.Group>
            )}
            {contexts.length > 0 && (
              <Command.Group heading="Context Safety & Timed Unlock" className="cmdk-group">
                {contexts.map((ctx) => {
                  const s = settingsByContext[ctx.name];
                  const isUnlocked = s?.unlockedUntil != null && s.unlockedUntil > nowSecs;
                  if (isUnlocked) {
                    return (
                      <Command.Item
                        key={`relock-${ctx.name}`}
                        value={`Lock context ${ctx.name}`}
                        keywords={["lock", "read-only", "protect", ctx.name]}
                        className="cmdk-item"
                        onSelect={() => {
                          void relockContext(ctx.name);
                          closePalette();
                        }}
                      >
                        Lock context {ctx.name} (currently unlocked)
                      </Command.Item>
                    );
                  }
                  if (s?.readOnly) {
                    return (
                      <Command.Item
                        key={`unlock-${ctx.name}`}
                        value={`Unlock context ${ctx.name} for 15 minutes`}
                        keywords={["unlock", "write", "permit", "15 minutes", ctx.name]}
                        className="cmdk-item"
                        onSelect={() => {
                          setUnlockPromptContext({ name: ctx.name, isProtected: s.protected });
                          closePalette();
                        }}
                      >
                        Unlock context {ctx.name} for 15 minutes
                      </Command.Item>
                    );
                  }
                  return null;
                })}
              </Command.Group>
            )}
            <Command.Group heading="Navigation" className="cmdk-group">
              <Command.Item
                value="New Tab"
                className="cmdk-item"
                onSelect={() => {
                  openTab({ title: "Pods", kind: "Pod" });
                  closePalette();
                }}
              >
                New Tab
              </Command.Item>
              <Command.Item
                value="Toggle Sidebar"
                className="cmdk-item"
                onSelect={() => {
                  toggleSidebar();
                  closePalette();
                }}
              >
                Toggle Sidebar
              </Command.Item>
              <Command.Item
                value="Navigate Back"
                className="cmdk-item"
                onSelect={() => {
                  goBack();
                  closePalette();
                }}
              >
                Navigate Back
              </Command.Item>
              <Command.Item
                value="Navigate Forward"
                className="cmdk-item"
                onSelect={() => {
                  goForward();
                  closePalette();
                }}
              >
                Navigate Forward
              </Command.Item>
            </Command.Group>
            <Command.Group heading="Settings" className="cmdk-group">
              <Command.Item
                value="Toggle compact rows"
                keywords={["compact", "density", "rows", "row height", "dense", "settings"]}
                className="cmdk-item"
                onSelect={() => {
                  toggleDensity();
                  closePalette();
                }}
              >
                Toggle compact rows
                <span className="cmdk-item__hint">{compactRows ? "on" : "off"}</span>
              </Command.Item>
              <Command.Item
                value="Theme: Dark"
                keywords={["theme", "dark", "color scheme", "mode", "settings"]}
                className="cmdk-item"
                onSelect={() => {
                  setTheme("dark");
                  closePalette();
                }}
              >
                Theme: Dark
                {theme === "dark" && <span className="cmdk-item__hint">active</span>}
              </Command.Item>
              <Command.Item
                value="Theme: Light"
                keywords={["theme", "light", "color scheme", "mode", "settings"]}
                className="cmdk-item"
                onSelect={() => {
                  setTheme("light");
                  closePalette();
                }}
              >
                Theme: Light
                {theme === "light" && <span className="cmdk-item__hint">active</span>}
              </Command.Item>
              <Command.Item
                value="Theme: Follow system"
                keywords={["theme", "system", "auto", "prefers-color-scheme", "color scheme", "mode", "settings"]}
                className="cmdk-item"
                onSelect={() => {
                  setTheme("system");
                  closePalette();
                }}
              >
                Theme: Follow system
                {theme === "system" && <span className="cmdk-item__hint">active</span>}
              </Command.Item>
              <Command.Item
                value="Reload team settings"
                keywords={["team", "settings", "reload", "yaml", "config"]}
                className="cmdk-item"
                onSelect={() => {
                  void reloadTeamSettings("team-settings.yaml");
                  closePalette();
                }}
              >
                Reload team settings
              </Command.Item>
              <Command.Item
                value="Plugins"
                keywords={["plugins", "extensions", "sandbox", "settings"]}
                className="cmdk-item"
                onSelect={() => {
                  openTab({ id: "view-plugins", title: "Plugins", view: "plugins" });
                  closePalette();
                }}
              >
                Plugins
              </Command.Item>
            </Command.Group>
          </Command.List>
        </Command>
        <div className="cmdk-chords" aria-label="Go-to shortcuts">
          {GO_CHORDS.map((c) => (
            <span key={c.key} className="cmdk-chord">
              <kbd>g</kbd> <kbd>{c.key}</kbd> {c.label}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}
