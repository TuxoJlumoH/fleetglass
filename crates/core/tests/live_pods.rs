//! R-01 live test: the pod watcher started by `ClusterManager::ensure_watching`
//! feeds `query` / `subscribe` from the `fleetglass-dev` kind cluster, and RBAC
//! denial surfaces as `StoreAccessState::Forbidden`.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_pods -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`), whatever `KUBECONFIG` says.

#![cfg(feature = "integration")]
// Test-only file: `allow-unwrap-in-tests` does not reach the async helpers below.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetglass_core::api::{Page, QuerySpec, StoreAccessState};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use futures::FutureExt;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Container, Namespace, Pod, PodSpec, PodTemplateSpec, ServiceAccount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::Api;
use kube::api::{DeleteParams, PostParams};

const APP_LABEL: &str = "fg-r01";
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

fn pod_spec(cluster: &str, namespace: &str) -> QuerySpec {
    QuerySpec {
        clusters: vec![cluster.to_owned()],
        kinds: vec!["Pod".to_owned()],
        namespaces: vec![namespace.to_owned()],
        label_selector: Some(format!("app={APP_LABEL}")),
        text_filter: None,
        sort: None,
        offset: 0,
        limit: 100,
        statuses: Vec::new(),
    }
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

fn deployment(namespace: &str) -> Deployment {
    let labels = BTreeMap::from([("app".to_owned(), APP_LABEL.to_owned())]);
    Deployment {
        metadata: ObjectMeta {
            name: Some("pause".to_owned()),
            namespace: Some(namespace.to_owned()),
            ..Default::default()
        },
        spec: DeploymentSpec {
            replicas: Some(3),
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "pause".to_owned(),
                        image: Some("registry.k8s.io/pause:3.10".to_owned()),
                        ..Default::default()
                    }],
                    termination_grace_period_seconds: Some(0),
                    ..Default::default()
                }),
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Writes `.dev/kubeconfig` with the context user impersonating `user` to a temp
/// file (JSON is valid kubeconfig YAML). The admin credentials stay in the copy, so
/// it is removed after use.
fn write_kubeconfig_0600(path: &std::path::Path, bytes: &[u8]) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).unwrap();
    }
}

fn impersonating_kubeconfig(context: &str, user: &str, dir: &std::path::Path) -> PathBuf {
    let mut config = KubeconfigSource::from_paths(vec![dev_kubeconfig()])
        .load()
        .unwrap()
        .config;
    let auth_name = config
        .contexts
        .iter()
        .find(|c| c.name == context)
        .and_then(|c| c.context.as_ref())
        .and_then(|c| c.user.clone())
        .unwrap();
    let auth = config
        .auth_infos
        .iter_mut()
        .find(|a| a.name == auth_name)
        .and_then(|a| a.auth_info.as_mut())
        .unwrap();
    auth.impersonate = Some(user.to_owned());
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("kubeconfig");
    write_kubeconfig_0600(&path, &serde_json::to_vec(&config).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "temporary kubeconfig must be written with 0600 permissions"
        );
    }
    path
}

