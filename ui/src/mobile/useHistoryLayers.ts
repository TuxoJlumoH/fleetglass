import { useEffect, useRef, useState } from "react";
import { historyDepthOf, historyPlan } from "./mobileNav";

/**
 * Keeps one browser history entry per closable layer (`depth`). A history back (the Android back
 * gesture in the Tauri WebView, or the browser button) calls `onBack` once per popped entry; at
 * depth 0 there is no entry of ours left, so back falls through and the app exits.
 *
 * Our entries carry `{ fgMobile: n }` (n = 1, 2, …), so the current depth is always readable
 * from `history.state`: a (re)mount starts from it, and rewinds are clamped to it. Layers closed
 * by the UI (scrim tap, bottom nav) rewind with `history.go(-n)`; when the shell unmounts
 * (rotation to the desktop layout) all our entries are rewound.
 *
 * The state is per module, not per hook instance: the history is global, and a remount
 * (StrictMode, rotating back) must see a rewind that is still in flight.
 */

/** Stop waiting for the popstate of our own `history.go()` after this long. */
export const GO_SETTLE_MS = 500;

interface Owner {
  target: () => number;
  /** Called with the number of entries a user back popped. */
  back: (popped: number) => void;
}

/** Our depth as last seen in `history.state`. */
let pushed = 0;
/** Safety timer while our own `history.go()` is in flight; null when none is. */
let pendingGo: ReturnType<typeof setTimeout> | null = null;
/** Deferred rewind after the shell unmounted (cancelled by an immediate remount). */
let unmountRewind: ReturnType<typeof setTimeout> | null = null;
let owner: Owner | null = null;
let listening = false;

function currentDepth(): number {
  return historyDepthOf(window.history.state);
}

function landed() {
  if (pendingGo !== null) clearTimeout(pendingGo);
  pendingGo = null;
  pushed = currentDepth();
  sync();
}

/** Moves the history to the owner's depth (0 without an owner, which rewinds our entries). */
function sync() {
  // A traversal is asynchronous: a push before it lands would be undone by it.
  if (pendingGo !== null) return;
  pushed = currentDepth();
  const step = historyPlan(pushed, owner?.target() ?? 0);
  if (step === null) return;
  if ("push" in step) {
    for (let i = 1; i <= step.push; i++) {
      window.history.pushState({ fgMobile: pushed + i }, "");
    }
    pushed += step.push;
    return;
  }
  // Clamp to the recorded depth: never rewind past our first entry (that would leave the app).
  const delta = Math.max(step.go, -pushed);
  if (delta === 0) return;
  pendingGo = setTimeout(landed, GO_SETTLE_MS);
  window.history.go(delta);
}

function onPop(e: PopStateEvent) {
  // Assumption: one popstate per go(). A traversal by any delta fires a single popstate when it
  // lands, so the first popstate after our go() is ours, whatever its depth. Should it never
  // come, the GO_SETTLE_MS timer re-reads the depth from `history.state` and re-syncs, so a
  // wrong guess costs one re-sync, not a stuck history.
  if (pendingGo !== null) {
    landed();
    return;
  }
  const at = historyDepthOf(e.state);
  const prev = pushed;
  pushed = at;
  if (owner === null) return;
  if (at >= prev) {
    // Forward navigation (not reachable on Android): just re-align.
    sync();
    return;
  }
  owner.back(prev - at);
}

export function useHistoryLayers(depth: number, onBack: () => void): void {
  const depthRef = useRef(depth);
  const onBackRef = useRef(onBack);
  // Bumped after a user back so the history is re-synced even when `depth` did not change
  // (a layer that refused to close, e.g. a running action, gets its entry back).
  const [backs, setBacks] = useState(0);
  useEffect(() => {
    onBackRef.current = onBack;
  });

  useEffect(() => {
    if (!listening) {
      window.addEventListener("popstate", onPop);
      listening = true;
    }
    if (unmountRewind !== null) clearTimeout(unmountRewind);
    unmountRewind = null;
    const me: Owner = {
      target: () => depthRef.current,
      back: (popped) => {
        for (let i = 0; i < popped; i++) onBackRef.current();
        setBacks((n) => n + 1);
      },
    };
    owner = me;
    sync();
    return () => {
      if (owner !== me) return;
      owner = null;
      // Deferred so StrictMode's immediate remount neither rewinds nor re-pushes.
      unmountRewind = setTimeout(() => {
        unmountRewind = null;
        if (owner === null) sync();
      }, 0);
    };
  }, []);

  useEffect(() => {
    depthRef.current = depth;
    sync();
  }, [depth, backs]);
}

/** For tests: true while a `history.go()` of ours or the unmount rewind has not landed. */
export function historyLayersBusy(): boolean {
  return pendingGo !== null || unmountRewind !== null;
}
