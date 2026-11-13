import { describe, expect, it } from "vitest";
import { parseLogsRoute, validateWorkloadLogTarget } from "./logsRoute";

describe("logsRoute", () => {
  it("ignores non-logs hashes", () => {
    expect(parseLogsRoute("")).toEqual({ kind: "notLogs" });
    expect(parseLogsRoute("#/")).toEqual({ kind: "notLogs" });
    expect(parseLogsRoute("#/pods")).toEqual({ kind: "notLogs" });
  });

  it("reports invalid when target query param is missing", () => {
    const res = parseLogsRoute("#/logs");
    expect(res.kind).toBe("invalid");
    if (res.kind === "invalid") {
      expect(res.message).toMatch(/no target parameter/i);
    }
  });

  it("reports invalid when target is not valid JSON", () => {
    const res = parseLogsRoute("#/logs?target=not-json");
    expect(res.kind).toBe("invalid");
    if (res.kind === "invalid") {
      expect(res.message).toMatch(/not valid JSON/i);
    }
  });

  it("parses valid labels target", () => {
    const target = {
      cluster: "dev",
      namespace: "default",
      selector: { type: "labels", selector: "app=api" },
      container: "api",
      tailLines: 100,
    };
    const hash = `#/logs?target=${encodeURIComponent(JSON.stringify(target))}`;
    const res = parseLogsRoute(hash);
    expect(res).toEqual({ kind: "logs", target });
  });

  it("parses valid owner target", () => {
    const target = {
      cluster: "prod",
      namespace: "shop",
      selector: { type: "owner", kind: "Deployment", name: "cart" },
      container: null,
      tailLines: null,
    };
    const hash = `#/logs?target=${encodeURIComponent(JSON.stringify(target))}`;
    const res = parseLogsRoute(hash);
    expect(res).toEqual({ kind: "logs", target });
  });

  it("parses valid pod target", () => {
    const target = {
      cluster: "dev",
      namespace: "default",
      selector: { type: "pod", name: "nginx-0" },
      container: null,
      tailLines: 500,
    };
    const hash = `#/logs?target=${encodeURIComponent(JSON.stringify(target))}`;
    const res = parseLogsRoute(hash);
    expect(res).toEqual({ kind: "logs", target });
  });

  it("validates target object structure", () => {
    expect(typeof validateWorkloadLogTarget(null)).toBe("string");
    expect(typeof validateWorkloadLogTarget({ cluster: "" })).toBe("string");
    expect(
      typeof validateWorkloadLogTarget({
        cluster: "c",
        namespace: "n",
        selector: { type: "unknown" },
      }),
    ).toBe("string");
  });
});
