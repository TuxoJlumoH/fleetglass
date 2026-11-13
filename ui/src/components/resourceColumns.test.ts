import { describe, expect, it } from "vitest";
import { columnsForKind } from "./resourceColumns";

describe("resourceColumns sort fields", () => {
  it("Event 'Type' sorts by its own cell, not by status_reason (which holds the reason)", () => {
    const type = columnsForKind("Event").find((c) => c.label === "Type");
    expect(type?.sortField).toEqual({ type: "cell", index: 0 });
  });

  it("Pod and Deployment 'Status' sort by the row status", () => {
    for (const kind of ["Pod", "Deployment"]) {
      const status = columnsForKind(kind).find((c) => c.label === "Status");
      expect(status?.sortField).toEqual({ type: "status" });
    }
  });
});

describe("resourceColumns Pod layout", () => {
  it("mirrors the core pod cell order (CPU and Memory at cells 8 and 9)", () => {
    const cols = columnsForKind("Pod");
    expect(cols.map((c) => c.label)).toEqual([
      "Cluster",
      "Namespace",
      "Name",
      "Ready",
      "Status",
      "Restarts",
      "Last restart",
      "Last reason",
      "Node",
      "IP",
      "Age",
      "CPU",
      "Memory",
    ]);
    const byLabel = (label: string) => cols.find((c) => c.label === label);
    expect(byLabel("Last restart")?.source).toEqual({ type: "cell", index: 3 });
    expect(byLabel("Last restart")?.sortField).toEqual({ type: "cell", index: 3 });
    expect(byLabel("Last reason")?.source).toEqual({ type: "cell", index: 4 });
    expect(byLabel("Node")?.source).toEqual({ type: "cell", index: 5 });
    expect(byLabel("CPU")?.source).toEqual({ type: "cell", index: 8 });
    expect(byLabel("CPU")?.sortField).toEqual({ type: "cell", index: 8 });
    expect(byLabel("CPU")?.width).toEqual("120px");
    expect(byLabel("Memory")?.source).toEqual({ type: "cell", index: 9 });
    expect(byLabel("Memory")?.sortField).toEqual({ type: "cell", index: 9 });
    expect(byLabel("Memory")?.width).toEqual("120px");
  });
});

describe("resourceColumns Node layout", () => {
  it("mirrors the core node cell order (CPU and Memory at cells 2 and 3)", () => {
    const cols = columnsForKind("Node");
    expect(cols.map((c) => c.label)).toEqual([
      "Cluster",
      "Name",
      "Status",
      "Age",
      "CPU",
      "Memory",
    ]);
    const byLabel = (label: string) => cols.find((c) => c.label === label);
    expect(byLabel("CPU")?.source).toEqual({ type: "cell", index: 2 });
    expect(byLabel("CPU")?.sortField).toEqual({ type: "cell", index: 2 });
    expect(byLabel("CPU")?.width).toEqual("120px");
    expect(byLabel("Memory")?.source).toEqual({ type: "cell", index: 3 });
    expect(byLabel("Memory")?.sortField).toEqual({ type: "cell", index: 3 });
    expect(byLabel("Memory")?.width).toEqual("120px");
  });
});

describe("resourceColumns 04-05a layouts", () => {
  it("defines column layouts for expanded built-in Kubernetes kinds", () => {
    const rs = columnsForKind("ReplicaSet");
    expect(rs.map((c) => c.label)).toEqual([
      "Cluster",
      "Namespace",
      "Name",
      "Ready",
      "Current",
      "Status",
      "Age",
    ]);

    const pv = columnsForKind("PersistentVolume");
    // Cluster scoped: no Namespace column
    expect(pv.map((c) => c.label)).toEqual([
      "Cluster",
      "Name",
      "Capacity",
      "Access modes",
      "Reclaim policy",
      "Status",
      "Claim",
      "Storage class",
      "Age",
    ]);

    const rb = columnsForKind("RoleBinding");
    expect(rb.map((c) => c.label)).toEqual([
      "Cluster",
      "Namespace",
      "Name",
      "Role",
      "Subjects",
      "Status",
      "Age",
    ]);

    const crb = columnsForKind("ClusterRoleBinding");
    expect(crb.map((c) => c.label)).toEqual([
      "Cluster",
      "Name",
      "Role",
      "Subjects",
      "Status",
      "Age",
    ]);

    const cert = columnsForKind("Certificate");
    expect(cert.map((c) => c.label)).toEqual([
      "Cluster",
      "Namespace",
      "Name",
      "Status",
      "Ready",
      "Secret",
      "Issuer",
      "Expiry",
      "Age",
    ]);

    const es = columnsForKind("ExternalSecret");
    expect(es.map((c) => c.label)).toEqual([
      "Cluster",
      "Namespace",
      "Name",
      "Status",
      "Store",
      "Target",
      "Refresh",
      "Age",
    ]);

    const ro = columnsForKind("Rollout");
    expect(ro.map((c) => c.label)).toEqual([
      "Cluster",
      "Namespace",
      "Name",
      "Status",
      "Strategy",
      "Step",
      "Replicas",
      "Age",
    ]);
  });
});

