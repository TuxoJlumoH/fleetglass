//! R-03 live test: fetch_resource_detail and fetch_resource_yaml against kind-fleetglass-dev.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_resource_detail -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`), whatever `KUBECONFIG` says.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::detail::{fetch_resource_detail, fetch_resource_yaml};
use fleetglass_core::kubeconfig::KubeconfigSource;
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::Api;
use kube::api::ListParams;

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

#[tokio::test]
async fn live_resource_detail_and_yaml() {
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

    // Find a running pod in the cluster (e.g. fg-demo or kube-system)
    let pods: Api<Pod> = Api::all(client.clone());
    let pod_list = pods.list(&ListParams::default()).await.unwrap();
    let pod = pod_list
        .items
        .into_iter()
        .find(|p| p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))
        .expect("at least one running pod in kind cluster");

    let pod_name = pod.metadata.name.clone().expect("pod name");
    let pod_ns = pod.metadata.namespace.clone();

    println!(
        "Testing detail and YAML on pod {}/{}",
        pod_ns.as_deref().unwrap_or(""),
        pod_name
    );

    // Call fetch_resource_detail
    let detail = fetch_resource_detail(&client, &context, "Pod", pod_ns.as_deref(), &pod_name)
        .await
        .unwrap();

    assert_eq!(detail.name, pod_name);
    assert_eq!(detail.kind, "Pod");
    assert_eq!(detail.cluster, context);
    assert_eq!(detail.namespace, pod_ns);
    assert!(!detail.uid.is_empty(), "detail uid should be populated");
    assert!(!detail.yaml.is_empty(), "detail yaml should not be empty");
    assert!(
        !detail.yaml.contains("managedFields"),
        "detail yaml must have managedFields stripped"
    );
    assert!(
        detail.events.len() <= 50,
        "events must be bounded (<= 50), got {}",
        detail.events.len()
    );

    for rel in &detail.related {
        assert_eq!(rel.relationship, "owner");
        assert!(!rel.name.is_empty());
        assert!(!rel.kind.is_empty());
    }

    println!(
        "Detail verified: uid={}, related count={}, events count={}",
        detail.uid,
        detail.related.len(),
        detail.events.len()
    );

    // Call fetch_resource_yaml
    let yaml = fetch_resource_yaml(&client, "Pod", pod_ns.as_deref(), &pod_name)
        .await
        .unwrap();

    assert!(!yaml.is_empty(), "yaml should not be empty");
    assert!(
        !yaml.contains("managedFields"),
        "yaml must have managedFields stripped"
    );

    // If crashloop pod in fg-demo exists, verify it gets its events
    if let Ok(crashloop_detail) =
        fetch_resource_detail(&client, &context, "Pod", Some("fg-demo"), "crashloop").await
    {
        assert_eq!(crashloop_detail.name, "crashloop");
        assert!(
            !crashloop_detail.events.is_empty(),
            "crashloop pod should have events"
        );
        println!(
            "crashloop pod has {} events, first event reason: {}",
            crashloop_detail.events.len(),
            crashloop_detail.events[0].status_reason
        );
    }

    // Also test a cluster-scoped resource (Node) with namespace: None
    let nodes: Api<Node> = Api::all(client.clone());
    let node_list = nodes.list(&ListParams::default()).await.unwrap();
    if let Some(node) = node_list.items.into_iter().next() {
        let node_name = node.metadata.name.unwrap();
        let node_detail = fetch_resource_detail(&client, &context, "Node", None, &node_name)
            .await
            .unwrap();
        assert_eq!(node_detail.name, node_name);
        assert_eq!(node_detail.kind, "Node");
        assert!(node_detail.namespace.is_none());
        assert!(!node_detail.yaml.is_empty());
        assert!(!node_detail.yaml.contains("managedFields"));
        assert!(node_detail.events.len() <= 50);

        let node_yaml = fetch_resource_yaml(&client, "Node", None, &node_name)
            .await
            .unwrap();
        assert!(!node_yaml.is_empty());
        assert!(!node_yaml.contains("managedFields"));
    }
}
