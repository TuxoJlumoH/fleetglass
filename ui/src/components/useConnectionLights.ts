import { useMemo } from "react";
import { useShallow } from "zustand/react/shallow";
import { useLayoutStore } from "../stores/layout";
import { usePodsStore } from "../stores/pods";
import { accessLightsByCluster, type ConnectionLight, type DotTone } from "./connectionStatus";

const SEP = "\u0000";

/** True while the active tab shows a resource table (the only view that owns the pods store). */
export function useResourceTabActive(): boolean {
  return useLayoutStore(
    (s) => (s.tabs.find((t) => t.id === s.activeTabId)?.view ?? "resources") === "resources",
  );
}

/**
 * Access-derived light per queried cluster of the active resource table. Empty on other tabs:
 * the pods store's access states are then left over from an unmounted table, not live.
 * Lights are encoded as strings inside the selector so a delta that leaves every light
 * unchanged does not re-render the caller.
 */
export function useAccessLights(clusters: readonly string[]): Record<string, ConnectionLight> {
  const resourceTab = useResourceTabActive();
  const encoded = usePodsStore(
    useShallow((s) => {
      const out: Record<string, string> = {};
      if (!resourceTab) return out;
      for (const [cluster, l] of Object.entries(accessLightsByCluster(s.access))) {
        if (clusters.includes(cluster)) out[cluster] = [l.tone, l.text, l.label].join(SEP);
      }
      return out;
    }),
  );
  return useMemo(() => {
    const out: Record<string, ConnectionLight> = {};
    for (const [cluster, value] of Object.entries(encoded)) {
      const [tone = "off", text = "", label = ""] = value.split(SEP);
      out[cluster] = { tone: tone as DotTone, text, label };
    }
    return out;
  }, [encoded]);
}
