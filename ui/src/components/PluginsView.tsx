import { useEffect, useState } from "react";
import { commands, type PluginManifest } from "../generated/bindings";
import { PluginSandboxView } from "./PluginSandboxView";

export interface PluginsViewProps {
  fetchPlugins?: () => Promise<PluginManifest[]>;
  initialPlugin?: PluginManifest | null;
  htmlContent?: string;
}

const defaultFetchPlugins = async (): Promise<PluginManifest[]> => {
  const res = await commands.listPlugins();
  if (res.status === "error") {
    throw new Error(res.error.message);
  }
  return res.data;
};

export function PluginsView({
  fetchPlugins = defaultFetchPlugins,
  initialPlugin = null,
  htmlContent,
}: PluginsViewProps) {
  const [pluginsState, setPluginsState] = useState<{
    loaded: boolean;
    plugins: PluginManifest[];
    error: string | null;
  }>({
    loaded: false,
    plugins: [],
    error: null,
  });
  const [selectedPlugin, setSelectedPlugin] = useState<PluginManifest | null>(initialPlugin);

  useEffect(() => {
    let cancelled = false;

    fetchPlugins()
      .then((data) => {
        if (cancelled) return;
        setPluginsState({
          loaded: true,
          plugins: data,
          error: null,
        });
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setPluginsState({
          loaded: true,
          plugins: [],
          error: err instanceof Error ? err.message : String(err),
        });
      });

    return () => {
      cancelled = true;
    };
  }, [fetchPlugins]);

  const loading = !pluginsState.loaded;
  const error = pluginsState.error;
  const plugins = pluginsState.plugins;

  if (selectedPlugin) {
    return (
      <PluginSandboxView
        manifest={selectedPlugin}
        {...(htmlContent !== undefined ? { htmlContent } : {})}
        onClose={() => {
          setSelectedPlugin(null);
        }}
      />
    );
  }

  return (
    <div className="plugins-view" data-testid="plugins-view" aria-label="Plugins View">
      <div className="plugins-view__header">
        <h2 className="plugins-view__title">Installed Plugins</h2>
      </div>

      {loading && (
        <div role="status" className="loading-state plugins-view__loading" data-testid="plugins-loading">
          Loading plugins…
        </div>
      )}

      {error !== null && (
        <div role="alert" className="error-state plugins-view__error" data-testid="plugins-error">
          Failed to load plugins: {error}
        </div>
      )}

      {!loading && error === null && plugins.length === 0 && (
        <div className="empty-state plugins-view__empty" data-testid="plugins-empty">
          No plugins installed
        </div>
      )}

      {!loading && error === null && plugins.length > 0 && (
        <div className="plugins-view__grid" data-testid="plugins-grid">
          {plugins.map((plugin) => (
            <div key={plugin.id} className="plugins-view__card" data-testid={`plugin-card-${plugin.id}`}>
              <div className="plugins-view__card-header">
                <div className="plugins-view__card-title-group">
                  <h3 className="plugins-view__card-title">{plugin.name}</h3>
                  <span className="badge badge--tag">v{plugin.version}</span>
                </div>
                <button
                  type="button"
                  className="btn btn--sm btn--primary plugins-view__open-btn"
                  aria-label={`Open ${plugin.name}`}
                  onClick={() => {
                    setSelectedPlugin(plugin);
                  }}
                >
                  Open Plugin
                </button>
              </div>

              {plugin.description && (
                <p className="plugins-view__card-desc">{plugin.description}</p>
              )}

              <div className="plugins-view__permissions" aria-label="Permissions">
                {plugin.permissions.map((perm) => (
                  <span key={perm} className="plugin-sandbox__permission-chip">
                    {perm}
                  </span>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
