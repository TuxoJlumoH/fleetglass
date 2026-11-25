//! Fleetglass performance benchmark harness (`S-03`, `R-14`).
//!
//! Measures `fleetglass-core` row projection, windowed query latency (`p50`,
//! `p95`, `max`), time to the first page, and process RSS memory against the
//! budgets in `docs/PLAN.md` §3.
//!
//! Synthetic mode projects in-memory pods. Cluster mode (`--cluster`) runs one
//! [`ClusterManager`] watching Pods on one or more kubeconfig contexts at once,
//! as the app does, and reports every cold first-page run plus peak (during the
//! initial sync) and idle (after the sync) RSS of this bench process only.
//!
//! Cluster-mode timings (R-14a progressive publish): while the stores load, the
//! UI's initial query is polled every ~5 ms.
//! - `first_rows_ms`: first poll that returned at least one row.
//! - `first_page_ms` (budgeted): first poll that returned a *full labelled page*
//!   (`rows >= min(limit, total)`, `total > 0`, every store `Ready` or
//!   `Loading`). The page may come from a partial store, labelled `Loading`.
//! - `full_sync_ms` (no budget): every store `Ready` (initial lists complete).
//! - `loading_query_*`: latency of the polled queries while any store loads.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fleetglass_core::api::{
    Page, QuerySpec, Row, SortDirection, SortField, SortSpec, StoreAccessState,
};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::projection::project_pod;
use fleetglass_core::query::execute_query;
use k8s_openapi::api::core::v1::{
    Container, ContainerState, ContainerStateRunning, ContainerStateWaiting, ContainerStatus, Pod,
    PodSpec, PodStatus,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use serde::Serialize;

// R-14b: mimalloc returns freed memory to the OS more eagerly than glibc's
// per-thread arenas. `--no-default-features` builds the glibc baseline for A/B.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Global allocator this binary was built with, recorded in the report.
const ALLOCATOR: &str = if cfg!(feature = "mimalloc") {
    "mimalloc"
} else {
    "system"
};

const USAGE: &str = "\
usage: bench [--pods N] [--clusters C] [--namespaces M] [--iterations I] [--runs R]
             [--cluster [CONTEXT[,CONTEXT...]]]... [--kubeconfig PATH]... [--idle-secs S]
             [--assert-budgets] [--json-out PATH] [--md-out PATH]

  --cluster     cluster mode. Repeat it or pass a comma-separated list to watch several
                contexts with one ClusterManager (no value: kind-fleetglass-dev).
  --kubeconfig  kubeconfig file merged in order; repeatable. Default: every entry of
                $KUBECONFIG (each one checked), else .dev/kubeconfig. Files under ~/.kube
                are refused.
  --runs        cluster mode: cold first-page runs, each with a fresh ClusterManager
                (default 3). The headline first page is the worst run.
  --idle-secs   cluster mode: wait after the initial sync before reading idle RSS (default 5)";

const DEFAULT_CONTEXT: &str = "kind-fleetglass-dev";
const DEFAULT_KUBECONFIG: &str = ".dev/kubeconfig";

const FIRST_PAGE_BUDGET_MS: f64 = 1500.0;
const QUERY_P95_BUDGET_MS: f64 = 50.0;
/// The UI's `PAGE_SIZE` (`ui/src/stores/pods.ts`): limit of its initial Pod query.
const UI_PAGE_SIZE: u32 = 100;
/// Poll interval for the initial query while the stores load.
const LOADING_POLL: Duration = Duration::from_millis(5);
/// "Memory, 3 clusters with 50k pods total" (`PLAN.md` §3).
const RSS_50K_BUDGET_MB: f64 = 500.0;
/// "Idle memory, 1 cluster" (`PLAN.md` §3).
const RSS_10K_BUDGET_MB: f64 = 200.0;
const SYNC_TIMEOUT: Duration = Duration::from_secs(300);
const RSS_SCOPE_NOTE: &str = "RSS of the bench process only (ClusterManager + watch stores); \
excludes the Tauri/WebKit baseline of the desktop app";

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchConfig {
    pods: usize,
    clusters: usize,
    namespaces: usize,
    iterations: usize,
    runs: usize,
    idle_secs: u64,
    assert_budgets: bool,
    json_out: Option<PathBuf>,
    md_out: Option<PathBuf>,
    /// Contexts to watch in cluster mode; empty means synthetic mode.
    contexts: Vec<String>,
    kubeconfigs: Vec<PathBuf>,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            pods: 10_000,
            clusters: 3,
            namespaces: 20,
            iterations: 50,
            runs: 3,
            idle_secs: 5,
            assert_budgets: false,
            json_out: None,
            md_out: None,
            contexts: Vec::new(),
            kubeconfigs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchReport {
    mode: String,
    /// Global allocator: `"mimalloc"` (default feature) or `"system"` (glibc).
    allocator: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    contexts: Vec<String>,
    /// Pods per context (cluster mode).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pods_per_context: BTreeMap<String, usize>,
    pods: usize,
    clusters: usize,
    namespaces: usize,
    iterations: usize,
    project_all_ms: f64,
    /// Every cold first-page run (cluster mode: one per fresh `ClusterManager`).
    /// Cluster mode: first full labelled page, possibly from `Loading` stores.
    first_page_runs_ms: Vec<f64>,
    /// Worst of `first_page_runs_ms`; compared with the budget.
    first_page_ms: f64,
    /// Cluster mode: per run, first poll of the initial query with >= 1 row.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    first_rows_runs_ms: Vec<f64>,
    /// Cluster mode: per run, until every store was `Ready` (no budget).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    full_sync_runs_ms: Vec<f64>,
    /// Worst of `full_sync_runs_ms` (no budget).
    #[serde(skip_serializing_if = "Option::is_none")]
    full_sync_ms: Option<f64>,
    /// Cluster mode: polled initial queries while any store was loading, all runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    loading_query_samples: Option<usize>,
    /// p95 of those polled queries; compared with the query budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    loading_query_p95_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loading_query_max_ms: Option<f64>,
    query_p50_ms: f64,
    query_p95_ms: f64,
    query_max_ms: f64,
    /// RSS before any watcher started (cluster mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_rss_mb: Option<f64>,
    /// Highest RSS sampled during the initial sync of run 1 (cluster mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    peak_sync_rss_mb: Option<f64>,
    /// RSS `idle_secs` after the initial sync of run 1, before queries (cluster mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_rss_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_secs: Option<u64>,
    /// `(idle - baseline)` per pod, in KiB (cluster mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_kb_per_pod: Option<f64>,
    /// `(peak - baseline)` per pod, in KiB (cluster mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    peak_kb_per_pod: Option<f64>,
    /// RSS compared with the memory budget: idle RSS in cluster mode, RSS after
    /// the queries in synthetic mode.
    rss_mb: f64,
    rss_budget_mb: f64,
    rss_budget_name: String,
    rss_note: String,
    budgets_passed: bool,
    budget_failures: Vec<String>,
}

fn main() -> Result<()> {
    let config = parse_args(std::env::args().skip(1))?;
    let report = if config.contexts.is_empty() {
        run_benchmark(&config)?
    } else {
        let kubeconfigs = resolve_kubeconfigs(
            &config.kubeconfigs,
            std::env::var_os("KUBECONFIG").as_deref(),
            std::env::home_dir().as_deref(),
        )?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build tokio runtime for cluster benchmark")?;
        rt.block_on(run_cluster_benchmark(&config, &kubeconfigs))?
    };

    let json_text =
        serde_json::to_string_pretty(&report).context("failed to serialize JSON report")?;
    let md_text = format_markdown(&report);

    println!("{json_text}\n\n{md_text}");

    if let Some(ref path) = config.json_out {
        fs::write(path, format!("{json_text}\n"))
            .with_context(|| format!("failed to write JSON report to {}", path.display()))?;
    }

    if let Some(ref path) = config.md_out {
        fs::write(path, &md_text)
            .with_context(|| format!("failed to write Markdown report to {}", path.display()))?;
    }

    if config.assert_budgets && !report.budgets_passed {
        bail!(
            "performance budget check failed:\n  - {}",
            report.budget_failures.join("\n  - ")
        );
    }

    Ok(())
}

fn parse_count(value: Option<String>, flag: &str) -> Result<usize> {
    let value = value.with_context(|| format!("{flag} needs a value\n{USAGE}"))?;
    let n: usize = value
        .parse()
        .with_context(|| format!("invalid {flag} value: {value}"))?;
    if n == 0 {
        bail!("{flag} must be greater than 0");
    }
    Ok(n)
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<BenchConfig> {
    let mut cfg = BenchConfig::default();
    let mut args = args.peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cluster" => {
                let value = match args.peek() {
                    Some(val) if !val.starts_with("--") => args.next().unwrap_or_default(),
                    _ => DEFAULT_CONTEXT.to_owned(),
                };
                for context in value.split(',').map(str::trim) {
                    if context.is_empty() {
                        bail!("--cluster has an empty context name in '{value}'");
                    }
                    if !cfg.contexts.iter().any(|c| c == context) {
                        cfg.contexts.push(context.to_owned());
                    }
                }
            }
            "--kubeconfig" => {
                let value = args.next().context(USAGE)?;
                if value.is_empty() {
                    bail!("--kubeconfig must not be empty");
                }
                cfg.kubeconfigs.push(PathBuf::from(value));
            }
            "--pods" => cfg.pods = parse_count(args.next(), "--pods")?,
            "--clusters" => cfg.clusters = parse_count(args.next(), "--clusters")?,
            "--namespaces" => cfg.namespaces = parse_count(args.next(), "--namespaces")?,
            "--iterations" => cfg.iterations = parse_count(args.next(), "--iterations")?,
            "--runs" => cfg.runs = parse_count(args.next(), "--runs")?,
            "--idle-secs" => {
                let value = args.next().context(USAGE)?;
                cfg.idle_secs = value
                    .parse()
                    .with_context(|| format!("invalid --idle-secs value: {value}"))?;
            }
            "--assert-budgets" => {
                cfg.assert_budgets = true;
            }
            "--json-out" => {
                let value = args.next().context(USAGE)?;
                cfg.json_out = Some(PathBuf::from(value));
            }
            "--md-out" => {
                let value = args.next().context(USAGE)?;
                cfg.md_out = Some(PathBuf::from(value));
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}\n{USAGE}"),
        }
    }

    Ok(cfg)
}

