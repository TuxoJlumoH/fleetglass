//! Multi-cluster connection manager, reference-counted watch stores, and
//! batched query subscriptions (`<= 10` updates/sec).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding,
    ValidatingWebhookConfiguration,
};
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::api::core::v1::{
    ConfigMap, Event as K8sEvent, LimitRange, Namespace, Node, PersistentVolume,
    PersistentVolumeClaim, Pod, ResourceQuota, Secret, Service, ServiceAccount,
};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::networking::v1::{Ingress, NetworkPolicy};
use k8s_openapi::api::node::v1::RuntimeClass;
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding};
use k8s_openapi::api::scheduling::v1::PriorityClass;
use k8s_openapi::api::storage::v1::{StorageClass, VolumeAttachment};
use kube::Resource;
use kube::api::{ApiResource, DynamicObject};
use kube::client::AuthError;
use kube::config::KubeConfigOptions;
use kube::discovery::Discovery;
use kube::runtime::watcher::{self, Event};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::api::{
    AppPage, AppQuerySpec, AppSummary, Cell, ColumnDef, KindCount, MetricsAvailability,
    NamespaceWatch, Page, PageDelta, QuerySpec, Row, RowStatus, StoreAccess, StoreAccessState,
    WatchScope,
};
use crate::app_view::group_rows_by_app_iter;
use crate::columns::CustomColumns;
use crate::error::{Error, Result};
use crate::kubeconfig::KubeconfigSource;
use crate::metrics::{
    METRICS_POLL_INTERVAL, METRICS_POLL_TIMEOUT, METRICS_STOP_GRACE, UsageMap, apply_usage,
    classify_metrics_error, fetch_usage, usage_cell_indices,
};
use crate::ns_fallback::FallbackPlan;
use crate::projection::{classify_kube_error, project_pod_with_columns};
use crate::query::{clamp_limit, execute_query_iter};
use crate::shell_env::ShellEnv;

/// Default duration after which a store with zero active subscriptions is eligible for eviction.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Minimum interval between pushed subscription deltas (at most 10 deltas/sec).
pub const SUBSCRIPTION_BATCH_INTERVAL: Duration = Duration::from_millis(100);

/// Capacity of the per-subscription bounded channel.
const SUBSCRIPTION_CHANNEL_CAPACITY: usize = 32;

/// How often [`ClusterManager::idle_reaper`] checks for idle stores by default.
pub const DEFAULT_REAP_INTERVAL: Duration = Duration::from_secs(30);

/// Upper bound on the entries [`ClusterManager::kind_counts`] returns. One
/// cluster has one store per watched kind, far fewer than this in practice.
pub const MAX_KIND_COUNTS: usize = 256;

/// Upper bound on the clusters one Overview request covers (02-08a). The
/// requested list is deduplicated and cut to its first `MAX_OVERVIEW_CLUSTERS`
/// entries, so one IPC call never starts more than
/// `MAX_OVERVIEW_CLUSTERS * OVERVIEW_KINDS.len()` watchers and the reply has at
/// most this many cluster cards.
pub const MAX_OVERVIEW_CLUSTERS: usize = 64;

/// Minimum spacing between two Overview pushes (≤ 1 update/s).
const OVERVIEW_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// An Overview subscription recomputes at least this often without store
/// changes, so ages and the "warnings in the last hour" count stay current.
const OVERVIEW_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Kind used when a [`QuerySpec`] lists no kinds.
const DEFAULT_KIND: &str = "Pod";

/// Canonical name of `kind` if [`ClusterManager::ensure_watching`] can watch it live as a built-in.
pub(crate) fn canonical_builtin_kind(kind: &str) -> Option<&'static str> {
    if kind.eq_ignore_ascii_case("Pod")
        || kind.eq_ignore_ascii_case("Pods")
        || kind.eq_ignore_ascii_case("po")
    {
        Some("Pod")
    } else if kind.eq_ignore_ascii_case("Deployment")
        || kind.eq_ignore_ascii_case("Deployments")
        || kind.eq_ignore_ascii_case("deploy")
    {
        Some("Deployment")
    } else if kind.eq_ignore_ascii_case("StatefulSet")
        || kind.eq_ignore_ascii_case("StatefulSets")
        || kind.eq_ignore_ascii_case("sts")
    {
        Some("StatefulSet")
    } else if kind.eq_ignore_ascii_case("DaemonSet")
        || kind.eq_ignore_ascii_case("DaemonSets")
        || kind.eq_ignore_ascii_case("ds")
    {
        Some("DaemonSet")
    } else if kind.eq_ignore_ascii_case("ReplicaSet")
        || kind.eq_ignore_ascii_case("ReplicaSets")
        || kind.eq_ignore_ascii_case("rs")
    {
        Some("ReplicaSet")
    } else if kind.eq_ignore_ascii_case("Job") || kind.eq_ignore_ascii_case("Jobs") {
        Some("Job")
    } else if kind.eq_ignore_ascii_case("CronJob")
        || kind.eq_ignore_ascii_case("CronJobs")
        || kind.eq_ignore_ascii_case("cj")
    {
        Some("CronJob")
    } else if kind.eq_ignore_ascii_case("Service")
        || kind.eq_ignore_ascii_case("Services")
        || kind.eq_ignore_ascii_case("svc")
    {
        Some("Service")
    } else if kind.eq_ignore_ascii_case("Ingress")
        || kind.eq_ignore_ascii_case("Ingresses")
        || kind.eq_ignore_ascii_case("ing")
    {
        Some("Ingress")
    } else if kind.eq_ignore_ascii_case("EndpointSlice")
        || kind.eq_ignore_ascii_case("EndpointSlices")
    {
        Some("EndpointSlice")
    } else if kind.eq_ignore_ascii_case("HorizontalPodAutoscaler")
        || kind.eq_ignore_ascii_case("HorizontalPodAutoscalers")
        || kind.eq_ignore_ascii_case("hpa")
        || kind.eq_ignore_ascii_case("hpas")
    {
        Some("HorizontalPodAutoscaler")
    } else if kind.eq_ignore_ascii_case("PodDisruptionBudget")
        || kind.eq_ignore_ascii_case("PodDisruptionBudgets")
        || kind.eq_ignore_ascii_case("pdb")
        || kind.eq_ignore_ascii_case("pdbs")
    {
        Some("PodDisruptionBudget")
    } else if kind.eq_ignore_ascii_case("ConfigMap")
        || kind.eq_ignore_ascii_case("ConfigMaps")
        || kind.eq_ignore_ascii_case("cm")
    {
        Some("ConfigMap")
    } else if kind.eq_ignore_ascii_case("Secret") || kind.eq_ignore_ascii_case("Secrets") {
        Some("Secret")
    } else if kind.eq_ignore_ascii_case("ResourceQuota")
        || kind.eq_ignore_ascii_case("ResourceQuotas")
        || kind.eq_ignore_ascii_case("quota")
    {
        Some("ResourceQuota")
    } else if kind.eq_ignore_ascii_case("LimitRange")
        || kind.eq_ignore_ascii_case("LimitRanges")
        || kind.eq_ignore_ascii_case("limits")
    {
        Some("LimitRange")
    } else if kind.eq_ignore_ascii_case("PriorityClass")
        || kind.eq_ignore_ascii_case("PriorityClasses")
        || kind.eq_ignore_ascii_case("pc")
    {
        Some("PriorityClass")
    } else if kind.eq_ignore_ascii_case("RuntimeClass")
        || kind.eq_ignore_ascii_case("RuntimeClasses")
    {
        Some("RuntimeClass")
    } else if kind.eq_ignore_ascii_case("Lease") || kind.eq_ignore_ascii_case("Leases") {
        Some("Lease")
    } else if kind.eq_ignore_ascii_case("Node")
        || kind.eq_ignore_ascii_case("Nodes")
        || kind.eq_ignore_ascii_case("no")
    {
        Some("Node")
    } else if kind.eq_ignore_ascii_case("Namespace")
        || kind.eq_ignore_ascii_case("Namespaces")
        || kind.eq_ignore_ascii_case("ns")
    {
        Some("Namespace")
    } else if kind.eq_ignore_ascii_case("Event") || kind.eq_ignore_ascii_case("Events") {
        Some("Event")
    } else if kind.eq_ignore_ascii_case("PersistentVolume")
        || kind.eq_ignore_ascii_case("PersistentVolumes")
        || kind.eq_ignore_ascii_case("pv")
        || kind.eq_ignore_ascii_case("pvs")
    {
        Some("PersistentVolume")
    } else if kind.eq_ignore_ascii_case("PersistentVolumeClaim")
        || kind.eq_ignore_ascii_case("PersistentVolumeClaims")
        || kind.eq_ignore_ascii_case("pvc")
        || kind.eq_ignore_ascii_case("pvcs")
    {
        Some("PersistentVolumeClaim")
    } else if kind.eq_ignore_ascii_case("StorageClass")
        || kind.eq_ignore_ascii_case("StorageClasses")
        || kind.eq_ignore_ascii_case("sc")
    {
        Some("StorageClass")
    } else if kind.eq_ignore_ascii_case("VolumeAttachment")
        || kind.eq_ignore_ascii_case("VolumeAttachments")
    {
        Some("VolumeAttachment")
    } else if kind.eq_ignore_ascii_case("ServiceAccount")
        || kind.eq_ignore_ascii_case("ServiceAccounts")
        || kind.eq_ignore_ascii_case("sa")
    {
        Some("ServiceAccount")
    } else if kind.eq_ignore_ascii_case("Role") || kind.eq_ignore_ascii_case("Roles") {
        Some("Role")
    } else if kind.eq_ignore_ascii_case("ClusterRole") || kind.eq_ignore_ascii_case("ClusterRoles")
    {
        Some("ClusterRole")
    } else if kind.eq_ignore_ascii_case("RoleBinding") || kind.eq_ignore_ascii_case("RoleBindings")
    {
        Some("RoleBinding")
    } else if kind.eq_ignore_ascii_case("ClusterRoleBinding")
        || kind.eq_ignore_ascii_case("ClusterRoleBindings")
    {
        Some("ClusterRoleBinding")
    } else if kind.eq_ignore_ascii_case("NetworkPolicy")
        || kind.eq_ignore_ascii_case("NetworkPolicies")
        || kind.eq_ignore_ascii_case("netpol")
    {
        Some("NetworkPolicy")
    } else if kind.eq_ignore_ascii_case("MutatingWebhookConfiguration")
        || kind.eq_ignore_ascii_case("MutatingWebhookConfigurations")
    {
        Some("MutatingWebhookConfiguration")
    } else if kind.eq_ignore_ascii_case("ValidatingWebhookConfiguration")
        || kind.eq_ignore_ascii_case("ValidatingWebhookConfigurations")
    {
        Some("ValidatingWebhookConfiguration")
    } else if kind.eq_ignore_ascii_case("ValidatingAdmissionPolicy")
        || kind.eq_ignore_ascii_case("ValidatingAdmissionPolicies")
    {
        Some("ValidatingAdmissionPolicy")
    } else if kind.eq_ignore_ascii_case("ValidatingAdmissionPolicyBinding")
        || kind.eq_ignore_ascii_case("ValidatingAdmissionPolicyBindings")
    {
        Some("ValidatingAdmissionPolicyBinding")
    } else {
        None
    }
}

/// Watch task bookkeeping of one store. A single mutex guards the
/// "is a watcher running?" check and the spawn, so concurrent
/// [`ClusterManager::ensure_watching`] calls start at most one watcher.
#[derive(Default)]
struct WatcherSlot {
    handle: Option<JoinHandle<()>>,
    /// Set once the store left the manager's map; no watcher may start afterwards.
    evicted: bool,
    /// Number of watcher tasks started for this store (diagnostics and tests).
    started: u32,
    /// `metrics.k8s.io` poller of a `Pod` / `Node` store (02-01); runs only
    /// while the store has subscribers. Cleared by the task when it exits.
    metrics_poller: Option<JoinHandle<()>>,
    /// Number of metrics pollers ever started (diagnostics and tests).
    metrics_pollers_started: u32,
    /// Set while the app is in the background (03-04a): the watcher and the
    /// metrics poller are stopped and none may start until resumed. Unlike
    /// `evicted`, rows and subscriptions are kept.
    paused: bool,
}

/// Result of [`ResourceStore::start_watcher_once`].
enum StartOutcome {
    Started,
    AlreadyRunning,
    Evicted,
    /// The app is in the background; the watcher starts on resume.
    Paused,
}

/// Rows plus the custom columns they were projected with, under one lock so a
/// row projected with an outdated column set is never inserted.
struct StoreData {
    rows: BTreeMap<String, Row>,
    columns: Arc<CustomColumns>,
    /// `(cpu, memory)` cell indices for kinds with usage columns (`Pod`, `Node`).
    usage_cols: Option<(usize, usize)>,
    /// Last successful metrics poll; `None` before the first one or while
    /// metrics are unavailable. Rows are joined with it on insert.
    usage: Option<UsageMap>,
}

impl StoreData {
    /// Fills the usage cells of a row about to be inserted from the last poll.
    fn join_usage(&self, row: &mut Row) {
        if let Some(cols) = self.usage_cols
            && self.usage.is_some()
        {
            apply_usage(row, cols, self.usage.as_ref());
        }
    }
}

/// In-memory store of projected [`Row`]s for a single `(cluster, kind)` pair.
///
/// Full Kubernetes objects are projected immediately on ingest and discarded so
/// memory stays proportional to row count and column count.
///
/// Custom JSONPath columns (`P2-08`) are evaluated at projection time into
/// [`Row::custom`]. Because no object copy is kept, adding a column path the
/// rows have not been projected with triggers a re-list of the store's watch
/// (see [`ResourceStore::set_custom_columns`]); removing or reordering columns
/// only remaps the existing values.
/// Callback invoked to invalidate a cached client when HTTP 401 Unauthorized occurs.
pub type ClientInvalidator = Arc<dyn Fn(&str) + Send + Sync>;

type ClientMap = Arc<Mutex<HashMap<String, Arc<tokio::sync::OnceCell<kube::Client>>>>>;
type StoreMap = Arc<RwLock<HashMap<(String, String), Arc<ResourceStore>>>>;
type ColumnMap = Arc<RwLock<HashMap<String, Arc<CustomColumns>>>>;
type DiscoveryMap = Arc<RwLock<HashMap<String, Vec<ApiResource>>>>;
/// Per cluster, `group/plural` of the discovered namespaced resources (05-01):
/// only those get a per-namespace fallback.
type NamespacedMap = Arc<RwLock<HashMap<String, HashSet<String>>>>;
/// Per-context `watchNamespaces` settings (05-01), shared with every store.
type WatchNamespacesMap = Arc<RwLock<HashMap<String, Vec<String>>>>;

/// After a namespace fallback could not start (no accessible namespace, or the
/// access reviews failed), the cluster-wide watch keeps retrying with its own
/// backoff, but the fallback is not planned again before this delay (or a resync).
const FALLBACK_RETRY: Duration = Duration::from_secs(300);

/// How a store whose cluster-wide list/watch is forbidden switches to one
/// watcher per accessible namespace (05-01). Only namespaced kinds have one.
pub(crate) struct NamespaceFallback<K> {
    client: kube::Client,
    /// API group (`""` for core) and plural resource, for the access reviews.
    group: String,
    resource: String,
    api_for: NamespacedApiFn<K>,
}

/// Builds the `Api` of one namespace for a [`NamespaceFallback`].
type NamespacedApiFn<K> = Arc<dyn Fn(&str) -> kube::Api<K> + Send + Sync>;

/// How [`ResourceStore::run_namespace_fallback`] ended.
enum FallbackEnd {
    /// A resync was requested: retry the cluster-wide watch.
    Resync,
    /// The store is gone or every stream ended.
    Closed,
}

pub struct ResourceStore {
    cluster: String,
    kind: String,
    data: RwLock<StoreData>,
    version: AtomicU32,
    version_tx: watch::Sender<u32>,
    /// Bumped to make the running watch loop drop its stream and re-list.
    resync_tx: watch::Sender<u64>,
    ref_count: AtomicU32,
    last_zero_refs_at: Mutex<Option<Instant>>,
    /// RBAC / connection state reported with every [`Page`] (non-negotiable 5).
    access: RwLock<StoreAccessState>,
    /// Set once the store reached [`StoreAccessState::Ready`]. Until then an
    /// initial list is published progressively (labelled `Loading { rows_so_far }`);
    /// afterwards a re-list keeps the old rows until it is complete.
    synced: AtomicBool,
    watcher: Mutex<WatcherSlot>,
    client_invalidator: Mutex<Option<ClientInvalidator>>,
    /// `metrics.k8s.io` state for kinds with usage columns, `None` otherwise.
    metrics: RwLock<Option<MetricsAvailability>>,
    /// Client the metrics poller uses; set when the store's watcher starts.
    metrics_client: Mutex<Option<kube::Client>>,
    /// Signalled when the last subscriber leaves, so the poller can stop.
    metrics_idle: tokio::sync::Notify,
    /// Restricted scope while per-namespace watchers replace a forbidden
    /// cluster-wide watch (05-01); `None` for a cluster-wide watch.
    scope: RwLock<Option<WatchScope>>,
    /// The manager's per-context `watchNamespaces` settings, if registered.
    watch_namespaces: Mutex<Option<WatchNamespacesMap>>,
}

impl ResourceStore {
    /// Creates an empty [`ResourceStore`] for `(cluster, kind)` starting at version `1`.
    #[must_use]
    pub fn new(cluster: impl Into<String>, kind: impl Into<String>) -> Arc<Self> {
        Self::with_columns(cluster, kind, Arc::default())
    }

    /// Creates an empty store whose rows are projected with `columns`.
    #[must_use]
    pub fn with_columns(
        cluster: impl Into<String>,
        kind: impl Into<String>,
        columns: Arc<CustomColumns>,
    ) -> Arc<Self> {
        let (version_tx, _) = watch::channel(1);
        let (resync_tx, _) = watch::channel(0);
        let kind: String = kind.into();
        let usage_cols = usage_cell_indices(&kind);
        Arc::new(Self {
            cluster: cluster.into(),
            metrics: RwLock::new(usage_cols.map(|_| MetricsAvailability::Pending)),
            metrics_client: Mutex::new(None),
            metrics_idle: tokio::sync::Notify::new(),
            kind,
            data: RwLock::new(StoreData {
                rows: BTreeMap::new(),
                columns,
                usage_cols,
                usage: None,
            }),
            version: AtomicU32::new(1),
            version_tx,
            resync_tx,
            ref_count: AtomicU32::new(0),
            last_zero_refs_at: Mutex::new(Some(Instant::now())),
            access: RwLock::new(StoreAccessState::CONNECTING),
            synced: AtomicBool::new(false),
            watcher: Mutex::new(WatcherSlot::default()),
            client_invalidator: Mutex::new(None),
            scope: RwLock::new(None),
            watch_namespaces: Mutex::new(None),
        })
    }

    /// Registers a callback invoked to invalidate cached clients when HTTP 401 Unauthorized occurs.
    pub fn set_client_invalidator(&self, invalidator: ClientInvalidator) {
        let mut guard = self
            .client_invalidator
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *guard = Some(invalidator);
    }

    /// Invalidation callback registered on this store, if any.
    #[must_use]
    pub fn client_invalidator(&self) -> Option<ClientInvalidator> {
        let guard = self
            .client_invalidator
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        guard.clone()
    }

    /// Restricted scope of a store fed by per-namespace watchers (05-01), `None`
    /// for a cluster-wide watch.
    #[must_use]
    pub fn scope(&self) -> Option<WatchScope> {
        self.scope
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_watch_namespaces_map(&self, map: WatchNamespacesMap) {
        *self
            .watch_namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(map);
    }

    /// Configured `watchNamespaces` of this store's context (empty: detect).
    fn configured_namespaces(&self) -> Vec<String> {
        let map = self
            .watch_namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        map.and_then(|m| {
            m.read()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&self.cluster)
                .cloned()
        })
        .unwrap_or_default()
    }

    /// Switches the store to the per-namespace scope of `plan`: every watched
    /// namespace starts loading, and rows outside those namespaces (left from an
    /// earlier cluster-wide list) are dropped, so rows and scope always agree.
    fn enter_namespace_scope(&self, plan: &FallbackPlan) {
        let scope = WatchScope {
            namespaces: plan
                .allowed
                .iter()
                .map(|ns| NamespaceWatch {
                    namespace: ns.clone(),
                    access: StoreAccessState::CONNECTING,
                })
                .collect(),
            source: plan.source,
            denied: plan.denied.clone(),
            truncated: plan.truncated,
        };
        let mut data = self.data.write().unwrap_or_else(PoisonError::into_inner);
        data.rows.retain(|_, row| {
            row.namespace
                .as_ref()
                .is_some_and(|ns| plan.allowed.binary_search(ns).is_ok())
        });
        let rows = data.rows.len();
        let access = combined_namespace_access(&scope.namespaces, self.has_synced(), rows);
        *self.scope.write().unwrap_or_else(PoisonError::into_inner) = Some(scope);
        self.replace_access(access);
        drop(data);
        self.notify_changed();
    }

    /// Back to a cluster-wide watch (watcher start or resync): clears the scope
    /// and reports the store as (re)connecting.
    fn leave_namespace_scope(&self) {
        let had = self
            .scope
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_some();
        if had {
            self.notify_changed();
        }
        self.watcher_starting();
    }

    /// Sets the state of one per-namespace watcher and recomputes the store's
    /// access state from all of them, with one notification on a change.
    fn set_namespace_state(&self, namespace: &str, state: StoreAccessState) {
        // Lock order data → scope → access, as in `enter_namespace_scope`.
        let data = self.data.read().unwrap_or_else(PoisonError::into_inner);
        let mut scope = self.scope.write().unwrap_or_else(PoisonError::into_inner);
        let Some(scope) = scope.as_mut() else {
            return;
        };
        let Some(entry) = scope
            .namespaces
            .iter_mut()
            .find(|n| n.namespace == namespace)
        else {
            return;
        };
        let mut changed = false;
        if entry.access != state {
            entry.access = state;
            changed = true;
        }
        let access =
            combined_namespace_access(&scope.namespaces, self.has_synced(), data.rows.len());
        changed |= self.replace_access(access);
        drop(data);
        if changed {
            self.notify_changed();
        }
    }

