//! Fleetglass Tauri 2 shell: a thin IPC layer over `fleetglass-core`.
//!
//! Commands parse input, call core and return bounded results. No Kubernetes
//! logic lives here.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use fleetglass_core::api::ImportPreview;
use fleetglass_core::api::{
    ActionRequest, AppInfo, AppPage, AppQuerySpec, AuditEntry, AuditLogPage, ClusterGroup,
    ColumnLayout, CompareOptions, ConnectionStatus, ContextList, ContextSettings, DiagnosticReport,
    DiffResult, ErrorKind, ExecSpec, HelmReleasePage, HelmReleaseSummary, HelmRevisionDiff,
    IpcError, KindCount, MetricsRange, MutatingAction, MuxLogEvent, OverviewData, Page, PageDelta,
    PluginManifest, PodUsageSeries, PortForwardInfo, PrometheusConfig, PrometheusDetection,
    QuerySpec, ResourceDetail, ResourceSchemaResult, RightSizingReport, SavedFile,
    SavedPortForward, TeamSettingsSummary, UpdateInfo, WorkloadLogTarget, WorkloadSelector,
};
use fleetglass_core::audit::AuditLogStore;
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::exec::ExecSession;
use fleetglass_core::import::{ImportOptions, ImportedStore, MAX_IMPORT_BYTES, SecretVault};
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::portforward::PortForwardManager;
use fleetglass_core::settings::SettingsStore;
use fleetglass_core::terminal::{ExternalTerminalTarget, TerminalEmulatorInfo};
use serde::{Deserialize, Serialize};
use specta_typescript::Typescript;
use tauri::async_runtime::{JoinHandle, TokioJoinHandle};
use tauri_specta::{Builder, collect_commands};

#[cfg(target_os = "android")]
mod mobile;

/// Capacity of the channel between a workload log follower and its IPC forwarder.
const LOG_EVENT_CHANNEL_CAPACITY: usize = 64;

/// The two tasks behind one `start_workload_logs` stream.
struct LogFollower {
    /// Core follower (pod watch + per-container log streams); `None` while the
    /// app is in the background (03-04a).
    follower: Option<TokioJoinHandle<()>>,
    /// Pushes follower events to the webview `Channel`.
    forwarder: JoinHandle<()>,
    /// What is needed to restart the follower after a pause; `None` for
    /// streams that are never restarted (tests).
    resume: Option<LogResume>,
}

/// Restart data of a workload log stream (03-04a).
struct LogResume {
    target: WorkloadLogTarget,
    /// Sink of the forwarder; a restarted follower sends into the same channel.
    sink: tauri::async_runtime::Sender<MuxLogEvent>,
    /// `seq` the next line gets, kept up to date by the forwarder so a restarted
    /// follower continues the sequence.
    next_seq: Arc<AtomicU32>,
    /// When the follower was stopped for the background.
    paused_at: Option<Instant>,
}

impl LogFollower {
    fn abort(&self) {
        if let Some(follower) = &self.follower {
            follower.abort();
        }
        self.forwarder.abort();
    }
}

/// The background session and IPC forwarder for an active container exec session (`R-10`).
struct ActiveExecSession {
    session: ExecSession,
    forwarder: JoinHandle<()>,
}

impl ActiveExecSession {
    fn abort(&self) {
        self.session.abort();
        self.forwarder.abort();
    }
}

/// Shared application state managed by Tauri.
pub struct AppState {
    pub manager: Arc<ClusterManager>,
    pub settings: Arc<SettingsStore>,
    pub port_forwards: Arc<PortForwardManager>,
    pub audit: Arc<AuditLogStore>,
    subscriptions: Mutex<HashMap<u32, JoinHandle<()>>>,
    log_followers: Mutex<HashMap<u32, LogFollower>>,
    next_log_follower_id: AtomicU32,
    next_logs_window: AtomicU32,
    exec_sessions: Mutex<HashMap<u32, ActiveExecSession>>,
    next_exec_session_id: AtomicU32,
    /// Installed plugins, shared with the `fgplugin` scheme handler.
    plugin_index: Arc<PluginIndex>,
    /// Kubeconfig text picked/pasted for import, kept between preview and commit. Contains
    /// credentials: never log it; cleared (and wiped) on commit, cancel or a new pick.
    pending_import: Arc<Mutex<Option<String>>>,
}

impl Drop for AppState {
    fn drop(&mut self) {
        let subscriptions = self
            .subscriptions
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        for handle in subscriptions.values() {
            handle.abort();
        }
        let followers = self
            .log_followers
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        for follower in followers.values() {
            follower.abort();
        }
        let exec_sessions = self
            .exec_sessions
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        for session in exec_sessions.values() {
            session.abort();
        }
    }
}

impl AppState {
    /// Constructs [`AppState`] backed by a [`ClusterManager`] reading `KUBECONFIG` / `~/.kube/config`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_source(env_kubeconfig_source())
    }

    /// Constructs [`AppState`] backed by a [`ClusterManager`] reading `source`.
    #[must_use]
    pub fn from_source(source: KubeconfigSource) -> Self {
        Self::with_manager(Arc::new(ClusterManager::new(source)))
    }

    /// Constructs [`AppState`] with an explicit [`ClusterManager`].
    #[must_use]
    pub fn with_manager(manager: Arc<ClusterManager>) -> Self {
        Self::with_parts(
            manager,
            Arc::new(SettingsStore::new(SettingsStore::default_path())),
        )
    }

    fn with_parts(manager: Arc<ClusterManager>, settings: Arc<SettingsStore>) -> Self {
        let audit = Arc::new(AuditLogStore::new(
            SettingsStore::default_path().with_file_name("audit.jsonl"),
        ));
        Self::with_parts_and_audit(manager, settings, audit)
    }

    fn with_parts_and_audit(
        manager: Arc<ClusterManager>,
        settings: Arc<SettingsStore>,
        audit: Arc<AuditLogStore>,
    ) -> Self {
        sync_watch_namespaces(&manager, &settings);
        Self {
            manager,
            settings,
            port_forwards: Arc::new(PortForwardManager::new()),
            audit,
            subscriptions: Mutex::new(HashMap::new()),
            log_followers: Mutex::new(HashMap::new()),
            next_log_follower_id: AtomicU32::new(1),
            next_logs_window: AtomicU32::new(1),
            exec_sessions: Mutex::new(HashMap::new()),
            next_exec_session_id: AtomicU32::new(1),
            plugin_index: Arc::new(PluginIndex::new(plugin_dirs())),
            pending_import: Arc::new(Mutex::new(None)),
        }
    }

    /// Registers the tasks of an active exec session and returns its new ID.
    /// Entries whose session/forwarder already ended are pruned here.
    fn insert_exec_session(&self, session: ExecSession, forwarder: JoinHandle<()>) -> u32 {
        let mut map = self
            .exec_sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, s| {
            let done = s.session.is_finished() || s.forwarder.inner().is_finished();
            if done {
                s.abort();
            }
            !done
        });
        let mut id = self.next_exec_session_id.fetch_add(1, Ordering::Relaxed);
        // IDs wrap at u32::MAX; skip 0 and any ID still in use.
        while id == 0 || map.contains_key(&id) {
            id = self.next_exec_session_id.fetch_add(1, Ordering::Relaxed);
        }
        map.insert(id, ActiveExecSession { session, forwarder });
        id
    }

    /// Stops and aborts an active exec session by ID, returning `true` if one existed.
    pub fn stop_exec_session(&self, id: u32) -> bool {
        let removed = self
            .exec_sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        if let Some(session) = &removed {
            session.abort();
        }
        removed.is_some()
    }

    /// Writes stdin bytes to an active exec session by ID.
    pub async fn write_exec_stdin(&self, id: u32, data: String) -> Result<(), IpcError> {
        let tx = {
            let map = self
                .exec_sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            map.get(&id).map(|s| s.session.stdin_sender())
        };
        let tx =
            tx.ok_or_else(|| IpcError::invalid_input(format!("exec session {id} not found")))?;
        tx.send(data.into_bytes())
            .await
            .map_err(|_| IpcError::internal(format!("exec session {id} stdin is closed")))?;
        Ok(())
    }

    /// Resizes an active exec/attach/node-debug session's remote PTY (`05-02`).
    /// Rejects zero or oversized dimensions; never blocks (only the latest size is kept).
    pub fn resize_exec_session(&self, id: u32, cols: u16, rows: u16) -> Result<(), IpcError> {
        let size = fleetglass_core::exec::TerminalSize::new(cols, rows).map_err(IpcError::from)?;
        let map = self
            .exec_sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = map
            .get(&id)
            .ok_or_else(|| IpcError::invalid_input(format!("exec session {id} not found")))?;
        entry.session.resize(size).map_err(IpcError::from)
    }

    /// Registers the tasks of a workload log stream and returns its new ID.
    /// Entries whose forwarder already ended (window closed) are pruned here.
    #[allow(dead_code)]
    fn insert_log_follower(
        &self,
        follower: Option<TokioJoinHandle<()>>,
        forwarder: JoinHandle<()>,
        resume: Option<LogResume>,
    ) -> u32 {
        let mut map = self
            .log_followers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, f| {
            let done = f.forwarder.inner().is_finished();
            if done {
                f.abort();
            }
            !done
        });
        let mut id = self.next_log_follower_id.fetch_add(1, Ordering::Relaxed);
        // IDs wrap at u32::MAX; skip 0 and any ID still in use.
        while id == 0 || map.contains_key(&id) {
            id = self.next_log_follower_id.fetch_add(1, Ordering::Relaxed);
        }
        map.insert(
            id,
            LogFollower {
                follower,
                forwarder,
                resume,
            },
        );
        id
    }

    /// Stops every workload log follower for the background, keeping its
    /// forwarder, channel and restart data. One-shot `previous` fetches are left
    /// to finish. Idempotent.
    fn pause_log_followers(&self) {
        let mut map = self
            .log_followers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        for f in map.values_mut() {
            let Some(resume) = f.resume.as_mut() else {
                continue;
            };
            // A `previous` fetch is one-shot and finite (no follow, no
            // reconnect): let it finish instead of resuming it with a window
            // relative to "now", which does not apply to a terminated container.
            if resume.target.previous {
                continue;
            }
            if let Some(follower) = f.follower.take() {
                let finished = follower.is_finished();
                follower.abort();
                if !finished && resume.paused_at.is_none() {
                    resume.paused_at = Some(now);
                }
            }
        }
    }

    /// Restarts the followers stopped by [`Self::pause_log_followers`] from where
    /// they stopped (see [`fleetglass_core::logs_mux::resume_target`]). A stream
    /// stopped meanwhile, or whose cluster cannot be reached, is skipped; the
    /// latter gets an error event.
    async fn resume_log_followers(&self) {
        let paused: Vec<(u32, WorkloadLogTarget, Instant)> = {
            let map = self
                .log_followers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !self.manager.is_foreground() {
                return;
            }
            map.iter()
                .filter_map(|(id, f)| {
                    let r = f.resume.as_ref()?;
                    Some((*id, r.target.clone(), r.paused_at?))
                })
                .collect()
        };
        for (id, target, paused_at) in paused {
            let client = self.manager.connect_client(&target.cluster).await;
            let mut map = self
                .log_followers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !self.manager.is_foreground() {
                return;
            }
            let Some(f) = map.get_mut(&id) else {
                continue;
            };
            let Some(resume) = f.resume.as_mut() else {
                continue;
            };
            if resume.paused_at != Some(paused_at) || f.follower.is_some() {
                continue;
            }
            resume.paused_at = None;
            match client {
                Ok(client) => {
                    f.follower = Some(fleetglass_core::logs_mux::spawn_workload_log_follower_from(
                        client,
                        fleetglass_core::logs_mux::resume_target(&target, paused_at.elapsed()),
                        resume.next_seq.load(Ordering::Acquire),
                        resume.sink.clone(),
                    ));
                }
                Err(err) => {
                    let message = format!("cannot resume logs: {}", IpcError::from(err).message);
                    let _ = resume.sink.try_send(MuxLogEvent::Error { message });
                }
            }
        }
    }

    /// Removes and aborts a workload log stream by ID, returning `true` if one existed.
    pub fn stop_log_follower(&self, id: u32) -> bool {
        let removed = self
            .log_followers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        if let Some(follower) = &removed {
            follower.abort();
        }
        removed.is_some()
    }

    /// Removes and aborts an active subscription task by ID, returning `true` if one existed.
    pub fn unsubscribe(&self, subscription_id: u32) -> bool {
        let removed = self
            .subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&subscription_id);
        if let Some(handle) = &removed {
            handle.abort();
        }
        removed.is_some()
    }
}

/// Process start, set first thing in `main` (V-06 startup marker).
static PROCESS_START: OnceLock<Instant> = OnceLock::new();
/// Guards the one-time "first rows" startup line.
static FIRST_ROWS: StartupOnce = StartupOnce::new();
/// Guards the one-time "first overview" startup line (v0.2 cold start).
static FIRST_OVERVIEW: StartupOnce = StartupOnce::new();
/// Guards the one-time "main page loaded" startup line.
static MAIN_PAGE_LOADED: StartupOnce = StartupOnce::new();

/// Records the process start time; later calls are no-ops. Call first thing in `main`.
pub fn mark_process_start() {
    PROCESS_START.get_or_init(Instant::now);
}

/// Milliseconds since [`mark_process_start`] (or since first use if it was never called).
fn ms_since_start() -> u128 {
    PROCESS_START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
}

/// A flag that lets exactly one caller through, ever.
struct StartupOnce(AtomicBool);

impl StartupOnce {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// `true` for the first caller only. Cheap (a relaxed load) once claimed.
    fn claim(&self) -> bool {
        !self.0.load(Ordering::Relaxed) && !self.0.swap(true, Ordering::AcqRel)
    }
}

/// Startup line for the first non-empty row window, or `None` when the window is empty or the
/// line was already produced.
fn first_rows_line(once: &StartupOnce, source: &str, page: &Page, ms: u128) -> Option<String> {
    if page.rows.is_empty() || !once.claim() {
        return None;
    }
    Some(format!(
        "fleetglass startup: first rows returned after {ms} ms (rows={}, total={}, via={source})",
        page.rows.len(),
        page.total
    ))
}

/// Writes the V-06 startup line once, when the UI first receives rows (`query_page` result or a
/// `subscribe_query` delta, whichever comes first). Local stderr only: the app installs no
/// tracing subscriber, and nothing is sent or persisted.
fn note_first_rows(source: &str, page: &Page) {
    if let Some(line) = first_rows_line(&FIRST_ROWS, source, page, ms_since_start()) {
        startup_log(&line);
    }
}

/// Startup line for the first [`OverviewData`] with a usable cluster (at least one card
/// `Connected`, or `Degraded` by a denied kind, with listed nodes or known pod counts, so a user
/// who may not list Nodes still gets it), or `None` before that or once the line was produced.
fn first_overview_line(
    once: &StartupOnce,
    source: &str,
    data: &OverviewData,
    ms: u128,
) -> Option<String> {
    let connected = data
        .clusters
        .iter()
        .filter(|c| c.connection == ConnectionStatus::Connected)
        .count();
    let usable = data.clusters.iter().any(|c| {
        // A forbidden Node list makes the card `Degraded`, not `Connected`.
        matches!(
            c.connection,
            ConnectionStatus::Connected | ConnectionStatus::Degraded { .. }
        ) && (c.nodes_total.is_some_and(|n| n > 0) || c.pods_running.is_some())
    });
    if !usable || !once.claim() {
        return None;
    }
    Some(format!(
        "fleetglass startup: first overview returned after {ms} ms \
         (clusters={}, connected={connected}, attention={}, via={source})",
        data.clusters.len(),
        data.attention.items.len()
    ))
}

/// Writes the v0.2 cold-start line ("first Overview data with ≥ 1 cluster") once, the first
/// time an Overview with a connected cluster reaches the UI. Local stderr only, like
/// [`note_first_rows`].
fn note_first_overview(source: &str, data: &OverviewData) {
    if let Some(line) = first_overview_line(&FIRST_OVERVIEW, source, data, ms_since_start()) {
        startup_log(&line);
    }
}

/// Writes one startup line to stderr, ignoring write errors (`eprintln!` would panic on a
/// closed stderr).
fn startup_log(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// Header written at the top of every generated TypeScript file.
const GENERATED_HEADER: &str = "// @generated by `cargo run -p fleetglass-app --bin gen-types`. DO NOT EDIT.\n\
// Source: crates/core/src/api (IPC types) and crates/app/src/lib.rs (commands).\n\
// See docs/adr/0001-ipc-type-generation.md.";

/// Basic information about this build.
#[tauri::command]
#[specta::specta]
fn app_info() -> AppInfo {
    fleetglass_core::app_info()
}

/// Contexts from the manager's kubeconfig source (`KUBECONFIG` or `~/.kube/config`, plus the
/// imported contexts on Android).
///
/// Reading and parsing files (and, on Android, opening the credential store) is blocking, so it
/// runs on the blocking pool.
#[tauri::command]
#[specta::specta]
async fn list_contexts(state: tauri::State<'_, AppState>) -> Result<ContextList, IpcError> {
    let source = state.manager.source().clone();
    run_blocking("listing contexts", move || source.list_contexts()).await
}

/// `KUBECONFIG` / `~/.kube/config`; an empty source if neither can be resolved.
fn env_kubeconfig_source() -> KubeconfigSource {
    KubeconfigSource::from_env().unwrap_or_else(|_| KubeconfigSource::from_paths(Vec::new()))
}

/// A [`SecretVault`] that is set once, after construction.
///
/// [`AppState`] (and with it the [`ImportedStore`]) is built before the Tauri app exists, but the
/// Android Keystore vault needs the app's plugin handle; `run()` sets it in `setup`. Until then
/// every call fails with [`fleetglass_core::Error::Vault`].
#[derive(Clone, Default)]
pub struct LateVault(Arc<OnceLock<Box<dyn SecretVault>>>);

impl LateVault {
    /// Installs the real vault. Returns `false` (and drops `vault`) if one was already set.
    pub fn set(&self, vault: Box<dyn SecretVault>) -> bool {
        self.0.set(vault).is_ok()
    }

    fn get(&self) -> fleetglass_core::Result<&dyn SecretVault> {
        self.0.get().map(AsRef::as_ref).ok_or_else(|| {
            fleetglass_core::Error::Vault("credential store is not ready".to_owned())
        })
    }
}

impl SecretVault for LateVault {
    fn seal(&self, plain: &[u8]) -> fleetglass_core::Result<Vec<u8>> {
        self.get()?.seal(plain)
    }

    fn open(&self, sealed: &[u8]) -> fleetglass_core::Result<Vec<u8>> {
        self.get()?.open(sealed)
    }
}

/// File (next to the settings file) holding the sealed imported contexts.
#[cfg(target_os = "android")]
fn imported_store_path() -> PathBuf {
    SettingsStore::default_path().with_file_name("imported-contexts.bin")
}

const IMPORT_UNAVAILABLE: &str = "importing contexts is only available in the Android app";

/// Import rules for this platform: Android refuses CA-less clusters (ADR 0004).
fn import_options() -> ImportOptions {
    ImportOptions {
        require_ca: cfg!(target_os = "android"),
    }
}

/// Best-effort wipe of credential text before its memory is freed.
fn wipe(text: String) {
    let mut bytes = text.into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

fn clear_pending(pending: &Mutex<Option<String>>) {
    if let Some(old) = pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        wipe(old);
    }
}

fn import_store(source: &KubeconfigSource) -> fleetglass_core::Result<Arc<ImportedStore>> {
    source
        .imported()
        .cloned()
        .ok_or_else(|| fleetglass_core::Error::Import(IMPORT_UNAVAILABLE.to_owned()))
}

/// Previews `yaml` for import and keeps it as the pending import (replacing any earlier one).
/// Blocking (may open the vault).
fn import_preview_in(
    store: &ImportedStore,
    pending: &Mutex<Option<String>>,
    yaml: String,
    opts: &ImportOptions,
) -> fleetglass_core::Result<ImportPreview> {
    clear_pending(pending);
    if yaml.len() > MAX_IMPORT_BYTES {
        wipe(yaml);
        return Err(fleetglass_core::Error::Import(format!(
            "the kubeconfig is larger than the {} KiB limit",
            MAX_IMPORT_BYTES / 1024
        )));
    }
    let preview = store
        .context_names()
        .and_then(|existing| fleetglass_core::import::preview_import(&yaml, &existing, opts));
    match preview {
        Ok(preview) => {
            *pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(yaml);
            Ok(preview)
        }
        Err(err) => {
            wipe(yaml);
            Err(err)
        }
    }
}

/// Imports `names` from the pending kubeconfig. On success the pending text is wiped; on failure
/// it is kept so the user can retry (e.g. with `replace_existing`). Blocking.
fn import_commit_in(
    manager: &ClusterManager,
    store: &ImportedStore,
    pending: &Mutex<Option<String>>,
    names: &[String],
    replace_existing: bool,
    opts: &ImportOptions,
) -> fleetglass_core::Result<Vec<String>> {
    // Not held across the vault call: the lock is never waited on while the vault waits for
    // the Android main thread.
    let yaml = pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .ok_or_else(|| {
            fleetglass_core::Error::Import("nothing to import; pick a file first".to_owned())
        })?;
    let result = fleetglass_core::import::extract_contexts(&yaml, names, opts)
        .and_then(|config| store.add(config, replace_existing));
    match result {
        Ok(imported) => {
            wipe(yaml);
            for name in &imported {
                manager.invalidate_client(name);
            }
            Ok(imported)
        }
        Err(err) => {
            let mut slot = pending.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(yaml);
            } else {
                wipe(yaml);
            }
            Err(err)
        }
    }
}

