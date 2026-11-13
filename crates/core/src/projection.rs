//! Pure row projection and `kubectl`-accurate status calculation.
//!
//! Converts full Kubernetes objects from the watch cache into compact [`Row`]
//! values so the backend stores only the columns and metadata needed for
//! filtering, sorting, and virtualised table rendering.

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ContainerStateTerminated, ContainerStatus, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, Time};

use serde_json::Value;

use crate::api::{Cell, Row, RowStatus, StoreAccessState};
use crate::columns::{CustomColumns, evaluate_custom_cells};
use crate::metrics::pod_usage_cells;

/// Default column headers for projected [`Pod`] rows, in cell order.
pub const POD_COLUMNS: &[&str] = &[
    "Ready",
    "Status",
    "Restarts",
    "LastRestart",
    "LastReason",
    "Node",
    "IP",
    "CreationTimestamp",
    "CPU",
    "Memory",
];

/// Default column headers for projected [`Deployment`] rows, in cell order.
pub const DEPLOYMENT_COLUMNS: &[&str] = &[
    "Ready",
    "UpToDate",
    "Available",
    "Status",
    "CreationTimestamp",
];

/// Default column headers for projected [`Job`] rows, in cell order.
pub const JOB_COLUMNS: &[&str] = &["Completions", "Status", "CreationTimestamp"];

