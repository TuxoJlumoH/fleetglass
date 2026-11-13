//! Pure in-memory query engine for filtering, sorting, and paginating [`Row`]s.
//!
//! All operations work on `&Row` borrows from the watch cache snapshot and clone
//! only the final `[offset .. offset + limit]` window so the UI never receives
//! an unbounded list (performance budget: bounded windowing).

use std::cmp::Ordering;

use crate::api::{
    Cell, Page, QuerySpec, Row, RowStatus, SortDirection, SortField, SortSpec, StatusCounts,
};
use crate::metrics::{UNMEASURED, finalize_usage_cells};

/// Maximum number of rows a single [`execute_query`] call may return.
pub const MAX_PAGE_LIMIT: u32 = 1000;

/// Default page size used when [`QuerySpec::limit`] is `0`.
pub const DEFAULT_PAGE_LIMIT: u32 = 100;

/// Normalises a requested page limit into `1..=MAX_PAGE_LIMIT`, defaulting `0`
/// to [`DEFAULT_PAGE_LIMIT`].
#[must_use]
pub fn clamp_limit(limit: u32) -> u32 {
    if limit == 0 {
        DEFAULT_PAGE_LIMIT
    } else {
        limit.min(MAX_PAGE_LIMIT)
    }
}

/// Executes a windowed filter, sort, and pagination query over `rows`.
#[must_use]
pub fn execute_query_iter<'a, I>(rows: I, spec: &QuerySpec, version: u32) -> Page
where
    I: IntoIterator<Item = &'a Row>,
{
    let limit = clamp_limit(spec.limit);
    let offset = spec.offset;

    let label_reqs = spec
        .label_selector
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_label_selector)
        .unwrap_or_default();

    let parsed_filter = spec
        .text_filter
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|raw| crate::filter::Filter::parse(raw).ok());

    let fallback_needle_lower = if parsed_filter.is_none() {
        spec.text_filter
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
    } else {
        None
    };

    let has_clusters = !spec.clusters.is_empty();
    let has_kinds = !spec.kinds.is_empty();
    let has_namespaces = !spec.namespaces.is_empty();
    let has_labels = !label_reqs.is_empty();
    // Bit set over the seven variants: dedupes `spec.statuses` and bounds it.
    let status_mask = spec
        .statuses
        .iter()
        .fold(0u8, |mask, s| mask | status_bit(*s));
    let mut status_counts = StatusCounts::default();

    let mut matched: Vec<&Row> = rows
        .into_iter()
        .filter(|row| {
            if has_clusters && !spec.clusters.iter().any(|c| c == &row.cluster) {
                return false;
            }
            if has_kinds && !spec.kinds.iter().any(|k| k.eq_ignore_ascii_case(&row.kind)) {
                return false;
            }
            if has_namespaces
                && !row
                    .namespace
                    .as_ref()
                    .is_some_and(|ns| spec.namespaces.iter().any(|n| n == ns))
            {
                return false;
            }
            if has_labels && !matches_labels(&row.labels, &label_reqs) {
                return false;
            }
            if let Some(ref filter) = parsed_filter {
                if !filter.matches(row) {
                    return false;
                }
            } else if let Some(ref needle) = fallback_needle_lower
                && !matches_text_filter(row, needle)
            {
                return false;
            }
            // Counts ignore the status filter (the picker shows each option's yield).
            status_counts.add(row.status);
            status_mask == 0 || status_mask & status_bit(row.status) != 0
        })
        .collect();

    let total = u32::try_from(matched.len()).unwrap_or(u32::MAX);
    let start = usize::try_from(offset).unwrap_or(usize::MAX);

    if start >= matched.len() {
        return Page {
            total,
            offset,
            limit,
            rows: Vec::new(),
            version,
            access: Vec::new(),
            status_counts,
        };
    }

    let end = start
        .saturating_add(usize::try_from(limit).unwrap_or(usize::MAX))
        .min(matched.len());

    let cmp = |a: &&Row, b: &&Row| compare_rows(a, b, spec.sort.as_ref());

    if end < matched.len() {
        matched.select_nth_unstable_by(end, cmp);
        let prefix = &mut matched[..end];
        if start > 0 {
            prefix.select_nth_unstable_by(start, cmp);
            prefix[start..].sort_unstable_by(cmp);
        } else {
            prefix.sort_unstable_by(cmp);
        }
    } else if start > 0 {
        matched.select_nth_unstable_by(start, cmp);
        matched[start..].sort_unstable_by(cmp);
    } else {
        matched.sort_unstable_by(cmp);
    }

    let page_rows = matched[start..end]
        .iter()
        .map(|r| {
            let mut row = (*r).clone();
            finalize_usage_cells(&mut row);
            row
        })
        .collect();

    Page {
        total,
        offset,
        limit,
        rows: page_rows,
        version,
        access: Vec::new(),
        status_counts,
    }
}

