//! Resource metrics parsing and `metrics.k8s.io` availability classification (P1-13),
//! and the CPU / memory usage cells of pod and node rows (02-01).
//!
//! # How usage reaches the rows
//!
//! Rows are projected once per watch event, but usage arrives separately every
//! [`METRICS_POLL_INTERVAL`]. Projection therefore stores each usage column as a
//! [`Cell::Usage`] *template*: request/limit from the object, `value` set to the
//! internal [`UNMEASURED`] marker and an empty `formatted` string (no heap
//! allocation). The store keeps the last poll's [`UsageMap`]; a poll patches
//! only `value` in place (no row is cloned or re-projected; see [`apply_usage`])
//! and every row a watch event inserts is joined with that map on insert, so
//! it shows the last measurement right away. Pages are converted at the
//! end of a query, for the window only ([`finalize_usage_cells`]): unmeasured
//! cells become `Text("")` and measured ones get their `formatted` string, so
//! the UI never sees the marker or a made-up `0`.

use std::collections::HashMap;

use k8s_openapi::api::core::v1::{Node, Pod};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use serde::Deserialize;

use crate::api::{Cell, MetricsAvailability, ResourceUsage, Row, UsageCell};

/// How often a subscribed pod / node store lists `metrics.k8s.io` (metrics-server
/// itself scrapes every 15 s by default, so polling faster shows no new data).
pub const METRICS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Upper bound of one metrics list request; a slower answer is reported as an error.
pub const METRICS_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a poller waits after its store lost the last subscriber before it
/// stops, so a UI re-subscribing (new filter, new sort) does not restart it.
pub const METRICS_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Pod cell index of the `CPU` column (see [`crate::projection::POD_COLUMNS`]).
pub const POD_CPU_CELL: usize = 8;
/// Pod cell index of the `Memory` column.
pub const POD_MEMORY_CELL: usize = 9;
/// Node cell index of the `CPU` column (see [`crate::detail::NODE_COLUMNS`]).
pub const NODE_CPU_CELL: usize = 2;
/// Node cell index of the `Memory` column.
pub const NODE_MEMORY_CELL: usize = 3;

/// `UsageCell::value` of a stored usage cell that has no measurement. Never
/// leaves the core: [`finalize_usage_cells`] turns such cells into `Text("")`.
pub(crate) const UNMEASURED: u32 = u32::MAX;

/// `(cpu, memory)` cell indices of the usage columns of `kind`, if it has them.
#[must_use]
pub fn usage_cell_indices(kind: &str) -> Option<(usize, usize)> {
    match kind {
        "Pod" => Some((POD_CPU_CELL, POD_MEMORY_CELL)),
        "Node" => Some((NODE_CPU_CELL, NODE_MEMORY_CELL)),
        _ => None,
    }
}

/// Stored, not yet measured usage cell carrying `request` / `limit`.
pub(crate) fn unmeasured_cell(request: Option<u32>, limit: Option<u32>) -> Cell {
    Cell::Usage(Box::new(UsageCell {
        value: UNMEASURED,
        formatted: String::new(),
        request,
        limit,
    }))
}

/// Requests and limits of a pod, summed over its (app) containers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PodResources {
    pub cpu_request: Option<u32>,
    pub cpu_limit: Option<u32>,
    pub memory_request: Option<u32>,
    pub memory_limit: Option<u32>,
}

