import { Box, House, type LucideIcon, Settings, Zap } from "lucide-react";

export type BottomNavItem = "overview" | "workloads" | "events" | "settings";

const ITEMS: readonly { id: BottomNavItem; label: string; icon: LucideIcon }[] = [
  { id: "overview", label: "Overview", icon: House },
  { id: "workloads", label: "Workloads", icon: Box },
  { id: "events", label: "Events", icon: Zap },
  { id: "settings", label: "Settings", icon: Settings },
];

/** Mockup v0.3 `.bnav`: four destinations, the active one with a top accent bar. */
export function BottomNav({
  active,
  eventsWarning,
  onSelect,
}: {
  /** null when the screen is not one of the four (e.g. a sidebar view like Deployments). */
  active: BottomNavItem | null;
  /** Red dot on Events: the last overview counted warning events. */
  eventsWarning: boolean;
  onSelect: (item: BottomNavItem) => void;
}) {
  return (
    <nav className="m-bnav" aria-label="Main">
      {ITEMS.map(({ id, label, icon: Icon }) => (
        <button
          key={id}
          type="button"
          className="m-bnav__item"
          data-testid={`m-bnav-${id}`}
          aria-current={active === id ? "page" : undefined}
          onClick={() => {
            onSelect(id);
          }}
        >
          <Icon size={20} strokeWidth={1.7} aria-hidden="true" />
          {label}
          {id === "events" && eventsWarning && (
            <>
              <span className="m-bnav__fail" aria-hidden="true" />
              <span className="visually-hidden">{", warnings"}</span>
            </>
          )}
        </button>
      ))}
    </nav>
  );
}
