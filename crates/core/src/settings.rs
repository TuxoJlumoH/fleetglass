//! Persistent per-context settings (`colour`, `tags`, `read_only`, `protected`, `pinned`).
//!
//! Stored locally as pretty-printed JSON so users can inspect or version-control
//! their context preferences without touching `~/.kube/config`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::api::{ClusterGroup, ColumnLayout, ContextSettings, PrometheusConfig, SavedPortForward};
use crate::error::{Error, Result};

/// Thread-safe store for per-context settings backed by a JSON file on disk.
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    entries: RwLock<BTreeMap<String, ContextSettings>>,
    cluster_groups: RwLock<BTreeMap<String, ClusterGroup>>,
    column_layouts: RwLock<BTreeMap<String, ColumnLayout>>,
    /// Opt-in update check (`P2-10`, non-negotiable #1). Off unless the user enables it;
    /// persisted in `app-settings.json` next to `path`.
    update_check_enabled: AtomicBool,
    /// Per-context Prometheus settings (`P2-09`), persisted in `prometheus.json` next to `path`.
    prometheus: RwLock<BTreeMap<String, PrometheusConfig>>,
    /// Saved port-forward bookmarks (`04-08`), persisted in `saved-port-forwards.json` next to `path`.
    saved_port_forwards: RwLock<BTreeMap<String, SavedPortForward>>,
    /// Temporarily unlocked contexts with expiration unix timestamp in seconds (`04-10`).
    unlocked_contexts: RwLock<BTreeMap<String, u32>>,
}

/// On-disk shape of `app-settings.json` (app-wide, not per context).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AppSettingsFile {
    update_check_enabled: bool,
}

