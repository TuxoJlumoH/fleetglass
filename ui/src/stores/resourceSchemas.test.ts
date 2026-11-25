import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, describe, expect, it } from "vitest";
import { waitFor } from "@testing-library/react";
import {
  MAX_CACHED_SCHEMAS,
  resetResourceSchemasStore,
  schemaKey,
  useResourceSchemasStore,
} from "./resourceSchemas";

type Args = { context: string; apiVersion: string; kind: string };

function mockSchemas(handler: (args: Args) => unknown): Args[] {
  const calls: Args[] = [];
  mockIPC((cmd, payload) => {
    if (cmd !== "get_resource_schema") throw new Error(`unexpected ${cmd}`);
    const args = payload as Args;
    calls.push(args);
    return handler(args);
  });
  return calls;
}

const entry = (ctx: string, av: string, kind: string) =>
  useResourceSchemasStore.getState().entries[schemaKey(ctx, av, kind)];

describe("resourceSchemas store", () => {
  afterEach(() => {
    clearMocks();
    resetResourceSchemasStore();
  });

  it("fetches once per context+GVK and parses the schema", async () => {
    const calls = mockSchemas(({ kind }) => ({
      state: "available",
      apiVersion: "apps/v1",
      kind,
      schemaJson: '{"type":"object","properties":{"spec":{"type":"object"}}}',
      truncated: false,
    }));
    const { ensure } = useResourceSchemasStore.getState();
    ensure("a", "apps/v1", "Deployment");
    ensure("a", "apps/v1", "Deployment");
    await waitFor(() => {
      expect(entry("a", "apps/v1", "Deployment")?.state).toBe("ready");
    });
    ensure("a", "apps/v1", "Deployment");
    ensure("b", "apps/v1", "Deployment");
    await waitFor(() => {
      expect(entry("b", "apps/v1", "Deployment")?.state).toBe("ready");
    });
    expect(calls.map((c) => c.context)).toEqual(["a", "b"]);
    const ready = entry("a", "apps/v1", "Deployment");
    expect(ready?.state === "ready" && ready.schema.properties?.spec?.type).toBe("object");
  });

  it("keeps unavailable results and IPC errors honest", async () => {
    mockSchemas(({ kind }) => {
      if (kind === "Broken") throw { kind: "internal", message: "boom" } as unknown as Error;
      return { state: "unavailable", reason: "forbidden", message: "no permission to read the OpenAPI schema" };
    });
    const { ensure } = useResourceSchemasStore.getState();
    ensure("a", "v1", "Pod");
    ensure("a", "v1", "Broken");
    await waitFor(() => {
      expect(entry("a", "v1", "Pod")?.state).toBe("unavailable");
    });
    await waitFor(() => {
      expect(entry("a", "v1", "Broken")?.state).toBe("unavailable");
    });
    expect(entry("a", "v1", "Pod")).toMatchObject({ reason: "forbidden" });
    expect(entry("a", "v1", "Broken")).toMatchObject({ reason: "error", message: "boom" });
  });

  it("is bounded and invalidated per context", () => {
    mockSchemas(() => ({ state: "unavailable", reason: "notFound", message: "x" }));
    const { ensure, invalidateContext } = useResourceSchemasStore.getState();
    for (let i = 0; i < MAX_CACHED_SCHEMAS + 5; i++) ensure("a", "v1", `K${String(i)}`);
    ensure("b", "v1", "Pod");
    expect(Object.keys(useResourceSchemasStore.getState().entries)).toHaveLength(MAX_CACHED_SCHEMAS);
    expect(entry("a", "v1", "K0")).toBeUndefined();
    invalidateContext("a");
    expect(Object.keys(useResourceSchemasStore.getState().entries)).toEqual([schemaKey("b", "v1", "Pod")]);
  });
});
