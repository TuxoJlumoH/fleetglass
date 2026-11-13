import { useEffect, useRef, useState } from "react";
import type { PluginManifest } from "../generated/bindings";
import { useContextsStore } from "../stores/contexts";
import { usePodsStore } from "../stores/pods";

export interface PluginSandboxViewProps {
  manifest: PluginManifest;
  htmlContent?: string; // used for testing or loaded from entrypoint
  onClose?: () => void;
  resolveContexts?: () => unknown;
  resolveResources?: (payload?: unknown) => unknown;
}

export interface PluginRequest {
  type: "fleetglass:request";
  id: string;
  action: "read:contexts" | "read:resources";
  payload?: unknown;
}

export interface PluginResponse {
  type: "fleetglass:response";
  id: string;
  data?: unknown;
  error?: string;
}

/**
 * URL of a plugin file served by the app's `fgplugin` scheme. WebView2 (Windows) and
 * Android cannot load custom schemes in frames; wry serves them as
 * `http(s)://fgplugin.localhost/<id>/<path>` (same http/https as the app origin
 * `http(s)://tauri.localhost`). Elsewhere each plugin has its own `fgplugin://<id>` origin.
 */
function pluginSrc(manifest: PluginManifest): string {
  const path = manifest.entrypoint.replace(/^\/+/, "");
  const id = encodeURIComponent(manifest.id);
  const { protocol, hostname } = window.location;
  if (hostname === "tauri.localhost") {
    return `${protocol}//fgplugin.localhost/${id}/${path}`;
  }
  // Dev server (`http://localhost:1420`) on WebView2/Android.
  if (/Windows|Android/.test(navigator.userAgent)) {
    return `http://fgplugin.localhost/${id}/${path}`;
  }
  return `fgplugin://${id}/${path}`;
}

export function PluginSandboxView({
  manifest,
  htmlContent,
  onClose,
  resolveContexts,
  resolveResources,
}: PluginSandboxViewProps) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const loadCount = useRef(0);
  const [navigatedAway, setNavigatedAway] = useState(false);
  const [frameKey, setFrameKey] = useState(0);

  useEffect(() => {
    const handleMessage = (event: MessageEvent) => {
      void (async () => {
        const iframe = iframeRef.current;
        // Listens to window message events with event.source === iframeRef.current?.contentWindow
        if (!iframe?.contentWindow || event.source !== iframe.contentWindow) {
          return;
        }
        // Best effort: requests a navigated document sends before its load event fires
        // are still answered (with this manifest's permissions, never another plugin's).
        if (loadCount.current > 1) {
          return;
        }

        const data = event.data as {
          type?: unknown;
          id?: unknown;
          action?: unknown;
          payload?: unknown;
        } | null;

        if (!data || typeof data !== "object") {
          return;
        }

        // Message request format: { type: "fleetglass:request", id: string, action: "read:contexts" | "read:resources", payload?: unknown }
        if (data.type !== "fleetglass:request") {
          return;
        }

        const { id, action, payload } = data;
        if (typeof id !== "string" || typeof action !== "string") {
          return;
        }

        // Validates that manifest.permissions includes the requested permission.
        if (!manifest.permissions.includes(action)) {
          iframe.contentWindow.postMessage(
            {
              type: "fleetglass:response",
              id,
              error: `Permission denied for action: ${action}`,
            },
            "*",
          );
          return;
        }

        // If allowed: resolves data (e.g. contexts from useContextsStore.getState().contexts or mock provider)
        try {
          let result: unknown;
          if (action === "read:contexts") {
            result = resolveContexts
              ? await resolveContexts()
              : useContextsStore.getState().contexts;
          } else if (action === "read:resources") {
            result = resolveResources
              ? await resolveResources(payload)
              : Object.values(usePodsStore.getState().rowsByIndex);
          } else if (resolveResources) {
            result = await resolveResources(payload);
          } else {
            result = null;
          }

          iframe.contentWindow.postMessage(
            {
              type: "fleetglass:response",
              id,
              data: result,
            },
            "*",
          );
        } catch (err) {
          iframe.contentWindow.postMessage(
            {
              type: "fleetglass:response",
              id,
              error: err instanceof Error ? err.message : String(err),
            },
            "*",
          );
        }
      })();
    };

    window.addEventListener("message", handleMessage);
    return () => {
      window.removeEventListener("message", handleMessage);
    };
  }, [manifest, resolveContexts, resolveResources]);

  return (
    <div className="plugin-sandbox" data-testid="plugin-sandbox-view">
      <header className="plugin-sandbox__header">
        <div className="plugin-sandbox__title-area">
          <span className="plugin-sandbox__title">{manifest.name}</span>
          <span className="plugin-sandbox__version">v{manifest.version}</span>
          {manifest.description ? (
            <span className="plugin-sandbox__description" title={manifest.description}>
              {manifest.description}
            </span>
          ) : null}
        </div>

        <div className="plugin-sandbox__permissions" aria-label="Plugin permissions">
          {manifest.permissions.map((perm) => (
            <span key={perm} className="plugin-sandbox__permission-chip">
              {perm}
            </span>
          ))}
        </div>

        {onClose ? (
          <button
            type="button"
            className="plugin-sandbox__close-btn"
            aria-label="Close plugin"
            data-testid="plugin-close-btn"
            onClick={onClose}
          >
            ×
          </button>
        ) : null}
      </header>

      <div className="plugin-sandbox__body" data-testid="plugin-sandbox-body">
        <iframe
          key={frameKey}
          ref={iframeRef}
          className={
            navigatedAway
              ? "plugin-sandbox__iframe plugin-sandbox__iframe--stale"
              : "plugin-sandbox__iframe"
          }
          data-testid="plugin-iframe"
          sandbox="allow-scripts"
          title={manifest.name}
          srcDoc={htmlContent}
          src={
            htmlContent !== undefined
              ? undefined
              : pluginSrc(manifest)
          }
          onLoad={() => {
            loadCount.current += 1;
            // A second load means the frame navigated (possibly to another plugin);
            // its window keeps the same identity, so stop answering it.
            if (loadCount.current > 1) {
              setNavigatedAway(true);
            }
          }}
        />
        {navigatedAway ? (
          <div className="plugin-sandbox__navigated" role="alert">
            <span>
              This plugin navigated away from its page, so it no longer gets cluster data.
            </span>
            <button
              type="button"
              className="plugin-sandbox__navigated-btn"
              onClick={() => {
                loadCount.current = 0;
                setNavigatedAway(false);
                setFrameKey((k) => k + 1);
              }}
            >
              Reload plugin
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}
