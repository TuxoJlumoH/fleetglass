//! IPC-facing types. Everything the UI sees is defined here.
//!
//! TypeScript types are generated from these with specta (ADR 0001): every IPC
//! type adds `#[cfg_attr(feature = "specta", derive(specta::Type))]`, and
//! `cargo run -p fleetglass-app --bin gen-types` writes `ui/src/generated/`.

use serde::{Deserialize, Serialize};

pub mod audit;
pub use audit::*;
pub mod team_settings;
pub use team_settings::*;
pub mod plugins;
pub use plugins::*;
pub mod auth;
pub mod columns;
pub mod debug;
mod error;
pub mod import;
pub mod logs_v2;
pub mod overview;
pub mod prometheus;
pub mod update;

pub use crate::cert::CertificateInfo;
pub use auth::{AuthRequirement, DeviceCodeInfo};
pub use columns::*;
pub use error::{ErrorKind, IpcError};
pub use import::{ImportCandidate, ImportPreview, ImportProblem};
pub use logs_v2::*;
pub use overview::*;
pub use prometheus::*;
pub use update::UpdateInfo;

/// Build information shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    /// Operating system the backend runs on (`std::env::consts::OS`: "linux", "macos",
    /// "windows", "android", ...). The UI shows Android-only features (kubeconfig import) by it.
    pub platform: String,
}

/// How a kubeconfig user authenticates. Display-only: never carries secrets.
///
/// Serialized as `{ "type": "exec", "command": "aws" }`, `{ "type": "token" }`, ...
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthType {
    /// Bearer token (`token` or `tokenFile`).
    Token,
    /// TLS client certificate (`client-certificate[-data]`).
    ClientCert,
    /// Exec credential plugin, e.g. `aws`, `gke-gcloud-auth-plugin`, `kubelogin`.
    Exec { command: Option<String> },
    /// Legacy `auth-provider: { name: oidc }`.
    Oidc,
    /// Any other legacy `auth-provider` (e.g. `gcp`, `azure`).
    AuthProvider { name: String },
    /// Username and password.
    Basic,
    /// No credentials configured (or the user entry is missing).
    None,
}

/// One kubeconfig context, as shown in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ContextInfo {
    pub name: String,
    /// Cluster entry name referenced by the context.
    pub cluster: String,
    /// User entry name referenced by the context (kubeconfig allows it to be absent).
    pub user: Option<String>,
    pub namespace: Option<String>,
    /// API server URL of the referenced cluster, if that cluster entry exists.
    pub server: Option<String>,
    pub auth: AuthType,
    pub is_current: bool,
}

/// Result of listing kubeconfig contexts. Bounded by the size of the user's kubeconfig.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ContextList {
    /// Contexts sorted by name (like `kubectl config get-contexts`).
    pub contexts: Vec<ContextInfo>,
    /// `current-context` of the merged kubeconfig, if set.
    pub current: Option<String>,
    /// Non-fatal problems (skipped missing files, dangling references), user-readable.
    pub warnings: Vec<String>,
}

/// High-level health classification of a resource row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum RowStatus {
    Healthy,
    Pending,
    Degraded,
    Failed,
    Completed,
    Terminating,
    Unknown,
}

/// Typed cell value inside a projected resource [`Row`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Cell {
    Text(String),
    Int(i32),
    Timestamp(String),
    Ready {
        ready: u32,
        total: u32,
    },
    Status {
        label: String,
        phase: RowStatus,
    },
    /// Measured CPU (millicores) or memory (MiB) usage from `metrics.k8s.io`
    /// (02-01). Rows without a measurement (before the first poll, metrics
    /// unavailable, pod not reported yet) carry `Text("")` instead, never `0`.
    /// Boxed so the variant does not grow every [`Cell`] from 32 to 48 bytes.
    Usage(Box<UsageCell>),
}

/// Payload of [`Cell::Usage`]: serialized as
/// `{ "type": "usage", "value": { "value", "formatted", "request", "limit" } }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct UsageCell {
    /// Millicores for CPU, MiB for memory.
    pub value: u32,
    /// `value` as a compact quantity, e.g. `"250m"` or `"128Mi"`.
    pub formatted: String,
    /// Pod: sum of the containers' requests (`None` = no container sets one, or
    /// the pod is in a terminal phase). Node: always `None`.
    pub request: Option<u32>,
    /// Pod: sum of the containers' limits (`None` = at least one container is
    /// unlimited). Node: `allocatable`.
    pub limit: Option<u32>,
}