    /// Replaces the rows of `namespace` with a completed list of it (projected
    /// with `projected_with`): rows of that namespace not in the list are removed.
    fn replace_namespace_rows(
        &self,
        namespace: &str,
        mut rows: BTreeMap<String, Row>,
        projected_with: &Arc<CustomColumns>,
    ) {
        let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&guard.columns, projected_with) && *guard.columns != **projected_with {
            let mapping = column_mapping(projected_with, &guard.columns);
            for row in rows.values_mut() {
                remap_custom(row, &mapping);
            }
        }
        guard.rows.retain(|uid, row| {
            row.namespace.as_deref() != Some(namespace) || rows.contains_key(uid)
        });
        for (uid, mut row) in rows {
            guard.join_usage(&mut row);
            guard.rows.insert(uid, row);
        }
        drop(guard);
        self.notify_changed();
    }

    /// Current access state (ready, loading, forbidden, error, ...).
    #[must_use]
    pub fn access(&self) -> StoreAccessState {
        self.access
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sets the access state, notifying subscribers only when it changed.
    pub fn set_access(&self, state: StoreAccessState) {
        if self.replace_access(state) {
            self.notify_changed();
        }
    }

    /// Sets the access state without notifying; `true` when it changed.
    fn replace_access(&self, state: StoreAccessState) -> bool {
        if state == StoreAccessState::Ready {
            self.synced.store(true, Ordering::Release);
        }
        {
            let guard = self.access.read().unwrap_or_else(PoisonError::into_inner);
            if *guard == state {
                return false;
            }
        }
        let mut guard = self.access.write().unwrap_or_else(PoisonError::into_inner);
        if *guard == state {
            return false;
        }
        *guard = state;
        true
    }

    /// Sets a failure `state`. A store that never synced and is denied by RBAC or
    /// hits a missing / unsupported API also drops its rows: the honest message is
    /// "No permission to list X", and rows received earlier would be misleading under
    /// it. Rows and state change under the data write lock (which [`ClusterManager::query`]
    /// takes before reading the state) with one notification, so no query sees the
    /// cleared rows next to the old (e.g. Loading) state.
    fn set_failure(&self, state: StoreAccessState) {
        let synced = self.has_synced();
        let clear = !synced
            && matches!(
                state,
                StoreAccessState::Forbidden { .. }
                    | StoreAccessState::NotFound { .. }
                    | StoreAccessState::Unsupported { .. }
                    | StoreAccessState::AuthExpired { .. }
            );
        // Lock order data → access, as in `query`.
        let mut data = self.data.write().unwrap_or_else(PoisonError::into_inner);
        let cleared = clear && !data.rows.is_empty();
        if cleared {
            data.rows.clear();
        }
        // Partial rows are counted after any clearing.
        let state = match state {
            StoreAccessState::Error { message, .. } => StoreAccessState::Error {
                message,
                partial_rows: Self::partial_rows(synced, data.rows.len()),
            },
            StoreAccessState::AuthExpired {
                provider,
                hint,
                login_command,
                device_code,
                ..
            } => StoreAccessState::AuthExpired {
                provider,
                hint,
                login_command,
                device_code,
                partial_rows: Self::partial_rows(synced, data.rows.len()),
            },
            other => other,
        };
        let changed = self.replace_access(state);
        drop(data);
        if cleared || changed {
            self.notify_changed();
        }
    }

    /// Triggers the watcher to drop its stream and re-list (`04-02`).
    pub fn resync(&self) {
        if self.resync_tx.receiver_count() > 0 {
            self.resync_tx.send_modify(|generation| *generation += 1);
        }
    }

    /// `true` while a watcher task started through [`ClusterManager::ensure_watching`] runs.
    #[must_use]
    pub fn watcher_active(&self) -> bool {
        let slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        slot.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Number of watcher tasks ever started for this store via [`ClusterManager::ensure_watching`].
    #[must_use]
    pub fn watchers_started(&self) -> u32 {
        self.watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .started
    }

    /// Starts a watcher with `spawn` unless one is running or the store was evicted.
    /// Check and spawn happen under one lock, so concurrent callers cannot both spawn.
    fn start_watcher_once(
        self: &Arc<Self>,
        spawn: impl FnOnce(&Arc<Self>) -> JoinHandle<()>,
    ) -> StartOutcome {
        let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.evicted {
            return StartOutcome::Evicted;
        }
        if slot.paused {
            return StartOutcome::Paused;
        }
        if slot.handle.as_ref().is_some_and(|h| !h.is_finished()) {
            return StartOutcome::AlreadyRunning;
        }
        slot.handle = Some(spawn(self));
        slot.started = slot.started.saturating_add(1);
        StartOutcome::Started
    }

    /// `true` while the store is paused because the app is in the background:
    /// rows are the last known ones and nothing refreshes them.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .paused
    }

    /// Pauses (`true`) or unpauses the store. Pausing aborts the watcher and the
    /// metrics poller (no reconnect, no backoff) and blocks restarting them;
    /// rows, access state and subscriptions are kept. Subscribers are notified
    /// when the flag changes, so pages report it. Returns `true` on a change.
    fn set_paused(&self, paused: bool) -> bool {
        {
            let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.paused == paused {
                return false;
            }
            slot.paused = paused;
            if paused {
                if let Some(handle) = slot.handle.take() {
                    handle.abort();
                }
                if let Some(handle) = slot.metrics_poller.take() {
                    handle.abort();
                }
            }
        }
        self.notify_changed();
        true
    }

    /// Aborts the watcher and metrics poller (if any) and marks the store
    /// evicted so none starts again.
    fn stop_watcher(&self) {
        let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        slot.evicted = true;
        if let Some(handle) = slot.handle.take() {
            handle.abort();
        }
        if let Some(handle) = slot.metrics_poller.take() {
            handle.abort();
        }
    }

    /// `metrics.k8s.io` state of this store's usage cells; `None` for kinds
    /// without usage columns.
    #[must_use]
    pub fn metrics(&self) -> Option<MetricsAvailability> {
        self.metrics
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sets the metrics availability state of this store.
    pub fn set_metrics(&self, state: MetricsAvailability) {
        let mut current = self.metrics.write().unwrap_or_else(PoisonError::into_inner);
        *current = Some(state);
    }

    /// `true` while the metrics poller of this store runs.
    #[must_use]
    pub fn metrics_poller_active(&self) -> bool {
        let slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        slot.metrics_poller
            .as_ref()
            .is_some_and(|h| !h.is_finished())
    }

    /// Number of metrics pollers ever started for this store.
    #[must_use]
    pub fn metrics_pollers_started(&self) -> u32 {
        self.watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .metrics_pollers_started
    }

    /// Gives the metrics poller its client (when the store's watcher starts)
    /// and starts the poller if the store already has subscribers.
    fn set_metrics_client(self: &Arc<Self>, client: kube::Client) {
        if self.data_usage_cols().is_none() {
            return;
        }
        *self
            .metrics_client
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(client);
        self.ensure_metrics_poller();
    }

    fn data_usage_cols(&self) -> Option<(usize, usize)> {
        self.data
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .usage_cols
    }

    /// Starts the metrics poller of a `Pod` / `Node` store that has at least one
    /// subscriber and a client, unless it runs already. Called when a
    /// subscription acquires the store and when the store's watcher starts.
    fn ensure_metrics_poller(self: &Arc<Self>) {
        if self.ref_count() == 0 || self.data_usage_cols().is_none() {
            return;
        }
        let Some(client) = self
            .metrics_client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.evicted
            || slot.paused
            || slot
                .metrics_poller
                .as_ref()
                .is_some_and(|h| !h.is_finished())
        {
            return;
        }
        let store = Arc::clone(self);
        slot.metrics_poller = Some(runtime.spawn(store.run_metrics_poller(client)));
        slot.metrics_pollers_started = slot.metrics_pollers_started.saturating_add(1);
        tracing::debug!(cluster = %self.cluster, kind = %self.kind, "started metrics poller");
    }

    /// Polls `metrics.k8s.io` every [`METRICS_POLL_INTERVAL`] (first poll right
    /// away) while the store has subscribers. When they drop to zero for
    /// [`METRICS_STOP_GRACE`], clears the usage cells (no stale numbers are
    /// shown later) and exits.
    async fn run_metrics_poller(self: Arc<Self>, client: kube::Client) {
        let mut last_poll: Option<tokio::time::Instant> = None;
        loop {
            if self.ref_count() == 0 {
                tokio::time::sleep(METRICS_STOP_GRACE).await;
                // Re-checked under the slot lock: `ensure_metrics_poller` either
                // sees this task still running (and the count > 0 here) or the
                // slot already cleared, so a new subscriber always gets a poller.
                let mut slot = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
                if self.ref_count() == 0 {
                    slot.metrics_poller = None;
                    drop(slot);
                    self.apply_metrics_outcome(Err(MetricsAvailability::Pending));
                    tracing::debug!(cluster = %self.cluster, kind = %self.kind, "stopped metrics poller");
                    return;
                }
                continue;
            }
            // A subscriber that came back within the grace period keeps the cadence.
            if let Some(due) = last_poll.map(|t| t + METRICS_POLL_INTERVAL)
                && tokio::time::Instant::now() < due
            {
                tokio::select! {
                    () = tokio::time::sleep_until(due) => {}
                    () = self.metrics_idle.notified() => {}
                }
                continue;
            }
            last_poll = Some(tokio::time::Instant::now());
            let outcome =
                match tokio::time::timeout(METRICS_POLL_TIMEOUT, fetch_usage(&client, &self.kind))
                    .await
                {
                    Ok(Ok(map)) => Ok(map),
                    Ok(Err(err)) => Err(classify_metrics_error(&err)),
                    Err(_) => Err(MetricsAvailability::Error {
                        message: format!(
                            "metrics.k8s.io did not answer within {} s",
                            METRICS_POLL_TIMEOUT.as_secs()
                        ),
                    }),
                };
            self.apply_metrics_outcome(outcome);
        }
    }

    /// Joins a poll result into the rows in place and records the availability.
    /// `Err(state)` clears every usage cell. Subscribers are notified only when
    /// a value or the state changed.
    fn apply_metrics_outcome(&self, outcome: std::result::Result<UsageMap, MetricsAvailability>) {
        let (map, state) = match outcome {
            Ok(map) => (Some(map), MetricsAvailability::Available),
            Err(state) => (None, state),
        };
        let mut changed = false;
        {
            let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
            let Some(cols) = guard.usage_cols else {
                return;
            };
            if map.is_some() || guard.usage.is_some() {
                for row in guard.rows.values_mut() {
                    changed |= apply_usage(row, cols, map.as_ref());
                }
            }
            guard.usage = map;
        }
        {
            let mut current = self.metrics.write().unwrap_or_else(PoisonError::into_inner);
            if current.as_ref() != Some(&state) {
                match &state {
                    MetricsAvailability::NotInstalled { message }
                    | MetricsAvailability::Forbidden { message }
                    | MetricsAvailability::Error { message } => tracing::warn!(
                        cluster = %self.cluster,
                        kind = %self.kind,
                        error = %message,
                        "metrics unavailable"
                    ),
                    MetricsAvailability::Available | MetricsAvailability::Pending => {}
                }
                *current = Some(state);
                changed = true;
            }
        }
        if changed {
            self.notify_changed();
        }
    }

    /// Restarts the idle timer of an unreferenced store, so stores read only through
    /// one-shot queries stay alive while the UI keeps querying them.
    fn touch(&self) {
        if self.ref_count() == 0 {
            let mut idle = self
                .last_zero_refs_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *idle = Some(Instant::now());
        }
    }

    /// Custom columns rows of this store are currently projected with.
    #[must_use]
    pub fn custom_columns(&self) -> Arc<CustomColumns> {
        let guard = self.data.read().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(&guard.columns)
    }

    /// Switches the store to `columns`.
    ///
    /// Values of columns whose path was already projected are kept (remapped to
    /// the new order). If any path is new, its cells are left empty and the
    /// running watch loop (if any) is asked to re-list so every row is projected
    /// again; a store without a watcher keeps the empty cells until the next
    /// upsert. Returns `true` when a re-list was requested.
    pub fn set_custom_columns(&self, columns: Arc<CustomColumns>) -> bool {
        let needs_relist = {
            let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
            if *guard.columns == *columns {
                return false;
            }
            let mapping = column_mapping(&guard.columns, &columns);
            for row in guard.rows.values_mut() {
                remap_custom(row, &mapping);
            }
            guard.columns = columns;
            mapping.iter().any(Option::is_none)
        };
        self.notify_changed();
        if needs_relist && self.resync_tx.receiver_count() > 0 {
            self.resync_tx.send_modify(|generation| *generation += 1);
            return true;
        }
        false
    }

    /// Cluster context name this store belongs to.
    #[must_use]
    pub fn cluster(&self) -> &str {
        &self.cluster
    }

    /// Resource kind stored (e.g. `"Pod"`).
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Current monotonic version of this store.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version.load(Ordering::Acquire)
    }

    fn notify_changed(&self) {
        let next = self.bump_version();
        let _ = self.version_tx.send_replace(next);
    }

    /// Subscribes to version change notifications.
    #[must_use]
    pub fn subscribe_version(&self) -> watch::Receiver<u32> {
        self.version_tx.subscribe()
    }

    /// Inserts or replaces a projected [`Row`] keyed by `row.uid`, bumps the
    /// store version, and notifies subscribers.
    pub fn upsert_row(&self, mut row: Row) {
        let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
        guard.join_usage(&mut row);
        guard.rows.insert(row.uid.clone(), row);
        drop(guard);
        self.notify_changed();
    }

    /// Projects `pod` into a compact [`Row`] and upserts it into the store.
    pub fn upsert_pod(&self, pod: &Pod) {
        let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
        let mut row = project_pod_with_columns(pod, &self.cluster, &guard.columns);
        guard.join_usage(&mut row);
        guard.rows.insert(row.uid.clone(), row);
        drop(guard);
        self.notify_changed();
    }

    /// Replaces the entire store contents with `rows`, bumps the
    /// store version once, and notifies subscribers.
    pub fn replace_all_rows(&self, rows: &[Row]) {
        let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
        let mut next_map = BTreeMap::new();
        for row in rows {
            let mut row = row.clone();
            guard.join_usage(&mut row);
            next_map.insert(row.uid.clone(), row);
        }
        guard.rows = next_map;
        drop(guard);
        self.notify_changed();
    }

    /// Publishes the rows staged during an initial list as the entire store
    /// contents (taken by value, no copy), bumps the store version once, and
    /// notifies subscribers.
    ///
    /// If the store's columns changed since the rows were projected with
    /// `projected_with` (a removal or reorder, which does not re-list), the
    /// custom cells are remapped under the same lock so rows never carry an
    /// outdated column layout.
    fn publish_initial_rows(
        &self,
        mut rows: BTreeMap<String, Row>,
        projected_with: &Arc<CustomColumns>,
    ) {
        let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&guard.columns, projected_with) && *guard.columns != **projected_with {
            let mapping = column_mapping(projected_with, &guard.columns);
            for row in rows.values_mut() {
                remap_custom(row, &mapping);
            }
        }
        if guard.usage.is_some() {
            for row in rows.values_mut() {
                guard.join_usage(row);
            }
        }
        guard.rows = rows;
        drop(guard);
        self.notify_changed();
    }

    /// Moves the rows staged since the last call into the store (progressive
    /// first list, see [`InitStaging`]), remapping them to the store's current
    /// columns, and reports `Loading { rows_so_far }` with one notification.
    ///
    /// With `done` (at [`Event::InitDone`]) rows not seen in this list (deleted
    /// while it ran, or left over from an earlier failed attempt) are removed
    /// and the store becomes [`StoreAccessState::Ready`].
    fn publish_progress(&self, staging: &mut InitStaging, done: bool) {
        let (rows, projected_with) = staging.take();
        // The first batch of a list attempt (after `Event::Init`) proves the
        // list is streaming again, e.g. a retry after a failed first list.
        let attempt_streaming = staging.stale.is_none() && !rows.is_empty();
        let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
        // Rows present before this list attempt's first publish are the only
        // ones that can be missing from the list (the store is normally empty
        // here, so this set is too); each re-listed uid is crossed off.
        let stale = staging
            .stale
            .get_or_insert_with(|| guard.rows.keys().cloned().collect());
        let mapping = (!Arc::ptr_eq(&guard.columns, &projected_with)
            && *guard.columns != *projected_with)
            .then(|| column_mapping(&projected_with, &guard.columns));
        for (uid, mut row) in rows {
            if let Some(mapping) = &mapping {
                remap_custom(&mut row, mapping);
            }
            guard.join_usage(&mut row);
            stale.remove(&uid);
            guard.rows.insert(uid, row);
        }
        if done {
            for uid in std::mem::take(stale) {
                guard.rows.remove(&uid);
            }
        }
        let rows_so_far = u32::try_from(guard.rows.len()).unwrap_or(u32::MAX);
        drop(guard);
        {
            let mut access = self.access.write().unwrap_or_else(PoisonError::into_inner);
            if done {
                self.synced.store(true, Ordering::Release);
                *access = StoreAccessState::Ready;
            } else {
                match &mut *access {
                    // A retry's first batch: the store shows a list in progress
                    // again (stale rows of the failed attempt are counted and
                    // removed at InitDone), so the error no longer applies.
                    _ if attempt_streaming => {
                        *access = StoreAccessState::Loading { rows_so_far };
                    }
                    // Otherwise a progress publish never hides an error or RBAC
                    // state; it only keeps the partial row count of a failed list honest.
                    StoreAccessState::Loading { .. } => {
                        *access = StoreAccessState::Loading { rows_so_far };
                    }
                    StoreAccessState::Error {
                        partial_rows: Some(n),
                        ..
                    } => *n = rows_so_far,
                    _ => {}
                }
            }
        }
        staging.mark_published();
        self.notify_changed();
    }

    /// Row count to report as [`StoreAccessState::Error::partial_rows`]: the
    /// `count` rows of a first list that failed partway, or `None` once the store
    /// has synced (its rows were complete then) or when it is empty.
    fn partial_rows(synced: bool, count: usize) -> Option<u32> {
        (!synced && count > 0).then(|| u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Sets [`StoreAccessState::Error`] with `message`, labelling rows of an
    /// incomplete first list (see [`Self::set_failure`]).
    fn set_error(&self, message: String) {
        self.set_failure(StoreAccessState::Error {
            message,
            partial_rows: None,
        });
    }

    /// A watcher error that changes the access state to `state` during a list.
    /// For a first list, staged rows not yet published are dropped (the list
    /// restarts with [`Event::Init`]), so no pending progress publish replaces
    /// the error; rows already published stay, labelled as partial. A denied
    /// first list drops the published rows too (see [`Self::set_failure`]).
    fn list_failed(&self, staging: &mut InitStaging, state: StoreAccessState) {
        if !self.has_synced() {
            staging.discard_unpublished();
            if matches!(
                state,
                StoreAccessState::Forbidden { .. }
                    | StoreAccessState::NotFound { .. }
                    | StoreAccessState::Unsupported { .. }
                    | StoreAccessState::AuthExpired { .. }
            ) {
                staging.clear();
            }
        }
        self.set_failure(state);
    }

    /// [`Event::InitApply`]: stages `row` (projected with `cols`). Only a first
    /// list is shown while it streams in, in batches when
    /// [`InitStaging::publish_due`]; a re-list keeps the old rows until it is complete.
    fn init_apply(
        &self,
        staging: &mut InitStaging,
        row: Row,
        cols: &Arc<CustomColumns>,
        now: Instant,
    ) {
        staging.stage(row, cols);
        if !self.has_synced() && staging.publish_due(now) {
            self.publish_progress(staging, false);
        }
    }

    /// [`Event::InitDone`]: the list is complete and becomes the store contents.
    fn init_done(&self, staging: &mut InitStaging) {
        if self.has_synced() {
            let (rows, cols) = staging.take();
            self.publish_initial_rows(rows, &cols);
            self.set_access(StoreAccessState::Ready);
        } else {
            self.publish_progress(staging, true);
        }
    }

    /// Access state while a (re)started watcher connects: a synced store keeps
    /// its last complete list ([`StoreAccessState::Reconnecting`]); one that
    /// never synced is loading, possibly with partial rows of a failed first list.
    fn watcher_starting(&self) {
        self.set_access(if self.has_synced() {
            StoreAccessState::Reconnecting
        } else {
            StoreAccessState::Loading {
                rows_so_far: u32::try_from(self.row_count()).unwrap_or(u32::MAX),
            }
        });
    }

    /// Number of rows currently in the store.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.data
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .rows
            .len()
    }

    /// `true` once the store has been [`StoreAccessState::Ready`] at least once.
    #[must_use]
    pub fn has_synced(&self) -> bool {
        self.synced.load(Ordering::Acquire)
    }

    /// Replaces the entire store contents with the projected `pods`, bumps the
    /// store version once, and notifies subscribers.
    pub fn replace_all_pods(&self, pods: &[Pod]) {
        let (columns, cluster) = {
            let guard = self.data.read().unwrap_or_else(PoisonError::into_inner);
            (Arc::clone(&guard.columns), self.cluster.clone())
        };
        let rows: Vec<Row> = pods
            .iter()
            .map(|pod| project_pod_with_columns(pod, &cluster, &columns))
            .collect();
        self.replace_all_rows(&rows);
    }

    /// Removes the row with `uid` if present, bumping the store version and
    /// notifying subscribers.
    pub fn remove_uid(&self, uid: &str) -> bool {
        let mut guard = self.data.write().unwrap_or_else(PoisonError::into_inner);
        let removed = guard.rows.remove(uid).is_some();
        drop(guard);
        if removed {
            self.notify_changed();
        }
        removed
    }

    /// Returns a point-in-time snapshot of all rows along with the store version.
    #[must_use]
    pub fn snapshot_rows(&self) -> (Vec<Row>, u32) {
        let guard = self.data.read().unwrap_or_else(PoisonError::into_inner);
        let rows = guard.rows.values().cloned().collect();
        let ver = self.version.load(Ordering::Acquire);
        (rows, ver)
    }

    /// Invokes `f` with a reference to the stored rows under a read lock.
    pub fn read_rows<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&BTreeMap<String, Row>) -> R,
    {
        let guard = self.data.read().unwrap_or_else(PoisonError::into_inner);
        f(&guard.rows)
    }

    /// Increments the active subscriber reference count and clears the idle timer.
    pub fn acquire(&self) {
        self.ref_count.fetch_add(1, Ordering::AcqRel);
        let mut idle = self
            .last_zero_refs_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *idle = None;
    }

    /// Decrements the active subscriber reference count and records the idle
    /// timestamp when the count reaches zero.
    pub fn release(&self) {
        let mut prev = self.ref_count.load(Ordering::Acquire);
        while let Err(actual) = self.ref_count.compare_exchange_weak(
            prev,
            prev.saturating_sub(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            prev = actual;
        }
        if prev <= 1 {
            let mut idle = self
                .last_zero_refs_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *idle = Some(Instant::now());
            drop(idle);
            // Wakes a metrics poller waiting for its next poll so it can stop.
            self.metrics_idle.notify_waiters();
        }
    }

    /// Current number of active subscriber references.
    #[must_use]
    pub fn ref_count(&self) -> u32 {
        self.ref_count.load(Ordering::Acquire)
    }

    /// Returns `true` if `ref_count() == 0` and the store has been idle for at
    /// least `idle_timeout`.
    #[must_use]
    pub fn is_idle_expired(&self, idle_timeout: Duration) -> bool {
        if self.ref_count() != 0 {
            return false;
        }
        let idle = self
            .last_zero_refs_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        idle.is_some_and(|since| since.elapsed() >= idle_timeout)
    }

    /// Spawns a background task watching cluster-wide [`Pod`]s and projecting
    /// changes into this store.
    pub(crate) fn spawn_pod_watcher(self: &Arc<Self>, client: kube::Client) -> JoinHandle<()> {
        self.spawn_namespaced_watcher(client, "pods", |pod: &Pod, cluster, cols| {
            project_pod_with_columns(pod, cluster, cols)
        })
    }

    /// Spawns a background task watching cluster-wide static resources of type `K`
    /// and projecting changes into this store.
    pub(crate) fn spawn_typed_watcher<K, F>(
        self: &Arc<Self>,
        client: kube::Client,
        resource_label: &'static str,
        project_fn: F,
    ) -> JoinHandle<()>
    where
        K: Resource<DynamicType = ()>
            + serde::de::DeserializeOwned
            + Clone
            + std::fmt::Debug
            + Send
            + 'static,
        F: Fn(&K, &str, &CustomColumns) -> Row + Send + Sync + 'static,
    {
        let api = kube::Api::<K>::all(client);
        self.spawn_watcher_from_api(api, resource_label.to_owned(), None, project_fn)
    }

    /// Like [`Self::spawn_typed_watcher`] for a namespaced kind: when the
    /// cluster-wide list/watch is forbidden, the store falls back to one watcher
    /// per accessible namespace (05-01, see [`crate::ns_fallback`]).
    pub(crate) fn spawn_namespaced_watcher<K, F>(
        self: &Arc<Self>,
        client: kube::Client,
        resource_label: &'static str,
        project_fn: F,
    ) -> JoinHandle<()>
    where
        K: Resource<DynamicType = (), Scope = k8s_openapi::NamespaceResourceScope>
            + serde::de::DeserializeOwned
            + Clone
            + std::fmt::Debug
            + Send
            + 'static,
        F: Fn(&K, &str, &CustomColumns) -> Row + Send + Sync + 'static,
    {
        let api = kube::Api::<K>::all(client.clone());
        let ns_client = client.clone();
        let fallback = NamespaceFallback {
            client,
            group: K::group(&()).into_owned(),
            resource: K::plural(&()).into_owned(),
            api_for: Arc::new(move |ns: &str| kube::Api::<K>::namespaced(ns_client.clone(), ns)),
        };
        self.spawn_watcher_from_api(api, resource_label.to_owned(), Some(fallback), project_fn)
    }

    /// Spawns a background task watching an arbitrary [`kube::Api`] and projecting
    /// changes into this store.
    pub(crate) fn spawn_watcher_from_api<K, F>(
        self: &Arc<Self>,
        api: kube::Api<K>,
        resource_label: String,
        fallback: Option<NamespaceFallback<K>>,
        project_fn: F,
    ) -> JoinHandle<()>
    where
        K: Resource + serde::de::DeserializeOwned + Clone + std::fmt::Debug + Send + 'static,
        F: Fn(&K, &str, &CustomColumns) -> Row + Send + Sync + 'static,
    {
        let store = Arc::clone(self);
        // A new watcher first tries the cluster-wide watch again.
        store.leave_namespace_scope();
        let mut resync_rx = store.resync_tx.subscribe();
        let project = Arc::new(project_fn);
        tokio::spawn(async move {
            let mut last_logged_error: Option<String> = None;
            let mut fallback_retry_at: Option<Instant> = None;
            let mut enter_fallback: Option<FallbackPlan> = None;
            loop {
                if let (Some(plan), Some(fb)) = (enter_fallback.take(), fallback.as_ref()) {
                    tracing::info!(
                        cluster = %store.cluster,
                        kind = %store.kind,
                        namespaces = plan.allowed.len(),
                        "cluster-wide watch forbidden; watching namespaces one by one"
                    );
                    match store
                        .run_namespace_fallback(
                            fb,
                            &plan,
                            &project,
                            &resource_label,
                            &mut resync_rx,
                        )
                        .await
                    {
                        FallbackEnd::Resync => {
                            fallback_retry_at = None;
                            store.leave_namespace_scope();
                            continue;
                        }
                        FallbackEnd::Closed => return,
                    }
                }
                let mut stream =
                    watch_with_backoff(watcher::watcher(api.clone(), watcher::Config::default()));
                // Rows of the initial list, projected as each object arrives so
                // full objects are never buffered. A first list is published in
                // batches (see `InitStaging::publish_due`), a re-list at InitDone.
                let mut staging = InitStaging::default();
                // Publishes a staged batch even if the next list page is slow;
                // re-armed only when the due time changes.
                let progress_timer = tokio::time::sleep(Duration::ZERO);
                tokio::pin!(progress_timer);
                let mut armed_at: Option<Instant> = None;
                loop {
                    let progress_at = if store.has_synced() {
                        None
                    } else {
                        staging.next_publish_at()
                    };
                    if progress_at != armed_at {
                        if let Some(at) = progress_at {
                            progress_timer
                                .as_mut()
                                .reset(tokio::time::Instant::from_std(at));
                        }
                        armed_at = progress_at;
                    }
                    let item = tokio::select! {
                        changed = resync_rx.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            break;
                        }
                        () = progress_timer.as_mut(), if progress_at.is_some() => {
                            store.publish_progress(&mut staging, false);
                            continue;
                        }
                        item = stream.next() => item,
                    };
                    let Some(item) = item else {
                        return;
                    };
                    match item {
                        Ok(Event::Apply(obj)) => {
                            last_logged_error = None;
                            let row = {
                                let guard =
                                    store.data.read().unwrap_or_else(PoisonError::into_inner);
                                project(&obj, &store.cluster, &guard.columns)
                            };
                            store.upsert_row(row);
                            store.set_access(StoreAccessState::Ready);
                        }
                        Ok(Event::Delete(obj)) => {
                            last_logged_error = None;
                            if let Some(uid) = obj.meta().uid.as_deref() {
                                store.remove_uid(uid);
                            }
                            store.set_access(StoreAccessState::Ready);
                        }
                        Ok(Event::Init) => {
                            staging.clear();
                        }
                        Ok(Event::InitApply(obj)) => {
                            let cols = store.custom_columns();
                            let row = project(&obj, &store.cluster, &cols);
                            drop(obj);
                            store.init_apply(&mut staging, row, &cols, Instant::now());
                        }
                        Ok(Event::InitDone) => {
                            last_logged_error = None;
                            store.init_done(&mut staging);
                        }
                        Err(err) => {
                            if is_unauthorized_watcher_error(&err)
                                && let Some(invalidator) = store.client_invalidator()
                            {
                                invalidator(&store.cluster);
                            }
                            let sanitized = sanitize_watcher_error(&err);
                            let new_access = watch_error_state(&err, &resource_label, None);
                            if let Some(StoreAccessState::Forbidden {
                                namespace: None, ..
                            }) = &new_access
                                && let Some(fb) = fallback.as_ref()
                                && fallback_retry_at.is_none_or(|at| Instant::now() >= at)
                            {
                                match crate::ns_fallback::plan(
                                    &fb.client,
                                    &fb.group,
                                    &fb.resource,
                                    store.configured_namespaces(),
                                )
                                .await
                                {
                                    Ok(plan) if !plan.allowed.is_empty() => {
                                        enter_fallback = Some(plan);
                                        break;
                                    }
                                    Ok(plan) => {
                                        tracing::info!(
                                            cluster = %store.cluster,
                                            kind = %store.kind,
                                            denied = plan.denied.len(),
                                            "cluster-wide watch forbidden and no namespace is accessible"
                                        );
                                        fallback_retry_at = Some(Instant::now() + FALLBACK_RETRY);
                                    }
                                    Err(review_err) => {
                                        tracing::warn!(
                                            cluster = %store.cluster,
                                            kind = %store.kind,
                                            error = %sanitize_kube_error(&review_err),
                                            "cannot detect accessible namespaces"
                                        );
                                        fallback_retry_at = Some(Instant::now() + FALLBACK_RETRY);
                                    }
                                }
                            }
                            let is_change = match &new_access {
                                Some(state) => *state != store.access(),
                                None => last_logged_error.as_deref() != Some(&sanitized),
                            };
                            if is_change {
                                tracing::warn!(
                                    cluster = %store.cluster,
                                    kind = %store.kind,
                                    error = %sanitized,
                                    "watch stream error"
                                );
                                last_logged_error = Some(sanitized);
                            } else {
                                tracing::debug!(
                                    cluster = %store.cluster,
                                    kind = %store.kind,
                                    error = %sanitized,
                                    "watch stream error (repeated)"
                                );
                            }
                            if let Some(state) = new_access {
                                store.list_failed(&mut staging, state);
                            }
                        }
                    }
                }
            }
        })
    }

    /// Feeds the store from one watcher per namespace of `plan` (05-01), merged
    /// into one stream. Each namespace's list replaces that namespace's rows when
    /// it completes; the store is Ready once every namespace is.
    async fn run_namespace_fallback<K, F>(
        self: &Arc<Self>,
        fallback: &NamespaceFallback<K>,
        plan: &FallbackPlan,
        project: &Arc<F>,
        resource_label: &str,
        resync_rx: &mut watch::Receiver<u64>,
    ) -> FallbackEnd
    where
        K: Resource + serde::de::DeserializeOwned + Clone + std::fmt::Debug + Send + 'static,
        F: Fn(&K, &str, &CustomColumns) -> Row + Send + Sync + 'static,
    {
        self.enter_namespace_scope(plan);
        let streams = plan.allowed.iter().map(|ns| {
            let tag = ns.clone();
            watch_with_backoff(watcher::watcher(
                (fallback.api_for)(ns),
                watcher::Config::default(),
            ))
            .map(move |item| (tag.clone(), item))
            .boxed()
        });
        let mut merged = futures::stream::select_all(streams);
        let mut staging: HashMap<String, InitStaging> = HashMap::new();
        let mut last_logged: HashMap<String, String> = HashMap::new();
        loop {
            let item = tokio::select! {
                changed = resync_rx.changed() => {
                    return if changed.is_ok() { FallbackEnd::Resync } else { FallbackEnd::Closed };
                }
                item = merged.next() => item,
            };
            let Some((ns, item)) = item else {
                return FallbackEnd::Closed;
            };
            match item {
                Ok(Event::Apply(obj)) => {
                    let row = {
                        let guard = self.data.read().unwrap_or_else(PoisonError::into_inner);
                        project(&obj, &self.cluster, &guard.columns)
                    };
                    self.upsert_row(row);
                    self.set_namespace_state(&ns, StoreAccessState::Ready);
                }
                Ok(Event::Delete(obj)) => {
                    if let Some(uid) = obj.meta().uid.as_deref() {
                        self.remove_uid(uid);
                    }
                    self.set_namespace_state(&ns, StoreAccessState::Ready);
                }
                Ok(Event::Init) => staging.entry(ns).or_default().clear(),
                Ok(Event::InitApply(obj)) => {
                    let cols = self.custom_columns();
                    let row = project(&obj, &self.cluster, &cols);
                    staging.entry(ns).or_default().stage(row, &cols);
                }
                Ok(Event::InitDone) => {
                    last_logged.remove(&ns);
                    let (rows, cols) = staging.entry(ns.clone()).or_default().take();
                    self.replace_namespace_rows(&ns, rows, &cols);
                    self.set_namespace_state(&ns, StoreAccessState::Ready);
                }
                Err(err) => {
                    if is_unauthorized_watcher_error(&err)
                        && let Some(invalidator) = self.client_invalidator()
                    {
                        invalidator(&self.cluster);
                    }
                    let sanitized = sanitize_watcher_error(&err);
                    if last_logged.get(&ns) != Some(&sanitized) {
                        tracing::warn!(
                            cluster = %self.cluster,
                            kind = %self.kind,
                            namespace = %ns,
                            error = %sanitized,
                            "namespace watch stream error"
                        );
                        last_logged.insert(ns.clone(), sanitized);
                    }
                    if let Some(state) = watch_error_state(&err, resource_label, Some(&ns)) {
                        if let Some(staged) = staging.get_mut(&ns) {
                            staged.clear();
                        }
                        self.set_namespace_state(&ns, state);
                    }
                }
            }
        }
    }

    fn bump_version(&self) -> u32 {
        self.version.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }
}