/// Executes a windowed filter, sort, and pagination query over `rows`.
#[must_use]
pub fn execute_query(rows: &[Row], spec: &QuerySpec, version: u32) -> Page {
    execute_query_iter(rows.iter(), spec, version)
}

const fn status_bit(status: RowStatus) -> u8 {
    1 << (status as u8)
}

/// Allocation-free ASCII case-insensitive substring check.
///
/// `needle_lower` must already be lowercase ASCII.
#[must_use]
pub fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    let n_bytes = needle_lower.as_bytes();
    let h_bytes = haystack.as_bytes();
    if n_bytes.is_empty() {
        return true;
    }
    if n_bytes.len() > h_bytes.len() {
        return false;
    }
    let first = n_bytes[0];
    let max_start = h_bytes.len() - n_bytes.len();
    for i in 0..=max_start {
        if h_bytes[i].to_ascii_lowercase() == first
            && h_bytes[i + 1..i + n_bytes.len()]
                .iter()
                .zip(&n_bytes[1..])
                .all(|(h, n)| h.to_ascii_lowercase() == *n)
        {
            return true;
        }
    }
    false
}

fn matches_text_filter(row: &Row, needle_lower: &str) -> bool {
    if contains_ignore_ascii_case(&row.name, needle_lower) {
        return true;
    }
    if row
        .namespace
        .as_deref()
        .is_some_and(|ns| contains_ignore_ascii_case(ns, needle_lower))
    {
        return true;
    }
    if contains_ignore_ascii_case(&row.status_reason, needle_lower) {
        return true;
    }
    row.cells.iter().any(|cell| match cell {
        Cell::Text(text) => contains_ignore_ascii_case(text, needle_lower),
        _ => false,
    })
}

#[derive(Debug)]
enum LabelReq<'a> {
    Equals { key: &'a str, value: &'a str },
    NotEquals { key: &'a str, value: &'a str },
    Exists { key: &'a str },
    NotExists { key: &'a str },
}

fn parse_label_selector(selector: &str) -> Vec<LabelReq<'_>> {
    selector
        .split(',')
        .filter_map(|raw| {
            let term = raw.trim();
            if term.is_empty() {
                return None;
            }
            if let Some((k, v)) = term.split_once("!=") {
                let key = k.trim();
                let value = v.trim();
                if key.is_empty() {
                    None
                } else {
                    Some(LabelReq::NotEquals { key, value })
                }
            } else if let Some((k, v)) = term.split_once("==") {
                let key = k.trim();
                let value = v.trim();
                if key.is_empty() {
                    None
                } else {
                    Some(LabelReq::Equals { key, value })
                }
            } else if let Some((k, v)) = term.split_once('=') {
                let key = k.trim();
                let value = v.trim();
                if key.is_empty() {
                    None
                } else {
                    Some(LabelReq::Equals { key, value })
                }
            } else if let Some(rest) = term.strip_prefix('!') {
                let key = rest.trim();
                if key.is_empty() {
                    None
                } else {
                    Some(LabelReq::NotExists { key })
                }
            } else {
                Some(LabelReq::Exists { key: term })
            }
        })
        .collect()
}

fn lookup_label<'a>(labels: &'a [(String, String)], key: &str) -> Option<&'a str> {
    labels
        .binary_search_by(|(k, _)| k.as_str().cmp(key))
        .ok()
        .map(|idx| labels[idx].1.as_str())
}

