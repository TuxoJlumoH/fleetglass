export interface GoChord {
  key: string;
  label: string;
  kind?: string;
  view?: "helm";
}

export const GO_CHORDS: readonly GoChord[] = [
  { key: "p", label: "Pods", kind: "Pod" },
  { key: "d", label: "Deployments", kind: "Deployment" },
  { key: "s", label: "Services", kind: "Service" },
  { key: "n", label: "Nodes", kind: "Node" },
  { key: "e", label: "Events", kind: "Event" },
  { key: "h", label: "Helm releases", view: "helm" },
];

export const CHORD_TIMEOUT_MS = 1000;

export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  return target.closest(".cm-editor") !== null;
}

/** Menus (toolbar pickers) and the table toolbar own their keys; `g s` there is not a chord. */
function isInsideMenuOrToolbar(target: EventTarget | null): boolean {
  return (
    target instanceof Element && target.closest('[role="menu"], .pod-table__toolbar') !== null
  );
}

/** A modal dialog owns the keyboard; switching the tab behind it would be surprising. */
function isModalOpen(): boolean {
  return document.querySelector('[role="dialog"][aria-modal="true"]') !== null;
}

/** Returns a keydown handler; `now` is injectable for tests. */
export function createGoChordHandler(
  run: (chord: GoChord) => void,
  now: () => number = () => Date.now(),
): (e: KeyboardEvent) => void {
  let pendingAt: number | null = null;
  return (e: KeyboardEvent) => {
    if (
      e.ctrlKey ||
      e.metaKey ||
      e.altKey ||
      isTypingTarget(e.target) ||
      isInsideMenuOrToolbar(e.target) ||
      isModalOpen()
    ) {
      pendingAt = null;
      return;
    }
    const t = now();
    if (pendingAt !== null && t - pendingAt <= CHORD_TIMEOUT_MS) {
      const chord = GO_CHORDS.find((c) => c.key === e.key);
      pendingAt = null;
      if (chord) {
        e.preventDefault();
        run(chord);
        return;
      }
    }
    pendingAt = e.key === "g" ? t : null;
  };
}
