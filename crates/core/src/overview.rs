//! Overview page data computation (02-08a).
//!
//! Gathers high-level metrics, health status, attention items and recent
//! warnings across all connected clusters from their in-memory resource stores.
//!
//! Each store is read once per overview (one pass under its read lock): the
//! summary counts, the capacity sums and the bounded top-k candidates of the
//! Attention / Recent warnings lists are all taken in that pass, and the IPC
//! items (messages, ages) are only built for the kept candidates.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

use k8s_openapi::jiff::Timestamp;

use crate::api::{
    Attention, AttentionItem, CapacitySummary, Cell, ClusterSummary, ConnectionStatus, KindProblem,
    MetricsAvailability, OverviewData, RightSizingItem, RightSizingReport, Row, RowStatus,
    StoreAccessState, UsageCell, WarningEvent,
};
use crate::cluster::{ClusterManager, OVERVIEW_KINDS, ResourceStore};
use crate::metrics::{
    NODE_CPU_CELL, NODE_MEMORY_CELL, POD_CPU_CELL, POD_MEMORY_CELL, UNMEASURED,
    format_cpu_millicores, format_memory_mib,
};

/// Maximum items returned in the attention list.
pub const ATTENTION_LIMIT: usize = 50;

/// Maximum events returned in the recent warnings list.
pub const WARNING_LIMIT: usize = 50;

/// Maximum items returned in each right-sizing category (`04-09`).
pub const RIGHT_SIZING_LIMIT: usize = 100;

/// Window of [`ClusterSummary::warning_events_1h`], in seconds.
const WARNING_WINDOW_SECS: i64 = 3600;

/// Pod cell index of the `Node` column (empty = not scheduled yet).
const POD_NODE_CELL: usize = 5;

/// Formats RFC 3339 timestamp as short kubectl-style age string (`"45s"`, `"12m"`, `"3h"`, `"2d"`, `"1y"`).
#[must_use]
pub fn format_age(timestamp_str: &str) -> String {
    let Ok(ts) = timestamp_str.parse::<Timestamp>() else {
        if timestamp_str.is_empty() {
            return "—".to_owned();
        }
        return timestamp_str.to_owned();
    };
    format_age_since(ts, Timestamp::now())
}

fn format_age_since(ts: Timestamp, now: Timestamp) -> String {
    let seconds = (now.as_second() - ts.as_second()).max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 365 {
        return format!("{days}d");
    }
    format!("{}y", days / 365)
}

fn calc_pct(part: u64, total: u64) -> u8 {
    let raw = part.saturating_mul(100).checked_div(total).unwrap_or(0);
    u8::try_from(raw.min(255)).unwrap_or(255)
}

fn count_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn extract_text(cells: &[Cell], idx: usize) -> String {
    match cells.get(idx) {
        Some(Cell::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn extract_int(cells: &[Cell], idx: usize) -> Option<i32> {
    match cells.get(idx) {
        Some(Cell::Int(i)) => Some(*i),
        _ => None,
    }
}

fn timestamp_str(cells: &[Cell], idx: usize) -> Option<&str> {
    match cells.get(idx) {
        Some(Cell::Timestamp(s) | Cell::Text(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn extract_timestamp(cells: &[Cell], idx: usize) -> Option<String> {
    timestamp_str(cells, idx).map(str::to_owned)
}

fn extract_usage(cells: &[Cell], idx: usize) -> Option<&UsageCell> {
    match cells.get(idx) {
        Some(Cell::Usage(u)) => Some(u),
        _ => None,
    }
}

fn extract_ready(cells: &[Cell], idx: usize) -> Option<(u32, u32)> {
    match cells.get(idx) {
        Some(Cell::Ready { ready, total }) => Some((*ready, *total)),
        _ => None,
    }
}

fn is_warning_event(row: &Row) -> bool {
    if row.status == RowStatus::Degraded {
        return true;
    }
    match row.cells.first() {
        Some(Cell::Status { label, .. }) => label.eq_ignore_ascii_case("Warning"),
        _ => false,
    }
}

/// `true` when a store's rows reflect the cluster (a completed list), so counts
/// taken from it are real numbers rather than "not known".
fn has_data(state: &StoreAccessState) -> bool {
    matches!(
        state,
        StoreAccessState::Ready | StoreAccessState::Reconnecting
    )
}

/// Problem text of a store watched namespace by namespace (05-01).
fn restricted_message(scope: &crate::api::WatchScope) -> String {
    match scope.namespaces.as_slice() {
        [only] => format!(
            "Restricted to namespace {} (cluster-wide watch forbidden)",
            only.namespace
        ),
        all => format!(
            "Restricted to {} namespaces (cluster-wide watch forbidden)",
            all.len()
        ),
    }
}

/// The user-readable problem of a store that cannot show data, if any.
fn access_problem(state: StoreAccessState) -> Option<String> {
    match state {
        StoreAccessState::Error { message, .. }
        | StoreAccessState::Forbidden { message, .. }
        | StoreAccessState::NotFound { message, .. }
        | StoreAccessState::Unsupported { message, .. } => Some(message),
        StoreAccessState::AuthExpired { hint, .. } => Some(hint),
        StoreAccessState::Ready
        | StoreAccessState::Reconnecting
        | StoreAccessState::Loading { .. } => None,
    }
}

/// Bounded "k smallest by `cmp`" selection in one pass: O(n) amortised time and
/// O(k) memory, instead of collecting and sorting every candidate. `cmp` must be
/// a total order for the result to be deterministic.
struct TopK<T, F> {
    k: usize,
    buf: Vec<T>,
    cmp: F,
    pushed: usize,
}

impl<T, F: Fn(&T, &T) -> Ordering> TopK<T, F> {
    fn new(k: usize, cmp: F) -> Self {
        Self {
            k,
            buf: Vec::with_capacity(k.saturating_mul(2)),
            cmp,
            pushed: 0,
        }
    }

    fn push(&mut self, item: T) {
        self.pushed += 1;
        if self.k == 0 {
            return;
        }
        self.buf.push(item);
        if self.buf.len() >= self.k.saturating_mul(2) {
            self.prune();
        }
    }

    /// Keeps only the `k` smallest items (unordered).
    fn prune(&mut self) {
        if self.buf.len() > self.k {
            let cmp = &self.cmp;
            self.buf
                .select_nth_unstable_by(self.k - 1, |a, b| cmp(a, b));
            self.buf.truncate(self.k);
        }
    }

    /// The `k` smallest items in order, and how many items were pushed in total.
    fn into_sorted(mut self) -> (Vec<T>, usize) {
        self.prune();
        let cmp = &self.cmp;
        self.buf.sort_unstable_by(|a, b| cmp(a, b));
        (self.buf, self.pushed)
    }
}

// --- Recent warnings ------------------------------------------------------

/// Sort key of a warning: newest first (parsed timestamp; rows without a
/// parsable one last, by their raw text descending), then cluster, namespace,
/// name and uid ascending, so the order is total.
struct WarnKey<'a> {
    ts: Option<Timestamp>,
    raw: Option<&'a str>,
    cluster: &'a str,
    namespace: &'a str,
    name: &'a str,
    uid: &'a str,
}

fn cmp_warn(a: &WarnKey<'_>, b: &WarnKey<'_>) -> Ordering {
    let by_time = match (a.ts, b.ts) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => b.raw.cmp(&a.raw),
    };
    by_time
        .then_with(|| a.cluster.cmp(b.cluster))
        .then_with(|| a.namespace.cmp(b.namespace))
        .then_with(|| a.name.cmp(b.name))
        .then_with(|| a.uid.cmp(b.uid))
}

/// A kept warning with its parsed timestamp and uid, for the cross-store merge.
struct WarnCandidate {
    ts: Option<Timestamp>,
    uid: String,
    event: WarningEvent,
}

impl WarnCandidate {
    fn key(&self) -> WarnKey<'_> {
        WarnKey {
            ts: self.ts,
            raw: self.event.last_timestamp.as_deref(),
            cluster: &self.event.cluster,
            namespace: &self.event.namespace,
            name: &self.event.name,
            uid: &self.uid,
        }
    }
}

/// Last-seen timestamp of an event row (`lastTimestamp`/`eventTime`, else creation).
fn event_time_str(row: &Row) -> Option<&str> {
    timestamp_str(&row.cells, 7).or_else(|| timestamp_str(&row.cells, 3))
}

/// One pass over an Event store: the number of warnings seen within the last
/// hour of `now`, and the newest [`WARNING_LIMIT`] warnings.
fn scan_events(store: &ResourceStore, cluster: &str, now: Timestamp) -> (u32, Vec<WarnCandidate>) {
    store.read_rows(|rows| {
        let mut within_hour = 0usize;
        let mut top = TopK::new(
            WARNING_LIMIT,
            |a: &(Option<Timestamp>, &Row), b: &(Option<Timestamp>, &Row)| {
                cmp_warn(&row_warn_key(cluster, a), &row_warn_key(cluster, b))
            },
        );
        for row in rows.values() {
            if !is_warning_event(row) {
                continue;
            }
            let ts = event_time_str(row).and_then(|s| s.parse::<Timestamp>().ok());
            if let Some(ts) = ts {
                let age = now.as_second() - ts.as_second();
                if (0..=WARNING_WINDOW_SECS).contains(&age) {
                    within_hour += 1;
                }
            }
            top.push((ts, row));
        }
        let (kept, _) = top.into_sorted();
        let kept = kept
            .into_iter()
            .map(|(ts, row)| {
                let last_timestamp = event_time_str(row).map(str::to_owned);
                let age = match ts {
                    Some(ts) => format_age_since(ts, now),
                    None => format_age(last_timestamp.as_deref().unwrap_or("")),
                };
                WarnCandidate {
                    ts,
                    uid: row.uid.clone(),
                    event: WarningEvent {
                        cluster: cluster.to_owned(),
                        namespace: row.namespace.clone().unwrap_or_default(),
                        name: row.name.clone(),
                        involved_kind: extract_text(&row.cells, 4),
                        involved_name: extract_text(&row.cells, 5),
                        reason: row.status_reason.clone(),
                        message: extract_text(&row.cells, 2),
                        count: extract_int(&row.cells, 6).unwrap_or(1),
                        last_timestamp,
                        age,
                    },
                }
            })
            .collect();
        (count_u32(within_hour), kept)
    })
}

fn row_warn_key<'a>(cluster: &'a str, (ts, row): &(Option<Timestamp>, &'a Row)) -> WarnKey<'a> {
    WarnKey {
        ts: *ts,
        raw: event_time_str(row),
        cluster,
        namespace: row.namespace.as_deref().unwrap_or(""),
        name: &row.name,
        uid: &row.uid,
    }
}

/// Merges per-store warning candidates into the newest [`WARNING_LIMIT`].
fn merge_warnings(candidates: Vec<WarnCandidate>) -> Vec<WarningEvent> {
    let mut top = TopK::new(WARNING_LIMIT, |a: &WarnCandidate, b: &WarnCandidate| {
        cmp_warn(&a.key(), &b.key())
    });
    for c in candidates {
        top.push(c);
    }
    top.into_sorted().0.into_iter().map(|c| c.event).collect()
}

// --- Attention ------------------------------------------------------------

/// Severity rank: Failed (0) < Pending (1) < Degraded (2) < other (3).
fn rank_status(s: RowStatus) -> u8 {
    match s {
        RowStatus::Failed => 0,
        RowStatus::Pending => 1,
        RowStatus::Degraded => 2,
        _ => 3,
    }
}

/// Position of an attention kind; breaks ties between equally named objects of
/// different kinds in the order the stores are scanned.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "Pod" => 0,
        "Deployment" => 1,
        "StatefulSet" => 2,
        "Certificate" => 3,
        _ => 4,
    }
}

/// Sort key of an attention item: severity, then cluster, namespace, name, kind
/// and uid, so the order is total.
struct AttnKey<'a> {
    rank: u8,
    cluster: &'a str,
    namespace: &'a str,
    name: &'a str,
    kind: u8,
    uid: &'a str,
}

fn cmp_attn(a: &AttnKey<'_>, b: &AttnKey<'_>) -> Ordering {
    a.rank
        .cmp(&b.rank)
        .then_with(|| a.cluster.cmp(b.cluster))
        .then_with(|| a.namespace.cmp(b.namespace))
        .then_with(|| a.name.cmp(b.name))
        .then_with(|| a.kind.cmp(&b.kind))
        .then_with(|| a.uid.cmp(b.uid))
}

struct AttnCandidate {
    uid: String,
    item: AttentionItem,
}

impl AttnCandidate {
    fn key(&self) -> AttnKey<'_> {
        AttnKey {
            rank: rank_status(self.item.status),
            cluster: &self.item.cluster,
            namespace: &self.item.namespace,
            name: &self.item.name,
            kind: kind_rank(&self.item.kind),
            uid: &self.uid,
        }
    }
}

/// Why a row needs attention; the item text is only built for kept rows.
#[derive(Clone, Copy)]
enum AttnReason {
    PodFailing,
    PodPending,
    PodNearLimit { cpu: Option<u64>, mem: Option<u64> },
    UnderDesired { ready: u32, desired: u32 },
    CertExpiring { days_left: i32 },
}

/// A row that qualified for Attention in a store pass.
struct RowAttn<'a> {
    row: &'a Row,
    status: RowStatus,
    reason: AttnReason,
}

