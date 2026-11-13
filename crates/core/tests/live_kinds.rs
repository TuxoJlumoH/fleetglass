//! R-02 live test: built-in kinds (Node, Namespace, Service, Deployment, ConfigMap, DaemonSet, Secret, Event)
//! and dynamic CRD resources are watched and queried via ClusterManager against kind-fleetglass-dev.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_kinds -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`), whatever `KUBECONFIG` says.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetglass_core::api::{Page, QuerySpec, StoreAccessState};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use futures::FutureExt;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{Api, DeleteParams, DynamicObject, Patch, PatchParams, PostParams};
use kube::runtime::wait::{await_condition, conditions};

const WAIT: Duration = Duration::from_secs(30);

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}{:x}", std::process::id(), nanos & 0xff_ffff)
}

/// Polls `manager.query(spec)` until `done` holds or [`WAIT`] passes.
async fn poll_query(
    manager: &ClusterManager,
    spec: &QuerySpec,
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
            start.elapsed() < WAIT,
            "timed out after {WAIT:?} waiting for {what}; last page: total={} rows={:?} access={:?}",
            page.total,
            page.rows.iter().map(|r| &r.name).collect::<Vec<_>>(),
            page.access
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run(manager: Arc<ClusterManager>, context: String, suffix: String) {
    // 1. Built-in kinds: Node, Namespace, Service, Deployment, ConfigMap, DaemonSet, Secret, Event
    let builtin_kinds = [
        "Node",
        "Namespace",
        "Service",
        "Deployment",
        "ConfigMap",
        "DaemonSet",
        "Secret",
        "Event",
    ];
    for kind in builtin_kinds {
        let spec = QuerySpec {
            clusters: vec![context.clone()],
            kinds: vec![kind.to_owned()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 100,
            statuses: Vec::new(),
        };
        manager.ensure_watching_spec(&spec).await;
        let (page, elapsed) = poll_query(&manager, &spec, kind, |p| {
            p.access
                .iter()
                .any(|a| matches!(a.access, StoreAccessState::Ready))
                && !p.rows.is_empty()
        })
        .await;

        println!("{kind}: {} rows in {elapsed:?}", page.rows.len());
        assert!(!page.rows.is_empty(), "expected rows.len() > 0 for {kind}");
        let store_access = page
            .access
            .iter()
            .find(|a| a.kind.eq_ignore_ascii_case(kind))
            .expect("store access reported for kind");
        assert_eq!(
            store_access.access,
            StoreAccessState::Ready,
            "expected Ready access for {kind}"
        );
    }

    // 2. CRD creation and DynamicObject watch / query
    let client = manager.connect_client(&context).await.unwrap();
    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
    let group = format!("fg{suffix}.test");
    let crd_name = format!("testitems.{group}");

    let crd_json = serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {
            "name": crd_name,
        },
        "spec": {
            "group": group,
            "names": {
                "kind": "TestItem",
                "plural": "testitems",
                "singular": "testitem",
            },
            "scope": "Namespaced",
            "versions": [{
                "name": "v1",
                "served": true,
                "storage": true,
                "schema": {
                    "openAPIV3Schema": {
                        "type": "object",
                        "properties": {
                            "spec": {
                                "type": "object",
                                "properties": {
                                    "message": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            }]
        }
    });

    println!("applying CRD {crd_name}...");
    let ssapply = PatchParams::apply("fleetglass-test").force();
    crds.patch(&crd_name, &ssapply, &Patch::Apply(&crd_json))
        .await
        .unwrap();

    let established = tokio::time::timeout(
        Duration::from_secs(15),
        await_condition(crds.clone(), &crd_name, conditions::is_crd_established()),
    )
    .await
    .expect("crd established timeout")
    .unwrap();
    assert!(established.is_some());

    // Give API server discovery a moment to register the new CRD
    tokio::time::sleep(Duration::from_secs(1)).await;

    let gvk = kube::api::GroupVersionKind::gvk(&group, "v1", "TestItem");
    let ar = kube::api::ApiResource::from_gvk(&gvk);
    let dyn_api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "default", &ar);

    let item_name = format!("item-{suffix}");
    let test_obj = DynamicObject::new(&item_name, &ar).data(serde_json::json!({
        "spec": { "message": "hello live kinds" }
    }));

    dyn_api
        .create(&PostParams::default(), &test_obj)
        .await
        .unwrap();

    // Query ClusterManager for the custom resource
    let crd_spec = QuerySpec {
        clusters: vec![context.clone()],
        kinds: vec!["TestItem".to_owned()],
        namespaces: Vec::new(),
        label_selector: None,
        text_filter: None,
        sort: None,
        offset: 0,
        limit: 100,
        statuses: Vec::new(),
    };
    manager.ensure_watching_spec(&crd_spec).await;
    let (crd_page, elapsed) = poll_query(&manager, &crd_spec, "TestItem", |p| {
        p.access
            .iter()
            .any(|a| matches!(a.access, StoreAccessState::Ready))
            && p.rows.iter().any(|r| r.name == item_name)
    })
    .await;

    println!("TestItem: {} rows in {elapsed:?}", crd_page.rows.len());
    let item_row = crd_page
        .rows
        .iter()
        .find(|r| r.name == item_name)
        .expect("created CRD object found in query rows");
    assert_eq!(item_row.kind, "TestItem");
    let crd_access = crd_page
        .access
        .iter()
        .find(|a| a.kind == "TestItem")
        .expect("access state for TestItem");
    assert_eq!(crd_access.access, StoreAccessState::Ready);

    // 3. Verify store dedup & query lookup for dynamic kind aliases ("testitems", "testitem")
    for alias in ["testitems", "testitem"] {
        let alias_spec = QuerySpec {
            clusters: vec![context.clone()],
            kinds: vec![alias.to_owned()],
            namespaces: Vec::new(),
            label_selector: None,
            text_filter: None,
            sort: None,
            offset: 0,
            limit: 100,
            statuses: Vec::new(),
        };
        manager.ensure_watching_spec(&alias_spec).await;
        let (alias_page, elapsed) = poll_query(&manager, &alias_spec, alias, |p| {
            p.rows.iter().any(|r| r.name == item_name)
        })
        .await;
        println!(
            "{alias} (alias of TestItem): {} rows in {elapsed:?}",
            alias_page.rows.len()
        );
        assert!(
            alias_page.rows.iter().any(|r| r.name == item_name),
            "expected created CRD object to be found when querying by alias '{alias}'"
        );
    }
}

#[tokio::test]
async fn test_live_kinds_builtin_and_crd() {
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

    let manager = Arc::new(ClusterManager::new(source));
    let suffix = unique_suffix();
    let group = format!("fg{suffix}.test");
    let crd_name = format!("testitems.{group}");

    let outcome = AssertUnwindSafe(run(Arc::clone(&manager), context.clone(), suffix.clone()))
        .catch_unwind()
        .await;

    // Cleanup CRD and instances
    if let Ok(client) = manager.connect_client(&context).await {
        let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
        let _ = crds.delete(&crd_name, &DeleteParams::default()).await;
    }

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