/// Compact projected representation of a Kubernetes resource for virtualised tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub uid: String,
    pub cluster: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub status: RowStatus,
    pub status_reason: String,
    pub cells: Vec<Cell>,
    pub labels: Vec<(String, String)>,
    /// Custom JSONPath column values (`P2-08`), index-aligned with the kind's
    /// `ColumnLayout::custom`; missing values are `Text("<none>")`.
    pub custom: Vec<Cell>,
}

/// Sort order direction for [`QuerySpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Column or attribute to sort [`Row`]s by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", content = "index", rename_all = "camelCase")]
pub enum SortField {
    Name,
    Namespace,
    Cluster,
    Status,
    Cell(u32),
    /// Index into [`Row::custom`] (layout order of the kind's custom columns).
    Custom(u32),
}

/// Sorting specification for a table query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct SortSpec {
    pub field: SortField,
    pub direction: SortDirection,
}

/// Helper deserializer to allow `clusters` to be specified as either a single
/// string (or with `alias = "cluster"`) or an array of strings, or omitted.
pub fn deserialize_clusters<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or sequence of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(vec![value.to_owned()])
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(vec![value])
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut items = Vec::new();
            while let Some(item) = seq.next_element()? {
                items.push(item);
            }
            Ok(items)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(Vec::new())
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

/// Windowed filter and sort query sent by the UI to the backend query engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct QuerySpec {
    /// Target clusters. May be single or multiple.
    #[serde(default, deserialize_with = "deserialize_clusters")]
    #[cfg_attr(feature = "specta", specta(type = Vec<String>))]
    pub clusters: Vec<String>,
    pub kinds: Vec<String>,
    pub namespaces: Vec<String>,
    pub label_selector: Option<String>,
    pub text_filter: Option<String>,
    pub sort: Option<SortSpec>,
    pub offset: u32,
    pub limit: u32,
    /// Keep only rows with one of these statuses; empty = no status filter.
    /// Duplicates are ignored, so at most the seven [`RowStatus`] variants count.
    #[serde(default)]
    pub statuses: Vec<RowStatus>,
}

impl QuerySpec {
    /// Convenience helper for a single-cluster query.
    #[must_use]
    pub fn for_cluster(cluster: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            clusters: vec![cluster.into()],
            kinds: vec![kind.into()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 50,
            statuses: Vec::new(),
        }
    }

    /// Convenience helper for a multi-cluster query.
    #[must_use]
    pub fn for_clusters(clusters: Vec<String>, kind: impl Into<String>) -> Self {
        Self {
            clusters,
            kinds: vec![kind.into()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 50,
            statuses: Vec::new(),
        }
    }
}

/// Per-status row counts of a [`Page`] (02-07b toolbar and status picker).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct StatusCounts {
    pub healthy: u32,
    pub pending: u32,
    pub degraded: u32,
    pub failed: u32,
    pub completed: u32,
    pub terminating: u32,
    pub unknown: u32,
}

impl StatusCounts {
    /// Counts one row with `status` (saturating).
    pub fn add(&mut self, status: RowStatus) {
        let slot = match status {
            RowStatus::Healthy => &mut self.healthy,
            RowStatus::Pending => &mut self.pending,
            RowStatus::Degraded => &mut self.degraded,
            RowStatus::Failed => &mut self.failed,
            RowStatus::Completed => &mut self.completed,
            RowStatus::Terminating => &mut self.terminating,
            RowStatus::Unknown => &mut self.unknown,
        };
        *slot = slot.saturating_add(1);
    }

    /// The count for `status`.
    #[must_use]
    pub const fn get(&self, status: RowStatus) -> u32 {
        match status {
            RowStatus::Healthy => self.healthy,
            RowStatus::Pending => self.pending,
            RowStatus::Degraded => self.degraded,
            RowStatus::Failed => self.failed,
            RowStatus::Completed => self.completed,
            RowStatus::Terminating => self.terminating,
            RowStatus::Unknown => self.unknown,
        }
    }
}

