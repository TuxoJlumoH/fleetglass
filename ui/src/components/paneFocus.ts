import type { PaneId } from "../stores/layout";

// What a pane's keyboard user wants first: its table, then an editor (CodeMirror's content is
// contenteditable), then any other control.
const FOCUS_TARGETS = [
  ".pod-table-container",
  ".cm-content",
  "textarea, input, select, button, [tabindex]:not([tabindex='-1'])",
];

/** Moves keyboard focus into `pane`: its main content, else the pane itself. */
export function focusPane(pane: PaneId): void {
  const root = document.querySelector<HTMLElement>(`[data-testid="split-pane-${pane}"]`);
  if (!root) return;
  // Skip the split controls: the pane's content comes first.
  const content = root.querySelector<HTMLElement>(".shell-secondary__body") ?? root;
  for (const selector of FOCUS_TARGETS) {
    const target = content.querySelector<HTMLElement>(selector);
    if (target) {
      target.focus();
      return;
    }
  }
  root.focus();
}