/// Access state of a store fed by per-namespace watchers: Ready when every
/// namespace is; loading (or reconnecting, once synced) while any namespace is
/// still listing; otherwise the first namespace's failure.
fn combined_namespace_access(
    namespaces: &[NamespaceWatch],
    synced: bool,
    rows: usize,
) -> StoreAccessState {
    if namespaces
        .iter()
        .all(|n| n.access == StoreAccessState::Ready)
    {
        return StoreAccessState::Ready;
    }
    if namespaces.iter().any(|n| n.access.is_syncing()) {
        return if synced {
            StoreAccessState::Reconnecting
        } else {
            StoreAccessState::Loading {
                rows_so_far: u32::try_from(rows).unwrap_or(u32::MAX),
            }
        };
    }
    namespaces
        .iter()
        .find(|n| n.access != StoreAccessState::Ready)
        .map_or(StoreAccessState::Ready, |n| n.access.clone())
}

/// For every column of `to`, the index of the same path in `from`, or `None`
/// when `to` evaluates a path `from` has not projected.
fn column_mapping(from: &CustomColumns, to: &CustomColumns) -> Vec<Option<usize>> {
    to.paths()
        .iter()
        .map(|p| from.position_of_expr(p.expr()))
        .collect()
}

/// Rewrites `row.custom` to the layout described by `mapping` (see
/// [`column_mapping`]); columns without a previous value become empty.
fn remap_custom(row: &mut Row, mapping: &[Option<usize>]) {
    let old = std::mem::take(&mut row.custom);
    row.custom = mapping
        .iter()
        .map(|m| {
            m.and_then(|i| old.get(i).cloned())
                .unwrap_or_else(|| Cell::Text(String::new()))
        })
        .collect();
}

/// Rows staged before the first progressive publish of a first list, about one
/// API page (kube's default list page size is 500).
const FIRST_PUBLISH_ROWS: usize = 500;

/// Longest wait after the first staged row before the first progressive publish.
const FIRST_PUBLISH_DELAY: Duration = Duration::from_millis(100);

/// Minimum interval between later progressive publishes of a first list.
const PROGRESS_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

/// Rows of an in-progress initial list (between [`Event::Init`] and
/// [`Event::InitDone`]).
///
/// Objects are projected on [`Event::InitApply`] and dropped, so only compact
/// rows are held while the list is streamed. Rows are keyed by uid (the latest
/// one wins) and all share the column layout in `columns`: if the store's
/// columns change mid-list without a re-list (a removal or reorder), already
/// staged rows are remapped to the new layout.
///
/// For a store's first list, staged rows are moved into the store in batches
/// ([`ResourceStore::publish_progress`]), so `rows` only holds the rows since
/// the last publish and `stale` tracks store rows this list has not re-listed.
#[derive(Default)]
struct InitStaging {
    rows: BTreeMap<String, Row>,
    columns: Arc<CustomColumns>,
    /// Store rows present when this list attempt first published and not seen
    /// in it since; removed at `InitDone`. `None` until the first publish.
    stale: Option<HashSet<String>>,
    /// When the first row since the last publish was staged.
    first_staged_at: Option<Instant>,
    /// When rows of this list attempt were last published.
    last_published_at: Option<Instant>,
}

impl InitStaging {
    /// Drops every staged row ([`Event::Init`]: a new list starts).
    fn clear(&mut self) {
        self.rows.clear();
        self.stale = None;
        self.first_staged_at = None;
        self.last_published_at = None;
    }

    /// Stages `row`, which was projected with `columns`.
    fn stage(&mut self, row: Row, columns: &Arc<CustomColumns>) {
        if !Arc::ptr_eq(&self.columns, columns) {
            if *self.columns != **columns && !self.rows.is_empty() {
                let mapping = column_mapping(&self.columns, columns);
                for staged in self.rows.values_mut() {
                    remap_custom(staged, &mapping);
                }
            }
            self.columns = Arc::clone(columns);
        }
        if self.first_staged_at.is_none() {
            self.first_staged_at = Some(Instant::now());
        }
        self.rows.insert(row.uid.clone(), row);
    }

    /// Whether the staged rows of a first list should be published at `now`:
    /// the first batch after about one page or [`FIRST_PUBLISH_DELAY`], later
    /// batches at most every [`PROGRESS_PUBLISH_INTERVAL`].
    fn publish_due(&self, now: Instant) -> bool {
        if self.last_published_at.is_none() && self.rows.len() >= FIRST_PUBLISH_ROWS {
            return true;
        }
        self.next_publish_at().is_some_and(|at| now >= at)
    }

    /// When the staged rows become due by time alone; `None` when nothing is staged.
    fn next_publish_at(&self) -> Option<Instant> {
        let first_staged = self.first_staged_at?;
        Some(match self.last_published_at {
            None => first_staged + FIRST_PUBLISH_DELAY,
            Some(last) => (last + PROGRESS_PUBLISH_INTERVAL).max(first_staged),
        })
    }

    /// Drops rows staged since the last publish and disarms the publish timer
    /// (a first list failed; the next attempt starts with [`Event::Init`]).
    fn discard_unpublished(&mut self) {
        self.rows.clear();
        self.first_staged_at = None;
    }

    /// Records a progressive publish of everything staged so far.
    fn mark_published(&mut self) {
        self.first_staged_at = None;
        self.last_published_at = Some(Instant::now());
    }

    /// Takes the staged rows and their column layout, leaving staging empty.
    fn take(&mut self) -> (BTreeMap<String, Row>, Arc<CustomColumns>) {
        (std::mem::take(&mut self.rows), Arc::clone(&self.columns))
    }
}

/// Returns `true` if `err` represents an HTTP 401 Unauthorized API error.
pub(crate) fn is_unauthorized_watcher_error(err: &watcher::Error) -> bool {
    match err {
        watcher::Error::InitialListFailed(kube::Error::Api(status))
        | watcher::Error::WatchStartFailed(kube::Error::Api(status))
        | watcher::Error::WatchFailed(kube::Error::Api(status))
        | watcher::Error::WatchError(status) => {
            status.code == 401 || status.reason == "Unauthorized"
        }
        _ => false,
    }
}

/// Access state to report for a watcher error, or `None` for transient errors the
/// watcher recovers from by itself (dropped watch connection, expired resource version).
fn watch_error_state(
    err: &watcher::Error,
    resource: &str,
    namespace: Option<&str>,
) -> Option<StoreAccessState> {
    if is_unauthorized_watcher_error(err) {
        let msg = sanitize_watcher_error(err);
        if let Some(auth_req) = crate::diagnostics::detect_auth_error(&msg) {
            return Some(StoreAccessState::AuthExpired {
                provider: auth_req.provider,
                hint: format!("Run '{}' to re-authenticate", auth_req.login_command),
                login_command: auth_req.login_command,
                device_code: auth_req.device_code,
                partial_rows: None,
            });
        }
        return Some(StoreAccessState::Error {
            message: msg,
            partial_rows: None,
        });
    }
    match err {
        watcher::Error::InitialListFailed(e) | watcher::Error::WatchStartFailed(e) => Some(
            classify_kube_error(e, resource, namespace).unwrap_or_else(|| {
                let msg = sanitize_kube_error(e);
                if let Some(auth_req) = crate::diagnostics::detect_auth_error(&msg) {
                    StoreAccessState::AuthExpired {
                        provider: auth_req.provider,
                        hint: format!("Run '{}' to re-authenticate", auth_req.login_command),
                        login_command: auth_req.login_command,
                        device_code: auth_req.device_code,
                        partial_rows: None,
                    }
                } else {
                    StoreAccessState::Error {
                        message: msg,
                        partial_rows: None,
                    }
                }
            }),
        ),
        watcher::Error::WatchFailed(e) => classify_kube_error(e, resource, namespace),
        watcher::Error::WatchError(status) => {
            classify_kube_error(&kube::Error::Api(status.clone()), resource, namespace)
        }
        watcher::Error::NoResourceVersion => Some(StoreAccessState::Error {
            message: "resource version not found in watch stream".to_owned(),
            partial_rows: None,
        }),
    }
}

fn with_custom_columns<T: serde::Serialize>(mut row: Row, obj: &T, columns: &CustomColumns) -> Row {
    if !columns.is_empty()
        && let Ok(val) = serde_json::to_value(obj)
    {
        row.custom = crate::columns::evaluate_custom_cells(&val, columns.paths());
    }
    row
}

fn with_custom_columns_secret(mut row: Row, secret: &Secret, columns: &CustomColumns) -> Row {
    if !columns.is_empty() {
        let mut sanitized = secret.clone();
        sanitized.data = None;
        sanitized.string_data = None;
        if let Ok(val) = serde_json::to_value(&sanitized) {
            row.custom = crate::columns::evaluate_custom_cells(&val, columns.paths());
        }
    }
    row
}