/// A single bounded window of [`Row`]s returned by [`crate::query::execute_query`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub total: u32,
    pub offset: u32,
    pub limit: u32,
    pub rows: Vec<Row>,
    pub version: u32,
    /// Access state of every `(cluster, kind)` store the query read, sorted by
    /// `(cluster, kind)`. Bounded by the number of matching stores. The UI shows
    /// these (e.g. "No permission to list pods") instead of treating an empty
    /// `rows` as "no objects".
    pub access: Vec<StoreAccess>,
    /// Rows per status after every filter except [`QuerySpec::statuses`], so the
    /// status picker can show what each choice would yield. `total` is after all
    /// filters.
    #[serde(default)]
    pub status_counts: StatusCounts,
}

impl Page {
    /// Returns the access states for a specific cluster.
    #[must_use]
    pub fn access_for_cluster(&self, cluster: &str) -> Vec<&StoreAccess> {
        self.access
            .iter()
            .filter(|a| a.cluster == cluster)
            .collect()
    }
}

/// Access state of one `(cluster, kind)` watch store, reported with each [`Page`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct StoreAccess {
    pub cluster: String,
    pub kind: String,
    pub access: StoreAccessState,
    /// Rows the store holds (all of them, before filters), so a non-ready state can
    /// say whether it is shown over last-known rows.
    #[serde(default)]
    pub rows: u32,
    /// `metrics.k8s.io` state behind the store's CPU / memory cells (02-01):
    /// `None` for kinds without usage columns (everything but `Pod` and `Node`).
    pub metrics: Option<MetricsAvailability>,
    /// `true` while the app is in the background (mobile, 03-04a): the watcher
    /// is stopped and `rows` are the last known ones; `access` is the state from
    /// before the pause. Always `false` on desktop. Omitted when `false`, so it is
    /// optional on both sides of the IPC.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
    /// Set when the cluster-wide list/watch was forbidden (HTTP 403) and the store
    /// is fed by per-namespace watchers instead (05-01): rows cover only these
    /// namespaces. `None` for a normal cluster-wide watch. Omitted when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<WatchScope>,
}

/// Restricted scope of a store whose cluster-wide watch is forbidden (05-01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct WatchScope {
    /// Namespaces watched one by one, sorted by name, each with its own state.
    /// Bounded by [`crate::ns_fallback::MAX_FALLBACK_NAMESPACES`].
    pub namespaces: Vec<NamespaceWatch>,
    /// Where the namespace candidates came from.
    pub source: NamespaceSource,
    /// Candidates whose rules review denied `list` or `watch`, sorted; not watched.
    pub denied: Vec<String>,
    /// `true` when there were more candidates than the cap and some were not checked.
    pub truncated: bool,
}

/// State of one per-namespace watcher of a [`WatchScope`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct NamespaceWatch {
    pub namespace: String,
    pub access: StoreAccessState,
}

/// Origin of the namespaces a restricted store watches (05-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum NamespaceSource {
    /// The context's `watchNamespaces` setting ([`ContextSettings::watch_namespaces`]).
    Configured,
    /// Listing namespaces was allowed; each was checked with a `SelfSubjectRulesReview`.
    Discovered,
    /// Listing namespaces was forbidden too: the kubeconfig context's namespace
    /// (or `default`) was checked as a last resort.
    ContextNamespace,
}

/// Row counts of one watched `(cluster, kind)` store, for the sidebar (02-07c).
/// Returned by `kind_counts`, one per store that already exists for the cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct KindCount {
    pub kind: String,
    /// Rows the store holds (all namespaces, no filters).
    pub total: u32,
    /// Rows whose [`RowStatus`] is `Failed` or `Degraded`.
    pub failing: u32,
    /// `true` only when the store's access state is [`StoreAccessState::Ready`]:
    /// otherwise the counts describe a partial, stale or empty-by-denial list
    /// and the UI should not present them as the cluster's real numbers.
    pub ready: bool,
}

/// Batched query update pushed over a subscription channel (at most 10/s).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PageDelta {
    pub subscription_id: u32,
    pub page: Page,
}

/// Live connection state of a kubeconfig context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Degraded { message: String },
    Error { message: String },
}

/// Per-context UI and safety preferences persisted by [`crate::settings::SettingsStore`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ContextSettings {
    pub colour: Option<String>,
    pub tags: Vec<String>,
    pub read_only: bool,
    pub protected: bool,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlocked_until: Option<u32>,
    /// Namespaces to watch one by one when a cluster-wide list/watch is forbidden
    /// (05-01). Empty: detect them (namespace list + access reviews, else the
    /// kubeconfig context's namespace). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_namespaces: Vec<String>,
}