/// Projects a [`Pod`] into a compact [`Row`] for the given `cluster`.
pub fn project_pod(pod: &Pod, cluster: &str) -> Row {
    let uid = pod.metadata.uid.clone().unwrap_or_default();
    let name = pod.metadata.name.clone().unwrap_or_default();
    let namespace = pod.metadata.namespace.clone();

    let mut labels: Vec<(String, String)> = pod
        .metadata
        .labels
        .as_ref()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    labels.sort();

    let (status_reason, status_phase, ready, total, restarts) = compute_pod_status(pod);

    let node_name = pod
        .spec
        .as_ref()
        .and_then(|spec| spec.node_name.clone())
        .unwrap_or_default();
    let pod_ip = pod
        .status
        .as_ref()
        .and_then(|status| status.pod_ip.clone())
        .unwrap_or_default();
    let creation_timestamp_rfc3339 = pod
        .metadata
        .creation_timestamp
        .as_ref()
        .map(|ts| ts.0.to_string())
        .unwrap_or_default();
    let (last_restart, last_reason) = match last_termination(pod) {
        Some((finished_at, label)) => (Cell::Timestamp(finished_at), Cell::Text(label)),
        None => (Cell::Text(String::new()), Cell::Text(String::new())),
    };

    // CPU / Memory (02-01): request/limit from the spec; the measured value is
    // patched in by the metrics poller (see `crate::metrics`).
    let [cpu, memory] = pod_usage_cells(pod);
    let cells = vec![
        Cell::Ready { ready, total },
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Int(restarts),
        last_restart,
        last_reason,
        Cell::Text(node_name),
        Cell::Text(pod_ip),
        Cell::Timestamp(creation_timestamp_rfc3339),
        cpu,
        memory,
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Pod".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Projects a [`Pod`] like [`project_pod`] and evaluates `columns` into [`Row::custom`].
pub fn project_pod_with_columns(pod: &Pod, cluster: &str, columns: &CustomColumns) -> Row {
    let mut row = project_pod(pod, cluster);
    row.custom = pod_custom_cells(pod, columns);
    row
}

/// Evaluates `columns` over `pod`.
///
/// Only the top-level parts the paths read (`metadata`, `spec`, `status`) are
/// converted to JSON, and the JSON is dropped right after evaluation; the whole
/// object is converted only for paths that start elsewhere (e.g. `.kind`, `[*]`).
pub fn pod_custom_cells(pod: &Pod, columns: &CustomColumns) -> Vec<Cell> {
    if columns.is_empty() {
        return Vec::new();
    }
    let mut parts = serde_json::Map::new();
    let mut needs_full = false;
    for path in columns.paths() {
        let Some(key) = path.root_field() else {
            needs_full = true;
            break;
        };
        if parts.contains_key(key) {
            continue;
        }
        let part = match key {
            "metadata" => serde_json::to_value(&pod.metadata),
            "spec" => serde_json::to_value(&pod.spec),
            "status" => serde_json::to_value(&pod.status),
            _ => {
                needs_full = true;
                break;
            }
        };
        parts.insert(key.to_owned(), part.unwrap_or(Value::Null));
    }
    let obj = if needs_full {
        serde_json::to_value(pod).unwrap_or(Value::Null)
    } else {
        Value::Object(parts)
    };
    evaluate_custom_cells(&obj, columns.paths())
}

/// Computes `(status_reason, status_phase, ready_count, total_count, restarts)`
/// for a [`Pod`] following `kubectl get pods` rules.
pub fn compute_pod_status(pod: &Pod) -> (String, RowStatus, u32, u32, i32) {
    let total_count = pod.spec.as_ref().map_or(0, |spec| {
        u32::try_from(spec.containers.len()).unwrap_or(u32::MAX)
    });

    let container_statuses = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_deref())
        .unwrap_or(&[]);

    let mut ready_count: u32 = 0;
    let mut restarts: i32 = 0;
    for cs in container_statuses {
        if cs.ready {
            ready_count = ready_count.saturating_add(1);
        }
        restarts = restarts.saturating_add(cs.restart_count.max(0));
    }

    // 1. Deletion timestamp takes highest precedence.
    if pod.metadata.deletion_timestamp.is_some() {
        return (
            "Terminating".to_owned(),
            RowStatus::Terminating,
            ready_count,
            total_count,
            restarts,
        );
    }

    // 2. Inspect init container statuses in order.
    let init_statuses = pod
        .status
        .as_ref()
        .and_then(|s| s.init_container_statuses.as_deref())
        .unwrap_or(&[]);
    if !init_statuses.is_empty() {
        let spec_init_total = pod
            .spec
            .as_ref()
            .and_then(|spec| spec.init_containers.as_ref())
            .map_or(0, Vec::len);
        let total_init = spec_init_total.max(init_statuses.len());

        if let Some((reason, phase)) = inspect_init_containers(init_statuses, total_init) {
            return (reason, phase, ready_count, total_count, restarts);
        }
    }

    // 3. Inspect regular container statuses.
    if let Some((reason, phase)) = inspect_containers(container_statuses) {
        return (reason, phase, ready_count, total_count, restarts);
    }

    // 4. Fall back to pod status.reason or status.phase.
    let (reason, phase) = fallback_pod_phase(pod, ready_count, total_count);
    (reason, phase, ready_count, total_count, restarts)
}

fn inspect_init_containers(
    init_statuses: &[ContainerStatus],
    total_init: usize,
) -> Option<(String, RowStatus)> {
    for (i, cs) in init_statuses.iter().enumerate() {
        let Some(state) = cs.state.as_ref() else {
            return Some((format!("Init:{i}/{total_init}"), RowStatus::Pending));
        };

        if let Some(terminated) = state.terminated.as_ref() {
            if terminated.exit_code == 0 {
                continue;
            }
            let reason = terminated
                .reason
                .as_deref()
                .filter(|r| !r.is_empty())
                .map_or_else(|| "Init:Error".to_owned(), |r| format!("Init:{r}"));
            return Some((reason, RowStatus::Failed));
        }

        if let Some(waiting) = state.waiting.as_ref()
            && let Some(reason) = waiting.reason.as_deref()
            && !reason.is_empty()
            && reason != "PodInitializing"
        {
            let phase = match reason {
                "ContainerCreating" => RowStatus::Pending,
                _ => RowStatus::Failed,
            };
            return Some((format!("Init:{reason}"), phase));
        }

        return Some((format!("Init:{i}/{total_init}"), RowStatus::Pending));
    }

    None
}

fn inspect_containers(container_statuses: &[ContainerStatus]) -> Option<(String, RowStatus)> {
    let mut completed_reason: Option<(String, RowStatus)> = None;
    let mut has_running = false;

    for cs in container_statuses {
        let Some(state) = cs.state.as_ref() else {
            continue;
        };

        if let Some(waiting) = state.waiting.as_ref()
            && let Some(reason) = waiting.reason.as_deref()
            && !reason.is_empty()
        {
            let phase = match reason {
                "ContainerCreating" | "PodInitializing" => RowStatus::Pending,
                _ => RowStatus::Failed,
            };
            return Some((reason.to_owned(), phase));
        }

        if let Some(terminated) = state.terminated.as_ref() {
            if let Some(reason) = terminated.reason.as_deref()
                && !reason.is_empty()
            {
                if reason == "Completed" {
                    completed_reason = Some((reason.to_owned(), RowStatus::Completed));
                } else {
                    return Some((reason.to_owned(), RowStatus::Failed));
                }
            } else if terminated.exit_code != 0 {
                return Some(("Error".to_owned(), RowStatus::Failed));
            } else {
                completed_reason = Some(("Completed".to_owned(), RowStatus::Completed));
            }
        }

        if state.running.is_some() {
            has_running = true;
        }
    }

    if !has_running {
        return completed_reason;
    }

    None
}

fn fallback_pod_phase(pod: &Pod, ready: u32, total: u32) -> (String, RowStatus) {
    let Some(status) = pod.status.as_ref() else {
        return ("Unknown".to_owned(), RowStatus::Unknown);
    };

    if let Some(reason) = status.reason.as_deref()
        && !reason.is_empty()
    {
        let phase = match status.phase.as_deref() {
            Some("Succeeded") => RowStatus::Completed,
            Some("Pending") => RowStatus::Pending,
            _ => RowStatus::Failed,
        };
        return (reason.to_owned(), phase);
    }

    match status.phase.as_deref() {
        Some("Running") => {
            let phase = if ready == total && total > 0 {
                RowStatus::Healthy
            } else {
                RowStatus::Pending
            };
            ("Running".to_owned(), phase)
        }
        Some("Succeeded") => ("Completed".to_owned(), RowStatus::Completed),
        Some("Failed") => ("Failed".to_owned(), RowStatus::Failed),
        // Deliberate difference from kubectl, which prints "Pending" here: a pod
        // the scheduler has rejected (PodScheduled=False, reason Unschedulable)
        // and that has no node will not start on its own, so we say so.
        Some("Pending") if is_unschedulable(pod) => {
            ("Unschedulable".to_owned(), RowStatus::Pending)
        }
        Some("Pending") => ("Pending".to_owned(), RowStatus::Pending),
        _ => ("Unknown".to_owned(), RowStatus::Unknown),
    }
}

fn is_unschedulable(pod: &Pod) -> bool {
    let has_node = pod
        .spec
        .as_ref()
        .and_then(|spec| spec.node_name.as_deref())
        .is_some_and(|n| !n.is_empty());
    if has_node {
        return false;
    }
    pod.status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[])
        .iter()
        .any(|c| {
            c.type_ == "PodScheduled"
                && c.status == "False"
                && c.reason.as_deref() == Some("Unschedulable")
        })
}

