import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";
import { resetBackLayers } from "../lib/backLayers";
import { LAST_SELECTION_STORAGE_KEY, resetLastSelectionState } from "../stores/contexts";

afterEach(() => {
  cleanup();
  // A modal that a failing test left open must not swallow the next test's back gesture.
  resetBackLayers();
  // The selection picked in one test must not become the next test's restored context.
  try {
    localStorage.removeItem(LAST_SELECTION_STORAGE_KEY);
  } catch {
    // No storage in this environment.
  }
  resetLastSelectionState();
});