impl ContextSettings {
    /// Returns `true` if this context is currently temporarily unlocked (`04-10`).
    pub fn is_unlocked(&self) -> bool {
        if let Some(expiry) = self.unlocked_until {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as u32)
                .unwrap_or(0);
            now < expiry
        } else {
            false
        }
    }

    /// Effective read-only status taking temporary unlocks into account.
    pub fn effective_read_only(&self) -> bool {
        self.read_only && !self.is_unlocked()
    }
}

/// Watch store RBAC or API availability state for honest UI feedback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum StoreAccessState {
    /// Rows reflect the cluster (initial list done, watch healthy).
    Ready,
    /// The watcher is connecting or running its first initial list. Rows already
    /// received are shown progressively, so the store may be incomplete:
    /// `rows_so_far` is its current row count (`0` while connecting).
    Loading {
        #[serde(rename = "rowsSoFar")]
        rows_so_far: u32,
    },
    /// The watcher of a store that already completed a list is restarting and
    /// re-listing. The rows are the last complete list (possibly outdated) and
    /// are replaced at once when the re-list finishes.
    Reconnecting,
    /// Live watching of this kind is not implemented yet; the store stays empty.
    Unsupported {
        kind: String,
        message: String,
    },
    /// Connecting or watching failed for a reason other than RBAC / missing API
    /// (unreachable, TLS, auth, kubeconfig). Rows, if any, may be stale.
    Error {
        message: String,
        /// Set when the store's first initial list failed partway: the number of
        /// rows received so far, which are kept visible but are an incomplete list.
        /// `None` when the store is empty or had completed a list before.
        #[serde(rename = "partialRows")]
        partial_rows: Option<u32>,
    },
    Forbidden {
        resource: String,
        namespace: Option<String>,
        message: String,
    },
    NotFound {
        resource: String,
        message: String,
    },
    /// Expired or missing cloud/SSO credentials from exec plugins or auth providers (`04-02`).
    AuthExpired {
        provider: String,
        hint: String,
        #[serde(rename = "loginCommand")]
        login_command: String,
        #[serde(rename = "deviceCode")]
        device_code: Option<crate::api::auth::DeviceCodeInfo>,
        #[serde(rename = "partialRows")]
        partial_rows: Option<u32>,
    },
}

impl StoreAccessState {
    /// Loading state of a store that has no rows yet (connecting / list not started).
    pub const CONNECTING: Self = Self::Loading { rows_so_far: 0 };

    /// `true` for [`StoreAccessState::Loading`], whatever its row count: the
    /// store has never completed a list, so its rows (if any) are incomplete.
    /// `false` for [`StoreAccessState::Reconnecting`], whose rows are a
    /// complete (if possibly outdated) list; see [`Self::is_syncing`].
    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }

    /// `true` while a list is in progress and the store is expected to become
    /// [`StoreAccessState::Ready`] on its own: [`StoreAccessState::Loading`] or
    /// [`StoreAccessState::Reconnecting`].
    #[must_use]
    pub fn is_syncing(&self) -> bool {
        matches!(self, Self::Loading { .. } | Self::Reconnecting)
    }
}

/// Mutating action requested against a Kubernetes cluster context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MutatingAction {
    Delete {
        kind: String,
        namespace: Option<String>,
        name: String,
    },
    Scale {
        kind: String,
        namespace: String,
        name: String,
        replicas: u32,
    },
    Restart {
        kind: String,
        namespace: String,
        name: String,
    },
    Cordon {
        node: String,
    },
    Uncordon {
        node: String,
    },
    Drain {
        node: String,
    },
    FluxReconcile {
        kind: String,
        namespace: String,
        name: String,
    },
    FluxSuspend {
        kind: String,
        namespace: String,
        name: String,
        suspend: bool,
    },
    ArgoRolloutPromote {
        namespace: String,
        name: String,
        full: bool,
    },
    ArgoRolloutAbort {
        namespace: String,
        name: String,
    },
    ArgoRolloutRetry {
        namespace: String,
        name: String,
    },
    ArgoAppSync {
        namespace: String,
        name: String,
    },
    CertRenew {
        namespace: String,
        name: String,
    },
    ExternalSecretForceSync {
        namespace: String,
        name: String,
    },
}

/// Request payload for executing or previewing a [`MutatingAction`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ActionRequest {
    pub context: String,
    pub action: MutatingAction,
    pub confirmed_name: Option<String>,
}

