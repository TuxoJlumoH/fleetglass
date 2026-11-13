//! 02-08a live test: an Overview subscription for `kind-fleetglass-dev` connects it from a
//! fresh manager and reports its nodes and the sample workloads in Attention.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_overview -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`), whatever `KUBECONFIG` says.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetglass_core::api::ConnectionStatus;
use fleetglass_core::cluster::{ClusterManager, OVERVIEW_KINDS};
use fleetglass_core::kubeconfig::KubeconfigSource;

const WAIT: Duration = Duration::from_secs(30);

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

#[tokio::test]
async fn test_live_overview_attention_items() {
    let kubeconfig = dev_kubeconfig();
    assert!(
        kubeconfig.exists(),
        "{} does not exist; run `scripts/dev-cluster.sh up` first",
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

    let other_contexts: Vec<String> = source
        .load()
        .unwrap()
        .config
        .contexts
        .iter()
        .map(|c| c.name.clone())
        .filter(|name| name != &context)
        .collect();

    // Fresh manager: nothing is connected before the subscription.
    let manager = Arc::new(ClusterManager::new(source));
    assert!(manager.connected_clusters().is_empty());
    assert_eq!(manager.client_builds(), 0);

    // The subscription itself connects the requested cluster (lazily, in the background).
    let (_sub_id, mut rx) = manager.subscribe_overview(vec![context.clone()]);

    let start = Instant::now();
    let mut updates = 0u32;
    loop {
        let remaining = WAIT.saturating_sub(start.elapsed());
        let overview = tokio::time::timeout(remaining, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out after {WAIT:?} waiting for the overview"))
            .expect("overview channel closed");
        updates += 1;

        assert_eq!(
            overview
                .clusters
                .iter()
                .map(|c| c.cluster.as_str())
                .collect::<Vec<_>>(),
            vec![context.as_str()],
            "the overview covers exactly the requested cluster"
        );
        let card = &overview.clusters[0];
        let find = |kind: &str, name: &str| {
            overview
                .attention
                .items
                .iter()
                .find(|item| item.kind == kind && item.name == name)
        };
        let crashloop_item = find("Pod", "crashloop");
        let pending_item = find("Pod", "pending");
        let undersized_item = find("Deployment", "undersized");

        if card.connection == ConnectionStatus::Connected
            && card.nodes_total.is_some_and(|n| n >= 1)
            && let (Some(cl), Some(pend), Some(under)) =
                (crashloop_item, pending_item, undersized_item)
        {
            println!(
                "Overview ready after {:?} ({updates} updates): {} nodes {:?}/{:?}, pods running={:?} pending={:?} failing={:?} problems={:?}\n  - {}: {} ({})\n  - {}: {} ({})\n  - {}: {} ({})",
                start.elapsed(),
                card.cluster,
                card.nodes_ready,
                card.nodes_total,
                card.pods_running,
                card.pods_pending,
                card.pods_failing,
                card.problems,
                cl.name,
                cl.reason,
                cl.message,
                pend.name,
                pend.reason,
                pend.message,
                under.name,
                under.reason,
                under.message
            );
            break;
        }
        println!(
            "update {updates} after {:?}: connection={:?} nodes={:?} attention={:?}",
            start.elapsed(),
            card.connection,
            card.nodes_total,
            overview
                .attention
                .items
                .iter()
                .map(|i| (&i.name, &i.kind, &i.reason))
                .collect::<Vec<_>>()
        );
    }

    // Only the requested context was ever connected.
    assert_eq!(manager.connected_clusters(), vec![context.clone()]);
    assert_eq!(
        manager.client_builds(),
        1,
        "one client build, for {context}"
    );
    for other in &other_contexts {
        assert!(
            !manager.connected_clusters().contains(other),
            "{other} must not be connected"
        );
    }

    // `get_overview` with the same list sees the same cluster, still without other connections.
    manager
        .ensure_watching_overview(std::slice::from_ref(&context))
        .await;
    let snapshot = manager.get_overview(std::slice::from_ref(&context));
    assert_eq!(snapshot.clusters.len(), 1);
    assert_eq!(snapshot.clusters[0].connection, ConnectionStatus::Connected);
    assert_eq!(manager.client_builds(), 1);

    // Dropping the receiver ends the subscription and releases its store leases
    // (the background ensure-watch never holds a lease).
    drop(rx);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let counts: Vec<u32> = OVERVIEW_KINDS
            .iter()
            .map(|kind| {
                manager
                    .get_store(&context, kind)
                    .map_or(0, |s| s.ref_count())
            })
            .collect();
        if counts.iter().all(|&c| c == 0) {
            break;
        }
        assert!(Instant::now() < deadline, "leases not released: {counts:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