/// Per-store selection of the [`ATTENTION_LIMIT`] most severe rows of `kind`,
/// deduplicated by namespace/name (the first qualifying row wins), plus the
/// number of distinct qualifying rows.
struct AttnSelector<'a, F> {
    cluster: &'a str,
    kind: &'a str,
    seen: HashSet<(&'a str, &'a str)>,
    top: TopK<RowAttn<'a>, F>,
}

fn attn_selector<'a>(
    cluster: &'a str,
    kind: &'a str,
) -> AttnSelector<'a, impl Fn(&RowAttn<'a>, &RowAttn<'a>) -> Ordering> {
    let kind_idx = kind_rank(kind);
    let key = move |r: &RowAttn<'a>| AttnKey {
        rank: rank_status(r.status),
        cluster,
        namespace: r.row.namespace.as_deref().unwrap_or(""),
        name: &r.row.name,
        kind: kind_idx,
        uid: &r.row.uid,
    };
    AttnSelector {
        cluster,
        kind,
        seen: HashSet::new(),
        top: TopK::new(ATTENTION_LIMIT, move |a: &RowAttn<'a>, b: &RowAttn<'a>| {
            cmp_attn(&key(a), &key(b))
        }),
    }
}

impl<'a, F: Fn(&RowAttn<'a>, &RowAttn<'a>) -> Ordering> AttnSelector<'a, F> {
    /// `false` if an object with the same namespace/name already qualified.
    fn is_new(&self, row: &'a Row) -> bool {
        !self
            .seen
            .contains(&(row.namespace.as_deref().unwrap_or(""), row.name.as_str()))
    }

    fn push(&mut self, row: &'a Row, status: RowStatus, reason: AttnReason) {
        if self
            .seen
            .insert((row.namespace.as_deref().unwrap_or(""), row.name.as_str()))
        {
            self.top.push(RowAttn {
                row,
                status,
                reason,
            });
        }
    }

    fn finish(self) -> (Vec<AttnCandidate>, usize) {
        let (kept, total) = self.top.into_sorted();
        let items = kept
            .into_iter()
            .map(|r| AttnCandidate {
                uid: r.row.uid.clone(),
                item: attention_item(self.cluster, self.kind, &r),
            })
            .collect();
        (items, total)
    }
}

fn attention_item(cluster: &str, kind: &str, r: &RowAttn<'_>) -> AttentionItem {
    let row = r.row;
    let (reason, message, since) = match r.reason {
        AttnReason::PodFailing => (
            row.status_reason.clone(),
            format_pod_failing_message(row),
            extract_timestamp(&row.cells, 3).or_else(|| extract_timestamp(&row.cells, 7)),
        ),
        AttnReason::PodPending => (
            if row.status_reason.contains("Unschedulable") {
                "Unschedulable".to_owned()
            } else {
                row.status_reason.clone()
            },
            format_pod_pending_message(row),
            extract_timestamp(&row.cells, 7),
        ),
        AttnReason::PodNearLimit { cpu, mem } => (
            "Near limit".to_owned(),
            match (cpu, mem) {
                (Some(c), Some(m)) => format!("CPU at {c}% · Memory at {m}% of limit"),
                (Some(c), None) => format!("CPU at {c}% of limit"),
                (None, Some(m)) => format!("Memory at {m}% of limit"),
                (None, None) => "Near limit".to_owned(),
            },
            extract_timestamp(&row.cells, 7),
        ),
        AttnReason::UnderDesired { ready, desired } => (
            "Under desired".to_owned(),
            format!("{ready}/{desired} ready"),
            // Age column: Deployment cell 4, StatefulSet cell 2.
            extract_timestamp(&row.cells, if kind == "Deployment" { 4 } else { 2 }),
        ),
        AttnReason::CertExpiring { days_left } => (
            if days_left < 0 {
                "Expired".to_owned()
            } else {
                "Expiring soon".to_owned()
            },
            if days_left < 0 {
                let days_ago = -days_left;
                if days_ago == 0 {
                    "certificate expired today".to_owned()
                } else {
                    format!("certificate expired {days_ago}d ago")
                }
            } else {
                format!("certificate expires in {days_left}d")
            },
            extract_timestamp(&row.cells, 4).or_else(|| extract_timestamp(&row.cells, 5)),
        ),
    };
    AttentionItem {
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace: row.namespace.clone().unwrap_or_default(),
        name: row.name.clone(),
        reason,
        message,
        since,
        status: r.status,
    }
}

/// Formats a human-readable detail message for a failing pod.
fn format_pod_failing_message(row: &Row) -> String {
    let restarts = extract_int(&row.cells, 2).unwrap_or(0);
    let last_reason = extract_text(&row.cells, 4);
    let last_restart = timestamp_str(&row.cells, 3);
    let ready = extract_ready(&row.cells, 0);

    if restarts > 0 {
        if !last_reason.is_empty() {
            if let Some(ts) = last_restart {
                let age = format_age(ts);
                format!("{restarts} restarts · last: {last_reason} {age} ago")
            } else {
                format!("{restarts} restarts · last: {last_reason}")
            }
        } else if let Some((r, t)) = ready {
            format!("{restarts} restarts · {r}/{t} ready")
        } else {
            format!("{restarts} restarts")
        }
    } else if let Some((r, t)) = ready {
        format!("{r}/{t} ready")
    } else {
        row.status_reason.clone()
    }
}

/// Formats a human-readable detail message for an unschedulable pod.
fn format_pod_pending_message(row: &Row) -> String {
    if let Some((ready, total)) = extract_ready(&row.cells, 0) {
        format!("{ready}/{total} ready")
    } else if !row.status_reason.is_empty() {
        row.status_reason.clone()
    } else {
        "0/1 ready".to_owned()
    }
}

/// Usage of `u` as a percentage of its limit when it is at least 90 %.
fn near_limit_pct(u: Option<&UsageCell>) -> Option<u64> {
    let u = u?;
    let lim = u.limit?;
    if lim == 0 || u.value == UNMEASURED {
        return None;
    }
    let pct = u64::from(u.value) * 100 / u64::from(lim);
    (pct >= 90).then_some(pct)
}

/// The attention reason of a pod row, if any (failing > unschedulable > near limit).
fn pod_attention(r: &Row) -> Option<(RowStatus, AttnReason)> {
    let is_failing = r.status == RowStatus::Failed
        || r.status_reason.contains("CrashLoopBackOff")
        || r.status_reason.contains("ImagePullBackOff")
        || r.status_reason.contains("ErrImagePull")
        || r.status_reason.contains("Error")
        || r.status_reason.contains("OOMKilled")
        || r.status_reason.contains("CreateContainerConfigError")
        || r.status_reason.contains("CreateContainerError");
    if is_failing {
        return Some((RowStatus::Failed, AttnReason::PodFailing));
    }
    if r.status == RowStatus::Pending
        && (r.status_reason.contains("Unschedulable") || r.status_reason == "Pending")
    {
        return Some((RowStatus::Pending, AttnReason::PodPending));
    }
    let cpu = near_limit_pct(extract_usage(&r.cells, POD_CPU_CELL));
    let mem = near_limit_pct(extract_usage(&r.cells, POD_MEMORY_CELL));
    if cpu.is_some() || mem.is_some() {
        return Some((RowStatus::Degraded, AttnReason::PodNearLimit { cpu, mem }));
    }
    None
}

fn workload_status(r: &Row) -> RowStatus {
    match r.status {
        RowStatus::Failed => RowStatus::Failed,
        RowStatus::Pending => RowStatus::Pending,
        _ => RowStatus::Degraded,
    }
}

/// Deployment below its desired replicas (ready or available).
fn deployment_attention(r: &Row) -> Option<(RowStatus, AttnReason)> {
    let (ready, desired) = extract_ready(&r.cells, 0)?;
    let available = extract_int(&r.cells, 2).unwrap_or(0);
    let is_under =
        desired > 0 && (ready < desired || available < i32::try_from(desired).unwrap_or(i32::MAX));
    is_under.then(|| {
        (
            workload_status(r),
            AttnReason::UnderDesired { ready, desired },
        )
    })
}

/// StatefulSet below its desired replicas.
fn statefulset_attention(r: &Row) -> Option<(RowStatus, AttnReason)> {
    let (ready, desired) = extract_ready(&r.cells, 0)?;
    (desired > 0 && ready < desired).then(|| {
        (
            workload_status(r),
            AttnReason::UnderDesired { ready, desired },
        )
    })
}

/// Attention candidates of a Deployment / StatefulSet store.
fn scan_workloads(
    store: &ResourceStore,
    cluster: &str,
    kind: &str,
    qualifies: fn(&Row) -> Option<(RowStatus, AttnReason)>,
) -> (Vec<AttnCandidate>, usize) {
    store.read_rows(|rows| {
        let mut sel = attn_selector(cluster, kind);
        for r in rows.values() {
            if !sel.is_new(r) {
                continue;
            }
            if let Some((status, reason)) = qualifies(r) {
                sel.push(r, status, reason);
            }
        }
        sel.finish()
    })
}

/// Attention candidates of a Certificate store (expiring within 14 days or expired).
fn scan_certificates(
    store: &ResourceStore,
    cluster: &str,
    now: Timestamp,
) -> (Vec<AttnCandidate>, usize) {
    store.read_rows(|rows| {
        let mut sel = attn_selector(cluster, "Certificate");
        for r in rows.values() {
            if !sel.is_new(r) {
                continue;
            }
            if let Some(ts_str) = extract_timestamp(&r.cells, 4)
                && let Ok(ts) = ts_str.parse::<Timestamp>()
            {
                let diff_sec = ts.as_second() - now.as_second();
                let days_left = i32::try_from(diff_sec / 86400).unwrap_or(0);
                if days_left < 14 {
                    let status = if diff_sec < 0 {
                        RowStatus::Failed
                    } else {
                        RowStatus::Degraded
                    };
                    sel.push(r, status, AttnReason::CertExpiring { days_left });
                }
            }
        }
        sel.finish()
    })
}

/// Merges per-store attention candidates into the [`ATTENTION_LIMIT`] most
/// severe items; `total` is the number of distinct qualifying objects.
fn merge_attention(candidates: Vec<AttnCandidate>, total: usize) -> Attention {
    let mut top = TopK::new(ATTENTION_LIMIT, |a: &AttnCandidate, b: &AttnCandidate| {
        cmp_attn(&a.key(), &b.key())
    });
    for c in candidates {
        top.push(c);
    }
    let items: Vec<AttentionItem> = top.into_sorted().0.into_iter().map(|c| c.item).collect();
    Attention {
        truncated: total > items.len(),
        items,
    }
}

// --- Per-cluster scan -----------------------------------------------------

/// Sums of one pass over a Pod store.
#[derive(Default)]
struct PodScan {
    running: u32,
    pending: u32,
    failing: u32,
    req_cpu: u64,
    req_mem: u64,
    used_cpu: u64,
    used_mem: u64,
    attention: Vec<AttnCandidate>,
    attention_total: usize,
}

fn scan_pods(store: &ResourceStore, cluster: &str) -> PodScan {
    store.read_rows(|rows| {
        let mut out = PodScan::default();
        let mut sel = attn_selector(cluster, "Pod");
        for r in rows.values() {
            if r.status == RowStatus::Healthy
                || (r.status == RowStatus::Degraded && r.status_reason == "Running")
                || r.status_reason == "Running"
            {
                out.running = out.running.saturating_add(1);
            } else if r.status == RowStatus::Pending {
                out.pending = out.pending.saturating_add(1);
            } else if r.status == RowStatus::Failed
                || (r.status == RowStatus::Degraded && r.status_reason != "Running")
            {
                out.failing = out.failing.saturating_add(1);
            }

            // Requests count like `kubectl describe node`'s "Allocated resources":
            // only pods bound to a node. Pods in a terminal phase (Succeeded /
            // Failed) carry no request at all (see `metrics::pod_usage_cells`).
            let scheduled =
                matches!(r.cells.get(POD_NODE_CELL), Some(Cell::Text(n)) if !n.is_empty());
            let cpu = extract_usage(&r.cells, POD_CPU_CELL);
            let mem = extract_usage(&r.cells, POD_MEMORY_CELL);
            if let Some(u) = cpu {
                if scheduled && let Some(req) = u.request {
                    out.req_cpu = out.req_cpu.saturating_add(u64::from(req));
                }
                if u.value != UNMEASURED {
                    out.used_cpu = out.used_cpu.saturating_add(u64::from(u.value));
                }
            }
            if let Some(u) = mem {
                if scheduled && let Some(req) = u.request {
                    out.req_mem = out.req_mem.saturating_add(u64::from(req));
                }
                if u.value != UNMEASURED {
                    out.used_mem = out.used_mem.saturating_add(u64::from(u.value));
                }
            }

            if sel.is_new(r)
                && let Some((status, reason)) = pod_attention(r)
            {
                sel.push(r, status, reason);
            }
        }
        (out.attention, out.attention_total) = sel.finish();
        out
    })
}