/// Top-level controller reference for a workload (e.g. Deployment, StatefulSet, DaemonSet, Job).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ControllerRef {
    pub kind: String,
    pub name: String,
    pub namespace: String,
}

/// One indexed line from a pod/container log stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub seq: u32,
    pub text: String,
}

/// Windowed page of log lines returned by a log buffer query or regex search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub total_lines: u32,
    pub matched_lines: u32,
    pub offset: u32,
    pub limit: u32,
    pub lines: Vec<LogLine>,
}

/// Outcome of a single diagnostic check step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticStatus {
    Pass,
    Warn,
    Fail,
}

/// One step in a structured connection diagnostic report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticStep {
    pub name: String,
    pub status: DiagnosticStatus,
    pub detail: String,
}

/// Result of a `SelfSubjectAccessReview` check for a resource and verb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct RbacCheck {
    pub resource: String,
    pub verb: String,
    pub allowed: bool,
}

/// Complete connection and RBAC diagnostic report for a kubeconfig context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReport {
    pub context: String,
    pub steps: Vec<DiagnosticStep>,
    pub rbac: Vec<RbacCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_error: Option<AuthRequirement>,
}

/// Active local port-forward session exposed to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PortForwardInfo {
    pub id: u32,
    pub context: String,
    pub namespace: String,
    pub pod: String,
    pub remote_port: u16,
    pub local_port: u16,
    pub browser_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub active_connections: u32,
    #[serde(default)]
    #[cfg_attr(feature = "specta", specta(type = u32))]
    pub total_connections: u64,
}

/// Persisted port-forward bookmark (`04-08`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct SavedPortForward {
    pub id: String,
    pub context: String,
    pub namespace: String,
    pub target_kind: String,
    pub target_name: String,
    pub container_port: u16,
    pub local_port: u16,
    pub label: String,
}

/// Whether `metrics.k8s.io` (`metrics-server`) is available on a cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum MetricsAvailability {
    /// Not polled yet: the store has no live subscriber, or the first poll is
    /// in flight. Usage cells are empty.
    Pending,
    Available,
    /// `metrics.k8s.io` answered 404 / 503 (metrics-server absent or not running).
    NotInstalled {
        message: String,
    },
    /// RBAC denies listing the metrics (HTTP 403).
    Forbidden {
        message: String,
    },
    /// Any other failure (unreachable, timeout, unparsable response).
    Error {
        message: String,
    },
}

/// Parsed CPU and memory usage for a pod or node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ResourceUsage {
    pub cpu_millicores: u32,
    pub memory_mib: u32,
    pub cpu_formatted: String,
    pub memory_formatted: String,
}

/// Reference to a related Kubernetes object (e.g. an `ownerReference`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct RelatedRef {
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    pub uid: String,
    pub relationship: String,
}

/// Full resource detail and cleaned YAML representation for the detail drawer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ResourceDetail {
    pub uid: String,
    pub cluster: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub yaml: String,
    pub related: Vec<RelatedRef>,
    #[serde(default)]
    pub events: Vec<Row>,
    #[serde(default)]
    pub events_truncated: bool,
    #[serde(default)]
    pub pod: Option<PodSummary>,
    #[serde(default)]
    pub certificates: Vec<CertificateInfo>,
    /// Browser endpoints of an Ingress, Gateway or HTTPRoute, capped at 50 (`05-05`).
    #[serde(default)]
    pub endpoints: Vec<ResourceEndpoint>,
    #[serde(default)]
    pub endpoints_truncated: bool,
}