fn matches_labels(labels: &[(String, String)], reqs: &[LabelReq<'_>]) -> bool {
    reqs.iter().all(|req| match req {
        LabelReq::Equals { key, value } => lookup_label(labels, key) == Some(*value),
        LabelReq::NotEquals { key, value } => lookup_label(labels, key) != Some(*value),
        LabelReq::Exists { key } => lookup_label(labels, key).is_some(),
        LabelReq::NotExists { key } => lookup_label(labels, key).is_none(),
    })
}

fn compare_rows(a: &Row, b: &Row, sort: Option<&SortSpec>) -> Ordering {
    if let Some(spec) = sort {
        let primary = match spec.field {
            SortField::Name => a.name.cmp(&b.name),
            SortField::Namespace => a.namespace.cmp(&b.namespace),
            SortField::Cluster => a.cluster.cmp(&b.cluster),
            SortField::Status => a.status_reason.cmp(&b.status_reason),
            SortField::Cell(idx) => {
                let i = usize::try_from(idx).unwrap_or(usize::MAX);
                let (ca, cb) = (a.cells.get(i), b.cells.get(i));
                // Rows without a usage measurement go last in both directions.
                match (measured_usage(ca), measured_usage(cb)) {
                    (true, false) => return Ordering::Less,
                    (false, true) => return Ordering::Greater,
                    _ => compare_optional_cells(ca, cb),
                }
            }
            SortField::Custom(idx) => {
                let i = usize::try_from(idx).unwrap_or(usize::MAX);
                compare_optional_cells(a.custom.get(i), b.custom.get(i))
            }
        };
        let directed = match spec.direction {
            SortDirection::Asc => primary,
            SortDirection::Desc => primary.reverse(),
        };
        directed.then_with(|| tie_break(a, b))
    } else {
        tie_break(a, b)
    }
}

fn tie_break(a: &Row, b: &Row) -> Ordering {
    a.name
        .cmp(&b.name)
        .then_with(|| a.namespace.cmp(&b.namespace))
        .then_with(|| a.cluster.cmp(&b.cluster))
        .then_with(|| a.uid.cmp(&b.uid))
}

fn compare_optional_cells(a: Option<&Cell>, b: Option<&Cell>) -> Ordering {
    match (a, b) {
        (Some(ca), Some(cb)) => compare_cells(ca, cb),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_cells(a: &Cell, b: &Cell) -> Ordering {
    match (a, b) {
        (Cell::Int(va), Cell::Int(vb)) => va.cmp(vb),
        (
            Cell::Ready {
                ready: r1,
                total: t1,
            },
            Cell::Ready {
                ready: r2,
                total: t2,
            },
        ) => (r1, t1).cmp(&(r2, t2)),
        (Cell::Timestamp(ta), Cell::Timestamp(tb)) => ta.cmp(tb),
        (Cell::Text(ta), Cell::Text(tb)) => ta.cmp(tb),
        (Cell::Status { label: la, .. }, Cell::Status { label: lb, .. }) => la.cmp(lb),
        (Cell::Usage(ua), Cell::Usage(ub)) => ua.value.cmp(&ub.value),
        _ => cell_rank(a).cmp(&cell_rank(b)),
    }
}

/// `true` for a [`Cell::Usage`] holding a measurement (stored rows mark
/// "no measurement" with [`UNMEASURED`]; finalized pages with `Text("")`).
fn measured_usage(cell: Option<&Cell>) -> bool {
    matches!(cell, Some(Cell::Usage(u)) if u.value != UNMEASURED)
}

const fn cell_rank(cell: &Cell) -> u8 {
    match cell {
        Cell::Int(_) => 0,
        Cell::Usage(_) => 1,
        Cell::Ready { .. } => 2,
        Cell::Status { .. } => 3,
        Cell::Text(_) => 4,
        Cell::Timestamp(_) => 5,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::api::RowStatus;

    #[allow(clippy::too_many_arguments)]
    fn make_row(
        uid: &str,
        cluster: &str,
        namespace: &str,
        name: &str,
        status: RowStatus,
        status_reason: &str,
        restarts: i32,
        node: &str,
        labels: &[(&str, &str)],
    ) -> Row {
        let mut sorted_labels: Vec<(String, String)> = labels
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        sorted_labels.sort();

        Row {
            uid: uid.to_owned(),
            cluster: cluster.to_owned(),
            kind: "Pod".to_owned(),
            namespace: Some(namespace.to_owned()),
            name: name.to_owned(),
            status,
            status_reason: status_reason.to_owned(),
            cells: vec![
                Cell::Ready { ready: 1, total: 1 },
                Cell::Status {
                    label: status_reason.to_owned(),
                    phase: status,
                },
                Cell::Int(restarts),
                Cell::Text(node.to_owned()),
                Cell::Text("10.0.0.1".to_owned()),
                Cell::Timestamp("2026-01-01T00:00:00Z".to_owned()),
            ],
            labels: sorted_labels,
            custom: Vec::new(),
        }
    }

    /// A pod row with stored usage cells at the real CPU / Memory indices;
    /// `cpu = None` leaves them unmeasured.
    fn usage_row(name: &str, cpu: Option<u32>) -> Row {
        use crate::metrics::{
            POD_CPU_CELL, POD_MEMORY_CELL, UsageMap, apply_usage, unmeasured_cell,
        };
        let mut row = make_row(
            &format!("uid-{name}"),
            "c",
            "ns",
            name,
            RowStatus::Healthy,
            "Running",
            0,
            "n",
            &[],
        );
        row.cells
            .resize(POD_MEMORY_CELL + 1, Cell::Text(String::new()));
        row.cells[POD_CPU_CELL] = unmeasured_cell(Some(100), None);
        row.cells[POD_MEMORY_CELL] = unmeasured_cell(None, None);
        if let Some(cpu) = cpu {
            let mut map = UsageMap::new();
            map.entry("ns".to_owned()).or_default().insert(
                name.to_owned(),
                crate::metrics::UsageSample { cpu, memory: cpu },
            );
            apply_usage(&mut row, (POD_CPU_CELL, POD_MEMORY_CELL), Some(&map));
        }
        row
    }

    #[test]
    fn sorts_usage_by_value_with_unmeasured_rows_last_in_both_directions() {
        let rows = vec![
            usage_row("a-none", None),
            usage_row("b-500", Some(500)),
            usage_row("c-0", Some(0)),
            usage_row("d-none", None),
            usage_row("e-20", Some(20)),
        ];
        let cpu = u32::try_from(crate::metrics::POD_CPU_CELL).unwrap();
        let names = |direction| {
            let mut spec = base_spec();
            spec.sort = Some(SortSpec {
                field: SortField::Cell(cpu),
                direction,
            });
            execute_query(&rows, &spec, 1)
                .rows
                .into_iter()
                .map(|r| r.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(SortDirection::Asc),
            ["c-0", "e-20", "b-500", "a-none", "d-none"]
        );
        assert_eq!(
            names(SortDirection::Desc),
            ["b-500", "e-20", "c-0", "a-none", "d-none"],
            "unmeasured rows stay last (ordered by name) when descending"
        );

        // Windowed pages (select_nth path) agree with the full sort.
        let mut spec = base_spec();
        spec.sort = Some(SortSpec {
            field: SortField::Cell(cpu),
            direction: SortDirection::Desc,
        });
        spec.offset = 2;
        spec.limit = 2;
        let page = execute_query(&rows, &spec, 1);
        let window: Vec<_> = page.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(window, ["c-0", "a-none"]);
        // The page carries the IPC form: the unmeasured cell is empty text, not 0.
        assert_eq!(
            page.rows[1].cells[crate::metrics::POD_CPU_CELL],
            Cell::Text(String::new())
        );
        match &page.rows[0].cells[crate::metrics::POD_CPU_CELL] {
            Cell::Usage(u) => assert_eq!((u.value, u.formatted.as_str()), (0, "0m")),
            other => panic!("expected usage, got {other:?}"),
        }
    }

    fn base_spec() -> QuerySpec {
        QuerySpec {
            clusters: Vec::new(),
            kinds: Vec::new(),
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 100,
            statuses: Vec::new(),
        }
    }

    fn status_rows() -> Vec<Row> {
        let row = |uid: &str, ns: &str, name: &str, status: RowStatus| {
            make_row(uid, "c", ns, name, status, "r", 0, "n", &[])
        };
        vec![
            row("1", "a", "web-1", RowStatus::Healthy),
            row("2", "a", "web-2", RowStatus::Failed),
            row("3", "a", "web-3", RowStatus::Pending),
            row("4", "a", "db-1", RowStatus::Failed),
            row("5", "b", "web-4", RowStatus::Failed),
            row("6", "b", "web-5", RowStatus::Completed),
        ]
    }

    fn names(page: &Page) -> Vec<&str> {
        page.rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn status_counts_ignore_status_filter_but_respect_other_filters() {
        let rows = status_rows();
        let mut spec = base_spec();
        spec.namespaces = vec!["a".to_owned()];
        spec.text_filter = Some("WEB".to_owned());
        spec.statuses = vec![RowStatus::Failed];
        spec.sort = Some(SortSpec {
            field: SortField::Name,
            direction: SortDirection::Asc,
        });

        let page = execute_query(&rows, &spec, 1);
        assert_eq!(names(&page), ["web-2"]);
        assert_eq!(page.total, 1, "total is after the status filter");
        assert_eq!(
            page.status_counts,
            StatusCounts {
                healthy: 1,
                failed: 1,
                pending: 1,
                ..StatusCounts::default()
            },
            "db-1 (text) and namespace b are excluded; Healthy/Pending still counted"
        );

        // Counts are identical without the status filter.
        spec.statuses.clear();
        let unfiltered = execute_query(&rows, &spec, 1);
        assert_eq!(unfiltered.status_counts, page.status_counts);
        assert_eq!(unfiltered.total, 3);
    }

    #[test]
    fn status_filter_applies_to_rows_and_total_and_survives_duplicates() {
        let rows = status_rows();
        let mut spec = base_spec();
        spec.sort = Some(SortSpec {
            field: SortField::Name,
            direction: SortDirection::Asc,
        });

        // Empty = no status filter.
        let all = execute_query(&rows, &spec, 1);
        assert_eq!(all.total, 6);
        assert_eq!(all.status_counts.failed, 3);
        assert_eq!(all.status_counts.get(RowStatus::Completed), 1);
        assert_eq!(all.status_counts.get(RowStatus::Unknown), 0);

        // Duplicates (more entries than variants) are harmless.
        spec.statuses = vec![
            RowStatus::Pending,
            RowStatus::Completed,
            RowStatus::Pending,
            RowStatus::Completed,
            RowStatus::Pending,
            RowStatus::Pending,
            RowStatus::Pending,
            RowStatus::Pending,
            RowStatus::Pending,
        ];
        let page = execute_query(&rows, &spec, 1);
        assert_eq!(names(&page), ["web-3", "web-5"]);
        assert_eq!(page.total, 2);
        assert_eq!(page.status_counts, all.status_counts);

        // Offset past the end still reports counts.
        spec.offset = 10;
        let empty = execute_query(&rows, &spec, 1);
        assert!(empty.rows.is_empty());
        assert_eq!(empty.total, 2);
        assert_eq!(empty.status_counts, all.status_counts);
    }

    #[test]
    fn statuses_default_to_empty_when_absent_from_json() {
        let spec: QuerySpec = serde_json::from_str(
            r#"{"clusters":[],"kinds":[],"namespaces":[],"labelSelector":null,
                "textFilter":null,"sort":null,"offset":0,"limit":10}"#,
        )
        .expect("spec without statuses");
        assert!(spec.statuses.is_empty());
    }

    #[test]
    fn multi_cluster_namespace_label_selector_and_text_filter_correctness() {
        let rows = vec![
            make_row(
                "u1",
                "prod-eu",
                "payments",
                "web-frontend-0",
                RowStatus::Healthy,
                "Running",
                0,
                "node-eu-1",
                &[("app", "web"), ("tier", "edge")],
            ),
            make_row(
                "u2",
                "prod-eu",
                "payments",
                "web-db-0",
                RowStatus::Healthy,
                "Running",
                1,
                "node-eu-2",
                &[("app", "web"), ("tier", "db")],
            ),
            make_row(
                "u3",
                "prod-us",
                "payments",
                "web-api-0",
                RowStatus::Degraded,
                "CrashLoopBackOff",
                7,
                "node-us-9",
                &[("app", "web"), ("tier", "api")],
            ),
            make_row(
                "u4",
                "prod-us",
                " kube-system",
                "coredns-0",
                RowStatus::Healthy,
                "Running",
                0,
                "node-us-1",
                &[("k8s-app", "kube-dns")],
            ),
        ];

        // Filter by cluster + namespace + label_selector (app=web,tier!=db).
        let mut spec = base_spec();
        spec.clusters = vec!["prod-eu".to_owned(), "prod-us".to_owned()];
        spec.kinds = vec!["pod".to_owned()];
        spec.namespaces = vec!["payments".to_owned()];
        spec.label_selector = Some("app=web,tier!=db".to_owned());

        let page = execute_query(&rows, &spec, 42);
        assert_eq!(page.version, 42);
        assert_eq!(page.total, 2);
        let names: Vec<&str> = page.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["web-api-0", "web-frontend-0"]);

        // Text filter matching status_reason case-insensitively.
        spec.text_filter = Some("crashloop".to_owned());
        let page_crash = execute_query(&rows, &spec, 43);
        assert_eq!(page_crash.total, 1);
        assert_eq!(page_crash.rows[0].name, "web-api-0");

        // Text filter matching Cell::Text (node name) case-insensitively.
        spec.text_filter = Some("NODE-EU-1".to_owned());
        let page_node = execute_query(&rows, &spec, 44);
        assert_eq!(page_node.total, 1);
        assert_eq!(page_node.rows[0].name, "web-frontend-0");

        // Structured boolean filter with field qualifier: name:web AND NOT status:failed
        spec.text_filter = Some("name:web AND NOT status:degraded".to_owned());
        let page_struct = execute_query(&rows, &spec, 45);
        assert_eq!(page_struct.total, 1);
        assert_eq!(page_struct.rows[0].name, "web-frontend-0");

        // Regex filter
        spec.text_filter = Some("/^web-api/".to_owned());
        let page_regex = execute_query(&rows, &spec, 46);
        assert_eq!(page_regex.total, 1);
        assert_eq!(page_regex.rows[0].name, "web-api-0");

        // Malformed regex falls back to substring match without failing
        spec.text_filter = Some("/[a-z/".to_owned());
        let page_fallback = execute_query(&rows, &spec, 47);
        assert_eq!(page_fallback.total, 0); // No row has substring "/[a-z/"
    }

    #[test]
    fn sorting_and_pagination_with_limit_clamping() {
        let rows = vec![
            make_row(
                "u1",
                "c1",
                "default",
                "pod-b",
                RowStatus::Healthy,
                "Running",
                2,
                "n1",
                &[],
            ),
            make_row(
                "u2",
                "c1",
                "default",
                "pod-a",
                RowStatus::Degraded,
                "CrashLoopBackOff",
                15,
                "n1",
                &[],
            ),
            make_row(
                "u3",
                "c1",
                "default",
                "pod-c",
                RowStatus::Pending,
                "Pending",
                0,
                "n2",
                &[],
            ),
            make_row(
                "u4",
                "c1",
                "default",
                "pod-d",
                RowStatus::Failed,
                "OOMKilled",
                9,
                "n2",
                &[],
            ),
        ];

        // Sort by Name Asc and Desc.
        let mut spec = base_spec();
        spec.sort = Some(SortSpec {
            field: SortField::Name,
            direction: SortDirection::Asc,
        });
        let asc_names: Vec<String> = execute_query(&rows, &spec, 1)
            .rows
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(asc_names, vec!["pod-a", "pod-b", "pod-c", "pod-d"]);

        spec.sort = Some(SortSpec {
            field: SortField::Name,
            direction: SortDirection::Desc,
        });
        let desc_names: Vec<String> = execute_query(&rows, &spec, 1)
            .rows
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(desc_names, vec!["pod-d", "pod-c", "pod-b", "pod-a"]);

        // Sort by Status Asc and Desc (CrashLoopBackOff < OOMKilled < Pending < Running).
        spec.sort = Some(SortSpec {
            field: SortField::Status,
            direction: SortDirection::Asc,
        });
        let status_asc: Vec<String> = execute_query(&rows, &spec, 1)
            .rows
            .into_iter()
            .map(|r| r.status_reason)
            .collect();
        assert_eq!(
            status_asc,
            vec!["CrashLoopBackOff", "OOMKilled", "Pending", "Running"]
        );

        spec.sort = Some(SortSpec {
            field: SortField::Status,
            direction: SortDirection::Desc,
        });
        let status_desc: Vec<String> = execute_query(&rows, &spec, 1)
            .rows
            .into_iter()
            .map(|r| r.status_reason)
            .collect();
        assert_eq!(
            status_desc,
            vec!["Running", "Pending", "OOMKilled", "CrashLoopBackOff"]
        );

        // Sort by numeric Cell(2) (restarts: 0, 2, 9, 15 — not lexicographical "15" < "2"!).
        spec.sort = Some(SortSpec {
            field: SortField::Cell(2),
            direction: SortDirection::Asc,
        });
        let restarts_asc: Vec<String> = execute_query(&rows, &spec, 1)
            .rows
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(restarts_asc, vec!["pod-c", "pod-b", "pod-d", "pod-a"]);

        spec.sort = Some(SortSpec {
            field: SortField::Cell(2),
            direction: SortDirection::Desc,
        });
        let restarts_desc: Vec<String> = execute_query(&rows, &spec, 1)
            .rows
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(restarts_desc, vec!["pod-a", "pod-d", "pod-b", "pod-c"]);

        // Pagination window offset=1, limit=2 on Cell(2) Desc -> ["pod-d", "pod-b"].
        spec.offset = 1;
        spec.limit = 2;
        let paged = execute_query(&rows, &spec, 9);
        assert_eq!(paged.total, 4);
        assert_eq!(paged.offset, 1);
        assert_eq!(paged.limit, 2);
        let paged_names: Vec<&str> = paged.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(paged_names, vec!["pod-d", "pod-b"]);

        // Clamping: limit=0 defaults to 100, limit=5000 clamps to MAX_PAGE_LIMIT (1000).
        spec.offset = 0;
        spec.limit = 0;
        assert_eq!(execute_query(&rows, &spec, 1).limit, DEFAULT_PAGE_LIMIT);

        spec.limit = 5000;
        assert_eq!(execute_query(&rows, &spec, 1).limit, MAX_PAGE_LIMIT);
    }

    #[test]
    fn synthetic_50k_rows_filter_sort_and_page_under_50ms() {
        let clusters = ["cluster-alpha", "cluster-beta", "cluster-gamma"];
        let namespaces: Vec<String> = (0..20).map(|i| format!("ns-{i:02}")).collect();

        let mut rows = Vec::with_capacity(50_000);
        for i in 0..50_000u32 {
            let cluster = clusters[(i as usize) % clusters.len()];
            let ns = &namespaces[(i as usize) % namespaces.len()];
            let app = if i % 2 == 0 { "web" } else { "worker" };
            let tier = if i % 5 == 0 { "db" } else { "api" };
            let restarts = ((i * 37) % 500) as i32;

            rows.push(Row {
                uid: format!("uid-{i:05}"),
                cluster: cluster.to_owned(),
                kind: "Pod".to_owned(),
                namespace: Some(ns.clone()),
                name: format!("workload-{i:05}"),
                status: RowStatus::Healthy,
                status_reason: "Running".to_owned(),
                cells: vec![
                    Cell::Ready { ready: 1, total: 1 },
                    Cell::Status {
                        label: "Running".to_owned(),
                        phase: RowStatus::Healthy,
                    },
                    Cell::Int(restarts),
                    Cell::Text(format!("node-{}", i % 64)),
                    Cell::Text("10.244.0.1".to_owned()),
                    Cell::Timestamp("2026-01-01T00:00:00Z".to_owned()),
                ],
                labels: vec![
                    ("app".to_owned(), app.to_owned()),
                    ("tier".to_owned(), tier.to_owned()),
                ],
                custom: Vec::new(),
            });
        }

        let spec = QuerySpec {
            clusters: vec!["cluster-alpha".to_owned(), "cluster-beta".to_owned()],
            kinds: vec!["Pod".to_owned()],
            namespaces: Vec::new(),
            label_selector: Some("app=web,tier!=db".to_owned()),
            text_filter: Some("workload-".to_owned()),
            sort: Some(SortSpec {
                field: SortField::Cell(2),
                direction: SortDirection::Desc,
            }),
            offset: 200,
            limit: 60,
            statuses: Vec::new(),
        };

        let start = Instant::now();
        let page = execute_query(&rows, &spec, 7);
        let elapsed = start.elapsed();
        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
        println!(
            "synthetic_50k_rows_filter_sort_and_page elapsed: {elapsed_ms:.2} ms (matched total: {})",
            page.total
        );

        assert_eq!(page.rows.len(), 60);
        assert_eq!(page.offset, 200);
        assert_eq!(page.limit, 60);
        assert!(page.total > 10_000);

        // Verify descending order on Cell(2) with deterministic tie-breaking.
        for pair in page.rows.windows(2) {
            let ord = compare_rows(&pair[0], &pair[1], spec.sort.as_ref());
            assert!(
                ord != Ordering::Greater,
                "rows out of order: {:?} vs {:?}",
                pair[0].name,
                pair[1].name
            );
        }

        let budget_ms = if cfg!(not(debug_assertions)) {
            50.0
        } else {
            200.0
        };
        assert!(
            elapsed_ms < budget_ms,
            "50k query took {elapsed_ms:.2} ms, exceeding budget of {budget_ms:.0} ms"
        );
    }
}