/// Sums of one pass over a Node store.
#[derive(Default)]
struct NodeScan {
    ready: u32,
    total: u32,
    alloc_cpu: u64,
    alloc_mem: u64,
    used_cpu: u64,
    used_mem: u64,
}

fn scan_nodes(store: &ResourceStore) -> NodeScan {
    store.read_rows(|rows| {
        let mut out = NodeScan {
            total: count_u32(rows.len()),
            ..NodeScan::default()
        };
        for r in rows.values() {
            if r.status_reason.starts_with("Ready") {
                out.ready = out.ready.saturating_add(1);
            }
            if let Some(u) = extract_usage(&r.cells, NODE_CPU_CELL) {
                if let Some(lim) = u.limit {
                    out.alloc_cpu = out.alloc_cpu.saturating_add(u64::from(lim));
                }
                if u.value != UNMEASURED {
                    out.used_cpu = out.used_cpu.saturating_add(u64::from(u.value));
                }
            }
            if let Some(u) = extract_usage(&r.cells, NODE_MEMORY_CELL) {
                if let Some(lim) = u.limit {
                    out.alloc_mem = out.alloc_mem.saturating_add(u64::from(lim));
                }
                if u.value != UNMEASURED {
                    out.used_mem = out.used_mem.saturating_add(u64::from(u.value));
                }
            }
        }
        out
    })
}

fn capacity(
    used: u64,
    requested: u64,
    allocatable: u64,
    format: fn(u32) -> String,
) -> CapacitySummary {
    let fmt = |v: u64| format(u32::try_from(v).unwrap_or(u32::MAX));
    CapacitySummary {
        used,
        requested,
        allocatable,
        formatted_used: fmt(used),
        formatted_requested: fmt(requested),
        formatted_allocatable: fmt(allocatable),
        used_pct: calc_pct(used, allocatable),
        requested_pct: calc_pct(requested, allocatable),
    }
}

/// Everything one cluster contributes to the overview.
struct ClusterScan {
    summary: ClusterSummary,
    attention: Vec<AttnCandidate>,
    attention_total: usize,
    warnings: Vec<WarnCandidate>,
}

fn scan_cluster(manager: &ClusterManager, cluster: &str, now: Timestamp) -> ClusterScan {
    let mut stores: BTreeMap<&str, (std::sync::Arc<ResourceStore>, StoreAccessState)> =
        BTreeMap::new();
    let mut problems = Vec::new();
    for &kind in OVERVIEW_KINDS {
        if let Some(store) = manager.get_store(cluster, kind) {
            let access = store.access();
            if let Some(message) = access_problem(access.clone()) {
                problems.push(KindProblem {
                    kind: kind.to_owned(),
                    message,
                });
            } else if let Some(scope) = store.scope() {
                // Counts are real but cover only these namespaces (05-01).
                problems.push(KindProblem {
                    kind: kind.to_owned(),
                    message: restricted_message(&scope),
                });
            }
            stores.insert(kind, (store, access));
        }
    }
    let with_data = |kind: &str| {
        stores
            .get(kind)
            .filter(|(_, access)| has_data(access))
            .map(|(store, _)| store)
    };

    let mut attention = Vec::new();
    let mut attention_total = 0usize;

    let nodes = stores.get("Node").map(|(s, _)| scan_nodes(s));
    let pods = stores.get("Pod").map(|(s, _)| scan_pods(s, cluster));
    for (kind, qualifies) in [
        (
            "Deployment",
            deployment_attention as fn(&Row) -> Option<(RowStatus, AttnReason)>,
        ),
        ("StatefulSet", statefulset_attention),
    ] {
        if let Some((store, _)) = stores.get(kind) {
            let (items, total) = scan_workloads(store, cluster, kind, qualifies);
            attention.extend(items);
            attention_total += total;
        }
    }
    if let Some(store) = manager.get_store(cluster, "Certificate") {
        let (items, total) = scan_certificates(&store, cluster, now);
        attention.extend(items);
        attention_total += total;
    }
    let (warnings_1h, warnings) = stores
        .get("Event")
        .map_or((0, Vec::new()), |(s, _)| scan_events(s, cluster, now));

    let node_known = with_data("Node").is_some();
    let pod_known = with_data("Pod").is_some();

    let metrics = stores
        .get("Node")
        .and_then(|(s, _)| s.metrics())
        .or_else(|| stores.get("Pod").and_then(|(s, _)| s.metrics()))
        .unwrap_or(MetricsAvailability::Pending);

    let (cpu, memory) = match (&nodes, &pods) {
        (Some(n), Some(p))
            if metrics == MetricsAvailability::Available && node_known && pod_known =>
        {
            let used_cpu = if n.used_cpu > 0 {
                n.used_cpu
            } else {
                p.used_cpu
            };
            let used_mem = if n.used_mem > 0 {
                n.used_mem
            } else {
                p.used_mem
            };
            (
                Some(capacity(
                    used_cpu,
                    p.req_cpu,
                    n.alloc_cpu,
                    format_cpu_millicores,
                )),
                Some(capacity(
                    used_mem,
                    p.req_mem,
                    n.alloc_mem,
                    format_memory_mib,
                )),
            )
        }
        _ => (None, None),
    };

    let node_counts = nodes.as_ref().filter(|_| node_known);
    let pod_counts = pods.as_ref().filter(|_| pod_known);
    let summary = ClusterSummary {
        cluster: cluster.to_owned(),
        connection: connection_status(manager, cluster),
        nodes_ready: node_counts.map(|n| n.ready),
        nodes_total: node_counts.map(|n| n.total),
        pods_running: pod_counts.map(|p| p.running),
        pods_pending: pod_counts.map(|p| p.pending),
        pods_failing: pod_counts.map(|p| p.failing),
        warning_events_1h: with_data("Event").map(|_| warnings_1h),
        metrics,
        cpu,
        memory,
        problems,
    };

    if let Some(p) = pods {
        attention.extend(p.attention);
        attention_total += p.attention_total;
    }

    ClusterScan {
        summary,
        attention,
        attention_total,
        warnings,
    }
}

/// Computes the overview for `clusters` (bounded and deduplicated by
/// [`ClusterManager::overview_clusters`]; empty = the currently connected
/// clusters). Every resolved cluster gets a card, also when it is still
/// connecting or its connection failed (see [`connection_status`]). Reads the
/// in-memory stores only (each once); never connects.
#[must_use]
pub fn compute_overview(manager: &ClusterManager, clusters: &[String]) -> OverviewData {
    let clusters = manager.overview_clusters(clusters);
    let now = Timestamp::now();
    let mut summaries = Vec::with_capacity(clusters.len());
    let mut attention = Vec::new();
    let mut attention_total = 0usize;
    let mut warnings = Vec::new();
    for cluster in &clusters {
        let scan = scan_cluster(manager, cluster, now);
        summaries.push(scan.summary);
        attention.extend(scan.attention);
        attention_total += scan.attention_total;
        warnings.extend(scan.warnings);
    }
    OverviewData {
        clusters: summaries,
        attention: merge_attention(attention, attention_total),
        recent_warnings: merge_warnings(warnings),
        right_sizing: Some(compute_right_sizing(manager, &clusters, 20)),
    }
}

/// Computes right-sizing and resource efficiency analysis across `clusters` (`04-09`).
#[must_use]
pub fn compute_right_sizing(
    manager: &ClusterManager,
    clusters: &[String],
    threshold_pct: u8,
) -> RightSizingReport {
    let clusters = manager.overview_clusters(clusters);
    let mut metrics_available = false;

    let mut near_limit = Vec::new();
    let mut over_request = Vec::new();
    let mut unused_requests = Vec::new();
    let mut missing_resources = Vec::new();

    for cluster in &clusters {
        let Some(pod_store) = manager.get_store(cluster, "Pod") else {
            continue;
        };

        pod_store.read_rows(|rows| {
            for r in rows.values() {
                if r.status == RowStatus::Completed {
                    continue;
                }

                let cpu = extract_usage(&r.cells, POD_CPU_CELL);
                let mem = extract_usage(&r.cells, POD_MEMORY_CELL);

                let has_cpu_measurement = cpu.is_some_and(|u| u.value != UNMEASURED);
                let has_mem_measurement = mem.is_some_and(|u| u.value != UNMEASURED);

                if has_cpu_measurement || has_mem_measurement {
                    metrics_available = true;
                }

                // 1. Missing requests or limits
                let cpu_req = cpu.and_then(|u| u.request);
                let cpu_lim = cpu.and_then(|u| u.limit);
                let mem_req = mem.and_then(|u| u.request);
                let mem_lim = mem.and_then(|u| u.limit);

                let missing_cpu = cpu_req.is_none() || cpu_lim.is_none();
                let missing_mem = mem_req.is_none() || mem_lim.is_none();

                if missing_cpu || missing_mem {
                    let label = if cpu_req.is_none()
                        && cpu_lim.is_none()
                        && mem_req.is_none()
                        && mem_lim.is_none()
                    {
                        "No requests or limits configured".to_owned()
                    } else if cpu_lim.is_none() && mem_lim.is_none() {
                        "No CPU or memory limits configured".to_owned()
                    } else if cpu_lim.is_none() {
                        "No CPU limit configured".to_owned()
                    } else if mem_lim.is_none() {
                        "No memory limit configured (OOM risk)".to_owned()
                    } else if cpu_req.is_none() && mem_req.is_none() {
                        "No CPU or memory requests configured".to_owned()
                    } else if cpu_req.is_none() {
                        "No CPU request configured".to_owned()
                    } else {
                        "No memory request configured".to_owned()
                    };

                    missing_resources.push(RightSizingItem {
                        cluster: cluster.clone(),
                        namespace: r.namespace.clone().unwrap_or_default(),
                        name: r.name.clone(),
                        kind: "Pod".to_owned(),
                        resource: if missing_cpu && missing_mem {
                            "cpu/memory".to_owned()
                        } else if missing_cpu {
                            "cpu".to_owned()
                        } else {
                            "memory".to_owned()
                        },
                        usage: 0,
                        formatted_usage: "—".to_owned(),
                        request: cpu_req.or(mem_req),
                        formatted_request: format_opt_resource(cpu_req, mem_req),
                        limit: cpu_lim.or(mem_lim),
                        formatted_limit: format_opt_resource(cpu_lim, mem_lim),
                        percent: None,
                        wasted: None,
                        formatted_wasted: "—".to_owned(),
                        label,
                    });
                }

                // 2. Near limit (>= 80% limit)
                if let Some(u) = cpu
                    && u.value != UNMEASURED
                {
                    if let Some(lim) = u.limit
                        && lim > 0
                    {
                        let pct = (u64::from(u.value) * 100 / u64::from(lim)) as u32;
                        if pct >= 80 {
                            near_limit.push(RightSizingItem {
                                cluster: cluster.clone(),
                                namespace: r.namespace.clone().unwrap_or_default(),
                                name: r.name.clone(),
                                kind: "Pod".to_owned(),
                                resource: "cpu".to_owned(),
                                usage: u.value,
                                formatted_usage: format_cpu_millicores(u.value),
                                request: u.request,
                                formatted_request: u
                                    .request
                                    .map_or("—".to_owned(), format_cpu_millicores),
                                limit: Some(lim),
                                formatted_limit: format_cpu_millicores(lim),
                                percent: Some(pct),
                                wasted: None,
                                formatted_wasted: "—".to_owned(),
                                label: format!("{pct}% limit · Throttling risk"),
                            });
                        }
                    }

                    if let Some(req) = u.request
                        && req > 0
                        && u.value > req
                    {
                        let pct = (u64::from(u.value) * 100 / u64::from(req)) as u32;
                        over_request.push(RightSizingItem {
                            cluster: cluster.clone(),
                            namespace: r.namespace.clone().unwrap_or_default(),
                            name: r.name.clone(),
                            kind: "Pod".to_owned(),
                            resource: "cpu".to_owned(),
                            usage: u.value,
                            formatted_usage: format_cpu_millicores(u.value),
                            request: Some(req),
                            formatted_request: format_cpu_millicores(req),
                            limit: u.limit,
                            formatted_limit: u.limit.map_or("—".to_owned(), format_cpu_millicores),
                            percent: Some(pct),
                            wasted: None,
                            formatted_wasted: "—".to_owned(),
                            label: format!("{pct}% of request"),
                        });
                    }

                    if let Some(req) = u.request
                        && req > 0
                    {
                        let pct = (u64::from(u.value) * 100 / u64::from(req)) as u32;
                        if pct < u32::from(threshold_pct) {
                            let wasted = req.saturating_sub(u.value);
                            unused_requests.push(RightSizingItem {
                                cluster: cluster.clone(),
                                namespace: r.namespace.clone().unwrap_or_default(),
                                name: r.name.clone(),
                                kind: "Pod".to_owned(),
                                resource: "cpu".to_owned(),
                                usage: u.value,
                                formatted_usage: format_cpu_millicores(u.value),
                                request: Some(req),
                                formatted_request: format_cpu_millicores(req),
                                limit: u.limit,
                                formatted_limit: u
                                    .limit
                                    .map_or("—".to_owned(), format_cpu_millicores),
                                percent: Some(pct),
                                wasted: Some(wasted),
                                formatted_wasted: format_cpu_millicores(wasted),
                                label: format!(
                                    "{} wasted ({pct}% used)",
                                    format_cpu_millicores(wasted)
                                ),
                            });
                        }
                    }
                }

                if let Some(u) = mem
                    && u.value != UNMEASURED
                {
                    if let Some(lim) = u.limit
                        && lim > 0
                    {
                        let pct = (u64::from(u.value) * 100 / u64::from(lim)) as u32;
                        if pct >= 80 {
                            near_limit.push(RightSizingItem {
                                cluster: cluster.clone(),
                                namespace: r.namespace.clone().unwrap_or_default(),
                                name: r.name.clone(),
                                kind: "Pod".to_owned(),
                                resource: "memory".to_owned(),
                                usage: u.value,
                                formatted_usage: format_memory_mib(u.value),
                                request: u.request,
                                formatted_request: u
                                    .request
                                    .map_or("—".to_owned(), format_memory_mib),
                                limit: Some(lim),
                                formatted_limit: format_memory_mib(lim),
                                percent: Some(pct),
                                wasted: None,
                                formatted_wasted: "—".to_owned(),
                                label: format!("{pct}% limit · OOMKill risk"),
                            });
                        }
                    }

                    if let Some(req) = u.request
                        && req > 0
                        && u.value > req
                    {
                        let pct = (u64::from(u.value) * 100 / u64::from(req)) as u32;
                        over_request.push(RightSizingItem {
                            cluster: cluster.clone(),
                            namespace: r.namespace.clone().unwrap_or_default(),
                            name: r.name.clone(),
                            kind: "Pod".to_owned(),
                            resource: "memory".to_owned(),
                            usage: u.value,
                            formatted_usage: format_memory_mib(u.value),
                            request: Some(req),
                            formatted_request: format_memory_mib(req),
                            limit: u.limit,
                            formatted_limit: u.limit.map_or("—".to_owned(), format_memory_mib),
                            percent: Some(pct),
                            wasted: None,
                            formatted_wasted: "—".to_owned(),
                            label: format!("{pct}% of request"),
                        });
                    }

                    if let Some(req) = u.request
                        && req > 0
                    {
                        let pct = (u64::from(u.value) * 100 / u64::from(req)) as u32;
                        if pct < u32::from(threshold_pct) {
                            let wasted = req.saturating_sub(u.value);
                            unused_requests.push(RightSizingItem {
                                cluster: cluster.clone(),
                                namespace: r.namespace.clone().unwrap_or_default(),
                                name: r.name.clone(),
                                kind: "Pod".to_owned(),
                                resource: "memory".to_owned(),
                                usage: u.value,
                                formatted_usage: format_memory_mib(u.value),
                                request: Some(req),
                                formatted_request: format_memory_mib(req),
                                limit: u.limit,
                                formatted_limit: u.limit.map_or("—".to_owned(), format_memory_mib),
                                percent: Some(pct),
                                wasted: Some(wasted),
                                formatted_wasted: format_memory_mib(wasted),
                                label: format!(
                                    "{} wasted ({pct}% used)",
                                    format_memory_mib(wasted)
                                ),
                            });
                        }
                    }
                }
            }
        });
    }

    let notice = if !metrics_available {
        Some("metrics-server not installed: right-sizing requires live container metrics from metrics.k8s.io".to_owned())
    } else {
        None
    };

    near_limit.sort_by_key(|a| std::cmp::Reverse(a.percent));
    near_limit.truncate(RIGHT_SIZING_LIMIT);

    over_request.sort_by_key(|a| std::cmp::Reverse(a.percent));
    over_request.truncate(RIGHT_SIZING_LIMIT);

    unused_requests.sort_by_key(|a| std::cmp::Reverse(a.wasted));
    unused_requests.truncate(RIGHT_SIZING_LIMIT);

    missing_resources.truncate(RIGHT_SIZING_LIMIT);

    RightSizingReport {
        metrics_available,
        notice,
        near_limit,
        over_request,
        unused_requests,
        missing_resources,
        threshold_pct,
    }
}