/// Builds, validates and stores a single token context. Blocking.
#[allow(clippy::too_many_arguments)]
fn import_token_in(
    manager: &ClusterManager,
    store: &ImportedStore,
    name: &str,
    server: &str,
    token: &str,
    ca_pem: Option<&str>,
    insecure_skip_tls_verify: bool,
    opts: &ImportOptions,
) -> fleetglass_core::Result<Vec<String>> {
    let config = fleetglass_core::import::token_kubeconfig(
        name,
        server,
        token,
        ca_pem,
        insecure_skip_tls_verify,
    )?;
    fleetglass_core::import::validate(&config, opts)?;
    let imported = store.add(config, false)?;
    for name in &imported {
        manager.invalidate_client(name);
    }
    Ok(imported)
}

/// Removes an imported context and drops its cached client. Blocking.
fn remove_imported_in(
    manager: &ClusterManager,
    store: &ImportedStore,
    name: &str,
) -> fleetglass_core::Result<bool> {
    let removed = store.remove(name)?;
    manager.invalidate_client(name);
    Ok(removed)
}

/// Deletes every imported context (the way out when the sealed store can no longer be opened)
/// and drops the cached clients of the contexts it could still read; returns those names.
/// If the store cannot be opened the names are unknown and nothing is invalidated: only a client
/// built before the open started failing could exist (it stays cached until a 401 or a restart).
/// Blocking.
fn reset_imported_in(
    manager: &ClusterManager,
    store: &ImportedStore,
) -> fleetglass_core::Result<Vec<String>> {
    let names = store.context_names().unwrap_or_default();
    store.reset()?;
    for name in &names {
        manager.invalidate_client(name);
    }
    Ok(names)
}

/// Opens the Android document picker and previews the chosen kubeconfig (kept until
/// [`import_commit`] or [`import_cancel`]). `None` = the user cancelled. Desktop: an error.
#[tauri::command]
#[specta::specta]
async fn import_pick_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<ImportPreview>, IpcError> {
    let store = import_store(state.manager.source())?;
    let Some(bytes) = pick_kubeconfig_file(&app).await? else {
        return Ok(None);
    };
    let yaml = String::from_utf8(bytes).map_err(|err| {
        let mut bytes = err.into_bytes();
        bytes.fill(0);
        fleetglass_core::Error::Import("the selected file is not a text kubeconfig".to_owned())
    })?;
    let pending = Arc::clone(&state.pending_import);
    run_blocking("import_pick_file", move || {
        import_preview_in(&store, &pending, yaml, &import_options()).map(Some)
    })
    .await
}

/// Largest text `share_text` accepts (same limit as the Kotlin side).
const MAX_SHARED_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// Shares `text` as the file `file_name` through the Android share sheet
/// (`title` is the chooser title). Android only; desktop returns an error.
#[tauri::command]
#[specta::specta]
async fn share_text(
    app: tauri::AppHandle,
    title: String,
    file_name: String,
    text: String,
) -> Result<(), IpcError> {
    if text.len() > MAX_SHARED_TEXT_BYTES {
        return Err(IpcError::invalid_input(
            "text to share is larger than 8 MiB",
        ));
    }
    share_text_impl(&app, &title, &file_name, &text).await
}

#[cfg(target_os = "android")]
async fn share_text_impl(
    app: &tauri::AppHandle,
    title: &str,
    file_name: &str,
    text: &str,
) -> Result<(), IpcError> {
    use tauri::Manager as _;
    app.state::<mobile::AndroidBridge<tauri::Wry>>()
        .share_text(title, file_name, text)
        .await
        .map_err(|e| IpcError::internal(format!("sharing failed: {e}")))
}

#[cfg(not(target_os = "android"))]
async fn share_text_impl(
    _app: &tauri::AppHandle,
    _title: &str,
    _file_name: &str,
    _text: &str,
) -> Result<(), IpcError> {
    Err(IpcError::invalid_input("not supported"))
}

/// Android document picker (async; runs off the main thread). `None` = cancelled.
#[cfg(target_os = "android")]
async fn pick_kubeconfig_file(app: &tauri::AppHandle) -> fleetglass_core::Result<Option<Vec<u8>>> {
    use tauri::Manager as _;
    let picked = app
        .state::<mobile::AndroidBridge<tauri::Wry>>()
        .pick_file()
        .await
        .map_err(fleetglass_core::Error::Import)?;
    Ok(picked.map(|(_display_name, bytes)| bytes))
}

#[cfg(not(target_os = "android"))]
async fn pick_kubeconfig_file(_app: &tauri::AppHandle) -> fleetglass_core::Result<Option<Vec<u8>>> {
    Err(fleetglass_core::Error::Import(
        IMPORT_UNAVAILABLE.to_owned(),
    ))
}

/// Previews pasted kubeconfig text for import (kept until [`import_commit`] or
/// [`import_cancel`]). Desktop: an error.
#[tauri::command]
#[specta::specta]
async fn import_preview_text(
    state: tauri::State<'_, AppState>,
    yaml: String,
) -> Result<ImportPreview, IpcError> {
    let store = import_store(state.manager.source())?;
    let pending = Arc::clone(&state.pending_import);
    run_blocking("import_preview_text", move || {
        import_preview_in(&store, &pending, yaml, &import_options())
    })
    .await
}

/// Imports the contexts `names` of the previewed kubeconfig; returns the imported names.
#[tauri::command]
#[specta::specta]
async fn import_commit(
    state: tauri::State<'_, AppState>,
    names: Vec<String>,
    replace_existing: bool,
) -> Result<Vec<String>, IpcError> {
    let store = import_store(state.manager.source())?;
    let manager = Arc::clone(&state.manager);
    let pending = Arc::clone(&state.pending_import);
    run_blocking("import_commit", move || {
        import_commit_in(
            &manager,
            &store,
            &pending,
            &names,
            replace_existing,
            &import_options(),
        )
    })
    .await
}

/// Forgets the previewed kubeconfig.
#[tauri::command]
#[specta::specta]
async fn import_cancel(state: tauri::State<'_, AppState>) -> Result<(), IpcError> {
    let pending = Arc::clone(&state.pending_import);
    run_blocking("import_cancel", move || -> Result<(), IpcError> {
        clear_pending(&pending);
        Ok(())
    })
    .await
}

/// Imports one context from an API server URL and a bearer token; returns its name.
#[tauri::command]
#[specta::specta]
async fn import_token(
    state: tauri::State<'_, AppState>,
    name: String,
    server: String,
    token: String,
    ca_pem: Option<String>,
    insecure_skip_tls_verify: bool,
) -> Result<Vec<String>, IpcError> {
    let store = import_store(state.manager.source())?;
    let manager = Arc::clone(&state.manager);
    run_blocking("import_token", move || {
        import_token_in(
            &manager,
            &store,
            &name,
            &server,
            &token,
            ca_pem.as_deref(),
            insecure_skip_tls_verify,
            &import_options(),
        )
    })
    .await
}

/// Removes an imported context; `false` if it was not imported.
#[tauri::command]
#[specta::specta]
async fn remove_imported_context(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<bool, IpcError> {
    let store = import_store(state.manager.source())?;
    let manager = Arc::clone(&state.manager);
    run_blocking("remove_imported_context", move || {
        remove_imported_in(&manager, &store, &name)
    })
    .await
}

/// Deletes all imported contexts, e.g. when the stored ones can no longer be decrypted.
#[tauri::command]
#[specta::specta]
async fn reset_imported_contexts(state: tauri::State<'_, AppState>) -> Result<(), IpcError> {
    let store = import_store(state.manager.source())?;
    let manager = Arc::clone(&state.manager);
    run_blocking("reset_imported_contexts", move || {
        reset_imported_in(&manager, &store).map(|_| ())
    })
    .await
}

/// Names of the imported contexts (bounded by the import size cap); empty on desktop.
#[tauri::command]
#[specta::specta]
async fn list_imported_contexts(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, IpcError> {
    let Some(store) = state.manager.source().imported().cloned() else {
        return Ok(Vec::new());
    };
    run_blocking("list_imported_contexts", move || store.context_names()).await
}

/// Executes a windowed filter and sort query against the in-memory resource stores.
///
/// Starts the watchers for every `(cluster, kind)` of `spec` first. Connection,
/// RBAC and watch errors are reported per store in [`Page::access`], not as errors.
#[tauri::command]
#[specta::specta]
async fn query_page(state: tauri::State<'_, AppState>, spec: QuerySpec) -> Result<Page, IpcError> {
    let manager = Arc::clone(&state.manager);
    manager.ensure_watching_spec(&spec).await;
    let page = manager.query(&spec).await;
    note_first_rows("query_page", &page);
    Ok(page)
}

/// Validates a filter query string against the structured search grammar (`04-06`).
/// Returns `None` if valid or empty, or `Some(error_message)` if malformed.
#[tauri::command]
#[specta::specta]
fn validate_filter_query(query: String) -> Option<String> {
    fleetglass_core::filter::validate_filter(&query).err()
}

/// Executes a windowed multi-cluster resource query across the provided cluster list.
#[tauri::command]
#[specta::specta]
async fn query_resources_multi(
    state: tauri::State<'_, AppState>,
    clusters: Vec<String>,
    kind: String,
    namespace: Option<String>,
    limit: Option<u32>,
) -> Result<Page, IpcError> {
    let spec = QuerySpec {
        clusters,
        kinds: vec![kind],
        namespaces: namespace.map(|ns| vec![ns]).unwrap_or_default(),
        label_selector: None,
        text_filter: None,
        sort: None,
        offset: 0,
        limit: limit.unwrap_or(50),
        statuses: Vec::new(),
    };
    query_page(state, spec).await
}

/// Executes a single-cluster resource query (for backwards compatibility).
#[tauri::command]
#[specta::specta]
async fn query_resources(
    state: tauri::State<'_, AppState>,
    cluster: String,
    kind: String,
    namespace: Option<String>,
    limit: Option<u32>,
) -> Result<Page, IpcError> {
    query_resources_multi(state, vec![cluster], kind, namespace, limit).await
}

/// Opens a batched query subscription pushing [`PageDelta`]s (at most 10/s) over `on_delta`.
///
/// Starts the watchers for every `(cluster, kind)` of `spec` first; their access
/// state (forbidden, connection error, ...) arrives in each delta's page.
#[tauri::command]
#[specta::specta]
async fn subscribe_query(
    state: tauri::State<'_, AppState>,
    spec: QuerySpec,
    on_delta: tauri::ipc::Channel<PageDelta>,
) -> Result<u32, IpcError> {
    state.manager.ensure_watching_spec(&spec).await;
    let (sub_id, mut rx) = state.manager.subscribe(spec);
    let handle = tauri::async_runtime::spawn(async move {
        while let Some(delta) = rx.recv().await {
            note_first_rows("subscribe_query", &delta.page);
            if on_delta.send(delta).is_err() {
                break;
            }
        }
    });
    state
        .subscriptions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(sub_id, handle);
    Ok(sub_id)
}

/// Cancels an active query subscription opened by [`subscribe_query`].
#[tauri::command]
#[specta::specta]
async fn unsubscribe_query(
    state: tauri::State<'_, AppState>,
    subscription_id: u32,
) -> Result<bool, IpcError> {
    Ok(state.unsubscribe(subscription_id))
}

/// Returns the overview data for `clusters` (the UI's active cluster selection).
///
/// Starts the Overview watchers for those clusters first, connecting them lazily
/// like a table query; contexts not in the list are never connected. The list is
/// deduplicated and capped at `MAX_OVERVIEW_CLUSTERS` (64). An empty list covers
/// the clusters already connected and opens no new connection.
#[tauri::command]
#[specta::specta]
async fn get_overview(
    state: tauri::State<'_, AppState>,
    clusters: Vec<String>,
) -> Result<OverviewData, IpcError> {
    state.manager.ensure_watching_overview(&clusters).await;
    let manager = Arc::clone(&state.manager);
    run_blocking("get_overview", move || -> fleetglass_core::Result<_> {
        Ok(manager.get_overview(&clusters))
    })
    .await
}

/// Returns right-sizing and resource efficiency analysis (`04-09`).
#[tauri::command]
#[specta::specta]
async fn get_right_sizing(
    state: tauri::State<'_, AppState>,
    clusters: Vec<String>,
    threshold_pct: Option<u8>,
) -> Result<RightSizingReport, IpcError> {
    state.manager.ensure_watching_overview(&clusters).await;
    let manager = Arc::clone(&state.manager);
    let threshold = threshold_pct.unwrap_or(20);
    run_blocking("get_right_sizing", move || -> fleetglass_core::Result<_> {
        Ok(fleetglass_core::overview::compute_right_sizing(
            &manager, &clusters, threshold,
        ))
    })
    .await
}

/// Opens a throttled subscription pushing [`OverviewData`] for `clusters` (at most 1/s)
/// over `on_update`. Same cluster semantics as [`get_overview`]; the first message is
/// sent at once (clusters still connecting show as such) and the watchers start in
/// the background.
#[tauri::command]
#[specta::specta]
async fn subscribe_overview(
    state: tauri::State<'_, AppState>,
    clusters: Vec<String>,
    on_update: tauri::ipc::Channel<OverviewData>,
) -> Result<u32, IpcError> {
    let (sub_id, mut rx) = state.manager.subscribe_overview(clusters);
    let handle = tauri::async_runtime::spawn(async move {
        while let Some(data) = rx.recv().await {
            note_first_overview("subscribe_overview", &data);
            if on_update.send(data).is_err() {
                break;
            }
        }
    });
    state
        .subscriptions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(sub_id, handle);
    Ok(sub_id)
}

/// Cancels an active overview subscription opened by [`subscribe_overview`].
#[tauri::command]
#[specta::specta]
async fn unsubscribe_overview(
    state: tauri::State<'_, AppState>,
    subscription_id: u32,
) -> Result<bool, IpcError> {
    Ok(state.unsubscribe(subscription_id))
}

/// Returns the persisted UI and write-safety settings for `context`.
#[tauri::command]
#[specta::specta]
async fn get_context_settings(
    state: tauri::State<'_, AppState>,
    context: String,
) -> Result<ContextSettings, IpcError> {
    Ok(state.settings.get(&context))
}

/// Hands every context's `watchNamespaces` setting (05-01) to the manager.
fn sync_watch_namespaces(manager: &ClusterManager, settings: &SettingsStore) {
    for (context, ctx_settings) in settings.get_all() {
        manager.set_watch_namespaces(&context, ctx_settings.watch_namespaces);
    }
}

/// Persists updated UI and write-safety settings for `context`.
#[tauri::command]
#[specta::specta]
async fn update_context_settings(
    state: tauri::State<'_, AppState>,
    context: String,
    settings: ContextSettings,
) -> Result<ContextSettings, IpcError> {
    let store = Arc::clone(&state.settings);
    let saved = run_blocking(
        "update_context_settings",
        move || -> fleetglass_core::Result<_> {
            store.set(context.clone(), settings.clone())?;
            Ok((context, settings))
        },
    )
    .await?;
    let (context, settings) = saved;
    state
        .manager
        .set_watch_namespaces(&context, settings.watch_namespaces.clone());
    Ok(settings)
}

/// Request to spawn an external terminal emulator (`04-10`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ExternalTerminalRequest {
    pub target: ExternalTerminalTarget,
    pub confirmed_name: Option<String>,
}

/// Implementation of `unlock_context`.
async fn unlock_context_impl(
    state: &AppState,
    context: String,
    confirmed_name: Option<String>,
) -> Result<ContextSettings, IpcError> {
    let settings = state.settings.get(&context);
    if settings.protected && confirmed_name.as_deref() != Some(&context) {
        return Err(IpcError::from(
            fleetglass_core::Error::ConfirmationRequired {
                context: context.clone(),
                expected_name: context.clone(),
            },
        ));
    }

    state.settings.unlock_context(&context, 900); // 15 minutes

    let entry = AuditEntry {
        id: 0,
        timestamp: fleetglass_core::audit::now_rfc3339(),
        cluster: context.clone(),
        user: state.manager.source().context_user(&context),
        action: "unlock_context".to_owned(),
        kind: "Context".to_owned(),
        namespace: None,
        name: context.clone(),
        kubectl: "# context unlocked for 15 minutes".to_owned(),
        success: true,
        error: None,
    };
    state.audit.append_or_warn(&entry);

    let store = Arc::clone(&state.settings);
    let audit = Arc::clone(&state.audit);
    let ctx = context.clone();
    let user = state.manager.source().context_user(&context);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(900)).await;
        if store.relock_context(&ctx) {
            let relock_entry = AuditEntry {
                id: 0,
                timestamp: fleetglass_core::audit::now_rfc3339(),
                cluster: ctx.clone(),
                user,
                action: "relock_context".to_owned(),
                kind: "Context".to_owned(),
                namespace: None,
                name: ctx.clone(),
                kubectl: "# context relocked (timer expired)".to_owned(),
                success: true,
                error: None,
            };
            audit.append_or_warn(&relock_entry);
        }
    });

    Ok(state.settings.get(&context))
}

/// Temporarily unlocks a read-only context for 15 minutes (`04-10`).
#[tauri::command]
#[specta::specta]
async fn unlock_context(
    state: tauri::State<'_, AppState>,
    context: String,
    confirmed_name: Option<String>,
) -> Result<ContextSettings, IpcError> {
    unlock_context_impl(&state, context, confirmed_name).await
}

/// Implementation of `relock_context`.
async fn relock_context_impl(
    state: &AppState,
    context: String,
) -> Result<ContextSettings, IpcError> {
    state.settings.relock_context(&context);

    let entry = AuditEntry {
        id: 0,
        timestamp: fleetglass_core::audit::now_rfc3339(),
        cluster: context.clone(),
        user: state.manager.source().context_user(&context),
        action: "relock_context".to_owned(),
        kind: "Context".to_owned(),
        namespace: None,
        name: context.clone(),
        kubectl: "# context relocked".to_owned(),
        success: true,
        error: None,
    };
    state.audit.append_or_warn(&entry);

    Ok(state.settings.get(&context))
}

/// Relocks a temporarily unlocked context (`04-10`).
#[tauri::command]
#[specta::specta]
async fn relock_context(
    state: tauri::State<'_, AppState>,
    context: String,
) -> Result<ContextSettings, IpcError> {
    relock_context_impl(&state, context).await
}

/// Returns detected terminal emulators available on this system (`04-10`).
#[tauri::command]
#[specta::specta]
async fn detect_terminal_emulators() -> Result<Vec<TerminalEmulatorInfo>, IpcError> {
    Ok(fleetglass_core::terminal::detect_available_terminal_emulators())
}

/// Implementation of `open_external_terminal`.
async fn open_external_terminal_impl(
    state: &AppState,
    request: ExternalTerminalRequest,
) -> Result<(), IpcError> {
    let settings = state.settings.get(request.target.context());
    if let Err(err) = fleetglass_core::terminal::validate_external_terminal_allowed(
        &request.target,
        &settings,
        request.confirmed_name.as_deref(),
    ) {
        let entry = AuditEntry {
            id: 0,
            timestamp: fleetglass_core::audit::now_rfc3339(),
            cluster: request.target.context().to_owned(),
            user: state
                .manager
                .source()
                .context_user(request.target.context()),
            action: "open_external_terminal".to_owned(),
            kind: request.target.resource_kind().to_owned(),
            namespace: request.target.namespace().map(str::to_owned),
            name: request.target.target_name().to_owned(),
            kubectl: "# blocked by write safety".to_owned(),
            success: false,
            error: Some(err.to_string()),
        };
        state.audit.append_or_warn(&entry);
        return Err(IpcError::from(err));
    }

    let emulator = fleetglass_core::terminal::detect_primary_terminal_emulator().ok_or_else(|| {
        IpcError::internal(
            "no supported terminal emulator found (installed: ghostty, kitty, alacritty, wezterm, foot, konsole, gnome-terminal, xfce4-terminal, Terminal.app, iTerm2, wt.exe, powershell.exe)",
        )
    })?;

    let kubeconfig_paths = std::env::var("KUBECONFIG").ok();
    let (preview, args, env) = fleetglass_core::terminal::preview_external_terminal_command(
        &request.target,
        kubeconfig_paths.as_deref(),
    );

    let mut spawn_cmd =
        fleetglass_core::terminal::build_terminal_spawn_command(&emulator, &args, &env);
    match spawn_cmd.spawn() {
        Ok(_) => {
            let entry = AuditEntry {
                id: 0,
                timestamp: fleetglass_core::audit::now_rfc3339(),
                cluster: request.target.context().to_owned(),
                user: state
                    .manager
                    .source()
                    .context_user(request.target.context()),
                action: "open_external_terminal".to_owned(),
                kind: request.target.resource_kind().to_owned(),
                namespace: request.target.namespace().map(str::to_owned),
                name: request.target.target_name().to_owned(),
                kubectl: preview,
                success: true,
                error: None,
            };
            state.audit.append_or_warn(&entry);
            Ok(())
        }
        Err(err) => {
            let entry = AuditEntry {
                id: 0,
                timestamp: fleetglass_core::audit::now_rfc3339(),
                cluster: request.target.context().to_owned(),
                user: state
                    .manager
                    .source()
                    .context_user(request.target.context()),
                action: "open_external_terminal".to_owned(),
                kind: request.target.resource_kind().to_owned(),
                namespace: request.target.namespace().map(str::to_owned),
                name: request.target.target_name().to_owned(),
                kubectl: preview,
                success: false,
                error: Some(err.to_string()),
            };
            state.audit.append_or_warn(&entry);
            Err(IpcError::internal(format!(
                "failed to spawn {}: {err}",
                emulator.name
            )))
        }
    }
}