/// Returns `(finished_at, label)` for the container whose last termination
/// (`lastState.terminated`) finished most recently, if any.
fn last_termination(pod: &Pod) -> Option<(String, String)> {
    let container_statuses = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_deref())
        .unwrap_or(&[]);

    let mut latest: Option<(&Time, &ContainerStateTerminated)> = None;
    for cs in container_statuses {
        let Some(terminated) = cs.last_state.as_ref().and_then(|s| s.terminated.as_ref()) else {
            continue;
        };
        let Some(finished_at) = terminated.finished_at.as_ref() else {
            continue;
        };
        if latest.is_none_or(|(best, _)| finished_at.0 > best.0) {
            latest = Some((finished_at, terminated));
        }
    }

    latest.map(|(finished_at, terminated)| {
        let code = terminated.exit_code;
        let label = match terminated.reason.as_deref().filter(|r| !r.is_empty()) {
            Some(reason) => format!("{reason} ({code})"),
            None => format!("exit {code}"),
        };
        (finished_at.0.to_string(), label)
    })
}

fn extract_metadata(
    meta: &ObjectMeta,
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

/// Projects a [`Deployment`] into a compact [`Row`] for the given `cluster`.
pub fn project_deployment(deploy: &Deployment, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_metadata(&deploy.metadata);

    let desired_i32 = deploy
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(1)
        .max(0);
    let ready_i32 = deploy
        .status
        .as_ref()
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0)
        .max(0);
    let updated_i32 = deploy
        .status
        .as_ref()
        .and_then(|s| s.updated_replicas)
        .unwrap_or(0)
        .max(0);
    let available_i32 = deploy
        .status
        .as_ref()
        .and_then(|s| s.available_replicas)
        .unwrap_or(0)
        .max(0);

    let desired = u32::try_from(desired_i32).unwrap_or(0);
    let ready = u32::try_from(ready_i32).unwrap_or(0);
    let updated = u32::try_from(updated_i32).unwrap_or(0);
    let available = u32::try_from(available_i32).unwrap_or(0);

    let (status_reason, status_phase) = if deploy.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if let Some(cond) = deploy
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[])
        .iter()
        .find(|c| c.type_ == "Progressing" && c.status == "False")
    {
        let reason = cond
            .reason
            .as_deref()
            .filter(|r| !r.is_empty())
            .unwrap_or("ProgressDeadlineExceeded");
        (reason.to_owned(), RowStatus::Failed)
    } else if ready == desired && available == desired {
        ("Available".to_owned(), RowStatus::Healthy)
    } else {
        ("Progressing".to_owned(), RowStatus::Pending)
    };

    let cells = vec![
        Cell::Ready {
            ready,
            total: desired,
        },
        Cell::Int(i32::try_from(updated).unwrap_or(i32::MAX)),
        Cell::Int(i32::try_from(available).unwrap_or(i32::MAX)),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Deployment".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Projects a [`Job`] into a compact [`Row`] for the given `cluster`.
pub fn project_job(job: &Job, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_metadata(&job.metadata);

    let completions_i32 = job
        .spec
        .as_ref()
        .and_then(|s| s.completions)
        .unwrap_or(1)
        .max(0);
    let succeeded_i32 = job
        .status
        .as_ref()
        .and_then(|s| s.succeeded)
        .unwrap_or(0)
        .max(0);
    let failed_i32 = job
        .status
        .as_ref()
        .and_then(|s| s.failed)
        .unwrap_or(0)
        .max(0);
    let active_i32 = job
        .status
        .as_ref()
        .and_then(|s| s.active)
        .unwrap_or(0)
        .max(0);

    let completions = u32::try_from(completions_i32).unwrap_or(0);
    let succeeded = u32::try_from(succeeded_i32).unwrap_or(0);
    let failed = u32::try_from(failed_i32).unwrap_or(0);
    let active = u32::try_from(active_i32).unwrap_or(0);

    let conditions = job
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);

    let (status_reason, status_phase) = if job.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if let Some(cond) = conditions
        .iter()
        .find(|c| c.type_ == "Failed" && c.status == "True")
    {
        let reason = cond
            .reason
            .as_deref()
            .filter(|r| !r.is_empty())
            .unwrap_or("Failed");
        (reason.to_owned(), RowStatus::Failed)
    } else if conditions
        .iter()
        .any(|c| c.type_ == "Complete" && c.status == "True")
        || succeeded >= completions
    {
        ("Complete".to_owned(), RowStatus::Completed)
    } else if active > 0 {
        ("Running".to_owned(), RowStatus::Pending)
    } else if failed > 0 {
        ("BackOff".to_owned(), RowStatus::Degraded)
    } else {
        ("Pending".to_owned(), RowStatus::Pending)
    };

    let cells = vec![
        Cell::Ready {
            ready: succeeded,
            total: completions,
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
        kind: "Job".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Maps a Kubernetes API error (403 Forbidden, 404 NotFound, or AuthExpired) to an honest [`StoreAccessState`].
pub fn classify_kube_error(
    err: &kube::Error,
    resource: &str,
    namespace: Option<&str>,
) -> Option<StoreAccessState> {
    if let kube::Error::Auth(kube::client::AuthError::AuthExecRun { cmd, out, .. }) = err {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let combined = format!("{stderr}\n{stdout}\n{cmd}");
        if let Some(auth_req) = crate::diagnostics::detect_auth_error(&combined) {
            return Some(StoreAccessState::AuthExpired {
                provider: auth_req.provider,
                hint: format!("Run '{}' to re-authenticate", auth_req.login_command),
                login_command: auth_req.login_command,
                device_code: auth_req.device_code,
                partial_rows: None,
            });
        }
    }
    let kube::Error::Api(status) = err else {
        return None;
    };
    match status.code {
        403 => {
            let message = match namespace {
                Some(ns) => format!("No permission to list {resource} in namespace \"{ns}\""),
                None => format!("No permission to list {resource}"),
            };
            Some(StoreAccessState::Forbidden {
                resource: resource.to_owned(),
                namespace: namespace.map(str::to_owned),
                message,
            })
        }
        404 => Some(StoreAccessState::NotFound {
            resource: resource.to_owned(),
            message: format!("{resource} API is not installed on this cluster"),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use k8s_openapi::api::core::v1::{
        Container, ContainerState, ContainerStateRunning, ContainerStateTerminated,
        ContainerStateWaiting, ContainerStatus, Pod, PodSpec, PodStatus,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, Time};

    use super::*;

    fn base_pod(containers: usize) -> Pod {
        let mut labels = BTreeMap::new();
        labels.insert("z-key".to_owned(), "2".to_owned());
        labels.insert("a-key".to_owned(), "1".to_owned());

        let creation: Time = serde_json::from_str("\"2025-01-15T10:20:30Z\"").unwrap();

        Pod {
            metadata: ObjectMeta {
                uid: Some("pod-uid-1".to_owned()),
                name: Some("api-7f8b9-x12".to_owned()),
                namespace: Some("production".to_owned()),
                creation_timestamp: Some(creation),
                labels: Some(labels),
                ..Default::default()
            },
            spec: Some(PodSpec {
                node_name: Some("node-01".to_owned()),
                containers: (0..containers)
                    .map(|i| Container {
                        name: format!("c-{i}"),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            status: Some(PodStatus {
                phase: Some("Running".to_owned()),
                pod_ip: Some("10.244.0.15".to_owned()),
                ..Default::default()
            }),
        }
    }

    fn running_status(name: &str, ready: bool, restart_count: i32) -> ContainerStatus {
        ContainerStatus {
            name: name.to_owned(),
            ready,
            restart_count,
            state: Some(ContainerState {
                running: Some(ContainerStateRunning::default()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn waiting_status(name: &str, reason: &str, restart_count: i32) -> ContainerStatus {
        ContainerStatus {
            name: name.to_owned(),
            ready: false,
            restart_count,
            state: Some(ContainerState {
                waiting: Some(ContainerStateWaiting {
                    reason: Some(reason.to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn terminated_status(
        name: &str,
        exit_code: i32,
        reason: Option<&str>,
        restart_count: i32,
    ) -> ContainerStatus {
        ContainerStatus {
            name: name.to_owned(),
            ready: false,
            restart_count,
            state: Some(ContainerState {
                terminated: Some(ContainerStateTerminated {
                    exit_code,
                    reason: reason.map(str::to_owned),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn projects_running_ready_pod() {
        let mut pod = base_pod(2);
        pod.status.as_mut().unwrap().container_statuses = Some(vec![
            running_status("c-0", true, 1),
            running_status("c-1", true, 2),
        ]);

        let row = project_pod(&pod, "prod-eu");

        assert_eq!(row.uid, "pod-uid-1");
        assert_eq!(row.cluster, "prod-eu");
        assert_eq!(row.kind, "Pod");
        assert_eq!(row.namespace.as_deref(), Some("production"));
        assert_eq!(row.name, "api-7f8b9-x12");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Running");
        assert_eq!(
            row.labels,
            vec![
                ("a-key".to_owned(), "1".to_owned()),
                ("z-key".to_owned(), "2".to_owned()),
            ]
        );
        assert_eq!(row.cells.len(), POD_COLUMNS.len());
        assert_eq!(row.cells[0], Cell::Ready { ready: 2, total: 2 });
        assert_eq!(
            row.cells[1],
            Cell::Status {
                label: "Running".to_owned(),
                phase: RowStatus::Healthy,
            }
        );
        assert_eq!(row.cells[2], Cell::Int(3));
        assert_eq!(row.cells[5], Cell::Text("node-01".to_owned()));
        assert_eq!(row.cells[6], Cell::Text("10.244.0.15".to_owned()));
        assert_eq!(
            row.cells[7],
            Cell::Timestamp("2025-01-15T10:20:30Z".to_owned())
        );
    }

    fn pending_pod_with_scheduled_condition(node: Option<&str>) -> Pod {
        use k8s_openapi::api::core::v1::PodCondition;

        let mut pod = base_pod(1);
        pod.spec.as_mut().unwrap().node_name = node.map(str::to_owned);
        let status = pod.status.as_mut().unwrap();
        status.phase = Some("Pending".to_owned());
        status.pod_ip = None;
        status.conditions = Some(vec![PodCondition {
            type_: "PodScheduled".to_owned(),
            status: "False".to_owned(),
            reason: Some("Unschedulable".to_owned()),
            message: Some("0/3 nodes are available".to_owned()),
            ..Default::default()
        }]);
        pod
    }

    #[test]
    fn projects_unschedulable_pending_pod() {
        let pod = pending_pod_with_scheduled_condition(None);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.status_reason, "Unschedulable");
        assert_eq!(row.status, RowStatus::Pending);
        assert_eq!(
            row.cells[1],
            Cell::Status {
                label: "Unschedulable".to_owned(),
                phase: RowStatus::Pending,
            }
        );
    }

    #[test]
    fn pending_pod_with_node_stays_pending() {
        let pod = pending_pod_with_scheduled_condition(Some("node-01"));

        let row = project_pod(&pod, "dev");

        assert_eq!(row.status_reason, "Pending");
        assert_eq!(row.status, RowStatus::Pending);
    }

    fn with_last_terminated(
        mut cs: ContainerStatus,
        exit_code: i32,
        reason: Option<&str>,
        finished_at: &str,
    ) -> ContainerStatus {
        let finished: Time = serde_json::from_str(&format!("\"{finished_at}\"")).unwrap();
        cs.last_state = Some(ContainerState {
            terminated: Some(ContainerStateTerminated {
                exit_code,
                reason: reason.map(str::to_owned),
                finished_at: Some(finished),
                ..Default::default()
            }),
            ..Default::default()
        });
        cs
    }

    #[test]
    fn projects_last_restart_of_crash_looping_pod() {
        let mut pod = base_pod(1);
        pod.status.as_mut().unwrap().container_statuses = Some(vec![with_last_terminated(
            waiting_status("c-0", "CrashLoopBackOff", 4),
            1,
            Some("Error"),
            "2025-01-15T11:00:00Z",
        )]);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.cells.len(), POD_COLUMNS.len());
        assert_eq!(row.status_reason, "CrashLoopBackOff");
        assert_eq!(row.cells[2], Cell::Int(4));
        assert_eq!(
            row.cells[3],
            Cell::Timestamp("2025-01-15T11:00:00Z".to_owned())
        );
        assert_eq!(row.cells[4], Cell::Text("Error (1)".to_owned()));
        assert_eq!(row.cells[5], Cell::Text("node-01".to_owned()));
    }

    #[test]
    fn projects_last_reason_oom_killed() {
        let mut pod = base_pod(1);
        pod.status.as_mut().unwrap().container_statuses = Some(vec![with_last_terminated(
            running_status("c-0", true, 1),
            137,
            Some("OOMKilled"),
            "2025-01-15T11:00:00Z",
        )]);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.cells[4], Cell::Text("OOMKilled (137)".to_owned()));
    }

    #[test]
    fn projects_last_reason_without_reason_as_exit_code() {
        let mut pod = base_pod(1);
        pod.status.as_mut().unwrap().container_statuses = Some(vec![with_last_terminated(
            running_status("c-0", true, 1),
            2,
            None,
            "2025-01-15T11:00:00Z",
        )]);
        let row = project_pod(&pod, "dev");
        assert_eq!(row.cells[4], Cell::Text("exit 2".to_owned()));

        pod.status.as_mut().unwrap().container_statuses = Some(vec![with_last_terminated(
            running_status("c-0", true, 1),
            2,
            Some(""),
            "2025-01-15T11:00:00Z",
        )]);
        let row_empty = project_pod(&pod, "dev");
        assert_eq!(row_empty.cells[4], Cell::Text("exit 2".to_owned()));
    }

    #[test]
    fn pod_without_restarts_has_empty_last_restart_cells() {
        let mut pod = base_pod(1);
        pod.status.as_mut().unwrap().container_statuses =
            Some(vec![running_status("c-0", true, 0)]);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.cells[3], Cell::Text(String::new()));
        assert_eq!(row.cells[4], Cell::Text(String::new()));
    }

    #[test]
    fn latest_last_termination_wins_across_containers() {
        let mut pod = base_pod(2);
        pod.status.as_mut().unwrap().container_statuses = Some(vec![
            with_last_terminated(
                running_status("c-0", true, 1),
                137,
                Some("OOMKilled"),
                "2025-01-15T12:30:00Z",
            ),
            with_last_terminated(
                running_status("c-1", true, 3),
                1,
                Some("Error"),
                "2025-01-15T09:00:00Z",
            ),
        ]);

        let row = project_pod(&pod, "dev");
        assert_eq!(
            row.cells[3],
            Cell::Timestamp("2025-01-15T12:30:00Z".to_owned())
        );
        assert_eq!(row.cells[4], Cell::Text("OOMKilled (137)".to_owned()));

        // Order-independent: the later finish wins even when listed second.
        pod.status
            .as_mut()
            .unwrap()
            .container_statuses
            .as_mut()
            .unwrap()
            .reverse();
        let reversed = project_pod(&pod, "dev");
        assert_eq!(reversed.cells[4], Cell::Text("OOMKilled (137)".to_owned()));
    }

    #[test]
    fn projects_crash_loop_back_off_waiting_container() {
        let mut pod = base_pod(2);
        pod.status.as_mut().unwrap().container_statuses = Some(vec![
            running_status("c-0", true, 0),
            waiting_status("c-1", "CrashLoopBackOff", 5),
        ]);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.status_reason, "CrashLoopBackOff");
        assert_eq!(row.status, RowStatus::Failed);
        assert_eq!(row.cells[0], Cell::Ready { ready: 1, total: 2 });
        assert_eq!(
            row.cells[1],
            Cell::Status {
                label: "CrashLoopBackOff".to_owned(),
                phase: RowStatus::Failed,
            }
        );
        assert_eq!(row.cells[2], Cell::Int(5));
    }

    #[test]
    fn projects_image_pull_back_off_waiting_container() {
        let mut pod = base_pod(1);
        pod.status.as_mut().unwrap().phase = Some("Pending".to_owned());
        pod.status.as_mut().unwrap().container_statuses =
            Some(vec![waiting_status("c-0", "ImagePullBackOff", 0)]);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.status_reason, "ImagePullBackOff");
        assert_eq!(row.status, RowStatus::Failed);
        assert_eq!(row.cells[0], Cell::Ready { ready: 0, total: 1 });
        assert_eq!(
            row.cells[1],
            Cell::Status {
                label: "ImagePullBackOff".to_owned(),
                phase: RowStatus::Failed,
            }
        );
    }

    #[test]
    fn projects_running_and_failing_init_containers() {
        let mut pod = base_pod(1);
        pod.spec.as_mut().unwrap().init_containers = Some(vec![
            Container {
                name: "init-0".to_owned(),
                ..Default::default()
            },
            Container {
                name: "init-1".to_owned(),
                ..Default::default()
            },
        ]);
        pod.status.as_mut().unwrap().phase = Some("Pending".to_owned());

        // Case A: first init container still running -> Init:0/2, Pending
        pod.status.as_mut().unwrap().init_container_statuses =
            Some(vec![running_status("init-0", false, 0)]);
        let row_running = project_pod(&pod, "dev");
        assert_eq!(row_running.status_reason, "Init:0/2");
        assert_eq!(row_running.status, RowStatus::Pending);

        // Case B: first init container succeeded, second waiting in CrashLoopBackOff -> Init:CrashLoopBackOff, Failed
        pod.status.as_mut().unwrap().init_container_statuses = Some(vec![
            terminated_status("init-0", 0, Some("Completed"), 0),
            waiting_status("init-1", "CrashLoopBackOff", 3),
        ]);
        let row_crash = project_pod(&pod, "dev");
        assert_eq!(row_crash.status_reason, "Init:CrashLoopBackOff");
        assert_eq!(row_crash.status, RowStatus::Failed);

        // Case C: init container terminated with non-zero exit code and no reason -> Init:Error, Failed
        pod.status.as_mut().unwrap().init_container_statuses =
            Some(vec![terminated_status("init-0", 1, None, 0)]);
        let row_err = project_pod(&pod, "dev");
        assert_eq!(row_err.status_reason, "Init:Error");
        assert_eq!(row_err.status, RowStatus::Failed);
    }

    #[test]
    fn projects_terminating_pod_when_deletion_timestamp_set() {
        let mut pod = base_pod(1);
        let deletion: Time = serde_json::from_str("\"2025-01-15T10:25:00Z\"").unwrap();
        pod.metadata.deletion_timestamp = Some(deletion);
        pod.status.as_mut().unwrap().container_statuses =
            Some(vec![running_status("c-0", true, 0)]);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.status_reason, "Terminating");
        assert_eq!(row.status, RowStatus::Terminating);
        assert_eq!(
            row.cells[1],
            Cell::Status {
                label: "Terminating".to_owned(),
                phase: RowStatus::Terminating,
            }
        );
    }

    #[test]
    fn projects_completed_pod() {
        let mut pod = base_pod(1);
        pod.status.as_mut().unwrap().phase = Some("Succeeded".to_owned());
        pod.status.as_mut().unwrap().container_statuses =
            Some(vec![terminated_status("c-0", 0, Some("Completed"), 0)]);

        let row = project_pod(&pod, "dev");

        assert_eq!(row.status_reason, "Completed");
        assert_eq!(row.status, RowStatus::Completed);
        assert_eq!(row.cells[0], Cell::Ready { ready: 0, total: 1 });
        assert_eq!(
            row.cells[1],
            Cell::Status {
                label: "Completed".to_owned(),
                phase: RowStatus::Completed,
            }
        );

        // Also when container_statuses is absent and phase is Succeeded:
        pod.status.as_mut().unwrap().container_statuses = None;
        let row_phase_only = project_pod(&pod, "dev");
        assert_eq!(row_phase_only.status_reason, "Completed");
        assert_eq!(row_phase_only.status, RowStatus::Completed);
    }

    #[test]
    fn projects_deployment_available_progressing_and_deadline_exceeded() {
        use k8s_openapi::api::apps::v1::{
            Deployment, DeploymentCondition, DeploymentSpec, DeploymentStatus,
        };

        let creation: Time = serde_json::from_str("\"2025-01-15T10:20:30Z\"").unwrap();
        let mut deploy = Deployment {
            metadata: ObjectMeta {
                uid: Some("dep-1".to_owned()),
                name: Some("web".to_owned()),
                namespace: Some("default".to_owned()),
                creation_timestamp: Some(creation),
                ..Default::default()
            },
            spec: Some(DeploymentSpec {
                replicas: Some(3),
                ..Default::default()
            }),
            status: Some(DeploymentStatus {
                ready_replicas: Some(3),
                updated_replicas: Some(3),
                available_replicas: Some(3),
                ..Default::default()
            }),
        };

        let available = project_deployment(&deploy, "prod");
        assert_eq!(available.kind, "Deployment");
        assert_eq!(available.status, RowStatus::Healthy);
        assert_eq!(available.status_reason, "Available");
        assert_eq!(available.cells.len(), DEPLOYMENT_COLUMNS.len());
        assert_eq!(available.cells[0], Cell::Ready { ready: 3, total: 3 });
        assert_eq!(available.cells[1], Cell::Int(3));
        assert_eq!(available.cells[2], Cell::Int(3));

        deploy.status.as_mut().unwrap().ready_replicas = Some(1);
        deploy.status.as_mut().unwrap().available_replicas = Some(1);
        let progressing = project_deployment(&deploy, "prod");
        assert_eq!(progressing.status, RowStatus::Pending);
        assert_eq!(progressing.status_reason, "Progressing");

        deploy.status.as_mut().unwrap().conditions = Some(vec![DeploymentCondition {
            type_: "Progressing".to_owned(),
            status: "False".to_owned(),
            reason: Some("ProgressDeadlineExceeded".to_owned()),
            ..Default::default()
        }]);
        let failed = project_deployment(&deploy, "prod");
        assert_eq!(failed.status, RowStatus::Failed);
        assert_eq!(failed.status_reason, "ProgressDeadlineExceeded");
    }

    #[test]
    fn projects_job_complete_running_and_failed() {
        use k8s_openapi::api::batch::v1::{Job, JobCondition, JobSpec, JobStatus};

        let mut job = Job {
            metadata: ObjectMeta {
                uid: Some("job-1".to_owned()),
                name: Some("migrate".to_owned()),
                namespace: Some("default".to_owned()),
                ..Default::default()
            },
            spec: Some(JobSpec {
                completions: Some(2),
                ..Default::default()
            }),
            status: Some(JobStatus {
                active: Some(1),
                succeeded: Some(1),
                ..Default::default()
            }),
        };

        let running = project_job(&job, "prod");
        assert_eq!(running.kind, "Job");
        assert_eq!(running.status, RowStatus::Pending);
        assert_eq!(running.status_reason, "Running");
        assert_eq!(running.cells.len(), JOB_COLUMNS.len());
        assert_eq!(running.cells[0], Cell::Ready { ready: 1, total: 2 });

        job.status.as_mut().unwrap().active = Some(0);
        job.status.as_mut().unwrap().succeeded = Some(2);
        let complete = project_job(&job, "prod");
        assert_eq!(complete.status, RowStatus::Completed);
        assert_eq!(complete.status_reason, "Complete");

        job.status.as_mut().unwrap().succeeded = Some(0);
        job.status.as_mut().unwrap().conditions = Some(vec![JobCondition {
            type_: "Failed".to_owned(),
            status: "True".to_owned(),
            reason: Some("BackoffLimitExceeded".to_owned()),
            ..Default::default()
        }]);
        let failed = project_job(&job, "prod");
        assert_eq!(failed.status, RowStatus::Failed);
        assert_eq!(failed.status_reason, "BackoffLimitExceeded");
    }

    #[test]
    fn classifies_403_and_404_kube_errors() {
        let forbidden_err = kube::Error::Api(Box::new(kube::core::Status {
            code: 403,
            message: "forbidden".to_owned(),
            reason: "Forbidden".to_owned(),
            ..Default::default()
        }));
        assert_eq!(
            classify_kube_error(&forbidden_err, "pods", Some("prod")),
            Some(StoreAccessState::Forbidden {
                resource: "pods".to_owned(),
                namespace: Some("prod".to_owned()),
                message: "No permission to list pods in namespace \"prod\"".to_owned(),
            })
        );
        assert_eq!(
            classify_kube_error(&forbidden_err, "nodes", None),
            Some(StoreAccessState::Forbidden {
                resource: "nodes".to_owned(),
                namespace: None,
                message: "No permission to list nodes".to_owned(),
            })
        );

        let not_found_err = kube::Error::Api(Box::new(kube::core::Status {
            code: 404,
            message: "not found".to_owned(),
            reason: "NotFound".to_owned(),
            ..Default::default()
        }));
        assert_eq!(
            classify_kube_error(&not_found_err, "metrics.k8s.io", None),
            Some(StoreAccessState::NotFound {
                resource: "metrics.k8s.io".to_owned(),
                message: "metrics.k8s.io API is not installed on this cluster".to_owned(),
            })
        );
    }
}