/// A URL derived from an Ingress / Gateway / HTTPRoute spec (`05-05`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ResourceEndpoint {
    /// Normalised URL, or a display pattern such as `https://*.example.com/` when not openable.
    pub url: String,
    /// True when `url` passes the browser launcher's validation.
    pub openable: bool,
    /// Where the endpoint came from, e.g. `rule app.example.com` or `listener web`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PodSummary {
    pub node: String,
    pub pod_ip: String,
    pub qos_class: String,
    pub service_account: String,
    pub priority_class: Option<String>,
    pub conditions: Vec<ConditionSummary>,
    pub tolerations: Vec<String>,
    pub containers: Vec<ContainerSummary>,
    pub uses: Vec<RelatedRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ConditionSummary {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ResourcePair {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ContainerPortSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub container_port: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ContainerSummary {
    pub name: String,
    pub image: String,
    pub init: bool,
    pub state: String, // "Running" | "Waiting" | "Terminated"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    pub ready: bool,
    pub restarts: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_termination: Option<String>, // e.g. "Error (1)" or "OOMKilled (137)"
    pub ports: Vec<ContainerPortSummary>,
    pub requests: ResourcePair,
    pub limits: ResourcePair,
}

/// Server-side dry-run diff and equivalent `kubectl` command preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DryRunResult {
    pub diff: String,
    pub kubectl_preview: String,
}

/// Specification for opening an interactive container shell session (`P1-09`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ExecSpec {
    pub context: String,
    pub namespace: String,
    pub pod: String,
    pub container: Option<String>,
    pub command: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

/// Specification for attaching to PID 1 of a running container (`04-03c`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AttachSpec {
    pub context: String,
    pub namespace: String,
    pub pod: String,
    pub container: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

/// A named group of Kubernetes cluster contexts (`P2-01`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ClusterGroup {
    pub name: String,
    pub contexts: Vec<String>,
}

/// Summary information for an in-cluster Helm v3 release (`P2-07`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct HelmReleaseSummary {
    pub name: String,
    pub namespace: String,
    pub revision: u32,
    pub updated: String,
    pub status: String,
    pub chart: String,
    pub app_version: String,
}

/// Full detail of an in-cluster Helm v3 release (`P2-07`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct HelmReleaseDetail {
    pub summary: HelmReleaseSummary,
    pub values_yaml: String,
    pub manifest_yaml: String,
    pub notes: String,
}

/// One bounded window of the Helm release list (latest revision per release) (`P2-07`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct HelmReleasePage {
    /// Number of releases before windowing.
    pub total: u32,
    pub offset: u32,
    pub releases: Vec<HelmReleaseSummary>,
}

/// Line diff between two Helm release revisions (`P2-07`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct HelmRevisionDiff {
    pub values_diff: String,
    pub manifest_diff: String,
}

/// Result of comparing two resources across clusters or namespaces (`P2-02`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DiffResult {
    pub unified_diff: String,
    pub identical: bool,
}

/// Options for resource comparison and diff generation (`04-04`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct CompareOptions {
    /// When true, preserves runtime metadata and status; default false (strips noise).
    #[serde(default)]
    pub include_metadata_and_status: bool,
    /// When true, collapses identical context blocks in the unified diff; default false.
    #[serde(default)]
    pub only_differences: bool,
    /// When true, decodes and reveals Secret values; default false (redacted).
    #[serde(default)]
    pub reveal_secrets: bool,
}

/// An individual Kubernetes resource component within an application group (`P2-05`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AppResource {
    pub cluster: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub status: RowStatus,
    pub status_reason: String,
}

/// Summary of an aggregated application workload and its constituent components (`P2-05`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AppSummary {
    pub name: String,
    pub namespace: String,
    pub cluster: String,
    pub components: Vec<AppResource>,
}

/// Windowed query for the application view (`P2-05`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AppQuerySpec {
    /// Cluster contexts to include; empty means all loaded clusters.
    #[serde(default, deserialize_with = "deserialize_clusters")]
    #[cfg_attr(feature = "specta", specta(type = Vec<String>))]
    pub clusters: Vec<String>,
    /// Namespaces to include; empty means all namespaces.
    pub namespaces: Vec<String>,
    /// Case-insensitive substring match on the application name.
    pub text_filter: Option<String>,
    pub offset: u32,
    pub limit: u32,
}

/// A single bounded window of [`AppSummary`]s sorted by `(cluster, namespace, name)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AppPage {
    /// Number of applications matching the query before windowing.
    pub total: u32,
    pub offset: u32,
    pub apps: Vec<AppSummary>,
    /// Access state of every `(cluster, kind)` store the apps were grouped from,
    /// sorted by `(cluster, kind)` (see [`Page::access`]). Lets the UI say
    /// "No permission to list pods" instead of showing an empty app list.
    pub access: Vec<StoreAccess>,
}

impl AppPage {
    /// Returns the access states for a specific cluster.
    #[must_use]
    pub fn access_for_cluster(&self, cluster: &str) -> Vec<&StoreAccess> {
        self.access
            .iter()
            .filter(|a| a.cluster == cluster)
            .collect()
    }
}