/// Spawns an external terminal emulator running Pod exec, Node debug, or context shell (`04-10`).
#[tauri::command]
#[specta::specta]
async fn open_external_terminal(
    state: tauri::State<'_, AppState>,
    request: ExternalTerminalRequest,
) -> Result<(), IpcError> {
    open_external_terminal_impl(&state, request).await
}

/// Launches the native OS browser for `url` using the platform opener (`xdg-open`, `open`, `rundll32.exe`).
fn launch_system_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");

    // `url` is a validated http(s) serialisation; refuse anything the opener could read as an option.
    if url.starts_with('-') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to pass an option-like argument to the browser opener",
        ));
    }
    cmd.arg(url);
    cmd.spawn().map(|_| ())
}

/// Who an `open_in_browser` audit entry is attributed to.
struct BrowserAuditSubject {
    kind: &'static str,
    cluster: String,
    user: Option<String>,
    namespace: Option<String>,
    name: String,
}

impl BrowserAuditSubject {
    fn browser(name: String) -> Self {
        Self {
            kind: "Browser",
            cluster: "local".to_owned(),
            user: None,
            namespace: None,
            name,
        }
    }

    fn record(self, state: &AppState, target: &str, error: Option<String>) {
        state.audit.append_or_warn(&AuditEntry {
            id: 0,
            timestamp: fleetglass_core::audit::now_rfc3339(),
            cluster: self.cluster,
            user: self.user,
            action: "open_in_browser".to_owned(),
            kind: self.kind.to_owned(),
            namespace: self.namespace,
            name: self.name,
            kubectl: format!("# open in browser {target}"),
            success: error.is_none(),
            error,
        });
    }
}

/// Helper for `open_in_browser` parameterised over the process launcher for testability (`05-05`).
///
/// Every call writes one audit entry. The URL is validated before anything is
/// formatted into the entry; rejected input is stored truncated.
async fn open_in_browser_with_launcher<F>(
    state: &AppState,
    url: &str,
    launcher: F,
) -> Result<(), IpcError>
where
    F: FnOnce(&str) -> std::io::Result<()>,
{
    use fleetglass_core::browser::{audit_safe_input, same_endpoint, validate_browser_url};

    let parsed = match validate_browser_url(url) {
        Ok(parsed) => parsed,
        Err(err) => {
            let shown = audit_safe_input(url);
            BrowserAuditSubject::browser(shown.clone()).record(
                state,
                &shown,
                Some(err.to_string()),
            );
            return Err(IpcError::from(&err));
        }
    };
    let target = parsed.as_str();

    let forward = state.port_forwards.list().into_iter().find(|pf| {
        validate_browser_url(&pf.browser_url).is_ok_and(|pf_url| same_endpoint(&pf_url, &parsed))
    });
    let subject = match forward {
        Some(pf) => BrowserAuditSubject {
            kind: "PortForward",
            user: state.manager.source().context_user(&pf.context),
            cluster: pf.context,
            namespace: Some(pf.namespace),
            name: pf.pod,
        },
        None => BrowserAuditSubject::browser(target.to_owned()),
    };

    let result = launcher(target);
    subject.record(
        state,
        target,
        result.as_ref().err().map(ToString::to_string),
    );
    result.map_err(|err| IpcError::internal(format!("failed to open browser: {err}")))
}

/// Implementation of `open_in_browser`.
async fn open_in_browser_impl(state: &AppState, url: &str) -> Result<(), IpcError> {
    open_in_browser_with_launcher(state, url, launch_system_browser).await
}

/// Spawns the system browser for a validated local port-forward or Ingress/Gateway URL (`05-05`).
#[tauri::command]
#[specta::specta]
async fn open_in_browser(state: tauri::State<'_, AppState>, url: String) -> Result<(), IpcError> {
    open_in_browser_impl(&state, &url).await
}

/// Runs structured kubeconfig and auth diagnostics for `context`.
#[tauri::command]
#[specta::specta]
async fn run_connection_diagnostics(
    state: tauri::State<'_, AppState>,
    context: String,
) -> Result<DiagnosticReport, IpcError> {
    let client = state.manager.connect_client(&context).await;
    let loaded = state.manager.source().load().map_err(IpcError::from)?;
    let shell_env = fleetglass_core::shell_env::ShellEnv::get().await;
    let shell_env_source = match shell_env.source() {
        fleetglass_core::shell_env::EnvSource::LoginShell { shell } => {
            format!("login-shell ({})", shell.display())
        }
        fleetglass_core::shell_env::EnvSource::ProcessEnv { reason } => {
            format!("process-env ({reason})")
        }
    };
    Ok(
        fleetglass_core::diagnostics::diagnose_kubeconfig_context_live(
            client,
            &loaded.config,
            &context,
            &shell_env_source,
        )
        .await,
    )
}

/// Invalidates any cached client and triggers all stores for `context` to re-authenticate and re-list (04-02).
#[tauri::command]
#[specta::specta]
fn reconnect_context(state: tauri::State<'_, AppState>, context: String) {
    state.manager.resync_context(&context);
}

/// Adds an ephemeral debug container to a pod and audits the operation.
#[tauri::command]
#[specta::specta]
async fn debug_pod(
    state: tauri::State<'_, AppState>,
    request: fleetglass_core::api::debug::DebugPodRequest,
) -> Result<fleetglass_core::api::debug::DebugPodResult, IpcError> {
    let settings = state.settings.get(&request.context);
    fleetglass_core::actions::debug::debug_pod_and_audit(
        &state.manager,
        Arc::clone(&state.audit),
        &request,
        &settings,
    )
    .await
    .map_err(IpcError::from)
}

/// Returns the equivalent `kubectl debug` command for `request`.
#[tauri::command]
#[specta::specta]
fn preview_debug_pod(request: fleetglass_core::api::debug::DebugPodRequest) -> String {
    fleetglass_core::actions::debug::preview_debug_pod_kubectl(&request)
}

/// Returns the equivalent `kubectl` command for `action` on `context`.
#[tauri::command]
#[specta::specta]
fn preview_kubectl_action(context: String, action: MutatingAction) -> String {
    fleetglass_core::actions::preview_kubectl(&context, &action)
}

/// Executes a mutating action (delete, scale, restart, cordon, uncordon, drain) on
/// `context` after the read-only / protected-context guards, and records every
/// attempt, including rejections, in the local audit log.
#[tauri::command]
#[specta::specta]
async fn execute_action(
    state: tauri::State<'_, AppState>,
    context: String,
    action: MutatingAction,
    confirmed_name: Option<String>,
) -> Result<(), IpcError> {
    let settings = state.settings.get(&context);
    let request = ActionRequest {
        context,
        action,
        confirmed_name,
    };
    fleetglass_core::actions::execute_and_audit(
        &state.manager,
        Arc::clone(&state.audit),
        &request,
        &settings,
    )
    .await
    .map_err(IpcError::from)
}

/// Performs a server-side dry-run apply of `yaml` on `context` and returns
/// the resulting cleaned YAML without mutating cluster state (`R-05`).
#[tauri::command]
#[specta::specta]
async fn dry_run_apply(
    state: tauri::State<'_, AppState>,
    context: String,
    yaml: String,
) -> Result<String, IpcError> {
    let settings = state.settings.get(&context);
    fleetglass_core::apply::dry_run_apply(&state.manager, &context, &yaml, &settings)
        .await
        .map_err(IpcError::from)
}

/// Applies `yaml` to `context` using server-side apply after write-safety checks,
/// recording the attempt in the audit log and returning the applied YAML (`R-05`).
#[tauri::command]
#[specta::specta]
async fn apply_yaml(
    state: tauri::State<'_, AppState>,
    context: String,
    yaml: String,
    confirmed_name: Option<String>,
) -> Result<String, IpcError> {
    let settings = state.settings.get(&context);
    fleetglass_core::apply::apply_yaml(
        &state.manager,
        Arc::clone(&state.audit),
        &context,
        &yaml,
        confirmed_name,
        &settings,
    )
    .await
    .map_err(IpcError::from)
}

/// Starts a local port-forward listener for a pod and returns its active session metadata.
#[tauri::command]
#[specta::specta]
async fn start_port_forward(
    state: tauri::State<'_, AppState>,
    context: String,
    namespace: String,
    pod: String,
    remote_port: u16,
    local_port: u16,
) -> Result<PortForwardInfo, IpcError> {
    let client = state
        .manager
        .connect_client(&context)
        .await
        .map_err(IpcError::from)?;
    state
        .port_forwards
        .start_local(
            context,
            namespace,
            pod,
            remote_port,
            local_port,
            Some(client),
        )
        .await
        .map_err(IpcError::from)
}

/// Lists all active local port-forward sessions.
#[tauri::command]
#[specta::specta]
async fn list_port_forwards(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<PortForwardInfo>, IpcError> {
    Ok(state.port_forwards.list())
}

/// Stops an active local port-forward session by ID.
#[tauri::command]
#[specta::specta]
async fn stop_port_forward(state: tauri::State<'_, AppState>, id: u32) -> Result<bool, IpcError> {
    Ok(state.port_forwards.stop(id))
}

/// Starts a local port-forward listener resolving Pod/Service/Deployment targets (`04-08`).
#[tauri::command]
#[specta::specta]
async fn start_port_forward_target(
    state: tauri::State<'_, AppState>,
    context: String,
    namespace: String,
    target_kind: String,
    target_name: String,
    container_port: u16,
    local_port: u16,
) -> Result<PortForwardInfo, IpcError> {
    let client = state
        .manager
        .connect_client(&context)
        .await
        .map_err(IpcError::from)?;
    let pod_name = fleetglass_core::portforward::resolve_target_pod(
        &client,
        &namespace,
        &target_kind,
        &target_name,
    )
    .await
    .map_err(IpcError::from)?;
    state
        .port_forwards
        .start_local(
            context,
            namespace,
            pod_name,
            container_port,
            local_port,
            Some(client),
        )
        .await
        .map_err(IpcError::from)
}

/// Lists all saved port-forward bookmarks (`04-08`).
#[tauri::command]
#[specta::specta]
async fn list_saved_port_forwards(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SavedPortForward>, IpcError> {
    Ok(state.settings.get_saved_port_forwards())
}

/// Saves a port-forward bookmark (`04-08`).
#[tauri::command]
#[specta::specta]
async fn save_port_forward(
    state: tauri::State<'_, AppState>,
    forward: SavedPortForward,
) -> Result<(), IpcError> {
    state
        .settings
        .save_port_forward(forward)
        .map_err(IpcError::from)
}

/// Deletes a saved port-forward bookmark by ID (`04-08`).
#[tauri::command]
#[specta::specta]
async fn delete_saved_port_forward(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<bool, IpcError> {
    state
        .settings
        .delete_saved_port_forward(&id)
        .map_err(IpcError::from)
}

/// Starts an interactive container exec session and streams output chunks over `on_output` (`R-10`, `V-02`).
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
async fn start_exec_session(
    state: tauri::State<'_, AppState>,
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
    command: Option<Vec<String>>,
    confirmed_name: Option<String>,
    on_output: tauri::ipc::Channel<String>,
) -> Result<u32, IpcError> {
    let settings = state.settings.get(&context);
    let spec = ExecSpec {
        context: context.clone(),
        namespace: namespace.clone(),
        pod: pod.clone(),
        container,
        command: command.unwrap_or_default(),
        cols: 80,
        rows: 24,
    };

    if let Err(err) =
        fleetglass_core::exec::validate_exec_allowed(&spec, &settings, confirmed_name.as_deref())
    {
        let entry = AuditEntry {
            id: 0,
            timestamp: fleetglass_core::audit::now_rfc3339(),
            cluster: context.clone(),
            user: state.manager.source().context_user(&context),
            action: "exec".to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some(namespace.clone()),
            name: pod.clone(),
            kubectl: fleetglass_core::exec::preview_exec_kubectl(&spec),
            success: false,
            error: Some(err.to_string()),
        };
        state.audit.append_or_warn(&entry);
        return Err(IpcError::from(err));
    }

    let client = state
        .manager
        .connect_client(&context)
        .await
        .map_err(IpcError::from)?;
    let (output_tx, mut output_rx) = tauri::async_runtime::channel(256);
    let session = fleetglass_core::exec::start_exec_session(
        &client,
        &spec,
        &settings,
        confirmed_name.as_deref(),
        output_tx,
    )
    .await
    .map_err(IpcError::from)?;

    let entry = AuditEntry {
        id: 0,
        timestamp: fleetglass_core::audit::now_rfc3339(),
        cluster: context.clone(),
        user: state.manager.source().context_user(&context),
        action: "exec".to_owned(),
        kind: "Pod".to_owned(),
        namespace: Some(namespace.clone()),
        name: pod.clone(),
        kubectl: fleetglass_core::exec::preview_exec_kubectl(&spec),
        success: true,
        error: None,
    };
    state.audit.append_or_warn(&entry);

    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(chunk) = output_rx.recv().await {
            if on_output.send(chunk).is_err() {
                break;
            }
        }
    });

    Ok(state.insert_exec_session(session, forwarder))
}

/// Sends stdin data to an active container exec session (`R-10`).
#[tauri::command]
#[specta::specta]
async fn write_exec_stdin(
    state: tauri::State<'_, AppState>,
    id: u32,
    data: String,
) -> Result<(), IpcError> {
    state.write_exec_stdin(id, data).await
}

/// Resizes an active container exec/attach/node-debug session's remote PTY dimensions (`05-02`).
#[tauri::command]
#[specta::specta]
async fn resize_exec_session(
    state: tauri::State<'_, AppState>,
    id: u32,
    cols: u16,
    rows: u16,
) -> Result<(), IpcError> {
    state.resize_exec_session(id, cols, rows)
}

/// Stops an active container exec session by ID (`R-10`).
#[tauri::command]
#[specta::specta]
async fn stop_exec_session(state: tauri::State<'_, AppState>, id: u32) -> Result<bool, IpcError> {
    Ok(state.stop_exec_session(id))
}

/// Returns the equivalent `kubectl attach` command for attaching to `pod` on `context` (`04-03c`).
#[tauri::command]
#[specta::specta]
fn preview_kubectl_attach(
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
) -> String {
    let spec = fleetglass_core::api::AttachSpec {
        context,
        namespace,
        pod,
        container,
        cols: 80,
        rows: 24,
    };
    fleetglass_core::exec::preview_attach_kubectl(&spec)
}

/// Starts an interactive container attach session and streams output chunks over `on_output` (`04-03c`).
#[tauri::command]
#[specta::specta]
async fn start_attach_session(
    state: tauri::State<'_, AppState>,
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
    confirmed_name: Option<String>,
    on_output: tauri::ipc::Channel<String>,
) -> Result<u32, IpcError> {
    let settings = state.settings.get(&context);
    let spec = fleetglass_core::api::AttachSpec {
        context: context.clone(),
        namespace: namespace.clone(),
        pod: pod.clone(),
        container,
        cols: 80,
        rows: 24,
    };

    if let Err(err) =
        fleetglass_core::exec::validate_attach_allowed(&spec, &settings, confirmed_name.as_deref())
    {
        let entry = AuditEntry {
            id: 0,
            timestamp: fleetglass_core::audit::now_rfc3339(),
            cluster: context.clone(),
            user: state.manager.source().context_user(&context),
            action: "attach".to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some(namespace.clone()),
            name: pod.clone(),
            kubectl: fleetglass_core::exec::preview_attach_kubectl(&spec),
            success: false,
            error: Some(err.to_string()),
        };
        state.audit.append_or_warn(&entry);
        return Err(IpcError::from(err));
    }

    let client = state
        .manager
        .connect_client(&context)
        .await
        .map_err(IpcError::from)?;
    let (output_tx, mut output_rx) = tauri::async_runtime::channel(256);
    let session = fleetglass_core::exec::start_attach_session(
        &client,
        &spec,
        &settings,
        confirmed_name.as_deref(),
        output_tx,
    )
    .await
    .map_err(IpcError::from)?;

    let entry = AuditEntry {
        id: 0,
        timestamp: fleetglass_core::audit::now_rfc3339(),
        cluster: context.clone(),
        user: state.manager.source().context_user(&context),
        action: "attach".to_owned(),
        kind: "Pod".to_owned(),
        namespace: Some(namespace.clone()),
        name: pod.clone(),
        kubectl: fleetglass_core::exec::preview_attach_kubectl(&spec),
        success: true,
        error: None,
    };
    state.audit.append_or_warn(&entry);

    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(chunk) = output_rx.recv().await {
            if on_output.send(chunk).is_err() {
                break;
            }
        }
    });

    Ok(state.insert_exec_session(session, forwarder))
}

/// Returns the equivalent `kubectl debug node/...` command for `request` (`04-03d`).
#[tauri::command]
#[specta::specta]
fn preview_debug_node(request: fleetglass_core::api::debug::DebugNodeRequest) -> String {
    fleetglass_core::actions::debug::preview_debug_node_kubectl(&request)
}

/// Starts an interactive node host debug shell session and streams output chunks over `on_output` (`04-03d`).
#[tauri::command]
#[specta::specta]
async fn start_node_debug_session(
    state: tauri::State<'_, AppState>,
    request: fleetglass_core::api::debug::DebugNodeRequest,
    on_output: tauri::ipc::Channel<String>,
) -> Result<u32, IpcError> {
    let settings = state.settings.get(&request.context);

    if let Err(err) = fleetglass_core::actions::debug::validate_node_debug_request(&request) {
        return Err(IpcError::from(err));
    }

    if let Err(err) = fleetglass_core::actions::enforce_write_safety(
        &request.context,
        &settings,
        &request.node,
        request.confirmed_name.as_deref(),
    ) {
        let entry = AuditEntry {
            id: 0,
            timestamp: fleetglass_core::audit::now_rfc3339(),
            cluster: request.context.clone(),
            user: state.manager.source().context_user(&request.context),
            action: "debug_node".to_owned(),
            kind: "Node".to_owned(),
            namespace: request.namespace.clone(),
            name: request.node.clone(),
            kubectl: fleetglass_core::actions::debug::preview_debug_node_kubectl(&request),
            success: false,
            error: Some(err.to_string()),
        };
        state.audit.append_or_warn(&entry);
        return Err(IpcError::from(err));
    }

    let client = state
        .manager
        .connect_client(&request.context)
        .await
        .map_err(IpcError::from)?;
    let (output_tx, mut output_rx) = tauri::async_runtime::channel(256);
    let (pod_name, session) = fleetglass_core::actions::debug::start_node_debug_session(
        &client, &request, &settings, output_tx,
    )
    .await
    .map_err(|err| {
        let entry = AuditEntry {
            id: 0,
            timestamp: fleetglass_core::audit::now_rfc3339(),
            cluster: request.context.clone(),
            user: state.manager.source().context_user(&request.context),
            action: "debug_node".to_owned(),
            kind: "Node".to_owned(),
            namespace: request.namespace.clone(),
            name: request.node.clone(),
            kubectl: fleetglass_core::actions::debug::preview_debug_node_kubectl(&request),
            success: false,
            error: Some(err.to_string()),
        };
        state.audit.append_or_warn(&entry);
        IpcError::from(err)
    })?;

    let entry = AuditEntry {
        id: 0,
        timestamp: fleetglass_core::audit::now_rfc3339(),
        cluster: request.context.clone(),
        user: state.manager.source().context_user(&request.context),
        action: "debug_node".to_owned(),
        kind: "Node".to_owned(),
        namespace: request.namespace.clone(),
        name: format!("{} (pod/{})", request.node, pod_name),
        kubectl: fleetglass_core::actions::debug::preview_debug_node_kubectl(&request),
        success: true,
        error: None,
    };
    state.audit.append_or_warn(&entry);

    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(chunk) = output_rx.recv().await {
            if on_output.send(chunk).is_err() {
                break;
            }
        }
    });

    Ok(state.insert_exec_session(session, forwarder))
}

/// Deletes a node host debug pod explicitly (`04-03d`).
#[tauri::command]
#[specta::specta]
async fn delete_node_debug_pod(
    state: tauri::State<'_, AppState>,
    context: String,
    namespace: String,
    pod_name: String,
) -> Result<(), IpcError> {
    let client = state
        .manager
        .connect_client(&context)
        .await
        .map_err(IpcError::from)?;
    fleetglass_core::actions::debug::delete_node_debug_pod(&client, &namespace, &pod_name)
        .await
        .map_err(IpcError::from)
}

