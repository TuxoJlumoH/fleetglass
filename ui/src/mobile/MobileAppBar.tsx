import { ChevronDown, Menu, Search } from "lucide-react";
import type { CSSProperties } from "react";
import { clusterLight, UNKNOWN_LIGHT } from "../components/connectionStatus";
import { useAccessLights } from "../components/useConnectionLights";
import { clustersForQuery, useContextsStore } from "../stores/contexts";

const ICON_SIZE = 20;
const ICON_STROKE = 1.7;

/** What the context selector names: the selected context, or the cluster group's size. */
function selectionLabel(clusters: readonly string[], selected: string | null): string {
  if (clusters.length > 1) return `${String(clusters.length)} clusters`;
  return clusters[0] ?? selected ?? "No context";
}

/** Mockup v0.3 `.appbar`: ☰ (slide-over), context selector, search (command palette). */
export function MobileAppBar({
  onMenu,
  onSearch,
  menuOpen,
}: {
  onMenu: () => void;
  onSearch: () => void;
  menuOpen: boolean;
}) {
  const selectedContext = useContextsStore((s) => s.selectedContext);
  const activeClusterSelection = useContextsStore((s) => s.activeClusterSelection);
  const connectionStatus = useContextsStore((s) => s.connectionStatus);
  const settingsByContext = useContextsStore((s) => s.settingsByContext);
  const clusters = clustersForQuery({ activeClusterSelection, selectedContext });
  const accessLights = useAccessLights(clusters);
  const single = clusters.length === 1 ? clusters[0] : undefined;
  const light =
    single === undefined
      ? null
      : (clusterLight(single, accessLights, connectionStatus[single]) ?? UNKNOWN_LIGHT);
  const colour = single === undefined ? undefined : settingsByContext[single]?.colour;
  const label = selectionLabel(clusters, selectedContext);

  return (
    <header className="m-appbar">
      <button
        type="button"
        className="m-appbar__btn"
        aria-label="Open navigation"
        aria-expanded={menuOpen}
        data-testid="m-menu"
        onClick={onMenu}
      >
        <Menu size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="m-appbar__ctxsel"
        aria-label={`Context: ${label}. Change context`}
        data-testid="m-ctxsel"
        style={colour ? ({ "--ctx": colour } as CSSProperties) : undefined}
        onClick={onMenu}
      >
        <span className="ctx__sw" aria-hidden="true" />
        <span className="m-appbar__ctxname">{label}</span>
        {light && (
          <span
            role="img"
            className={`dot ${light.tone}`}
            data-tone={light.tone}
            aria-label={light.label}
            title={light.label}
          />
        )}
        <ChevronDown className="m-appbar__chev" size={14} strokeWidth={ICON_STROKE} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="m-appbar__btn"
        aria-label="Search and commands"
        data-testid="m-search"
        onClick={onSearch}
      >
        <Search size={ICON_SIZE} strokeWidth={ICON_STROKE} aria-hidden="true" />
      </button>
    </header>
  );
}