/// Picks the kubeconfig files for cluster mode and refuses unsafe ones.
///
/// Order: explicit `--kubeconfig` paths, else every entry of `$KUBECONFIG`,
/// else `.dev/kubeconfig`. Every file must exist and must not be, or resolve
/// (through symlinks) to, a path under `~/.kube` (safety policy: never read the
/// user's real kubeconfig in test/bench runs).
fn resolve_kubeconfigs(
    explicit: &[PathBuf],
    env_value: Option<&OsStr>,
    home: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let candidates: Vec<PathBuf> = if !explicit.is_empty() {
        explicit.to_vec()
    } else if let Some(value) = env_value.filter(|v| !v.is_empty()) {
        std::env::split_paths(value)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    } else {
        vec![PathBuf::from(DEFAULT_KUBECONFIG)]
    };
    if candidates.is_empty() {
        bail!("no kubeconfig file given");
    }
    let kube_dir = home.map(|h| h.join(".kube"));
    let kube_dir_real = kube_dir.as_deref().and_then(|d| d.canonicalize().ok());
    let under_kube_dir = |p: &Path| {
        kube_dir.as_deref().is_some_and(|d| p.starts_with(d))
            || kube_dir_real.as_deref().is_some_and(|d| p.starts_with(d))
    };
    let mut out = Vec::with_capacity(candidates.len());
    for path in candidates {
        if under_kube_dir(&path) {
            bail!(
                "refusing kubeconfig {}: files under ~/.kube are the user's real clusters",
                path.display()
            );
        }
        if !path.is_file() {
            bail!(
                "kubeconfig file not found at {} (start a test cluster: scripts/dev-cluster.sh up \
                 or scripts/kwok-local.sh up)",
                path.display()
            );
        }
        let real = path
            .canonicalize()
            .with_context(|| format!("cannot resolve kubeconfig {}", path.display()))?;
        if under_kube_dir(&real) {
            bail!(
                "refusing kubeconfig {}: it resolves to {} under ~/.kube",
                path.display(),
                real.display()
            );
        }
        out.push(path);
    }
    Ok(out)
}

/// Memory budget: "Idle memory, 1 cluster" for one cluster, "Memory, 3
/// clusters with 50k pods total" for several.
fn rss_budget_for_clusters(clusters: usize) -> (f64, &'static str) {
    if clusters <= 1 {
        (RSS_10K_BUDGET_MB, "Idle memory, 1 cluster")
    } else {
        (RSS_50K_BUDGET_MB, "Memory, 3 clusters with 50k pods total")
    }
}

/// Returns one message per measured value at or above its budget.
fn budget_failures(
    first_page_ms: f64,
    query_p95_ms: f64,
    loading_query_p95_ms: Option<f64>,
    rss_mb: f64,
    rss_budget: f64,
) -> Vec<String> {
    let mut failures = Vec::new();
    if first_page_ms >= FIRST_PAGE_BUDGET_MS {
        failures.push(format!(
            "first_page_ms ({first_page_ms:.2} ms, worst run) >= budget ({FIRST_PAGE_BUDGET_MS:.1} ms)"
        ));
    }
    if query_p95_ms >= QUERY_P95_BUDGET_MS {
        failures.push(format!(
            "query_p95_ms ({query_p95_ms:.2} ms) >= budget ({QUERY_P95_BUDGET_MS:.1} ms)"
        ));
    }
    if let Some(p95) = loading_query_p95_ms
        && p95 >= QUERY_P95_BUDGET_MS
    {
        failures.push(format!(
            "loading_query_p95_ms ({p95:.2} ms) >= budget ({QUERY_P95_BUDGET_MS:.1} ms)"
        ));
    }
    if rss_mb >= rss_budget {
        failures.push(format!(
            "rss_mb ({rss_mb:.2} MB) >= budget ({rss_budget:.1} MB)"
        ));
    }
    failures
}

/// RSS growth over `baseline_mb` per pod, in KiB.
fn kb_per_pod(rss_mb: f64, baseline_mb: f64, pods: usize) -> f64 {
    if pods == 0 {
        return 0.0;
    }
    (rss_mb - baseline_mb).max(0.0) * 1024.0 / pods as f64
}