/// Returns all configured cluster groups sorted by name (bounded by user config).
#[tauri::command]
#[specta::specta]
async fn get_cluster_groups(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ClusterGroup>, IpcError> {
    Ok(state.settings.get_cluster_groups())
}

/// Adds or replaces a named cluster group and persists it.
#[tauri::command]
#[specta::specta]
async fn set_cluster_group(
    state: tauri::State<'_, AppState>,
    group: ClusterGroup,
) -> Result<(), IpcError> {
    let store = Arc::clone(&state.settings);
    run_blocking("set_cluster_group", move || store.set_cluster_group(group)).await
}

/// Deletes a cluster group by name and persists the change.
#[tauri::command]
#[specta::specta]
async fn delete_cluster_group(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<(), IpcError> {
    let store = Arc::clone(&state.settings);
    run_blocking("delete_cluster_group", move || {
        store.delete_cluster_group(&name)
    })
    .await
}

/// Compares two resource definitions (YAML or JSON), ignoring runtime metadata.
#[tauri::command]
#[specta::specta]
async fn compare_resources(left_yaml: String, right_yaml: String) -> Result<DiffResult, IpcError> {
    run_blocking(
        "compare_resources",
        move || -> fleetglass_core::Result<_> {
            Ok(fleetglass_core::detail::compare_resources(
                &left_yaml,
                &right_yaml,
            ))
        },
    )
    .await
}

/// Compares two resource definitions with normalization and diff options (04-04).
#[tauri::command]
#[specta::specta]
async fn compare_resources_with_options(
    left_yaml: String,
    right_yaml: String,
    options: Option<CompareOptions>,
) -> Result<DiffResult, IpcError> {
    run_blocking(
        "compare_resources_with_options",
        move || -> fleetglass_core::Result<_> {
            Ok(fleetglass_core::detail::compare_resources_with_options(
                &left_yaml,
                &right_yaml,
                options.unwrap_or_default(),
            ))
        },
    )
    .await
}

/// Returns detailed resource information, including cleaned YAML and related events (`R-03`).
#[tauri::command]
#[specta::specta]
async fn get_resource_detail(
    state: tauri::State<'_, AppState>,
    context: String,
    kind: String,
    namespace: Option<String>,
    name: String,
) -> Result<ResourceDetail, IpcError> {
    let client = state
        .manager
        .connect_client(&context)
        .await
        .map_err(IpcError::from)?;
    fleetglass_core::detail::fetch_resource_detail(
        &client,
        &context,
        &kind,
        namespace.as_deref(),
        &name,
    )
    .await
    .map_err(IpcError::from)
}

/// Returns the cleaned YAML representation of a Kubernetes resource (`R-03`).
#[tauri::command]
#[specta::specta]
async fn get_resource_yaml(
    state: tauri::State<'_, AppState>,
    context: String,
    kind: String,
    namespace: Option<String>,
    name: String,
) -> Result<String, IpcError> {
    let client = state
        .manager
        .connect_client(&context)
        .await
        .map_err(IpcError::from)?;
    fleetglass_core::detail::fetch_resource_yaml(&client, &kind, namespace.as_deref(), &name)
        .await
        .map_err(IpcError::from)
}

/// Returns the JSON schema of `api_version`/`kind` from the cluster's OpenAPI v3 document,
/// for validation, completion and hover docs in the YAML editor (`05-04`).
///
/// Bounded: one resolved schema, capped in nodes and description length. A schema that
/// cannot be fetched (RBAC, offline, old cluster, unknown kind) is an `unavailable`
/// result, not an error.
#[tauri::command]
#[specta::specta]
async fn get_resource_schema(
    state: tauri::State<'_, AppState>,
    context: String,
    api_version: String,
    kind: String,
) -> Result<ResourceSchemaResult, IpcError> {
    Ok(state
        .manager
        .resource_schema(&context, &api_version, &kind)
        .await)
}

/// Decodes base64 secret values into (lossy UTF-8) plaintext.
#[tauri::command]
#[specta::specta]
async fn decode_secret_values(
    data: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, IpcError> {
    run_blocking("decode_secret_values", move || {
        fleetglass_core::detail::decode_secret_values(&data)
    })
    .await
}

/// Encodes plaintext secret values as base64.
#[tauri::command]
#[specta::specta]
async fn encode_secret_values(
    data: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, IpcError> {
    run_blocking(
        "encode_secret_values",
        move || -> fleetglass_core::Result<_> {
            Ok(fleetglass_core::detail::encode_secret_values(&data))
        },
    )
    .await
}

/// Returns a bounded, sorted window of applications grouped from the watch stores.
///
/// Starts the pod watcher of every cluster in `spec` first (other kinds join once
/// their stores are watched); per-store RBAC / connection state is reported in
/// [`AppPage::access`].
#[tauri::command]
#[specta::specta]
async fn query_apps(
    state: tauri::State<'_, AppState>,
    spec: AppQuerySpec,
) -> Result<AppPage, IpcError> {
    let manager = Arc::clone(&state.manager);
    manager
        .ensure_watching_pairs(&spec.clusters, &["Pod"])
        .await;
    run_blocking("query_apps", move || -> fleetglass_core::Result<_> {
        Ok(manager.query_apps(&spec))
    })
    .await
}

/// Row and failing-row counts per kind for the sidebar, over the stores that are
/// already watched for `cluster` (one entry per store, bounded). Never connects
/// or starts a watcher; an unopened cluster returns an empty list.
#[tauri::command]
#[specta::specta]
async fn kind_counts(
    state: tauri::State<'_, AppState>,
    cluster: String,
) -> Result<Vec<KindCount>, IpcError> {
    let manager = Arc::clone(&state.manager);
    run_blocking("kind_counts", move || -> fleetglass_core::Result<_> {
        Ok(manager.kind_counts(&cluster))
    })
    .await
}

/// Returns the saved table layout for resource `kind`, if any.
#[tauri::command]
#[specta::specta]
async fn get_column_layout(
    state: tauri::State<'_, AppState>,
    kind: String,
) -> Result<Option<ColumnLayout>, IpcError> {
    Ok(state.settings.get_column_layout(&kind))
}

/// Validates the custom column paths of `layout` and persists it.
/// An invalid `jsonPath` is rejected with an `invalidInput` error.
#[tauri::command]
#[specta::specta]
async fn set_column_layout(
    state: tauri::State<'_, AppState>,
    layout: ColumnLayout,
) -> Result<(), IpcError> {
    let store = Arc::clone(&state.settings);
    let manager = Arc::clone(&state.manager);
    run_blocking("set_column_layout", move || {
        store.set_column_layout(layout.clone())?;
        manager.set_custom_columns(&layout.kind, layout.custom)
    })
    .await
}

/// Follows the logs of every pod of a workload across rollouts, pushing
/// batched [`MuxLogEvent`]s over `on_event`. Returns an ID for [`stop_workload_logs`].
#[tauri::command]
#[specta::specta]
async fn start_workload_logs(
    state: tauri::State<'_, AppState>,
    target: WorkloadLogTarget,
    on_event: tauri::ipc::Channel<MuxLogEvent>,
) -> Result<u32, IpcError> {
    fleetglass_core::logs_mux::validate_target(&target).map_err(IpcError::invalid_input)?;
    let client = state
        .manager
        .connect_client(&target.cluster)
        .await
        .map_err(IpcError::from)?;
    let (tx, mut rx) = tauri::async_runtime::channel(LOG_EVENT_CHANNEL_CAPACITY);
    let next_seq = Arc::new(AtomicU32::new(0));
    let next_seq_forwarder = Arc::clone(&next_seq);

    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let MuxLogEvent::Lines { lines } = &event
                && let Some(last) = lines.last()
            {
                next_seq_forwarder.store(last.seq.wrapping_add(1), Ordering::Release);
            }
            if on_event.send(event).is_err() {
                break;
            }
        }
    });

    let mut map = state
        .log_followers
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let background = !state.manager.is_foreground();
    let follower = if background {
        None
    } else {
        Some(fleetglass_core::logs_mux::spawn_workload_log_follower_from(
            client,
            target.clone(),
            0,
            tx.clone(),
        ))
    };
    let resume = LogResume {
        target,
        sink: tx,
        next_seq,
        paused_at: background.then(Instant::now),
    };

    let mut id = state.next_log_follower_id.fetch_add(1, Ordering::Relaxed);
    while id == 0 || map.contains_key(&id) {
        id = state.next_log_follower_id.fetch_add(1, Ordering::Relaxed);
    }
    map.insert(
        id,
        LogFollower {
            follower,
            forwarder,
            resume: Some(resume),
        },
    );
    Ok(id)
}

/// Tells the backend whether the app is visible (mobile only; desktop never
/// calls it and stays in the foreground). In the background every watcher,
/// metrics poller, overview refresh and log stream stops at once, keeping
/// subscriptions and last-known data (pages report `access[].paused`); back in
/// the foreground subscribed stores re-list and log streams continue from where
/// they stopped. Idempotent.
#[tauri::command]
#[specta::specta]
async fn set_app_foreground(
    state: tauri::State<'_, AppState>,
    foreground: bool,
) -> Result<(), IpcError> {
    state.manager.set_foreground(foreground).await;
    if foreground {
        state.resume_log_followers().await;
    } else {
        state.pause_log_followers();
    }
    Ok(())
}

/// Stops a workload log stream opened by [`start_workload_logs`].
#[tauri::command]
#[specta::specta]
async fn stop_workload_logs(state: tauri::State<'_, AppState>, id: u32) -> Result<bool, IpcError> {
    Ok(state.stop_log_follower(id))
}

/// Opens a separate `logs-<n>` window that follows `target`.
#[tauri::command]
#[specta::specta]
async fn open_detached_logs(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    target: WorkloadLogTarget,
) -> Result<(), IpcError> {
    let json = serde_json::to_string(&target)
        .map_err(|e| IpcError::internal(format!("encoding logs target: {e}")))?;
    let n = state.next_logs_window.fetch_add(1, Ordering::Relaxed);
    let url = detached_logs_url(&json);
    tauri::WebviewWindowBuilder::new(
        &app,
        format!("logs-{n}"),
        tauri::WebviewUrl::App(url.into()),
    )
    .title(detached_logs_title(&target))
    .inner_size(1000.0, 700.0)
    .build()
    .map_err(|e| IpcError::internal(format!("opening logs window: {e}")))?;
    Ok(())
}

/// Largest log text `save_log_file` accepts.
const MAX_SAVED_LOG_BYTES: usize = 64 * 1024 * 1024;
/// Longest sanitised file name, including the `.log` extension.
const MAX_LOG_FILE_NAME_CHARS: usize = 100;
/// Highest ` (n)` suffix tried before giving up on finding a free name.
const MAX_LOG_FILE_SUFFIX: u32 = 99;

/// Saves `contents` as a new `.log` file in the user's Downloads directory (home
/// directory if there is none). Never overwrites: picks `name (1).log`, … if taken.
/// The webview's `<a download>` does nothing on WebKitGTK, hence this command.
#[tauri::command]
#[specta::specta]
async fn save_log_file(
    app: tauri::AppHandle,
    file_name: String,
    contents: String,
) -> Result<SavedFile, IpcError> {
    use tauri::Manager as _;
    if contents.len() > MAX_SAVED_LOG_BYTES {
        return Err(IpcError::invalid_input(format!(
            "log text is {} bytes; at most {} MiB can be saved",
            contents.len(),
            MAX_SAVED_LOG_BYTES / (1024 * 1024)
        )));
    }
    let paths = app.path();
    let dir = match paths.download_dir() {
        Ok(dir) if dir.is_dir() => dir,
        _ => paths
            .home_dir()
            .map_err(|e| IpcError::internal(format!("no Downloads or home directory: {e}")))?,
    };
    let path = run_blocking("save_log_file", move || {
        write_new_log_file(&dir, &file_name, contents.as_bytes())
    })
    .await?;
    Ok(SavedFile {
        path: path.display().to_string(),
    })
}

/// File name restricted to `[A-Za-z0-9._-]` (others become `_`), without leading
/// dots, ending in `.log`, at most [`MAX_LOG_FILE_NAME_CHARS`] long; `logs.log` if empty.
fn sanitize_log_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.');
    let stem = cleaned.strip_suffix(".log").unwrap_or(cleaned);
    let stem: String = stem
        .chars()
        .take(MAX_LOG_FILE_NAME_CHARS - ".log".len())
        .collect();
    if stem.is_empty() {
        "logs.log".to_owned()
    } else {
        format!("{stem}.log")
    }
}

/// Creates a new file in `dir` named after `file_name` (sanitised), adding
/// ` (1)` … ` (99)` before `.log` if the name is taken, and writes `contents`.
fn write_new_log_file(dir: &Path, file_name: &str, contents: &[u8]) -> Result<PathBuf, IpcError> {
    use std::io::Write as _;
    let name = sanitize_log_file_name(file_name);
    let stem = name.strip_suffix(".log").unwrap_or(&name);
    for n in 0..=MAX_LOG_FILE_SUFFIX {
        let candidate = if n == 0 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem} ({n}).log"))
        };
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        // Logs can contain secrets: owner-only on Unix.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = match opts.open(&candidate) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(IpcError::internal(format!(
                    "cannot create {}: {e}",
                    candidate.display()
                )));
            }
        };
        if let Err(e) = file.write_all(contents).and_then(|()| file.sync_all()) {
            drop(file);
            // Do not leave a truncated file behind.
            let _ = std::fs::remove_file(&candidate);
            return Err(IpcError::internal(format!(
                "cannot write {}: {e}",
                candidate.display()
            )));
        }
        return Ok(candidate);
    }
    Err(IpcError::internal(format!(
        "{name} and {stem} (1).log … ({MAX_LOG_FILE_SUFFIX}).log already exist in {}",
        dir.display()
    )))
}

/// `Logs: <cluster>/<namespace>/<selector summary>`.
fn detached_logs_title(target: &WorkloadLogTarget) -> String {
    let selector = match &target.selector {
        WorkloadSelector::Pod { name } => format!("pod/{name}"),
        WorkloadSelector::Labels { selector } => selector.clone(),
        WorkloadSelector::Owner { kind, name } => format!("{kind}/{name}"),
    };
    format!("Logs: {}/{}/{selector}", target.cluster, target.namespace)
}

/// App-relative URL of the detached logs route carrying the JSON-encoded target.
fn detached_logs_url(target_json: &str) -> String {
    format!("index.html#/logs?target={}", url_encode(target_json))
}