/// Sums container requests and limits of `pod` (millicores / MiB).
///
/// A request is the sum over the containers that set one (`None` if none does);
/// a limit is `None` as soon as one container is unlimited for that resource,
/// because the pod as a whole is then unbounded. Init containers and pod
/// overhead are not included.
#[must_use]
pub fn pod_resources(pod: &Pod) -> PodResources {
    let Some(spec) = pod.spec.as_ref() else {
        return PodResources::default();
    };
    let mut out = PodResources::default();
    let mut cpu_limit = Some(0u32);
    let mut memory_limit = Some(0u32);
    for container in &spec.containers {
        let resources = container.resources.as_ref();
        let requests = resources.and_then(|r| r.requests.as_ref());
        let limits = resources.and_then(|r| r.limits.as_ref());
        let get = |map: Option<&std::collections::BTreeMap<String, Quantity>>,
                   key: &str,
                   parse: fn(&str) -> Option<u32>| {
            map.and_then(|m| m.get(key)).and_then(|q| parse(&q.0))
        };
        if let Some(v) = get(requests, "cpu", parse_cpu_millicores) {
            out.cpu_request = Some(out.cpu_request.unwrap_or(0).saturating_add(v));
        }
        if let Some(v) = get(requests, "memory", parse_memory_mib) {
            out.memory_request = Some(out.memory_request.unwrap_or(0).saturating_add(v));
        }
        cpu_limit = cpu_limit
            .zip(get(limits, "cpu", parse_cpu_millicores))
            .map(|(sum, v)| sum.saturating_add(v));
        memory_limit = memory_limit
            .zip(get(limits, "memory", parse_memory_mib))
            .map(|(sum, v)| sum.saturating_add(v));
    }
    if !spec.containers.is_empty() {
        out.cpu_limit = cpu_limit;
        out.memory_limit = memory_limit;
    }
    out
}

/// The two stored usage cells (`CPU`, `Memory`) of a pod row.
///
/// A pod in a terminal phase (`Succeeded` / `Failed`) carries no `request`: it
/// no longer holds node resources, so the Overview's requested capacity skips
/// it like `kubectl describe node` does.
pub(crate) fn pod_usage_cells(pod: &Pod) -> [Cell; 2] {
    let mut r = pod_resources(pod);
    let terminal = matches!(
        pod.status.as_ref().and_then(|s| s.phase.as_deref()),
        Some("Succeeded" | "Failed")
    );
    if terminal {
        r.cpu_request = None;
        r.memory_request = None;
    }
    [
        unmeasured_cell(r.cpu_request, r.cpu_limit),
        unmeasured_cell(r.memory_request, r.memory_limit),
    ]
}

/// The two stored usage cells (`CPU`, `Memory`) of a node row: `limit` is the
/// node's `allocatable`, `request` is `None`.
pub(crate) fn node_usage_cells(node: &Node) -> [Cell; 2] {
    let allocatable = node.status.as_ref().and_then(|s| s.allocatable.as_ref());
    let get = |key: &str, parse: fn(&str) -> Option<u32>| {
        allocatable
            .and_then(|m| m.get(key))
            .and_then(|q| parse(&q.0))
    };
    [
        unmeasured_cell(None, get("cpu", parse_cpu_millicores)),
        unmeasured_cell(None, get("memory", parse_memory_mib)),
    ]
}

/// One measurement from `metrics.k8s.io`, in millicores and MiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsageSample {
    pub cpu: u32,
    pub memory: u32,
}

/// Measurements of one poll: namespace (`""` for nodes) -> name -> sample.
/// Nested so rows can be looked up by `&str` without allocating a key.
pub(crate) type UsageMap = HashMap<String, HashMap<String, UsageSample>>;

fn set_usage_value(cell: Option<&mut Cell>, value: u32) -> bool {
    match cell {
        Some(Cell::Usage(u)) if u.value != value => {
            u.value = value;
            true
        }
        _ => false,
    }
}

/// Patches the stored usage cells `cols` of `row` from `map` in place;
/// `None` (metrics unavailable) or a row missing from `map` clears them.
/// Returns `true` if a value changed.
pub(crate) fn apply_usage(row: &mut Row, cols: (usize, usize), map: Option<&UsageMap>) -> bool {
    let sample = map.and_then(|m| {
        m.get(row.namespace.as_deref().unwrap_or(""))
            .and_then(|by_name| by_name.get(&row.name))
    });
    let (cpu, memory) = sample.map_or((UNMEASURED, UNMEASURED), |s| (s.cpu, s.memory));
    let a = set_usage_value(row.cells.get_mut(cols.0), cpu);
    let b = set_usage_value(row.cells.get_mut(cols.1), memory);
    a || b
}

/// Converts the stored usage cells of a page row into their IPC form:
/// unmeasured -> `Text("")`, measured -> `formatted` filled in.
pub fn finalize_usage_cells(row: &mut Row) {
    let Some((cpu_idx, memory_idx)) = usage_cell_indices(&row.kind) else {
        return;
    };
    for (idx, format) in [
        (cpu_idx, format_cpu_millicores as fn(u32) -> String),
        (memory_idx, format_memory_mib),
    ] {
        let Some(cell) = row.cells.get_mut(idx) else {
            continue;
        };
        if let Cell::Usage(u) = cell {
            if u.value == UNMEASURED {
                *cell = Cell::Text(String::new());
            } else if u.formatted.is_empty() {
                u.formatted = format(u.value);
            }
        }
    }
}

