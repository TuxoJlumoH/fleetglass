import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Row } from "../stores/pods";
import { ResourceDrawer } from "./ResourceDrawer";

const sampleRow: Row = {
  uid: "uid-web-0",
  cluster: "prod",
  kind: "Pod",
  namespace: "default",
  name: "web-0",
  status: "healthy",
  statusReason: "Running",
  cells: [
    { type: "ready", value: { ready: 1, total: 1 } },
    { type: "status", value: { label: "Running", phase: "healthy" } },
    { type: "int", value: 0 },
    { type: "text", value: "node-1" },
    { type: "text", value: "10.244.0.10" },
    { type: "timestamp", value: "2026-01-01T00:00:00Z" },
  ],
  labels: [
    ["app", "web"],
    ["tier", "frontend"],
  ],
  custom: [],
};

describe("P1-05, P1-06, P1-08 ResourceDrawer", () => {
  // P1-07 (kubectl preview, read-only and typed-name gates) is covered against the backend
  // commands in ActionConfirmMount.test.tsx since R-07.

  it("P1-05 & P1-06: renders ResourceDrawer Summary, switches to YAML tab, and runs Dry-run & Diff showing diff and kubectl preview", async () => {
    const user = userEvent.setup();
    const onDryRunApply = vi.fn((yaml: string) =>
      Promise.resolve({
        diff: `--- live\n+++ dry-run\n+${yaml.trim()}`,
        kubectlPreview: "kubectl --context prod -n default apply -f - --dry-run=server",
      }),
    );

    render(
      <ResourceDrawer
        row={sampleRow}
        yamlText={"apiVersion: v1\nkind: Pod\nmetadata:\n  name: web-0\n"}
        onClose={vi.fn()}
        onDryRunApply={onDryRunApply}
      />,
    );

    const drawer = screen.getByRole("region", { name: "Resource details" });
    expect(drawer).toBeInTheDocument();
    expect(screen.getByText("default")).toBeInTheDocument();
    expect(screen.getByText("prod")).toBeInTheDocument();
    // Shown in the Summary and (since 02-06c) in the drawer header.
    expect(screen.getAllByText("Running")).toHaveLength(2);
    expect(screen.getByTestId("drawer-status-reason")).toHaveTextContent("Running");
    expect(screen.getByText("app=web")).toBeInTheDocument();
    expect(screen.getByText("tier=frontend")).toBeInTheDocument();

    // Switch to YAML tab
    await user.click(screen.getByRole("tab", { name: "YAML" }));
    const yamlEditor = screen.getByLabelText("Resource YAML");
    expect(yamlEditor).toHaveValue("apiVersion: v1\nkind: Pod\nmetadata:\n  name: web-0\n");

    // Trigger Dry-run & Diff
    await user.click(screen.getByRole("button", { name: "Dry-run & Diff" }));

    expect(onDryRunApply).toHaveBeenCalledTimes(1);
    expect(await screen.findByTestId("yaml-kubectl-preview")).toHaveTextContent(
      "kubectl --context prod -n default apply -f - --dry-run=server",
    );
    expect(screen.getByTestId("yaml-diff-preview")).toHaveTextContent("--- live");
  });

  it("P1-08: switches to Logs tab, filters log lines by regex, and toggles Wrap lines and Previous container", async () => {
    const user = userEvent.setup();
    const logLines = [
      { seq: 1, text: "2026-01-01T00:00:01Z INFO starting server on :8080" },
      { seq: 2, text: "2026-01-01T00:00:02Z ERROR upstream connection timeout after 5s" },
      { seq: 3, text: "2026-01-01T00:00:03Z WARN retrying request" },
      { seq: 4, text: "2026-01-01T00:00:04Z ERROR db query timeout exceeded" },
    ];

    render(<ResourceDrawer row={sampleRow} logLines={logLines} onClose={vi.fn()} />);

    await user.click(screen.getByRole("tab", { name: "Logs" }));

    const logContainer = screen.getByTestId("log-lines");
    expect(logContainer).toHaveTextContent("starting server on :8080");
    expect(logContainer).toHaveTextContent("upstream connection timeout after 5s");

    // Regex filter
    const regexInput = screen.getByLabelText("Search logs (regex)");
    await user.type(regexInput, "ERROR.*timeout");

    expect(logContainer).not.toHaveTextContent("starting server on :8080");
    expect(logContainer).not.toHaveTextContent("retrying request");
    expect(logContainer).toHaveTextContent("ERROR upstream connection timeout after 5s");
    expect(logContainer).toHaveTextContent("ERROR db query timeout exceeded");

    // Toggle Wrap lines and Previous container
    const wrapToggle = screen.getByLabelText("Wrap log lines");
    const prevToggle = screen.getByLabelText("Previous container logs");
    const followToggle = screen.getByLabelText("Follow logs");

    expect(followToggle).toBeChecked();
    expect(wrapToggle).not.toBeChecked();
    expect(prevToggle).not.toBeChecked();

    await user.click(wrapToggle);
    await user.click(prevToggle);

    expect(wrapToggle).toBeChecked();
    expect(prevToggle).toBeChecked();
    expect(logContainer).toHaveAttribute("data-wrap", "true");
    expect(logContainer).toHaveAttribute("data-previous", "true");
  });
});
