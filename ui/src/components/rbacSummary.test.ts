import { describe, expect, it } from "vitest";
import { parseRbacFromYaml } from "./rbacSummary";

describe("parseRbacFromYaml", () => {
  it("parses RoleBinding roleRef and subjects", () => {
    const yaml = `
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: read-pods
  namespace: default
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: pod-reader
subjects:
- kind: ServiceAccount
  name: test-sa
  namespace: default
- kind: User
  name: alice
`;
    const rbac = parseRbacFromYaml(yaml);
    expect(rbac.roleRef).toEqual({ kind: "Role", name: "pod-reader" });
    expect(rbac.subjects).toEqual([
      { kind: "ServiceAccount", name: "test-sa", namespace: "default" },
      { kind: "User", name: "alice" },
    ]);
  });

  it("parses Role rules", () => {
    const yaml = `
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: pod-reader
rules:
- apiGroups: [""]
  resources: ["pods", "pods/log"]
  verbs: ["get", "list", "watch"]
- apiGroups: ["apps"]
  resources: ["deployments"]
  verbs: ["get"]
`;
    const rbac = parseRbacFromYaml(yaml);
    expect(rbac.rules).toEqual([
      {
        apiGroups: [""],
        resources: ["pods", "pods/log"],
        verbs: ["get", "list", "watch"],
      },
      {
        apiGroups: ["apps"],
        resources: ["deployments"],
        verbs: ["get"],
      },
    ]);
  });
});