/// The minimal shape of a `PodMetricsList` / `NodeMetricsList`: only the fields
/// the usage columns need, so labels and timestamps of thousands of items are
/// skipped while parsing instead of being built into maps.
#[derive(Deserialize)]
struct MetricsList {
    #[serde(default)]
    items: Vec<MetricsItem>,
}

#[derive(Deserialize)]
struct MetricsItem {
    metadata: MetricsMeta,
    /// `NodeMetrics` only.
    #[serde(default)]
    usage: Option<UsageQuantities>,
    /// `PodMetrics` only.
    #[serde(default)]
    containers: Vec<ContainerMetrics>,
}

#[derive(Deserialize)]
struct MetricsMeta {
    name: String,
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Deserialize)]
struct ContainerMetrics {
    usage: UsageQuantities,
}

#[derive(Deserialize)]
struct UsageQuantities {
    #[serde(default)]
    cpu: Option<String>,
    #[serde(default)]
    memory: Option<String>,
}

fn sample_of<'a>(usages: impl Iterator<Item = &'a UsageQuantities>) -> Option<UsageSample> {
    let mut sample = UsageSample { cpu: 0, memory: 0 };
    let mut any = false;
    for u in usages {
        let cpu = u.cpu.as_deref().and_then(parse_cpu_millicores)?;
        let memory = u.memory.as_deref().and_then(parse_memory_mib)?;
        sample.cpu = sample.cpu.saturating_add(cpu);
        sample.memory = sample.memory.saturating_add(memory);
        any = true;
    }
    any.then_some(sample)
}

/// Parses a `PodMetricsList` or `NodeMetricsList` JSON body. Pod usage is the
/// sum over containers; items with unparsable quantities are left out (their
/// rows stay unmeasured rather than showing a partial sum).
pub(crate) fn parse_metrics_list(body: &str) -> Result<UsageMap, serde_json::Error> {
    let list: MetricsList = serde_json::from_str(body)?;
    let mut map = UsageMap::new();
    for item in list.items {
        let sample = match &item.usage {
            Some(usage) => sample_of(std::iter::once(usage)),
            None => sample_of(item.containers.iter().map(|c| &c.usage)),
        };
        if let Some(sample) = sample {
            map.entry(item.metadata.namespace.unwrap_or_default())
                .or_default()
                .insert(item.metadata.name, sample);
        }
    }
    Ok(map)
}

/// `metrics.k8s.io/v1beta1` collection path of `kind`'s metrics.
fn metrics_path(kind: &str) -> Option<&'static str> {
    match kind {
        "Pod" => Some("/apis/metrics.k8s.io/v1beta1/pods"),
        "Node" => Some("/apis/metrics.k8s.io/v1beta1/nodes"),
        _ => None,
    }
}

/// Lists the current `PodMetrics` / `NodeMetrics` of the whole cluster.
///
/// The list API of metrics-server does not paginate, so this is one request.
pub(crate) async fn fetch_usage(
    client: &kube::Client,
    kind: &str,
) -> Result<UsageMap, kube::Error> {
    let Some(path) = metrics_path(kind) else {
        return Ok(UsageMap::new());
    };
    let req = kube::core::Request::new(path)
        .list(&kube::api::ListParams::default())
        .map_err(kube::Error::BuildRequest)?;
    let body = client.request_text(req).await?;
    parse_metrics_list(&body).map_err(kube::Error::SerdeError)
}