fn format_opt_resource(cpu: Option<u32>, mem: Option<u32>) -> String {
    match (cpu, mem) {
        (Some(c), Some(m)) => format!("{}/{}", format_cpu_millicores(c), format_memory_mib(m)),
        (Some(c), None) => format_cpu_millicores(c),
        (None, Some(m)) => format_memory_mib(m),
        (None, None) => "—".to_owned(),
    }
}

/// Connection state of a cluster card, derived from the access states of its
/// [`OVERVIEW_KINDS`] stores (in that order), so the card never claims more than
/// the stores show:
///
/// - no store yet → `Disconnected` (nothing was requested for this cluster);
/// - no store has data (`Ready`/`Reconnecting`): an `Error` while neither the
///   `Node` nor the `Pod` store is still loading → `Error { message }` (first
///   error: unreachable, TLS, auth, ...); else any store `Loading` →
///   `Connecting`; else every store is denied / missing → `Degraded { message }`;
/// - the `Node` or `Pod` store is still on its first list (`Loading`) →
///   `Connecting` (the headline counts are not known yet);
/// - some store has an `Error` / `Forbidden` / `NotFound` / `Unsupported` state
///   while others have data → `Degraded { message }` (first such message);
/// - otherwise → `Connected`.
#[must_use]
pub fn connection_status(manager: &ClusterManager, cluster: &str) -> ConnectionStatus {
    let mut any_store = false;
    let mut has_data = false;
    let mut headline_loading = false;
    let mut any_loading = false;
    let mut first_error: Option<String> = None;
    let mut first_problem: Option<String> = None;

    for &kind in OVERVIEW_KINDS {
        let Some(store) = manager.get_store(cluster, kind) else {
            continue;
        };
        any_store = true;
        match store.access() {
            StoreAccessState::Ready | StoreAccessState::Reconnecting => has_data = true,
            StoreAccessState::Loading { .. } => {
                any_loading = true;
                if kind == "Node" || kind == "Pod" {
                    headline_loading = true;
                }
            }
            StoreAccessState::Error { message, .. } => {
                if first_error.is_none() {
                    first_error = Some(message.clone());
                }
                if first_problem.is_none() {
                    first_problem = Some(message);
                }
            }
            StoreAccessState::Forbidden { message, .. }
            | StoreAccessState::NotFound { message, .. }
            | StoreAccessState::Unsupported { message, .. } => {
                if first_problem.is_none() {
                    first_problem = Some(message);
                }
            }
            StoreAccessState::AuthExpired { hint, .. } => {
                if first_error.is_none() {
                    first_error = Some(hint.clone());
                }
                if first_problem.is_none() {
                    first_problem = Some(hint);
                }
            }
        }
    }

    if !any_store {
        return ConnectionStatus::Disconnected;
    }
    if !has_data {
        if !headline_loading && let Some(message) = first_error {
            return ConnectionStatus::Error { message };
        }
        if any_loading {
            return ConnectionStatus::Connecting;
        }
        return match first_problem {
            Some(message) => ConnectionStatus::Degraded { message },
            None => ConnectionStatus::Connecting,
        };
    }
    if headline_loading {
        return ConnectionStatus::Connecting;
    }
    match first_problem {
        Some(message) => ConnectionStatus::Degraded { message },
        None => ConnectionStatus::Connected,
    }
}

/// Computes summary health and metrics for a single cluster.
#[must_use]
pub fn compute_cluster_summary(manager: &ClusterManager, cluster: &str) -> ClusterSummary {
    scan_cluster(manager, cluster, Timestamp::now()).summary
}

/// Computes the attention items across `clusters` (as given, not re-resolved),
/// most severe first, capped at [`ATTENTION_LIMIT`].
#[must_use]
pub fn compute_attention(manager: &ClusterManager, clusters: &[String]) -> Attention {
    let now = Timestamp::now();
    let mut items = Vec::new();
    let mut total = 0usize;
    for cluster in clusters {
        let scan = scan_cluster(manager, cluster, now);
        items.extend(scan.attention);
        total += scan.attention_total;
    }
    merge_attention(items, total)
}

