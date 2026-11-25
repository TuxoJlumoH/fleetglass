//! 02-01a live test: the per-cluster metrics poller fills the CPU / Memory cells
//! of pod and node rows from metrics-server on the `fleetglass-dev` kind cluster,
//! and reports `NotInstalled` (empty cells, never zeros) while metrics-server is
//! scaled to 0. metrics-server is scaled back to its previous replica count in
//! the cleanup, also after a failed assertion.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_metrics -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`), whatever `KUBECONFIG` says.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetglass_core::api::{Cell, MetricsAvailability, Page, PageDelta, QuerySpec};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::metrics::{NODE_CPU_CELL, NODE_MEMORY_CELL, POD_CPU_CELL, POD_MEMORY_CELL};
use futures::FutureExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::Api;
use kube::api::{Patch, PatchParams};
use tokio::sync::mpsc::Receiver;

/// Budget of the task: usage visible within 60 s of metrics-server running.
const WAIT: Duration = Duration::from_secs(60);
/// Waiting for a restored metrics-server to serve again (pod start + first scrape).
const RESTORE_WAIT: Duration = Duration::from_secs(180);

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

fn spec(cluster: &str, kind: &str, namespaces: Vec<String>) -> QuerySpec {
    QuerySpec {
        clusters: vec![cluster.to_owned()],
        kinds: vec![kind.to_owned()],
        namespaces,
        label_selector: None,
        text_filter: None,
        sort: None,
        offset: 0,
        limit: 100,
        statuses: Vec::new(),
    }
}

/// Polls `manager.query(spec)` until `done` holds or `wait` passes.
async fn poll_query(
    manager: &ClusterManager,
    spec: &QuerySpec,
    wait: Duration,
    what: &str,
    mut done: impl FnMut(&Page) -> bool,
) -> (Page, Duration) {
    let start = Instant::now();
    loop {
        let page = manager.query(spec).await;
        if done(&page) {
            return (page, start.elapsed());
        }
        assert!(
            start.elapsed() < wait,
            "timed out after {wait:?} waiting for {what}; access={:?}",
            page.access
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn metrics_state(page: &Page) -> Option<&MetricsAvailability> {
    page.access.first().and_then(|a| a.metrics.as_ref())
}

fn usage_value(cell: &Cell) -> Option<u32> {
    match cell {
        Cell::Usage(u) => Some(u.value),
        _ => None,
    }
}

/// Rows of `page` whose `(cpu, memory)` cells hold a measurement.
fn measured_rows(page: &Page, cols: (usize, usize)) -> usize {
    page.rows
        .iter()
        .filter(|r| {
            usage_value(&r.cells[cols.0]).is_some() && usage_value(&r.cells[cols.1]).is_some()
        })
        .count()
}

fn all_empty(page: &Page, cols: (usize, usize)) -> bool {
    page.rows.iter().all(|r| {
        r.cells[cols.0] == Cell::Text(String::new()) && r.cells[cols.1] == Cell::Text(String::new())
    })
}

async fn scale_metrics_server(client: &kube::Client, replicas: i32) {
    Api::<Deployment>::namespaced(client.clone(), "kube-system")
        .patch(
            "metrics-server",
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({ "spec": { "replicas": replicas } })),
        )
        .await
        .unwrap();
    println!("scaled kube-system/metrics-server to {replicas}");
}

/// Keeps the subscriptions alive (the poller only runs while subscribed) and
/// drains their deltas so the channels never fill up.
fn drain(mut rx: Receiver<PageDelta>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { while rx.recv().await.is_some() {} })
}

async fn run(manager: Arc<ClusterManager>, context: String, client: kube::Client) {
    let pods = spec(&context, "Pod", vec!["kube-system".to_owned()]);
    let nodes = spec(&context, "Node", Vec::new());
    manager.ensure_watching_spec(&pods).await;
    manager.ensure_watching_spec(&nodes).await;
    let pod_store = manager.get_or_create_store(&context, "Pod");
    let node_store = manager.get_or_create_store(&context, "Node");

    // Without a subscriber nothing polls metrics.
    assert!(!pod_store.metrics_poller_active());
    let page = manager.query(&pods).await;
    assert_eq!(metrics_state(&page), Some(&MetricsAvailability::Pending));

    let (_pod_sub, pod_rx) = manager.subscribe(pods.clone());
    let (_node_sub, node_rx) = manager.subscribe(nodes.clone());
    let drains = [drain(pod_rx), drain(node_rx)];
    assert!(pod_store.metrics_poller_active());
    assert!(node_store.metrics_poller_active());

    // 1. metrics-server running: non-empty usage within 60 s.
    let pod_cols = (POD_CPU_CELL, POD_MEMORY_CELL);
    let (page, took) = poll_query(&manager, &pods, WAIT, "pod usage", |p| {
        metrics_state(p) == Some(&MetricsAvailability::Available) && measured_rows(p, pod_cols) > 0
    })
    .await;
    println!(
        "pod usage after {took:?}: {}/{} kube-system pods measured",
        measured_rows(&page, pod_cols),
        page.rows.len()
    );
    for row in page.rows.iter().take(5) {
        println!(
            "  {} cpu={:?} mem={:?}",
            row.name, row.cells[POD_CPU_CELL], row.cells[POD_MEMORY_CELL]
        );
    }
    let api = page
        .rows
        .iter()
        .find(|r| r.name.starts_with("kube-apiserver"))
        .expect("kube-apiserver pod");
    assert!(usage_value(&api.cells[POD_MEMORY_CELL]).is_some_and(|mib| mib > 0));
    match &api.cells[POD_CPU_CELL] {
        // kind's kube-apiserver requests 250m CPU and sets no limit.
        Cell::Usage(u) => {
            assert_eq!(u.request, Some(250));
            assert_eq!(u.limit, None);
            assert!(u.formatted.ends_with('m'), "{}", u.formatted);
        }
        other => panic!("expected usage, got {other:?}"),
    }

    let node_cols = (NODE_CPU_CELL, NODE_MEMORY_CELL);
    let (page, took) = poll_query(&manager, &nodes, WAIT, "node usage", |p| {
        p.total > 0 && measured_rows(p, node_cols) == p.rows.len()
    })
    .await;
    println!("node usage after {took:?}");
    for row in &page.rows {
        println!(
            "  {} cpu={:?} mem={:?}",
            row.name, row.cells[NODE_CPU_CELL], row.cells[NODE_MEMORY_CELL]
        );
        match &row.cells[NODE_CPU_CELL] {
            Cell::Usage(u) => {
                assert_eq!(u.request, None);
                assert!(
                    u.limit.is_some_and(|alloc| alloc > 0),
                    "allocatable as limit"
                );
            }
            other => panic!("expected usage, got {other:?}"),
        }
    }

    // 2. metrics-server scaled to 0: NotInstalled and empty cells, not zeros.
    scale_metrics_server(&client, 0).await;
    let (page, took) = poll_query(
        &manager,
        &pods,
        WAIT + Duration::from_secs(30),
        "NotInstalled",
        |p| {
            matches!(
                metrics_state(p),
                Some(MetricsAvailability::NotInstalled { .. })
            )
        },
    )
    .await;
    println!(
        "pods after scale-down ({took:?}): {:?}",
        metrics_state(&page)
    );
    assert!(
        all_empty(&page, pod_cols),
        "no usage numbers while not installed"
    );
    assert!(page.total > 0, "the pod list itself is unaffected");
    let (page, _) = poll_query(&manager, &nodes, WAIT, "node NotInstalled", |p| {
        matches!(
            metrics_state(p),
            Some(MetricsAvailability::NotInstalled { .. })
        )
    })
    .await;
    assert!(all_empty(&page, node_cols));

    // 3. Dropping the subscriptions (receivers) stops both pollers.
    for d in drains {
        d.abort();
    }
    let start = Instant::now();
    while (pod_store.metrics_poller_active() || node_store.metrics_poller_active())
        && start.elapsed() < Duration::from_secs(15)
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !pod_store.metrics_poller_active(),
        "poller stops without subscribers"
    );
    assert!(!node_store.metrics_poller_active());
    assert_eq!(pod_store.metrics(), Some(MetricsAvailability::Pending));
    println!("pollers stopped after {:?}", start.elapsed());
}

