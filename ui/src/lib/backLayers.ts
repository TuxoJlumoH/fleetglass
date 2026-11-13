import { useEffect, useRef, useSyncExternalStore } from "react";

/**
 * Open modal layers that a "back" (the Android back gesture in the phone shell, 03-03a) should
 * close before anything else. Any dialog calls `useBackLayer(open, close)`; the phone shell
 * counts the layers (one history entry each) and closes the newest on back. On the desktop
 * nothing reads the registry, so registering is free.
 */
interface Layer {
  /** Returns false when the layer refused to close (it then stays open). */
  close: () => boolean;
}

let layers: readonly Layer[] = [];
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

function subscribe(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

function count(): number {
  return layers.length;
}

/** Number of open back layers (re-renders when it changes). */
export function useBackLayerCount(): number {
  return useSyncExternalStore(subscribe, count, count);
}

/**
 * Closes the newest open layer. Returns false when there is none. The layer's `close` may
 * refuse (e.g. an action that is already running); the layer then stays registered.
 */
export function closeTopBackLayer(): boolean {
  const top = layers[layers.length - 1];
  if (top === undefined || !top.close()) return false;
  // Dropped now rather than when the dialog's effect cleans up, so a second back in the same
  // tick reaches the layer below.
  layers = layers.filter((l) => l !== top);
  emit();
  return true;
}

/** True when a modal layer is open (the phone shell's back closes it first). */
export function hasBackLayer(): boolean {
  return layers.length > 0;
}

/**
 * Registers a closable modal layer while `open` is true. `close` may change between renders;
 * it may return false to refuse (e.g. an action that is already running).
 */
export function useBackLayer(open: boolean, close: () => unknown): void {
  const closeRef = useRef(close);
  useEffect(() => {
    closeRef.current = close;
  });
  useEffect(() => {
    if (!open) return;
    const layer: Layer = {
      close: () => closeRef.current() !== false,
    };
    layers = [...layers, layer];
    emit();
    return () => {
      layers = layers.filter((l) => l !== layer);
      emit();
    };
  }, [open]);
}

/** Test helper: forget every layer. */
export function resetBackLayers(): void {
  layers = [];
  emit();
}
