import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  LAST_SELECTION_STORAGE_KEY,
  initialContextsState,
  parseLastSelection,
  restoredSelection,
  useContextsStore,
} from "./contexts";

// 03-03b: the selected context (or cluster group) survives a restart.

const ctx = (name: string, isCurrent = false) => ({
  name,
  cluster: name,
  user: "u",
  namespace: null,
  server: null,
  auth: { type: "token" },
  isCurrent,
});

function backend(current: string | null, groups: { name: string; contexts: string[] }[] = []) {
  (globalThis as { isTauri?: boolean }).isTauri = true;
  mockWindows("main");
  mockIPC((cmd) => {
    if (cmd === "list_contexts") {
      return {
        contexts: [ctx("dev", current === "dev"), ctx("prod"), ctx("staging")],
        current,
        warnings: [],
      };
    }
    if (cmd === "get_cluster_groups") return groups;
    return undefined;
  });
}

const store = (value: unknown) => {
  localStorage.setItem(LAST_SELECTION_STORAGE_KEY, JSON.stringify(value));
};
const stored = () => parseLastSelection(localStorage.getItem(LAST_SELECTION_STORAGE_KEY));

beforeEach(() => {
  useContextsStore.setState({ ...initialContextsState, clusterGroups: [], activeClusterSelection: [] });
});

afterEach(() => {
  clearMocks();
  delete (globalThis as { isTauri?: boolean }).isTauri;
});

describe("last selection", () => {
  it("restores the stored context when it still exists", async () => {
    store({ context: "prod", group: null, kubeCurrent: "dev" });
    backend("dev");
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().selectedContext).toBe("prod");
    expect(useContextsStore.getState().current).toBe("dev");
  });

  it("restores on a kubeconfig without current-context (imported contexts on Android)", async () => {
    store({ context: "staging", group: null, kubeCurrent: null });
    backend(null);
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().selectedContext).toBe("staging");
  });

  it("falls back to the kubeconfig current-context when the stored one is gone", async () => {
    store({ context: "deleted-ctx", group: null, kubeCurrent: "dev" });
    backend("dev");
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().selectedContext).toBe("dev");
  });

  it("uses the kubeconfig current-context when nothing is stored, or the record is malformed", async () => {
    backend("dev");
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().selectedContext).toBe("dev");

    localStorage.setItem(LAST_SELECTION_STORAGE_KEY, "{not json");
    useContextsStore.setState({ ...initialContextsState });
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().selectedContext).toBe("dev");
  });

  it("a kubeconfig current-context changed outside the app wins over the stored selection", async () => {
    store({ context: "prod", group: "All Clusters", kubeCurrent: "dev" });
    backend("staging");
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().selectedContext).toBe("staging");
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);
  });

  it("restores a saved cluster group once the groups have loaded", async () => {
    store({ context: "dev", group: "edge", kubeCurrent: "dev" });
    backend("dev", [{ name: "edge", contexts: ["prod", "staging", "gone"] }]);
    await useContextsStore.getState().loadContexts();
    // The groups are not loaded yet: nothing applied.
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);
    await useContextsStore.getState().loadClusterGroups();
    // Contexts that no longer exist are dropped.
    expect(useContextsStore.getState().activeClusterSelection).toEqual(["prod", "staging"]);
    expect(useContextsStore.getState().selectedContext).toBe("dev");
  });

  it("restores All Clusters without waiting for the groups", async () => {
    store({ context: "dev", group: "All Clusters", kubeCurrent: "dev" });
    backend("dev");
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().activeClusterSelection).toEqual(["dev", "prod", "staging"]);
  });

  it("drops a saved group that no longer exists and keeps the context", async () => {
    store({ context: "prod", group: "removed", kubeCurrent: "dev" });
    backend("dev", []);
    await useContextsStore.getState().loadContexts();
    await useContextsStore.getState().loadClusterGroups();
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);
    expect(useContextsStore.getState().selectedContext).toBe("prod");
  });

  it("saves the picked context and group, and an ad-hoc selection clears the group", async () => {
    backend("dev", [{ name: "edge", contexts: ["prod"] }]);
    await useContextsStore.getState().loadContexts();
    await useContextsStore.getState().loadClusterGroups();

    await useContextsStore.getState().selectContext("prod");
    expect(stored()).toEqual({ context: "prod", group: null, kubeCurrent: "dev" });

    useContextsStore.getState().selectClusterGroup("edge");
    expect(stored()).toEqual({ context: "prod", group: "edge", kubeCurrent: "dev" });

    useContextsStore.getState().setActiveClusterSelection(["dev", "prod"]);
    expect(stored()).toEqual({ context: "prod", group: null, kubeCurrent: "dev" });
  });

  it("toggleClusterSelection transitions from single context and toggles items in/out", async () => {
    backend("dev");
    await useContextsStore.getState().loadContexts();
    expect(useContextsStore.getState().selectedContext).toBe("dev");
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);

    // Toggling the same cluster leaves activeClusterSelection empty
    useContextsStore.getState().toggleClusterSelection("dev");
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);

    // Toggling another cluster transitions from selectedContext + name
    useContextsStore.getState().toggleClusterSelection("prod");
    expect(useContextsStore.getState().activeClusterSelection).toEqual(["dev", "prod"]);
    expect(stored()).toEqual({ context: "dev", group: null, kubeCurrent: "dev" });

    // Toggling a third cluster appends it
    useContextsStore.getState().toggleClusterSelection("staging");
    expect(useContextsStore.getState().activeClusterSelection).toEqual(["dev", "prod", "staging"]);

    // Toggling one out leaves remaining
    useContextsStore.getState().toggleClusterSelection("prod");
    expect(useContextsStore.getState().activeClusterSelection).toEqual(["dev", "staging"]);

    // Toggling out to 1 item keeps it in activeClusterSelection
    useContextsStore.getState().toggleClusterSelection("dev");
    expect(useContextsStore.getState().activeClusterSelection).toEqual(["staging"]);

    // Toggling out to 0 items clears activeClusterSelection and keeps selectedContext
    useContextsStore.getState().toggleClusterSelection("staging");
    expect(useContextsStore.getState().activeClusterSelection).toEqual([]);
    expect(useContextsStore.getState().selectedContext).toBe("dev");
    expect(stored()).toEqual({ context: "dev", group: null, kubeCurrent: "dev" });
  });
});

describe("restoredSelection", () => {
  it("keeps the stored context when the kubeconfig never had a current-context", () => {
    expect(restoredSelection({ context: "a", group: "g", kubeCurrent: null }, ["a"], null)).toEqual({
      context: "a",
      group: "g",
    });
  });

  it("records without kubeCurrent (older format) are restored", () => {
    expect(restoredSelection({ context: "a", group: null }, ["a", "b"], "b")).toEqual({
      context: "a",
      group: null,
    });
  });
});