/// Percent-encodes everything except RFC 3986 unreserved characters (UTF-8 bytes).
fn url_encode(input: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            // Writing to a String cannot fail.
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Lists Helm releases across namespaces or in `namespace` (`P2-07`).
#[tauri::command]
#[specta::specta]
async fn list_helm_releases(
    state: tauri::State<'_, AppState>,
    cluster: String,
    namespace: Option<String>,
    offset: u32,
    limit: u32,
) -> Result<HelmReleasePage, IpcError> {
    let client = state
        .manager
        .connect_client(&cluster)
        .await
        .map_err(IpcError::from)?;
    let releases = fleetglass_core::helm::list_helm_releases(&client, namespace.as_deref())
        .await
        .map_err(|e| IpcError::from(&e))?;
    Ok(fleetglass_core::helm::window_helm_releases(
        &releases, offset, limit,
    ))
}

/// Returns the revision history of a Helm release, newest first (`P2-07`).
#[tauri::command]
#[specta::specta]
async fn get_helm_release_history(
    state: tauri::State<'_, AppState>,
    cluster: String,
    namespace: String,
    name: String,
) -> Result<Vec<HelmReleaseSummary>, IpcError> {
    let client = state
        .manager
        .connect_client(&cluster)
        .await
        .map_err(IpcError::from)?;
    fleetglass_core::helm::helm_release_history(&client, &namespace, &name)
        .await
        .map_err(|e| IpcError::from(&e))
}

/// Diffs values and manifest between two revisions of a Helm release (`P2-07`).
#[tauri::command]
#[specta::specta]
async fn diff_helm_revisions(
    state: tauri::State<'_, AppState>,
    cluster: String,
    namespace: String,
    name: String,
    rev_a: u32,
    rev_b: u32,
) -> Result<HelmRevisionDiff, IpcError> {
    let client = state
        .manager
        .connect_client(&cluster)
        .await
        .map_err(IpcError::from)?;
    let a = fleetglass_core::helm::get_helm_release_detail(&client, &namespace, &name, rev_a)
        .await
        .map_err(|e| IpcError::from(&e))?;
    let b = fleetglass_core::helm::get_helm_release_detail(&client, &namespace, &name, rev_b)
        .await
        .map_err(|e| IpcError::from(&e))?;
    Ok(fleetglass_core::helm::diff_helm_revisions(&a, &b))
}

/// Previews the equivalent `helm rollback` command (`P2-07`).
#[tauri::command]
#[specta::specta]
async fn preview_helm_rollback(
    namespace: String,
    name: String,
    revision: u32,
) -> Result<String, IpcError> {
    Ok(fleetglass_core::helm::preview_helm_rollback(
        &namespace, &name, revision,
    ))
}

/// Returns the Prometheus configuration for `context` (`P2-09`).
#[tauri::command]
#[specta::specta]
async fn get_prometheus_config(
    state: tauri::State<'_, AppState>,
    context: String,
) -> Result<PrometheusConfig, IpcError> {
    Ok(state.settings.get_prometheus_config(&context))
}

/// Persists Prometheus configuration for `context` (`P2-09`).
#[tauri::command]
#[specta::specta]
async fn set_prometheus_config(
    state: tauri::State<'_, AppState>,
    context: String,
    config: PrometheusConfig,
) -> Result<(), IpcError> {
    let store = Arc::clone(&state.settings);
    run_blocking("set_prometheus_config", move || {
        store.set_prometheus_config(context, config)
    })
    .await
}

/// Probes candidate Prometheus service endpoints through the API proxy (`P2-09`).
#[tauri::command]
#[specta::specta]
async fn detect_prometheus(
    state: tauri::State<'_, AppState>,
    cluster: String,
) -> Result<PrometheusDetection, IpcError> {
    let client = state
        .manager
        .connect_client(&cluster)
        .await
        .map_err(IpcError::from)?;
    Ok(fleetglass_core::prometheus::detect_prometheus(&client).await)
}

/// Queries CPU and memory utilization series for `pods` via the API proxy (`P2-09`).
#[tauri::command]
#[specta::specta]
async fn query_pod_usage(
    state: tauri::State<'_, AppState>,
    cluster: String,
    namespace: String,
    pods: Vec<String>,
    range: MetricsRange,
) -> Result<PodUsageSeries, IpcError> {
    let cfg = state.settings.get_prometheus_config(&cluster);
    let client = state
        .manager
        .connect_client(&cluster)
        .await
        .map_err(IpcError::from)?;
    fleetglass_core::prometheus::query_pod_usage(&client, &cfg, &namespace, &pods, range)
        .await
        .map_err(|e| IpcError::from(&e))
}

/// Returns whether the user opted into update checks (`P2-10`).
#[tauri::command]
#[specta::specta]
async fn get_update_check_enabled(state: tauri::State<'_, AppState>) -> Result<bool, IpcError> {
    Ok(state.settings.update_check_enabled())
}

/// Enables or disables the opt-in update check (`P2-10`).
#[tauri::command]
#[specta::specta]
async fn set_update_check_enabled(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), IpcError> {
    let store = Arc::clone(&state.settings);
    run_blocking("set_update_check_enabled", move || {
        store.set_update_check_enabled(enabled)
    })
    .await
}

/// Checks for updates if opted in (`P2-10`).
#[tauri::command]
#[specta::specta]
async fn check_for_update(
    state: tauri::State<'_, AppState>,
) -> Result<Option<UpdateInfo>, IpcError> {
    if !state.settings.update_check_enabled() {
        return Err(IpcError {
            kind: ErrorKind::UpdateCheckDisabled,
            message: "Automatic update checking is disabled in settings".to_owned(),
        });
    }
    // Not implemented yet: no updater plugin, signing key or release endpoint is configured
    // (docs/KNOWN-LIMITATIONS.md). Until then report "no update" without any network call.
    Ok(None)
}

/// Queries the local action audit log in reverse chronological order with optional filtering.
#[tauri::command]
#[specta::specta]
async fn get_audit_log(
    state: tauri::State<'_, AppState>,
    offset: u32,
    limit: u32,
    cluster: Option<String>,
    action: Option<String>,
) -> Result<AuditLogPage, IpcError> {
    let audit = Arc::clone(&state.audit);
    run_blocking("get_audit_log", move || {
        audit.query(offset, limit, cluster.as_deref(), action.as_deref())
    })
    .await
}

/// Merges Git-shared team settings from a local YAML file into context settings.
#[tauri::command]
#[specta::specta]
async fn reload_team_settings(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<TeamSettingsSummary, IpcError> {
    let store = Arc::clone(&state.settings);
    let summary = run_blocking("reload_team_settings", move || {
        fleetglass_core::team_settings::merge_team_settings(&store, Path::new(&path))
    })
    .await?;
    sync_watch_namespaces(&state.manager, &state.settings);
    Ok(summary)
}

/// Maximum number of plugins returned by [`list_plugins`].
const MAX_LISTED_PLUGINS: usize = 200;
/// Largest plugin file served over `fgplugin://`.
const MAX_PLUGIN_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Whether this platform's webview serves custom schemes through wry's workaround URL
/// `http(s)://fgplugin.localhost/<id>/<path>` (WebView2 on Windows, Android), which wry
/// rewrites to `fgplugin://localhost/<id>/<path>` before calling our handler. There all
/// plugins share one origin; elsewhere each plugin has its own `fgplugin://<id>` origin.
const PLUGIN_SHARED_ORIGIN: bool = cfg!(any(windows, target_os = "android"));

/// The plugin's own CSP sources (with a leading space). The iframe is sandboxed (opaque
/// origin), so engines do not match the plugin's files against `'self'`; its origin is
/// listed explicitly. With the shared Windows origin the source is path-scoped to the
/// plugin's own directory, so a plugin cannot load another plugin's scripts.
fn plugin_own_sources(plugin_id: &str, shared_origin: bool) -> String {
    if shared_origin {
        format!(" http://fgplugin.localhost/{plugin_id}/ https://fgplugin.localhost/{plugin_id}/")
    } else {
        format!(" fgplugin://{plugin_id}")
    }
}

/// CSP sent with every plugin response. Plugins talk to the app only via `postMessage`;
/// they cannot fetch/XHR/WebSocket, submit forms, set `<base>` or load remote code. Only
/// the app's own pages may embed a plugin. `own` comes from [`plugin_own_sources`] (or is
/// empty for errors without a plugin id).
fn plugin_csp(own: &str) -> String {
    let dev = if cfg!(debug_assertions) {
        " http://localhost:1420"
    } else {
        ""
    };
    format!(
        "default-src 'none'; script-src 'self'{own}; style-src 'self'{own} 'unsafe-inline'; \
img-src 'self'{own} data:; connect-src 'none'; form-action 'none'; base-uri 'none'; \
frame-ancestors tauri://localhost http://tauri.localhost https://tauri.localhost{dev}"
    )
}

/// Validated plugins by id, so serving a file does not rescan every plugin directory.
/// The cache is rebuilt by [`list_plugins`] and on a miss; a hit is re-validated with
/// [`fleetglass_core::plugins::load_plugin_dir`] (same rules as discovery).
///
/// Unknown ids are negatively cached: a request for an id that is not in the cache only
/// triggers a rescan if the last full scan is older than `miss_rescan_interval`, so a
/// frame requesting random `fgplugin://<id>` hosts cannot force a scan per request.
/// [`list_plugins`] always rescans, so a newly installed plugin shows up when listed.
struct PluginIndex {
    dirs: Vec<PathBuf>,
    cache: Mutex<HashMap<String, PathBuf>>,
    last_scan: Mutex<Option<std::time::Instant>>,
    miss_rescan_interval: std::time::Duration,
}

const PLUGIN_MISS_RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

impl PluginIndex {
    fn new(dirs: Vec<PathBuf>) -> Self {
        Self::with_miss_rescan_interval(dirs, PLUGIN_MISS_RESCAN_INTERVAL)
    }

    fn with_miss_rescan_interval(dirs: Vec<PathBuf>, interval: std::time::Duration) -> Self {
        Self {
            dirs,
            cache: Mutex::new(HashMap::new()),
            last_scan: Mutex::new(None),
            miss_rescan_interval: interval,
        }
    }

    /// Full discovery; replaces the cache.
    fn refresh(
        &self,
    ) -> fleetglass_core::plugins::Result<Vec<fleetglass_core::plugins::DiscoveredPlugin>> {
        *self
            .last_scan
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(std::time::Instant::now());
        let all = discover_all_plugins(&self.dirs)?;
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = all
            .iter()
            .map(|p| (p.manifest.id.clone(), p.dir.clone()))
            .collect();
        Ok(all)
    }

    /// Directory of the validated plugin `plugin_id`, if installed.
    fn plugin_dir(&self, plugin_id: &str) -> Option<PathBuf> {
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(plugin_id)
            .cloned();
        match cached {
            Some(dir)
                if fleetglass_core::plugins::load_plugin_dir(&dir)
                    .is_ok_and(|p| p.manifest.id == plugin_id) =>
            {
                return Some(dir);
            }
            // A stale cached entry means the plugins changed: always rescan.
            Some(_) => {}
            None => {
                let recent = self
                    .last_scan
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some_and(|t| t.elapsed() < self.miss_rescan_interval);
                if recent {
                    return None;
                }
            }
        }
        self.refresh()
            .ok()?
            .into_iter()
            .find(|p| p.manifest.id == plugin_id)
            .map(|p| p.dir)
    }
}

/// Directories searched for plugins, highest priority first. Release builds only use the
/// user plugins dir; debug builds also use the repository's `plugins/` (fixed path, not cwd).
fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![SettingsStore::default_path().with_file_name("plugins")];
    if cfg!(debug_assertions) {
        dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins"));
    }
    dirs
}

/// Validated plugins from `dirs`, deduplicated by id (first dir wins), sorted by id.
fn discover_all_plugins(
    dirs: &[PathBuf],
) -> fleetglass_core::plugins::Result<Vec<fleetglass_core::plugins::DiscoveredPlugin>> {
    let mut all: Vec<fleetglass_core::plugins::DiscoveredPlugin> = Vec::new();
    for dir in dirs {
        let found = match fleetglass_core::plugins::discover_plugin_dirs(dir) {
            Ok(found) => found,
            Err(fleetglass_core::plugins::Error::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        for p in found {
            if !all.iter().any(|e| e.manifest.id == p.manifest.id) {
                all.push(p);
            }
        }
    }
    all.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(all)
}

/// Resolves `relative_path` inside the directory of the discovered (validated) plugin
/// `plugin_id`. Symlinked plugin roots are never discovered; symlinks inside a plugin may
/// not escape its directory.
fn find_plugin_file(index: &PluginIndex, plugin_id: &str, relative_path: &str) -> Option<PathBuf> {
    let root = index.plugin_dir(plugin_id)?.canonicalize().ok()?;
    let target = root.join(relative_path).canonicalize().ok()?;
    (target.starts_with(&root) && target.is_file()).then_some(target)
}

/// Builds a plugin response. Infallible: every response carries the CSP and `nosniff`.
fn plugin_response(
    status: u16,
    content_type: &'static str,
    own_sources: &str,
    body: Vec<u8>,
) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::{HeaderValue, StatusCode, header};
    let mut resp = tauri::http::Response::new(body);
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    let csp = HeaderValue::from_str(&plugin_csp(own_sources)).unwrap_or_else(|_| {
        // Unreachable for validated ids; never serve content without a CSP.
        *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        resp.body_mut().clear();
        HeaderValue::from_static("default-src 'none'")
    });
    resp.headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, csp);
    resp
}

/// Splits a plugin request URI (as our handler receives it) into `(plugin id, path)`.
/// `shared_origin` (see [`PLUGIN_SHARED_ORIGIN`]): `fgplugin://localhost/<id>/<path>`;
/// otherwise `fgplugin://<id>/<path>`. Exactly one `/` after the id is consumed.
fn parse_plugin_uri(uri: &tauri::http::Uri, shared_origin: bool) -> Option<(&str, &str)> {
    let host = uri.host()?;
    let path = uri.path();
    let path = path.strip_prefix('/').unwrap_or(path);
    if shared_origin {
        (host == "localhost").then(|| path.split_once('/').unwrap_or((path, "")))
    } else {
        Some((host, path))
    }
}

fn serve_plugin_request_from(
    index: &PluginIndex,
    req: &tauri::http::Request<Vec<u8>>,
    shared_origin: bool,
) -> tauri::http::Response<Vec<u8>> {
    const TEXT: &str = "text/plain; charset=utf-8";
    let Some((plugin_id, raw_path)) = parse_plugin_uri(req.uri(), shared_origin) else {
        return plugin_response(400, TEXT, "", b"missing plugin id".to_vec());
    };
    if plugin_id.is_empty()
        || !plugin_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return plugin_response(400, TEXT, "", b"invalid plugin id".to_vec());
    }
    let own = plugin_own_sources(plugin_id, shared_origin);
    let err = |status, msg: &[u8]| plugin_response(status, TEXT, &own, msg.to_vec());
    let rel_path = if raw_path.is_empty() {
        "index.html"
    } else {
        raw_path
    };
    if rel_path.starts_with('/')
        || rel_path.contains("..")
        || rel_path.contains('\\')
        || rel_path.contains('%')
        || rel_path.contains(':')
    {
        return err(400, b"invalid plugin path");
    }

    let Some(file_path) = find_plugin_file(index, plugin_id, rel_path) else {
        return err(404, b"plugin file not found");
    };
    match std::fs::metadata(&file_path) {
        Ok(meta) if meta.len() <= MAX_PLUGIN_FILE_BYTES => {}
        Ok(_) => return err(413, b"plugin file too large"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return err(404, b"plugin file not found");
        }
        Err(_) => return err(500, b"failed to read plugin file"),
    }
    let Ok(bytes) = std::fs::read(&file_path) else {
        return err(500, b"failed to read plugin file");
    };

    let mime = match file_path.extension().and_then(|s| s.to_str()) {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    plugin_response(200, mime, &own, bytes)
}

/// Navigation policy for app webviews (see [`run`] for what the hook covers): only the
/// app's own origins and plugin URLs. Everything else (remote and loopback sites, `file:`,
/// `data:`, `javascript:`) is denied. The hook does not say which frame navigates, so it
/// cannot tie a frame to its plugin id; `PluginSandboxView` stops serving the bridge to a
/// frame that navigated after its first load instead.
fn is_allowed_navigation(url: &tauri::Url) -> bool {
    let host = url.host_str();
    match url.scheme() {
        "tauri" => host == Some("localhost"),
        "fgplugin" => host.is_some_and(|h| {
            !h.is_empty()
                && h.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }),
        "about" => matches!(url.path(), "blank" | "srcdoc"),
        "http" | "https" => {
            // These are wry's workaround origins only where custom schemes use them;
            // elsewhere `*.localhost` resolves to loopback and could reach a local server.
            let app_origin = PLUGIN_SHARED_ORIGIN
                && matches!(host, Some("tauri.localhost" | "fgplugin.localhost"))
                && url.port().is_none();
            let dev_server = cfg!(debug_assertions)
                && url.scheme() == "http"
                && host == Some("localhost")
                && url.port() == Some(1420);
            app_origin || dev_server
        }
        _ => false,
    }
}

/// Discovers installed plugins (user plugins dir; plus the repo `plugins/` in debug builds).
#[tauri::command]
#[specta::specta]
async fn list_plugins(state: tauri::State<'_, AppState>) -> Result<Vec<PluginManifest>, IpcError> {
    let index = Arc::clone(&state.plugin_index);
    run_blocking("list_plugins", move || {
        let mut plugins: Vec<PluginManifest> =
            index.refresh()?.into_iter().map(|p| p.manifest).collect();
        plugins.truncate(MAX_LISTED_PLUGINS);
        Ok::<_, fleetglass_core::plugins::Error>(plugins)
    })
    .await
}

trait RunBlockingResult<T> {
    fn into_ipc_result(self) -> Result<T, IpcError>;
}

impl<T> RunBlockingResult<T> for fleetglass_core::Result<T> {
    fn into_ipc_result(self) -> Result<T, IpcError> {
        self.map_err(IpcError::from)
    }
}

impl<T> RunBlockingResult<T> for fleetglass_core::team_settings::Result<T> {
    fn into_ipc_result(self) -> Result<T, IpcError> {
        self.map_err(|err| match err {
            fleetglass_core::team_settings::Error::InvalidInput(_) => IpcError {
                kind: ErrorKind::InvalidInput,
                message: err.to_string(),
            },
            fleetglass_core::team_settings::Error::Io { .. } => IpcError::internal(err.to_string()),
            fleetglass_core::team_settings::Error::Core(e) => IpcError::from(e),
        })
    }
}

impl<T> RunBlockingResult<T> for fleetglass_core::plugins::Result<T> {
    fn into_ipc_result(self) -> Result<T, IpcError> {
        self.map_err(|err| match err {
            fleetglass_core::plugins::Error::InvalidInput(_) => IpcError {
                kind: ErrorKind::InvalidInput,
                message: err.to_string(),
            },
            fleetglass_core::plugins::Error::Io { .. } => IpcError::internal(err.to_string()),
        })
    }
}

impl<T> RunBlockingResult<T> for std::io::Result<T> {
    fn into_ipc_result(self) -> Result<T, IpcError> {
        self.map_err(|e| IpcError::internal(e.to_string()))
    }
}

impl<T> RunBlockingResult<T> for Result<T, IpcError> {
    fn into_ipc_result(self) -> Result<T, IpcError> {
        self
    }
}

/// Run blocking core work on Tauri's blocking pool and map errors for IPC.
/// A panic in `f` becomes an [`ErrorKind::Internal`](fleetglass_core::api::ErrorKind) error.
async fn run_blocking<T, R, F>(what: &str, f: F) -> Result<T, IpcError>
where
    T: Send + 'static,
    R: RunBlockingResult<T> + Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| IpcError::internal(format!("{what} failed unexpectedly: {e}")))?
        .into_ipc_result()
}

/// All IPC commands, registered once for both the runtime and type generation.
fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        app_info,
        list_contexts,
        query_page,
        query_resources,
        query_resources_multi,
        validate_filter_query,
        subscribe_query,
        unsubscribe_query,
        get_overview,
        get_right_sizing,
        subscribe_overview,
        unsubscribe_overview,
        get_context_settings,
        update_context_settings,
        unlock_context,
        relock_context,
        detect_terminal_emulators,
        open_external_terminal,
        open_in_browser,
        run_connection_diagnostics,
        reconnect_context,
        debug_pod,
        preview_debug_pod,
        preview_kubectl_action,
        execute_action,
        dry_run_apply,
        apply_yaml,
        start_port_forward,
        start_port_forward_target,
        list_port_forwards,
        stop_port_forward,
        list_saved_port_forwards,
        save_port_forward,
        delete_saved_port_forward,
        start_exec_session,
        start_attach_session,
        preview_kubectl_attach,
        preview_debug_node,
        start_node_debug_session,
        delete_node_debug_pod,
        write_exec_stdin,
        resize_exec_session,
        stop_exec_session,
        get_cluster_groups,
        set_cluster_group,
        delete_cluster_group,
        compare_resources,
        compare_resources_with_options,
        get_resource_detail,
        get_resource_yaml,
        get_resource_schema,
        decode_secret_values,
        encode_secret_values,
        query_apps,
        kind_counts,
        get_column_layout,
        set_column_layout,
        start_workload_logs,
        stop_workload_logs,
        set_app_foreground,
        share_text,
        open_detached_logs,
        save_log_file,
        list_helm_releases,
        get_helm_release_history,
        diff_helm_revisions,
        preview_helm_rollback,
        get_prometheus_config,
        set_prometheus_config,
        detect_prometheus,
        query_pod_usage,
        get_update_check_enabled,
        set_update_check_enabled,
        check_for_update,
        get_audit_log,
        reload_team_settings,
        list_plugins,
        import_pick_file,
        import_preview_text,
        import_commit,
        import_cancel,
        import_token,
        remove_imported_context,
        reset_imported_contexts,
        list_imported_contexts
    ])
}

/// Path of the generated bindings, relative to this crate.
pub fn bindings_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/generated/bindings.ts")
}

/// Write the TypeScript bindings for all IPC commands and types to `path`.
pub fn export_bindings(path: &Path) -> anyhow::Result<()> {
    specta_builder()
        .export(Typescript::default().header(GENERATED_HEADER), path)
        .map_err(|e| anyhow::anyhow!("exporting TypeScript bindings: {e}"))
}

/// Android/iOS entry point: the mobile shell expects `fn()`, so the error is logged here.
#[cfg(mobile)]
#[tauri::mobile_entry_point]
fn mobile_main() {
    if let Err(e) = run() {
        eprintln!("fleetglass: {e:#}");
    }
}