fn extract_metadata(
    meta: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> (
    String,
    String,
    Option<String>,
    Vec<(String, String)>,
    String,
) {
    let uid = meta.uid.clone().unwrap_or_default();
    let name = meta.name.clone().unwrap_or_default();
    let namespace = meta.namespace.clone();
    let mut labels: Vec<(String, String)> = meta
        .labels
        .as_ref()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    labels.sort();
    let created = meta
        .creation_timestamp
        .as_ref()
        .map(|ts| ts.0.to_string())
        .unwrap_or_default();
    (uid, name, namespace, labels, created)
}

fn project_stateful_set(ss: &StatefulSet, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_metadata(&ss.metadata);

    let desired_i32 = ss.spec.replicas.unwrap_or(1).max(0);
    let ready_i32 = ss
        .status
        .as_ref()
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0)
        .max(0);

    let desired = u32::try_from(desired_i32).unwrap_or(0);
    let ready = u32::try_from(ready_i32).unwrap_or(0);

    let (status_reason, status_phase) = if ss.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if ready == desired {
        ("Ready".to_owned(), RowStatus::Healthy)
    } else {
        ("Progressing".to_owned(), RowStatus::Pending)
    };

    let cells = vec![
        Cell::Ready {
            ready,
            total: desired,
        },
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "StatefulSet".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

fn project_daemon_set(ds: &DaemonSet, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_metadata(&ds.metadata);

    let desired_i32 = ds
        .status
        .as_ref()
        .map_or(0, |s| s.desired_number_scheduled.max(0));
    let ready_i32 = ds.status.as_ref().map_or(0, |s| s.number_ready.max(0));

    let desired = u32::try_from(desired_i32).unwrap_or(0);
    let ready = u32::try_from(ready_i32).unwrap_or(0);

    let (status_reason, status_phase) = if ds.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if ready == desired {
        ("Ready".to_owned(), RowStatus::Healthy)
    } else {
        ("Progressing".to_owned(), RowStatus::Pending)
    };

    let cells = vec![
        Cell::Ready {
            ready,
            total: desired,
        },
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "DaemonSet".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

fn project_cron_job(cj: &CronJob, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_metadata(&cj.metadata);

    let schedule = cj.spec.schedule.clone();
    let suspend = cj.spec.suspend.unwrap_or(false);

    let (status_reason, status_phase) = if cj.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if suspend {
        ("Suspended".to_owned(), RowStatus::Degraded)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(schedule),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "CronJob".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

fn project_ingress(ing: &Ingress, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_metadata(&ing.metadata);

    let class_name = ing
        .spec
        .as_ref()
        .and_then(|s| s.ingress_class_name.clone())
        .unwrap_or_default();

    let (status_reason, status_phase) = if ing.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(class_name),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Ingress".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

fn project_pvc(pvc: &PersistentVolumeClaim, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_metadata(&pvc.metadata);

    let phase_str = pvc
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("Unknown");
    let status_phase = match phase_str {
        "Bound" => RowStatus::Healthy,
        "Pending" => RowStatus::Pending,
        "Lost" => RowStatus::Failed,
        _ => RowStatus::Unknown,
    };
    let (status_reason, status_phase) = if pvc.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        (phase_str.to_owned(), status_phase)
    };

    let volume_name = pvc
        .spec
        .as_ref()
        .and_then(|s| s.volume_name.clone())
        .unwrap_or_default();
    let storage_class = pvc
        .spec
        .as_ref()
        .and_then(|s| s.storage_class_name.clone())
        .unwrap_or_default();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(volume_name),
        Cell::Text(storage_class),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "PersistentVolumeClaim".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

fn find_matching_resource(resources: &[ApiResource], kind: &str) -> Option<ApiResource> {
    for ar in resources {
        if ar.kind == kind {
            return Some(ar.clone());
        }
    }
    for ar in resources {
        if ar.kind.eq_ignore_ascii_case(kind) || ar.plural.eq_ignore_ascii_case(kind) {
            return Some(ar.clone());
        }
    }
    None
}

fn find_matching_resource_with_version(
    resources: &[ApiResource],
    api_version: Option<&str>,
    kind: &str,
) -> Option<ApiResource> {
    if let Some(version) = api_version {
        for ar in resources {
            if ar.kind == kind && ar.api_version == version {
                return Some(ar.clone());
            }
        }
        for ar in resources {
            if (ar.kind.eq_ignore_ascii_case(kind) || ar.plural.eq_ignore_ascii_case(kind))
                && ar.api_version.eq_ignore_ascii_case(version)
            {
                return Some(ar.clone());
            }
        }
    }
    find_matching_resource(resources, kind)
}

/// Sanitizes a [`kube::Error`] to prevent leaking credentials (tokens, secrets,
/// auth headers, or exec plugin environments/commands) into user-facing access states or logs.
pub(crate) fn sanitize_kube_error(err: &kube::Error) -> String {
    match err {
        kube::Error::Auth(auth_err) => match auth_err {
            AuthError::AuthExecRun { out, .. } => {
                format!(
                    "authentication failed: exec credential plugin failed (exit status {})",
                    out.status
                )
            }
            AuthError::AuthExecStart(source) => {
                format!("authentication failed: unable to run exec credential plugin: {source}")
            }
            AuthError::AuthExecParse(_) => {
                "authentication failed: invalid exec credential plugin output".to_owned()
            }
            AuthError::AuthExecSerialize(_) => {
                "authentication failed: cannot serialize exec credential plugin input".to_owned()
            }
            AuthError::MissingCommand => {
                "authentication failed: exec credential plugin command not specified".to_owned()
            }
            AuthError::ExecPluginFailed => {
                "authentication failed: exec credential plugin response missing status".to_owned()
            }
            AuthError::InvalidBearerToken(_) => {
                "authentication failed: invalid bearer token header".to_owned()
            }
            AuthError::InvalidBasicAuth(_) => {
                "authentication failed: invalid basic auth header".to_owned()
            }
            AuthError::ReadTokenFile(source, path) => {
                format!(
                    "authentication failed: cannot read token file {}: {source}",
                    path.display()
                )
            }
            AuthError::ParseTokenKey(_) => {
                "authentication failed: cannot parse token key".to_owned()
            }
            AuthError::UnrefreshableTokenResponse => {
                "authentication failed: token response is not refreshable".to_owned()
            }
            AuthError::MalformedTokenExpirationDate(_) => {
                "authentication failed: malformed token expiration date".to_owned()
            }
            AuthError::ExecMissingClusterInfo => {
                "authentication failed: exec plugin requires cluster info".to_owned()
            }
            AuthError::NoValidNativeRootCA(source) => {
                format!("authentication failed: no valid native root CA: {source}")
            }
            _ => "authentication failed".to_owned(),
        },
        kube::Error::Api(status) => {
            if !status.message.is_empty() {
                format!("Kubernetes API error ({}): {}", status.code, status.message)
            } else if !status.reason.is_empty() {
                format!("Kubernetes API error ({}): {}", status.code, status.reason)
            } else {
                format!("Kubernetes API error (code {})", status.code)
            }
        }
        kube::Error::HyperError(source) => format!("HTTP client error: {source}"),
        kube::Error::Service(source) => format!("service error: {source}"),
        kube::Error::ProxyProtocolUnsupported { .. } => {
            "configured proxy uses an unsupported protocol".to_owned()
        }
        kube::Error::ProxyProtocolDisabled {
            protocol_feature, ..
        } => {
            format!("configured proxy requires disabled feature {protocol_feature}")
        }
        kube::Error::FromUtf8(source) => format!("UTF-8 error: {source}"),
        kube::Error::LinesCodecMaxLineLengthExceeded => "max line length exceeded".to_owned(),
        kube::Error::ReadEvents(source) => format!("event stream read error: {source}"),
        kube::Error::HttpError(source) => format!("HTTP protocol error: {source}"),
        kube::Error::SerdeError(source) => format!("deserialization error: {source}"),
        kube::Error::BuildRequest(source) => format!("request build error: {source}"),
        kube::Error::InferConfig(source) => format!("configuration error: {source}"),
        kube::Error::Discovery(source) => format!("API discovery error: {source}"),
        kube::Error::RustlsTls(source) => format!("TLS error: {source}"),
        kube::Error::TlsRequired => "TLS required but no TLS stack selected".to_owned(),
        kube::Error::InferKubeconfig(source) => format!("kubeconfig error: {source}"),
        _ => "kubernetes client error".to_owned(),
    }
}

/// Sanitizes a core [`Error`].
pub(crate) fn sanitize_error(err: &Error) -> String {
    match err {
        Error::Kube(kube_err) => sanitize_kube_error(kube_err),
        other => other.to_string(),
    }
}

/// Sanitizes a [`watcher::Error`].
pub(crate) fn sanitize_watcher_error(err: &watcher::Error) -> String {
    match err {
        watcher::Error::InitialListFailed(e) => {
            format!("initial list failed: {}", sanitize_kube_error(e))
        }
        watcher::Error::WatchStartFailed(e) => {
            format!("watch start failed: {}", sanitize_kube_error(e))
        }
        watcher::Error::WatchFailed(e) => {
            format!("watch failed: {}", sanitize_kube_error(e))
        }
        watcher::Error::WatchError(status) => {
            if !status.message.is_empty() {
                format!("watch error: {}", status.message)
            } else {
                format!("watch error (code {})", status.code)
            }
        }
        watcher::Error::NoResourceVersion => {
            "resource version not found in watch stream".to_owned()
        }
    }
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_FACTOR: f64 = 2.0;
const BACKOFF_MAX: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x853c_49e6_748f_ea9b
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Float in range [0.8, 1.2]
    fn next_jitter(&mut self) -> f64 {
        let frac = (self.next_u64() % 1000) as f64 / 1000.0;
        0.8 + 0.4 * frac
    }
}

fn current_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x853c_49e6_748f_ea9b);
    if nanos == 0 {
        0x853c_49e6_748f_ea9b
    } else {
        nanos
    }
}

#[derive(Debug)]
pub(crate) struct WatchBackoffPolicy {
    current: Duration,
    initial: Duration,
    factor: f64,
    max: Duration,
    rng: XorShift64,
}

impl Default for WatchBackoffPolicy {
    fn default() -> Self {
        Self::new(BACKOFF_INITIAL, BACKOFF_FACTOR, BACKOFF_MAX, current_seed())
    }
}

impl WatchBackoffPolicy {
    pub(crate) fn new(initial: Duration, factor: f64, max: Duration, seed: u64) -> Self {
        Self {
            current: initial,
            initial,
            factor,
            max,
            rng: XorShift64::new(seed),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.current = self.initial;
    }

    pub(crate) fn next_delay(&mut self) -> Duration {
        let base = self.current;
        let next_secs = (base.as_secs_f64() * self.factor).min(self.max.as_secs_f64());
        self.current = Duration::from_secs_f64(next_secs);

        let jitter = self.rng.next_jitter();
        let jittered_secs = (base.as_secs_f64() * jitter).min(self.max.as_secs_f64());
        Duration::from_secs_f64(jittered_secs)
    }
}

/// A wrapper stream that applies exponential backoff with jitter after watch errors,
/// resetting ONLY on healthy watch events ([`Event::InitDone`], [`Event::Apply`], [`Event::Delete`]).
///
/// It does NOT reset on [`Event::Init`] or [`Event::InitApply`], avoiding the hot loop where
/// a failed initial list repeatedly yields [`Event::Init`] and resets backoff to 1s.
pub(crate) struct WatchBackoffStream<K> {
    stream: BoxStream<'static, Result<Event<K>, watcher::Error>>,
    policy: WatchBackoffPolicy,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<K: 'static> WatchBackoffStream<K> {
    pub(crate) fn new(
        stream: impl Stream<Item = Result<Event<K>, watcher::Error>> + Send + 'static,
    ) -> Self {
        Self {
            stream: stream.boxed(),
            policy: WatchBackoffPolicy::default(),
            sleep: None,
        }
    }
}

impl<K> Stream for WatchBackoffStream<K> {
    type Item = Result<Event<K>, watcher::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();

        if let Some(sleep) = &mut this.sleep {
            match sleep.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    this.sleep = None;
                }
                Poll::Pending => return Poll::Pending,
            }
        }

        let item = this.stream.as_mut().poll_next(cx);
        match &item {
            Poll::Ready(Some(Ok(event))) => {
                // Reset backoff ONLY on healthy steady-state events or successful list completion.
                // Do NOT reset on Event::Init or Event::InitApply!
                if matches!(event, Event::InitDone | Event::Apply(_) | Event::Delete(_)) {
                    this.policy.reset();
                }
            }
            Poll::Ready(Some(Err(_))) => {
                let delay = this.policy.next_delay();
                this.sleep = Some(Box::pin(tokio::time::sleep(delay)));
            }
            Poll::Ready(None) | Poll::Pending => {}
        }
        item
    }
}

pub(crate) fn watch_with_backoff<K: 'static>(
    stream: impl Stream<Item = Result<Event<K>, watcher::Error>> + Send + 'static,
) -> WatchBackoffStream<K> {
    WatchBackoffStream::new(stream)
}

/// RAII lease guard that releases acquired [`ResourceStore`] references on drop.
struct SubscriptionLease {
    stores: Vec<Arc<ResourceStore>>,
}

impl Drop for SubscriptionLease {
    fn drop(&mut self) {
        for store in &self.stores {
            store.release();
        }
    }
}

/// Kinds aggregated into application summaries (`P2-05`).
pub const APP_KINDS: &[&str] = &[
    "Pod",
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "Service",
    "Job",
];

/// Kinds required for the cluster overview page (02-08a).
pub const OVERVIEW_KINDS: &[&str] = &["Node", "Pod", "Deployment", "StatefulSet", "Event"];

/// Manages lazy per-context [`kube::Client`] connections, reference-counted
/// [`ResourceStore`]s, multi-cluster queries, and batched subscriptions.
#[derive(Clone)]
pub struct ClusterManager {
    source: KubeconfigSource,
    idle_timeout: Duration,
    /// One cell per context: concurrent first calls share a single client build
    /// (single-flight). A failed build leaves the cell empty, so a later call retries.
    clients: ClientMap,
    /// Number of client builds attempted (diagnostics and tests).
    client_builds: Arc<AtomicU32>,
    stores: StoreMap,
    custom_columns: ColumnMap,
    discovery_cache: DiscoveryMap,
    /// Per-context namespaces to watch when a cluster-wide watch is forbidden
    /// (05-01); see [`Self::set_watch_namespaces`].
    watch_namespaces: WatchNamespacesMap,
    namespaced_resources: NamespacedMap,
    next_sub_id: Arc<AtomicU32>,
    /// Per-context clients of the metrics pollers (02-01), built without kube's
    /// default retry layer: that layer retries 503 up to 15 times with
    /// exponential backoff, which turns "metrics-server not running" (an
    /// immediate 503 from the aggregated API) into a hang instead of an honest
    /// `NotInstalled`. The pollers have their own 15 s cadence.
    metrics_clients: Arc<Mutex<HashMap<String, kube::Client>>>,
    /// `true` while the app is in the foreground (default; desktop never
    /// changes it). See [`Self::set_foreground`].
    foreground: Arc<watch::Sender<bool>>,
    /// Serializes `set_foreground` calls.
    foreground_mutex: Arc<tokio::sync::Mutex<()>>,
}

impl ClusterManager {
    /// Creates a new [`ClusterManager`] using [`DEFAULT_IDLE_TIMEOUT`].
    #[must_use]
    pub fn new(source: KubeconfigSource) -> Self {
        Self::with_idle_timeout(source, DEFAULT_IDLE_TIMEOUT)
    }

    /// Creates a new [`ClusterManager`] with a custom store `idle_timeout`.
    #[must_use]
    pub fn with_idle_timeout(source: KubeconfigSource, idle_timeout: Duration) -> Self {
        Self {
            source,
            idle_timeout,
            clients: Arc::new(Mutex::new(HashMap::new())),
            client_builds: Arc::new(AtomicU32::new(0)),
            metrics_clients: Arc::new(Mutex::new(HashMap::new())),
            stores: Arc::new(RwLock::new(HashMap::new())),
            custom_columns: Arc::new(RwLock::new(HashMap::new())),
            discovery_cache: Arc::new(RwLock::new(HashMap::new())),
            watch_namespaces: Arc::new(RwLock::new(HashMap::new())),
            namespaced_resources: Arc::new(RwLock::new(HashMap::new())),
            next_sub_id: Arc::new(AtomicU32::new(1)),
            foreground: Arc::new(watch::channel(true).0),
            foreground_mutex: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// `true` unless the app was put in the background with [`Self::set_foreground`].
    #[must_use]
    pub fn is_foreground(&self) -> bool {
        *self.foreground.borrow()
    }

    /// Moves the manager to the foreground (`true`) or background (`false`), for
    /// mobile apps whose process keeps running behind other apps (03-04a).
    /// Idempotent; returns `true` when the state changed.
    ///
    /// Background: every store is paused at once: watchers and metrics pollers are
    /// aborted (no reconnect, no backoff) and cannot start again, while stores,
    /// rows and subscriptions stay, so the UI keeps the last-known data and
    /// [`StoreAccess::paused`] says so. Overview subscriptions stop their periodic
    /// refresh.
    ///
    /// Foreground: stores are unpaused; those with subscribers are re-listed
    /// through the normal start path (access goes `Reconnecting` → `Ready`, and the
    /// version is bumped so pages refresh) and get their metrics poller back.
    /// Stores without subscribers stay stopped until the next query asks for them.
    pub async fn set_foreground(self: &Arc<Self>, foreground: bool) -> bool {
        let _guard = self.foreground_mutex.lock().await;

        // Set before walking the map: a store created afterwards sees the flag
        // under the map's write lock (see `get_or_create_store`).
        let changed = self.foreground.send_if_modified(|current| {
            let changed = *current != foreground;
            *current = foreground;
            changed
        });
        let stores: Vec<Arc<ResourceStore>> = {
            let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            guard.values().cloned().collect()
        };
        if !foreground {
            for store in &stores {
                store.set_paused(true);
            }
            if changed {
                tracing::debug!(stores = stores.len(), "paused all watchers (background)");
            }
            return changed;
        }
        let mut resume = Vec::new();
        for store in &stores {
            store.set_paused(false);
            if store.ref_count() > 0 && !store.watcher_active() {
                resume.push((store.cluster.clone(), store.kind.clone()));
            }
        }
        if changed {
            tracing::debug!(resumed = resume.len(), "resumed watchers (foreground)");
        }
        if !resume.is_empty()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let this = Arc::clone(self);
            runtime.spawn(async move {
                let tasks = resume.iter().map(|(cluster, kind)| {
                    let this = &this;
                    async move {
                        if let Err(err) = this.ensure_watching(cluster, kind).await {
                            tracing::warn!(cluster = %cluster, kind = %kind, error = %sanitize_error(&err), "cannot resume watch");
                        }
                    }
                });
                futures::future::join_all(tasks).await;
            });
        }
        changed
    }

    /// Waits until the manager is in the foreground (immediately if it is).
    async fn wait_foreground(&self) {
        let mut rx = self.foreground.subscribe();
        // The sender lives in `self`, so this only ends when the flag is true.
        let _ = rx.wait_for(|fg| *fg).await;
    }

    /// Returns a reference to the [`KubeconfigSource`] backing this manager.
    #[must_use]
    pub fn source(&self) -> &KubeconfigSource {
        &self.source
    }

    /// Returns the existing [`ResourceStore`] for `(cluster, kind)` or creates a new one.
    pub fn get_or_create_store(&self, cluster: &str, kind: &str) -> Arc<ResourceStore> {
        let key = (cluster.to_owned(), kind.to_owned());
        {
            let read_guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = read_guard.get(&key) {
                return Arc::clone(existing);
            }
        }
        let custom_cols = {
            let guard = self
                .custom_columns
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            guard.get(kind).cloned().unwrap_or_default()
        };
        let mut write_guard = self.stores.write().unwrap_or_else(PoisonError::into_inner);
        let clients = Arc::clone(&self.clients);
        let paused = !self.is_foreground();
        Arc::clone(write_guard.entry(key).or_insert_with(|| {
            let store = ResourceStore::with_columns(cluster, kind, custom_cols);
            if paused {
                store.set_paused(true);
            }
            store.set_client_invalidator(Arc::new(move |ctx: &str| {
                let mut guard = clients.lock().unwrap_or_else(PoisonError::into_inner);
                guard.remove(ctx);
            }));
            store.set_watch_namespaces_map(Arc::clone(&self.watch_namespaces));
            store
        }))
    }

    /// Sets the namespaces to watch one by one for `context` when a cluster-wide
    /// list/watch is forbidden (05-01; the context's `watchNamespaces` setting).
    /// Empty means "detect". When the list changes, the context's stores that
    /// already run a per-namespace fallback re-plan it.
    pub fn set_watch_namespaces(&self, context: &str, namespaces: Vec<String>) {
        let changed = {
            let mut guard = self
                .watch_namespaces
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let previous = guard.get(context).cloned().unwrap_or_default();
            if namespaces.is_empty() {
                guard.remove(context);
            } else {
                guard.insert(context.to_owned(), namespaces.clone());
            }
            previous != namespaces
        };
        if !changed {
            return;
        }
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        for ((ctx, _), store) in stores.iter() {
            if ctx == context && store.scope().is_some() {
                store.resync();
            }
        }
    }

    /// Returns the existing [`ResourceStore`] for `(cluster, kind)` if present.
    #[must_use]
    pub fn get_store(&self, cluster: &str, kind: &str) -> Option<Arc<ResourceStore>> {
        let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        guard.get(&(cluster.to_owned(), kind.to_owned())).cloned()
    }

    /// Resolves `kind` to its canonical name: built-in kinds or discovered dynamic kinds.
    pub(crate) fn canonical_resource_kind(&self, cluster: &str, kind: &str) -> Option<String> {
        if let Some(builtin) = canonical_builtin_kind(kind) {
            return Some(builtin.to_owned());
        }
        let cache = self
            .discovery_cache
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(resources) = cache.get(cluster)
            && let Some(ar) = find_matching_resource(resources, kind)
        {
            return Some(ar.kind);
        }
        None
    }

    /// Configures custom JSONPath columns for `kind` across all active and future stores.
    pub fn set_custom_columns(&self, kind: &str, cols: Vec<ColumnDef>) -> Result<()> {
        let compiled = Arc::new(CustomColumns::compile(cols)?);
        {
            let mut guard = self
                .custom_columns
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            guard.insert(kind.to_owned(), Arc::clone(&compiled));
        }
        let stores = {
            let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            guard
                .iter()
                .filter(|((_, k), _)| k == kind)
                .map(|(_, s)| Arc::clone(s))
                .collect::<Vec<_>>()
        };
        for store in stores {
            store.set_custom_columns(Arc::clone(&compiled));
        }
        Ok(())
    }

    /// Custom columns currently configured for `kind`.
    #[must_use]
    pub fn custom_columns(&self, kind: &str) -> Arc<CustomColumns> {
        let guard = self
            .custom_columns
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        guard.get(kind).cloned().unwrap_or_default()
    }

    /// Evicts stores whose reference count is zero and whose idle time has
    /// reached `self.idle_timeout`. Returns the number of evicted stores.
    ///
    /// The watcher task of every evicted store is aborted.
    pub fn evict_idle_stores(&self) -> usize {
        let mut evicted = Vec::new();
        {
            let mut guard = self.stores.write().unwrap_or_else(PoisonError::into_inner);
            guard.retain(|_, store| {
                let expired = store.is_idle_expired(self.idle_timeout);
                if expired {
                    evicted.push(Arc::clone(store));
                }
                !expired
            });
        }
        for store in &evicted {
            store.stop_watcher();
            tracing::debug!(cluster = %store.cluster, kind = %store.kind, "evicted idle store");
        }
        evicted.len()
    }

    /// Future that calls [`Self::evict_idle_stores`] every `interval` until the
    /// manager is dropped. Spawn it once on the async runtime.
    pub fn idle_reaper(
        self: &Arc<Self>,
        interval: Duration,
    ) -> impl Future<Output = ()> + Send + 'static {
        let manager: Weak<Self> = Arc::downgrade(self);
        async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(manager) = manager.upgrade() else {
                    return;
                };
                manager.evict_idle_stores();
            }
        }
    }

    pub async fn discover_api_resource_with_version(
        &self,
        cluster: &str,
        api_version: Option<&str>,
        kind: &str,
    ) -> std::result::Result<Option<ApiResource>, Error> {
        {
            let cache = self
                .discovery_cache
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(resources) = cache.get(cluster) {
                return Ok(find_matching_resource_with_version(
                    resources,
                    api_version,
                    kind,
                ));
            }
        }

        let client = self.connect_client(cluster).await?;
        let discovery = match Discovery::new(client.clone()).run_aggregated().await {
            Ok(d) => d,
            Err(_) => Discovery::new(client.clone())
                .run()
                .await
                .map_err(Error::Kube)?,
        };

        let mut all_resources = Vec::new();
        let mut namespaced = HashSet::new();
        for group in discovery.groups() {
            for (ar, caps) in group.recommended_resources() {
                if caps.scope == kube::discovery::Scope::Namespaced {
                    namespaced.insert(format!("{}/{}", ar.group, ar.plural));
                }
                all_resources.push(ar);
            }
        }
        self.namespaced_resources
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(cluster.to_owned(), namespaced);

        let found = find_matching_resource_with_version(&all_resources, api_version, kind);

        {
            let mut cache = self
                .discovery_cache
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            cache.insert(cluster.to_owned(), all_resources);
        }

        Ok(found)
    }

    pub async fn discover_api_resource(
        &self,
        cluster: &str,
        kind: &str,
    ) -> std::result::Result<Option<ApiResource>, Error> {
        self.discover_api_resource_with_version(cluster, None, kind)
            .await
    }

    fn start_builtin_watcher(
        &self,
        store: &Arc<ResourceStore>,
        client: kube::Client,
        builtin: &'static str,
    ) -> StartOutcome {
        match builtin {
            "Pod" => store.start_watcher_once(|s| s.spawn_pod_watcher(client)),
            "Deployment" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "deployments", |d: &Deployment, cl, cols| {
                    with_custom_columns(crate::projection::project_deployment(d, cl), d, cols)
                })
            }),
            "StatefulSet" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "statefulsets", |ss: &StatefulSet, cl, cols| {
                    with_custom_columns(project_stateful_set(ss, cl), ss, cols)
                })
            }),
            "DaemonSet" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "daemonsets", |ds: &DaemonSet, cl, cols| {
                    with_custom_columns(project_daemon_set(ds, cl), ds, cols)
                })
            }),
            "Job" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "jobs", |j: &Job, cl, cols| {
                    with_custom_columns(crate::projection::project_job(j, cl), j, cols)
                })
            }),
            "CronJob" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "cronjobs", |cj: &CronJob, cl, cols| {
                    with_custom_columns(project_cron_job(cj, cl), cj, cols)
                })
            }),
            "Service" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "services", |svc: &Service, cl, cols| {
                    with_custom_columns(crate::detail::project_service(svc, cl), svc, cols)
                })
            }),
            "Ingress" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "ingresses", |ing: &Ingress, cl, cols| {
                    with_custom_columns(project_ingress(ing, cl), ing, cols)
                })
            }),
            "ConfigMap" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "configmaps", |cm: &ConfigMap, cl, cols| {
                    with_custom_columns(crate::detail::project_config_map(cm, cl), cm, cols)
                })
            }),
            "Secret" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "secrets", |sec: &Secret, cl, cols| {
                    with_custom_columns_secret(crate::detail::project_secret(sec, cl), sec, cols)
                })
            }),
            "Node" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(client, "nodes", |node: &Node, cl, cols| {
                    with_custom_columns(crate::detail::project_node(node, cl), node, cols)
                })
            }),
            "Namespace" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(client, "namespaces", |ns: &Namespace, cl, cols| {
                    with_custom_columns(crate::detail::project_namespace(ns, cl), ns, cols)
                })
            }),
            "Event" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "events", |ev: &K8sEvent, cl, cols| {
                    with_custom_columns(crate::detail::project_event(ev, cl), ev, cols)
                })
            }),
            "PersistentVolumeClaim" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(
                    client,
                    "persistentvolumeclaims",
                    |pvc: &PersistentVolumeClaim, cl, cols| {
                        with_custom_columns(project_pvc(pvc, cl), pvc, cols)
                    },
                )
            }),
            "ReplicaSet" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "replicasets", |rs: &ReplicaSet, cl, cols| {
                    with_custom_columns(crate::builtin_kinds::project_replica_set(rs, cl), rs, cols)
                })
            }),
            "EndpointSlice" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(
                    client,
                    "endpointslices",
                    |eps: &EndpointSlice, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_endpoint_slice(eps, cl),
                            eps,
                            cols,
                        )
                    },
                )
            }),
            "HorizontalPodAutoscaler" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(
                    client,
                    "horizontalpodautoscalers",
                    |hpa: &HorizontalPodAutoscaler, cl, cols| {
                        with_custom_columns(crate::builtin_kinds::project_hpa(hpa, cl), hpa, cols)
                    },
                )
            }),
            "PodDisruptionBudget" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(
                    client,
                    "poddisruptionbudgets",
                    |pdb: &PodDisruptionBudget, cl, cols| {
                        with_custom_columns(crate::builtin_kinds::project_pdb(pdb, cl), pdb, cols)
                    },
                )
            }),
            "ResourceQuota" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(
                    client,
                    "resourcequotas",
                    |rq: &ResourceQuota, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_resource_quota(rq, cl),
                            rq,
                            cols,
                        )
                    },
                )
            }),
            "LimitRange" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "limitranges", |lr: &LimitRange, cl, cols| {
                    with_custom_columns(crate::builtin_kinds::project_limit_range(lr, cl), lr, cols)
                })
            }),
            "PriorityClass" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(client, "priorityclasses", |pc: &PriorityClass, cl, cols| {
                    with_custom_columns(
                        crate::builtin_kinds::project_priority_class(pc, cl),
                        pc,
                        cols,
                    )
                })
            }),
            "RuntimeClass" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(client, "runtimeclasses", |rc: &RuntimeClass, cl, cols| {
                    with_custom_columns(
                        crate::builtin_kinds::project_runtime_class(rc, cl),
                        rc,
                        cols,
                    )
                })
            }),
            "Lease" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "leases", |l: &Lease, cl, cols| {
                    with_custom_columns(crate::builtin_kinds::project_lease(l, cl), l, cols)
                })
            }),
            "PersistentVolume" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(
                    client,
                    "persistentvolumes",
                    |pv: &PersistentVolume, cl, cols| {
                        with_custom_columns(crate::builtin_kinds::project_pv(pv, cl), pv, cols)
                    },
                )
            }),
            "StorageClass" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(client, "storageclasses", |sc: &StorageClass, cl, cols| {
                    with_custom_columns(
                        crate::builtin_kinds::project_storage_class(sc, cl),
                        sc,
                        cols,
                    )
                })
            }),
            "VolumeAttachment" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(
                    client,
                    "volumeattachments",
                    |va: &VolumeAttachment, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_volume_attachment(va, cl),
                            va,
                            cols,
                        )
                    },
                )
            }),
            "ServiceAccount" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(
                    client,
                    "serviceaccounts",
                    |sa: &ServiceAccount, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_service_account(sa, cl),
                            sa,
                            cols,
                        )
                    },
                )
            }),
            "Role" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "roles", |r: &Role, cl, cols| {
                    with_custom_columns(crate::builtin_kinds::project_role(r, cl), r, cols)
                })
            }),
            "ClusterRole" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(client, "clusterroles", |cr: &ClusterRole, cl, cols| {
                    with_custom_columns(
                        crate::builtin_kinds::project_cluster_role(cr, cl),
                        cr,
                        cols,
                    )
                })
            }),
            "RoleBinding" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(client, "rolebindings", |rb: &RoleBinding, cl, cols| {
                    with_custom_columns(
                        crate::builtin_kinds::project_role_binding(rb, cl),
                        rb,
                        cols,
                    )
                })
            }),
            "ClusterRoleBinding" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(
                    client,
                    "clusterrolebindings",
                    |crb: &ClusterRoleBinding, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_cluster_role_binding(crb, cl),
                            crb,
                            cols,
                        )
                    },
                )
            }),
            "NetworkPolicy" => store.start_watcher_once(|s| {
                s.spawn_namespaced_watcher(
                    client,
                    "networkpolicies",
                    |np: &NetworkPolicy, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_network_policy(np, cl),
                            np,
                            cols,
                        )
                    },
                )
            }),
            "MutatingWebhookConfiguration" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(
                    client,
                    "mutatingwebhookconfigurations",
                    |mwc: &MutatingWebhookConfiguration, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_mutating_webhook_config(mwc, cl),
                            mwc,
                            cols,
                        )
                    },
                )
            }),
            "ValidatingWebhookConfiguration" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(
                    client,
                    "validatingwebhookconfigurations",
                    |vwc: &ValidatingWebhookConfiguration, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_validating_webhook_config(vwc, cl),
                            vwc,
                            cols,
                        )
                    },
                )
            }),
            "ValidatingAdmissionPolicy" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(
                    client,
                    "validatingadmissionpolicies",
                    |vap: &ValidatingAdmissionPolicy, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_validating_admission_policy(vap, cl),
                            vap,
                            cols,
                        )
                    },
                )
            }),
            "ValidatingAdmissionPolicyBinding" => store.start_watcher_once(|s| {
                s.spawn_typed_watcher(
                    client,
                    "validatingadmissionpolicybindings",
                    |vapb: &ValidatingAdmissionPolicyBinding, cl, cols| {
                        with_custom_columns(
                            crate::builtin_kinds::project_validating_admission_policy_binding(
                                vapb, cl,
                            ),
                            vapb,
                            cols,
                        )
                    },
                )
            }),
            _ => StartOutcome::Evicted,
        }
    }

    fn start_dynamic_watcher(
        &self,
        store: &Arc<ResourceStore>,
        client: kube::Client,
        ar: ApiResource,
    ) -> StartOutcome {
        let api = kube::Api::<DynamicObject>::all_with(client.clone(), &ar);
        let ar_kind = ar.kind.clone();
        let plural = ar.plural.clone();
        let namespaced = self
            .namespaced_resources
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&store.cluster)
            .is_some_and(|set| set.contains(&format!("{}/{}", ar.group, ar.plural)));
        let fallback = namespaced.then(|| {
            let ns_client = client.clone();
            let ns_ar = ar.clone();
            NamespaceFallback {
                client,
                group: ar.group.clone(),
                resource: ar.plural.clone(),
                api_for: Arc::new(move |ns: &str| {
                    kube::Api::<DynamicObject>::namespaced_with(ns_client.clone(), ns, &ns_ar)
                }) as NamespacedApiFn<DynamicObject>,
            }
        });
        store.start_watcher_once(move |s| {
            s.spawn_watcher_from_api(api, plural, fallback, move |obj, cl, cols| {
                with_custom_columns(
                    crate::ecosystem::project_ecosystem_object(obj, cl, &ar_kind),
                    obj,
                    cols,
                )
            })
        })
    }

    /// Makes sure the `(cluster, kind)` store is fed by a live watcher.
    ///
    /// Connects lazily ([`Self::connect_client`]) and starts the watcher once per
    /// store; later calls only restart the store's idle timer. Concurrent calls
    /// start at most one watcher. Supports all built-in kinds and discovers CRDs
    /// and other resources via [`kube::discovery::Discovery`].
    ///
    /// A connection error is stored as [`StoreAccessState::Error`] (so `query`
    /// reports it) and returned; the next call retries.
    pub async fn ensure_watching(&self, cluster: &str, kind: &str) -> Result<()> {
        let canonical_builtin = canonical_builtin_kind(kind);
        let canonical_cached = if canonical_builtin.is_none() {
            self.canonical_resource_kind(cluster, kind)
        } else {
            None
        };
        let mut target_kind = canonical_builtin
            .or(canonical_cached.as_deref())
            .unwrap_or(kind);

        // Check whether watcher is already active before connecting or running discovery.
        if let Some(store) = self.get_store(cluster, target_kind) {
            store.touch();
            if store.watcher_active() {
                return Ok(());
            }
        }
        // In the background nothing connects; `set_foreground(true)` restarts
        // the stores that have subscribers.
        if !self.is_foreground() {
            return Ok(());
        }

        let dynamic_ar = if canonical_builtin.is_none() {
            match self.discover_api_resource(cluster, kind).await {
                Ok(Some(ar)) => Some(ar),
                Ok(None) => {
                    let store = self.get_or_create_store(cluster, target_kind);
                    store.touch();
                    store.set_failure(StoreAccessState::NotFound {
                        resource: kind.to_owned(),
                        message: format!(
                            "Resource kind '{kind}' was not found in cluster '{cluster}'"
                        ),
                    });
                    return Ok(());
                }
                Err(err) => {
                    let store = self.get_or_create_store(cluster, target_kind);
                    store.touch();
                    store.set_error(sanitize_error(&err));
                    return Err(err);
                }
            }
        } else {
            None
        };

        if let Some(ref ar) = dynamic_ar {
            target_kind = ar.kind.as_str();
        }

        // A store evicted between lookup and spawn is replaced; retry with the new one.
        for _ in 0..3 {
            let store = self.get_or_create_store(cluster, target_kind);
            store.touch();
            if store.watcher_active() {
                return Ok(());
            }
            let client = match self.connect_client(cluster).await {
                Ok(client) => client,
                Err(err) => {
                    store.set_error(sanitize_error(&err));
                    return Err(err);
                }
            };
            if usage_cell_indices(target_kind).is_some() {
                // Refreshed on every watcher (re)start, so a client invalidated
                // after a 401 is replaced too. Metrics are optional: a failure
                // only leaves the usage cells empty.
                match self.metrics_client(cluster).await {
                    Ok(metrics_client) => store.set_metrics_client(metrics_client),
                    Err(err) => tracing::warn!(
                        cluster,
                        error = %sanitize_error(&err),
                        "cannot build the metrics client"
                    ),
                }
            }
            let outcome = if let Some(builtin) = canonical_builtin {
                self.start_builtin_watcher(&store, client, builtin)
            } else if let Some(ref ar) = dynamic_ar {
                self.start_dynamic_watcher(&store, client, ar.clone())
            } else {
                return Ok(());
            };
            match outcome {
                StartOutcome::Started => {
                    tracing::debug!(cluster, kind = target_kind, "started watcher");
                    return Ok(());
                }
                StartOutcome::AlreadyRunning | StartOutcome::Paused => return Ok(()),
                StartOutcome::Evicted => {}
            }
        }
        let store = self.get_or_create_store(cluster, target_kind);
        let message = format!(
            "failed to start watcher for {target_kind} in cluster '{cluster}': store was evicted repeatedly"
        );
        store.set_error(message.clone());
        Err(Error::WatcherEvicted {
            cluster: cluster.to_owned(),
            kind: target_kind.to_owned(),
        })
    }

    /// Runs [`Self::ensure_watching`] for every `(cluster, kind)` of `spec`
    /// (kinds default to `Pod`). Failures are logged and left in the stores'
    /// access state, which [`Self::query`] reports.
    pub async fn ensure_watching_spec(&self, spec: &QuerySpec) {
        let kinds: Vec<&str> = if spec.kinds.is_empty() {
            vec![DEFAULT_KIND]
        } else {
            spec.kinds.iter().map(String::as_str).collect()
        };
        self.ensure_watching_pairs(&spec.clusters, &kinds).await;
    }

    /// Runs [`Self::ensure_watching`] for `kinds` on each of `clusters`, concurrently.
    pub async fn ensure_watching_pairs(&self, clusters: &[String], kinds: &[&str]) {
        let tasks = clusters.iter().flat_map(|cluster| {
            kinds.iter().map(move |kind| async move {
                if let Err(err) = self.ensure_watching(cluster, kind).await {
                    let sanitized = sanitize_error(&err);
                    tracing::warn!(cluster = %cluster, kind = %kind, error = %sanitized, "cannot watch");
                }
            })
        });
        futures::future::join_all(tasks).await;
    }

    /// Lazily builds and caches a [`kube::Client`] for `context_name`, injecting
    /// the captured login-shell environment into any exec credential plugins.
    ///
    /// Single-flight per context: concurrent first calls wait for one build and
    /// share its client. Errors are not cached; a waiter whose build failed
    /// retries, and so does any later call.
    pub async fn connect_client(&self, context_name: &str) -> Result<kube::Client> {
        let cell = {
            let mut guard = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(guard.entry(context_name.to_owned()).or_default())
        };
        let result = cell
            .get_or_try_init(|| self.build_client(context_name))
            .await
            .cloned();
        if result.is_err() {
            // Drop the empty cell so unknown or failing context names don't accumulate,
            // but only if no other waiter is holding the cell.
            let mut guard = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
            if Arc::strong_count(&cell) <= 2
                && guard
                    .get(context_name)
                    .is_some_and(|c| Arc::ptr_eq(c, &cell) && !c.initialized())
            {
                guard.remove(context_name);
            }
        }
        result
    }

    /// Invalidates and removes any cached [`kube::Client`] for `context`.
    ///
    /// Returns `true` if an entry was present and removed, `false` otherwise.
    pub fn invalidate_client(&self, context: &str) -> bool {
        self.metrics_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(context);
        let mut guard = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        guard.remove(context).is_some()
    }

    /// Invalidates any cached client and triggers all stores for `context` to re-authenticate and re-list (`04-02`).
    pub fn resync_context(&self, context: &str) {
        self.invalidate_client(context);
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        for ((ctx, _), store) in stores.iter() {
            if ctx == context {
                store.resync();
            }
        }
    }

    /// Client for the `metrics.k8s.io` pollers of `context`: same kubeconfig
    /// and credentials as [`Self::connect_client`], but without kube's default
    /// retry layer (see [`ClusterManager::metrics_clients`]). Cached per context.
    async fn metrics_client(&self, context: &str) -> Result<kube::Client> {
        if let Some(client) = self
            .metrics_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(context)
        {
            return Ok(client.clone());
        }
        let mut cfg = self.build_config(context).await?;
        cfg.default_retry = false;
        let client = kube::Client::try_from(cfg)?;
        self.metrics_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(context.to_owned())
            .or_insert_with(|| client.clone());
        Ok(client)
    }

    /// Alias for [`Self::connect_client`].
    pub async fn client_for(&self, context_name: &str) -> Result<kube::Client> {
        self.connect_client(context_name).await
    }

    /// Ensures that watchers for all application kinds ([`APP_KINDS`]) are running
    /// for `clusters`.
    pub async fn ensure_watching_apps(&self, clusters: &[String]) {
        self.ensure_watching_pairs(clusters, APP_KINDS).await;
    }

    /// Returns the list of context names that are currently connected (have an initialized
    /// client or active watch stores). Does not perform any network calls or lazily connect.
    #[must_use]
    pub fn connected_clusters(&self) -> Vec<String> {
        let mut set = HashSet::new();
        {
            let guard = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
            for (name, cell) in guard.iter() {
                if cell.initialized() {
                    set.insert(name.clone());
                }
            }
        }
        {
            let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            for (cluster, _) in guard.keys() {
                set.insert(cluster.clone());
            }
        }
        let mut list: Vec<String> = set.into_iter().collect();
        list.sort();
        list
    }

    /// Contexts whose client has been built (no store-only entries, no network).
    fn clients_built(&self) -> Vec<String> {
        let guard = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        let mut list: Vec<String> = guard
            .iter()
            .filter(|(_, cell)| cell.initialized())
            .map(|(name, _)| name.clone())
            .collect();
        list.sort();
        list
    }

    /// Clusters an Overview request for `requested` covers: `requested` without
    /// empty names and duplicates, cut to [`MAX_OVERVIEW_CLUSTERS`] (first entries
    /// win) and sorted. An empty request falls back to [`Self::connected_clusters`]
    /// (same cap).
    #[must_use]
    pub fn overview_clusters(&self, requested: &[String]) -> Vec<String> {
        let mut list: Vec<String> = if requested.is_empty() {
            self.connected_clusters()
        } else {
            let mut seen = HashSet::new();
            requested
                .iter()
                .filter(|c| !c.is_empty() && seen.insert(c.as_str()))
                .cloned()
                .collect()
        };
        list.truncate(MAX_OVERVIEW_CLUSTERS);
        list.sort();
        list
    }

    /// Clusters whose [`OVERVIEW_KINDS`] an Overview request for `requested`
    /// watches: the [`Self::overview_clusters`] of an explicit request (connecting
    /// them lazily, like a table query), or for an empty request only the
    /// contexts whose client is already built, so it never opens a new connection.
    fn overview_watch_clusters(&self, requested: &[String]) -> Vec<String> {
        if requested.is_empty() {
            let mut list = self.clients_built();
            list.truncate(MAX_OVERVIEW_CLUSTERS);
            list
        } else {
            self.overview_clusters(requested)
        }
    }

    /// Ensures that watchers for all overview kinds ([`OVERVIEW_KINDS`]) run for
    /// the clusters of an Overview request (see [`Self::overview_clusters`]).
    ///
    /// An explicit `clusters` list is connected lazily, exactly like the clusters
    /// of a table query ([`Self::ensure_watching_spec`]); connection errors end up
    /// in the stores' access state. An empty list only touches clusters that are
    /// already connected.
    pub async fn ensure_watching_overview(&self, clusters: &[String]) {
        let clusters = self.overview_watch_clusters(clusters);
        if !clusters.is_empty() {
            self.ensure_watching_pairs(&clusters, OVERVIEW_KINDS).await;
        }
    }

    /// Number of client builds attempted by [`Self::connect_client`].
    #[must_use]
    pub fn client_builds(&self) -> u32 {
        self.client_builds.load(Ordering::Relaxed)
    }

    async fn build_client(&self, context_name: &str) -> Result<kube::Client> {
        self.client_builds.fetch_add(1, Ordering::Relaxed);
        let cfg = self.build_config(context_name).await?;
        Ok(kube::Client::try_from(cfg)?)
    }

    /// Loads the kubeconfig and resolves `context_name` into a [`kube::Config`].
    async fn build_config(&self, context_name: &str) -> Result<kube::Config> {
        let mut loaded = self.source.load()?;
        let primary_path = loaded.files.first().cloned().unwrap_or_default();
        ShellEnv::get()
            .await
            .apply_to_kubeconfig(&mut loaded.config);

        let options = KubeConfigOptions {
            context: Some(context_name.to_owned()),
            ..Default::default()
        };
        let cfg = kube::Config::from_custom_kubeconfig(loaded.config, &options)
            .await
            .map_err(|source| Error::Kubeconfig {
                path: primary_path,
                source,
            })?;
        Ok(cfg)
    }

    /// Finds all stores matching `cluster` and `spec.kinds`.
    fn matching_stores_for_cluster(
        &self,
        cluster: &str,
        spec: &QuerySpec,
    ) -> Vec<Arc<ResourceStore>> {
        let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let mut stores: Vec<Arc<ResourceStore>> = guard
            .iter()
            .filter(|((c, kind), _)| {
                if c != cluster {
                    return false;
                }
                if spec.kinds.is_empty() {
                    kind.eq_ignore_ascii_case(DEFAULT_KIND)
                } else {
                    spec.kinds.iter().any(|k| {
                        k.eq_ignore_ascii_case(kind)
                            || canonical_builtin_kind(k)
                                .is_some_and(|b| b.eq_ignore_ascii_case(kind))
                            || self
                                .canonical_resource_kind(cluster, k)
                                .is_some_and(|c| c.eq_ignore_ascii_case(kind))
                    })
                }
            })
            .map(|(_, store)| Arc::clone(store))
            .collect();
        stores.sort_by(|a, b| a.kind().cmp(b.kind()));
        stores
    }

    /// Executes a windowed query across all matching `(cluster, kind)` stores.
    ///
    /// Iterates over `spec.clusters`, runs queries against stores in parallel
    /// using [`futures::future::join_all`], and aggregates results into a single [`Page`].
    #[must_use]
    pub async fn query(&self, spec: &QuerySpec) -> Page {
        let target_clusters: Vec<String> = if spec.clusters.is_empty() {
            let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            let mut clusters: Vec<String> = guard.keys().map(|(c, _)| c.clone()).collect();
            clusters.sort();
            clusters.dedup();
            clusters
        } else {
            spec.clusters.clone()
        };

        let stores_by_cluster: Vec<Vec<Arc<ResourceStore>>> = target_clusters
            .iter()
            .map(|cluster| self.matching_stores_for_cluster(cluster, spec))
            .collect();

        let cluster_queries = stores_by_cluster.iter().map(|stores| async move {
            let mut cluster_access = Vec::with_capacity(stores.len());
            let mut cluster_version: u32 = 0;
            let mut cluster_rows = Vec::new();

            for store in stores {
                let guard = store.data.read().unwrap_or_else(PoisonError::into_inner);
                cluster_version = cluster_version.wrapping_add(store.version());
                cluster_access.push(StoreAccess {
                    cluster: store.cluster.clone(),
                    kind: store.kind.clone(),
                    access: store.access(),
                    rows: u32::try_from(guard.rows.len()).unwrap_or(u32::MAX),
                    metrics: store.metrics(),
                    paused: store.is_paused(),
                    scope: store.scope(),
                });
                cluster_rows.extend(guard.rows.values().cloned());
            }

            (cluster_access, cluster_version, cluster_rows)
        });

        let results = futures::future::join_all(cluster_queries).await;

        let mut access = Vec::new();
        let mut combined_version: u32 = 0;
        for (cluster_access, cluster_version, _) in &results {
            access.extend(cluster_access.clone());
            combined_version = combined_version.wrapping_add(*cluster_version);
        }

        access.sort_by(|a, b| a.cluster.cmp(&b.cluster).then_with(|| a.kind.cmp(&b.kind)));

        if combined_version == 0 {
            combined_version = 1;
        }

        let mut query_spec = spec.clone();
        if !query_spec.kinds.is_empty() {
            let mut expanded_kinds = Vec::new();
            for k in &query_spec.kinds {
                expanded_kinds.push(k.clone());
                if let Some(builtin) = canonical_builtin_kind(k)
                    && !expanded_kinds
                        .iter()
                        .any(|e| e.eq_ignore_ascii_case(builtin))
                {
                    expanded_kinds.push(builtin.to_owned());
                }
                for cluster in &query_spec.clusters {
                    if let Some(canonical) = self.canonical_resource_kind(cluster, k)
                        && !expanded_kinds
                            .iter()
                            .any(|e| e.eq_ignore_ascii_case(&canonical))
                    {
                        expanded_kinds.push(canonical);
                    }
                }
                if query_spec.clusters.is_empty() {
                    let cache = self
                        .discovery_cache
                        .read()
                        .unwrap_or_else(PoisonError::into_inner);
                    for resources in cache.values() {
                        if let Some(ar) = find_matching_resource(resources, k)
                            && !expanded_kinds
                                .iter()
                                .any(|e| e.eq_ignore_ascii_case(&ar.kind))
                        {
                            expanded_kinds.push(ar.kind.clone());
                        }
                    }
                }
            }
            query_spec.kinds = expanded_kinds;
        }

        let row_iter = results.iter().flat_map(|(_, _, rows)| rows.iter());

        let mut page = execute_query_iter(row_iter, &query_spec, combined_version);
        page.access = access;
        page
    }

    /// Synchronous variant of [`Self::query`].
    #[must_use]
    pub fn query_blocking(&self, spec: &QuerySpec) -> Page {
        futures::executor::block_on(self.query(spec))
    }

    /// Row and failing-row counts of every store that already exists for
    /// `cluster` (02-07c sidebar), sorted by kind and capped at
    /// [`MAX_KIND_COUNTS`]. Never creates a store, connects or starts a watcher:
    /// an unopened cluster yields an empty list. "Failing" is
    /// [`RowStatus::Failed`] or [`RowStatus::Degraded`], the two problem states
    /// the app view's badge ranks above `pending` (transient, like `terminating`).
    #[must_use]
    pub fn kind_counts(&self, cluster: &str) -> Vec<KindCount> {
        let mut stores: Vec<Arc<ResourceStore>> = {
            let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            guard
                .iter()
                .filter(|((c, _), _)| c == cluster)
                .map(|(_, store)| Arc::clone(store))
                .collect()
        };
        stores.sort_by(|a, b| a.kind().cmp(b.kind()));
        stores.truncate(MAX_KIND_COUNTS);
        stores
            .iter()
            .map(|store| {
                // Lock order data → access, as in `query`, so the counts and the
                // ready flag come from the same moment.
                let data = store.data.read().unwrap_or_else(PoisonError::into_inner);
                let failing = data
                    .rows
                    .values()
                    .filter(|row| matches!(row.status, RowStatus::Failed | RowStatus::Degraded))
                    .count();
                let ready = store.access() == StoreAccessState::Ready;
                KindCount {
                    kind: store.kind.clone(),
                    total: u32::try_from(data.rows.len()).unwrap_or(u32::MAX),
                    failing: u32::try_from(failing).unwrap_or(u32::MAX),
                    ready,
                }
            })
            .collect()
    }

    /// Groups rows from every store of the matching clusters into applications
    /// (`P2-05`) and returns a bounded window sorted by `(cluster, namespace, name)`.
    ///
    /// All kinds are included. When `spec.namespaces` is non-empty, cluster-scoped
    /// rows are excluded. The limit is normalised with [`clamp_limit`].
    #[must_use]
    pub fn query_apps(&self, spec: &AppQuerySpec) -> AppPage {
        if !spec.clusters.is_empty() {
            for cluster in &spec.clusters {
                for &kind in APP_KINDS {
                    let store = self.get_or_create_store(cluster, kind);
                    if !store.watcher_active() {
                        let client_opt = {
                            let guard = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
                            guard.get(cluster).and_then(|c| c.get().cloned())
                        };
                        if let Some(client) = client_opt {
                            self.start_builtin_watcher(&store, client, kind);
                        }
                    }
                }
            }

            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let this = self.clone();
                let clusters_clone = spec.clusters.clone();
                handle.spawn(async move {
                    this.ensure_watching_pairs(&clusters_clone, APP_KINDS).await;
                });
            }
        }

        let mut stores: Vec<Arc<ResourceStore>> = {
            let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            guard
                .iter()
                .filter(|((cluster, _), _)| {
                    spec.clusters.is_empty() || spec.clusters.iter().any(|c| c == cluster)
                })
                .map(|(_, store)| Arc::clone(store))
                .collect()
        };
        stores.sort_by(|a, b| {
            a.cluster()
                .cmp(b.cluster())
                .then_with(|| a.kind().cmp(b.kind()))
        });

        let guards: Vec<_> = stores
            .iter()
            .map(|s| s.data.read().unwrap_or_else(PoisonError::into_inner))
            .collect();

        let access: Vec<StoreAccess> = stores
            .iter()
            .zip(&guards)
            .map(|(store, guard)| StoreAccess {
                cluster: store.cluster.clone(),
                kind: store.kind.clone(),
                access: store.access(),
                rows: u32::try_from(guard.rows.len()).unwrap_or(u32::MAX),
                metrics: store.metrics(),
                paused: store.is_paused(),
                scope: store.scope(),
            })
            .collect();

        let row_iter = guards.iter().flat_map(|g| g.rows.values()).filter(|row| {
            spec.namespaces.is_empty()
                || row
                    .namespace
                    .as_ref()
                    .is_some_and(|ns| spec.namespaces.contains(ns))
        });

        let needle = spec
            .text_filter
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);

        let mut apps: Vec<AppSummary> = group_rows_by_app_iter(row_iter)
            .into_iter()
            .filter(|app| {
                needle
                    .as_deref()
                    .is_none_or(|n| app.name.to_lowercase().contains(n))
            })
            .collect();
        apps.sort_by(|a, b| {
            a.cluster
                .cmp(&b.cluster)
                .then_with(|| a.namespace.cmp(&b.namespace))
                .then_with(|| a.name.cmp(&b.name))
        });

        let total = u32::try_from(apps.len()).unwrap_or(u32::MAX);
        let offset = usize::try_from(spec.offset).unwrap_or(usize::MAX);
        let limit = usize::try_from(clamp_limit(spec.limit)).unwrap_or(usize::MAX);
        let apps = apps.into_iter().skip(offset).take(limit).collect();

        AppPage {
            total,
            offset: spec.offset,
            apps,
            access,
        }
    }

    /// Opens a batched query subscription that pushes an initial [`PageDelta`]
    /// immediately and coalesces subsequent store updates to at most 10 deltas/sec.
    pub fn subscribe(self: &Arc<Self>, spec: QuerySpec) -> (u32, mpsc::Receiver<PageDelta>) {
        let subscription_id = self.next_sub_id.fetch_add(1, Ordering::Relaxed);
        let stores = self.resolve_subscription_stores(&spec);

        for store in &stores {
            store.acquire();
            store.ensure_metrics_poller();
        }
        let lease = SubscriptionLease {
            stores: stores.clone(),
        };

        let mut version_rxs: Vec<watch::Receiver<u32>> =
            stores.iter().map(|s| s.subscribe_version()).collect();

        // Mark current versions as seen before taking the initial query snapshot.
        for rx in &mut version_rxs {
            let _ = rx.borrow_and_update();
        }

        let (tx, rx) = mpsc::channel(SUBSCRIPTION_CHANNEL_CAPACITY);
        let initial_page = self.query_blocking(&spec);
        let _ = tx.try_send(PageDelta {
            subscription_id,
            page: initial_page,
        });

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let _lease = lease;
            if version_rxs.is_empty() {
                tx.closed().await;
                return;
            }

            loop {
                let changed = tokio::select! {
                    () = tx.closed() => break,
                    ok = wait_any_changed(&mut version_rxs) => ok,
                };
                if !changed {
                    break;
                }

                tokio::select! {
                    () = tx.closed() => break,
                    () = tokio::time::sleep(SUBSCRIPTION_BATCH_INTERVAL) => {}
                }

                for rx in &mut version_rxs {
                    let _ = rx.borrow_and_update();
                }

                let page = manager.query(&spec).await;
                if tx
                    .send(PageDelta {
                        subscription_id,
                        page,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        (subscription_id, rx)
    }

    /// Computes the overview for `clusters` from the in-memory stores, synchronously
    /// and without connecting (see [`Self::overview_clusters`] for how the list is
    /// bounded; empty = the currently connected clusters). Call
    /// [`Self::ensure_watching_overview`] first to start the watchers.
    #[must_use]
    pub fn get_overview(&self, clusters: &[String]) -> crate::api::OverviewData {
        crate::overview::compute_overview(self, clusters)
    }

    /// Opens a throttled overview subscription for `clusters` that emits an
    /// initial [`OverviewData`] immediately and pushes coalesced updates (at most
    /// 1/s) when the watched stores change, and at least every
    /// [`OVERVIEW_REFRESH_INTERVAL`].
    ///
    /// A non-empty `clusters` list is bounded by [`Self::overview_clusters`]; its
    /// [`OVERVIEW_KINDS`] stores are leased for the life of the subscription and
    /// their watchers are started in the background (connecting those clusters
    /// lazily, like a table query), so the first message arrives at once with the
    /// clusters still `Connecting`. An empty list covers the clusters connected
    /// at subscribe time and never opens a new connection.
    ///
    /// The leases are released when the subscription task ends, i.e. when the
    /// receiver is dropped (`tx.closed()`) or a send fails.
    pub fn subscribe_overview(
        self: &Arc<Self>,
        clusters: Vec<String>,
    ) -> (u32, mpsc::Receiver<crate::api::OverviewData>) {
        let subscription_id = self.next_sub_id.fetch_add(1, Ordering::Relaxed);
        // Every push reports exactly these clusters (an empty request is pinned to
        // the clusters connected now, like the leased stores).
        let covered = self.overview_clusters(&clusters);
        let watch_clusters = self.overview_watch_clusters(&clusters);
        let mut stores = Vec::new();
        for cluster in &covered {
            for &kind in OVERVIEW_KINDS {
                let store = self.get_or_create_store(cluster, kind);
                store.acquire();
                store.ensure_metrics_poller();
                stores.push(store);
            }
        }
        let lease = SubscriptionLease {
            stores: stores.clone(),
        };

        if !watch_clusters.is_empty() {
            let manager = Arc::clone(self);
            tokio::spawn(async move {
                manager
                    .ensure_watching_pairs(&watch_clusters, OVERVIEW_KINDS)
                    .await;
            });
        }

        let mut version_rxs: Vec<watch::Receiver<u32>> =
            stores.iter().map(|s| s.subscribe_version()).collect();

        for rx in &mut version_rxs {
            let _ = rx.borrow_and_update();
        }

        let (tx, rx) = mpsc::channel(SUBSCRIPTION_CHANNEL_CAPACITY);
        let initial_data = self.get_overview(&covered);
        let mut connecting = connecting_clusters(&initial_data);
        let _ = tx.try_send(initial_data);

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let _lease = lease;
            let mut last_push = tokio::time::Instant::now();

            loop {
                // In the background the periodic refresh stops; the pause itself
                // bumped the store versions, so one push reports it first.
                if !manager.is_foreground()
                    && !version_rxs
                        .iter()
                        .any(|rx| rx.has_changed().unwrap_or(false))
                {
                    tokio::select! {
                        () = tx.closed() => break,
                        () = manager.wait_foreground() => {}
                    }
                }
                // Wait for a store change, or refresh ages without one.
                let refresh_at = last_push + OVERVIEW_REFRESH_INTERVAL;
                let changed = if version_rxs.is_empty() {
                    tokio::select! {
                        () = tx.closed() => break,
                        () = tokio::time::sleep_until(refresh_at) => true,
                    }
                } else {
                    tokio::select! {
                        () = tx.closed() => break,
                        ok = wait_any_changed(&mut version_rxs) => ok,
                        () = tokio::time::sleep_until(refresh_at) => true,
                    }
                };
                if !changed {
                    break;
                }

                // At most one push per OVERVIEW_MIN_INTERVAL since the previous
                // push: send at once if that was long enough ago, else wait out
                // only the remainder. Exception: a cluster that was still
                // `Connecting` in the previous push and now has its headline
                // counts is pushed at once (cold start); that happens at most
                // once per cluster connect, so the rate stays bounded.
                let next_at = last_push + OVERVIEW_MIN_INTERVAL;
                let mut closed = false;
                while tokio::time::Instant::now() < next_at
                    && !manager.any_left_connecting(&connecting)
                {
                    if version_rxs.is_empty() {
                        tokio::select! {
                            () = tx.closed() => { closed = true; break; }
                            () = tokio::time::sleep_until(next_at) => {}
                        }
                    } else {
                        tokio::select! {
                            () = tx.closed() => { closed = true; break; }
                            () = tokio::time::sleep_until(next_at) => {}
                            ok = wait_any_changed(&mut version_rxs) => if !ok {
                                closed = true;
                                break;
                            },
                        }
                    }
                }
                if closed {
                    break;
                }

                for rx in &mut version_rxs {
                    let _ = rx.borrow_and_update();
                }

                let data = manager.get_overview(&covered);
                connecting = connecting_clusters(&data);
                last_push = tokio::time::Instant::now();
                if tx.send(data).await.is_err() {
                    break;
                }
            }
        });

        (subscription_id, rx)
    }

    fn matching_stores_for_query(&self, spec: &QuerySpec) -> Vec<Arc<ResourceStore>> {
        let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let mut stores: Vec<Arc<ResourceStore>> = guard
            .iter()
            .filter(|((cluster, kind), _)| {
                let cluster_ok =
                    spec.clusters.is_empty() || spec.clusters.iter().any(|c| c == cluster);
                let kind_ok = if spec.kinds.is_empty() {
                    kind.eq_ignore_ascii_case(DEFAULT_KIND)
                } else {
                    spec.kinds.iter().any(|k| {
                        k.eq_ignore_ascii_case(kind)
                            || canonical_builtin_kind(k)
                                .is_some_and(|c| c.eq_ignore_ascii_case(kind))
                            || self
                                .canonical_resource_kind(cluster, k)
                                .is_some_and(|c| c.eq_ignore_ascii_case(kind))
                    })
                };
                cluster_ok && kind_ok
            })
            .map(|(_, store)| Arc::clone(store))
            .collect();
        stores.sort_by(|a, b| {
            a.cluster()
                .cmp(b.cluster())
                .then_with(|| a.kind().cmp(b.kind()))
        });
        stores
    }

    fn resolve_subscription_stores(&self, spec: &QuerySpec) -> Vec<Arc<ResourceStore>> {
        if spec.clusters.is_empty() {
            return self.matching_stores_for_query(spec);
        }

        let kinds: Vec<&str> = if spec.kinds.is_empty() {
            vec![DEFAULT_KIND]
        } else {
            spec.kinds.iter().map(String::as_str).collect()
        };

        let mut stores = Vec::new();
        for cluster in &spec.clusters {
            for kind in &kinds {
                let canonical = self.canonical_resource_kind(cluster, kind);
                let target_kind = canonical.as_deref().unwrap_or(kind);
                stores.push(self.get_or_create_store(cluster, target_kind));
            }
        }
        stores
    }
}