impl SettingsStore {
    /// Creates a store backed by `path`, loading existing JSON entries if the file exists.
    pub fn new(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, ContextSettings>>(&raw).ok())
            .unwrap_or_default();
        let groups_path =
            if path.file_name().and_then(|n| n.to_str()) == Some("cluster-groups.json") {
                path.clone()
            } else {
                path.with_file_name("cluster-groups.json")
            };
        let cluster_groups = std::fs::read_to_string(&groups_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, ClusterGroup>>(&raw).ok())
            .unwrap_or_default();
        let column_layouts = std::fs::read_to_string(path.with_file_name("column-layouts.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, ColumnLayout>>(&raw).ok())
            .unwrap_or_default();
        // A missing or unreadable file means the default: update check off.
        let app_settings = std::fs::read_to_string(path.with_file_name("app-settings.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<AppSettingsFile>(&raw).ok())
            .unwrap_or_default();
        let prometheus = std::fs::read_to_string(path.with_file_name("prometheus.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, PrometheusConfig>>(&raw).ok())
            .unwrap_or_default();
        let saved_port_forwards =
            std::fs::read_to_string(path.with_file_name("saved-port-forwards.json"))
                .ok()
                .and_then(|raw| {
                    serde_json::from_str::<BTreeMap<String, SavedPortForward>>(&raw).ok()
                })
                .unwrap_or_default();
        Self {
            path,
            entries: RwLock::new(entries),
            cluster_groups: RwLock::new(cluster_groups),
            column_layouts: RwLock::new(column_layouts),
            update_check_enabled: AtomicBool::new(app_settings.update_check_enabled),
            prometheus: RwLock::new(prometheus),
            saved_port_forwards: RwLock::new(saved_port_forwards),
            unlocked_contexts: RwLock::new(BTreeMap::new()),
        }
    }

    fn layouts_path(&self) -> PathBuf {
        self.path.with_file_name("column-layouts.json")
    }

    fn app_settings_path(&self) -> PathBuf {
        self.path.with_file_name("app-settings.json")
    }

    /// Returns `true` only if the user opted in to the update check (default `false`).
    pub fn update_check_enabled(&self) -> bool {
        self.update_check_enabled.load(Ordering::Relaxed)
    }

    /// Enables or disables the opt-in update check and persists the choice.
    /// The in-memory value is only changed once the file is written.
    pub fn set_update_check_enabled(&self, enabled: bool) -> Result<()> {
        let path = self.app_settings_path();
        let io_err = |source| Error::KubeconfigRead {
            path: path.clone(),
            source,
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let json = serde_json::to_string_pretty(&AppSettingsFile {
            update_check_enabled: enabled,
        })
        .map_err(|err| io_err(std::io::Error::other(err)))?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(io_err)?;
        std::fs::rename(&tmp_path, &path).map_err(io_err)?;
        self.update_check_enabled.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    fn groups_path(&self) -> PathBuf {
        if self.path.file_name().and_then(|n| n.to_str()) == Some("cluster-groups.json") {
            self.path.clone()
        } else {
            self.path.with_file_name("cluster-groups.json")
        }
    }

    /// Default settings file path (`$XDG_CONFIG_HOME/fleetglass/context-settings.json`
    /// or `~/.config/fleetglass/context-settings.json`).
    pub fn default_path() -> PathBuf {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg)
                .join("fleetglass")
                .join("context-settings.json");
        }
        if let Some(home) = std::env::home_dir() {
            return home
                .join(".config")
                .join("fleetglass")
                .join("context-settings.json");
        }
        PathBuf::from("context-settings.json")
    }

    /// Returns the settings for `context`, or [`ContextSettings::default`] if unconfigured.
    ///
    /// A poisoned lock still yields the stored settings: falling back to defaults would
    /// silently drop `read_only` / `protected` and open the context for writes.
    /// If the context is temporarily unlocked (`04-10`), `unlocked_until` is populated.
    pub fn get(&self, context: &str) -> ContextSettings {
        let mut settings = self
            .entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(context)
            .cloned()
            .unwrap_or_default();
        if let Some(expiry) = self.get_unlocked_expiry(context) {
            settings.unlocked_until = Some(expiry);
        }
        settings
    }

    /// Returns a snapshot of all configured context settings.
    pub fn get_all(&self) -> BTreeMap<String, ContextSettings> {
        let mut all = self
            .entries
            .read()
            .map(|map| map.clone())
            .unwrap_or_default();
        let now = now_unix_secs();
        if let Ok(unlocked) = self.unlocked_contexts.read() {
            for (ctx, &expiry) in unlocked.iter() {
                if now < expiry {
                    all.entry(ctx.clone()).or_default().unlocked_until = Some(expiry);
                }
            }
        }
        all
    }

    /// Returns the expiration unix timestamp if `context` is currently temporarily unlocked (`04-10`).
    pub fn get_unlocked_expiry(&self, context: &str) -> Option<u32> {
        let now = now_unix_secs();
        let guard = self.unlocked_contexts.read().ok()?;
        if let Some(&expiry) = guard.get(context)
            && now < expiry
        {
            return Some(expiry);
        }
        None
    }

    /// Temporarily unlocks a context for `duration_secs` (e.g. 15 minutes = 900 s) (`04-10`).
    /// Returns the expiration epoch timestamp in seconds.
    pub fn unlock_context(&self, context: &str, duration_secs: u32) -> u32 {
        let now = now_unix_secs();
        let expiry = now.saturating_add(duration_secs);
        if let Ok(mut guard) = self.unlocked_contexts.write() {
            guard.insert(context.to_owned(), expiry);
        }
        expiry
    }

    /// Relocks a temporarily unlocked context (`04-10`). Returns true if it was unlocked.
    pub fn relock_context(&self, context: &str) -> bool {
        if let Ok(mut guard) = self.unlocked_contexts.write() {
            guard.remove(context).is_some()
        } else {
            false
        }
    }

    /// Updates the settings for `context` and persists the full map to disk.
    pub fn set(&self, context: impl Into<String>, settings: ContextSettings) -> Result<()> {
        let mut persistent = settings;
        persistent.unlocked_until = None;
        let snapshot = {
            let mut guard = self.entries.write().map_err(|_| Error::KubeconfigRead {
                path: self.path.clone(),
                source: std::io::Error::other("settings lock poisoned"),
            })?;
            guard.insert(context.into(), persistent);
            guard.clone()
        };

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| Error::KubeconfigRead {
                path: self.path.clone(),
                source,
            })?;
        }

        let json =
            serde_json::to_string_pretty(&snapshot).map_err(|err| Error::KubeconfigRead {
                path: self.path.clone(),
                source: std::io::Error::other(err),
            })?;

        let tmp_path = self.path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(|source| Error::KubeconfigRead {
            path: self.path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &self.path).map_err(|source| Error::KubeconfigRead {
            path: self.path.clone(),
            source,
        })?;

        Ok(())
    }

    /// Returns `true` if `context` is marked `read_only`.
    pub fn is_read_only(&self, context: &str) -> bool {
        self.get(context).read_only
    }

    /// Returns a list of all configured cluster groups sorted by name.
    pub fn get_cluster_groups(&self) -> Vec<ClusterGroup> {
        self.cluster_groups
            .read()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Returns the cluster group with `name`, if configured.
    pub fn get_cluster_group(&self, name: &str) -> Option<ClusterGroup> {
        self.cluster_groups
            .read()
            .ok()
            .and_then(|map| map.get(name).cloned())
    }

    /// Adds or updates a named [`ClusterGroup`] and persists it to disk.
    pub fn set_cluster_group(&self, group: ClusterGroup) -> Result<()> {
        let groups_path = self.groups_path();
        let snapshot = {
            let mut guard = self
                .cluster_groups
                .write()
                .map_err(|_| Error::KubeconfigRead {
                    path: groups_path.clone(),
                    source: std::io::Error::other("cluster groups lock poisoned"),
                })?;
            guard.insert(group.name.clone(), group);
            guard.clone()
        };

        if let Some(parent) = groups_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| Error::KubeconfigRead {
                path: groups_path.clone(),
                source,
            })?;
        }

        let json =
            serde_json::to_string_pretty(&snapshot).map_err(|err| Error::KubeconfigRead {
                path: groups_path.clone(),
                source: std::io::Error::other(err),
            })?;

        let tmp_path = groups_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(|source| Error::KubeconfigRead {
            path: groups_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &groups_path).map_err(|source| Error::KubeconfigRead {
            path: groups_path.clone(),
            source,
        })?;

        Ok(())
    }

    /// Deletes a [`ClusterGroup`] by name.
    pub fn delete_cluster_group(&self, name: &str) -> Result<()> {
        let groups_path = self.groups_path();
        let snapshot = {
            let mut guard = self
                .cluster_groups
                .write()
                .map_err(|_| Error::KubeconfigRead {
                    path: groups_path.clone(),
                    source: std::io::Error::other("cluster groups lock poisoned"),
                })?;
            guard.remove(name);
            guard.clone()
        };

        if let Some(parent) = groups_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| Error::KubeconfigRead {
                path: groups_path.clone(),
                source,
            })?;
        }

        let json =
            serde_json::to_string_pretty(&snapshot).map_err(|err| Error::KubeconfigRead {
                path: groups_path.clone(),
                source: std::io::Error::other(err),
            })?;

        let tmp_path = groups_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(|source| Error::KubeconfigRead {
            path: groups_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &groups_path).map_err(|source| Error::KubeconfigRead {
            path: groups_path.clone(),
            source,
        })?;

        Ok(())
    }

    /// Returns the saved [`ColumnLayout`] for resource `kind`, if any.
    pub fn get_column_layout(&self, kind: &str) -> Option<ColumnLayout> {
        self.column_layouts
            .read()
            .ok()
            .and_then(|map| map.get(kind).cloned())
    }

    /// Validates every custom column path, then saves `layout` (keyed by `kind`) to disk.
    pub fn set_column_layout(&self, layout: ColumnLayout) -> Result<()> {
        for col in &layout.custom {
            crate::columns::compile(&col.json_path)?;
        }

        let layouts_path = self.layouts_path();
        let snapshot = {
            let mut guard = self
                .column_layouts
                .write()
                .map_err(|_| Error::KubeconfigRead {
                    path: layouts_path.clone(),
                    source: std::io::Error::other("column layouts lock poisoned"),
                })?;
            guard.insert(layout.kind.clone(), layout);
            guard.clone()
        };

        if let Some(parent) = layouts_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| Error::KubeconfigRead {
                path: layouts_path.clone(),
                source,
            })?;
        }

        let json =
            serde_json::to_string_pretty(&snapshot).map_err(|err| Error::KubeconfigRead {
                path: layouts_path.clone(),
                source: std::io::Error::other(err),
            })?;

        let tmp_path = layouts_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(|source| Error::KubeconfigRead {
            path: layouts_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &layouts_path).map_err(|source| Error::KubeconfigRead {
            path: layouts_path.clone(),
            source,
        })?;

        Ok(())
    }

    fn prometheus_path(&self) -> PathBuf {
        self.path.with_file_name("prometheus.json")
    }

    /// Returns the Prometheus settings for `context`, or the disabled default.
    pub fn get_prometheus_config(&self, context: &str) -> PrometheusConfig {
        self.prometheus
            .read()
            .ok()
            .and_then(|map| map.get(context).cloned())
            .unwrap_or_default()
    }

    /// Saves the Prometheus settings for `context` and persists them to `prometheus.json`.
    pub fn set_prometheus_config(
        &self,
        context: impl Into<String>,
        config: PrometheusConfig,
    ) -> Result<()> {
        let prometheus_path = self.prometheus_path();
        let io_err = |source| Error::KubeconfigRead {
            path: prometheus_path.clone(),
            source,
        };
        let snapshot = {
            let mut guard = self
                .prometheus
                .write()
                .map_err(|_| io_err(std::io::Error::other("prometheus lock poisoned")))?;
            guard.insert(context.into(), config);
            guard.clone()
        };

        if let Some(parent) = prometheus_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|err| io_err(std::io::Error::other(err)))?;
        let tmp_path = prometheus_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(io_err)?;
        std::fs::rename(&tmp_path, &prometheus_path).map_err(io_err)?;
        Ok(())
    }
    fn saved_port_forwards_path(&self) -> PathBuf {
        self.path.with_file_name("saved-port-forwards.json")
    }

    /// Returns all saved port forwards.
    pub fn get_saved_port_forwards(&self) -> Vec<SavedPortForward> {
        self.saved_port_forwards
            .read()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Persists a saved port forward (`04-08`).
    pub fn save_port_forward(&self, forward: SavedPortForward) -> Result<()> {
        let pf_path = self.saved_port_forwards_path();
        let io_err = |source| Error::KubeconfigRead {
            path: pf_path.clone(),
            source,
        };
        let snapshot = {
            let mut guard = self
                .saved_port_forwards
                .write()
                .map_err(|_| io_err(std::io::Error::other("saved port forwards lock poisoned")))?;
            guard.insert(forward.id.clone(), forward);
            guard.clone()
        };

        if let Some(parent) = pf_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }

        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|err| io_err(std::io::Error::other(err)))?;

        let tmp_path = pf_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(io_err)?;
        std::fs::rename(&tmp_path, &pf_path).map_err(io_err)?;

        Ok(())
    }

