//! IPC types for workload logs that follow pods across rollouts (`P2-03`).

use serde::{Deserialize, Serialize};

/// Which pods of a workload to follow.
///
/// Serialized as `{ "type": "pod", "name": "counter" }`,
/// `{ "type": "labels", "selector": "app=web" }` or
/// `{ "type": "owner", "kind": "Deployment", "name": "web" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkloadSelector {
    /// A single pod by name.
    Pod { name: String },
    /// A Kubernetes label selector string, e.g. `app=web,tier!=cache`.
    Labels { selector: String },
    /// A `Deployment`, `StatefulSet` or `DaemonSet`; its `spec.selector` is used.
    Owner { kind: String, name: String },
}

/// Target of a multiplexed workload log stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct WorkloadLogTarget {
    pub cluster: String,
    pub namespace: String,
    pub selector: WorkloadSelector,
    /// Only follow this container; `None` follows every container of each pod.
    pub container: Option<String>,
    /// Initial history per container for pods present when the stream starts.
    /// Pods that join later are streamed from their first line.
    pub tail_lines: Option<u32>,
    /// Only lines newer than this many seconds, for pods present at start;
    /// combined with `tail_lines` the API applies both. `0` means no limit;
    /// values above 7 days are capped.
    #[serde(default)]
    pub since_seconds: Option<u32>,
    /// Logs of the previous (terminated) instance of each container, like
    /// `kubectl logs --previous`. Only valid with [`WorkloadSelector::Pod`].
    /// The stream is a one-shot fetch: lines (honouring `tail_lines`,
    /// `since_seconds` and `container`) are sent, then the stream ends.
    #[serde(default)]
    pub previous: bool,
}

/// One log line tagged with its source pod and container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct MuxLogLine {
    /// Global, monotonically increasing (wrapping) sequence number across all pods.
    pub seq: u32,
    pub pod: String,
    pub container: String,
    pub text: String,
    /// `true` when the trimmed text parses as a JSON object.
    pub json: bool,
}

/// Result of saving the loaded log lines to a file (`save_log_file`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct SavedFile {
    /// Absolute path of the newly created file.
    pub path: String,
}

/// Event pushed over a workload log channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MuxLogEvent {
    /// A batch of at most 500 lines, in `seq` order.
    Lines { lines: Vec<MuxLogLine> },
    /// A pod started being followed.
    PodJoined { pod: String },
    /// A pod is no longer followed (deleted, or none of its containers run).
    PodLeft { pod: String },
    /// A user-readable, non-fatal or fatal problem.
    Error { message: String },
}