/// Start the app (desktop `main` and `mobile_main`).
pub fn run() -> anyhow::Result<()> {
    let builder = specta_builder();
    // Android: imported contexts live in a Keystore-sealed store. The vault needs the app's
    // plugin handle, so it is installed in `setup` below; the store only touches it on first use
    // (from blocking-pool threads, never the main thread).
    #[cfg(target_os = "android")]
    let late_vault = LateVault::default();
    #[cfg(target_os = "android")]
    let source = env_kubeconfig_source().with_imported(Arc::new(ImportedStore::new(
        imported_store_path(),
        Arc::new(late_vault.clone()),
    )));
    #[cfg(not(target_os = "android"))]
    let source = env_kubeconfig_source();
    let state = AppState::from_source(source);
    let reaper = state
        .manager
        .idle_reaper(fleetglass_core::cluster::DEFAULT_REAP_INTERVAL);
    let plugin_index = Arc::clone(&state.plugin_index);
    let app = tauri::Builder::default();
    // Plugin `setup` hooks (which manage `mobile::AndroidBridge`) run in `Builder::build` via
    // `initialize_plugins`, before the app `setup` closure (tauri 2.12.5 `app.rs`).
    #[cfg(target_os = "android")]
    let app = app.plugin(mobile::init());
    app.register_uri_scheme_protocol("fgplugin", move |_ctx, req| {
        serve_plugin_request_from(&plugin_index, &req, PLUGIN_SHARED_ORIGIN)
    })
    // Plugin hooks run for every webview (main window from tauri.conf.json and logs
    // windows). On WebKitGTK the hook sees every NavigationAction, including iframe
    // navigations (verified for plugin frames in V-01); fetch/XHR/subresources are not
    // navigations and are blocked by the plugin CSP instead. On WebView2 wry hooks only
    // `NavigationStarting` (top level), so plugin frame navigations are limited by the
    // plugin CSP and the iframe sandbox alone.
    .plugin(
        tauri::plugin::Builder::<tauri::Wry>::new("fg-navigation-guard")
            .on_navigation(|_webview, url| is_allowed_navigation(url))
            .build(),
    )
    .manage(state)
    .on_page_load(|webview, payload| {
        if webview.label() == "main"
            && payload.event() == tauri::webview::PageLoadEvent::Finished
            && MAIN_PAGE_LOADED.claim()
        {
            startup_log(&format!(
                "fleetglass startup: main page loaded after {} ms",
                ms_since_start()
            ));
        }
    })
    .setup(move |app| {
        // Evicts idle stores and aborts their watchers.
        tauri::async_runtime::spawn(reaper);
        // Only hands over the handle: setup runs on the main thread, where calling the
        // (blocking) vault would deadlock.
        #[cfg(target_os = "android")]
        {
            use tauri::Manager as _;
            let vault = app.state::<mobile::AndroidBridge<tauri::Wry>>().vault();
            late_vault.set(Box::new(vault));
        }
        #[cfg(not(target_os = "android"))]
        let _ = app;
        Ok(())
    })
    .invoke_handler(builder.invoke_handler())
    .run(tauri::generate_context!())
    .map_err(|e| anyhow::anyhow!("running Tauri application: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleetglass_core::api::{AuditEntry, ErrorKind};
    use fleetglass_core::kubeconfig::KubeconfigSource;

    #[test]
    fn sanitize_log_file_name_restricts_charset_and_extension() {
        assert_eq!(sanitize_log_file_name("web-1.log"), "web-1.log");
        assert_eq!(sanitize_log_file_name("web-1"), "web-1.log");
        assert_eq!(
            sanitize_log_file_name("../../etc/passwd"),
            "_.._etc_passwd.log"
        );
        assert_eq!(sanitize_log_file_name("..hidden.log"), "hidden.log");
        assert_eq!(sanitize_log_file_name("a b/c\\d:é"), "a_b_c_d__.log");
        assert_eq!(sanitize_log_file_name(""), "logs.log");
        assert_eq!(sanitize_log_file_name("..."), "logs.log");
        assert_eq!(sanitize_log_file_name(".log"), "log.log");
        assert_eq!(sanitize_log_file_name("..log"), "log.log");
        let long = sanitize_log_file_name(&"x".repeat(500));
        assert_eq!(long.len(), MAX_LOG_FILE_NAME_CHARS);
        assert!(long.ends_with(".log"));
        let long_log = sanitize_log_file_name(&format!("{}.log", "y".repeat(500)));
        assert_eq!(long_log.len(), MAX_LOG_FILE_NAME_CHARS);
        assert!(long_log.ends_with("y.log"));
    }

    #[test]
    fn write_new_log_file_never_overwrites() {
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-save-log-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = write_new_log_file(&dir, "web-1", b"one").unwrap();
        assert_eq!(first, dir.join("web-1.log"));
        let second = write_new_log_file(&dir, "web-1.log", b"two").unwrap();
        assert_eq!(second, dir.join("web-1 (1).log"));
        let third = write_new_log_file(&dir, "web-1", b"three").unwrap();
        assert_eq!(third, dir.join("web-1 (2).log"));
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two");
        assert_eq!(std::fs::read_to_string(&third).unwrap(), "three");

        for n in 3..=MAX_LOG_FILE_SUFFIX {
            std::fs::write(dir.join(format!("web-1 ({n}).log")), "x").unwrap();
        }
        let err = write_new_log_file(&dir, "web-1", b"four").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Internal);
        assert!(err.message.contains("already exist"), "{}", err.message);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fails when `ui/src/generated/bindings.ts` is stale. Fix with
    /// `cargo run -p fleetglass-app --bin gen-types`.
    #[test]
    fn generated_bindings_are_up_to_date() {
        let dir = std::env::temp_dir().join(format!("fleetglass-bindings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fresh_path = dir.join("bindings.ts");
        export_bindings(&fresh_path).unwrap();
        let fresh = std::fs::read_to_string(&fresh_path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        let committed = std::fs::read_to_string(bindings_path()).unwrap_or_default();
        assert!(
            fresh == committed,
            "ui/src/generated/bindings.ts is out of date; run `cargo run -p fleetglass-app --bin gen-types`"
        );
    }

    #[test]
    fn startup_once_lets_one_caller_through() {
        let once = StartupOnce::new();
        assert!(once.claim());
        assert!(!once.claim());
        assert!(!once.claim());
    }

    #[test]
    fn first_rows_line_skips_empty_pages_and_fires_once() {
        use fleetglass_core::api::{Row, RowStatus};
        let row = Row {
            uid: "u".to_owned(),
            cluster: "dev".to_owned(),
            kind: "Pod".to_owned(),
            namespace: None,
            name: "p".to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: Vec::new(),
            labels: Vec::new(),
            custom: Vec::new(),
        };
        let empty = Page {
            total: 0,
            offset: 0,
            limit: 100,
            rows: Vec::new(),
            version: 0,
            access: Vec::new(),
            status_counts: fleetglass_core::api::StatusCounts::default(),
        };
        let full = Page {
            total: 16667,
            rows: vec![row.clone(), row],
            ..empty.clone()
        };
        let once = StartupOnce::new();
        assert_eq!(first_rows_line(&once, "query_page", &empty, 5), None);
        assert_eq!(
            first_rows_line(&once, "subscribe_query", &full, 812).as_deref(),
            Some(
                "fleetglass startup: first rows returned after 812 ms \
                 (rows=2, total=16667, via=subscribe_query)"
            )
        );
        assert_eq!(first_rows_line(&once, "query_page", &full, 900), None);
    }

    #[test]
    fn first_overview_line_waits_for_a_connected_cluster_and_fires_once() {
        use fleetglass_core::api::{
            Attention, AttentionItem, ClusterSummary, MetricsAvailability, RowStatus,
        };
        let card = |cluster: &str, connection: ConnectionStatus, nodes_total: u32| ClusterSummary {
            cluster: cluster.to_owned(),
            connection,
            nodes_ready: Some(nodes_total),
            nodes_total: Some(nodes_total),
            pods_running: None,
            pods_pending: None,
            pods_failing: None,
            warning_events_1h: None,
            metrics: MetricsAvailability::Pending,
            cpu: None,
            memory: None,
            problems: Vec::new(),
        };
        let item = AttentionItem {
            cluster: "dev".to_owned(),
            kind: "Pod".to_owned(),
            namespace: "default".to_owned(),
            name: "crashloop".to_owned(),
            reason: "CrashLoopBackOff".to_owned(),
            message: String::new(),
            since: None,
            status: RowStatus::Failed,
        };
        let connecting = OverviewData {
            clusters: vec![card("dev", ConnectionStatus::Connecting, 0)],
            attention: Attention {
                items: Vec::new(),
                truncated: false,
            },
            recent_warnings: Vec::new(),
            right_sizing: None,
        };
        // Connected but nodes not listed yet: still not "first data".
        let no_nodes = OverviewData {
            clusters: vec![
                card("dev", ConnectionStatus::Connected, 0),
                card(
                    "down",
                    ConnectionStatus::Error {
                        message: "refused".to_owned(),
                    },
                    0,
                ),
            ],
            ..connecting.clone()
        };
        let ready = OverviewData {
            clusters: vec![
                card("dev", ConnectionStatus::Connected, 1),
                card(
                    "down",
                    ConnectionStatus::Error {
                        message: "refused".to_owned(),
                    },
                    0,
                ),
            ],
            attention: Attention {
                items: vec![item.clone(), item],
                truncated: false,
            },
            recent_warnings: Vec::new(),
            right_sizing: None,
        };
        let once = StartupOnce::new();
        assert_eq!(
            first_overview_line(&once, "subscribe_overview", &connecting, 5),
            None
        );
        assert_eq!(
            first_overview_line(&once, "subscribe_overview", &no_nodes, 6),
            None
        );
        assert_eq!(
            first_overview_line(&once, "subscribe_overview", &ready, 1234).as_deref(),
            Some(
                "fleetglass startup: first overview returned after 1234 ms \
                 (clusters=2, connected=1, attention=2, via=subscribe_overview)"
            )
        );
        assert_eq!(
            first_overview_line(&once, "subscribe_overview", &ready, 2000),
            None
        );

        // Nodes forbidden (unknown, card Degraded) but pods known: still "first data".
        let no_node_rbac = OverviewData {
            clusters: vec![ClusterSummary {
                nodes_ready: None,
                nodes_total: None,
                pods_running: Some(3),
                pods_pending: Some(0),
                pods_failing: Some(0),
                ..card(
                    "dev",
                    ConnectionStatus::Degraded {
                        message: "No permission to list nodes".to_owned(),
                    },
                    0,
                )
            }],
            ..connecting.clone()
        };
        // Neither nodes nor pods known: not yet.
        let nothing_known = OverviewData {
            clusters: vec![ClusterSummary {
                pods_running: None,
                ..no_node_rbac.clusters[0].clone()
            }],
            ..connecting.clone()
        };
        let once = StartupOnce::new();
        assert_eq!(
            first_overview_line(&once, "subscribe_overview", &nothing_known, 7),
            None
        );
        assert_eq!(
            first_overview_line(&once, "subscribe_overview", &no_node_rbac, 800).as_deref(),
            Some(
                "fleetglass startup: first overview returned after 800 ms \
                 (clusters=1, connected=0, attention=0, via=subscribe_overview)"
            )
        );
    }

    #[test]
    fn app_info_command_returns_core_version() {
        assert_eq!(app_info().version, fleetglass_core::VERSION);
    }

    // The command itself reads `KUBECONFIG`/`~/.kube/config`, which tests must not
    // touch, so these exercise `run_blocking` with fixture-backed sources instead.
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../core/tests/fixtures")
            .join(name)
    }

    #[test]
    fn run_blocking_returns_core_result() {
        let source = KubeconfigSource::from_paths(vec![fixture("merge-a.yaml")]);
        let list =
            tauri::async_runtime::block_on(run_blocking("test", move || source.list_contexts()))
                .unwrap();
        assert_eq!(list.current.as_deref(), Some("dev"));
        assert!(
            list.contexts
                .iter()
                .any(|c| c.name == "dev" && c.is_current)
        );
    }

    #[test]
    fn run_blocking_maps_core_errors() {
        let source = KubeconfigSource::from_paths(vec![fixture("invalid.yaml")]);
        let err =
            tauri::async_runtime::block_on(run_blocking("test", move || source.list_contexts()))
                .unwrap_err();
        assert_eq!(err.kind, ErrorKind::KubeconfigInvalid);
        assert!(err.message.contains("invalid.yaml"), "{}", err.message);
    }

    /// XORs the bytes behind a header (as the core import tests do).
    struct FakeVault;

    impl SecretVault for FakeVault {
        fn seal(&self, plain: &[u8]) -> fleetglass_core::Result<Vec<u8>> {
            let mut out = b"FAKE".to_vec();
            out.extend(plain.iter().map(|b| b ^ 0x5a));
            Ok(out)
        }

        fn open(&self, sealed: &[u8]) -> fleetglass_core::Result<Vec<u8>> {
            let body = sealed
                .strip_prefix(b"FAKE")
                .ok_or_else(|| fleetglass_core::Error::Vault("not sealed here".to_owned()))?;
            Ok(body.iter().map(|b| b ^ 0x5a).collect())
        }
    }

    struct ImportDir(PathBuf);

    impl ImportDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "fleetglass-app-import-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for ImportDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBfakefakefake\n-----END CERTIFICATE-----";

    const IMPORT_YAML: &str = "\
apiVersion: v1
kind: Config
clusters:
  - name: c1
    cluster:
      server: https://c1.example.test:6443
      certificate-authority-data: LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCg==
  - name: bare
    cluster:
      server: https://bare.example.test:6443
users:
  - name: u1
    user:
      token: abc
contexts:
  - name: phone-a
    context: {cluster: c1, user: u1}
  - name: phone-b
    context: {cluster: c1, user: u1}
  - name: no-ca
    context: {cluster: bare, user: u1}
";

    fn imported_state(dir: &ImportDir) -> (AppState, Arc<ImportedStore>) {
        let store = Arc::new(ImportedStore::new(
            dir.0.join("imported-contexts.bin"),
            Arc::new(FakeVault),
        ));
        let source = KubeconfigSource::from_paths(vec![fixture("merge-a.yaml")])
            .with_imported(Arc::clone(&store));
        (AppState::from_source(source), store)
    }

    const ANDROID_RULES: ImportOptions = ImportOptions { require_ca: true };

    fn context_names(state: &AppState) -> Vec<String> {
        state
            .manager
            .source()
            .list_contexts()
            .unwrap()
            .contexts
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    #[test]
    fn import_store_is_unavailable_without_an_imported_store() {
        let source = KubeconfigSource::from_paths(Vec::new());
        let err = IpcError::from(import_store(&source).unwrap_err());
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(err.message.contains("only available in the Android app"));
        assert!(
            !import_options().require_ca,
            "desktop keeps CA-less contexts"
        );
    }

    #[test]
    fn import_preview_then_commit_adds_contexts_to_the_source() {
        let dir = ImportDir::new("commit");
        let (state, store) = imported_state(&dir);
        let pending = Arc::clone(&state.pending_import);

        let preview =
            import_preview_in(&store, &pending, IMPORT_YAML.to_owned(), &ANDROID_RULES).unwrap();
        let names: Vec<_> = preview.contexts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["phone-a", "phone-b", "no-ca"]);
        assert!(preview.contexts[0].problem.is_none());
        assert!(preview.contexts[2].problem.is_some(), "CA-less refused");
        assert!(pending.lock().unwrap().is_some());

        // A refused context fails the commit and keeps the pending text for a retry.
        let err = import_commit_in(
            &state.manager,
            &store,
            &pending,
            &["no-ca".to_owned()],
            false,
            &ANDROID_RULES,
        )
        .unwrap_err();
        assert!(matches!(err, fleetglass_core::Error::Import(_)), "{err}");
        assert!(pending.lock().unwrap().is_some());

        let imported = import_commit_in(
            &state.manager,
            &store,
            &pending,
            &["phone-a".to_owned(), "phone-b".to_owned()],
            false,
            &ANDROID_RULES,
        )
        .unwrap();
        assert_eq!(imported, ["phone-a", "phone-b"]);
        assert!(pending.lock().unwrap().is_none(), "cleared after commit");

        let names = context_names(&state);
        assert!(names.contains(&"dev".to_owned()), "file contexts kept");
        assert!(names.contains(&"phone-a".to_owned()));
        assert!(names.contains(&"phone-b".to_owned()));

        // The next preview flags existing names.
        let preview =
            import_preview_in(&store, &pending, IMPORT_YAML.to_owned(), &ANDROID_RULES).unwrap();
        assert!(preview.contexts[0].exists);
        clear_pending(&pending);
        assert!(pending.lock().unwrap().is_none());
    }

    #[test]
    fn import_commit_without_pending_errors() {
        let dir = ImportDir::new("nopending");
        let (state, store) = imported_state(&dir);
        let err = import_commit_in(
            &state.manager,
            &store,
            &state.pending_import,
            &["phone-a".to_owned()],
            false,
            &ANDROID_RULES,
        )
        .unwrap_err();
        assert!(err.to_string().contains("pick a file first"), "{err}");
    }

    #[test]
    fn import_preview_rejects_invalid_and_oversized_text_without_keeping_it() {
        let dir = ImportDir::new("invalid");
        let (state, store) = imported_state(&dir);
        let pending = &state.pending_import;
        assert!(import_preview_in(&store, pending, "{{ nope".to_owned(), &ANDROID_RULES).is_err());
        assert!(pending.lock().unwrap().is_none());
        let big = "#".repeat(MAX_IMPORT_BYTES + 1);
        let err = import_preview_in(&store, pending, big, &ANDROID_RULES).unwrap_err();
        assert!(err.to_string().contains("KiB limit"), "{err}");
        assert!(pending.lock().unwrap().is_none());
    }

    #[test]
    fn import_token_then_remove() {
        let dir = ImportDir::new("token");
        let (state, store) = imported_state(&dir);

        // CA-less token contexts are refused under the Android rules.
        let err = import_token_in(
            &state.manager,
            &store,
            "tok",
            "https://tok.example.test:6443",
            "secret-token",
            None,
            false,
            &ANDROID_RULES,
        )
        .unwrap_err();
        assert!(matches!(err, fleetglass_core::Error::Import(_)), "{err}");
        assert!(!err.to_string().contains("secret-token"));

        let imported = import_token_in(
            &state.manager,
            &store,
            "tok",
            "https://tok.example.test:6443",
            "secret-token",
            Some(CA_PEM),
            false,
            &ANDROID_RULES,
        )
        .unwrap();
        assert_eq!(imported, ["tok"]);
        assert!(context_names(&state).contains(&"tok".to_owned()));
        assert_eq!(store.context_names().unwrap(), ["tok"]);

        assert!(remove_imported_in(&state.manager, &store, "tok").unwrap());
        assert!(!remove_imported_in(&state.manager, &store, "tok").unwrap());
        assert!(!context_names(&state).contains(&"tok".to_owned()));
    }

    #[test]
    fn reset_imported_recovers_an_unopenable_store() {
        let dir = ImportDir::new("reset");
        let (state, store) = imported_state(&dir);
        let token = |state: &AppState, store: &ImportedStore, name: &str| {
            import_token_in(
                &state.manager,
                store,
                name,
                "https://tok.example.test:6443",
                "secret-token",
                Some(CA_PEM),
                false,
                &ANDROID_RULES,
            )
        };

        // Readable store: the imported names are returned (their clients invalidated).
        token(&state, &store, "tok").unwrap();
        assert_eq!(reset_imported_in(&state.manager, &store).unwrap(), ["tok"]);
        assert!(!context_names(&state).contains(&"tok".to_owned()));
        assert!(store.context_names().unwrap().is_empty());

        // The key is gone: the file cannot be opened and a new import fails ...
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join("imported-contexts.bin"), b"sealed by a lost key").unwrap();
        let (state, store) = imported_state(&dir);
        let err = token(&state, &store, "tok2").unwrap_err();
        assert!(matches!(err, fleetglass_core::Error::Vault(_)), "{err}");

        // ... until a reset (no names known, nothing to invalidate).
        assert!(
            reset_imported_in(&state.manager, &store)
                .unwrap()
                .is_empty()
        );
        assert_eq!(token(&state, &store, "tok2").unwrap(), ["tok2"]);
        assert!(context_names(&state).contains(&"tok2".to_owned()));
        assert!(context_names(&state).contains(&"dev".to_owned()));
    }

    #[test]
    fn late_vault_fails_until_set_then_delegates() {
        let late = LateVault::default();
        let err = late.seal(b"x").unwrap_err();
        assert!(matches!(err, fleetglass_core::Error::Vault(_)));
        assert!(err.to_string().contains("not ready"));
        assert!(late.open(b"x").is_err());

        // A clone shares the slot, as the store's copy does in `run()`.
        let store_copy = late.clone();
        assert!(late.set(Box::new(FakeVault)));
        assert!(!late.set(Box::new(FakeVault)), "set only once");
        let sealed = store_copy.seal(b"hello").unwrap();
        assert_eq!(store_copy.open(&sealed).unwrap(), b"hello");
    }

    #[test]
    fn run_blocking_maps_panics_to_internal() {
        let err = tauri::async_runtime::block_on(run_blocking(
            "test",
            || -> fleetglass_core::Result<()> { panic!("boom") },
        ))
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Internal);
    }

    #[test]
    fn app_state_query_and_unsubscribe() {
        use fleetglass_core::api::{Cell, Row, RowStatus};

        let source = KubeconfigSource::from_paths(Vec::new());
        let manager = Arc::new(ClusterManager::new(source));
        let state = AppState::with_manager(Arc::clone(&manager));

        let store = manager.get_or_create_store("dev", "Pod");
        store.upsert_row(Row {
            uid: "uid-1".to_owned(),
            cluster: "dev".to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some("default".to_owned()),
            name: "api-1".to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: vec![Cell::Text("node-1".to_owned())],
            labels: Vec::new(),
            custom: Vec::new(),
        });

        let spec = QuerySpec {
            clusters: vec!["dev".to_owned()],
            kinds: vec!["Pod".to_owned()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 50,
            statuses: Vec::new(),
        };

        let page = state.manager.query_blocking(&spec);
        assert_eq!(page.total, 1);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].name, "api-1");

        tauri::async_runtime::block_on(async {
            let (sub_id, mut rx) = state.manager.subscribe(spec);
            let handle =
                tauri::async_runtime::spawn(async move { while rx.recv().await.is_some() {} });
            state.subscriptions.lock().unwrap().insert(sub_id, handle);

            assert!(state.unsubscribe(sub_id));
            assert!(!state.unsubscribe(sub_id));
        });
    }

    #[test]
    fn test_query_resources_and_multi_ipc() {
        use fleetglass_core::api::{Cell, Row, RowStatus};

        let source = KubeconfigSource::from_paths(Vec::new());
        let manager = Arc::new(ClusterManager::new(source));
        let state = AppState::with_manager(Arc::clone(&manager));

        let store_c1 = manager.get_or_create_store("c1", "Pod");
        store_c1.upsert_row(Row {
            uid: "uid-c1".to_owned(),
            cluster: "c1".to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some("default".to_owned()),
            name: "pod-c1".to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: vec![Cell::Text("node-1".to_owned())],
            labels: Vec::new(),
            custom: Vec::new(),
        });

        let store_c2 = manager.get_or_create_store("c2", "Pod");
        store_c2.upsert_row(Row {
            uid: "uid-c2".to_owned(),
            cluster: "c2".to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some("default".to_owned()),
            name: "pod-c2".to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: vec![Cell::Text("node-2".to_owned())],
            labels: Vec::new(),
            custom: Vec::new(),
        });

        // 1. Single cluster query_resources
        let page_single = state
            .manager
            .query_blocking(&QuerySpec::for_cluster("c1", "Pod"));
        assert_eq!(page_single.total, 1);
        assert_eq!(page_single.rows[0].cluster, "c1");

        // 2. Multi-cluster query_resources_multi
        tauri::async_runtime::block_on(async {
            let page_multi = state
                .manager
                .query(&QuerySpec::for_clusters(
                    vec!["c1".to_owned(), "c2".to_owned()],
                    "Pod",
                ))
                .await;
            assert_eq!(page_multi.total, 2);
            assert_eq!(page_multi.rows.len(), 2);
            assert_eq!(page_multi.access.len(), 2);
        });
    }

    #[test]
    fn phase1_commands_and_state() {
        let cmd = preview_kubectl_action(
            "dev".to_owned(),
            MutatingAction::Scale {
                kind: "Deployment".to_owned(),
                namespace: "default".to_owned(),
                name: "web".to_owned(),
                replicas: 3,
            },
        );
        assert_eq!(
            cmd,
            "kubectl --context dev -n default scale deployment/web --replicas=3"
        );

        let source = KubeconfigSource::from_paths(Vec::new());
        let state = AppState::with_manager(Arc::new(ClusterManager::new(source)));
        assert_eq!(
            state.settings.get("unknown-ctx"),
            ContextSettings::default()
        );

        tauri::async_runtime::block_on(async {
            let pf = state
                .port_forwards
                .start_local(
                    "dev".to_owned(),
                    "default".to_owned(),
                    "web-0".to_owned(),
                    8080,
                    0,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(state.port_forwards.list().len(), 1);
            assert!(state.port_forwards.stop(pf.id));
            assert!(state.port_forwards.list().is_empty());
        });
    }

    #[test]
    fn phase2_commands_and_state() {
        use fleetglass_core::api::{Row, RowStatus};

        // Stateless commands are called directly.
        let diff = tauri::async_runtime::block_on(compare_resources(
            "kind: ConfigMap\ndata:\n  k: v1\n".to_owned(),
            "kind: ConfigMap\ndata:\n  k: v2\n".to_owned(),
        ))
        .unwrap();
        assert!(!diff.identical);
        assert!(diff.unified_diff.contains("+  k: v2"));

        let plain = BTreeMap::from([("user".to_owned(), "admin".to_owned())]);
        let encoded = tauri::async_runtime::block_on(encode_secret_values(plain.clone())).unwrap();
        assert_eq!(encoded["user"], "YWRtaW4=");
        let decoded = tauri::async_runtime::block_on(decode_secret_values(encoded)).unwrap();
        assert_eq!(decoded, plain);

        let bad = BTreeMap::from([("pw".to_owned(), "%%%".to_owned())]);
        let err = tauri::async_runtime::block_on(decode_secret_values(bad)).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        assert!(err.message.contains("pw") && !err.message.contains("%%%"));

        // Stateful commands: exercise the same state calls with a temp settings file
        // (never the user's real config dir).
        let dir = std::env::temp_dir().join(format!("fleetglass-app-p2-{}", std::process::id()));
        let manager = Arc::new(ClusterManager::new(
            KubeconfigSource::from_paths(Vec::new()),
        ));
        let state = AppState::with_parts(
            Arc::clone(&manager),
            Arc::new(SettingsStore::new(dir.join("context-settings.json"))),
        );

        let group = ClusterGroup {
            name: "prod".to_owned(),
            contexts: vec!["prod-eu".to_owned()],
        };
        let store = Arc::clone(&state.settings);
        tauri::async_runtime::block_on(run_blocking("test", {
            let group = group.clone();
            move || store.set_cluster_group(group)
        }))
        .unwrap();
        assert_eq!(state.settings.get_cluster_groups(), vec![group]);
        let store = Arc::clone(&state.settings);
        tauri::async_runtime::block_on(run_blocking("test", move || {
            store.delete_cluster_group("prod")
        }))
        .unwrap();
        assert!(state.settings.get_cluster_groups().is_empty());

        // Column layouts (P2-08): same calls as get/set_column_layout.
        let layout = |json_path: &str| ColumnLayout {
            kind: "Pod".to_owned(),
            visible: vec!["name".to_owned(), "ingress-class".to_owned()],
            custom: vec![fleetglass_core::api::ColumnDef {
                id: "ingress-class".to_owned(),
                title: "Class".to_owned(),
                json_path: json_path.to_owned(),
                width: Some(120),
            }],
            widths: vec![("name".to_owned(), 240)],
        };
        assert_eq!(state.settings.get_column_layout("Pod"), None);
        let good = layout(".metadata.annotations['kubernetes.io/ingress.class']");
        let store = Arc::clone(&state.settings);
        tauri::async_runtime::block_on(run_blocking("test", {
            let good = good.clone();
            move || store.set_column_layout(good)
        }))
        .unwrap();
        assert_eq!(state.settings.get_column_layout("Pod"), Some(good.clone()));
        let store = Arc::clone(&state.settings);
        let err = tauri::async_runtime::block_on(run_blocking("set_column_layout", move || {
            store.set_column_layout(layout(".metadata.annotations['unclosed"))
        }))
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);
        // The rejected layout did not replace the saved one.
        assert_eq!(state.settings.get_column_layout("Pod"), Some(good));
        let _ = std::fs::remove_dir_all(&dir);

        manager.get_or_create_store("dev", "Pod").upsert_row(Row {
            uid: "uid-1".to_owned(),
            cluster: "dev".to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some("default".to_owned()),
            name: "api-1".to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: Vec::new(),
            labels: vec![("app".to_owned(), "api".to_owned())],
            custom: Vec::new(),
        });
        let spec = AppQuerySpec {
            clusters: Vec::new(),
            namespaces: Vec::new(),
            text_filter: Some("API".to_owned()),
            offset: 0,
            limit: 10,
        };
        let page = state.manager.query_apps(&spec);
        assert_eq!(page.total, 1);
        assert_eq!(page.apps[0].name, "api");
        assert_eq!(page.access.len(), 1);
        assert_eq!(page.access[0].kind, "Pod");
    }

    /// Stand-in for a follower + forwarder pair: tasks that never finish on their own.
    fn dummy_follower() -> (TokioJoinHandle<()>, JoinHandle<()>) {
        // Core spawns the follower with `tokio::spawn`, so use a raw tokio handle.
        let follower = tauri::async_runtime::handle()
            .inner()
            .spawn(std::future::pending::<()>());
        let forwarder = tauri::async_runtime::spawn(std::future::pending::<()>());
        (follower, forwarder)
    }

    #[test]
    fn workload_log_followers_stop_and_abort() {
        let state = AppState::with_manager(Arc::new(ClusterManager::new(
            KubeconfigSource::from_paths(Vec::new()),
        )));
        let (follower, forwarder) = dummy_follower();
        let follower_abort = follower.abort_handle();
        let forwarder_abort = forwarder.inner().abort_handle();
        let a = state.insert_log_follower(Some(follower), forwarder, None);
        let (f2, w2) = dummy_follower();
        let b = state.insert_log_follower(Some(f2), w2, None);
        assert_ne!(a, 0);
        assert_ne!(a, b);
        assert_eq!(state.log_followers.lock().unwrap().len(), 2);

        assert!(state.stop_log_follower(a));
        assert!(!state.stop_log_follower(a));
        assert_eq!(state.log_followers.lock().unwrap().len(), 1);
        wait_until(|| follower_abort.is_finished() && forwarder_abort.is_finished());

        // A finished forwarder (window closed) is pruned, and its follower
        // aborted, on the next insert.
        let (f3, w3) = dummy_follower();
        let f3_abort = f3.abort_handle();
        let w3_abort = w3.inner().abort_handle();
        w3.abort();
        let c = state.insert_log_follower(Some(f3), w3, None);
        wait_until(|| w3_abort.is_finished());
        let (f4, w4) = dummy_follower();
        let d = state.insert_log_follower(Some(f4), w4, None);
        {
            let map = state.log_followers.lock().unwrap();
            assert!(!map.contains_key(&c) && map.contains_key(&b) && map.contains_key(&d));
        }
        wait_until(|| f3_abort.is_finished());

        // Dropping AppState aborts what is left.
        let remaining: Vec<_> = state
            .log_followers
            .lock()
            .unwrap()
            .values()
            .map(|f| {
                (
                    f.follower.as_ref().unwrap().abort_handle(),
                    f.forwarder.inner().abort_handle(),
                )
            })
            .collect();
        assert_eq!(remaining.len(), 2);
        drop(state);
        wait_until(|| {
            remaining
                .iter()
                .all(|(f, w)| f.is_finished() && w.is_finished())
        });
    }

    #[test]
    fn log_followers_pause_for_the_background_and_resume_idempotently() {
        tauri::async_runtime::block_on(async {
            let state = AppState::with_manager(Arc::new(ClusterManager::new(
                KubeconfigSource::from_paths(Vec::new()),
            )));
            let (follower, forwarder) = dummy_follower();
            let follower_abort = follower.abort_handle();
            let forwarder_abort = forwarder.inner().abort_handle();
            let (tx, mut rx) = tauri::async_runtime::channel(8);
            let target = WorkloadLogTarget {
                cluster: "missing".to_owned(),
                namespace: "default".to_owned(),
                selector: WorkloadSelector::Pod {
                    name: "p".to_owned(),
                },
                container: None,
                tail_lines: None,
                since_seconds: None,
                previous: false,
            };
            let id = state.insert_log_follower(
                Some(follower),
                forwarder,
                Some(LogResume {
                    target,
                    sink: tx,
                    next_seq: Arc::new(AtomicU32::new(7)),
                    paused_at: None,
                }),
            );

            state.pause_log_followers();
            state.pause_log_followers();
            wait_until(|| follower_abort.is_finished());
            {
                let map = state.log_followers.lock().unwrap();
                let f = map.get(&id).expect("stream kept while paused");
                assert!(f.follower.is_none());
                assert!(f.resume.as_ref().unwrap().paused_at.is_some());
            }
            assert!(!forwarder_abort.is_finished(), "forwarder (channel) kept");

            // The cluster is unknown: resuming reports it on the stream once.
            state.resume_log_followers().await;
            state.resume_log_followers().await;
            match rx.try_recv() {
                Ok(MuxLogEvent::Error { message }) => {
                    assert!(message.starts_with("cannot resume logs"), "{message}");
                }
                other => panic!("expected an error event, got {other:?}"),
            }
            assert!(rx.try_recv().is_err(), "resume is idempotent");
            {
                let map = state.log_followers.lock().unwrap();
                assert!(
                    map.get(&id)
                        .unwrap()
                        .resume
                        .as_ref()
                        .unwrap()
                        .paused_at
                        .is_none()
                );
            }
            assert!(state.stop_log_follower(id));
            wait_until(|| forwarder_abort.is_finished());
        });
    }

    #[test]
    fn exec_sessions_lifecycle_and_stdin() {
        tauri::async_runtime::block_on(async {
            let state = AppState::with_manager(Arc::new(ClusterManager::new(
                KubeconfigSource::from_paths(Vec::new()),
            )));

            let (tx, mut rx) = tauri::async_runtime::channel::<Vec<u8>>(10);
            let (resize_tx, mut resize_rx) = tokio::sync::watch::channel(None);
            let dummy_task = tauri::async_runtime::handle()
                .inner()
                .spawn(async move { while rx.recv().await.is_some() {} });
            let session = ExecSession::new(tx, resize_tx, dummy_task);
            let forwarder = tauri::async_runtime::spawn(std::future::pending::<()>());

            let id = state.insert_exec_session(session, forwarder);
            assert_ne!(id, 0);

            // Stdin writing succeeds
            assert!(
                state
                    .write_exec_stdin(id, "echo hello\n".to_owned())
                    .await
                    .is_ok()
            );

            // Dynamic PTY resize succeeds; bursts coalesce to the latest size (05-02)
            for cols in 100..=119 {
                state.resize_exec_session(id, cols, 40).unwrap();
            }
            state.resize_exec_session(id, 120, 40).unwrap();
            assert!(resize_rx.has_changed().unwrap());
            assert_eq!(
                *resize_rx.borrow_and_update(),
                Some(fleetglass_core::exec::TerminalSize::new(120, 40).unwrap())
            );

            // Zero and absurd dimensions are rejected at the IPC boundary
            for (cols, rows) in [(0, 40), (120, 0), (1001, 40), (120, 5000)] {
                let err = state.resize_exec_session(id, cols, rows).unwrap_err();
                assert_eq!(err.kind, ErrorKind::InvalidInput, "{cols}x{rows}");
            }
            assert!(!resize_rx.has_changed().unwrap());

            // Nonexistent session returns InvalidInput
            let err = state.resize_exec_session(9999, 80, 24).unwrap_err();
            assert_eq!(err.kind, ErrorKind::InvalidInput);

            // Nonexistent session returns InvalidInput
            let err = state
                .write_exec_stdin(9999, "hi".to_owned())
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::InvalidInput);

            // Stopping session
            assert!(state.stop_exec_session(id));
            assert!(!state.stop_exec_session(id));
        });
    }

    /// Waits up to ~2 s for `done`; aborted tasks finish on the runtime's worker threads.
    fn wait_until(done: impl Fn() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("tasks were not aborted in time");
    }

    #[test]
    fn detached_logs_url_and_title() {
        assert_eq!(url_encode("aZ09-_.~"), "aZ09-_.~");
        assert_eq!(
            url_encode("{\"a\":\"b c/é\"}"),
            "%7B%22a%22%3A%22b%20c%2F%C3%A9%22%7D"
        );

        let target = WorkloadLogTarget {
            cluster: "dev".to_owned(),
            namespace: "default".to_owned(),
            selector: WorkloadSelector::Owner {
                kind: "Deployment".to_owned(),
                name: "web".to_owned(),
            },
            container: None,
            tail_lines: Some(100),
            since_seconds: Some(300),
            previous: false,
        };
        assert_eq!(
            detached_logs_title(&target),
            "Logs: dev/default/Deployment/web"
        );
        let labels = WorkloadLogTarget {
            selector: WorkloadSelector::Labels {
                selector: "app=web,tier!=cache".to_owned(),
            },
            ..target.clone()
        };
        assert_eq!(
            detached_logs_title(&labels),
            "Logs: dev/default/app=web,tier!=cache"
        );
        let pod_target = WorkloadLogTarget {
            selector: WorkloadSelector::Pod {
                name: "counter".to_owned(),
            },
            ..target.clone()
        };
        assert_eq!(
            detached_logs_title(&pod_target),
            "Logs: dev/default/pod/counter"
        );

        let json = serde_json::to_string(&target).unwrap();
        let url = detached_logs_url(&json);
        let encoded = url.strip_prefix("index.html#/logs?target=").unwrap();
        assert!(!encoded.contains(['{', '"', '#', '&', '?', ' ']));
        // Decoding the query value gives back the exact target.
        let bytes: Vec<u8> = {
            let raw = encoded.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < raw.len() {
                if raw[i] == b'%' {
                    let hex = std::str::from_utf8(&raw[i + 1..i + 3]).unwrap();
                    out.push(u8::from_str_radix(hex, 16).unwrap());
                    i += 3;
                } else {
                    out.push(raw[i]);
                    i += 1;
                }
            }
            out
        };
        let decoded: WorkloadLogTarget = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn get_audit_log_queries_through_app_state() {
        let dir = std::env::temp_dir().join(format!("fleetglass-app-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
        let source = KubeconfigSource::from_paths(Vec::new());
        let manager = Arc::new(ClusterManager::new(source));
        let settings = Arc::new(SettingsStore::new(dir.join("context-settings.json")));
        let state = AppState::with_parts_and_audit(manager, settings, Arc::clone(&audit));

        let entry1 = AuditEntry {
            id: 1,
            timestamp: "2026-04-16T10:00:00Z".to_owned(),
            cluster: "prod".to_owned(),
            user: Some("alice".to_owned()),
            action: "delete".to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some("default".to_owned()),
            name: "web-1".to_owned(),
            kubectl: "kubectl --context prod -n default delete pod web-1".to_owned(),
            success: true,
            error: None,
        };
        let entry2 = AuditEntry {
            id: 2,
            timestamp: "2026-04-16T10:05:00Z".to_owned(),
            cluster: "dev".to_owned(),
            user: Some("bob".to_owned()),
            action: "scale".to_owned(),
            kind: "Deployment".to_owned(),
            namespace: Some("staging".to_owned()),
            name: "api".to_owned(),
            kubectl: "kubectl --context dev -n staging scale deployment/api --replicas=3"
                .to_owned(),
            success: true,
            error: None,
        };

        state.audit.append(&entry1).unwrap();
        state.audit.append(&entry2).unwrap();

        // Query through AppState using run_blocking as get_audit_log does
        let store = Arc::clone(&state.audit);
        let page = tauri::async_runtime::block_on(run_blocking("get_audit_log", move || {
            store.query(0, 10, None, None)
        }))
        .unwrap();

        assert_eq!(page.total, 2);
        assert_eq!(page.offset, 0);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0], entry2);
        assert_eq!(page.entries[1], entry1);

        // Filter by cluster
        let store = Arc::clone(&state.audit);
        let prod_page = tauri::async_runtime::block_on(run_blocking("get_audit_log", move || {
            store.query(0, 10, Some("prod"), None)
        }))
        .unwrap();
        assert_eq!(prod_page.total, 1);
        assert_eq!(prod_page.entries.len(), 1);
        assert_eq!(prod_page.entries[0], entry1);

        // Filter by action
        let store = Arc::clone(&state.audit);
        let scale_page = tauri::async_runtime::block_on(run_blocking("get_audit_log", move || {
            store.query(0, 10, None, Some("scale"))
        }))
        .unwrap();
        assert_eq!(scale_page.total, 1);
        assert_eq!(scale_page.entries[0], entry2);

        // Windowed pagination
        let store = Arc::clone(&state.audit);
        let paged = tauri::async_runtime::block_on(run_blocking("get_audit_log", move || {
            store.query(1, 1, None, None)
        }))
        .unwrap();
        assert_eq!(paged.total, 2);
        assert_eq!(paged.offset, 1);
        assert_eq!(paged.entries.len(), 1);
        assert_eq!(paged.entries[0], entry1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_team_settings_merges_team_yaml_through_app_state() {
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-app-team-settings-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let source = KubeconfigSource::from_paths(Vec::new());
        let manager = Arc::new(ClusterManager::new(source));
        let settings = Arc::new(SettingsStore::new(dir.join("context-settings.json")));
        let state = AppState::with_parts(manager, settings);

        let yaml = r##"
version: 1
groups:
  - name: staging
    contexts: [staging-eu, staging-us]
contexts:
  prod-eu:
    readOnly: true
    protected: true
    colour: "#e11d48"
    tags: [prod, europe]
"##;
        let yaml_path = dir.join("team-settings.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();

        let path_str = yaml_path.to_str().unwrap().to_owned();
        let store = Arc::clone(&state.settings);
        let summary =
            tauri::async_runtime::block_on(run_blocking("reload_team_settings", move || {
                fleetglass_core::team_settings::merge_team_settings(&store, Path::new(&path_str))
            }))
            .unwrap();

        assert_eq!(summary.applied_groups, 1);
        assert_eq!(summary.applied_contexts, 1);
        assert!(summary.warnings.is_empty());
        assert_eq!(summary.loaded_path, yaml_path.display().to_string());

        let groups = state.settings.get_cluster_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "staging");
        assert_eq!(groups[0].contexts, vec!["staging-eu", "staging-us"]);

        let ctx = state.settings.get("prod-eu");
        assert!(ctx.read_only);
        assert!(ctx.protected);
        assert_eq!(ctx.colour.as_deref(), Some("#e11d48"));
        assert_eq!(ctx.tags, vec!["prod", "europe"]);

        // Verify invalid YAML error mapping
        let bad_path = dir.join("invalid.yaml");
        std::fs::write(&bad_path, "invalid: [unclosed").unwrap();
        let bad_path_str = bad_path.to_str().unwrap().to_owned();
        let store = Arc::clone(&state.settings);
        let err = tauri::async_runtime::block_on(run_blocking("reload_team_settings", move || {
            fleetglass_core::team_settings::merge_team_settings(&store, Path::new(&bad_path_str))
        }))
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidInput);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_plugins_works_through_app_state() {
        let dir =
            std::env::temp_dir().join(format!("fleetglass-app-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let source = KubeconfigSource::from_paths(Vec::new());
        let manager = Arc::new(ClusterManager::new(source));
        let settings = Arc::new(SettingsStore::new(dir.join("context-settings.json")));
        let state = AppState::with_parts(manager, settings);
        let _ = &state;

        let plugin_dir = dir.join("plugins");
        let plugin_sub = plugin_dir.join("demo-plugin");
        std::fs::create_dir_all(&plugin_sub).unwrap();
        let manifest_json = r#"{
            "id": "demo-plugin",
            "name": "Demo Plugin",
            "version": "1.0.0",
            "description": "A demo plugin",
            "permissions": ["read:resources"],
            "entrypoint": "dist/index.js"
        }"#;
        std::fs::write(plugin_sub.join("manifest.json"), manifest_json).unwrap();

        let plugins = tauri::async_runtime::block_on(run_blocking("list_plugins", move || {
            fleetglass_core::plugins::discover_plugins(&plugin_dir)
        }))
        .unwrap();

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, "demo-plugin");
        assert_eq!(plugins[0].name, "Demo Plugin");
        assert_eq!(plugins[0].version, "1.0.0");
        assert_eq!(plugins[0].description, "A demo plugin");
        assert_eq!(plugins[0].permissions, vec!["read:resources".to_owned()]);
        assert_eq!(plugins[0].entrypoint, "dist/index.js");

        // Verify error mapping for non-existent directory
        let nonexistent = dir.join("nonexistent");
        let err = tauri::async_runtime::block_on(run_blocking("list_plugins", move || {
            fleetglass_core::plugins::discover_plugins(&nonexistent)
        }))
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Internal);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn plugin_get_with(
        dirs: &[PathBuf],
        uri: &str,
        shared_origin: bool,
    ) -> tauri::http::Response<Vec<u8>> {
        let req = tauri::http::Request::builder()
            .uri(uri)
            .body(Vec::new())
            .unwrap();
        serve_plugin_request_from(&PluginIndex::new(dirs.to_vec()), &req, shared_origin)
    }

    /// Per-plugin origin (`fgplugin://<id>/…`, Linux/macOS).
    fn plugin_get(dirs: &[PathBuf], uri: &str) -> tauri::http::Response<Vec<u8>> {
        plugin_get_with(dirs, uri, false)
    }

    fn repo_plugins_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
    }

    fn header<'a>(resp: &'a tauri::http::Response<Vec<u8>>, name: &str) -> &'a str {
        resp.headers().get(name).unwrap().to_str().unwrap()
    }

    #[test]
    fn plugin_dirs_are_user_dir_plus_repo_dir_only_in_debug() {
        let dirs = plugin_dirs();
        assert_eq!(
            dirs[0],
            SettingsStore::default_path().with_file_name("plugins")
        );
        if cfg!(debug_assertions) {
            assert_eq!(dirs.len(), 2);
            assert_eq!(dirs[1], repo_plugins_dir());
        } else {
            assert_eq!(dirs.len(), 1);
        }
        // Never derived from the working directory.
        if let Ok(cwd) = std::env::current_dir() {
            assert!(!dirs.contains(&cwd.join("plugins")));
        }
    }

    #[test]
    fn serve_plugin_html_has_strict_csp_blocking_external_requests() {
        let resp = plugin_get(
            &[repo_plugins_dir()],
            "fgplugin://sample-pod-counter/index.html",
        );
        assert_eq!(resp.status(), 200);
        assert_eq!(header(&resp, "Content-Type"), "text/html; charset=utf-8");
        assert_eq!(header(&resp, "X-Content-Type-Options"), "nosniff");
        let csp = header(&resp, "Content-Security-Policy");
        let directive = |name: &str| -> Vec<&str> {
            csp.split(';')
                .map(str::trim)
                .find(|d| d.split_whitespace().next() == Some(name))
                .map(|d| d.split_whitespace().skip(1).collect())
                .unwrap_or_else(|| panic!("{name} missing in {csp}"))
        };
        // fetch/XHR/WebSocket/EventSource, form posts, <base> rewrites and everything not
        // listed (objects, frames, media, fonts) are blocked.
        for name in ["default-src", "connect-src", "form-action", "base-uri"] {
            assert_eq!(directive(name), ["'none'"], "{name} in {csp}");
        }
        assert_eq!(
            directive("script-src"),
            ["'self'", "fgplugin://sample-pod-counter"]
        );
        let ancestors = directive("frame-ancestors");
        assert!(ancestors.contains(&"tauri://localhost"));
        assert!(!ancestors.contains(&"*") && !ancestors.contains(&"'self'"));
        // Only the app's own origins appear; no remote hosts or wildcards.
        for source in csp.split(';').flat_map(|d| d.split_whitespace().skip(1)) {
            let ok = source.starts_with('\'')
                || source == "data:"
                || source == "fgplugin://sample-pod-counter"
                || source.ends_with("tauri.localhost")
                || source == "tauri://localhost"
                || (cfg!(debug_assertions) && source == "http://localhost:1420");
            assert!(ok, "unexpected CSP source {source} in {csp}");
        }
        assert!(!csp.contains("unsafe-eval"));
        // `navigate-to` was dropped from CSP3 and is not implemented by any engine:
        // navigation is enforced by `is_allowed_navigation` instead.
        assert!(!csp.contains("navigate-to"));
        let body = std::str::from_utf8(resp.body()).unwrap();
        assert!(body.contains("Pod Counter"));

        // The root path serves the entrypoint.
        let root = plugin_get(&[repo_plugins_dir()], "fgplugin://sample-pod-counter/");
        assert_eq!(root.status(), 200);
    }

    #[test]
    fn serve_plugin_rejects_traversal_unknown_ids_and_symlink_escapes() {
        let dirs = [repo_plugins_dir()];
        for uri in [
            "fgplugin://sample-pod-counter/../../Cargo.toml",
            "fgplugin://sample-pod-counter/%2e%2e/%2e%2e/Cargo.toml",
            "fgplugin://sample-pod-counter/manifest.json/../../../Cargo.toml",
            "fgplugin://Sample-Pod-Counter/index.html",
            "fgplugin://no-such-plugin/index.html",
            "fgplugin://sample-pod-counter/missing.html",
            "fgplugin://sample-pod-counter//index.html",
            "fgplugin://sample-pod-counter//etc/passwd",
            "fgplugin://sample-pod-counter/C:/Windows/win.ini",
            "fgplugin://sample-pod-counter/index.html:stream",
            "fgplugin://localhost/sample-pod-counter/index.html",
        ] {
            let resp = plugin_get(&dirs, uri);
            assert_ne!(resp.status(), 200, "{uri}");
            // Error responses carry the same CSP.
            assert!(header(&resp, "Content-Security-Policy").contains("default-src 'none'"));
        }

        let tmp = std::env::temp_dir().join(format!(
            "fleetglass-app-plugin-serve-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("plugins");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        // A directory without a valid manifest is not served.
        std::fs::create_dir_all(root.join("not-a-plugin")).unwrap();
        std::fs::write(root.join("not-a-plugin/index.html"), "x").unwrap();
        let plugin = root.join("good");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("manifest.json"),
            r#"{"id":"good-plugin","name":"G","version":"1.0.0","description":"d","permissions":[],"entrypoint":"index.html"}"#,
        )
        .unwrap();
        std::fs::write(plugin.join("index.html"), "<h1>good</h1>").unwrap();
        let dirs = [root.clone()];
        assert_eq!(
            plugin_get(&dirs, "fgplugin://good-plugin/index.html").status(),
            200
        );
        assert_eq!(
            plugin_get(&dirs, "fgplugin://not-a-plugin/index.html").status(),
            404
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("secret.txt"), plugin.join("leak.txt"))
                .unwrap();
            assert_eq!(
                plugin_get(&dirs, "fgplugin://good-plugin/leak.txt").status(),
                404
            );
            // A symlinked plugin root is not discovered, hence not served.
            let linked = tmp.join("linked-src");
            std::fs::create_dir_all(&linked).unwrap();
            std::fs::write(
                linked.join("manifest.json"),
                r#"{"id":"linked-plugin","name":"L","version":"1.0.0","description":"d","permissions":[],"entrypoint":"index.html"}"#,
            )
            .unwrap();
            std::fs::write(linked.join("index.html"), "x").unwrap();
            std::os::unix::fs::symlink(&linked, root.join("linked")).unwrap();
            assert_eq!(
                plugin_get(&dirs, "fgplugin://linked-plugin/index.html").status(),
                404
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn serve_plugin_parses_both_url_shapes() {
        let dirs = [repo_plugins_dir()];
        // Per-plugin origin: fgplugin://<id>/<path>.
        let resp = plugin_get_with(&dirs, "fgplugin://sample-pod-counter/main.js", false);
        assert_eq!(resp.status(), 200);
        assert_eq!(
            header(&resp, "Content-Type"),
            "text/javascript; charset=utf-8"
        );
        // Windows/Android: wry turns http://fgplugin.localhost/<id>/<path> into
        // fgplugin://localhost/<id>/<path> before calling the handler.
        for uri in [
            "fgplugin://localhost/sample-pod-counter/index.html",
            "fgplugin://localhost/sample-pod-counter/",
            "fgplugin://localhost/sample-pod-counter",
        ] {
            let resp = plugin_get_with(&dirs, uri, true);
            assert_eq!(resp.status(), 200, "{uri}");
            assert_eq!(header(&resp, "Content-Type"), "text/html; charset=utf-8");
            let csp = header(&resp, "Content-Security-Policy");
            // Path-scoped to the plugin's own directory on the shared origin.
            assert!(
                csp.contains(
                    "script-src 'self' http://fgplugin.localhost/sample-pod-counter/ \
https://fgplugin.localhost/sample-pod-counter/;"
                ),
                "{csp}"
            );
            assert!(!csp.contains("fgplugin://"), "{csp}");
        }
        for uri in [
            "fgplugin://localhost/",
            "fgplugin://localhost//sample-pod-counter/index.html",
            "fgplugin://localhost/sample-pod-counter//index.html",
            "fgplugin://localhost/sample-pod-counter/../sample-pod-counter/index.html",
            "fgplugin://localhost/Sample-Pod-Counter/index.html",
            "fgplugin://localhost/sample-pod-counter/C:/x",
            // The per-plugin shape is not accepted where the origin is shared.
            "fgplugin://sample-pod-counter/index.html",
        ] {
            let resp = plugin_get_with(&dirs, uri, true);
            assert_ne!(resp.status(), 200, "{uri}");
            assert_eq!(header(&resp, "X-Content-Type-Options"), "nosniff");
            assert!(header(&resp, "Content-Security-Policy").contains("default-src 'none'"));
        }
    }

    #[test]
    fn plugin_response_is_infallible_and_always_has_csp() {
        let resp = plugin_response(1000, "text/plain", "", Vec::new());
        assert_eq!(resp.status(), 500);
        assert!(header(&resp, "Content-Security-Policy").contains("default-src 'none'"));
        assert_eq!(header(&resp, "X-Content-Type-Options"), "nosniff");
        // An own-sources value that is not a valid header value never yields an
        // unprotected 200.
        let resp = plugin_response(200, "text/html", " \n", b"<h1>x</h1>".to_vec());
        assert_eq!(resp.status(), 500);
        assert!(resp.body().is_empty());
        assert_eq!(
            header(&resp, "Content-Security-Policy"),
            "default-src 'none'"
        );
    }

    #[test]
    fn plugin_index_caches_lookups_and_revalidates_hits() {
        let tmp = std::env::temp_dir().join(format!(
            "fleetglass-app-plugin-index-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugin = tmp.join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        let manifest = |id: &str| {
            format!(
                r#"{{"id":"{id}","name":"N","version":"1.0.0","description":"d","permissions":[],"entrypoint":"index.html"}}"#
            )
        };
        std::fs::write(plugin.join("manifest.json"), manifest("cached-plugin")).unwrap();
        std::fs::write(plugin.join("index.html"), "x").unwrap();
        let index = PluginIndex::new(vec![tmp.clone()]);
        let get = |uri: &str| {
            let req = tauri::http::Request::builder()
                .uri(uri)
                .body(Vec::new())
                .unwrap();
            serve_plugin_request_from(&index, &req, false).status()
        };
        assert_eq!(get("fgplugin://cached-plugin/index.html"), 200);
        assert_eq!(
            index.cache.lock().unwrap().get("cached-plugin"),
            Some(&plugin)
        );
        // A cached plugin whose manifest changed id (or became invalid) is not served.
        std::fs::write(plugin.join("manifest.json"), manifest("renamed-plugin")).unwrap();
        assert_eq!(get("fgplugin://cached-plugin/index.html"), 404);
        assert_eq!(get("fgplugin://renamed-plugin/index.html"), 200);
        std::fs::write(plugin.join("manifest.json"), manifest("BAD")).unwrap();
        assert_eq!(get("fgplugin://renamed-plugin/index.html"), 404);
        assert!(index.refresh().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn plugin_index_rate_limits_rescans_for_unknown_ids() {
        let tmp = std::env::temp_dir().join(format!(
            "fleetglass-app-plugin-negcache-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let install = |id: &str| {
            let d = tmp.join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("manifest.json"),
                format!(
                    r#"{{"id":"{id}","name":"N","version":"1.0.0","description":"d","permissions":[],"entrypoint":"index.html"}}"#
                ),
            )
            .unwrap();
            std::fs::write(d.join("index.html"), "x").unwrap();
            d
        };
        let index = PluginIndex::with_miss_rescan_interval(
            vec![tmp.clone()],
            std::time::Duration::from_secs(3600),
        );
        let scanned_at = || *index.last_scan.lock().unwrap();

        // First miss scans (nothing scanned yet).
        assert_eq!(index.plugin_dir("random-id"), None);
        let first = scanned_at().expect("first miss scans");
        // Further misses within the interval do not rescan, even for an id installed since.
        let late = install("late-plugin");
        assert_eq!(index.plugin_dir("other-random-id"), None);
        assert_eq!(index.plugin_dir("late-plugin"), None);
        assert_eq!(scanned_at(), Some(first));
        // An explicit refresh (list_plugins) always rescans and picks it up.
        assert_eq!(index.refresh().unwrap().len(), 1);
        assert!(scanned_at().unwrap() > first);
        assert_eq!(index.plugin_dir("late-plugin"), Some(late));

        // Once the interval has passed, a miss rescans again.
        let index =
            PluginIndex::with_miss_rescan_interval(vec![tmp.clone()], std::time::Duration::ZERO);
        assert_eq!(index.plugin_dir("random-id"), None);
        let first = index.last_scan.lock().unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let other = install("other-plugin");
        assert_eq!(index.plugin_dir("other-plugin"), Some(other));
        assert!(index.last_scan.lock().unwrap().unwrap() > first);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_all_plugins_dedupes_ids_and_skips_missing_dirs() {
        let tmp = std::env::temp_dir().join(format!(
            "fleetglass-app-plugin-dedupe-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        for (dir, name) in [("a/p", "First"), ("b/p", "Second")] {
            let d = tmp.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("manifest.json"),
                format!(
                    r#"{{"id":"same-plugin","name":"{name}","version":"1.0.0","description":"d","permissions":[],"entrypoint":"index.html"}}"#
                ),
            )
            .unwrap();
        }
        let found =
            discover_all_plugins(&[tmp.join("missing"), tmp.join("a"), tmp.join("b")]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.name, "First");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn navigation_allows_only_app_origins_and_plugins() {
        let allowed = [
            "tauri://localhost/",
            "tauri://localhost/index.html#/logs?target=x",
            "fgplugin://sample-pod-counter/index.html",
            "about:blank",
            "about:srcdoc",
        ];
        for u in allowed {
            assert!(is_allowed_navigation(&tauri::Url::parse(u).unwrap()), "{u}");
        }
        // wry workaround origins: allowed only on the platforms that use them.
        for u in [
            "http://tauri.localhost/",
            "https://tauri.localhost/index.html",
            "http://fgplugin.localhost/sample-pod-counter/index.html",
            "https://fgplugin.localhost/sample-pod-counter/index.html",
        ] {
            assert_eq!(
                is_allowed_navigation(&tauri::Url::parse(u).unwrap()),
                PLUGIN_SHARED_ORIGIN,
                "{u}"
            );
        }
        let denied = [
            "fgplugin:x",
            "fgplugin:///index.html",
            "fgplugin://Evil-Plugin/index.html",
            "fgplugin://evil.example.com/index.html",
            "http://fgplugin.localhost:8080/x",
            "https://example.com/",
            "http://example.com/",
            "https://tauri.localhost.evil.com/",
            "http://tauri.localhost:8080/",
            "tauri://evil/",
            "http://localhost:8080/",
            "https://localhost:1420/",
            "file:///etc/passwd",
            "data:text/html,<h1>x</h1>",
            "javascript:alert(1)",
            "ws://localhost:1420/",
            "about:config",
        ];
        for u in denied {
            assert!(
                !is_allowed_navigation(&tauri::Url::parse(u).unwrap()),
                "{u}"
            );
        }
        assert_eq!(
            is_allowed_navigation(&tauri::Url::parse("http://localhost:1420/").unwrap()),
            cfg!(debug_assertions)
        );
    }

    #[test]
    fn test_timed_unlock_context_guards_and_audit() {
        tauri::async_runtime::block_on(async {
            let dir = std::env::temp_dir().join(format!("fg-unlock-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let settings = Arc::new(SettingsStore::new(dir.join("context-settings.json")));
            let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
            let manager = Arc::new(ClusterManager::new(
                KubeconfigSource::from_paths(Vec::new()),
            ));
            let state = AppState::with_parts_and_audit(manager, settings.clone(), audit.clone());

            // 1. Configure protected + read-only context
            settings
                .set(
                    "prod-cluster",
                    ContextSettings {
                        read_only: true,
                        protected: true,
                        ..Default::default()
                    },
                )
                .unwrap();

            // Attempt unlock without confirmation -> fails
            let conf_err = unlock_context_impl(&state, "prod-cluster".to_owned(), None)
                .await
                .unwrap_err();
            assert_eq!(conf_err.kind, ErrorKind::ConfirmationRequired);

            // Attempt unlock with wrong confirmation -> fails
            let wrong_err = unlock_context_impl(
                &state,
                "prod-cluster".to_owned(),
                Some("wrong-name".to_owned()),
            )
            .await
            .unwrap_err();
            assert_eq!(wrong_err.kind, ErrorKind::ConfirmationRequired);

            // Unlock with exact confirmation -> succeeds
            let unlocked = unlock_context_impl(
                &state,
                "prod-cluster".to_owned(),
                Some("prod-cluster".to_owned()),
            )
            .await
            .unwrap();
            assert!(unlocked.read_only); // Base setting preserved
            assert!(unlocked.is_unlocked());
            assert!(!unlocked.effective_read_only());
            assert!(unlocked.unlocked_until.is_some());

            // Verify audit log has unlock_context entry
            let page = audit.query(0, 10, None, None).unwrap();
            assert_eq!(page.entries.len(), 1);
            assert_eq!(page.entries[0].action, "unlock_context");
            assert_eq!(page.entries[0].cluster, "prod-cluster");
            assert!(page.entries[0].success);

            // Relock
            let relocked = relock_context_impl(&state, "prod-cluster".to_owned())
                .await
                .unwrap();
            assert!(relocked.effective_read_only());
            assert!(!relocked.is_unlocked());

            // Verify audit log has relock_context entry
            let page2 = audit.query(0, 10, None, None).unwrap();
            assert_eq!(page2.entries.len(), 2);
            assert_eq!(page2.entries[0].action, "relock_context");
            assert_eq!(page2.entries[0].cluster, "prod-cluster");

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn test_open_external_terminal_guards_and_audit() {
        tauri::async_runtime::block_on(async {
            let dir = std::env::temp_dir().join(format!("fg-term-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let settings = Arc::new(SettingsStore::new(dir.join("context-settings.json")));
            let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
            let manager = Arc::new(ClusterManager::new(
                KubeconfigSource::from_paths(Vec::new()),
            ));
            let state = AppState::with_parts_and_audit(manager, settings.clone(), audit.clone());

            settings
                .set(
                    "stage-cluster",
                    ContextSettings {
                        read_only: true,
                        protected: false,
                        ..Default::default()
                    },
                )
                .unwrap();

            let req = ExternalTerminalRequest {
                target: ExternalTerminalTarget::PodExec {
                    cluster: "stage-cluster".to_owned(),
                    namespace: "default".to_owned(),
                    pod: "web-pod".to_owned(),
                    container: None,
                    shell: None,
                },
                confirmed_name: None,
            };

            // Read-only blocks and writes audit entry
            let err = open_external_terminal_impl(&state, req.clone())
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::ReadOnlyContext);

            let page = audit.query(0, 10, None, None).unwrap();
            assert_eq!(page.entries.len(), 1);
            assert_eq!(page.entries[0].action, "open_external_terminal");
            assert_eq!(page.entries[0].cluster, "stage-cluster");
            assert!(!page.entries[0].success);
            assert!(page.entries[0].error.is_some());

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn test_open_in_browser_guards_and_audit() {
        tauri::async_runtime::block_on(async {
            let dir = std::env::temp_dir().join(format!("fg-browser-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let settings = Arc::new(SettingsStore::new(dir.join("context-settings.json")));
            let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
            let manager = Arc::new(ClusterManager::new(
                KubeconfigSource::from_paths(Vec::new()),
            ));
            let state = AppState::with_parts_and_audit(manager, settings, audit.clone());
            let latest = || audit.query(0, 1, None, None).unwrap().entries[0].clone();
            let count = || audit.query(0, 100, None, None).unwrap().entries.len();

            // 1. Invalid URL: rejected without launching, audited with truncated input.
            let long_bad = format!("javascript:alert(1)//{}", "x".repeat(5000));
            let mut launched = false;
            let err = open_in_browser_with_launcher(&state, &long_bad, |_| {
                launched = true;
                Ok(())
            })
            .await
            .unwrap_err();
            assert!(!launched);
            assert_eq!(err.kind, ErrorKind::InvalidInput);
            assert_eq!(count(), 1);
            let rejected = latest();
            assert_eq!(rejected.action, "open_in_browser");
            assert_eq!(rejected.kind, "Browser");
            assert!(!rejected.success);
            assert!(rejected.error.is_some());
            assert!(rejected.name.chars().count() <= 201);
            assert!(rejected.kubectl.len() < 400);

            let err = open_in_browser_impl(&state, "http://0.0.0.0:8080")
                .await
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::InvalidInput);
            assert_eq!(count(), 2);

            // 2. An active port-forward (ephemeral port) is attributed, also with a trailing `/`.
            let pf = state
                .port_forwards
                .start_local(
                    "stage-cluster".to_owned(),
                    "default".to_owned(),
                    "nginx-pod".to_owned(),
                    80,
                    0,
                    None,
                )
                .await
                .unwrap();
            for url in [pf.browser_url.clone(), format!("{}/", pf.browser_url)] {
                let mut launched_url = String::new();
                open_in_browser_with_launcher(&state, &url, |u| {
                    launched_url = u.to_owned();
                    Ok(())
                })
                .await
                .unwrap();
                assert_eq!(launched_url, format!("{}/", pf.browser_url));
                let entry = latest();
                assert_eq!(entry.cluster, "stage-cluster");
                assert_eq!(entry.namespace.as_deref(), Some("default"));
                assert_eq!(entry.name, "nginx-pod");
                assert_eq!(entry.kind, "PortForward");
                assert!(entry.success);
            }
            assert_eq!(count(), 4);

            // 3. Ingress hostname with a query string: normalised URL goes to the launcher.
            let mut launched_url = String::new();
            open_in_browser_with_launcher(&state, "https://App.Example.com/a?x=1&y=2", |u| {
                launched_url = u.to_owned();
                Ok(())
            })
            .await
            .unwrap();
            assert_eq!(launched_url, "https://app.example.com/a?x=1&y=2");
            let entry = latest();
            assert_eq!(entry.kind, "Browser");
            assert_eq!(entry.name, "https://app.example.com/a?x=1&y=2");
            assert!(entry.success);

            // 4. Launcher failure is returned and audited.
            let err = open_in_browser_with_launcher(&state, &pf.browser_url, |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "opener not found",
                ))
            })
            .await
            .unwrap_err();
            assert_eq!(err.kind, ErrorKind::Internal);
            let entry = latest();
            assert_eq!(entry.kind, "PortForward");
            assert!(!entry.success);
            assert_eq!(entry.error.as_deref(), Some("opener not found"));
            assert_eq!(count(), 6);

            state.port_forwards.stop(pf.id);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}