impl Drop for ClusterManager {
    fn drop(&mut self) {
        // Watcher tasks hold their store, not the manager; stop them explicitly
        // when the last manager sharing these stores is dropped.
        if Arc::strong_count(&self.stores) == 1 {
            let guard = self.stores.read().unwrap_or_else(PoisonError::into_inner);
            for store in guard.values() {
                store.stop_watcher();
            }
        }
    }
}

/// Clusters shown as `Connecting` in an Overview push.
fn connecting_clusters(data: &crate::api::OverviewData) -> Vec<String> {
    data.clusters
        .iter()
        .filter(|c| c.connection == crate::api::ConnectionStatus::Connecting)
        .map(|c| c.cluster.clone())
        .collect()
}

impl ClusterManager {
    /// `true` when one of `clusters` (all `Connecting` in the previous Overview
    /// push) is no longer `Connecting`. Reads access states only, never rows.
    fn any_left_connecting(&self, clusters: &[String]) -> bool {
        clusters.iter().any(|c| {
            crate::overview::connection_status(self, c) != crate::api::ConnectionStatus::Connecting
        })
    }
}

async fn wait_any_changed(rxs: &mut [watch::Receiver<u32>]) -> bool {
    let futures = rxs.iter_mut().map(|rx| Box::pin(rx.changed()));
    let (res, _, _) = futures::future::select_all(futures).await;
    res.is_ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use k8s_openapi::api::core::v1::{Container, Pod, PodSpec, PodStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    use super::*;
    use crate::api::{Cell, RowStatus};

    #[test]
    fn test_canonical_builtin_kinds_expansion() {
        assert_eq!(canonical_builtin_kind("rs"), Some("ReplicaSet"));
        assert_eq!(canonical_builtin_kind("replicasets"), Some("ReplicaSet"));
        assert_eq!(
            canonical_builtin_kind("endpointslice"),
            Some("EndpointSlice")
        );
        assert_eq!(
            canonical_builtin_kind("hpa"),
            Some("HorizontalPodAutoscaler")
        );
        assert_eq!(canonical_builtin_kind("pdb"), Some("PodDisruptionBudget"));
        assert_eq!(canonical_builtin_kind("quota"), Some("ResourceQuota"));
        assert_eq!(canonical_builtin_kind("limits"), Some("LimitRange"));
        assert_eq!(canonical_builtin_kind("pc"), Some("PriorityClass"));
        assert_eq!(canonical_builtin_kind("runtimeclass"), Some("RuntimeClass"));
        assert_eq!(canonical_builtin_kind("lease"), Some("Lease"));
        assert_eq!(canonical_builtin_kind("pv"), Some("PersistentVolume"));
        assert_eq!(canonical_builtin_kind("pvc"), Some("PersistentVolumeClaim"));
        assert_eq!(canonical_builtin_kind("sc"), Some("StorageClass"));
        assert_eq!(
            canonical_builtin_kind("volumeattachment"),
            Some("VolumeAttachment")
        );
        assert_eq!(canonical_builtin_kind("sa"), Some("ServiceAccount"));
        assert_eq!(canonical_builtin_kind("role"), Some("Role"));
        assert_eq!(canonical_builtin_kind("clusterroles"), Some("ClusterRole"));
        assert_eq!(canonical_builtin_kind("rolebinding"), Some("RoleBinding"));
        assert_eq!(
            canonical_builtin_kind("clusterrolebindings"),
            Some("ClusterRoleBinding")
        );
        assert_eq!(canonical_builtin_kind("netpol"), Some("NetworkPolicy"));
        assert_eq!(
            canonical_builtin_kind("mutatingwebhookconfigurations"),
            Some("MutatingWebhookConfiguration")
        );
        assert_eq!(
            canonical_builtin_kind("validatingwebhookconfigurations"),
            Some("ValidatingWebhookConfiguration")
        );
        assert_eq!(
            canonical_builtin_kind("validatingadmissionpolicies"),
            Some("ValidatingAdmissionPolicy")
        );
        assert_eq!(
            canonical_builtin_kind("validatingadmissionpolicybinding"),
            Some("ValidatingAdmissionPolicyBinding")
        );
        assert_eq!(canonical_builtin_kind("unknowncrd"), None);
    }

    fn dummy_manager(idle_timeout: Duration) -> Arc<ClusterManager> {
        let src = KubeconfigSource::from_paths(vec![PathBuf::from("/nonexistent/kubeconfig")]);
        Arc::new(ClusterManager::with_idle_timeout(src, idle_timeout))
    }

    fn synthetic_pod(uid: &str, namespace: &str, name: &str) -> Pod {
        Pod {
            metadata: ObjectMeta {
                uid: Some(uid.to_owned()),
                name: Some(name.to_owned()),
                namespace: Some(namespace.to_owned()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                node_name: Some("node-1".to_owned()),
                containers: vec![Container {
                    name: "app".to_owned(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            status: Some(PodStatus {
                phase: Some("Running".to_owned()),
                pod_ip: Some("10.0.0.1".to_owned()),
                ..Default::default()
            }),
        }
    }

    fn synthetic_row(uid: &str, cluster: &str, name: &str) -> Row {
        Row {
            uid: uid.to_owned(),
            cluster: cluster.to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some("default".to_owned()),
            name: name.to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: vec![Cell::Text("node-1".to_owned())],
            labels: Vec::new(),
            custom: Vec::new(),
        }
    }

    fn base_spec(clusters: Vec<String>) -> QuerySpec {
        QuerySpec {
            clusters,
            kinds: vec!["Pod".to_owned()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 100,
            statuses: Vec::new(),
        }
    }

    #[tokio::test]
    async fn store_upsert_remove_and_multi_cluster_query() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let store_eu = manager.get_or_create_store("prod-eu", "Pod");
        let store_us = manager.get_or_create_store("prod-us", "Pod");

        store_eu.upsert_pod(&synthetic_pod("uid-eu-1", "default", "api-eu-1"));
        store_eu.upsert_pod(&synthetic_pod("uid-eu-2", "default", "api-eu-2"));
        store_us.upsert_pod(&synthetic_pod("uid-us-1", "default", "api-us-1"));

        let spec = base_spec(vec!["prod-eu".to_owned(), "prod-us".to_owned()]);
        let page1 = manager.query(&spec).await;
        assert_eq!(page1.total, 3);
        assert_eq!(page1.rows.len(), 3);

        let removed = store_eu.remove_uid("uid-eu-1");
        assert!(removed);
        assert!(!store_eu.remove_uid("uid-eu-1"));

        let page2 = manager.query(&spec).await;
        assert_eq!(page2.total, 2);
        assert!(page2.version > page1.version);
    }

    #[tokio::test]
    async fn multi_cluster_query_parallel_stores_and_aggregation() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let store_a = manager.get_or_create_store("cluster-a", "Pod");
        let store_b = manager.get_or_create_store("cluster-b", "Pod");
        let store_c = manager.get_or_create_store("cluster-c", "Pod");

        store_a.upsert_pod(&synthetic_pod("uid-a-1", "default", "pod-a-1"));
        store_b.upsert_pod(&synthetic_pod("uid-b-1", "default", "pod-b-1"));
        store_b.upsert_pod(&synthetic_pod("uid-b-2", "default", "pod-b-2"));
        store_c.upsert_pod(&synthetic_pod("uid-c-1", "default", "pod-c-1"));

        // 1. Query subsets of clusters
        let spec_ab =
            QuerySpec::for_clusters(vec!["cluster-a".to_owned(), "cluster-b".to_owned()], "Pod");
        let page_ab = manager.query(&spec_ab).await;
        assert_eq!(page_ab.total, 3);
        assert_eq!(page_ab.rows.len(), 3);
        assert_eq!(page_ab.access.len(), 2);
        assert_eq!(page_ab.access_for_cluster("cluster-a").len(), 1);
        assert_eq!(page_ab.access_for_cluster("cluster-b").len(), 1);
        assert!(page_ab.access_for_cluster("cluster-c").is_empty());

        // 2. Single cluster query (cluster-specific)
        let spec_c = QuerySpec::for_cluster("cluster-c", "Pod");
        let page_c = manager.query(&spec_c).await;
        assert_eq!(page_c.total, 1);
        assert_eq!(page_c.rows[0].cluster, "cluster-c");
        assert_eq!(page_c.access.len(), 1);

        // 3. All clusters query (empty clusters)
        let spec_all = QuerySpec {
            clusters: Vec::new(),
            kinds: vec!["Pod".to_owned()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 50,
            statuses: Vec::new(),
        };
        let page_all = manager.query(&spec_all).await;
        assert_eq!(page_all.total, 4);
        assert_eq!(page_all.access.len(), 3);
    }

    fn labelled_row(
        uid: &str,
        cluster: &str,
        kind: &str,
        namespace: Option<&str>,
        name: &str,
        app: &str,
    ) -> Row {
        Row {
            uid: uid.to_owned(),
            cluster: cluster.to_owned(),
            kind: kind.to_owned(),
            namespace: namespace.map(str::to_owned),
            name: name.to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: Vec::new(),
            labels: vec![("app.kubernetes.io/name".to_owned(), app.to_owned())],
            custom: Vec::new(),
        }
    }

    fn app_spec(limit: u32) -> AppQuerySpec {
        AppQuerySpec {
            clusters: Vec::new(),
            namespaces: Vec::new(),
            text_filter: None,
            offset: 0,
            limit,
        }
    }

    #[test]
    fn query_apps_groups_filters_sorts_and_windows() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let pods_b = manager.get_or_create_store("b", "Pod");
        let svcs_b = manager.get_or_create_store("b", "Service");
        let pods_a = manager.get_or_create_store("a", "Pod");

        pods_b.upsert_row(labelled_row("1", "b", "Pod", Some("prod"), "web-0", "Web"));
        svcs_b.upsert_row(labelled_row(
            "2",
            "b",
            "Service",
            Some("prod"),
            "web",
            "Web",
        ));
        pods_b.upsert_row(labelled_row("3", "b", "Pod", Some("dev"), "db-0", "db"));
        pods_a.upsert_row(labelled_row("4", "a", "Pod", Some("prod"), "web-0", "Web"));
        pods_a.upsert_row(labelled_row("5", "a", "Pod", None, "ops", "ops"));

        // Grouping across kinds within (cluster, namespace), sorted by (cluster, namespace, name).
        let page = manager.query_apps(&app_spec(50));
        assert_eq!(page.total, 4);
        let keys: Vec<(&str, &str, &str)> = page
            .apps
            .iter()
            .map(|a| (a.cluster.as_str(), a.namespace.as_str(), a.name.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("a", "", "ops"),
                ("a", "prod", "Web"),
                ("b", "dev", "db"),
                ("b", "prod", "Web"),
            ]
        );
        assert_eq!(page.apps[3].components.len(), 2);

        // Cluster and namespace filters.
        let mut spec = app_spec(50);
        spec.clusters = vec!["b".to_owned()];
        spec.namespaces = vec!["prod".to_owned()];
        let page = manager.query_apps(&spec);
        assert_eq!(page.total, 1);
        assert_eq!(page.apps[0].components.len(), 2);

        // Case-insensitive text filter on the app name.
        let mut spec = app_spec(50);
        spec.text_filter = Some("  wE ".to_owned());
        let page = manager.query_apps(&spec);
        assert_eq!(page.total, 2);
        assert!(page.apps.iter().all(|a| a.name == "Web"));

        // Window: total counts all matches, apps holds only the window.
        let mut spec = app_spec(2);
        spec.offset = 1;
        let page = manager.query_apps(&spec);
        assert_eq!(page.total, 4);
        assert_eq!(page.offset, 1);
        assert_eq!(page.apps.len(), 2);
        assert_eq!(page.apps[0].name, "Web");
        assert_eq!(page.apps[1].name, "db");

        let mut spec = app_spec(10);
        spec.offset = 10;
        let page = manager.query_apps(&spec);
        assert_eq!(page.total, 4);
        assert!(page.apps.is_empty());
    }

    #[test]
    fn query_apps_clamps_limit_to_max_page_limit() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let store = manager.get_or_create_store("c1", "Pod");
        let count = crate::query::MAX_PAGE_LIMIT + 5;
        for i in 0..count {
            let name = format!("app-{i:05}");
            store.upsert_row(labelled_row(
                &format!("uid-{i}"),
                "c1",
                "Pod",
                Some("default"),
                &name,
                &name,
            ));
        }

        let page = manager.query_apps(&app_spec(u32::MAX));
        assert_eq!(page.total, count);
        assert_eq!(page.apps.len(), crate::query::MAX_PAGE_LIMIT as usize);
    }

    #[test]
    fn reference_count_and_idle_eviction() {
        let manager_keep = dummy_manager(Duration::from_secs(60));
        let store = manager_keep.get_or_create_store("dev", "Pod");
        store.acquire();
        assert_eq!(store.ref_count(), 1);
        assert!(!store.is_idle_expired(Duration::ZERO));

        store.release();
        assert_eq!(store.ref_count(), 0);
        assert_eq!(manager_keep.evict_idle_stores(), 0);

        let manager_evict = dummy_manager(Duration::ZERO);
        let store2 = manager_evict.get_or_create_store("dev", "Pod");
        store2.acquire();
        store2.release();
        assert_eq!(manager_evict.evict_idle_stores(), 1);
    }

    /// Manager whose only context points at a closed local port (no DNS, no real cluster).
    fn unreachable_manager(idle_timeout: Duration) -> (Arc<ClusterManager>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "fg-cluster-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kubeconfig");
        let content = "apiVersion: v1\nkind: Config\ncurrent-context: closed\n\
             clusters:\n- name: closed\n  cluster:\n    server: https://127.0.0.1:1\n\
             contexts:\n- name: closed\n  context:\n    cluster: closed\n    user: u\n\
             users:\n- name: u\n  user:\n    token: not-a-secret\n";
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(content.as_bytes()).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&path, content).unwrap();
        }
        let src = KubeconfigSource::from_paths(vec![path]);
        (
            Arc::new(ClusterManager::with_idle_timeout(src, idle_timeout)),
            dir,
        )
    }

    async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cond()
    }

    #[tokio::test]
    async fn ensure_watching_starts_one_watcher_under_concurrency_and_eviction_aborts_it() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);
        let calls = (0..16).map(|_| manager.ensure_watching("closed", "pod"));
        for res in futures::future::join_all(calls).await {
            res.unwrap();
        }
        let store = manager.get_or_create_store("closed", "Pod");
        assert_eq!(store.watchers_started(), 1);
        assert!(store.watcher_active());

        // Further calls reuse the running watcher.
        manager
            .ensure_watching_spec(&base_spec(vec!["closed".to_owned()]))
            .await;
        assert_eq!(store.watchers_started(), 1);

        // The list against the closed port fails: reported as an error, not an empty list.
        let spec = base_spec(vec!["closed".to_owned()]);
        assert!(
            wait_until(|| matches!(
                manager.query_blocking(&spec).access[0].access,
                StoreAccessState::Error { .. }
            ))
            .await,
            "{:?}",
            manager.query(&spec).await.access
        );
        let page = manager.query(&spec).await;
        assert_eq!(page.total, 0);
        assert_eq!(page.access.len(), 1);
        assert_eq!(page.access[0].cluster, "closed");
        assert_eq!(page.access[0].kind, "Pod");

        // Idle eviction aborts the watcher task and blocks restarting on the old store.
        let manager_evict = ClusterManager::with_idle_timeout(
            KubeconfigSource::from_paths(manager.source.paths().to_vec()),
            Duration::ZERO,
        );
        manager_evict
            .ensure_watching("closed", "Pod")
            .await
            .unwrap();
        let old = manager_evict.get_or_create_store("closed", "Pod");
        assert!(old.watcher_active());
        assert_eq!(manager_evict.evict_idle_stores(), 1);
        assert!(wait_until(|| !old.watcher_active()).await);
        assert!(matches!(
            old.start_watcher_once(|_| tokio::spawn(async {})),
            StartOutcome::Evicted
        ));

        // A later call gets a fresh store with its own watcher.
        manager_evict
            .ensure_watching("closed", "Pod")
            .await
            .unwrap();
        let fresh = manager_evict.get_or_create_store("closed", "Pod");
        assert!(!Arc::ptr_eq(&old, &fresh));
        assert_eq!(fresh.watchers_started(), 1);

        // Dropping the manager stops its watchers.
        drop(manager_evict);
        assert!(wait_until(|| !fresh.watcher_active()).await);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn connect_client_is_single_flight_and_does_not_cache_errors() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);

        // Concurrent first calls share one build.
        let calls = (0..16).map(|_| manager.connect_client("closed"));
        for res in futures::future::join_all(calls).await {
            res.unwrap();
        }
        assert_eq!(manager.client_builds(), 1);
        manager.connect_client("closed").await.unwrap();
        assert_eq!(manager.client_builds(), 1);

        // Failures are not cached: every later call builds again, and no empty
        // cell is left behind for the unknown context.
        assert!(manager.connect_client("missing").await.is_err());
        assert!(manager.connect_client("missing").await.is_err());
        assert_eq!(manager.client_builds(), 3);
        assert!(!manager.clients.lock().unwrap().contains_key("missing"));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn status_row(uid: &str, cluster: &str, kind: &str, status: RowStatus) -> Row {
        Row {
            kind: kind.to_owned(),
            status,
            ..synthetic_row(uid, cluster, uid)
        }
    }

    #[test]
    fn kind_counts_counts_failing_rows_per_watched_store() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let pods = manager.get_or_create_store("a", "Pod");
        for (uid, status) in [
            ("p1", RowStatus::Healthy),
            ("p2", RowStatus::Failed),
            ("p3", RowStatus::Degraded),
            ("p4", RowStatus::Pending),
            ("p5", RowStatus::Completed),
            ("p6", RowStatus::Terminating),
            ("p7", RowStatus::Unknown),
        ] {
            pods.upsert_row(status_row(uid, "a", "Pod", status));
        }
        pods.set_access(StoreAccessState::Ready);
        let deps = manager.get_or_create_store("a", "Deployment");
        deps.upsert_row(status_row("d1", "a", "Deployment", RowStatus::Healthy));
        deps.set_access(StoreAccessState::Ready);
        // Another cluster's store is not reported.
        let other = manager.get_or_create_store("b", "Pod");
        other.upsert_row(status_row("x", "b", "Pod", RowStatus::Failed));

        let counts = manager.kind_counts("a");
        assert_eq!(
            counts,
            vec![
                KindCount {
                    kind: "Deployment".to_owned(),
                    total: 1,
                    failing: 0,
                    ready: true,
                },
                KindCount {
                    kind: "Pod".to_owned(),
                    total: 7,
                    failing: 2,
                    ready: true,
                },
            ]
        );
    }

    #[test]
    fn kind_counts_of_unwatched_cluster_is_empty_and_creates_nothing() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        manager.get_or_create_store("a", "Pod");
        assert!(manager.kind_counts("never-opened").is_empty());
        assert!(manager.get_store("never-opened", "Pod").is_none());
        assert_eq!(manager.client_builds(), 0);
        assert_eq!(manager.stores.read().unwrap().len(), 1);
    }

    #[test]
    fn kind_counts_flags_non_ready_stores() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let pods = manager.get_or_create_store("a", "Pod");
        pods.upsert_row(status_row("p1", "a", "Pod", RowStatus::Failed));
        // A new store is Loading (connecting) until its first list completes.
        let counts = manager.kind_counts("a");
        assert_eq!(counts.len(), 1);
        assert!(!counts[0].ready);
        assert_eq!((counts[0].total, counts[0].failing), (1, 1));

        let secrets = manager.get_or_create_store("a", "Secret");
        secrets.set_access(StoreAccessState::Forbidden {
            resource: "secrets".to_owned(),
            namespace: None,
            message: "No permission to list secrets".to_owned(),
        });
        pods.set_access(StoreAccessState::Ready);
        let counts = manager.kind_counts("a");
        let flags: Vec<(&str, bool)> = counts.iter().map(|c| (c.kind.as_str(), c.ready)).collect();
        assert_eq!(flags, vec![("Pod", true), ("Secret", false)]);

        pods.set_access(StoreAccessState::Reconnecting);
        assert!(!manager.kind_counts("a")[0].ready);
    }

    #[test]
    fn kind_counts_is_capped() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        for i in 0..MAX_KIND_COUNTS + 10 {
            manager.get_or_create_store("a", &format!("Kind{i:04}"));
        }
        let counts = manager.kind_counts("a");
        assert_eq!(counts.len(), MAX_KIND_COUNTS);
        assert_eq!(counts[0].kind, "Kind0000");
    }

    #[test]
    fn query_apps_reports_store_access() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let pods = manager.get_or_create_store("b", "Pod");
        pods.set_access(StoreAccessState::Forbidden {
            resource: "pods".to_owned(),
            namespace: None,
            message: "No permission to list pods".to_owned(),
        });
        let svcs = manager.get_or_create_store("a", "Service");
        svcs.set_access(StoreAccessState::Ready);

        let page = manager.query_apps(&app_spec(10));
        assert_eq!(page.total, 0);
        let access: Vec<(&str, &str)> = page
            .access
            .iter()
            .map(|a| (a.cluster.as_str(), a.kind.as_str()))
            .collect();
        assert_eq!(access, vec![("a", "Service"), ("b", "Pod")]);
        assert!(matches!(
            page.access[1].access,
            StoreAccessState::Forbidden { .. }
        ));

        let mut spec = app_spec(10);
        spec.clusters = vec!["a".to_owned()];
        let page_a = manager.query_apps(&spec);
        assert_eq!(page_a.access.len(), 6);
        let a_kinds: Vec<&str> = page_a.access.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(
            a_kinds,
            vec![
                "DaemonSet",
                "Deployment",
                "Job",
                "Pod",
                "Service",
                "StatefulSet"
            ]
        );
    }

    #[tokio::test]
    async fn invalidate_client_removes_context_and_forces_rebuild() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);

        // Connect initially
        manager.connect_client("closed").await.unwrap();
        assert_eq!(manager.client_builds(), 1);
        assert!(manager.clients.lock().unwrap().contains_key("closed"));

        // Invalidate removes it and returns true
        assert!(manager.invalidate_client("closed"));
        assert!(!manager.clients.lock().unwrap().contains_key("closed"));

        // Second invalidate returns false
        assert!(!manager.invalidate_client("closed"));

        // Next connect builds a new client
        manager.connect_client("closed").await.unwrap();
        assert_eq!(manager.client_builds(), 2);
        assert!(manager.clients.lock().unwrap().contains_key("closed"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn watcher_401_invalidates_client_and_sets_error_access() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);

        manager.connect_client("closed").await.unwrap();
        assert!(manager.clients.lock().unwrap().contains_key("closed"));

        let store = manager.get_or_create_store("closed", "Pod");
        assert_eq!(store.access(), StoreAccessState::CONNECTING);

        // Simulate 401 unauthorized watcher error
        let status = Box::new(kube::core::Status {
            code: 401,
            message: "Unauthorized".to_owned(),
            reason: "Unauthorized".to_owned(),
            ..Default::default()
        });
        let err = watcher::Error::WatchError(status);
        assert!(is_unauthorized_watcher_error(&err));

        let access_state = watch_error_state(&err, "pods", None);
        assert!(matches!(
            access_state,
            Some(StoreAccessState::Error { ref message, .. }) if message.contains("Unauthorized")
        ));

        // When the invalidator is called (as spawn_watcher_from_api does on 401):
        if let Some(invalidator) = store.client_invalidator() {
            invalidator(&store.cluster);
        }
        assert!(!manager.clients.lock().unwrap().contains_key("closed"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn query_apps_watches_all_app_kinds_across_queried_clusters() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);

        // Pre-populate client so builtin watchers can start immediately
        manager.connect_client("closed").await.unwrap();

        let spec = AppQuerySpec {
            clusters: vec!["closed".to_owned()],
            namespaces: Vec::new(),
            text_filter: None,
            offset: 0,
            limit: 50,
        };

        let page = manager.query_apps(&spec);

        // Verify all 6 app kinds are represented in store access
        let kinds: Vec<&str> = page.access.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "DaemonSet",
                "Deployment",
                "Job",
                "Pod",
                "Service",
                "StatefulSet"
            ]
        );

        // Verify stores for all 6 kinds were created and have watchers started
        for kind in APP_KINDS {
            let store = manager.get_store("closed", kind).expect("store must exist");
            assert!(
                store.watcher_active(),
                "watcher for {kind} should be active"
            );
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn ensure_watching_reports_connect_errors_and_unsupported_kinds() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);

        // Unknown context: the connect error is returned and stored in the page.
        assert!(manager.ensure_watching("missing", "Pod").await.is_err());
        let spec = base_spec(vec!["missing".to_owned()]);
        manager.ensure_watching_spec(&spec).await;
        let page = manager.query(&spec).await;
        assert_eq!(page.total, 0);
        assert!(
            matches!(&page.access[0].access, StoreAccessState::Error { message, .. } if !message.is_empty()),
            "{:?}",
            page.access
        );
        assert_eq!(
            manager
                .get_or_create_store("missing", "Pod")
                .watchers_started(),
            0
        );

        // Built-in kinds start a watcher even against unreachable cluster.
        let mut spec = base_spec(vec!["closed".to_owned()]);
        spec.kinds = vec!["Deployment".to_owned()];
        manager.ensure_watching_spec(&spec).await;
        assert_eq!(
            manager
                .get_or_create_store("closed", "Deployment")
                .watchers_started(),
            1
        );

        // Unknown CRD kinds attempt discovery; against unreachable cluster it sets Error.
        let mut spec = base_spec(vec!["closed".to_owned()]);
        spec.kinds = vec!["UnknownCrdKind".to_owned()];
        manager.ensure_watching_spec(&spec).await;
        let page = manager.query(&spec).await;
        assert_eq!(page.total, 0);
        assert!(
            matches!(&page.access[0].access, StoreAccessState::Error { message, .. } if !message.is_empty()),
            "{:?}",
            page.access
        );
        assert_eq!(
            manager
                .get_or_create_store("closed", "UnknownCrdKind")
                .watchers_started(),
            0
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn discovery_cache_bypass_and_alias_lookup() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);

        let gvk = kube::api::GroupVersionKind::gvk("example.com", "v1", "TestItem");
        let ar = ApiResource::from_gvk(&gvk);
        {
            let mut cache = manager
                .discovery_cache
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            cache.insert("closed".to_owned(), vec![ar]);
        }

        // 1. Non-existent kind returns Ok(None) immediately without attempting network connect.
        // If it attempted network connect, against "closed" it would return Error because closed cluster fails to connect.
        let non_existent = manager.discover_api_resource("closed", "NonExistent").await;
        assert!(matches!(non_existent, Ok(None)));

        // 2. Dynamic kind canonicalization
        assert_eq!(
            manager.canonical_resource_kind("closed", "testitems"),
            Some("TestItem".to_owned())
        );
        assert_eq!(
            manager.canonical_resource_kind("closed", "testitem"),
            Some("TestItem".to_owned())
        );
        assert_eq!(
            manager.canonical_resource_kind("closed", "TestItem"),
            Some("TestItem".to_owned())
        );

        // Populate a row in ("closed", "TestItem") store
        let store = manager.get_or_create_store("closed", "TestItem");
        store.upsert_row(Row {
            uid: "uid-1".to_owned(),
            cluster: "closed".to_owned(),
            kind: "TestItem".to_owned(),
            namespace: Some("default".to_owned()),
            name: "item-1".to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Ready".to_owned(),
            cells: Vec::new(),
            labels: Vec::new(),
            custom: Vec::new(),
        });

        // 3. Query lookup using plural alias "testitems" matches "TestItem" store
        let mut spec = base_spec(vec!["closed".to_owned()]);
        spec.kinds = vec!["testitems".to_owned()];
        let page = manager.query(&spec).await;
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].name, "item-1");

        // 4. Query lookup using lowercase singular alias "testitem" matches "TestItem" store
        let mut spec = base_spec(vec!["closed".to_owned()]);
        spec.kinds = vec!["testitem".to_owned()];
        let page = manager.query(&spec).await;
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].name, "item-1");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn watch_errors_map_to_access_states() {
        let status = |code| {
            Box::new(kube::core::Status {
                code,
                message: "m".to_owned(),
                ..Default::default()
            })
        };
        let forbidden = watcher::Error::InitialListFailed(kube::Error::Api(status(403)));
        assert!(matches!(
            watch_error_state(&forbidden, "pods", None),
            Some(StoreAccessState::Forbidden { message, .. }) if message == "No permission to list pods"
        ));
        assert_eq!(
            watch_error_state(&watcher::Error::WatchError(status(410)), "pods", None),
            None
        );
        assert!(matches!(
            watch_error_state(&watcher::Error::WatchError(status(403)), "pods", None),
            Some(StoreAccessState::Forbidden { .. })
        ));
        let server_error = watcher::Error::InitialListFailed(kube::Error::Api(status(500)));
        assert!(matches!(
            watch_error_state(&server_error, "pods", None),
            Some(StoreAccessState::Error { .. })
        ));
    }

    #[tokio::test]
    #[cfg_attr(
        windows,
        ignore = "Windows timer resolution (~15.6ms) stretches the 2ms writer sleeps past the 250ms window"
    )]
    async fn subscription_batches_rapid_updates_to_at_most_10_per_sec() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let store = manager.get_or_create_store("c1", "Pod");

        let spec = base_spec(vec!["c1".to_owned()]);
        let (_sub_id, mut rx) = manager.subscribe(spec);
        assert_eq!(store.ref_count(), 1);

        let writer_store = Arc::clone(&store);
        let writer = tokio::spawn(async move {
            for i in 0..50u32 {
                writer_store.upsert_row(synthetic_row(
                    &format!("uid-{i}"),
                    "c1",
                    &format!("pod-{i:02}"),
                ));
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });

        let mut deltas = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
        while let Ok(Some(delta)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            deltas.push(delta);
        }

        writer.await.unwrap();

        assert!(
            !deltas.is_empty() && deltas.len() <= 4,
            "expected 1..=4 batched deltas over 250ms, got {}",
            deltas.len()
        );
        assert_eq!(deltas.last().unwrap().page.total, 50);

        drop(rx);
        // Allow the subscription task to observe channel closure and release its lease.
        for _ in 0..20 {
            if store.ref_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(store.ref_count(), 0);
    }

    #[tokio::test]
    async fn metrics_poll_joins_usage_in_place_and_bumps_version() {
        use crate::metrics::{POD_CPU_CELL, UsageSample};
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let store = manager.get_or_create_store("c1", "Pod");
        store.upsert_pod(&synthetic_pod("u1", "ns", "a"));
        let spec = base_spec(vec!["c1".to_owned()]);

        let page = manager.query(&spec).await;
        assert_eq!(page.access[0].metrics, Some(MetricsAvailability::Pending));
        assert_eq!(page.rows[0].cells[POD_CPU_CELL], Cell::Text(String::new()));

        let mut map = UsageMap::new();
        map.entry("ns".to_owned())
            .or_default()
            .insert("a".to_owned(), UsageSample { cpu: 7, memory: 9 });
        map.entry("ns".to_owned())
            .or_default()
            .insert("b".to_owned(), UsageSample { cpu: 3, memory: 4 });
        let before = store.version();
        store.apply_metrics_outcome(Ok(map));
        assert!(store.version() > before, "a poll must bump the version");

        let page = manager.query(&spec).await;
        assert_eq!(page.access[0].metrics, Some(MetricsAvailability::Available));
        assert!(matches!(&page.rows[0].cells[POD_CPU_CELL], Cell::Usage(u) if u.value == 7));

        // A pod added after the poll is joined with the last poll on insert.
        store.upsert_pod(&synthetic_pod("u2", "ns", "b"));
        let page = manager.query(&spec).await;
        assert!(matches!(&page.rows[1].cells[POD_CPU_CELL], Cell::Usage(u) if u.value == 3));

        // Unavailable metrics clear the cells and are reported, not shown as 0.
        store.apply_metrics_outcome(Err(MetricsAvailability::NotInstalled {
            message: "metrics-server not installed".to_owned(),
        }));
        let page = manager.query(&spec).await;
        assert!(matches!(
            page.access[0].metrics,
            Some(MetricsAvailability::NotInstalled { .. })
        ));
        assert!(
            page.rows
                .iter()
                .all(|r| r.cells[POD_CPU_CELL] == Cell::Text(String::new()))
        );

        // Kinds without usage columns report `None`.
        manager.get_or_create_store("c1", "Service");
        let mut svc = base_spec(vec!["c1".to_owned()]);
        svc.kinds = vec!["Service".to_owned()];
        assert_eq!(manager.query(&svc).await.access[0].metrics, None);
    }

    #[tokio::test]
    async fn metrics_poller_runs_only_while_subscribed() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);
        manager.ensure_watching("closed", "Pod").await.unwrap();
        let store = manager.get_or_create_store("closed", "Pod");
        // A watcher without subscribers does not poll metrics.
        assert!(!store.metrics_poller_active());
        assert_eq!(store.metrics_pollers_started(), 0);

        let (_id, rx) = manager.subscribe(base_spec(vec!["closed".to_owned()]));
        assert!(store.metrics_poller_active());
        let (_id2, rx2) = manager.subscribe(base_spec(vec!["closed".to_owned()]));
        assert_eq!(store.metrics_pollers_started(), 1, "one poller per store");

        // The unreachable API server is an error, not "available" or zeros.
        assert!(
            wait_until(|| matches!(store.metrics(), Some(MetricsAvailability::Error { .. }))).await,
            "{:?}",
            store.metrics()
        );

        drop(rx);
        drop(rx2);
        assert!(wait_until(|| store.ref_count() == 0).await);
        // Stops after the grace period and resets to Pending.
        let deadline = Instant::now() + METRICS_STOP_GRACE + Duration::from_secs(3);
        while store.metrics_poller_active() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!store.metrics_poller_active());
        assert_eq!(store.metrics(), Some(MetricsAvailability::Pending));

        // A new subscriber starts it again.
        let (_id3, _rx3) = manager.subscribe(base_spec(vec!["closed".to_owned()]));
        assert!(store.metrics_poller_active());
        assert_eq!(store.metrics_pollers_started(), 2);
        drop(manager);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sanitize_kube_error_redacts_auth_exec_run_secrets() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        use std::process::Output;

        #[cfg(unix)]
        let status = std::process::ExitStatus::from_raw(1 << 8);
        #[cfg(not(unix))]
        let status = std::process::ExitStatus::default();

        let fake_secret = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let fake_cmd = format!("aws-iam-authenticator token -i cluster --env {fake_secret}");
        let fake_out = Output {
            status,
            stdout: format!("token output containing {fake_secret}").into_bytes(),
            stderr: format!("error: {fake_secret} was invalid").into_bytes(),
        };

        let auth_err = kube::client::AuthError::AuthExecRun {
            cmd: fake_cmd,
            out: Box::new(fake_out),
        };
        let kube_err = kube::Error::Auth(auth_err);
        let sanitized = sanitize_kube_error(&kube_err);

        assert!(
            !sanitized.contains("AWS_SECRET_ACCESS_KEY"),
            "secret leaked in sanitized error: {sanitized}"
        );
        assert!(
            !sanitized.contains("wJalrXUtnFEMI"),
            "secret leaked in sanitized error: {sanitized}"
        );
        assert!(
            sanitized.contains("authentication failed: exec credential plugin failed (exit status"),
            "unexpected sanitized message: {sanitized}"
        );

        let core_err = Error::Kube(kube_err);
        let sanitized_core = sanitize_error(&core_err);
        assert!(!sanitized_core.contains("AWS_SECRET_ACCESS_KEY"));
        assert_eq!(sanitized_core, sanitized);
    }

    #[tokio::test]
    #[cfg_attr(
        windows,
        ignore = "connect to a closed port takes ~2s on Windows (SYN retries) while paused time auto-advances past the deadline"
    )]
    async fn watcher_backoff_exponential_growth_on_unreachable_endpoint() {
        tokio::time::pause();
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);
        let client = manager.connect_client("closed").await.unwrap();
        let api = kube::Api::<Pod>::all(client);
        let stream = watcher::watcher(api, watcher::Config::default());
        let mut backoff_stream = watch_with_backoff(stream);

        let mut retries = 0;
        let test_duration = Duration::from_secs(60);
        let deadline = tokio::time::Instant::now() + test_duration;

        while tokio::time::Instant::now() < deadline {
            tokio::select! {
                item = backoff_stream.next() => {
                    match item {
                        Some(Err(_)) => {
                            retries += 1;
                        }
                        Some(Ok(Event::Init)) => {
                            // Event::Init is emitted on every attempt before listing;
                            // backoff MUST NOT reset on it.
                        }
                        other => {
                            panic!("unexpected item: {other:?}");
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }

        assert!(
            (3..=8).contains(&retries),
            "expected between 3 and 8 retries over 60s with exponential backoff, got {retries}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn custom_cols(paths: &[&str]) -> Arc<CustomColumns> {
        let defs = paths
            .iter()
            .map(|p| ColumnDef {
                id: (*p).to_owned(),
                title: (*p).to_owned(),
                json_path: (*p).to_owned(),
                width: None,
            })
            .collect();
        Arc::new(CustomColumns::compile(defs).unwrap())
    }

    /// Runs the InitApply/InitDone handling of the watch loop over `pods`.
    fn stage_and_publish(store: &ResourceStore, staging: &mut InitStaging, pods: &[Pod]) {
        for pod in pods {
            let cols = store.custom_columns();
            staging.stage(project_pod_with_columns(pod, &store.cluster, &cols), &cols);
        }
        let (rows, cols) = staging.take();
        store.publish_initial_rows(rows, &cols);
    }

    #[test]
    fn init_staging_matches_buffered_projection() {
        let cols = custom_cols(&[".metadata.name", ".spec.nodeName"]);
        let pods: Vec<Pod> = (0..50)
            .map(|i| synthetic_pod(&format!("uid-{i}"), "default", &format!("pod-{i}")))
            .collect();

        let old_way = ResourceStore::with_columns("c", "Pod", Arc::clone(&cols));
        old_way.replace_all_pods(&pods);

        let new_way = ResourceStore::with_columns("c", "Pod", Arc::clone(&cols));
        let mut staging = InitStaging::default();
        stage_and_publish(&new_way, &mut staging, &pods);

        assert_eq!(new_way.snapshot_rows().0, old_way.snapshot_rows().0);
        assert_eq!(new_way.snapshot_rows().0.len(), 50);
        assert!(staging.rows.is_empty(), "take() must reset staging");
    }

    #[test]
    fn init_staging_clear_drops_partial_list_and_latest_uid_wins() {
        let store = ResourceStore::new("c", "Pod");
        let cols = store.custom_columns();
        let mut staging = InitStaging::default();
        let stale = synthetic_pod("uid-stale", "default", "stale");
        staging.stage(project_pod_with_columns(&stale, "c", &cols), &cols);
        // A failed list restarts with Event::Init.
        staging.clear();
        assert!(staging.rows.is_empty());

        let first = synthetic_pod("uid-1", "default", "old-name");
        let second = synthetic_pod("uid-1", "default", "new-name");
        stage_and_publish(&store, &mut staging, &[first, second]);
        let (rows, _) = store.snapshot_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "new-name");
    }

    #[test]
    fn init_staging_remaps_rows_when_columns_shrink_mid_list() {
        let store = ResourceStore::with_columns(
            "c",
            "Pod",
            custom_cols(&[".metadata.name", ".spec.nodeName"]),
        );
        let mut staging = InitStaging::default();
        let cols = store.custom_columns();
        let a = synthetic_pod("uid-a", "default", "a");
        staging.stage(project_pod_with_columns(&a, "c", &cols), &cols);

        // Removing a column does not re-list, so the list continues.
        assert!(!store.set_custom_columns(custom_cols(&[".spec.nodeName"])));
        let cols = store.custom_columns();
        let b = synthetic_pod("uid-b", "default", "b");
        staging.stage(project_pod_with_columns(&b, "c", &cols), &cols);
        let (rows, staged_cols) = staging.take();

        // Columns reordered after the last InitApply: remapped on publish.
        store.set_custom_columns(custom_cols(&[".spec.nodeName", ".metadata.name"]));
        store.publish_initial_rows(rows, &staged_cols);

        let (rows, _) = store.snapshot_rows();
        let node = Cell::Text("node-1".to_owned());
        let empty = Cell::Text(String::new());
        for row in &rows {
            assert_eq!(
                row.custom,
                vec![node.clone(), empty.clone()],
                "{}",
                row.name
            );
        }
    }

    fn pod_query_spec(cluster: &str) -> QuerySpec {
        QuerySpec {
            clusters: vec![cluster.into()],
            kinds: vec!["Pod".into()],
            namespaces: vec![],
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 60,
            statuses: Vec::new(),
        }
    }

    /// Feeds `range` as `InitApply` events at `now`, like the watch loop.
    fn init_apply_range(
        store: &ResourceStore,
        staging: &mut InitStaging,
        range: std::ops::Range<usize>,
        now: Instant,
    ) {
        for i in range {
            let cols = store.custom_columns();
            let pod = synthetic_pod(&format!("uid-{i}"), "default", &format!("pod-{i}"));
            let row = project_pod_with_columns(&pod, &store.cluster, &cols);
            store.init_apply(staging, row, &cols, now);
        }
    }

    #[tokio::test]
    async fn first_list_is_published_progressively_and_labelled_loading() {
        let manager = dummy_manager(Duration::from_secs(60));
        let store = manager.get_or_create_store("c", "Pod");
        let mut rx = store.subscribe_version();
        let mut staging = InitStaging::default();
        let t0 = Instant::now();

        // Less than a page, before the first-publish delay: nothing visible yet.
        init_apply_range(&store, &mut staging, 0..499, t0);
        assert_eq!(store.row_count(), 0);
        assert_eq!(store.access(), StoreAccessState::CONNECTING);
        assert!(!rx.has_changed().unwrap());

        // One full page: published at once and labelled with the count so far.
        init_apply_range(&store, &mut staging, 499..500, t0);
        assert_eq!(store.row_count(), 500);
        assert!(staging.rows.is_empty(), "published rows leave staging");
        assert!(rx.has_changed().unwrap(), "a progressive publish notifies");
        rx.mark_unchanged();
        let page = manager.query(&pod_query_spec("c")).await;
        assert_eq!(page.total, 500);
        assert_eq!(page.rows.len(), 60);
        assert_eq!(
            page.access[0].access,
            StoreAccessState::Loading { rows_so_far: 500 }
        );

        // Later batches wait for the publish interval...
        init_apply_range(
            &store,
            &mut staging,
            500..900,
            t0 + Duration::from_millis(10),
        );
        assert_eq!(store.row_count(), 500);
        assert!(!rx.has_changed().unwrap());
        let due = staging.next_publish_at().expect("rows are staged");
        assert!(due >= t0 + PROGRESS_PUBLISH_INTERVAL);
        // ...then go out with the next InitApply (or the loop's timer).
        init_apply_range(&store, &mut staging, 900..901, due);
        assert_eq!(store.row_count(), 901);
        assert_eq!(
            store.access(),
            StoreAccessState::Loading { rows_so_far: 901 }
        );

        init_apply_range(&store, &mut staging, 901..1000, due);
        store.init_done(&mut staging);
        assert_eq!(store.row_count(), 1000);
        assert_eq!(store.access(), StoreAccessState::Ready);
        assert!(store.has_synced());
    }

    #[test]
    fn small_first_list_is_published_after_the_first_delay() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        init_apply_range(&store, &mut staging, 0..3, t0);
        assert_eq!(store.row_count(), 0);
        init_apply_range(&store, &mut staging, 3..4, t0 + FIRST_PUBLISH_DELAY * 2);
        assert_eq!(store.row_count(), 4);
        assert_eq!(store.access(), StoreAccessState::Loading { rows_so_far: 4 });
    }

    #[test]
    fn init_done_removes_rows_not_seen_in_the_list() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        // A first attempt publishes one page (uid-0..uid-499), then the list
        // fails and restarts.
        init_apply_range(&store, &mut staging, 0..600, t0);
        assert_eq!(store.row_count(), 500);
        staging.clear();

        // The new list no longer has uid-0..uid-99 (deleted meanwhile). Its
        // first page (uid-100..uid-599) is published next to the old rows.
        init_apply_range(&store, &mut staging, 100..700, t0);
        assert_eq!(store.row_count(), 600, "partial: old rows still shown");
        assert!(store.access().is_loading());
        store.init_done(&mut staging);

        let (rows, _) = store.snapshot_rows();
        assert_eq!(rows.len(), 600);
        assert!(rows.iter().all(|r| r.uid != "uid-0" && r.uid != "uid-99"));
        assert!(rows.iter().any(|r| r.uid == "uid-100"));
        assert_eq!(store.access(), StoreAccessState::Ready);
    }

    #[test]
    fn relist_keeps_old_rows_until_init_done() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        init_apply_range(&store, &mut staging, 0..10, t0);
        store.init_done(&mut staging);
        assert_eq!(store.access(), StoreAccessState::Ready);
        let before = store.snapshot_rows().0;

        // Watcher restart: a long re-list without uid-0..uid-4.
        staging.clear();
        let later = t0 + Duration::from_secs(10);
        init_apply_range(&store, &mut staging, 5..2000, later);
        init_apply_range(
            &store,
            &mut staging,
            2000..2001,
            later + Duration::from_secs(1),
        );
        assert!(staging.next_publish_at().is_some());
        assert_eq!(store.snapshot_rows().0, before, "no half-new list is shown");
        assert_eq!(store.access(), StoreAccessState::Ready);

        store.init_done(&mut staging);
        assert_eq!(store.row_count(), 1996);
        assert!(store.snapshot_rows().0.iter().all(|r| r.uid != "uid-0"));
        assert_eq!(store.access(), StoreAccessState::Ready);
    }

    #[test]
    fn progressive_publish_remaps_columns_changed_mid_list() {
        let store = ResourceStore::with_columns(
            "c",
            "Pod",
            custom_cols(&[".metadata.name", ".spec.nodeName"]),
        );
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        // First batch published with [name, node].
        init_apply_range(&store, &mut staging, 0..500, t0);
        assert_eq!(store.row_count(), 500);

        // Removing a column does not re-list: published rows are remapped by
        // the store, staged ones by the staging.
        init_apply_range(&store, &mut staging, 500..510, t0);
        assert!(!store.set_custom_columns(custom_cols(&[".spec.nodeName"])));
        init_apply_range(&store, &mut staging, 510..520, t0);
        // Reordered after the last InitApply: remapped on the final publish.
        store.set_custom_columns(custom_cols(&[".spec.nodeName", ".metadata.name"]));
        store.init_done(&mut staging);

        let (rows, _) = store.snapshot_rows();
        assert_eq!(rows.len(), 520);
        let node = Cell::Text("node-1".to_owned());
        let empty = Cell::Text(String::new());
        for row in &rows {
            assert_eq!(
                row.custom,
                vec![node.clone(), empty.clone()],
                "{}",
                row.name
            );
        }
    }

    fn timeout_error() -> StoreAccessState {
        StoreAccessState::Error {
            message: "list timed out".to_owned(),
            partial_rows: None,
        }
    }

    #[test]
    fn failed_first_list_keeps_error_and_disarms_the_publish_timer() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        // Rows staged but not yet due: the loop's timer is armed.
        init_apply_range(&store, &mut staging, 0..10, t0);
        assert!(staging.next_publish_at().is_some());
        assert_eq!(store.row_count(), 0);

        store.list_failed(&mut staging, timeout_error());
        assert_eq!(store.access(), timeout_error(), "nothing was published");
        assert_eq!(staging.next_publish_at(), None, "timer disarmed");
        assert!(!staging.publish_due(t0 + Duration::from_secs(1)));

        // Defence in depth: a stray progress publish never hides the error.
        store.publish_progress(&mut staging, false);
        assert_eq!(store.access(), timeout_error());
        assert_eq!(store.row_count(), 0);

        // The restarted list recovers to Loading, then Ready.
        staging.clear();
        store.set_access(StoreAccessState::CONNECTING);
        init_apply_range(&store, &mut staging, 0..500, t0);
        assert_eq!(
            store.access(),
            StoreAccessState::Loading { rows_so_far: 500 }
        );
        store.init_done(&mut staging);
        assert_eq!(store.access(), StoreAccessState::Ready);
    }

    #[tokio::test]
    async fn failed_first_list_labels_published_rows_as_partial() {
        let manager = dummy_manager(Duration::from_secs(60));
        let store = manager.get_or_create_store("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        // One page published, 100 more staged when the list fails.
        init_apply_range(&store, &mut staging, 0..600, t0);
        assert_eq!(store.row_count(), 500);

        store.list_failed(&mut staging, timeout_error());
        let partial = StoreAccessState::Error {
            message: "list timed out".to_owned(),
            partial_rows: Some(500),
        };
        assert_eq!(store.access(), partial);
        assert_eq!(staging.next_publish_at(), None);
        let page = manager.query(&pod_query_spec("c")).await;
        assert_eq!(page.total, 500, "received rows stay visible");
        assert_eq!(page.access[0].access, partial);

        store.publish_progress(&mut staging, false);
        assert_eq!(store.access(), partial);
    }

    #[test]
    fn failed_relist_of_a_synced_store_is_not_partial() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        init_apply_range(&store, &mut staging, 0..10, t0);
        store.init_done(&mut staging);

        staging.clear();
        init_apply_range(&store, &mut staging, 0..5, t0 + Duration::from_secs(5));
        store.list_failed(&mut staging, timeout_error());
        assert_eq!(store.access(), timeout_error());
        assert_eq!(store.row_count(), 10, "last complete list kept");
    }

    #[tokio::test]
    async fn forbidden_first_list_clears_published_rows() {
        let denied = [
            StoreAccessState::Forbidden {
                resource: "pods".to_owned(),
                namespace: None,
                message: "No permission to list pods".to_owned(),
            },
            StoreAccessState::NotFound {
                resource: "pods".to_owned(),
                message: "not found".to_owned(),
            },
            StoreAccessState::Unsupported {
                kind: "Pod".to_owned(),
                message: "unsupported".to_owned(),
            },
        ];
        for state in denied {
            let manager = dummy_manager(Duration::from_secs(60));
            let store = manager.get_or_create_store("c", "Pod");
            let mut staging = InitStaging::default();
            init_apply_range(&store, &mut staging, 0..600, Instant::now());
            assert_eq!(store.row_count(), 500);
            let mut rx = store.subscribe_version();
            rx.mark_unchanged();
            let version = store.version();

            store.list_failed(&mut staging, state.clone());
            assert_eq!(store.access(), state);
            assert_eq!(store.row_count(), 0, "no rows under {state:?}");
            assert!(store.version() > version);
            assert!(
                rx.has_changed().unwrap(),
                "subscribers see the cleared rows"
            );
            assert_eq!(staging.next_publish_at(), None);
            let page = manager.query(&pod_query_spec("c")).await;
            assert_eq!(page.total, 0);
            assert_eq!(page.access[0].access, state);

            // A later retry's first batch must not reuse the cleared rows as stale.
            store.publish_progress(&mut staging, false);
            assert_eq!(store.access(), state, "an empty publish keeps the denial");
        }
    }

    #[test]
    fn denied_first_list_clears_rows_and_sets_state_in_one_notification() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        init_apply_range(&store, &mut staging, 0..600, Instant::now());
        assert!(store.row_count() > 0);
        let rx = store.subscribe_version();
        let before = *rx.borrow();
        let forbidden = StoreAccessState::Forbidden {
            resource: "pods".to_owned(),
            namespace: None,
            message: "No permission to list pods".to_owned(),
        };
        store.list_failed(&mut staging, forbidden.clone());
        assert_eq!(store.row_count(), 0);
        assert_eq!(store.access(), forbidden);
        assert_eq!(
            store.version(),
            before.wrapping_add(1),
            "rows and state change with a single notification"
        );
    }

    #[test]
    fn set_failure_not_found_before_first_sync_clears_rows() {
        // The `ensure_watching` discovery path shares the rule with `list_failed`.
        let store = ResourceStore::new("c", "Widget");
        let mut staging = InitStaging::default();
        init_apply_range(&store, &mut staging, 0..600, Instant::now());
        let not_found = StoreAccessState::NotFound {
            resource: "Widget".to_owned(),
            message: "gone".to_owned(),
        };
        store.set_failure(not_found.clone());
        assert_eq!(store.row_count(), 0);
        assert_eq!(store.access(), not_found);

        // A plain error keeps (and labels) partial rows instead.
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        init_apply_range(&store, &mut staging, 0..600, Instant::now());
        store.set_error("connection refused".to_owned());
        assert_eq!(
            store.access(),
            StoreAccessState::Error {
                message: "connection refused".to_owned(),
                partial_rows: Some(500),
            }
        );
    }

    #[test]
    fn forbidden_on_a_synced_store_keeps_rows() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        init_apply_range(&store, &mut staging, 0..10, Instant::now());
        store.init_done(&mut staging);
        let forbidden = StoreAccessState::Forbidden {
            resource: "pods".to_owned(),
            namespace: None,
            message: "No permission to list pods".to_owned(),
        };
        staging.clear();
        store.list_failed(&mut staging, forbidden.clone());
        assert_eq!(store.access(), forbidden);
        assert_eq!(store.row_count(), 10);
    }

    fn fallback_plan(allowed: &[&str], denied: &[&str]) -> FallbackPlan {
        FallbackPlan {
            source: crate::api::NamespaceSource::Configured,
            allowed: allowed.iter().map(|s| (*s).to_owned()).collect(),
            denied: denied.iter().map(|s| (*s).to_owned()).collect(),
            truncated: false,
        }
    }

    fn ns_rows(store: &ResourceStore, ns: &str, uids: &[&str]) -> BTreeMap<String, Row> {
        let cols = store.custom_columns();
        uids.iter()
            .map(|uid| {
                let pod = synthetic_pod(uid, ns, uid);
                (
                    (*uid).to_owned(),
                    project_pod_with_columns(&pod, &store.cluster, &cols),
                )
            })
            .collect()
    }

    fn namespaces_of(store: &ResourceStore) -> Vec<String> {
        let (rows, _) = store.snapshot_rows();
        let mut out: Vec<String> = rows.into_iter().filter_map(|r| r.namespace).collect();
        out.sort();
        out
    }

    #[test]
    fn namespace_fallback_merges_namespaces_and_is_ready_when_all_are() {
        let store = ResourceStore::new("c", "Pod");
        store.enter_namespace_scope(&fallback_plan(&["dev", "qa"], &["prod"]));
        let scope = store.scope().expect("restricted scope");
        assert_eq!(scope.denied, vec!["prod".to_owned()]);
        assert_eq!(scope.namespaces.len(), 2);
        assert!(store.access().is_loading());

        let cols = store.custom_columns();
        store.replace_namespace_rows("dev", ns_rows(&store, "dev", &["d1", "d2"]), &cols);
        store.set_namespace_state("dev", StoreAccessState::Ready);
        assert!(
            store.access().is_loading(),
            "qa still listing: not Ready yet"
        );
        store.replace_namespace_rows("qa", ns_rows(&store, "qa", &["q1"]), &cols);
        store.set_namespace_state("qa", StoreAccessState::Ready);
        assert_eq!(store.access(), StoreAccessState::Ready);
        assert_eq!(namespaces_of(&store), vec!["dev", "dev", "qa"]);

        // A re-list of `dev` replaces only dev's rows.
        store.replace_namespace_rows("dev", ns_rows(&store, "dev", &["d2"]), &cols);
        assert_eq!(namespaces_of(&store), vec!["dev", "qa"]);
    }

    #[test]
    fn namespace_fallback_reports_one_failing_namespace() {
        let store = ResourceStore::new("c", "Pod");
        store.enter_namespace_scope(&fallback_plan(&["dev", "qa"], &[]));
        store.set_namespace_state("dev", StoreAccessState::Ready);
        let forbidden = StoreAccessState::Forbidden {
            resource: "pods".to_owned(),
            namespace: Some("qa".to_owned()),
            message: "No permission to list pods in qa".to_owned(),
        };
        store.set_namespace_state("qa", forbidden.clone());
        assert_eq!(store.access(), forbidden);
        let scope = store.scope().expect("restricted scope");
        assert_eq!(scope.namespaces[0].access, StoreAccessState::Ready);
        assert_eq!(scope.namespaces[1].access, forbidden);
        // Unknown namespaces are ignored.
        store.set_namespace_state("other", StoreAccessState::Ready);
        assert_eq!(store.access(), forbidden);
    }

    #[test]
    fn entering_namespace_scope_drops_rows_outside_it_and_leaving_clears_it() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        // Rows of `default` from an earlier cluster-wide list.
        init_apply_range(&store, &mut staging, 0..3, Instant::now());
        store.init_done(&mut staging);
        let cols = store.custom_columns();
        store.replace_namespace_rows("dev", ns_rows(&store, "dev", &["d1"]), &cols);
        store.enter_namespace_scope(&fallback_plan(&["dev"], &[]));
        assert_eq!(namespaces_of(&store), vec!["dev"]);
        assert_eq!(
            store.access(),
            StoreAccessState::Reconnecting,
            "a synced store re-lists as reconnecting"
        );
        store.leave_namespace_scope();
        assert!(store.scope().is_none());
    }

    #[test]
    fn combined_access_prefers_syncing_over_failures() {
        let ns = |name: &str, access: StoreAccessState| NamespaceWatch {
            namespace: name.to_owned(),
            access,
        };
        let err = StoreAccessState::Error {
            message: "boom".to_owned(),
            partial_rows: None,
        };
        assert_eq!(
            combined_namespace_access(&[], false, 0),
            StoreAccessState::Ready
        );
        assert_eq!(
            combined_namespace_access(
                &[ns("a", err.clone()), ns("b", StoreAccessState::CONNECTING)],
                false,
                4
            ),
            StoreAccessState::Loading { rows_so_far: 4 }
        );
        assert_eq!(
            combined_namespace_access(
                &[ns("a", StoreAccessState::Ready), ns("b", err.clone())],
                true,
                4
            ),
            err
        );
    }

    #[test]
    fn retry_after_partial_first_list_is_labelled_loading() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        // First attempt publishes uid-0..uid-499, then fails partway.
        init_apply_range(&store, &mut staging, 0..600, t0);
        store.list_failed(&mut staging, timeout_error());
        assert_eq!(
            store.access(),
            StoreAccessState::Error {
                message: "list timed out".to_owned(),
                partial_rows: Some(500),
            }
        );

        // The watcher retries: Init, then the new list (without uid-0..uid-99).
        staging.clear();
        init_apply_range(&store, &mut staging, 100..599, t0);
        assert!(
            matches!(store.access(), StoreAccessState::Error { .. }),
            "nothing of the retry published yet"
        );
        init_apply_range(&store, &mut staging, 599..600, t0);
        assert_eq!(
            store.access(),
            StoreAccessState::Loading { rows_so_far: 600 },
            "the retry's first batch switches back to Loading (stale rows counted)"
        );

        store.init_done(&mut staging);
        assert_eq!(store.access(), StoreAccessState::Ready);
        let (rows, _) = store.snapshot_rows();
        assert_eq!(rows.len(), 500);
        assert!(rows.iter().all(|r| r.uid != "uid-0" && r.uid != "uid-99"));
    }

    #[test]
    fn watcher_restart_of_a_synced_store_is_reconnecting() {
        let store = ResourceStore::new("c", "Pod");
        let mut staging = InitStaging::default();
        let t0 = Instant::now();
        init_apply_range(&store, &mut staging, 0..10, t0);
        store.init_done(&mut staging);

        store.watcher_starting();
        assert_eq!(store.access(), StoreAccessState::Reconnecting);
        assert!(!store.access().is_loading());
        assert!(store.access().is_syncing());
        assert_eq!(store.row_count(), 10, "last complete list kept");

        staging.clear();
        init_apply_range(&store, &mut staging, 0..2000, t0 + Duration::from_secs(5));
        assert_eq!(store.access(), StoreAccessState::Reconnecting);
        store.init_done(&mut staging);
        assert_eq!(store.access(), StoreAccessState::Ready);
        assert_eq!(store.row_count(), 2000);
    }

    #[test]
    fn watcher_start_of_a_never_synced_store_is_loading() {
        let store = ResourceStore::new("c", "Pod");
        store.watcher_starting();
        assert_eq!(store.access(), StoreAccessState::CONNECTING);

        // Partial rows of a failed first list stay labelled when it restarts.
        let mut staging = InitStaging::default();
        init_apply_range(&store, &mut staging, 0..500, Instant::now());
        store.list_failed(&mut staging, timeout_error());
        store.watcher_starting();
        assert_eq!(
            store.access(),
            StoreAccessState::Loading { rows_so_far: 500 }
        );
    }

    #[test]
    fn reconnecting_state_serializes_as_tag_only() {
        let json = serde_json::to_value(StoreAccessState::Reconnecting).unwrap();
        assert_eq!(json, serde_json::json!({ "state": "reconnecting" }));
    }

    #[test]
    fn error_state_serializes_partial_rows() {
        let json = serde_json::to_value(StoreAccessState::Error {
            message: "boom".to_owned(),
            partial_rows: Some(3),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "state": "error", "message": "boom", "partialRows": 3 })
        );
        let json = serde_json::to_value(timeout_error()).unwrap();
        assert_eq!(json["partialRows"], serde_json::Value::Null);
    }

    #[test]
    fn loading_state_serializes_rows_so_far() {
        let json = serde_json::to_value(StoreAccessState::Loading { rows_so_far: 42 }).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "state": "loading", "rowsSoFar": 42 })
        );
    }

    #[tokio::test]
    async fn cluster_manager_query_50k_rows_under_50ms() {
        let manager = dummy_manager(Duration::from_secs(60));
        for c in ["c1", "c2", "c3"] {
            let store = manager.get_or_create_store(c, "Pod");
            let mut staging = InitStaging::default();
            let cols = store.custom_columns();
            for i in 0..16_667 {
                let pod = synthetic_pod(&format!("{c}-uid-{i}"), "default", &format!("pod-{i}"));
                let row = project_pod_with_columns(&pod, c, &cols);
                staging.stage(row, &cols);
            }
            let (rows, staged_cols) = staging.take();
            store.publish_initial_rows(rows, &staged_cols);
        }

        let spec = QuerySpec {
            clusters: vec!["c1".into(), "c2".into(), "c3".into()],
            kinds: vec!["Pod".into()],
            namespaces: vec![],
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 60,
            statuses: Vec::new(),
        };

        let page = manager.query(&spec).await;
        assert_eq!(page.total, 50_001);
        assert_eq!(page.rows.len(), 60);

        // Timing is only meaningful in optimised builds; CI runs debug `cargo test`.
        if cfg!(debug_assertions) {
            return;
        }
        let mut samples: Vec<Duration> = Vec::with_capacity(50);
        for _ in 0..50 {
            let start = std::time::Instant::now();
            let page = manager.query(&spec).await;
            let elapsed = start.elapsed();
            assert_eq!(page.total, 50_001);
            samples.push(elapsed);
        }
        samples.sort();
        let p95 = samples[(samples.len() * 95).div_ceil(100) - 1];
        println!("query p95 over 50 runs at 50k rows: {p95:?}");
        assert!(p95 < Duration::from_millis(50), "query p95 {p95:?}");
    }

    #[tokio::test]
    async fn background_pauses_watchers_and_foreground_resumes_subscribed_stores() {
        let (manager, dir) = unreachable_manager(DEFAULT_IDLE_TIMEOUT);
        assert!(manager.is_foreground(), "foreground by default");
        let spec = base_spec(vec!["closed".to_owned()]);
        manager.ensure_watching("closed", "Pod").await.unwrap();
        manager.ensure_watching("closed", "Service").await.unwrap();
        let pods = manager.get_or_create_store("closed", "Pod");
        let services = manager.get_or_create_store("closed", "Service");
        pods.replace_all_pods(&[synthetic_pod("u1", "default", "p1")]);
        // Pods are subscribed (with a metrics poller), Services are not.
        let (_id, mut rx) = manager.subscribe(spec.clone());
        assert!(rx.recv().await.is_some(), "initial page");
        assert!(pods.watcher_active() && services.watcher_active());
        assert!(pods.metrics_poller_active());
        let started = pods.watchers_started();

        // Background: everything stops at once, rows and subscription stay.
        assert!(manager.set_foreground(false).await);
        assert!(!manager.set_foreground(false).await, "idempotent");
        assert!(!manager.is_foreground());
        assert!(wait_until(|| !pods.watcher_active() && !services.watcher_active()).await);
        assert!(wait_until(|| !pods.metrics_poller_active()).await);
        assert!(pods.is_paused() && services.is_paused());
        assert_eq!(pods.ref_count(), 1, "subscription kept");
        let page = manager.query(&spec).await;
        assert_eq!(page.total, 1, "last-known rows are kept");
        assert!(page.access[0].paused);
        // The subscriber is told about the pause.
        let delta = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("delta after pause")
            .expect("open subscription");
        assert!(delta.page.access[0].paused);

        // Nothing restarts in the background, not even on demand.
        manager.ensure_watching("closed", "Pod").await.unwrap();
        let (_id2, _rx2) = manager.subscribe(spec.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!pods.watcher_active() && !pods.metrics_poller_active());
        assert_eq!(pods.watchers_started(), started);
        // A store created in the background is paused too.
        let late = manager.get_or_create_store("closed", "Node");
        assert!(late.is_paused());

        // Foreground: the subscribed store re-lists, the unsubscribed one does not.
        let version = pods.version();
        assert!(manager.set_foreground(true).await);
        assert!(!manager.set_foreground(true).await, "idempotent");
        assert!(wait_until(|| pods.watcher_active()).await);
        assert_eq!(pods.watchers_started(), started + 1);
        assert!(pods.version() > version, "version bumped so pages refresh");
        assert!(wait_until(|| pods.metrics_poller_active()).await);
        assert!(!pods.is_paused() && !services.is_paused() && !late.is_paused());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!services.watcher_active(), "no subscriber, no restart");
        assert!(!manager.query(&spec).await.access[0].paused);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(start_paused = true)]
    async fn overview_refresh_stops_in_the_background() {
        let manager = dummy_manager(DEFAULT_IDLE_TIMEOUT);
        let (_id, mut rx) = manager.subscribe_overview(vec!["c1".to_owned()]);
        assert!(rx.recv().await.is_some(), "initial push");
        manager.set_foreground(false).await;
        // One push reports the pause, then no periodic refresh.
        let paused = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await;
        assert!(matches!(paused, Ok(Some(_))), "push after pausing");
        let idle = tokio::time::timeout(OVERVIEW_REFRESH_INTERVAL * 4, rx.recv()).await;
        assert!(idle.is_err(), "no refresh in the background");
        manager.set_foreground(true).await;
        let resumed = tokio::time::timeout(OVERVIEW_REFRESH_INTERVAL * 2, rx.recv()).await;
        assert!(matches!(resumed, Ok(Some(_))), "refresh resumes");
    }
}
