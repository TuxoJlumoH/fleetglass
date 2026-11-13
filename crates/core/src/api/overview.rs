//! Overview page IPC types (02-08a).

use serde::{Deserialize, Serialize};

use super::{ConnectionStatus, MetricsAvailability, RowStatus};

/// Overview data for all connected clusters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct OverviewData {
    pub clusters: Vec<ClusterSummary>,
    pub attention: Attention,
    pub recent_warnings: Vec<WarningEvent>,
    #[serde(default)]
    pub right_sizing: Option<RightSizingReport>,
}

/// High-level summary of a single connected cluster.
///
/// Counts are `None` when they are not known (honest UI, non-negotiable 5):
/// the backing store is still loading, was denied (RBAC), is missing or failed.
/// `None` must never be shown as `0`; [`Self::problems`] says why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ClusterSummary {
    pub cluster: String,
    pub connection: ConnectionStatus,
    /// Nodes whose status starts with `Ready`; `None` unless the Node store has data.
    pub nodes_ready: Option<u32>,
    /// `None` unless the Node store has data (`Ready` / `Reconnecting`).
    pub nodes_total: Option<u32>,
    /// `None` unless the Pod store has data (same for `pods_pending` / `pods_failing`).
    pub pods_running: Option<u32>,
    pub pods_pending: Option<u32>,
    pub pods_failing: Option<u32>,
    /// Warning events seen in the last hour; `None` unless the Event store has data.
    pub warning_events_1h: Option<u32>,
    pub metrics: MetricsAvailability,
    /// `None` unless metrics are available and both the Node and Pod stores have data.
    pub cpu: Option<CapacitySummary>,
    /// `None` unless metrics are available and both the Node and Pod stores have data.
    pub memory: Option<CapacitySummary>,
    /// Every overview kind whose store is denied, missing, unsupported or failed, in
    /// `OVERVIEW_KINDS` order (at most one entry per kind, so bounded).
    pub problems: Vec<KindProblem>,
}

/// Why the overview has no data for one resource kind of a cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct KindProblem {
    /// Resource kind, e.g. `"Node"` or `"Event"`.
    pub kind: String,
    /// User-readable reason, e.g. `"No permission to list nodes"`.
    pub message: String,
}

/// Aggregated resource capacity, usage and requests for a cluster.
///
/// Units: CPU summaries are in millicores, memory summaries in MiB (both for
/// the raw numbers; the `formatted_*` strings carry their unit).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct CapacitySummary {
    /// Measured usage (CPU: millicores, memory: MiB) from `metrics.k8s.io`: the sum
    /// over nodes, or over pods when no node is measured.
    #[cfg_attr(feature = "specta", specta(type = u32))]
    pub used: u64,
    /// Sum of the requests of scheduled, non-terminal pods (CPU: millicores,
    /// memory: MiB), like `kubectl describe node`'s "Allocated resources".
    #[cfg_attr(feature = "specta", specta(type = u32))]
    pub requested: u64,
    /// Sum of the nodes' `allocatable` (CPU: millicores, memory: MiB).
    #[cfg_attr(feature = "specta", specta(type = u32))]
    pub allocatable: u64,
    /// `used` as a compact quantity, e.g. `"1500m"` or `"2048Mi"`.
    pub formatted_used: String,
    /// `requested` as a compact quantity.
    pub formatted_requested: String,
    /// `allocatable` as a compact quantity.
    pub formatted_allocatable: String,
    /// `used` as a percentage of `allocatable` (0 when `allocatable` is 0).
    pub used_pct: u8,
    /// `requested` as a percentage of `allocatable` (0 when `allocatable` is 0).
    pub requested_pct: u8,
}

/// Workloads and pods needing human attention across connected clusters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    pub items: Vec<AttentionItem>,
    pub truncated: bool,
}

/// A single failing or degraded workload or pod needing attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AttentionItem {
    pub cluster: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub reason: String,
    pub message: String,
    pub since: Option<String>,
    pub status: RowStatus,
}

/// A recent warning event across connected clusters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct WarningEvent {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    pub involved_kind: String,
    pub involved_name: String,
    pub reason: String,
    pub message: String,
    pub count: i32,
    pub last_timestamp: Option<String>,
    pub age: String,
}

/// Right-sizing report computed across connected clusters (`04-09`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct RightSizingReport {
    /// Whether metrics.k8s.io is available across queried clusters.
    pub metrics_available: bool,
    /// Honest notice when metrics are not installed, forbidden, or unavailable.
    pub notice: Option<String>,
    /// Containers/pods near limit (≥ 80 % limit).
    pub near_limit: Vec<RightSizingItem>,
    /// Containers/pods over request (> 100 % request).
    pub over_request: Vec<RightSizingItem>,
    /// Containers/pods with unused request (< threshold % of request).
    pub unused_requests: Vec<RightSizingItem>,
    /// Pods running without CPU or memory requests/limits configured.
    pub missing_resources: Vec<RightSizingItem>,
    /// Configured threshold for unused requests (e.g. 10, 20, 50).
    pub threshold_pct: u8,
}

/// A workload/pod candidate surfaced in right-sizing analysis (`04-09`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct RightSizingItem {
    pub cluster: String,
    pub namespace: String,
    pub name: String,
    pub kind: String,
    /// Resource type: `"cpu"` or `"memory"`.
    pub resource: String,
    /// Measured usage (millicores or MiB), 0 if unmeasured.
    pub usage: u32,
    pub formatted_usage: String,
    /// Configured request (millicores or MiB), if set.
    pub request: Option<u32>,
    pub formatted_request: String,
    /// Configured limit (millicores or MiB), if set.
    pub limit: Option<u32>,
    pub formatted_limit: String,
    /// Percentage of limit or request (e.g. 85 for 85%).
    pub percent: Option<u32>,
    /// Wasted amount (request - usage) for unused requests.
    pub wasted: Option<u32>,
    pub formatted_wasted: String,
    /// Human-readable risk/status label (e.g. `"OOMKill risk"`, `"Throttling risk"`).
    pub label: String,
}
