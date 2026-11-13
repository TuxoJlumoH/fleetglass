import { useEffect, type CSSProperties } from "react";
import { CommandPalette } from "./components/CommandPalette";
import { DiffModal } from "./components/DiffModal";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { TabsArea, type TabsAreaProps } from "./components/TabsArea";
import { useAppLifecycle } from "./lib/appLifecycle";
import { useIsAndroid } from "./stores/appInfo";
import { useCompareStore } from "./stores/compare";
import { useContextsStore } from "./stores/contexts";
import { useLayoutStore } from "./stores/layout";
import { MobileShell } from "./mobile/MobileShell";
import { useMobileLayout } from "./mobile/useMobileLayout";

export type AppProps = TabsAreaProps;

/** Phone layout on Android phones and narrow windows (03-03a); desktop layout otherwise. */
export function App(props: AppProps = {}) {
  const layout = useMobileLayout();
  // Android only: pause the backend's watchers and log streams while the app is hidden (03-04b).
  useAppLifecycle(useIsAndroid());
  // Startup work owned by the shell (contexts/app info are loaded in main.tsx).
  // No-op outside Tauri unless a cluster group client is injected. The status guard keeps
  // StrictMode's second effect run from issuing a duplicate request.
  useEffect(() => {
    const contexts = useContextsStore.getState();
    if (contexts.clusterGroupsStatus === "idle") void contexts.loadClusterGroups();
  }, []);
  // "pending": an Android phone in landscape is possible but `app_info` has not answered yet;
  // render nothing for that moment instead of flashing the desktop layout.
  if (layout === "pending") return null;
  return layout === "mobile" ? <MobileShell {...props} /> : <DesktopShell {...props} />;
}

function DesktopShell(props: AppProps) {
  const { fetchApps, fetchAuditLog, ...restProps } = props;
  const toggleSidebar = useLayoutStore((s) => s.toggleSidebar);
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const activeSettings = selectedContext ? settingsByContext[selectedContext] : undefined;
  const colour = activeSettings?.colour ?? null;
  const readOnly = activeSettings?.readOnly ?? false;

  const activeComparison = useCompareStore((s) => s.activeComparison);
  const closeComparison = useCompareStore((s) => s.closeComparison);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "b") {
        e.preventDefault();
        toggleSidebar();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [toggleSidebar]);

  return (
    <div
      className="shell app-shell"
      style={colour ? ({ "--context-accent": colour } as CSSProperties) : undefined}
    >
      {readOnly && (
        <div className="context-readonly-banner" data-testid="active-context-readonly">
          Read-only context ({selectedContext})
        </div>
      )}
      <Sidebar />
      <TabsArea
        {...(fetchApps ? { fetchApps } : {})}
        {...(fetchAuditLog ? { fetchAuditLog } : {})}
        {...restProps}
      />
      <StatusBar />
      <CommandPalette />
      {activeComparison && (
        <DiffModal
          open={true}
          left={{
            context: activeComparison.left.cluster,
            namespace: activeComparison.left.namespace ?? undefined,
            name: activeComparison.left.name,
            kind: activeComparison.left.kind,
            manifest: activeComparison.left.manifest,
          }}
          right={{
            context: activeComparison.right.cluster,
            namespace: activeComparison.right.namespace ?? undefined,
            name: activeComparison.right.name,
            kind: activeComparison.right.kind,
            manifest: activeComparison.right.manifest,
          }}
          resourceKind={activeComparison.left.kind}
          onClose={() => {
            closeComparison();
          }}
        />
      )}
    </div>
  );
}
