import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ClusterGroup, IpcError } from "../generated/bindings";
import { HelmView, type HelmRelease } from "./HelmView";
import { SecretEditor, type SecretCodec } from "./SecretEditor";
import { Sidebar } from "./Sidebar";
import {
  type ClusterGroupClient,
  initialContextsState,
  setClusterGroupClient,
  useContextsStore,
} from "../stores/contexts";
import { initialLayoutState, useLayoutStore } from "../stores/layout";

describe("Phase 2 Wave 1 UI Components", () => {
  beforeEach(() => {
    useLayoutStore.setState(initialLayoutState);
    useContextsStore.setState(initialContextsState);
  });

  // Test 1: Selecting a cluster group updates activeClusterSelection with all context names in that group.
  it("selecting a cluster group updates activeClusterSelection with all context names in that group", async () => {
    const user = userEvent.setup();

    useContextsStore.setState({
      ...initialContextsState,
      available: true,
      status: "ready",
      contexts: [
        {
          name: "cluster-prod-us",
          cluster: "c-prod-us",
          user: "admin",
          namespace: "prod",
          server: "https://prod-us.example.com",
          auth: { type: "none" },
          isCurrent: true,
        },
        {
          name: "cluster-prod-eu",
          cluster: "c-prod-eu",
          user: "admin",
          namespace: "prod",
          server: "https://prod-eu.example.com",
          auth: { type: "none" },
          isCurrent: false,
        },
        {
          name: "cluster-staging",
          cluster: "c-stage",
          user: "dev",
          namespace: "staging",
          server: "https://stage.example.com",
          auth: { type: "none" },
          isCurrent: false,
        },
      ],
      clusterGroups: [
        { name: "Production", contexts: ["cluster-prod-us", "cluster-prod-eu"] },
        { name: "Staging", contexts: ["cluster-staging"] },
      ],
      activeClusterSelection: [],
    });

    render(<Sidebar />);

    const clusterGroupsList = screen.getByRole("list", { name: "Cluster Groups" });
    expect(clusterGroupsList).toBeInTheDocument();

    const prodGroupBtn = within(clusterGroupsList).getByRole("button", {
      name: /production/i,
    });
    expect(prodGroupBtn).toBeInTheDocument();

    // Click the Production group
    await user.click(prodGroupBtn);

    expect(useContextsStore.getState().activeClusterSelection).toEqual([
      "cluster-prod-us",
      "cluster-prod-eu",
    ]);

    // Click the Staging group
    const stagingGroupBtn = within(clusterGroupsList).getByRole("button", {
      name: /staging/i,
    });
    await user.click(stagingGroupBtn);

    expect(useContextsStore.getState().activeClusterSelection).toEqual([
      "cluster-staging",
    ]);

    // Click All Clusters
    const allClustersBtn = within(clusterGroupsList).getByRole("button", {
      name: /all clusters/i,
    });
    await user.click(allClustersBtn);

    expect(useContextsStore.getState().activeClusterSelection).toEqual([
      "cluster-prod-us",
      "cluster-prod-eu",
      "cluster-staging",
    ]);
  });

  // Test 2: <SecretEditor /> decodes via the injected codec and toggles plaintext visibility.
  it("<SecretEditor /> decodes via injected codec (non-ASCII), masks by default and encodes on save", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn<(decoded: Record<string, string>, base64: Record<string, string>) => void>();

    let resolveDecode: (v: Record<string, string>) => void = () => undefined;
    const decode = vi.fn<SecretCodec>(
      () =>
        new Promise<Record<string, string>>((resolve) => {
          resolveDecode = resolve;
        }),
    );
    const encode = vi.fn((data: Record<string, string>) =>
      Promise.resolve(
        Object.fromEntries(Object.entries(data).map(([k, v]) => [k, `enc(${v})`])),
      ),
    );

    // Opaque "encoded" payloads: the component must not decode them itself.
    const secretData = { DATABASE_PASSWORD: "ENC-1", API_TOKEN: "ENC-2" };

    render(
      <SecretEditor
        name="my-db-secret"
        namespace="production"
        data={secretData}
        decode={decode}
        encode={encode}
        onSave={onSave}
      />,
    );

    // Loading until the single decode call resolves.
    expect(screen.getByText("Decoding secret…")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /save/i })).toBeDisabled();
    expect(decode).toHaveBeenCalledTimes(1);
    expect(decode).toHaveBeenCalledWith(secretData);

    await act(async () => {
      resolveDecode({ DATABASE_PASSWORD: "pässwörd", API_TOKEN: "token-xyz-456" });
      await Promise.resolve();
    });

    expect(screen.queryByText("Decoding secret…")).not.toBeInTheDocument();

    // Masked by default.
    expect(screen.queryByText("pässwörd")).not.toBeInTheDocument();
    expect(screen.getAllByText("••••••••")).toHaveLength(2);
    expect(screen.getByLabelText("Value for DATABASE_PASSWORD")).toHaveAttribute("type", "password");

    // "Reveal / Decode" shows the decoded UTF-8 plaintext.
    const revealBtn = screen.getByRole("button", { name: /reveal \/ decode/i });
    await user.click(revealBtn);
    expect(screen.getByText("pässwörd")).toBeInTheDocument();
    expect(screen.getByDisplayValue("pässwörd")).toBeInTheDocument();
    expect(screen.getByText("token-xyz-456")).toBeInTheDocument();

    await user.click(revealBtn);
    expect(screen.queryByText("pässwörd")).not.toBeInTheDocument();

    // The plaintext switch toggles the same state.
    const plaintextToggle = screen.getByRole("switch", { name: /plaintext toggle/i });
    await user.click(plaintextToggle);
    expect(screen.getByText("pässwörd")).toBeInTheDocument();

    // Edit a value and save: encode receives the plaintext map, onSave gets both maps.
    const firstInput = screen.getByDisplayValue("pässwörd");
    await user.clear(firstInput);
    await user.type(firstInput, "nëw-pässwörd");
    await user.click(screen.getByRole("button", { name: /save/i }));

    expect(decode).toHaveBeenCalledTimes(1);
    expect(encode).toHaveBeenCalledTimes(1);
    expect(encode).toHaveBeenCalledWith({
      DATABASE_PASSWORD: "nëw-pässwörd",
      API_TOKEN: "token-xyz-456",
    });
    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onSave).toHaveBeenCalledWith(
      { DATABASE_PASSWORD: "nëw-pässwörd", API_TOKEN: "token-xyz-456" },
      { DATABASE_PASSWORD: "enc(nëw-pässwörd)", API_TOKEN: "enc(token-xyz-456)" },
    );
  });

  it("<SecretEditor /> shows the typed decode error and keeps values masked", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    const ipcError: IpcError = { kind: "invalidInput", message: 'invalid base64 in key "TOKEN"' };
    // The default codec rethrows the serialized `IpcError` object, not an `Error`.
    // eslint-disable-next-line @typescript-eslint/prefer-promise-reject-errors
    const decode = vi.fn(() => Promise.reject(ipcError));
    const encode = vi.fn(() => Promise.resolve({}));

    render(
      <SecretEditor name="broken" data={{ TOKEN: "%%%not-base64" }} decode={decode} encode={encode} onSave={onSave} />,
    );

    expect(await screen.findByTestId("secret-codec-error")).toHaveTextContent(
      'Cannot decode secret: invalid base64 in key "TOKEN"',
    );
    // The key is listed, but the raw value is never shown and cannot be revealed.
    expect(screen.getByDisplayValue("TOKEN")).toBeInTheDocument();
    expect(screen.queryByText("%%%not-base64")).not.toBeInTheDocument();
    expect(screen.queryByDisplayValue("%%%not-base64")).not.toBeInTheDocument();
    expect(screen.getByText("••••••••")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /reveal \/ decode/i })).toBeDisabled();
    expect(screen.getByRole("switch", { name: /plaintext toggle/i })).not.toBeChecked();

    const saveBtn = screen.getByRole("button", { name: /save/i });
    expect(saveBtn).toBeDisabled();
    await user.click(saveBtn);
    expect(encode).not.toHaveBeenCalled();
    expect(onSave).not.toHaveBeenCalled();
  });

  it("<SecretEditor /> surfaces an encode error and does not call onSave", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    const decode = vi.fn((data: Record<string, string>) => Promise.resolve({ ...data }));
    const encode = vi.fn(() => Promise.reject(new Error("backend unavailable")));

    render(<SecretEditor data={{ K: "v" }} decode={decode} encode={encode} onSave={onSave} />);

    const saveBtn = await screen.findByRole("button", { name: /save/i });
    await waitFor(() => {
      expect(saveBtn).toBeEnabled();
    });
    await user.click(saveBtn);

    expect(encode).toHaveBeenCalledWith({ K: "v" });
    expect(onSave).not.toHaveBeenCalled();
    expect(await screen.findByTestId("secret-codec-error")).toHaveTextContent(
      "Cannot encode secret: backend unavailable",
    );
  });

  describe("cluster groups store", () => {
    function fakeClient(initial: ClusterGroup[]) {
      const groups = new Map(initial.map((g) => [g.name, g]));
      const client = {
        getClusterGroups: vi.fn(() =>
          Promise.resolve({
            status: "ok" as const,
            data: [...groups.values()].sort((a, b) => a.name.localeCompare(b.name)),
          }),
        ),
        setClusterGroup: vi.fn((group: ClusterGroup) => {
          groups.set(group.name, group);
          return Promise.resolve({ status: "ok" as const, data: null });
        }),
        deleteClusterGroup: vi.fn((name: string) => {
          groups.delete(name);
          return Promise.resolve({ status: "ok" as const, data: null });
        }),
      } satisfies ClusterGroupClient;
      return { client, groups };
    }

    afterEach(() => {
      setClusterGroupClient();
    });

    it("loads, saves and deletes cluster groups through the injected client", async () => {
      const { client, groups } = fakeClient([
        { name: "Staging", contexts: ["cluster-staging"] },
        { name: "Production", contexts: ["cluster-prod-us", "cluster-prod-eu"] },
      ]);
      setClusterGroupClient(client);

      await act(() => useContextsStore.getState().loadClusterGroups());
      expect(client.getClusterGroups).toHaveBeenCalledTimes(1);
      expect(useContextsStore.getState().clusterGroupsStatus).toBe("ready");
      expect(useContextsStore.getState().clusterGroups.map((g) => g.name)).toEqual([
        "Production",
        "Staging",
      ]);

      // Selection still works on loaded groups.
      act(() => {
        useContextsStore.getState().selectClusterGroup("Production");
      });
      expect(useContextsStore.getState().activeClusterSelection).toEqual([
        "cluster-prod-us",
        "cluster-prod-eu",
      ]);

      // Save adds a new group, sorted by name.
      const qa: ClusterGroup = { name: "QA", contexts: ["cluster-qa"] };
      await expect(useContextsStore.getState().saveClusterGroup(qa)).resolves.toBe(true);
      expect(client.setClusterGroup).toHaveBeenCalledWith(qa);
      expect(groups.has("QA")).toBe(true);
      expect(useContextsStore.getState().clusterGroups.map((g) => g.name)).toEqual([
        "Production",
        "QA",
        "Staging",
      ]);

      // Save replaces an existing group by name.
      await useContextsStore.getState().saveClusterGroup({ name: "Staging", contexts: ["a", "b"] });
      expect(useContextsStore.getState().clusterGroups.find((g) => g.name === "Staging")).toEqual({
        name: "Staging",
        contexts: ["a", "b"],
      });
      expect(useContextsStore.getState().clusterGroups).toHaveLength(3);

      // Delete removes it.
      await expect(useContextsStore.getState().deleteClusterGroup("QA")).resolves.toBe(true);
      expect(client.deleteClusterGroup).toHaveBeenCalledWith("QA");
      expect(groups.has("QA")).toBe(false);
      expect(useContextsStore.getState().clusterGroups.map((g) => g.name)).toEqual([
        "Production",
        "Staging",
      ]);
      expect(useContextsStore.getState().clusterGroupsError).toBeNull();
    });

    it("surfaces typed cluster group errors in store state and keeps the existing groups", async () => {
      const existing: ClusterGroup[] = [{ name: "Production", contexts: ["p"] }];
      useContextsStore.setState({ clusterGroups: existing });
      const err: IpcError = { kind: "invalidInput", message: "group name must not be empty" };
      setClusterGroupClient({
        getClusterGroups: vi.fn(() => Promise.resolve({ status: "error" as const, error: err })),
        setClusterGroup: vi.fn(() => Promise.resolve({ status: "error" as const, error: err })),
        deleteClusterGroup: vi.fn(() => Promise.reject(new Error("Command not allowed by ACL"))),
      });

      await useContextsStore.getState().loadClusterGroups();
      expect(useContextsStore.getState().clusterGroupsStatus).toBe("error");
      expect(useContextsStore.getState().clusterGroupsError).toEqual(err);
      expect(useContextsStore.getState().clusterGroups).toEqual(existing);

      await expect(
        useContextsStore.getState().saveClusterGroup({ name: "", contexts: [] }),
      ).resolves.toBe(false);
      expect(useContextsStore.getState().clusterGroupsError).toEqual(err);
      expect(useContextsStore.getState().clusterGroups).toEqual(existing);

      await expect(useContextsStore.getState().deleteClusterGroup("Production")).resolves.toBe(false);
      expect(useContextsStore.getState().clusterGroupsError).toEqual({
        kind: null,
        message: "Command not allowed by ACL",
      });
      expect(useContextsStore.getState().clusterGroups).toEqual(existing);
    });

    it("reloads team settings through injected client and updates summary and error states", async () => {
      const { client } = fakeClient([]);
      const reloadMock = vi.fn((path: string) =>
        Promise.resolve({
          status: "ok" as const,
          data: {
            loadedPath: path,
            appliedGroups: 1,
            appliedContexts: 3,
            warnings: [],
          },
        }),
      );
      setClusterGroupClient({
        ...client,
        reloadTeamSettings: reloadMock,
      });

      const ok = await useContextsStore.getState().reloadTeamSettings("team-settings.yaml");
      expect(ok).toBe(true);
      expect(reloadMock).toHaveBeenCalledWith("team-settings.yaml");
      expect(client.getClusterGroups).toHaveBeenCalled();
      expect(useContextsStore.getState().teamSettingsStatus).toBe("ready");
      expect(useContextsStore.getState().teamSettingsSummary).toEqual({
        loadedPath: "team-settings.yaml",
        appliedGroups: 1,
        appliedContexts: 3,
        warnings: [],
      });

      // Error handling
      const errMock = vi.fn(() =>
        Promise.resolve({
          status: "error" as const,
          error: { kind: "invalidInput" as const, message: "File not found" },
        }),
      );
      setClusterGroupClient({
        ...client,
        reloadTeamSettings: errMock,
      });

      const failed = await useContextsStore.getState().reloadTeamSettings("missing.yaml");
      expect(failed).toBe(false);
      expect(useContextsStore.getState().teamSettingsStatus).toBe("error");
      expect(useContextsStore.getState().teamSettingsError).toEqual({
        kind: "invalidInput",
        message: "File not found",
      });
    });
  });

  // Test 3: <HelmView /> renders release table and displays values/manifest upon selection.
  it("<HelmView /> renders release table and displays values/manifest upon selection", async () => {
    const user = userEvent.setup();
    const onSelectRelease = vi.fn();

    const releases: HelmRelease[] = [
      {
        name: "ingress-nginx",
        namespace: "ingress",
        revision: 2,
        status: "deployed",
        chart: "ingress-nginx-4.8.3",
        appVersion: "1.9.4",
        updated: "2026-10-31 12:00:00",
        values: "controller:\n  replicaCount: 3\n  service:\n    type: LoadBalancer\n",
        manifest:
          "---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: ingress-nginx-controller\nspec:\n  replicas: 3\n",
        notes: "The ingress-nginx controller has been successfully installed.",
      },
      {
        name: "redis-cluster",
        namespace: "cache",
        revision: 1,
        status: "failed",
        chart: "redis-6.2.1",
        appVersion: "6.2",
        updated: "2026-10-30 09:30:00",
        values: "cluster:\n  enabled: true\n",
        manifest: "---\napiVersion: v1\nkind: Service\nmetadata:\n  name: redis-leader\n",
        notes: "Redis cluster setup failed due to PVC error.",
      },
    ];

    render(<HelmView releases={releases} onSelectRelease={onSelectRelease} />);

    // Renders table with all required columns
    expect(screen.getByRole("table", { name: "Helm Releases List" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Name" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Namespace" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Revision" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Status" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Chart" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "App Version" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Updated" })).toBeInTheDocument();

    // Release rows are present
    expect(screen.getByText("ingress-nginx")).toBeInTheDocument();
    expect(screen.getByText("ingress")).toBeInTheDocument();
    expect(screen.getByText("ingress-nginx-4.8.3")).toBeInTheDocument();
    expect(screen.getByText("1.9.4")).toBeInTheDocument();

    expect(screen.getByText("redis-cluster")).toBeInTheDocument();
    expect(screen.getByText("redis-6.2.1")).toBeInTheDocument();

    // Initially no detail panel is open
    expect(screen.queryByTestId("helm-detail-panel")).not.toBeInTheDocument();

    // Click on ingress-nginx release row
    await user.click(screen.getByText("ingress-nginx"));

    expect(onSelectRelease).toHaveBeenCalledWith(releases[0]);

    // Detail panel is now displayed
    const detailPanel = screen.getByTestId("helm-detail-panel");
    expect(detailPanel).toBeInTheDocument();

    // Check Values tab displays YAML values
    expect(screen.getByRole("tab", { name: /values/i })).toHaveAttribute("aria-selected", "true");
    const valuesContent = screen.getByTestId("helm-values-content");
    expect(valuesContent).toHaveTextContent("replicaCount: 3");
    expect(valuesContent).toHaveTextContent("type: LoadBalancer");

    // Click Manifest tab
    const manifestTab = screen.getByRole("tab", { name: /manifest/i });
    await user.click(manifestTab);
    expect(manifestTab).toHaveAttribute("aria-selected", "true");

    const manifestContent = screen.getByTestId("helm-manifest-content");
    expect(manifestContent).toHaveTextContent("ingress-nginx-controller");
    expect(manifestContent).toHaveTextContent("kind: Deployment");

    // Click Notes tab
    const notesTab = screen.getByRole("tab", { name: /notes/i });
    await user.click(notesTab);
    expect(notesTab).toHaveAttribute("aria-selected", "true");

    const notesContent = screen.getByTestId("helm-notes-content");
    expect(notesContent).toHaveTextContent("The ingress-nginx controller has been successfully installed.");
  });
});