    /// Removes a saved port forward by ID. Returns `true` if it was present.
    pub fn delete_saved_port_forward(&self, id: &str) -> Result<bool> {
        let pf_path = self.saved_port_forwards_path();
        let io_err = |source| Error::KubeconfigRead {
            path: pf_path.clone(),
            source,
        };
        let (removed, snapshot) = {
            let mut guard = self
                .saved_port_forwards
                .write()
                .map_err(|_| io_err(std::io::Error::other("saved port forwards lock poisoned")))?;
            let existed = guard.remove(id).is_some();
            (existed, guard.clone())
        };

        if !removed {
            return Ok(false);
        }

        if let Some(parent) = pf_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }

        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|err| io_err(std::io::Error::other(err)))?;

        let tmp_path = pf_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json).map_err(io_err)?;
        std::fs::rename(&tmp_path, &pf_path).map_err(io_err)?;

        Ok(true)
    }
}

fn now_unix_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_config_defaults_disabled_and_persists() {
        let path = unique_temp_path("prometheus");
        let store = SettingsStore::new(path.clone());
        assert!(!store.get_prometheus_config("kind").enabled);

        let cfg = PrometheusConfig {
            enabled: true,
            namespace: "prometheus".to_owned(),
            service: "prometheus-server".to_owned(),
            port: 80,
            scheme: crate::api::PrometheusScheme::Https,
        };
        store.set_prometheus_config("kind", cfg.clone()).unwrap();
        assert_eq!(store.get_prometheus_config("kind"), cfg);
        assert!(path.with_file_name("prometheus.json").exists());

        let reloaded = SettingsStore::new(path.clone());
        assert_eq!(reloaded.get_prometheus_config("kind"), cfg);
        assert!(!reloaded.get_prometheus_config("other").enabled);

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "fleetglass-settings-test-{}-{label}-{nanos}",
                std::process::id()
            ))
            .join("context-settings.json")
    }

    #[test]
    fn get_keeps_write_guards_after_lock_poisoning() {
        let path = unique_temp_path("poisoned");
        let store = std::sync::Arc::new(SettingsStore::new(path.clone()));
        store
            .set(
                "prod-eu",
                ContextSettings {
                    read_only: true,
                    protected: true,
                    ..ContextSettings::default()
                },
            )
            .unwrap();

        let poisoner = std::sync::Arc::clone(&store);
        let joined = std::thread::spawn(move || {
            let _guard = poisoner.entries.write().unwrap();
            panic!("poison the settings lock");
        })
        .join();
        assert!(joined.is_err());
        assert!(store.entries.is_poisoned());

        let settings = store.get("prod-eu");
        assert!(settings.read_only, "read-only flag lost after poisoning");
        assert!(settings.protected, "protected flag lost after poisoning");
        assert!(store.is_read_only("prod-eu"));

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn defaults_and_persists_across_instances() {
        let path = unique_temp_path("persist");
        let store = SettingsStore::new(path.clone());

        assert_eq!(store.get("prod-eu"), ContextSettings::default());
        assert!(!store.is_read_only("prod-eu"));
        assert!(store.get_all().is_empty());

        let prod_settings = ContextSettings {
            colour: Some("#ef4444".to_owned()),
            tags: vec!["prod".to_owned(), "eu".to_owned()],
            read_only: true,
            protected: true,
            pinned: true,
            unlocked_until: None,
        };
        store.set("prod-eu", prod_settings.clone()).unwrap();

        assert_eq!(store.get("prod-eu"), prod_settings);
        assert!(store.is_read_only("prod-eu"));

        let reloaded = SettingsStore::new(path.clone());
        assert_eq!(reloaded.get("prod-eu"), prod_settings);
        assert!(reloaded.is_read_only("prod-eu"));
        assert_eq!(reloaded.get_all().len(), 1);

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn cluster_groups_lifecycle_and_persistence() {
        let path = unique_temp_path("groups");
        let store = SettingsStore::new(path.clone());

        assert!(store.get_cluster_groups().is_empty());
        assert_eq!(store.get_cluster_group("production"), None);

        let prod_group = ClusterGroup {
            name: "production".to_owned(),
            contexts: vec!["prod-eu".to_owned(), "prod-us".to_owned()],
        };
        let staging_group = ClusterGroup {
            name: "staging".to_owned(),
            contexts: vec!["stage-eu".to_owned()],
        };

        store.set_cluster_group(prod_group.clone()).unwrap();
        store.set_cluster_group(staging_group.clone()).unwrap();

        let groups = store.get_cluster_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], prod_group);
        assert_eq!(groups[1], staging_group);
        assert_eq!(
            store.get_cluster_group("production"),
            Some(prod_group.clone())
        );

        // Reload from disk
        let reloaded = SettingsStore::new(path.clone());
        let reloaded_groups = reloaded.get_cluster_groups();
        assert_eq!(reloaded_groups.len(), 2);
        assert_eq!(reloaded_groups[0], prod_group);

        // Delete group
        store.delete_cluster_group("production").unwrap();
        assert_eq!(store.get_cluster_groups().len(), 1);
        assert_eq!(store.get_cluster_group("production"), None);

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    fn sample_layout(json_path: &str) -> ColumnLayout {
        ColumnLayout {
            kind: "Pod".to_owned(),
            visible: vec!["name".to_owned(), "ingress".to_owned()],
            custom: vec![crate::api::ColumnDef {
                id: "ingress".to_owned(),
                title: "Ingress class".to_owned(),
                json_path: json_path.to_owned(),
                width: Some(140),
            }],
            widths: vec![("name".to_owned(), 240)],
        }
    }

    #[test]
    fn column_layouts_round_trip() {
        let path = unique_temp_path("layouts");
        let store = SettingsStore::new(path.clone());
        assert_eq!(store.get_column_layout("Pod"), None);

        let layout = sample_layout(".metadata.annotations['kubernetes.io/ingress.class']");
        store.set_column_layout(layout.clone()).unwrap();
        assert_eq!(store.get_column_layout("Pod"), Some(layout.clone()));
        assert!(path.with_file_name("column-layouts.json").exists());

        let reloaded = SettingsStore::new(path.clone());
        assert_eq!(reloaded.get_column_layout("Pod"), Some(layout));
        assert_eq!(reloaded.get_column_layout("Node"), None);

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn update_check_is_off_by_default_and_persists() {
        let path = unique_temp_path("update-check");
        let store = SettingsStore::new(path.clone());
        assert!(!store.update_check_enabled());

        store.set_update_check_enabled(true).unwrap();
        assert!(store.update_check_enabled());
        assert!(SettingsStore::new(path.clone()).update_check_enabled());

        store.set_update_check_enabled(false).unwrap();
        assert!(!SettingsStore::new(path.clone()).update_check_enabled());

        // A corrupt file falls back to the default (off).
        std::fs::write(path.with_file_name("app-settings.json"), "not json").unwrap();
        assert!(!SettingsStore::new(path.clone()).update_check_enabled());

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn column_layouts_reject_invalid_paths() {
        let path = unique_temp_path("layouts-invalid");
        let store = SettingsStore::new(path.clone());

        let err = store
            .set_column_layout(sample_layout(".metadata.annotations['unclosed"))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidColumnPath { .. }));
        assert_eq!(store.get_column_layout("Pod"), None);
        assert!(!path.with_file_name("column-layouts.json").exists());

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn saved_port_forwards_persistence() {
        let path = unique_temp_path("saved-port-forwards");
        let store = SettingsStore::new(path.clone());
        assert!(store.get_saved_port_forwards().is_empty());

        let forward = SavedPortForward {
            id: "kind/default/deployment/my-service/8080".to_owned(),
            context: "kind".to_owned(),
            namespace: "default".to_owned(),
            target_kind: "deployment".to_owned(),
            target_name: "my-service".to_owned(),
            container_port: 8080,
            local_port: 8080,
            label: "My Service Web".to_owned(),
        };

        store.save_port_forward(forward.clone()).unwrap();
        assert_eq!(store.get_saved_port_forwards(), vec![forward.clone()]);
        assert!(path.with_file_name("saved-port-forwards.json").exists());

        // Reload from disk
        let reloaded = SettingsStore::new(path.clone());
        assert_eq!(reloaded.get_saved_port_forwards(), vec![forward.clone()]);

        // Delete
        assert!(reloaded.delete_saved_port_forward(&forward.id).unwrap());
        assert!(reloaded.get_saved_port_forwards().is_empty());
        assert!(!reloaded.delete_saved_port_forward(&forward.id).unwrap());

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn timed_context_unlock_and_auto_relock() {
        let path = unique_temp_path("unlock-test");
        let store = SettingsStore::new(path.clone());

        // Context is permanently read-only
        store
            .set(
                "prod",
                ContextSettings {
                    read_only: true,
                    protected: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let initial = store.get("prod");
        assert!(initial.read_only);
        assert!(initial.effective_read_only());
        assert!(!initial.is_unlocked());
        assert_eq!(initial.unlocked_until, None);

        // Unlock for 900 seconds (15 minutes)
        let expiry = store.unlock_context("prod", 900);
        let unlocked = store.get("prod");
        assert!(unlocked.read_only); // Base setting preserved
        assert!(unlocked.is_unlocked());
        assert!(!unlocked.effective_read_only());
        assert_eq!(unlocked.unlocked_until, Some(expiry));

        // Disk JSON does not persist unlocked_until
        let reloaded = SettingsStore::new(path.clone());
        assert_eq!(reloaded.get("prod").unlocked_until, None);

        // Relock
        assert!(store.relock_context("prod"));
        let relocked = store.get("prod");
        assert!(relocked.effective_read_only());
        assert!(!relocked.is_unlocked());

        // Expired unlock immediately acts as locked
        store
            .unlocked_contexts
            .write()
            .unwrap()
            .insert("prod".to_owned(), 100); // timestamp in the past
        let expired = store.get("prod");
        assert!(expired.effective_read_only());
        assert!(!expired.is_unlocked());
        assert_eq!(expired.unlocked_until, None);

        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