/// Computes recent warning events across `clusters` (as given), newest first,
/// capped at [`WARNING_LIMIT`].
#[must_use]
pub fn compute_recent_warnings(manager: &ClusterManager, clusters: &[String]) -> Vec<WarningEvent> {
    let now = Timestamp::now();
    let mut warnings = Vec::new();
    for cluster in clusters {
        if let Some(store) = manager.get_store(cluster, "Event") {
            warnings.extend(scan_events(&store, cluster, now).1);
        }
    }
    merge_warnings(warnings)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::api::{Cell, RowStatus, StoreAccessState, UsageCell};
    use crate::kubeconfig::KubeconfigSource;

    fn test_manager() -> Arc<ClusterManager> {
        let src = KubeconfigSource::from_paths(vec![]);
        Arc::new(ClusterManager::with_idle_timeout(
            src,
            Duration::from_secs(60),
        ))
    }

    #[test]
    fn test_format_age() {
        assert_eq!(format_age(""), "—");
        assert_eq!(format_age("invalid-date"), "invalid-date");

        let now = k8s_openapi::jiff::Timestamp::now();
        assert_eq!(format_age(&now.to_string()), "0s");

        // 30 seconds ago
        let t_30s = k8s_openapi::jiff::Timestamp::from_second(now.as_second() - 30).unwrap();
        assert_eq!(format_age(&t_30s.to_string()), "30s");

        // 5 minutes ago
        let t_5m = k8s_openapi::jiff::Timestamp::from_second(now.as_second() - 300).unwrap();
        assert_eq!(format_age(&t_5m.to_string()), "5m");

        // 3 hours ago
        let t_3h = k8s_openapi::jiff::Timestamp::from_second(now.as_second() - 10800).unwrap();
        assert_eq!(format_age(&t_3h.to_string()), "3h");

        // 2 days ago
        let t_2d = k8s_openapi::jiff::Timestamp::from_second(now.as_second() - 172800).unwrap();
        assert_eq!(format_age(&t_2d.to_string()), "2d");
    }

    #[test]
    fn test_overview_computation_with_mock_stores() {
        let mgr = test_manager();
        let cluster = "dev-cluster";

        // Setup Node store with 2 nodes: 1 ready, 1 not ready.
        let node_store = mgr.get_or_create_store(cluster, "Node");
        node_store.set_access(StoreAccessState::Ready);
        let node1 = Row {
            uid: "node-1".into(),
            cluster: cluster.into(),
            kind: "Node".into(),
            namespace: None,
            name: "node-1".into(),
            status: RowStatus::Healthy,
            status_reason: "Ready".into(),
            cells: vec![
                Cell::Status {
                    label: "Ready".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: 200,
                    formatted: "200m".into(),
                    request: None,
                    limit: Some(1000),
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: 500,
                    formatted: "500Mi".into(),
                    request: None,
                    limit: Some(2000),
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        let node2 = Row {
            uid: "node-2".into(),
            cluster: cluster.into(),
            kind: "Node".into(),
            namespace: None,
            name: "node-2".into(),
            status: RowStatus::Failed,
            status_reason: "NotReady".into(),
            cells: vec![
                Cell::Status {
                    label: "NotReady".into(),
                    phase: RowStatus::Failed,
                },
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: Some(1000),
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: Some(2000),
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        node_store.upsert_row(node1);
        node_store.upsert_row(node2);

        // Setup Pod store: 1 running, 1 crashloop, 1 pending (unschedulable), 1 near-limit
        let pod_store = mgr.get_or_create_store(cluster, "Pod");
        pod_store.set_access(StoreAccessState::Ready);
        let pod_running = Row {
            uid: "pod-run".into(),
            cluster: cluster.into(),
            kind: "Pod".into(),
            namespace: Some("default".into()),
            name: "web-running".into(),
            status: RowStatus::Healthy,
            status_reason: "Running".into(),
            cells: vec![
                Cell::Ready { ready: 1, total: 1 },
                Cell::Status {
                    label: "Running".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Int(0),
                Cell::Text(String::new()),
                Cell::Text(String::new()),
                Cell::Text("node-1".into()),
                Cell::Text("10.0.0.1".into()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: 50,
                    formatted: "50m".into(),
                    request: Some(100),
                    limit: Some(500),
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: 100,
                    formatted: "100Mi".into(),
                    request: Some(200),
                    limit: Some(500),
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        let pod_crashloop = Row {
            uid: "pod-crash".into(),
            cluster: cluster.into(),
            kind: "Pod".into(),
            namespace: Some("default".into()),
            name: "crashloop".into(),
            status: RowStatus::Failed,
            status_reason: "CrashLoopBackOff".into(),
            cells: vec![
                Cell::Ready { ready: 0, total: 1 },
                Cell::Status {
                    label: "CrashLoopBackOff".into(),
                    phase: RowStatus::Failed,
                },
                Cell::Int(5),
                Cell::Timestamp("2026-01-01T01:00:00Z".into()),
                Cell::Text("Error (1)".into()),
                Cell::Text("node-1".into()),
                Cell::Text("10.0.0.2".into()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: None,
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: None,
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        let pod_pending = Row {
            uid: "pod-pend".into(),
            cluster: cluster.into(),
            kind: "Pod".into(),
            namespace: Some("default".into()),
            name: "pending".into(),
            status: RowStatus::Pending,
            status_reason: "Unschedulable".into(),
            cells: vec![
                Cell::Ready { ready: 0, total: 1 },
                Cell::Status {
                    label: "Pending".into(),
                    phase: RowStatus::Pending,
                },
                Cell::Int(0),
                Cell::Text(String::new()),
                Cell::Text(String::new()),
                Cell::Text(String::new()),
                Cell::Text(String::new()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: None,
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: None,
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        let pod_near_limit = Row {
            uid: "pod-near".into(),
            cluster: cluster.into(),
            kind: "Pod".into(),
            namespace: Some("default".into()),
            name: "near-limit".into(),
            status: RowStatus::Healthy,
            status_reason: "Running".into(),
            cells: vec![
                Cell::Ready { ready: 1, total: 1 },
                Cell::Status {
                    label: "Running".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Int(0),
                Cell::Text(String::new()),
                Cell::Text(String::new()),
                Cell::Text("node-1".into()),
                Cell::Text("10.0.0.3".into()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: 460, // 460 / 500 = 92%
                    formatted: "460m".into(),
                    request: Some(100),
                    limit: Some(500),
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: 100,
                    formatted: "100Mi".into(),
                    request: Some(200),
                    limit: Some(500),
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        pod_store.upsert_row(pod_running);
        pod_store.upsert_row(pod_crashloop);
        pod_store.upsert_row(pod_pending);
        pod_store.upsert_row(pod_near_limit);

        // Setup Deployment store with 1 undersized deployment
        let deploy_store = mgr.get_or_create_store(cluster, "Deployment");
        deploy_store.set_access(StoreAccessState::Ready);
        let deploy_undersized = Row {
            uid: "deploy-under".into(),
            cluster: cluster.into(),
            kind: "Deployment".into(),
            namespace: Some("default".into()),
            name: "undersized".into(),
            status: RowStatus::Pending,
            status_reason: "Progressing".into(),
            cells: vec![
                Cell::Ready { ready: 0, total: 2 },
                Cell::Int(0),
                Cell::Int(0),
                Cell::Status {
                    label: "Progressing".into(),
                    phase: RowStatus::Pending,
                },
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
            ],
            labels: vec![],
            custom: vec![],
        };
        deploy_store.upsert_row(deploy_undersized);

        // Setup Event store with 1 recent warning event (within 1h) and 1 normal event
        let event_store = mgr.get_or_create_store(cluster, "Event");
        event_store.set_access(StoreAccessState::Ready);
        let now_ts = k8s_openapi::jiff::Timestamp::now().to_string();
        let ev_warn = Row {
            uid: "ev-1".into(),
            cluster: cluster.into(),
            kind: "Event".into(),
            namespace: Some("default".into()),
            name: "ev-warn.1".into(),
            status: RowStatus::Degraded,
            status_reason: "FailedScheduling".into(),
            cells: vec![
                Cell::Status {
                    label: "Warning".into(),
                    phase: RowStatus::Degraded,
                },
                Cell::Text("FailedScheduling".into()),
                Cell::Text("0/1 nodes are available".into()),
                Cell::Timestamp(now_ts.clone()),
                Cell::Text("Pod".into()),
                Cell::Text("pending".into()),
                Cell::Int(3),
                Cell::Timestamp(now_ts),
            ],
            labels: vec![],
            custom: vec![],
        };
        let ev_normal = Row {
            uid: "ev-2".into(),
            cluster: cluster.into(),
            kind: "Event".into(),
            namespace: Some("default".into()),
            name: "ev-norm.1".into(),
            status: RowStatus::Healthy,
            status_reason: "Scheduled".into(),
            cells: vec![
                Cell::Status {
                    label: "Normal".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Text("Scheduled".into()),
                Cell::Text("Successfully assigned default/web-running to node-1".into()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Text("Pod".into()),
                Cell::Text("web-running".into()),
                Cell::Int(1),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
            ],
            labels: vec![],
            custom: vec![],
        };
        event_store.upsert_row(ev_warn);
        event_store.upsert_row(ev_normal);

        // Compute overview
        let overview = compute_overview(&mgr, &[]);
        assert_eq!(overview.clusters.len(), 1);
        let cs = &overview.clusters[0];
        assert_eq!(cs.cluster, cluster);
        assert_eq!(cs.connection, ConnectionStatus::Connected);
        assert_eq!(cs.nodes_ready, Some(1));
        assert_eq!(cs.nodes_total, Some(2));
        assert_eq!(cs.pods_running, Some(2)); // web-running and near-limit
        assert_eq!(cs.pods_pending, Some(1)); // pending
        assert_eq!(cs.pods_failing, Some(1)); // crashloop
        assert_eq!(cs.warning_events_1h, Some(1));

        // Attention items should include crashloop, pending, undersized, near-limit
        let names: Vec<&str> = overview
            .attention
            .items
            .iter()
            .map(|it| it.name.as_str())
            .collect();
        assert!(
            names.contains(&"crashloop"),
            "expected crashloop in attention"
        );
        assert!(names.contains(&"pending"), "expected pending in attention");
        assert!(
            names.contains(&"undersized"),
            "expected undersized in attention"
        );
        assert!(
            names.contains(&"near-limit"),
            "expected near-limit in attention"
        );
        assert!(!overview.attention.truncated);

        // Recent warnings
        assert_eq!(overview.recent_warnings.len(), 1);
        assert_eq!(overview.recent_warnings[0].reason, "FailedScheduling");
        assert_eq!(overview.recent_warnings[0].involved_name, "pending");
        assert_eq!(overview.recent_warnings[0].count, 3);
    }

    #[test]
    fn test_attention_capping_at_50() {
        let mgr = test_manager();
        let cluster = "big-cluster";
        let pod_store = mgr.get_or_create_store(cluster, "Pod");
        pod_store.set_access(StoreAccessState::Ready);

        for i in 0..60 {
            let pod = Row {
                uid: format!("pod-{i}"),
                cluster: cluster.into(),
                kind: "Pod".into(),
                namespace: Some("default".into()),
                name: format!("failing-pod-{i}"),
                status: RowStatus::Failed,
                status_reason: "CrashLoopBackOff".into(),
                cells: vec![
                    Cell::Ready { ready: 0, total: 1 },
                    Cell::Status {
                        label: "CrashLoopBackOff".into(),
                        phase: RowStatus::Failed,
                    },
                    Cell::Int(i),
                    Cell::Text(String::new()),
                    Cell::Text("Error".into()),
                    Cell::Text(String::new()),
                    Cell::Text(String::new()),
                    Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                ],
                labels: vec![],
                custom: vec![],
            };
            pod_store.upsert_row(pod);
        }

        let overview = compute_overview(&mgr, &[]);
        assert_eq!(overview.attention.items.len(), 50);
        assert!(overview.attention.truncated);
    }

    #[test]
    fn test_capacity_summary_metrics_available_and_missing() {
        let mgr = test_manager();
        let cluster = "metrics-cluster";
        let node_store = mgr.get_or_create_store(cluster, "Node");
        node_store.set_access(StoreAccessState::Ready);
        let pod_store = mgr.get_or_create_store(cluster, "Pod");
        pod_store.set_access(StoreAccessState::Ready);

        // When metrics are NotInstalled
        node_store.set_metrics(MetricsAvailability::NotInstalled {
            message: "metrics-server missing".into(),
        });
        let overview = compute_overview(&mgr, &[]);
        let cs = &overview.clusters[0];
        assert!(cs.cpu.is_none());
        assert!(cs.memory.is_none());
        assert!(matches!(
            cs.metrics,
            MetricsAvailability::NotInstalled { .. }
        ));

        // Now set metrics to Available
        node_store.set_metrics(MetricsAvailability::Available);
        let node = Row {
            uid: "node-cap".into(),
            cluster: cluster.into(),
            kind: "Node".into(),
            namespace: None,
            name: "node-cap".into(),
            status: RowStatus::Healthy,
            status_reason: "Ready".into(),
            cells: vec![
                Cell::Status {
                    label: "Ready".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: 1000,
                    formatted: "1000m".into(),
                    request: None,
                    limit: Some(2000),
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: 2000,
                    formatted: "2000Mi".into(),
                    request: None,
                    limit: Some(4000),
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        node_store.upsert_row(node);

        let pod = Row {
            uid: "pod-cap".into(),
            cluster: cluster.into(),
            kind: "Pod".into(),
            namespace: Some("default".into()),
            name: "pod-cap".into(),
            status: RowStatus::Healthy,
            status_reason: "Running".into(),
            cells: vec![
                Cell::Ready { ready: 1, total: 1 },
                Cell::Status {
                    label: "Running".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Int(0),
                Cell::Text(String::new()),
                Cell::Text(String::new()),
                Cell::Text("node-cap".into()),
                Cell::Text("10.0.0.1".into()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: 400,
                    formatted: "400m".into(),
                    request: Some(500),
                    limit: Some(1000),
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: 800,
                    formatted: "800Mi".into(),
                    request: Some(1000),
                    limit: Some(2000),
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        pod_store.upsert_row(pod);

        let overview = compute_overview(&mgr, &[]);
        let cs = &overview.clusters[0];
        assert_eq!(cs.metrics, MetricsAvailability::Available);

        let cpu = cs.cpu.as_ref().expect("cpu capacity summary");
        assert_eq!(cpu.used, 1000);
        assert_eq!(cpu.requested, 500);
        assert_eq!(cpu.allocatable, 2000);
        assert_eq!(cpu.used_pct, 50);
        assert_eq!(cpu.requested_pct, 25);
        assert_eq!(cpu.formatted_used, "1000m");
        assert_eq!(cpu.formatted_requested, "500m");
        assert_eq!(cpu.formatted_allocatable, "2000m");

        let mem = cs.memory.as_ref().expect("memory capacity summary");
        assert_eq!(mem.used, 2000);
        assert_eq!(mem.requested, 1000);
        assert_eq!(mem.allocatable, 4000);
        assert_eq!(mem.used_pct, 50);
        assert_eq!(mem.requested_pct, 25);
        assert_eq!(mem.formatted_used, "2000Mi");
        assert_eq!(mem.formatted_requested, "1000Mi");
        assert_eq!(mem.formatted_allocatable, "4000Mi");
    }

    #[tokio::test]
    async fn test_subscribe_overview() {
        let mgr = test_manager();
        let cluster = "sub-cluster";
        let node_store = mgr.get_or_create_store(cluster, "Node");
        node_store.set_access(StoreAccessState::Ready);

        let (sub_id, mut rx) = mgr.subscribe_overview(Vec::new());
        assert!(sub_id > 0);

        // First message is immediate
        let initial = rx.recv().await.expect("initial overview data");
        assert_eq!(initial.clusters.len(), 1);
        assert_eq!(initial.clusters[0].nodes_total, Some(0));

        // Add a node -> bumps store version
        let node = Row {
            uid: "node-sub".into(),
            cluster: cluster.into(),
            kind: "Node".into(),
            namespace: None,
            name: "node-sub".into(),
            status: RowStatus::Healthy,
            status_reason: "Ready".into(),
            cells: vec![
                Cell::Status {
                    label: "Ready".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: None,
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: None,
                    limit: None,
                })),
            ],
            labels: vec![],
            custom: vec![],
        };
        node_store.upsert_row(node);

        // Receive throttled update
        let updated = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timeout waiting for overview update")
            .expect("overview update channel closed");

        assert_eq!(updated.clusters[0].nodes_total, Some(1));
        assert_eq!(updated.clusters[0].nodes_ready, Some(1));
    }

    fn store_with(mgr: &ClusterManager, cluster: &str, kind: &str, state: StoreAccessState) {
        mgr.get_or_create_store(cluster, kind).set_access(state);
    }

    fn error(message: &str) -> StoreAccessState {
        StoreAccessState::Error {
            message: message.into(),
            partial_rows: None,
        }
    }

    fn forbidden(kind: &str) -> StoreAccessState {
        StoreAccessState::Forbidden {
            resource: kind.into(),
            namespace: None,
            message: format!("No permission to list {kind}"),
        }
    }

    #[test]
    fn connection_status_follows_store_access_states() {
        let mgr = test_manager();

        // Nothing requested for the cluster yet.
        assert_eq!(
            connection_status(&mgr, "none"),
            ConnectionStatus::Disconnected
        );

        // Fresh stores (Loading) → Connecting.
        for &kind in OVERVIEW_KINDS {
            mgr.get_or_create_store("fresh", kind);
        }
        assert_eq!(
            connection_status(&mgr, "fresh"),
            ConnectionStatus::Connecting
        );

        // Connection failed on every store → Error with the message.
        for &kind in OVERVIEW_KINDS {
            store_with(&mgr, "down", kind, error("connection refused"));
        }
        assert_eq!(
            connection_status(&mgr, "down"),
            ConnectionStatus::Error {
                message: "connection refused".into()
            }
        );

        // Forbidden on everything → Degraded.
        for &kind in OVERVIEW_KINDS {
            store_with(&mgr, "rbac", kind, forbidden(kind));
        }
        assert_eq!(
            connection_status(&mgr, "rbac"),
            ConnectionStatus::Degraded {
                message: "No permission to list Node".into()
            }
        );

        // All Ready → Connected; Reconnecting keeps it Connected.
        for &kind in OVERVIEW_KINDS {
            store_with(&mgr, "ok", kind, StoreAccessState::Ready);
        }
        assert_eq!(connection_status(&mgr, "ok"), ConnectionStatus::Connected);
        store_with(&mgr, "ok", "Event", StoreAccessState::Reconnecting);
        assert_eq!(connection_status(&mgr, "ok"), ConnectionStatus::Connected);

        // One kind forbidden while others have data → Degraded.
        store_with(&mgr, "ok", "Node", forbidden("Node"));
        assert_eq!(
            connection_status(&mgr, "ok"),
            ConnectionStatus::Degraded {
                message: "No permission to list Node".into()
            }
        );

        // Pods still on their first list while Nodes are Ready → Connecting.
        for &kind in OVERVIEW_KINDS {
            store_with(&mgr, "half", kind, StoreAccessState::Ready);
        }
        store_with(&mgr, "half", "Pod", StoreAccessState::CONNECTING);
        assert_eq!(
            connection_status(&mgr, "half"),
            ConnectionStatus::Connecting
        );
    }

    #[test]
    fn overview_reports_requested_clusters_even_without_data() {
        let mgr = test_manager();
        for &kind in OVERVIEW_KINDS {
            store_with(&mgr, "down", kind, error("dial tcp: connection refused"));
        }
        let requested = vec![
            "down".to_owned(),
            "never-opened".to_owned(),
            "down".to_owned(),
            String::new(),
        ];
        let overview = compute_overview(&mgr, &requested);
        let cards: Vec<(&str, &ConnectionStatus)> = overview
            .clusters
            .iter()
            .map(|c| (c.cluster.as_str(), &c.connection))
            .collect();
        assert_eq!(
            cards,
            vec![
                (
                    "down",
                    &ConnectionStatus::Error {
                        message: "dial tcp: connection refused".into()
                    }
                ),
                ("never-opened", &ConnectionStatus::Disconnected),
            ]
        );
        assert_eq!(overview.clusters[0].nodes_total, None);
        // A store-only cluster outside the request is not reported.
        store_with(&mgr, "other", "Pod", StoreAccessState::Ready);
        assert_eq!(compute_overview(&mgr, &requested).clusters.len(), 2);
        assert_eq!(mgr.client_builds(), 0);
    }

    #[test]
    fn overview_clusters_are_deduplicated_and_capped() {
        use crate::cluster::MAX_OVERVIEW_CLUSTERS;
        let mgr = test_manager();
        let many: Vec<String> = (0..MAX_OVERVIEW_CLUSTERS + 10)
            .flat_map(|i| [format!("c{i:03}"), format!("c{i:03}")])
            .collect();
        let resolved = mgr.overview_clusters(&many);
        assert_eq!(resolved.len(), MAX_OVERVIEW_CLUSTERS);
        // First entries win, result is sorted.
        assert_eq!(resolved.first().map(String::as_str), Some("c000"));
        assert_eq!(
            resolved.last().cloned(),
            Some(format!("c{:03}", MAX_OVERVIEW_CLUSTERS - 1))
        );

        // Empty request → currently connected (store-backed) clusters.
        assert!(mgr.overview_clusters(&[]).is_empty());
        store_with(&mgr, "b", "Pod", StoreAccessState::Ready);
        store_with(&mgr, "a", "Pod", StoreAccessState::Ready);
        assert_eq!(mgr.overview_clusters(&[]), vec!["a", "b"]);
    }

    #[tokio::test]
    async fn empty_overview_request_never_connects() {
        let mgr = test_manager();
        mgr.ensure_watching_overview(&[]).await;
        let (_id, mut rx) = mgr.subscribe_overview(Vec::new());
        let initial = rx.recv().await.expect("initial overview");
        assert!(initial.clusters.is_empty());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mgr.client_builds(), 0);
        assert!(mgr.connected_clusters().is_empty());
    }

    #[tokio::test]
    async fn subscribe_overview_releases_its_leases_when_the_receiver_drops() {
        let mgr = test_manager();
        let clusters = vec!["leased".to_owned()];
        let (_id, mut rx) = mgr.subscribe_overview(clusters.clone());

        // The requested cluster is reported at once (no data yet).
        let initial = rx.recv().await.expect("initial overview");
        assert_eq!(initial.clusters.len(), 1);
        assert_eq!(initial.clusters[0].cluster, "leased");

        for &kind in OVERVIEW_KINDS {
            let store = mgr.get_store("leased", kind).expect("leased store");
            assert_eq!(store.ref_count(), 1, "{kind}");
        }

        // The background ensure-watch fails (the context does not exist), which
        // the next push reports honestly as an Error card.
        let update = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timeout waiting for overview update")
            .expect("overview channel closed");
        assert!(
            matches!(
                &update.clusters[0].connection,
                ConnectionStatus::Error { message } if !message.is_empty()
            ),
            "{:?}",
            update.clusters[0].connection
        );

        drop(rx);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let counts: Vec<u32> = OVERVIEW_KINDS
                .iter()
                .map(|kind| mgr.get_store("leased", kind).map_or(0, |s| s.ref_count()))
                .collect();
            if counts.iter().all(|&c| c == 0) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "leases not released: {counts:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // --- top-k, honest zeros, throttle, bench -----------------------------

    /// Deterministic xorshift64* (no new dependency).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[usize::try_from(self.below(items.len() as u64)).unwrap_or(0)]
        }
    }

    #[test]
    fn top_k_matches_sort_then_truncate() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..500 {
            let n = usize::try_from(rng.below(400)).unwrap();
            let k = [0, 1, 7, 50, 120][round % 5];
            let items: Vec<(u64, u64)> = (0..n).map(|i| (rng.below(20), i as u64)).collect();
            let mut top = TopK::new(k, |a: &(u64, u64), b: &(u64, u64)| a.cmp(b));
            for &it in &items {
                top.push(it);
            }
            let (kept, pushed) = top.into_sorted();
            let mut all = items.clone();
            all.sort_unstable();
            all.truncate(k);
            assert_eq!(kept, all, "round {round} n={n} k={k}");
            assert_eq!(pushed, n);
        }
    }

    fn pod_row(
        cluster: &str,
        uid: &str,
        ns: &str,
        name: &str,
        status: RowStatus,
        reason: &str,
    ) -> Row {
        Row {
            uid: uid.into(),
            cluster: cluster.into(),
            kind: "Pod".into(),
            namespace: Some(ns.into()),
            name: name.into(),
            status,
            status_reason: reason.into(),
            cells: vec![
                Cell::Ready { ready: 0, total: 1 },
                Cell::Status {
                    label: reason.into(),
                    phase: status,
                },
                Cell::Int(1),
                Cell::Text(String::new()),
                Cell::Text(String::new()),
                Cell::Text("node-1".into()),
                Cell::Text(String::new()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Usage(Box::new(UsageCell {
                    value: if uid.ends_with('7') { 95 } else { UNMEASURED },
                    formatted: String::new(),
                    request: Some(100),
                    limit: Some(100),
                })),
                Cell::Usage(Box::new(UsageCell {
                    value: UNMEASURED,
                    formatted: String::new(),
                    request: Some(64),
                    limit: None,
                })),
            ],
            labels: vec![],
            custom: vec![],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn workload_row(
        cluster: &str,
        kind: &str,
        uid: &str,
        ns: &str,
        name: &str,
        status: RowStatus,
        ready: u32,
        desired: u32,
    ) -> Row {
        let mut cells = vec![Cell::Ready {
            ready,
            total: desired,
        }];
        if kind == "Deployment" {
            cells.extend([
                Cell::Int(i32::try_from(ready).unwrap()),
                Cell::Int(i32::try_from(ready).unwrap()),
                Cell::Status {
                    label: "x".into(),
                    phase: status,
                },
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
            ]);
        } else {
            cells.extend([
                Cell::Text(String::new()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
            ]);
        }
        Row {
            uid: uid.into(),
            cluster: cluster.into(),
            kind: kind.into(),
            namespace: Some(ns.into()),
            name: name.into(),
            status,
            status_reason: "x".into(),
            cells,
            labels: vec![],
            custom: vec![],
        }
    }

    fn event_row(cluster: &str, uid: &str, ns: &str, name: &str, warning: bool, ts: &str) -> Row {
        let (label, phase) = if warning {
            ("Warning", RowStatus::Degraded)
        } else {
            ("Normal", RowStatus::Healthy)
        };
        Row {
            uid: uid.into(),
            cluster: cluster.into(),
            kind: "Event".into(),
            namespace: Some(ns.into()),
            name: name.into(),
            status: phase,
            status_reason: "BackOff".into(),
            cells: vec![
                Cell::Status {
                    label: label.into(),
                    phase,
                },
                Cell::Text("BackOff".into()),
                Cell::Text("Back-off restarting failed container".into()),
                Cell::Timestamp("2026-01-01T00:00:00Z".into()),
                Cell::Text("Pod".into()),
                Cell::Text(name.into()),
                Cell::Int(2),
                Cell::Timestamp(ts.into()),
            ],
            labels: vec![],
            custom: vec![],
        }
    }

    /// The old algorithm: every candidate, full sort by the same total order, truncate.
    fn reference_attention(mgr: &ClusterManager, clusters: &[String]) -> Attention {
        let mut all: Vec<AttnCandidate> = Vec::new();
        for cluster in clusters {
            for (kind, qualifies) in [
                (
                    "Pod",
                    pod_attention as fn(&Row) -> Option<(RowStatus, AttnReason)>,
                ),
                ("Deployment", deployment_attention),
                ("StatefulSet", statefulset_attention),
            ] {
                let Some(store) = mgr.get_store(cluster, kind) else {
                    continue;
                };
                store.read_rows(|rows| {
                    let mut seen = HashSet::new();
                    for r in rows.values() {
                        let key = (r.namespace.clone(), r.name.clone());
                        if seen.contains(&key) {
                            continue;
                        }
                        if let Some((status, reason)) = qualifies(r) {
                            seen.insert(key);
                            all.push(AttnCandidate {
                                uid: r.uid.clone(),
                                item: attention_item(
                                    cluster,
                                    kind,
                                    &RowAttn {
                                        row: r,
                                        status,
                                        reason,
                                    },
                                ),
                            });
                        }
                    }
                });
            }
        }
        all.sort_by(|a, b| cmp_attn(&a.key(), &b.key()));
        let truncated = all.len() > ATTENTION_LIMIT;
        all.truncate(ATTENTION_LIMIT);
        Attention {
            items: all.into_iter().map(|c| c.item).collect(),
            truncated,
        }
    }

    fn reference_warnings(mgr: &ClusterManager, clusters: &[String]) -> Vec<WarningEvent> {
        let now = Timestamp::now();
        let mut all = Vec::new();
        for cluster in clusters {
            let Some(store) = mgr.get_store(cluster, "Event") else {
                continue;
            };
            store.read_rows(|rows| {
                for r in rows.values().filter(|r| is_warning_event(r)) {
                    let last_timestamp = event_time_str(r).map(str::to_owned);
                    all.push(WarnCandidate {
                        ts: last_timestamp.as_deref().and_then(|s| s.parse().ok()),
                        uid: r.uid.clone(),
                        event: WarningEvent {
                            cluster: cluster.clone(),
                            namespace: r.namespace.clone().unwrap_or_default(),
                            name: r.name.clone(),
                            involved_kind: extract_text(&r.cells, 4),
                            involved_name: extract_text(&r.cells, 5),
                            reason: r.status_reason.clone(),
                            message: extract_text(&r.cells, 2),
                            count: extract_int(&r.cells, 6).unwrap_or(1),
                            age: match last_timestamp.as_deref().and_then(|s| s.parse().ok()) {
                                Some(ts) => format_age_since(ts, now),
                                None => format_age(last_timestamp.as_deref().unwrap_or("")),
                            },
                            last_timestamp,
                        },
                    });
                }
            });
        }
        all.sort_by(|a, b| cmp_warn(&a.key(), &b.key()));
        all.truncate(WARNING_LIMIT);
        all.into_iter().map(|c| c.event).collect()
    }

    #[test]
    fn overview_top_k_matches_full_sort_on_random_stores() {
        let statuses = [
            (RowStatus::Failed, "CrashLoopBackOff"),
            (RowStatus::Failed, "Error"),
            (RowStatus::Pending, "Pending"),
            (RowStatus::Pending, "Unschedulable"),
            (RowStatus::Pending, "ContainerCreating"),
            (RowStatus::Healthy, "Running"),
            (RowStatus::Completed, "Completed"),
        ];
        let names = ["a", "b", "c", "web", "db", "zz"];
        let namespaces = ["default", "kube-system", "prod"];
        let stamps = [
            "2026-11-10T10:00:00Z",
            "2026-11-10T10:00:00Z",
            "2026-11-10T09:59:59Z",
            "2026-11-10T10:00:00.5Z",
            "2026-11-10T11:00:00+01:00",
            "2025-01-01T00:00:00Z",
            "not-a-time",
            "",
        ];
        let mut rng = Rng(42);
        for round in 0..40 {
            let mgr = test_manager();
            let clusters: Vec<String> = (0..=rng.below(3)).map(|c| format!("c{c}")).collect();
            for cluster in &clusters {
                let n = rng.below(if round % 4 == 0 { 400 } else { 60 });
                for &kind in OVERVIEW_KINDS {
                    mgr.get_or_create_store(cluster, kind)
                        .set_access(StoreAccessState::Ready);
                }
                let pods = mgr.get_or_create_store(cluster, "Pod");
                let deploys = mgr.get_or_create_store(cluster, "Deployment");
                let sets = mgr.get_or_create_store(cluster, "StatefulSet");
                let events = mgr.get_or_create_store(cluster, "Event");
                for i in 0..n {
                    let (status, reason) =
                        statuses[usize::try_from(rng.below(statuses.len() as u64)).unwrap()];
                    let ns = rng.pick(&namespaces);
                    let name = rng.pick(&names);
                    pods.upsert_row(pod_row(
                        cluster,
                        &format!("p{i:04}{}", rng.below(10)),
                        ns,
                        name,
                        status,
                        reason,
                    ));
                    let wl_status =
                        statuses[usize::try_from(rng.below(statuses.len() as u64)).unwrap()].0;
                    let desired = u32::try_from(rng.below(4)).unwrap();
                    let ready = u32::try_from(rng.below(u64::from(desired) + 1)).unwrap();
                    let (kind, store) = if rng.below(2) == 0 {
                        ("Deployment", &deploys)
                    } else {
                        ("StatefulSet", &sets)
                    };
                    store.upsert_row(workload_row(
                        cluster,
                        kind,
                        &format!("w{i:04}"),
                        ns,
                        rng.pick(&names),
                        wl_status,
                        ready,
                        desired,
                    ));
                    events.upsert_row(event_row(
                        cluster,
                        &format!("e{i:04}"),
                        ns,
                        rng.pick(&names),
                        rng.below(4) != 0,
                        rng.pick(&stamps),
                    ));
                }
            }
            let overview = compute_overview(&mgr, &clusters);
            assert_eq!(
                overview.attention,
                reference_attention(&mgr, &clusters),
                "round {round}"
            );
            assert_eq!(
                overview.recent_warnings,
                reference_warnings(&mgr, &clusters),
                "round {round}"
            );
            assert_eq!(compute_attention(&mgr, &clusters), overview.attention);
        }
    }

    #[test]
    fn attention_keeps_severity_order_and_truncation() {
        let mgr = test_manager();
        let pods = mgr.get_or_create_store("c", "Pod");
        pods.set_access(StoreAccessState::Ready);
        // 30 pending, 30 failing, 30 near limit (uid ending in 7).
        for i in 0..30 {
            pods.upsert_row(pod_row(
                "c",
                &format!("pend{i:02}"),
                "ns",
                &format!("pend{i:02}"),
                RowStatus::Pending,
                "Pending",
            ));
            pods.upsert_row(pod_row(
                "c",
                &format!("fail{i:02}"),
                "ns",
                &format!("fail{i:02}"),
                RowStatus::Failed,
                "Error",
            ));
            pods.upsert_row(pod_row(
                "c",
                &format!("near{i:02}7"),
                "ns",
                &format!("near{i:02}"),
                RowStatus::Healthy,
                "Running",
            ));
        }
        let attention = compute_overview(&mgr, &[]).attention;
        assert!(attention.truncated);
        assert_eq!(attention.items.len(), ATTENTION_LIMIT);
        assert!(
            attention.items[..30]
                .iter()
                .all(|i| i.status == RowStatus::Failed)
        );
        assert!(
            attention.items[30..]
                .iter()
                .all(|i| i.status == RowStatus::Pending)
        );
        assert_eq!(attention.items[0].name, "fail00");
        assert_eq!(attention.items[30].name, "pend00");

        // Exactly ATTENTION_LIMIT qualifying objects: not truncated.
        let mgr = test_manager();
        let pods = mgr.get_or_create_store("c", "Pod");
        for i in 0..ATTENTION_LIMIT {
            pods.upsert_row(pod_row(
                "c",
                &format!("u{i:02}"),
                "ns",
                &format!("p{i:02}"),
                RowStatus::Failed,
                "Error",
            ));
        }
        let attention = compute_overview(&mgr, &[]).attention;
        assert_eq!(attention.items.len(), ATTENTION_LIMIT);
        assert!(!attention.truncated);
    }

    #[test]
    fn warnings_are_newest_first_with_deterministic_ties() {
        let mgr = test_manager();
        let events = mgr.get_or_create_store("c", "Event");
        events.set_access(StoreAccessState::Ready);
        let now = Timestamp::now();
        let at = |secs_ago: i64| {
            Timestamp::from_second(now.as_second() - secs_ago)
                .unwrap()
                .to_string()
        };
        events.upsert_row(event_row("c", "e1", "ns", "old", true, &at(7200)));
        events.upsert_row(event_row("c", "e2", "ns", "b-tie", true, &at(60)));
        events.upsert_row(event_row("c", "e3", "ns", "a-tie", true, &at(60)));
        events.upsert_row(event_row("c", "e4", "ns", "newest", true, &at(5)));
        events.upsert_row(event_row("c", "e5", "ns", "normal", false, &at(1)));
        events.upsert_row(event_row("c", "e6", "ns", "no-time", true, ""));
        let overview = compute_overview(&mgr, &[]);
        let names: Vec<&str> = overview
            .recent_warnings
            .iter()
            .map(|w| w.name.as_str())
            .collect();
        // An event without lastTimestamp falls back to its creation time (Jan 2026).
        assert_eq!(names, vec!["newest", "a-tie", "b-tie", "old", "no-time"]);
        assert_eq!(overview.recent_warnings[0].age, "5s");
        assert_eq!(
            overview.recent_warnings[4].last_timestamp.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        // Three warnings within the hour (the 2 h old and the January one are not).
        assert_eq!(overview.clusters[0].warning_events_1h, Some(3));
    }

    #[test]
    fn unknown_counts_are_none_and_every_problem_is_listed() {
        let mgr = test_manager();
        let cluster = "rbac";
        store_with(&mgr, cluster, "Node", forbidden("Node"));
        store_with(&mgr, cluster, "Event", forbidden("Event"));
        store_with(&mgr, cluster, "Deployment", StoreAccessState::Ready);
        store_with(&mgr, cluster, "StatefulSet", StoreAccessState::Ready);
        let pods = mgr.get_or_create_store(cluster, "Pod");
        pods.set_access(StoreAccessState::Ready);
        pods.upsert_row(pod_row(
            cluster,
            "p1",
            "ns",
            "web",
            RowStatus::Healthy,
            "Running",
        ));
        pods.set_metrics(MetricsAvailability::Available);

        let cs = compute_overview(&mgr, &[]).clusters.remove(0);
        assert_eq!(cs.nodes_ready, None);
        assert_eq!(cs.nodes_total, None);
        assert_eq!(cs.warning_events_1h, None);
        assert_eq!(cs.pods_running, Some(1));
        assert_eq!(cs.pods_pending, Some(0));
        assert_eq!(cs.pods_failing, Some(0));
        // No node allocatable known: no capacity bars, even with metrics.
        assert!(cs.cpu.is_none() && cs.memory.is_none());
        assert_eq!(
            cs.problems,
            vec![
                KindProblem {
                    kind: "Node".into(),
                    message: "No permission to list Node".into()
                },
                KindProblem {
                    kind: "Event".into(),
                    message: "No permission to list Event".into()
                },
            ]
        );
        assert!(matches!(cs.connection, ConnectionStatus::Degraded { .. }));

        // Loading stores: unknown as well, but not a problem.
        let mgr = test_manager();
        for &kind in OVERVIEW_KINDS {
            mgr.get_or_create_store("fresh", kind);
        }
        let cs = compute_overview(&mgr, &[]).clusters.remove(0);
        assert_eq!(
            (cs.nodes_total, cs.pods_running, cs.warning_events_1h),
            (None, None, None)
        );
        assert!(cs.problems.is_empty());

        // Every kind failing: one problem per kind, bounded by OVERVIEW_KINDS.
        let mgr = test_manager();
        for &kind in OVERVIEW_KINDS {
            store_with(&mgr, "down", kind, error("refused"));
        }
        let cs = compute_overview(&mgr, &[]).clusters.remove(0);
        assert_eq!(cs.problems.len(), OVERVIEW_KINDS.len());
    }

    #[test]
    fn requested_capacity_skips_unscheduled_pods() {
        let mgr = test_manager();
        let nodes = mgr.get_or_create_store("c", "Node");
        nodes.set_access(StoreAccessState::Ready);
        nodes.set_metrics(MetricsAvailability::Available);
        let mut node = workload_row("c", "Node", "n1", "", "n1", RowStatus::Healthy, 0, 0);
        node.namespace = None;
        node.status_reason = "Ready".into();
        node.cells = vec![
            Cell::Text("Ready".into()),
            Cell::Timestamp(String::new()),
            Cell::Usage(Box::new(UsageCell {
                value: 100,
                formatted: String::new(),
                request: None,
                limit: Some(1000),
            })),
            Cell::Usage(Box::new(UsageCell {
                value: 100,
                formatted: String::new(),
                request: None,
                limit: Some(1000),
            })),
        ];
        nodes.upsert_row(node);
        let pods = mgr.get_or_create_store("c", "Pod");
        pods.set_access(StoreAccessState::Ready);
        pods.upsert_row(pod_row(
            "c",
            "p1",
            "ns",
            "bound",
            RowStatus::Healthy,
            "Running",
        ));
        let mut unbound = pod_row("c", "p2", "ns", "unbound", RowStatus::Pending, "Pending");
        unbound.cells[POD_NODE_CELL] = Cell::Text(String::new());
        pods.upsert_row(unbound);
        let cs = compute_overview(&mgr, &[]).clusters.remove(0);
        assert_eq!(cs.cpu.as_ref().map(|c| c.requested), Some(100));
        assert_eq!(cs.memory.as_ref().map(|c| c.requested), Some(64));
    }

    fn ready_node(cluster: &str, uid: &str) -> Row {
        let mut row = pod_row(cluster, uid, "", uid, RowStatus::Healthy, "Ready");
        row.kind = "Node".into();
        row.namespace = None;
        row
    }

    #[tokio::test(start_paused = true)]
    async fn overview_push_waits_only_the_rest_of_the_interval() {
        let mgr = test_manager();
        let nodes = mgr.get_or_create_store("t", "Node");
        nodes.set_access(StoreAccessState::Ready);
        let (_id, mut rx) = mgr.subscribe_overview(Vec::new());
        let _initial = rx.recv().await.expect("initial");

        // Quiet for 3 s (< refresh interval): the next change is pushed at once.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let t0 = tokio::time::Instant::now();
        nodes.upsert_row(ready_node("t", "n1"));
        let first = rx.recv().await.expect("push");
        assert_eq!(first.clusters[0].nodes_total, Some(1));
        assert!(
            t0.elapsed() < Duration::from_millis(10),
            "{:?}",
            t0.elapsed()
        );

        // A change right after a push waits for the rest of the 1 s interval.
        let t1 = tokio::time::Instant::now();
        nodes.upsert_row(ready_node("t", "n2"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        nodes.upsert_row(ready_node("t", "n3"));
        let second = rx.recv().await.expect("push");
        let waited = t1.elapsed();
        assert!(
            waited >= OVERVIEW_TEST_MIN && waited < OVERVIEW_TEST_MIN + Duration::from_millis(10),
            "{waited:?}"
        );
        // Coalesced: both changes in one push.
        assert_eq!(second.clusters[0].nodes_total, Some(3));
    }

    const OVERVIEW_TEST_MIN: Duration = Duration::from_secs(1);

    #[tokio::test(start_paused = true)]
    async fn overview_stays_at_most_one_push_per_second_under_churn() {
        let mgr = test_manager();
        let nodes = mgr.get_or_create_store("t", "Node");
        nodes.set_access(StoreAccessState::Ready);
        let (_id, mut rx) = mgr.subscribe_overview(Vec::new());
        let _initial = rx.recv().await.expect("initial");
        let start = tokio::time::Instant::now();
        let mut pushes = Vec::new();
        for i in 0..100 {
            nodes.upsert_row(ready_node("t", &format!("n{i}")));
            tokio::time::sleep(Duration::from_millis(50)).await;
            while let Ok(_data) = rx.try_recv() {
                pushes.push(start.elapsed());
            }
        }
        // 5 s of constant churn: about one push per second, never closer.
        assert!((4..=6).contains(&pushes.len()), "{pushes:?}");
        for pair in pushes.windows(2) {
            assert!(
                pair[1] - pair[0] >= Duration::from_millis(950),
                "{pushes:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn overview_pushes_at_once_when_a_cluster_finishes_connecting() {
        let mgr = test_manager();
        for &kind in OVERVIEW_KINDS {
            mgr.get_or_create_store("cold", kind);
        }
        let (_id, mut rx) = mgr.subscribe_overview(Vec::new());
        let initial = rx.recv().await.expect("initial");
        assert_eq!(initial.clusters[0].connection, ConnectionStatus::Connecting);
        let t0 = tokio::time::Instant::now();

        // Node lists first (Pod still loading: still Connecting, so no early push) ...
        tokio::time::sleep(Duration::from_millis(100)).await;
        let nodes = mgr.get_store("cold", "Node").expect("node store");
        nodes.upsert_row(ready_node("cold", "n1"));
        nodes.set_access(StoreAccessState::Ready);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err(), "no push while still Connecting");

        // ... then Pod: the card is Connected 200 ms after the first push and is
        // sent right away instead of at the 1 s mark.
        for kind in ["Pod", "Deployment", "StatefulSet", "Event"] {
            mgr.get_store("cold", kind)
                .expect("store")
                .set_access(StoreAccessState::Ready);
        }
        let update = rx.recv().await.expect("push");
        assert_eq!(update.clusters[0].connection, ConnectionStatus::Connected);
        assert_eq!(update.clusters[0].nodes_total, Some(1));
        assert!(
            t0.elapsed() < Duration::from_millis(250),
            "{:?}",
            t0.elapsed()
        );

        // The next change is throttled again (≥ 1 s after that push).
        let t1 = tokio::time::Instant::now();
        nodes.upsert_row(ready_node("cold", "n2"));
        let _next = rx.recv().await.expect("push");
        assert!(
            t1.elapsed() >= Duration::from_millis(990),
            "{:?}",
            t1.elapsed()
        );
    }

    /// Release-mode micro bench (02-08 QA): 50k pods + 50k warning events + 500
    /// nodes in one cluster. Budget: `get_overview` p95 < 20 ms.
    ///
    /// `cargo test --release -p fleetglass-core --lib overview::tests::bench_get_overview_50k -- --ignored --nocapture`
    #[test]
    #[ignore = "release-mode micro bench"]
    fn bench_get_overview_50k() {
        let mgr = test_manager();
        let c = "bench";
        for &kind in OVERVIEW_KINDS {
            mgr.get_or_create_store(c, kind)
                .set_access(StoreAccessState::Ready);
        }
        let now = Timestamp::now().as_second();
        let mut rng = Rng(7);
        let statuses = [
            (RowStatus::Healthy, "Running"),
            (RowStatus::Healthy, "Running"),
            (RowStatus::Healthy, "Running"),
            (RowStatus::Failed, "CrashLoopBackOff"),
            (RowStatus::Pending, "Pending"),
            (RowStatus::Completed, "Completed"),
        ];
        let pods: Vec<Row> = (0..50_000)
            .map(|i| {
                let (s, r) = statuses[usize::try_from(rng.below(statuses.len() as u64)).unwrap()];
                pod_row(
                    c,
                    &format!("pod-{i:06}"),
                    &format!("ns-{}", i % 50),
                    &format!("pod-{i}"),
                    s,
                    r,
                )
            })
            .collect();
        mgr.get_or_create_store(c, "Pod").replace_all_rows(&pods);
        let events: Vec<Row> = (0..50_000)
            .map(|i| {
                let ts = Timestamp::from_second(now - i64::try_from(rng.below(7200)).unwrap())
                    .unwrap()
                    .to_string();
                event_row(
                    c,
                    &format!("ev-{i:06}"),
                    &format!("ns-{}", i % 50),
                    &format!("ev-{i}"),
                    true,
                    &ts,
                )
            })
            .collect();
        mgr.get_or_create_store(c, "Event")
            .replace_all_rows(&events);
        let nodes: Vec<Row> = (0..500)
            .map(|i| ready_node(c, &format!("node-{i:03}")))
            .collect();
        mgr.get_or_create_store(c, "Node").replace_all_rows(&nodes);
        let deploys: Vec<Row> = (0..5_000)
            .map(|i| {
                workload_row(
                    c,
                    "Deployment",
                    &format!("d-{i:05}"),
                    "ns",
                    &format!("d-{i}"),
                    RowStatus::Healthy,
                    u32::from(i % 7 != 0),
                    1,
                )
            })
            .collect();
        mgr.get_or_create_store(c, "Deployment")
            .replace_all_rows(&deploys);

        let clusters = vec![c.to_owned()];
        let _warm = mgr.get_overview(&clusters);
        let mut samples: Vec<f64> = (0..50)
            .map(|_| {
                let t = std::time::Instant::now();
                let data = mgr.get_overview(&clusters);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(data.recent_warnings.len(), WARNING_LIMIT);
                assert!(data.attention.truncated);
                ms
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        let pct = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
        println!(
            "get_overview 50k pods + 50k warning events: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms ({} runs)",
            pct(0.50),
            pct(0.95),
            pct(1.0),
            samples.len()
        );
        if !cfg!(debug_assertions) {
            assert!(pct(0.95) < 20.0, "p95 {:.2} ms >= 20 ms budget", pct(0.95));
        }
    }

    #[test]
    fn test_certificate_attention_surfaces_expiring_within_14_days() {
        let mgr = test_manager();
        let cluster = "prod-cert";
        let cert_store = mgr.get_or_create_store(cluster, "Certificate");
        cert_store.set_access(StoreAccessState::Ready);

        let now = k8s_openapi::jiff::Timestamp::now();
        let in_5d = k8s_openapi::jiff::Timestamp::from_second(now.as_second() + 5 * 86400).unwrap();
        let in_60d =
            k8s_openapi::jiff::Timestamp::from_second(now.as_second() + 60 * 86400).unwrap();

        let cert_expiring = Row {
            uid: "cert-exp-uid".into(),
            cluster: cluster.into(),
            kind: "Certificate".into(),
            namespace: Some("default".into()),
            name: "web-tls".into(),
            status: RowStatus::Degraded,
            status_reason: "Expiring".into(),
            cells: vec![
                Cell::Status {
                    label: "Ready".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Text("True".into()),
                Cell::Text("web-tls-secret".into()),
                Cell::Text("letsencrypt".into()),
                Cell::Text(in_5d.to_string()),
                Cell::Timestamp(now.to_string()),
            ],
            labels: vec![],
            custom: vec![],
        };

        let cert_valid = Row {
            uid: "cert-val-uid".into(),
            cluster: cluster.into(),
            kind: "Certificate".into(),
            namespace: Some("default".into()),
            name: "api-tls".into(),
            status: RowStatus::Healthy,
            status_reason: "Ready".into(),
            cells: vec![
                Cell::Status {
                    label: "Ready".into(),
                    phase: RowStatus::Healthy,
                },
                Cell::Text("True".into()),
                Cell::Text("api-tls-secret".into()),
                Cell::Text("letsencrypt".into()),
                Cell::Text(in_60d.to_string()),
                Cell::Timestamp(now.to_string()),
            ],
            labels: vec![],
            custom: vec![],
        };

        cert_store.replace_all_rows(&[cert_expiring, cert_valid]);

        let overview = mgr.get_overview(&[cluster.into()]);
        assert_eq!(overview.attention.items.len(), 1);
        let item = &overview.attention.items[0];
        assert_eq!(item.name, "web-tls");
        assert_eq!(item.kind, "Certificate");
        assert_eq!(item.reason, "Expiring soon");
        assert!(item.message.contains("expires in 5d") || item.message.contains("expires in 4d"));
    }

    #[test]
    fn right_sizing_computation_and_thresholds() {
        let mgr = test_manager();
        let cluster = "dev-rs";
        let pods = mgr.get_or_create_store(cluster, "Pod");
        pods.set_access(StoreAccessState::Ready);

        // 1. Pod near limit: cpu 850m / 1000m (85%), mem 900Mi / 1000Mi (90%)
        let mut r_near = pod_row(
            cluster,
            "p-near",
            "default",
            "pod-near",
            RowStatus::Healthy,
            "Running",
        );
        r_near.cells[POD_CPU_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 850,
            formatted: "850m".into(),
            request: Some(950),
            limit: Some(1000),
        }));
        r_near.cells[POD_MEMORY_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 900,
            formatted: "900Mi".into(),
            request: Some(950),
            limit: Some(1000),
        }));

        // 2. Pod over request: cpu 600m / req 500m (120%)
        let mut r_over = pod_row(
            cluster,
            "p-over",
            "default",
            "pod-over",
            RowStatus::Healthy,
            "Running",
        );
        r_over.cells[POD_CPU_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 600,
            formatted: "600m".into(),
            request: Some(500),
            limit: Some(1000),
        }));
        r_over.cells[POD_MEMORY_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 200,
            formatted: "200Mi".into(),
            request: Some(500),
            limit: Some(1000),
        }));

        // 3. Pod unused requests (over-provisioned):
        // CPU: 50m / req 1000m (5%), wasted = 950m
        // Memory: 150Mi / req 1000Mi (15%), wasted = 850Mi
        let mut r_unused = pod_row(
            cluster,
            "p-unused",
            "default",
            "pod-unused",
            RowStatus::Healthy,
            "Running",
        );
        r_unused.cells[POD_CPU_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 50,
            formatted: "50m".into(),
            request: Some(1000),
            limit: Some(2000),
        }));
        r_unused.cells[POD_MEMORY_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 150,
            formatted: "150Mi".into(),
            request: Some(1000),
            limit: Some(2000),
        }));

        // 4. Pod missing limits/requests
        let mut r_missing = pod_row(
            cluster,
            "p-missing",
            "default",
            "pod-missing",
            RowStatus::Healthy,
            "Running",
        );
        r_missing.cells[POD_CPU_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 100,
            formatted: "100m".into(),
            request: None,
            limit: None,
        }));
        r_missing.cells[POD_MEMORY_CELL] = Cell::Usage(Box::new(UsageCell {
            value: 100,
            formatted: "100Mi".into(),
            request: None,
            limit: None,
        }));

        pods.replace_all_rows(&[r_near, r_over, r_unused, r_missing]);

        // Compute with 20% threshold
        let rs_20 = compute_right_sizing(&mgr, &[cluster.into()], 20);
        assert!(rs_20.metrics_available);
        assert!(rs_20.notice.is_none());

        // Near limit: both CPU (85%) and Memory (90%) from pod-near
        assert_eq!(rs_20.near_limit.len(), 2);
        assert_eq!(rs_20.near_limit[0].name, "pod-near");
        assert_eq!(rs_20.near_limit[0].percent, Some(90)); // sorted desc
        assert!(rs_20.near_limit[0].label.contains("OOMKill risk"));

        // Over request: pod-over CPU at 120%
        assert_eq!(rs_20.over_request.len(), 1);
        assert_eq!(rs_20.over_request[0].name, "pod-over");
        assert_eq!(rs_20.over_request[0].percent, Some(120));

        // Unused requests at 20%:
        // CPU 5% is < 20% (wasted 950m)
        // Memory 15% is < 20% (wasted 850Mi)
        assert_eq!(rs_20.unused_requests.len(), 2);
        assert_eq!(rs_20.unused_requests[0].wasted, Some(950)); // sorted by wasted desc
        assert_eq!(rs_20.unused_requests[1].wasted, Some(850));

        // Threshold switch: at 10%, only CPU 5% qualifies (< 10%), Memory 15% does not
        let rs_10 = compute_right_sizing(&mgr, &[cluster.into()], 10);
        assert_eq!(rs_10.unused_requests.len(), 1);
        assert_eq!(rs_10.unused_requests[0].resource, "cpu");
        assert_eq!(rs_10.unused_requests[0].wasted, Some(950));

        // Missing resources: pod-missing is reported
        assert_eq!(rs_20.missing_resources.len(), 1);
        assert_eq!(rs_20.missing_resources[0].name, "pod-missing");
        assert!(
            rs_20.missing_resources[0]
                .label
                .contains("No requests or limits")
        );

        // Verify OverviewData includes right_sizing report
        let overview = compute_overview(&mgr, &[cluster.into()]);
        assert!(overview.right_sizing.is_some());
    }
}