/// Parses a Kubernetes CPU quantity string into millicores (`u32`).
///
/// Supported formats:
/// - `"250m"` -> `250`
/// - `"500000n"` -> `1` (rounded millicores)
/// - `"250000000n"` -> `250`
/// - `"250u"` -> `0`
/// - `"2"` -> `2000`
/// - `"0.5"` -> `500`
pub fn parse_cpu_millicores(quantity: &str) -> Option<u32> {
    let q = quantity.trim();
    if q.is_empty() {
        return None;
    }

    if let Some(milli) = q.strip_suffix('m') {
        let val: f64 = milli.parse().ok()?;
        if !val.is_finite() || val < 0.0 {
            return None;
        }
        return u32::try_from(val.round() as u64).ok();
    }

    if let Some(nanos) = q.strip_suffix('n') {
        let val: u64 = nanos.parse().ok()?;
        let millicores = (val + 500_000) / 1_000_000;
        return u32::try_from(millicores).ok();
    }

    if let Some(micros) = q.strip_suffix('u') {
        let val: u64 = micros.parse().ok()?;
        let millicores = (val + 500) / 1_000;
        return u32::try_from(millicores).ok();
    }

    let cores: f64 = q.parse().ok()?;
    if !cores.is_finite() || cores < 0.0 {
        return None;
    }
    u32::try_from((cores * 1000.0).round() as u64).ok()
}

/// Parses a Kubernetes memory quantity string into MiB (`u32`).
///
/// Supported formats:
/// - `"128Mi"` -> `128`
/// - `"1Gi"` -> `1024`
/// - `"65536Ki"` -> `64`
/// - `"134217728"` (raw bytes) -> `128`
pub fn parse_memory_mib(quantity: &str) -> Option<u32> {
    let q = quantity.trim();
    if q.is_empty() {
        return None;
    }

    let mib: u64 = if let Some(ki) = q.strip_suffix("Ki") {
        let val: u64 = ki.parse().ok()?;
        val / 1024
    } else if let Some(mi) = q.strip_suffix("Mi") {
        mi.parse().ok()?
    } else if let Some(gi) = q.strip_suffix("Gi") {
        let val: u64 = gi.parse().ok()?;
        val.checked_mul(1024)?
    } else if let Some(ti) = q.strip_suffix("Ti") {
        let val: u64 = ti.parse().ok()?;
        val.checked_mul(1024 * 1024)?
    } else {
        let bytes: u64 = q.parse().ok()?;
        bytes / (1024 * 1024)
    };

    u32::try_from(mib).ok()
}

/// Formats millicores as a compact `"<m>m"` quantity string.
pub fn format_cpu_millicores(m: u32) -> String {
    format!("{m}m")
}

/// Formats mebibytes as a compact `"<mib>Mi"` quantity string.
pub fn format_memory_mib(mib: u32) -> String {
    format!("{mib}Mi")
}

/// Parses CPU and memory quantity strings into a formatted [`ResourceUsage`].
pub fn parse_usage(cpu: &str, memory: &str) -> Option<ResourceUsage> {
    let cpu_millicores = parse_cpu_millicores(cpu)?;
    let memory_mib = parse_memory_mib(memory)?;
    Some(ResourceUsage {
        cpu_millicores,
        memory_mib,
        cpu_formatted: format_cpu_millicores(cpu_millicores),
        memory_formatted: format_memory_mib(memory_mib),
    })
}

