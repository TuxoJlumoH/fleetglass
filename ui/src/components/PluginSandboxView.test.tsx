import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PluginManifest } from "../generated/bindings";
import { initialContextsState, useContextsStore } from "../stores/contexts";
import { initialPodsState, usePodsStore } from "../stores/pods";
import { PluginSandboxView } from "./PluginSandboxView";

describe("PluginSandboxView (P3-04)", () => {
  const sampleManifest: PluginManifest = {
    id: "sample-pod-counter",
    name: "Sample Pod Counter",
    version: "0.1.0",
    description: "Displays pod counts across connected clusters",
    permissions: ["read:contexts", "read:resources"],
    entrypoint: "index.html",
  };

  beforeEach(() => {
    useContextsStore.setState(initialContextsState);
    usePodsStore.setState(initialPodsState);
  });

  it("renders iframe with sandbox=\"allow-scripts\" and correct attributes", () => {
    render(<PluginSandboxView manifest={sampleManifest} htmlContent="<div>Plugin HTML</div>" />);

    const iframe = screen.getByTestId<HTMLIFrameElement>("plugin-iframe");
    expect(iframe).toBeInTheDocument();
    expect(iframe).toHaveAttribute("sandbox", "allow-scripts");
    expect(iframe).toHaveAttribute("title", "Sample Pod Counter");
    expect(iframe).toHaveAttribute("srcdoc", "<div>Plugin HTML</div>");

    // Header metadata
    expect(screen.getByText("Sample Pod Counter")).toBeInTheDocument();
    expect(screen.getByText("v0.1.0")).toBeInTheDocument();
    expect(screen.getByText("Displays pod counts across connected clusters")).toBeInTheDocument();
    expect(screen.getByText("read:contexts")).toBeInTheDocument();
    expect(screen.getByText("read:resources")).toBeInTheDocument();
  });

  it("renders iframe with fgplugin custom scheme source when htmlContent is omitted", () => {
    render(<PluginSandboxView manifest={sampleManifest} />);
    const iframe = screen.getByTestId<HTMLIFrameElement>("plugin-iframe");
    expect(iframe.src).toBe(`fgplugin://${sampleManifest.id}/${sampleManifest.entrypoint}`);
    expect(iframe.getAttribute("sandbox")).toBe("allow-scripts");
  });

  it.each([
    ["http://tauri.localhost/", "", "http://fgplugin.localhost/sample-pod-counter/index.html"],
    ["https://tauri.localhost/", "", "https://fgplugin.localhost/sample-pod-counter/index.html"],
    [
      "http://localhost:1420/",
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Edg/130.0",
      "http://fgplugin.localhost/sample-pod-counter/index.html",
    ],
    ["tauri://localhost/", "Mozilla/5.0 (X11; Linux x86_64)", "fgplugin://sample-pod-counter/index.html"],
  ])("on app origin %s (UA %s) uses %s", (origin, userAgent, expected) => {
    const location = new URL(origin);
    vi.spyOn(window, "location", "get").mockReturnValue(location as unknown as Location);
    if (userAgent) {
      vi.spyOn(navigator, "userAgent", "get").mockReturnValue(userAgent);
    }
    try {
      render(<PluginSandboxView manifest={sampleManifest} />);
      expect(screen.getByTestId<HTMLIFrameElement>("plugin-iframe").getAttribute("src")).toBe(
        expected,
      );
    } finally {
      vi.restoreAllMocks();
    }
  });

  it("stops answering a frame after it navigates away and offers a reload", async () => {
    const user = userEvent.setup();
    render(<PluginSandboxView manifest={sampleManifest} />);
    const iframe = screen.getByTestId<HTMLIFrameElement>("plugin-iframe");
    const postMessageSpy = vi.fn();
    if (iframe.contentWindow) {
      iframe.contentWindow.postMessage = postMessageSpy;
    }
    const request = () => {
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { type: "fleetglass:request", id: "after-nav", action: "read:contexts" },
          source: iframe.contentWindow,
        }),
      );
    };
    // First load is the plugin's own page; a second one is a navigation.
    fireEvent.load(iframe);
    fireEvent.load(iframe);
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("This plugin navigated away from its page");
    // Overlay over the frame (absolutely positioned in the body), not in flow below it.
    expect(alert).toHaveClass("plugin-sandbox__navigated");
    expect(alert.parentElement).toBe(screen.getByTestId("plugin-sandbox-body"));
    expect(screen.getByTestId("plugin-iframe")).toHaveClass("plugin-sandbox__iframe--stale");
    const reload = screen.getByRole("button", { name: "Reload plugin" });
    reload.focus();
    expect(reload).toHaveFocus();
    request();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(postMessageSpy).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Reload plugin" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByTestId("plugin-iframe")).not.toBe(iframe);
    expect(screen.getByTestId("plugin-iframe")).not.toHaveClass("plugin-sandbox__iframe--stale");
  });

  it("tests that a request for an authorized permission receives response", async () => {
    useContextsStore.setState({
      contexts: [
        {
          name: "kind-fleetglass-dev",
          cluster: "fleetglass-dev",
          user: "kind-user",
          namespace: "default",
          server: "https://127.0.0.1:6443",
          auth: { type: "none" },
          isCurrent: true,
        },
      ],
    });

    render(<PluginSandboxView manifest={sampleManifest} />);

    const iframe = screen.getByTestId<HTMLIFrameElement>("plugin-iframe");
    expect(iframe.contentWindow).not.toBeNull();

    const postMessageSpy = vi.fn();
    if (iframe.contentWindow) {
      iframe.contentWindow.postMessage = postMessageSpy;
    }

    // Dispatch message request for authorized action "read:contexts"
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          type: "fleetglass:request",
          id: "req-101",
          action: "read:contexts",
        },
        source: iframe.contentWindow,
      }),
    );

    await waitFor(() => {
      expect(postMessageSpy).toHaveBeenCalledWith(
        {
          type: "fleetglass:response",
          id: "req-101",
          data: [
            expect.objectContaining({
              name: "kind-fleetglass-dev",
              cluster: "fleetglass-dev",
            }),
          ],
        },
        "*",
      );
    });
  });

  it("tests that a request for an unauthorized permission returns 'Permission denied'", async () => {
    const limitedManifest: PluginManifest = {
      id: "read-only-contexts",
      name: "Only Contexts",
      version: "0.0.1",
      description: "Only reads contexts",
      permissions: ["read:contexts"],
      entrypoint: "index.html",
    };

    render(<PluginSandboxView manifest={limitedManifest} />);

    const iframe = screen.getByTestId<HTMLIFrameElement>("plugin-iframe");
    const postMessageSpy = vi.fn();
    if (iframe.contentWindow) {
      iframe.contentWindow.postMessage = postMessageSpy;
    }

    // Request unauthorized action "read:resources"
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          type: "fleetglass:request",
          id: "req-unauth",
          action: "read:resources",
        },
        source: iframe.contentWindow,
      }),
    );

    await waitFor(() => {
      expect(postMessageSpy).toHaveBeenCalledWith(
        {
          type: "fleetglass:response",
          id: "req-unauth",
          error: "Permission denied for action: read:resources",
        },
        "*",
      );
    });
  });

  it("tests close button triggers onClose", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();

    render(<PluginSandboxView manifest={sampleManifest} onClose={onClose} />);

    const closeBtn = screen.getByRole("button", { name: "Close plugin" });
    expect(closeBtn).toBeInTheDocument();

    await user.click(closeBtn);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("ignores messages from untrusted external sources", async () => {
    render(<PluginSandboxView manifest={sampleManifest} />);

    const iframe = screen.getByTestId<HTMLIFrameElement>("plugin-iframe");
    const postMessageSpy = vi.fn();
    if (iframe.contentWindow) {
      iframe.contentWindow.postMessage = postMessageSpy;
    }

    // Message from window itself, not iframe.contentWindow
    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          type: "fleetglass:request",
          id: "fake-req",
          action: "read:contexts",
        },
        source: window,
      }),
    );

    // Give microtasks a chance to process
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(postMessageSpy).not.toHaveBeenCalled();
  });

  it("resolves resources when authorized using custom resolver or store", async () => {
    const mockResources = [
      { uid: "pod-1", name: "nginx-1", cluster: "kind-dev" },
      { uid: "pod-2", name: "nginx-2", cluster: "kind-dev" },
    ];
    const resolveResources = vi.fn().mockResolvedValue(mockResources);

    render(
      <PluginSandboxView
        manifest={sampleManifest}
        resolveResources={resolveResources}
      />,
    );

    const iframe = screen.getByTestId<HTMLIFrameElement>("plugin-iframe");
    const postMessageSpy = vi.fn();
    if (iframe.contentWindow) {
      iframe.contentWindow.postMessage = postMessageSpy;
    }

    window.dispatchEvent(
      new MessageEvent("message", {
        data: {
          type: "fleetglass:request",
          id: "req-resources",
          action: "read:resources",
          payload: { namespace: "default" },
        },
        source: iframe.contentWindow,
      }),
    );

    await waitFor(() => {
      expect(resolveResources).toHaveBeenCalledWith({ namespace: "default" });
      expect(postMessageSpy).toHaveBeenCalledWith(
        {
          type: "fleetglass:response",
          id: "req-resources",
          data: mockResources,
        },
        "*",
      );
    });
  });
});
