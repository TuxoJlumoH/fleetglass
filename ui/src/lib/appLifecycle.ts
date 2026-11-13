import { useEffect } from "react";
import { create } from "zustand";
import { commands } from "../generated/bindings";

/**
 * Background pause (03-04b): on Android the app tells the backend when it goes to the
 * background (`visibilitychange` → hidden) so every watcher, metrics poller, overview refresh
 * and log follower stops at once, and when it comes back. Desktop never calls this.
 */

interface AppLifecycleState {
  /** True after the backend was told the app is in the background (and not told otherwise). */
  background: boolean;
}

export const useAppLifecycleStore = create<AppLifecycleState>()(() => ({ background: false }));

/** What the backend was last told; it starts in the foreground. `null` after a failed call. */
let sent: boolean | null = true;
let warned = false;

function warnOnce(err: unknown): void {
  if (warned) return;
  warned = true;
  const message =
    typeof err === "object" && err !== null && "message" in err
      ? String(err.message)
      : String(err);
  console.warn(`set_app_foreground failed (further failures are not logged): ${message}`);
}

/** Tells the backend whether the app is visible; repeated values are not re-sent. */
export function reportForeground(foreground: boolean): void {
  if (sent === foreground) return;
  sent = foreground;
  useAppLifecycleStore.setState({ background: !foreground });
  const failed = (err: unknown) => {
    // Unknown backend state: send the next change again, whatever it is.
    if (sent === foreground) sent = null;
    warnOnce(err);
  };
  try {
    void commands
      .setAppForeground(foreground)
      .then((res) => {
        if (res.status === "error") failed(res.error);
      })
      .catch(failed);
  } catch (err: unknown) {
    failed(err);
  }
}

/** Follows `doc`'s visibility until the returned function is called. */
export function watchAppVisibility(doc: Document = document): () => void {
  const onChange = () => {
    reportForeground(doc.visibilityState !== "hidden");
  };
  doc.addEventListener("visibilitychange", onChange);
  onChange();
  return () => {
    doc.removeEventListener("visibilitychange", onChange);
  };
}

/** Mount once (App): follows visibility only while `enabled` (the Android app). */
export function useAppLifecycle(enabled: boolean): void {
  useEffect(() => (enabled ? watchAppVisibility() : undefined), [enabled]);
}

/** Test helper: forget what was sent. */
export function resetAppLifecycle(): void {
  sent = true;
  warned = false;
  useAppLifecycleStore.setState({ background: false });
}