/// Classifies a `kube::Error` from `metrics.k8s.io` into an honest [`MetricsAvailability`] state.
///
/// 404 (no `metrics.k8s.io` APIService) and 503 (APIService without a running
/// metrics-server) are [`MetricsAvailability::NotInstalled`], 403 is
/// [`MetricsAvailability::Forbidden`], anything else an
/// [`MetricsAvailability::Error`] with a sanitized message.
pub fn classify_metrics_error(err: &kube::Error) -> MetricsAvailability {
    if let kube::Error::Api(status) = err {
        match status.code {
            404 | 503 => {
                return MetricsAvailability::NotInstalled {
                    message: "metrics-server not installed".into(),
                };
            }
            403 => {
                return MetricsAvailability::Forbidden {
                    message: "No permission to list metrics.k8s.io".into(),
                };
            }
            _ => {}
        }
    }
    MetricsAvailability::Error {
        message: crate::cluster::sanitize_kube_error(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_quantities_to_millicores() {
        assert_eq!(parse_cpu_millicores("100m"), Some(100));
        assert_eq!(parse_cpu_millicores("250m"), Some(250));
        assert_eq!(parse_cpu_millicores("2"), Some(2000));
        assert_eq!(parse_cpu_millicores("0.5"), Some(500));
        assert_eq!(parse_cpu_millicores("0.25"), Some(250));
        assert_eq!(parse_cpu_millicores("250000000n"), Some(250));
        assert_eq!(parse_cpu_millicores("500000000n"), Some(500));
        assert_eq!(parse_cpu_millicores("500000n"), Some(1));
        assert_eq!(parse_cpu_millicores("250u"), Some(0));
        assert_eq!(parse_cpu_millicores(""), None);
    }

    #[test]
    fn parses_memory_quantities_to_mib() {
        assert_eq!(parse_memory_mib("64Mi"), Some(64));
        assert_eq!(parse_memory_mib("128Mi"), Some(128));
        assert_eq!(parse_memory_mib("1Gi"), Some(1024));
        assert_eq!(parse_memory_mib("2Gi"), Some(2048));
        assert_eq!(parse_memory_mib("1024Ki"), Some(1));
        assert_eq!(parse_memory_mib("65536Ki"), Some(64));
        assert_eq!(parse_memory_mib("134217728"), Some(128));
        assert_eq!(parse_memory_mib(""), None);
    }

    #[test]
    fn formats_resource_usage() {
        let usage = parse_usage("250m", "128Mi").unwrap();
        assert_eq!(
            usage,
            ResourceUsage {
                cpu_millicores: 250,
                memory_mib: 128,
                cpu_formatted: "250m".to_owned(),
                memory_formatted: "128Mi".to_owned(),
            }
        );
    }

    #[test]
    fn classifies_404_and_503_as_metrics_server_not_installed() {
        let err_404 = kube::Error::Api(Box::new(kube::core::Status {
            code: 404,
            message: "the server could not find the requested resource".to_owned(),
            reason: "NotFound".to_owned(),
            ..Default::default()
        }));
        assert_eq!(
            classify_metrics_error(&err_404),
            MetricsAvailability::NotInstalled {
                message: "metrics-server not installed".to_owned(),
            }
        );

        let err_503 = kube::Error::Api(Box::new(kube::core::Status {
            code: 503,
            message: "service unavailable".to_owned(),
            reason: "ServiceUnavailable".to_owned(),
            ..Default::default()
        }));
        assert_eq!(
            classify_metrics_error(&err_503),
            MetricsAvailability::NotInstalled {
                message: "metrics-server not installed".to_owned(),
            }
        );

        let err_403 = kube::Error::Api(Box::new(kube::core::Status {
            code: 403,
            message: "forbidden".to_owned(),
            reason: "Forbidden".to_owned(),
            ..Default::default()
        }));
        assert_eq!(
            classify_metrics_error(&err_403),
            MetricsAvailability::Forbidden {
                message: "No permission to list metrics.k8s.io".to_owned(),
            }
        );

        let err_500 = kube::Error::Api(Box::new(kube::core::Status {
            code: 500,
            message: "boom".to_owned(),
            ..Default::default()
        }));
        assert!(matches!(
            classify_metrics_error(&err_500),
            MetricsAvailability::Error { .. }
        ));
    }

    fn container(
        requests: &[(&str, &str)],
        limits: &[(&str, &str)],
    ) -> k8s_openapi::api::core::v1::Container {
        let map = |kv: &[(&str, &str)]| {
            (!kv.is_empty()).then(|| {
                kv.iter()
                    .map(|(k, v)| ((*k).to_owned(), Quantity((*v).to_owned())))
                    .collect()
            })
        };
        k8s_openapi::api::core::v1::Container {
            name: "c".to_owned(),
            resources: Some(k8s_openapi::api::core::v1::ResourceRequirements {
                requests: map(requests),
                limits: map(limits),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn pod(ns: &str, name: &str, containers: Vec<k8s_openapi::api::core::v1::Container>) -> Pod {
        Pod {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                uid: Some(format!("uid-{ns}-{name}")),
                name: Some(name.to_owned()),
                namespace: Some(ns.to_owned()),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::core::v1::PodSpec {
                containers,
                ..Default::default()
            }),
            status: None,
        }
    }

    #[test]
    fn sums_container_requests_and_limits() {
        let p = pod(
            "ns",
            "p",
            vec![
                container(
                    &[("cpu", "100m"), ("memory", "64Mi")],
                    &[("cpu", "500m"), ("memory", "128Mi")],
                ),
                container(&[("cpu", "0.25")], &[("cpu", "1"), ("memory", "1Gi")]),
            ],
        );
        assert_eq!(
            pod_resources(&p),
            PodResources {
                cpu_request: Some(350),
                cpu_limit: Some(1500),
                memory_request: Some(64),
                memory_limit: Some(1152),
            }
        );

        // One container without a memory limit makes the pod unlimited.
        let p = pod(
            "ns",
            "p",
            vec![container(&[], &[("memory", "64Mi")]), container(&[], &[])],
        );
        assert_eq!(pod_resources(&p), PodResources::default());
    }

    #[test]
    fn terminal_pods_carry_no_requests() {
        let request = |cells: &[Cell; 2]| {
            cells
                .iter()
                .map(|c| match c {
                    Cell::Usage(u) => (u.request, u.limit),
                    _ => (None, None),
                })
                .collect::<Vec<_>>()
        };
        let mut p = pod(
            "ns",
            "job",
            vec![container(
                &[("cpu", "100m"), ("memory", "64Mi")],
                &[("cpu", "200m"), ("memory", "128Mi")],
            )],
        );
        for (phase, expected) in [
            (None, vec![(Some(100), Some(200)), (Some(64), Some(128))]),
            (
                Some("Running"),
                vec![(Some(100), Some(200)), (Some(64), Some(128))],
            ),
            (
                Some("Pending"),
                vec![(Some(100), Some(200)), (Some(64), Some(128))],
            ),
            (
                Some("Succeeded"),
                vec![(None, Some(200)), (None, Some(128))],
            ),
            (Some("Failed"), vec![(None, Some(200)), (None, Some(128))]),
        ] {
            p.status = phase.map(|ph| k8s_openapi::api::core::v1::PodStatus {
                phase: Some(ph.to_owned()),
                ..Default::default()
            });
            assert_eq!(request(&pod_usage_cells(&p)), expected, "{phase:?}");
        }
    }

    fn usage_map(entries: &[(&str, &str, u32, u32)]) -> UsageMap {
        let mut map = UsageMap::new();
        for (ns, name, cpu, memory) in entries {
            map.entry((*ns).to_owned()).or_default().insert(
                (*name).to_owned(),
                UsageSample {
                    cpu: *cpu,
                    memory: *memory,
                },
            );
        }
        map
    }

    #[test]
    fn joins_usage_into_pod_rows_and_finalizes_pages() {
        let p = pod(
            "ns",
            "web",
            vec![container(&[("cpu", "100m")], &[("memory", "256Mi")])],
        );
        let mut row = crate::projection::project_pod(&p, "c");
        assert_eq!(row.cells.len(), crate::projection::POD_COLUMNS.len());
        assert_eq!(crate::projection::POD_COLUMNS[POD_CPU_CELL], "CPU");
        assert_eq!(crate::projection::POD_COLUMNS[POD_MEMORY_CELL], "Memory");

        // Before the first poll: empty text, never 0.
        let mut page_row = row.clone();
        finalize_usage_cells(&mut page_row);
        assert_eq!(page_row.cells[POD_CPU_CELL], Cell::Text(String::new()));
        assert_eq!(page_row.cells[POD_MEMORY_CELL], Cell::Text(String::new()));

        let cols = (POD_CPU_CELL, POD_MEMORY_CELL);
        let map = usage_map(&[("ns", "web", 42, 300), ("other", "web", 1, 1)]);
        assert!(apply_usage(&mut row, cols, Some(&map)));
        assert!(!apply_usage(&mut row, cols, Some(&map)), "unchanged values");
        let mut page_row = row.clone();
        finalize_usage_cells(&mut page_row);
        assert_eq!(
            page_row.cells[POD_CPU_CELL],
            Cell::Usage(Box::new(UsageCell {
                value: 42,
                formatted: "42m".to_owned(),
                request: Some(100),
                limit: None,
            }))
        );
        assert_eq!(
            page_row.cells[POD_MEMORY_CELL],
            Cell::Usage(Box::new(UsageCell {
                value: 300,
                formatted: "300Mi".to_owned(),
                request: None,
                limit: Some(256),
            }))
        );

        // A pod missing from the poll, or metrics becoming unavailable, clears it.
        let other = usage_map(&[("ns", "api", 5, 5)]);
        assert!(apply_usage(&mut row, cols, Some(&other)));
        let mut page_row = row.clone();
        finalize_usage_cells(&mut page_row);
        assert_eq!(page_row.cells[POD_CPU_CELL], Cell::Text(String::new()));
        apply_usage(&mut row, cols, Some(&map));
        assert!(apply_usage(&mut row, cols, None));
        finalize_usage_cells(&mut row);
        assert_eq!(row.cells[POD_MEMORY_CELL], Cell::Text(String::new()));
    }

    #[test]
    fn node_usage_uses_allocatable_as_limit() {
        use k8s_openapi::api::core::v1::{Node, NodeStatus};
        let node = Node {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                uid: Some("n1".to_owned()),
                name: Some("node-1".to_owned()),
                ..Default::default()
            },
            spec: None,
            status: Some(NodeStatus {
                allocatable: Some(
                    [
                        ("cpu".to_owned(), Quantity("4".to_owned())),
                        ("memory".to_owned(), Quantity("8137012Ki".to_owned())),
                    ]
                    .into_iter()
                    .collect(),
                ),
                ..Default::default()
            }),
        };
        let mut row = crate::detail::project_node(&node, "c");
        assert_eq!(row.cells.len(), crate::detail::NODE_COLUMNS.len());
        let map = usage_map(&[("", "node-1", 1234, 2048)]);
        apply_usage(&mut row, (NODE_CPU_CELL, NODE_MEMORY_CELL), Some(&map));
        finalize_usage_cells(&mut row);
        assert_eq!(
            row.cells[NODE_CPU_CELL],
            Cell::Usage(Box::new(UsageCell {
                value: 1234,
                formatted: "1234m".to_owned(),
                request: None,
                limit: Some(4000),
            }))
        );
        assert_eq!(
            row.cells[NODE_MEMORY_CELL],
            Cell::Usage(Box::new(UsageCell {
                value: 2048,
                formatted: "2048Mi".to_owned(),
                request: None,
                limit: Some(7946),
            }))
        );
    }

    #[test]
    fn parses_pod_and_node_metrics_lists() {
        let pods = r#"{"kind":"PodMetricsList","apiVersion":"metrics.k8s.io/v1beta1","metadata":{},
            "items":[
              {"metadata":{"name":"a","namespace":"ns","labels":{"x":"y"}},"timestamp":"t","window":"15s",
               "containers":[{"name":"c1","usage":{"cpu":"1232820n","memory":"19600Ki"}},
                             {"name":"c2","usage":{"cpu":"10m","memory":"1Mi"}}]},
              {"metadata":{"name":"bad","namespace":"ns"},"containers":[{"name":"c","usage":{"cpu":"x","memory":"1Mi"}}]},
              {"metadata":{"name":"empty","namespace":"ns"},"containers":[]}
            ]}"#;
        let map = parse_metrics_list(pods).unwrap();
        assert_eq!(
            map["ns"]["a"],
            UsageSample {
                cpu: 11,
                memory: 20
            }
        );
        assert!(
            !map["ns"].contains_key("bad"),
            "unparsable usage is skipped"
        );
        assert!(!map["ns"].contains_key("empty"));

        let nodes = r#"{"kind":"NodeMetricsList","items":[
            {"metadata":{"name":"n1"},"usage":{"cpu":"250m","memory":"1Gi"}}]}"#;
        let map = parse_metrics_list(nodes).unwrap();
        assert_eq!(
            map[""]["n1"],
            UsageSample {
                cpu: 250,
                memory: 1024
            }
        );

        assert!(parse_metrics_list("not json").is_err());
    }

    #[test]
    fn usage_cell_serializes_as_tagged_value() {
        let cell = Cell::Usage(Box::new(UsageCell {
            value: 250,
            formatted: "250m".to_owned(),
            request: Some(100),
            limit: None,
        }));
        assert_eq!(
            serde_json::to_value(&cell).unwrap(),
            serde_json::json!({
                "type": "usage",
                "value": { "value": 250, "formatted": "250m", "request": 100, "limit": null }
            })
        );
        assert_eq!(
            serde_json::to_value(MetricsAvailability::Pending).unwrap(),
            serde_json::json!({ "state": "pending" })
        );
    }
}