async fn run(manager: Arc<ClusterManager>, context: String, ns: String, tmp: PathBuf) {
    let client = manager.connect_client(&context).await.unwrap();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(ns.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    Api::<Deployment>::namespaced(client.clone(), &ns)
        .create(&PostParams::default(), &deployment(&ns))
        .await
        .unwrap();
    println!("created namespace {ns} with a 3-replica deployment");

    // 1. ensure_watching (what query_page / subscribe_query call) feeds query.
    let spec = pod_spec(&context, &ns);
    let started = Instant::now();
    manager.ensure_watching_spec(&spec).await;
    manager.ensure_watching(&context, "Pod").await.unwrap();
    let store = manager.get_or_create_store(&context, "Pod");
    assert_eq!(store.watchers_started(), 1, "exactly one watcher per store");

    let (page, _) = poll_query(&manager, &spec, "3 pods", |p| p.total == 3).await;
    let first_seen = started.elapsed();
    println!(
        "query returned 3 pods after {first_seen:?}: {:?}",
        page.rows.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
    assert_eq!(page.access.len(), 1);
    assert_eq!(page.access[0].access, StoreAccessState::Ready);
    assert!(
        page.rows
            .iter()
            .all(|r| r.cluster == context && r.kind == "Pod")
    );
    assert!(
        first_seen <= Duration::from_secs(10),
        "R-01 budget: pods within 10 s, took {first_seen:?}"
    );

    // 2. Deleting a pod updates both one-shot queries and an open subscription.
    let (_sub_id, mut deltas) = manager.subscribe(spec.clone());
    let initial = deltas.recv().await.unwrap();
    assert_eq!(initial.page.total, 3);

    let victim = page.rows[0].clone();
    Api::<Pod>::namespaced(client.clone(), &ns)
        .delete(
            &victim.name,
            &DeleteParams {
                grace_period_seconds: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    println!("deleted pod {} (uid {})", victim.name, victim.uid);

    let (page, took) = poll_query(&manager, &spec, "replacement pod", |p| {
        p.total == 3 && p.rows.iter().all(|r| r.uid != victim.uid)
    })
    .await;
    println!(
        "page updated after {took:?}: {:?}",
        page.rows.iter().map(|r| &r.name).collect::<Vec<_>>()
    );

    let sub_deadline = Instant::now() + WAIT;
    loop {
        let remaining = sub_deadline.saturating_duration_since(Instant::now());
        let delta = tokio::time::timeout(remaining, deltas.recv())
            .await
            .expect("subscription delta without the deleted pod")
            .unwrap();
        if delta.page.total == 3 && delta.page.rows.iter().all(|r| r.uid != victim.uid) {
            println!("subscription pushed the updated page");
            break;
        }
    }
    drop(deltas);

    // 3. A ServiceAccount without list rights gets the forbidden state, not an empty list.
    Api::<ServiceAccount>::namespaced(client.clone(), &ns)
        .create(
            &PostParams::default(),
            &ServiceAccount {
                metadata: ObjectMeta {
                    name: Some("no-list".to_owned()),
                    namespace: Some(ns.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let sa_user = format!("system:serviceaccount:{ns}:no-list");
    let sa_kubeconfig = impersonating_kubeconfig(&context, &sa_user, &tmp);
    let denied = ClusterManager::new(KubeconfigSource::from_paths(vec![sa_kubeconfig]));
    denied.ensure_watching(&context, "Pod").await.unwrap();
    let (page, took) = poll_query(&denied, &spec, "forbidden state", |p| {
        matches!(
            p.access.first().map(|a| &a.access),
            Some(StoreAccessState::Forbidden { .. })
        )
    })
    .await;
    println!(
        "impersonating {sa_user}: {:?} after {took:?}",
        page.access[0].access
    );
    assert_eq!(page.total, 0);
    match &page.access[0].access {
        StoreAccessState::Forbidden {
            resource, message, ..
        } => {
            assert_eq!(resource, "pods");
            assert_eq!(message, "No permission to list pods");
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
    drop(denied);
}

#[tokio::test]
async fn live_pod_watcher_feeds_query_and_reports_forbidden() {
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

    let suffix = unique_suffix();
    let ns = format!("fg-r01-{suffix}");
    let tmp = std::env::temp_dir().join(format!("fg-r01-{suffix}"));

    let outcome = AssertUnwindSafe(run(
        Arc::clone(&manager),
        context.clone(),
        ns.clone(),
        tmp.clone(),
    ))
    .catch_unwind()
    .await;

    // Best-effort cleanup, also after a failed assertion.
    let _ = std::fs::remove_dir_all(&tmp);
    match manager.connect_client(&context).await {
        Ok(client) => {
            match Api::<Namespace>::all(client)
                .delete(&ns, &DeleteParams::default())
                .await
            {
                Ok(_) => println!("deleted namespace {ns}"),
                Err(err) => eprintln!("cleanup: deleting namespace {ns} failed: {err}"),
            }
        }
        Err(err) => eprintln!("cleanup: cannot connect to delete {ns}: {err}"),
    }

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