/// Builds a synthetic [`Pod`] with realistic containers, statuses (`Running`,
/// `Pending`, `CrashLoopBackOff`), labels, node names, and IPs.
pub fn build_synthetic_pod(index: usize, namespace: &str) -> Pod {
    let app = match index % 4 {
        0 => "web",
        1 => "api",
        2 => "worker",
        _ => "cache",
    };
    let tier = if index.is_multiple_of(5) {
        "db"
    } else if index.is_multiple_of(2) {
        "edge"
    } else {
        "backend"
    };

    let mut labels = BTreeMap::new();
    labels.insert("app".to_owned(), app.to_owned());
    labels.insert("tier".to_owned(), tier.to_owned());
    labels.insert(
        "fleetglass.io/shard".to_owned(),
        format!("s-{}", index % 16),
    );

    let restarts = i32::try_from((index * 37) % 250).unwrap_or(0);

    let (phase, container_statuses) = match index % 20 {
        0 => (
            "Running",
            vec![
                ContainerStatus {
                    name: "main".to_owned(),
                    ready: true,
                    restart_count: 0,
                    state: Some(ContainerState {
                        running: Some(ContainerStateRunning::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ContainerStatus {
                    name: "sidecar".to_owned(),
                    ready: false,
                    restart_count: restarts.max(1),
                    state: Some(ContainerState {
                        waiting: Some(ContainerStateWaiting {
                            reason: Some("CrashLoopBackOff".to_owned()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
        ),
        1 => (
            "Pending",
            vec![
                ContainerStatus {
                    name: "main".to_owned(),
                    ready: false,
                    restart_count: 0,
                    state: Some(ContainerState {
                        waiting: Some(ContainerStateWaiting {
                            reason: Some("ContainerCreating".to_owned()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ContainerStatus {
                    name: "sidecar".to_owned(),
                    ready: false,
                    restart_count: 0,
                    state: Some(ContainerState {
                        waiting: Some(ContainerStateWaiting {
                            reason: Some("ContainerCreating".to_owned()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
        ),
        _ => (
            "Running",
            vec![
                ContainerStatus {
                    name: "main".to_owned(),
                    ready: true,
                    restart_count: restarts,
                    state: Some(ContainerState {
                        running: Some(ContainerStateRunning::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ContainerStatus {
                    name: "sidecar".to_owned(),
                    ready: true,
                    restart_count: 0,
                    state: Some(ContainerState {
                        running: Some(ContainerStateRunning::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
        ),
    };

    let octet_2 = (index / 65_536) % 256;
    let octet_3 = (index / 256) % 256;
    let octet_4 = (index % 254) + 1;

    Pod {
        metadata: ObjectMeta {
            uid: Some(format!("uid-{index:06}")),
            name: Some(format!("{app}-workload-{index:06}")),
            namespace: Some(namespace.to_owned()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(PodSpec {
            node_name: Some(format!("node-{:03}", index % 128)),
            containers: vec![
                Container {
                    name: "main".to_owned(),
                    image: Some("ghcr.io/fleetglass/bench:1.0".to_owned()),
                    ..Default::default()
                },
                Container {
                    name: "sidecar".to_owned(),
                    image: Some("ghcr.io/fleetglass/proxy:1.0".to_owned()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        status: Some(PodStatus {
            phase: Some(phase.to_owned()),
            pod_ip: Some(format!("10.{octet_2}.{octet_3}.{octet_4}")),
            container_statuses: Some(container_statuses),
            ..Default::default()
        }),
    }
}

fn run_benchmark(config: &BenchConfig) -> Result<BenchReport> {
    let cluster_names: Vec<String> = (0..config.clusters)
        .map(|i| format!("cluster-{i:02}"))
        .collect();
    let namespace_names: Vec<String> = (0..config.namespaces)
        .map(|i| format!("ns-{i:02}"))
        .collect();

    // Build synthetic Pod objects in 1,000-pod chunks (matching K8s LIST chunking)
    // outside the projection timer so `project_all_ms` times only `project_pod`
    // and RSS reflects the projected Row cache rather than 50k raw Pod fixtures.
    const CHUNK_SIZE: usize = 1000;
    let mut rows: Vec<Row> = Vec::with_capacity(config.pods);
    let mut chunk_pods: Vec<(Pod, &str)> = Vec::with_capacity(CHUNK_SIZE.min(config.pods));
    let mut project_elapsed_secs = 0.0_f64;

    let mut idx = 0;
    while idx < config.pods {
        let end = (idx + CHUNK_SIZE).min(config.pods);
        chunk_pods.clear();
        for i in idx..end {
            let cluster = cluster_names[i % cluster_names.len()].as_str();
            let ns = namespace_names[i % namespace_names.len()].as_str();
            chunk_pods.push((build_synthetic_pod(i, ns), cluster));
        }

        let t0 = Instant::now();
        for (pod, cluster) in &chunk_pods {
            rows.push(project_pod(pod, cluster));
        }
        project_elapsed_secs += t0.elapsed().as_secs_f64();
        idx = end;
    }
    drop(chunk_pods);

    let project_all_ms = project_elapsed_secs * 1000.0;

    let initial_spec = QuerySpec {
        clusters: Vec::new(),
        kinds: vec!["Pod".to_owned()],
        namespaces: Vec::new(),
        label_selector: None,
        text_filter: None,
        sort: Some(SortSpec {
            field: SortField::Name,
            direction: SortDirection::Asc,
        }),
        offset: 0,
        limit: 60,
        statuses: Vec::new(),
    };
    let first_query_start = Instant::now();
    let first_page = execute_query(&rows, &initial_spec, 1);
    let first_page_ms = project_all_ms + first_query_start.elapsed().as_secs_f64() * 1000.0;
    if first_page.rows.is_empty() {
        bail!("first page query unexpectedly returned 0 rows");
    }

    let selected_clusters: Vec<String> = cluster_names.iter().take(2).cloned().collect();
    let selected_namespaces: Vec<String> = namespace_names.iter().take(5).cloned().collect();
    let deep_offset = u32::try_from(config.pods / 2).unwrap_or(0);

    let scenarios = [
        // 1. Text filter + sort by restarts desc (offset: 0, limit: 60)
        QuerySpec {
            clusters: Vec::new(),
            kinds: vec!["Pod".to_owned()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: Some("workload-".to_owned()),
            sort: Some(SortSpec {
                field: SortField::Cell(2),
                direction: SortDirection::Desc,
            }),
            offset: 0,
            limit: 60,
            statuses: Vec::new(),
        },
        // 2. Multi-cluster + namespace + label selector + sort by name asc (offset: 200, limit: 60)
        QuerySpec {
            clusters: selected_clusters,
            kinds: vec!["Pod".to_owned()],
            namespaces: selected_namespaces,
            label_selector: Some("app=web,tier!=db".to_owned()),
            text_filter: None,
            sort: Some(SortSpec {
                field: SortField::Name,
                direction: SortDirection::Asc,
            }),
            offset: 200,
            limit: 60,
            statuses: Vec::new(),
        },
        // 3. Deep page scroll (offset: N / 2, limit: 60)
        QuerySpec {
            clusters: Vec::new(),
            kinds: vec!["Pod".to_owned()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: Some(SortSpec {
                field: SortField::Name,
                direction: SortDirection::Asc,
            }),
            offset: deep_offset,
            limit: 60,
            statuses: Vec::new(),
        },
    ];

    let mut samples_ms = Vec::with_capacity(config.iterations);
    for i in 0..config.iterations {
        let spec = &scenarios[i % scenarios.len()];
        let version = u32::try_from(i + 2).unwrap_or(1);
        let t0 = Instant::now();
        let page = execute_query(&rows, spec, version);
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(&page);
        samples_ms.push(elapsed_ms);
    }

    samples_ms.sort_by(|a, b| a.total_cmp(b));
    let query_p50_ms = percentile_sorted(&samples_ms, 0.50);
    let query_p95_ms = percentile_sorted(&samples_ms, 0.95);
    let query_max_ms = samples_ms.last().copied().unwrap_or(0.0);

    let rss_mb = read_process_rss_mb();

    // Synthetic mode keeps the historical rule: the 1-cluster budget up to 10k
    // pods, the 50k budget above.
    let (rss_budget, rss_budget_name) = rss_budget_for_clusters(if config.pods <= 10_000 {
        1
    } else {
        config.clusters.max(2)
    });
    let budget_failures = budget_failures(first_page_ms, query_p95_ms, None, rss_mb, rss_budget);
    let budgets_passed = budget_failures.is_empty();

    Ok(BenchReport {
        mode: "synthetic".to_owned(),
        allocator: ALLOCATOR.to_owned(),
        contexts: Vec::new(),
        pods_per_context: BTreeMap::new(),
        pods: config.pods,
        clusters: config.clusters,
        namespaces: config.namespaces,
        iterations: config.iterations,
        project_all_ms,
        first_page_runs_ms: vec![first_page_ms],
        first_page_ms,
        first_rows_runs_ms: Vec::new(),
        full_sync_runs_ms: Vec::new(),
        full_sync_ms: None,
        loading_query_samples: None,
        loading_query_p95_ms: None,
        loading_query_max_ms: None,
        query_p50_ms,
        query_p95_ms,
        query_max_ms,
        baseline_rss_mb: None,
        peak_sync_rss_mb: None,
        idle_rss_mb: None,
        idle_secs: None,
        idle_kb_per_pod: None,
        peak_kb_per_pod: None,
        rss_mb,
        rss_budget_mb: rss_budget,
        rss_budget_name: rss_budget_name.to_owned(),
        rss_note: "RSS of the bench process after projecting and querying in memory".to_owned(),
        budgets_passed,
        budget_failures,
    })
}

/// Result of one cold sync: a fresh [`ClusterManager`] starts watching Pods on
/// every context while the UI's initial query is polled until every store is
/// `Ready`.
struct SyncRun {
    manager: ClusterManager,
    /// Start of watching until the first poll returned at least one row.
    first_rows_ms: f64,
    /// Start of watching until the first full labelled page (see [`page_is_first_page`]).
    first_page_ms: f64,
    /// Start of watching until every store was `Ready`.
    full_sync_ms: f64,
    /// Latency of every polled query while some store was not yet `Ready`.
    loading_query_ms: Vec<f64>,
    peak_rss_mb: f64,
}

/// Pod query over `contexts` with the UI's default `PodTable` spec: name
/// ascending, offset 0, limit `PAGE_SIZE` (`defaultQuerySpec` in
/// `ui/src/stores/pods.ts`).
fn pod_query(contexts: &[String]) -> QuerySpec {
    QuerySpec {
        clusters: contexts.to_vec(),
        kinds: vec!["Pod".to_owned()],
        namespaces: Vec::new(),
        label_selector: None,
        text_filter: None,
        sort: Some(SortSpec {
            field: SortField::Name,
            direction: SortDirection::Asc,
        }),
        offset: 0,
        limit: UI_PAGE_SIZE,
        statuses: Vec::new(),
    }
}

/// How the initial query's stores look in one polled page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoresState {
    /// Every store is `Ready`.
    AllReady,
    /// Every store is `Ready` or `Loading` and at least one is `Loading`.
    Loading,
}

/// Classifies the stores of `page`; an error for any failed store. A requested
/// context without a store in the page yet counts as still connecting.
fn stores_state(page: &Page, contexts: &[String]) -> Result<StoresState> {
    let mut all_ready = true;
    for entry in &page.access {
        match &entry.access {
            StoreAccessState::Ready => {}
            StoreAccessState::Loading { .. } | StoreAccessState::Reconnecting => all_ready = false,
            StoreAccessState::Error { message, .. }
            | StoreAccessState::Unsupported { message, .. }
            | StoreAccessState::Forbidden { message, .. }
            | StoreAccessState::NotFound { message, .. } => {
                bail!(
                    "{} watcher on '{}' reported a failure: {message}",
                    entry.kind,
                    entry.cluster
                );
            }
            StoreAccessState::AuthExpired { hint, .. } => {
                bail!(
                    "{} watcher on '{}' reported auth expired: {hint}",
                    entry.kind,
                    entry.cluster
                );
            }
        }
    }
    if contexts
        .iter()
        .any(|c| !page.access.iter().any(|a| &a.cluster == c))
    {
        all_ready = false;
    }
    Ok(if all_ready {
        StoresState::AllReady
    } else {
        StoresState::Loading
    })
}

/// `true` when `page` is the first page the UI can show: `total > 0`, a full
/// window (`rows >= min(limit, total)`), and every store `Ready` or `Loading`
/// (the caller has already rejected failed stores via [`stores_state`]).
/// With progressive publish this page may come from partial stores; the UI
/// labels it as loading.
fn page_is_first_page(page: &Page) -> bool {
    let want = page.limit.min(page.total) as usize;
    page.total > 0
        && page.rows.len() >= want
        && page.access.iter().all(|a| {
            matches!(
                a.access,
                StoreAccessState::Ready | StoreAccessState::Loading { .. }
            )
        })
}

async fn cold_sync(kubeconfigs: &[PathBuf], contexts: &[String]) -> Result<SyncRun> {
    let manager = ClusterManager::new(KubeconfigSource::from_paths(kubeconfigs.to_vec()));
    let initial_spec = pod_query(contexts);
    let mut peak_rss_mb = read_process_rss_mb();
    let mut first_rows_ms: Option<f64> = None;
    let mut first_page_ms: Option<f64> = None;
    let mut loading_query_ms = Vec::new();

    let start = Instant::now();
    manager.ensure_watching_pairs(contexts, &["Pod"]).await;

    loop {
        peak_rss_mb = peak_rss_mb.max(read_process_rss_mb());
        // The UI's initial query, as it re-runs it while rows stream in.
        let t0 = Instant::now();
        let page = manager.query(&initial_spec).await;
        let query_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let now_ms = start.elapsed().as_secs_f64() * 1000.0;
        let state = stores_state(&page, contexts)?;

        if first_rows_ms.is_none() && !page.rows.is_empty() {
            first_rows_ms = Some(now_ms);
        }
        if first_page_ms.is_none() && page_is_first_page(&page) {
            first_page_ms = Some(now_ms);
        }
        std::hint::black_box(&page);
        drop(page);

        match state {
            StoresState::Loading => loading_query_ms.push(query_ms),
            StoresState::AllReady => {
                peak_rss_mb = peak_rss_mb.max(read_process_rss_mb());
                let Some(first_page_ms) = first_page_ms else {
                    bail!("every store over {contexts:?} is Ready but the first page has 0 pods");
                };
                return Ok(SyncRun {
                    manager,
                    first_rows_ms: first_rows_ms.unwrap_or(first_page_ms),
                    first_page_ms,
                    full_sync_ms: now_ms,
                    loading_query_ms,
                    peak_rss_mb,
                });
            }
        }
        if start.elapsed() > SYNC_TIMEOUT {
            bail!(
                "timed out after {SYNC_TIMEOUT:?} before every store over {contexts:?} was Ready"
            );
        }
        tokio::time::sleep(LOADING_POLL).await;
    }
}

async fn run_cluster_benchmark(
    config: &BenchConfig,
    kubeconfigs: &[PathBuf],
) -> Result<BenchReport> {
    let contexts = &config.contexts;
    let known = KubeconfigSource::from_paths(kubeconfigs.to_vec())
        .list_contexts()
        .context("failed to load the kubeconfig files")?;
    for context in contexts {
        if !known.contexts.iter().any(|c| &c.name == context) {
            bail!(
                "context '{context}' is not in {}",
                kubeconfigs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    // Run 1 measures everything (memory on a fresh process); later runs only
    // repeat the cold first page with a fresh ClusterManager.
    let baseline_rss_mb = read_process_rss_mb();
    let run1 = cold_sync(kubeconfigs, contexts).await?;
    let mut first_page_runs_ms = vec![run1.first_page_ms];
    let mut first_rows_runs_ms = vec![run1.first_rows_ms];
    let mut full_sync_runs_ms = vec![run1.full_sync_ms];
    let mut loading_query_ms = run1.loading_query_ms;
    let peak_sync_rss_mb = run1.peak_rss_mb;

    tokio::time::sleep(Duration::from_secs(config.idle_secs)).await;
    let idle_rss_mb = read_process_rss_mb();

    let manager = run1.manager;
    let mut pods_per_context = BTreeMap::new();
    let mut namespaces = BTreeSet::new();
    for context in contexts {
        if let Some(store) = manager.get_store(context, "Pod") {
            let (rows, _) = store.snapshot_rows();
            pods_per_context.insert(context.clone(), rows.len());
            for r in &rows {
                if let Some(ref ns) = r.namespace {
                    namespaces.insert((context.clone(), ns.clone()));
                }
            }
        }
    }
    let sample_ns = namespaces
        .iter()
        .find(|(_, ns)| ns.starts_with("scale-ns-"))
        .or_else(|| namespaces.iter().next())
        .map(|(_, ns)| ns.clone())
        .unwrap_or_default();
    let total_pods: usize = pods_per_context.values().sum();
    if total_pods == 0 {
        bail!("no pods found on {contexts:?}");
    }

    let samples_ms = run_cluster_queries(
        &manager,
        contexts,
        &sample_ns,
        total_pods,
        config.iterations,
    );
    drop(manager);

    for _ in 1..config.runs {
        // Let the previous manager's watch tasks stop before the next cold run.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let run = cold_sync(kubeconfigs, contexts).await?;
        first_page_runs_ms.push(run.first_page_ms);
        first_rows_runs_ms.push(run.first_rows_ms);
        full_sync_runs_ms.push(run.full_sync_ms);
        loading_query_ms.extend(run.loading_query_ms);
    }
    let first_page_ms = first_page_runs_ms.iter().copied().fold(0.0, f64::max);
    let full_sync_ms = full_sync_runs_ms.iter().copied().fold(0.0, f64::max);
    loading_query_ms.sort_by(|a, b| a.total_cmp(b));
    let loading_query_p95_ms =
        (!loading_query_ms.is_empty()).then(|| percentile_sorted(&loading_query_ms, 0.95));
    let loading_query_max_ms = loading_query_ms.last().copied();

    let query_p50_ms = percentile_sorted(&samples_ms, 0.50);
    let query_p95_ms = percentile_sorted(&samples_ms, 0.95);
    let query_max_ms = samples_ms.last().copied().unwrap_or(0.0);

    let (rss_budget, rss_budget_name) = rss_budget_for_clusters(contexts.len());
    let budget_failures = budget_failures(
        first_page_ms,
        query_p95_ms,
        loading_query_p95_ms,
        idle_rss_mb,
        rss_budget,
    );
    let budgets_passed = budget_failures.is_empty();

    Ok(BenchReport {
        mode: "cluster".to_owned(),
        allocator: ALLOCATOR.to_owned(),
        contexts: contexts.clone(),
        pods_per_context,
        pods: total_pods,
        clusters: contexts.len(),
        namespaces: namespaces.len(),
        iterations: config.iterations,
        project_all_ms: 0.0,
        first_page_runs_ms,
        first_page_ms,
        first_rows_runs_ms,
        full_sync_runs_ms,
        full_sync_ms: Some(full_sync_ms),
        loading_query_samples: Some(loading_query_ms.len()),
        loading_query_p95_ms,
        loading_query_max_ms,
        query_p50_ms,
        query_p95_ms,
        query_max_ms,
        baseline_rss_mb: Some(baseline_rss_mb),
        peak_sync_rss_mb: Some(peak_sync_rss_mb),
        idle_rss_mb: Some(idle_rss_mb),
        idle_secs: Some(config.idle_secs),
        idle_kb_per_pod: Some(kb_per_pod(idle_rss_mb, baseline_rss_mb, total_pods)),
        peak_kb_per_pod: Some(kb_per_pod(peak_sync_rss_mb, baseline_rss_mb, total_pods)),
        rss_mb: idle_rss_mb,
        rss_budget_mb: rss_budget,
        rss_budget_name: rss_budget_name.to_owned(),
        rss_note: RSS_SCOPE_NOTE.to_owned(),
        budgets_passed,
        budget_failures,
    })
}

/// Runs the query mix over all contexts and returns sorted latencies in ms.
fn run_cluster_queries(
    manager: &ClusterManager,
    contexts: &[String],
    sample_ns: &str,
    total_pods: usize,
    iterations: usize,
) -> Vec<f64> {
    let base = pod_query(contexts);
    let deep_offset = u32::try_from(total_pods / 2).unwrap_or(0);
    let scenarios = [
        // 1. Text filter + sort by name asc
        QuerySpec {
            text_filter: Some("app".to_owned()),
            ..base.clone()
        },
        // 2. Namespace filter + sort by restarts (cell 2) desc
        QuerySpec {
            namespaces: if sample_ns.is_empty() {
                Vec::new()
            } else {
                vec![sample_ns.to_owned()]
            },
            sort: Some(SortSpec {
                field: SortField::Cell(2),
                direction: SortDirection::Desc,
            }),
            ..base.clone()
        },
        // 3. Deep scroll
        QuerySpec {
            offset: deep_offset,
            ..base.clone()
        },
        // 4. Label selector filter
        QuerySpec {
            label_selector: Some("fleetglass.dev/scale=true".to_owned()),
            ..base
        },
    ];

    let mut samples_ms = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let spec = &scenarios[i % scenarios.len()];
        let t0 = Instant::now();
        let page = manager.query_blocking(spec);
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(&page);
        samples_ms.push(elapsed_ms);
    }
    samples_ms.sort_by(|a, b| a.total_cmp(b));
    samples_ms
}

fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let clamped = p.clamp(0.0, 1.0);
    let idx = ((sorted.len() - 1) as f64 * clamped).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn read_process_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        const PAGE_SIZE_BYTES: f64 = 4096.0;
        const BYTES_PER_MB: f64 = 1_048_576.0;
        let Ok(statm) = fs::read_to_string("/proc/self/statm") else {
            return 0.0;
        };
        let Some(resident_pages_str) = statm.split_whitespace().nth(1) else {
            return 0.0;
        };
        let Ok(resident_pages) = resident_pages_str.parse::<u64>() else {
            return 0.0;
        };
        let rss_bytes = (resident_pages as f64) * PAGE_SIZE_BYTES;
        rss_bytes / BYTES_PER_MB
    }
    #[cfg(not(target_os = "linux"))]
    {
        0.0
    }
}

fn pass_fail(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

fn format_markdown(r: &BenchReport) -> String {
    let status = pass_fail(r.budgets_passed);
    let mut out = String::from("| Metric | Measured | Budget | Status |\n|---|---:|---:|:---:|\n");
    out.push_str(&format!(
        "| Allocator (`allocator`) | {} | - | - |\n",
        r.allocator
    ));
    if r.mode == "cluster" {
        out.push_str(&format!(
            "| Benchmark mode | Real cluster(s): {} | - | - |\n",
            r.contexts.join(", ")
        ));
        let per: Vec<String> = r
            .pods_per_context
            .iter()
            .map(|(c, n)| format!("{c}: {n}"))
            .collect();
        out.push_str(&format!(
            "| Pods per context | {} | - | - |\n",
            per.join(", ")
        ));
    } else {
        out.push_str("| Benchmark mode | Synthetic (in-memory) | - | - |\n");
        out.push_str(&format!(
            "| Project all rows (`project_all_ms`) | {:.2} ms | - | - |\n",
            r.project_all_ms
        ));
    }
    let join_runs = |runs: &[f64]| {
        runs.iter()
            .map(|v| format!("{v:.0}"))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    out.push_str(&format!(
        "| Pods / Clusters / Namespaces | {} / {} / {} | - | - |\n",
        r.pods, r.clusters, r.namespaces,
    ));
    let cluster = r.mode == "cluster";
    let first_page_label = if cluster {
        "First full labelled page"
    } else {
        "First page"
    };
    if !r.first_rows_runs_ms.is_empty() {
        out.push_str(&format!(
            "| First rows (>= 1 row), each run (ms) | {} | - | - |\n",
            join_runs(&r.first_rows_runs_ms)
        ));
    }
    out.push_str(&format!(
        "| {first_page_label}, each run (ms) | {} | - | - |\n\
         | {first_page_label}, worst run (`first_page_ms`) | {:.2} ms | < {FIRST_PAGE_BUDGET_MS:.1} ms | {} |\n",
        join_runs(&r.first_page_runs_ms),
        r.first_page_ms,
        pass_fail(r.first_page_ms < FIRST_PAGE_BUDGET_MS),
    ));
    if let Some(full) = r.full_sync_ms {
        out.push_str(&format!(
            "| Full sync (all stores Ready), each run (ms) | {} | - | - |\n\
             | Full sync, worst run (`full_sync_ms`) | {full:.2} ms | no budget | - |\n",
            join_runs(&r.full_sync_runs_ms)
        ));
    }
    if let (Some(n), Some(p95), Some(max)) = (
        r.loading_query_samples,
        r.loading_query_p95_ms,
        r.loading_query_max_ms,
    ) {
        out.push_str(&format!(
            "| Query latency while loading p95 (`loading_query_p95_ms`, {n} polls) | {p95:.2} ms | < {QUERY_P95_BUDGET_MS:.1} ms | {} |\n\
             | Query latency while loading max (`loading_query_max_ms`) | {max:.2} ms | - | - |\n",
            pass_fail(p95 < QUERY_P95_BUDGET_MS),
        ));
    }
    out.push_str(&format!(
        "| Query latency p50 (`query_p50_ms`) | {:.2} ms | - | - |\n\
         | Query latency p95 (`query_p95_ms`) | {:.2} ms | < {QUERY_P95_BUDGET_MS:.1} ms | {} |\n\
         | Query latency max (`query_max_ms`) | {:.2} ms | - | - |\n",
        r.query_p50_ms,
        r.query_p95_ms,
        pass_fail(r.query_p95_ms < QUERY_P95_BUDGET_MS),
        r.query_max_ms,
    ));
    if let (Some(base), Some(peak), Some(idle)) =
        (r.baseline_rss_mb, r.peak_sync_rss_mb, r.idle_rss_mb)
    {
        out.push_str(&format!(
            "| RSS before watching | {base:.1} MB | - | - |\n\
             | Peak RSS during initial sync | {peak:.1} MB ({:.1} KB/pod) | - | - |\n\
             | Idle RSS {}s after sync (`rss_mb`) | {idle:.1} MB ({:.1} KB/pod) | < {:.1} MB ({}) | {} |\n",
            r.peak_kb_per_pod.unwrap_or(0.0),
            r.idle_secs.unwrap_or(0),
            r.idle_kb_per_pod.unwrap_or(0.0),
            r.rss_budget_mb,
            r.rss_budget_name,
            pass_fail(r.rss_mb < r.rss_budget_mb),
        ));
    } else {
        out.push_str(&format!(
            "| Process RSS (`rss_mb`) | {:.2} MB | < {:.1} MB ({}) | {} |\n",
            r.rss_mb,
            r.rss_budget_mb,
            r.rss_budget_name,
            pass_fail(r.rss_mb < r.rss_budget_mb),
        ));
    }
    out.push_str(&format!(
        "| Overall (`budgets_passed`) | {} | all pass | {status} |\n\nNote: {}.\n",
        r.budgets_passed, r.rss_note
    ));
    if cluster {
        out.push_str(&format!(
            "\nFirst page = first full labelled page: the UI's initial query (name asc, \
             limit {UI_PAGE_SIZE}) polled every {} ms returned min(limit, total) > 0 rows with \
             every store Ready or Loading. With progressive publish the rows may come from a \
             store still in its initial list; the UI labels them as partial. Full sync (every \
             store Ready) has no budget.\n",
            LOADING_POLL.as_millis()
        ));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fleetglass_core::api::StoreAccess;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fg-bench-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_cli_flags_and_defaults() {
        let def = parse_args(args(&[])).unwrap();
        assert_eq!(def, BenchConfig::default());
        assert_eq!(def.pods, 10_000);
        assert_eq!(def.clusters, 3);
        assert_eq!(def.runs, 3);
        assert_eq!(def.idle_secs, 5);
        assert!(def.contexts.is_empty());
        assert!(def.kubeconfigs.is_empty());

        let custom = parse_args(args(&[
            "--pods",
            "50000",
            "--clusters",
            "5",
            "--namespaces",
            "30",
            "--iterations",
            "25",
            "--runs",
            "4",
            "--idle-secs",
            "0",
            "--assert-budgets",
            "--json-out",
            "out.json",
            "--md-out",
            "out.md",
            "--cluster",
            "my-cluster",
            "--kubeconfig",
            "/tmp/kcfg",
        ]))
        .unwrap();
        assert_eq!(custom.pods, 50_000);
        assert_eq!(custom.clusters, 5);
        assert_eq!(custom.namespaces, 30);
        assert_eq!(custom.iterations, 25);
        assert_eq!(custom.runs, 4);
        assert_eq!(custom.idle_secs, 0);
        assert!(custom.assert_budgets);
        assert_eq!(custom.json_out.as_deref(), Some(Path::new("out.json")));
        assert_eq!(custom.md_out.as_deref(), Some(Path::new("out.md")));
        assert_eq!(custom.contexts, vec!["my-cluster".to_owned()]);
        assert_eq!(custom.kubeconfigs, vec![PathBuf::from("/tmp/kcfg")]);

        assert!(parse_args(args(&["--pods"])).is_err());
        assert!(parse_args(args(&["--pods", "0"])).is_err());
        assert!(parse_args(args(&["--runs", "0"])).is_err());
        assert!(parse_args(args(&["--nope"])).is_err());
    }

    #[test]
    fn cluster_flag_defaults_and_accepts_several_contexts() {
        let bare = parse_args(args(&["--cluster"])).unwrap();
        assert_eq!(bare.contexts, vec![DEFAULT_CONTEXT.to_owned()]);

        let before_flag = parse_args(args(&["--cluster", "--iterations", "10"])).unwrap();
        assert_eq!(before_flag.contexts, vec![DEFAULT_CONTEXT.to_owned()]);
        assert_eq!(before_flag.iterations, 10);

        let many = parse_args(args(&[
            "--cluster",
            "a, b",
            "--cluster",
            "c",
            "--cluster",
            "a",
            "--kubeconfig",
            "one.yaml",
            "--kubeconfig",
            "two.yaml",
        ]))
        .unwrap();
        assert_eq!(many.contexts, vec!["a", "b", "c"]);
        assert_eq!(
            many.kubeconfigs,
            vec![PathBuf::from("one.yaml"), PathBuf::from("two.yaml")]
        );

        assert!(parse_args(args(&["--cluster", "a,,b"])).is_err());
        assert!(parse_args(args(&["--kubeconfig", ""])).is_err());
    }

    #[test]
    fn kubeconfig_resolution_refuses_home_kube_and_checks_every_env_entry() {
        let dir = temp_dir("kcfg");
        let home = dir.join("home");
        fs::create_dir_all(home.join(".kube")).unwrap();
        let real = home.join(".kube").join("config");
        fs::write(&real, "apiVersion: v1\nkind: Config\n").unwrap();
        let ok_a = dir.join("a.yaml");
        let ok_b = dir.join("b.yaml");
        fs::write(&ok_a, "apiVersion: v1\nkind: Config\n").unwrap();
        fs::write(&ok_b, "apiVersion: v1\nkind: Config\n").unwrap();

        // Explicit paths win over $KUBECONFIG.
        let got = resolve_kubeconfigs(
            std::slice::from_ref(&ok_a),
            Some(real.as_os_str()),
            Some(&home),
        )
        .unwrap();
        assert_eq!(got, vec![ok_a.clone()]);

        // An explicit ~/.kube path is refused.
        assert!(resolve_kubeconfigs(std::slice::from_ref(&real), None, Some(&home)).is_err());

        // A symlink outside ~/.kube that points into it is refused.
        #[cfg(unix)]
        {
            let link = dir.join("sneaky.yaml");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(resolve_kubeconfigs(std::slice::from_ref(&link), None, Some(&home)).is_err());
        }

        // ':' lists in $KUBECONFIG: all safe entries are accepted, in order...
        let list = std::env::join_paths([&ok_a, &ok_b]).unwrap();
        let got = resolve_kubeconfigs(&[], Some(&list), Some(&home)).unwrap();
        assert_eq!(got, vec![ok_a.clone(), ok_b.clone()]);
        // ...and one entry under ~/.kube (or a missing file) rejects the list.
        let bad_list = std::env::join_paths([&ok_a, &real]).unwrap();
        assert!(resolve_kubeconfigs(&[], Some(&bad_list), Some(&home)).is_err());
        let missing_list = std::env::join_paths([ok_a.clone(), dir.join("missing.yaml")]).unwrap();
        assert!(resolve_kubeconfigs(&[], Some(&missing_list), Some(&home)).is_err());

        // No explicit path and no $KUBECONFIG: the .dev/kubeconfig default.
        match resolve_kubeconfigs(&[], None, Some(&home)) {
            Ok(paths) => assert_eq!(paths, vec![PathBuf::from(DEFAULT_KUBECONFIG)]),
            Err(err) => assert!(err.to_string().contains(DEFAULT_KUBECONFIG), "{err}"),
        }

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cluster_budgets_use_plan_values() {
        // One context: idle 1-cluster budget; several: the 3-cluster/50k budget.
        assert!((rss_budget_for_clusters(1).0 - 200.0).abs() < f64::EPSILON);
        assert!((rss_budget_for_clusters(3).0 - 500.0).abs() < f64::EPSILON);
        assert!((rss_budget_for_clusters(5).0 - 500.0).abs() < f64::EPSILON);
        // The first-page budget is PLAN's 1.5 s in every mode.
        assert!((FIRST_PAGE_BUDGET_MS - 1500.0).abs() < f64::EPSILON);

        assert!(budget_failures(1499.0, 49.0, Some(49.0), 199.0, 200.0).is_empty());
        assert!(budget_failures(1499.0, 49.0, None, 199.0, 200.0).is_empty());
        let fails = budget_failures(1500.0, 50.0, Some(50.0), 200.0, 200.0);
        assert_eq!(fails.len(), 4, "{fails:?}");
        assert!(fails[0].starts_with("first_page_ms"));
        assert!(fails[1].starts_with("query_p95_ms"));
        assert!(fails[2].starts_with("loading_query_p95_ms"));
        assert!(fails[3].starts_with("rss_mb"));
        let only_rss = budget_failures(10.0, 1.0, None, 600.0, 500.0);
        assert_eq!(only_rss.len(), 1);
        let only_loading = budget_failures(10.0, 1.0, Some(80.0), 10.0, 500.0);
        assert_eq!(only_loading.len(), 1);
        assert!(only_loading[0].starts_with("loading_query_p95_ms"));
    }

    #[test]
    fn kb_per_pod_uses_growth_over_baseline() {
        assert!((kb_per_pod(110.0, 10.0, 1024) - 100.0).abs() < 1e-9);
        assert!(kb_per_pod(5.0, 10.0, 100).abs() < f64::EPSILON);
        assert!(kb_per_pod(50.0, 10.0, 0).abs() < f64::EPSILON);
    }

    #[test]
    fn cluster_markdown_reports_runs_peak_and_idle() {
        let report = BenchReport {
            mode: "cluster".to_owned(),
            allocator: "mimalloc".to_owned(),
            contexts: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            pods_per_context: [("a".to_owned(), 10), ("b".to_owned(), 10)]
                .into_iter()
                .collect(),
            pods: 20,
            clusters: 3,
            namespaces: 2,
            iterations: 4,
            project_all_ms: 0.0,
            first_page_runs_ms: vec![900.0, 1600.0, 1000.0],
            first_page_ms: 1600.0,
            first_rows_runs_ms: vec![400.0, 500.0, 450.0],
            full_sync_runs_ms: vec![5000.0, 6000.0, 5500.0],
            full_sync_ms: Some(6000.0),
            loading_query_samples: Some(120),
            loading_query_p95_ms: Some(12.5),
            loading_query_max_ms: Some(70.0),
            query_p50_ms: 1.0,
            query_p95_ms: 2.0,
            query_max_ms: 3.0,
            baseline_rss_mb: Some(10.0),
            peak_sync_rss_mb: Some(400.0),
            idle_rss_mb: Some(300.0),
            idle_secs: Some(5),
            idle_kb_per_pod: Some(1.0),
            peak_kb_per_pod: Some(2.0),
            rss_mb: 300.0,
            rss_budget_mb: 500.0,
            rss_budget_name: "Memory, 3 clusters with 50k pods total".to_owned(),
            rss_note: RSS_SCOPE_NOTE.to_owned(),
            budgets_passed: false,
            budget_failures: vec!["first_page_ms".to_owned()],
        };
        let md = format_markdown(&report);
        assert!(
            md.contains("| Allocator (`allocator`) | mimalloc |"),
            "{md}"
        );
        assert!(md.contains("900 / 1600 / 1000"), "{md}");
        assert!(
            md.contains(
                "| First full labelled page, worst run (`first_page_ms`) | 1600.00 ms | < 1500.0 ms | FAIL |"
            ),
            "{md}"
        );
        assert!(md.contains("400 / 500 / 450"), "{md}");
        assert!(
            md.contains("| Full sync, worst run (`full_sync_ms`) | 6000.00 ms | no budget | - |"),
            "{md}"
        );
        assert!(
            md.contains(
                "| Query latency while loading p95 (`loading_query_p95_ms`, 120 polls) | 12.50 ms | < 50.0 ms | PASS |"
            ),
            "{md}"
        );
        assert!(
            md.contains("| Query latency while loading max (`loading_query_max_ms`) | 70.00 ms |"),
            "{md}"
        );
        assert!(md.contains("First page = first full labelled page"), "{md}");
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["fullSyncMs"], 6000.0);
        assert_eq!(json["loadingQueryP95Ms"], 12.5);
        assert_eq!(json["loadingQueryMaxMs"], 70.0);
        assert_eq!(json["firstRowsRunsMs"][1], 500.0);
        assert!(
            md.contains("Peak RSS during initial sync | 400.0 MB"),
            "{md}"
        );
        assert!(
            md.contains("Idle RSS 5s after sync (`rss_mb`) | 300.0 MB"),
            "{md}"
        );
        assert!(
            md.contains("< 500.0 MB (Memory, 3 clusters with 50k pods total) | PASS"),
            "{md}"
        );
        assert!(md.contains("Tauri/WebKit"), "{md}");
    }

    fn page(total: u32, rows: usize, states: &[(&str, StoreAccessState)]) -> Page {
        Page {
            total,
            offset: 0,
            limit: UI_PAGE_SIZE,
            rows: (0..rows)
                .map(|i| project_pod(&build_synthetic_pod(i, "ns"), "a"))
                .collect(),
            version: 1,
            access: states
                .iter()
                .map(|(cluster, access)| StoreAccess {
                    cluster: (*cluster).to_owned(),
                    kind: "Pod".to_owned(),
                    access: access.clone(),
                    rows: 0,
                    metrics: None,
                    paused: false,
                    scope: None,
                })
                .collect(),
            status_counts: fleetglass_core::api::StatusCounts::default(),
        }
    }

    #[test]
    fn initial_query_matches_the_ui_default_spec() {
        let spec = pod_query(&["a".to_owned()]);
        assert_eq!(spec.limit, 100);
        assert_eq!(spec.offset, 0);
        assert_eq!(spec.kinds, vec!["Pod".to_owned()]);
        assert_eq!(
            spec.sort,
            Some(SortSpec {
                field: SortField::Name,
                direction: SortDirection::Asc,
            })
        );
    }

    #[test]
    fn first_page_needs_a_full_window_from_ready_or_loading_stores() {
        let loading = |n| StoreAccessState::Loading { rows_so_far: n };
        let ctx = ["a".to_owned(), "b".to_owned()];

        // Partial stores still loading, but a full window: counts as the first page.
        let p = page(
            500,
            100,
            &[("a", loading(500)), ("b", StoreAccessState::CONNECTING)],
        );
        assert!(page_is_first_page(&p));
        assert_eq!(stores_state(&p, &ctx).unwrap(), StoresState::Loading);

        // Fewer rows than the window while total exceeds it: not yet.
        assert!(!page_is_first_page(&page(
            500,
            40,
            &[("a", loading(500)), ("b", loading(0))]
        )));
        // A small total: min(limit, total) rows are enough.
        assert!(page_is_first_page(&page(
            30,
            30,
            &[("a", loading(30)), ("b", loading(0))]
        )));
        // Nothing yet.
        let empty = page(0, 0, &[("a", loading(0)), ("b", loading(0))]);
        assert!(!page_is_first_page(&empty));
        assert_eq!(stores_state(&empty, &ctx).unwrap(), StoresState::Loading);

        let ready = page(
            500,
            100,
            &[
                ("a", StoreAccessState::Ready),
                ("b", StoreAccessState::Ready),
            ],
        );
        assert!(page_is_first_page(&ready));
        assert_eq!(stores_state(&ready, &ctx).unwrap(), StoresState::AllReady);

        // A failed store is never a first page and aborts the run.
        let forbidden = page(
            500,
            100,
            &[
                ("a", StoreAccessState::Ready),
                (
                    "b",
                    StoreAccessState::Error {
                        message: "boom".to_owned(),
                        partial_rows: Some(100),
                    },
                ),
            ],
        );
        assert!(!page_is_first_page(&forbidden));
        let err = stores_state(&forbidden, &ctx).unwrap_err().to_string();
        assert!(err.contains("'b'") && err.contains("boom"), "{err}");

        // A requested context without a store yet is still connecting.
        let missing = page(500, 100, &[("a", StoreAccessState::Ready)]);
        assert_eq!(stores_state(&missing, &ctx).unwrap(), StoresState::Loading);
    }

    #[test]
    fn computes_percentiles_correctly() {
        assert!(percentile_sorted(&[], 0.5).abs() < f64::EPSILON);
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert!((percentile_sorted(&data, 0.0) - 1.0).abs() < f64::EPSILON);
        assert!((percentile_sorted(&data, 0.5) - 6.0).abs() < f64::EPSILON);
        assert!((percentile_sorted(&data, 0.95) - 10.0).abs() < f64::EPSILON);
        assert!((percentile_sorted(&data, 1.0) - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn runs_small_benchmark_and_passes_budgets() {
        let cfg = BenchConfig {
            pods: 500,
            clusters: 3,
            namespaces: 10,
            iterations: 9,
            assert_budgets: true,
            ..BenchConfig::default()
        };
        let report = run_benchmark(&cfg).unwrap();
        assert_eq!(report.pods, 500);
        assert_eq!(report.allocator, ALLOCATOR);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["allocator"], ALLOCATOR);
        assert!(
            report.budgets_passed,
            "unexpected budget failures: {:?}",
            report.budget_failures
        );
        assert!(report.first_page_ms < FIRST_PAGE_BUDGET_MS);
        assert!(report.query_p95_ms < QUERY_P95_BUDGET_MS);
        assert!(report.rss_mb < RSS_50K_BUDGET_MB);
    }
}