#[tokio::test]
async fn live_metrics_usage_and_not_installed() {
    let kubeconfig = dev_kubeconfig();
    assert!(
        kubeconfig.is_file(),
        "{} missing: run scripts/dev-cluster.sh up",
        kubeconfig.display()
    );
    let source = KubeconfigSource::from_paths(vec![kubeconfig]);
    let context = source
        .load()
        .unwrap()
        .config
        .current_context
        .expect("current-context in .dev/kubeconfig");
    assert_eq!(context, "kind-fleetglass-dev", "only the dev kind cluster");
    let manager = Arc::new(ClusterManager::new(source));
    let client = manager.connect_client(&context).await.unwrap();

    let deployments = Api::<Deployment>::namespaced(client.clone(), "kube-system");
    let original = deployments
        .get("metrics-server")
        .await
        .expect("metrics-server deployment on fleetglass-dev")
        .spec
        .replicas
        .unwrap_or(1)
        .max(1);

    let outcome = AssertUnwindSafe(run(Arc::clone(&manager), context.clone(), client.clone()))
        .catch_unwind()
        .await;

    // Cleanup: restore metrics-server and wait until it serves again, so later
    // tests (and developers) find the cluster as it was.
    scale_metrics_server(&client, original).await;
    let start = Instant::now();
    loop {
        let req = kube::core::Request::new("/apis/metrics.k8s.io/v1beta1/nodes")
            .list(&kube::api::ListParams::default())
            .unwrap();
        match client.request_text(req).await {
            Ok(body) if body.contains("\"usage\"") => {
                println!("metrics-server serving again after {:?}", start.elapsed());
                break;
            }
            _ if start.elapsed() > RESTORE_WAIT => {
                eprintln!("cleanup: metrics-server not serving after {RESTORE_WAIT:?}");
                break;
            }
            _ => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
