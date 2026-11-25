//! 05-01 live test: a ServiceAccount whose Role only allows pods in one namespace
//! is forbidden the cluster-wide pod watch, and the store falls back to watching
//! that namespace, reported honestly as a restricted scope (not as Forbidden).
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_ns_fallback -- --nocapture`
//!
//! Only the dev kind cluster is used: `KUBECONFIG` (a single file) or `.dev/kubeconfig`,
//! never `~/.kube/config`.

#![cfg(feature = "integration")]
// Test-only file: `allow-unwrap-in-tests` does not reach the async helpers below.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetglass_core::api::{NamespaceSource, Page, QuerySpec, StoreAccess, StoreAccessState};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use futures::FutureExt;
use k8s_openapi::api::core::v1::{Container, Namespace, Pod, PodSpec, ServiceAccount};
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{DeleteParams, PostParams};

const WAIT: Duration = Duration::from_secs(60);
const SA_NAME: &str = "pods-reader";

fn dev_kubeconfig() -> PathBuf {
    match std::env::var_os("KUBECONFIG") {
        Some(path) if !path.is_empty() && Path::new(&path).is_file() => PathBuf::from(path),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig"),
    }
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}{:x}", std::process::id(), nanos & 0xff_ffff)
}

fn meta(name: &str, namespace: &str) -> ObjectMeta {
    ObjectMeta {
        name: Some(name.to_owned()),
        namespace: Some(namespace.to_owned()),
        ..Default::default()
    }
}

fn pod_spec(cluster: &str) -> QuerySpec {
    QuerySpec {
        clusters: vec![cluster.to_owned()],
        kinds: vec!["Pod".to_owned()],
        namespaces: Vec::new(),
        label_selector: None,
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
) -> Page {
    let start = Instant::now();
    loop {
        let page = manager.query(spec).await;
        if done(&page) {
            return page;
        }
        assert!(
            start.elapsed() < WAIT,
            "timed out after {WAIT:?} waiting for {what}; last page: total={} rows={:?} access={:?}",
            page.total,
            page.rows.iter().map(|r| &r.name).collect::<Vec<_>>(),
            page.access
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn write_kubeconfig_0600(path: &Path, bytes: &[u8]) {
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

/// Copy of the dev kubeconfig whose `context` user impersonates `user` and whose
/// context namespace is `namespace` (if any). Holds admin credentials, so it is
/// written 0600 to a temp dir that is removed after the test.
fn impersonating_kubeconfig(
    context: &str,
    user: &str,
    namespace: Option<&str>,
    path: &Path,
) -> PathBuf {
    let mut config = KubeconfigSource::from_paths(vec![dev_kubeconfig()])
        .load()
        .unwrap()
        .config;
    let ctx = config
        .contexts
        .iter_mut()
        .find(|c| c.name == context)
        .and_then(|c| c.context.as_mut())
        .unwrap();
    ctx.namespace = namespace.map(str::to_owned);
    let auth_name = ctx.user.clone().unwrap();
    let auth = config
        .auth_infos
        .iter_mut()
        .find(|a| a.name == auth_name)
        .and_then(|a| a.auth_info.as_mut())
        .unwrap();
    auth.impersonate = Some(user.to_owned());
    write_kubeconfig_0600(path, &serde_json::to_vec(&config).unwrap());
    path.to_owned()
}

/// The one Pod access entry of `page`.
fn pod_access(page: &Page) -> &StoreAccess {
    assert_eq!(page.access.len(), 1, "one store: {:?}", page.access);
    &page.access[0]
}

async fn create_fixtures(client: &kube::Client, ns: &str) {
    Api::<Namespace>::all(client.clone())
        .create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(ns.to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    Api::<ServiceAccount>::namespaced(client.clone(), ns)
        .create(
            &PostParams::default(),
            &ServiceAccount {
                metadata: meta(SA_NAME, ns),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    Api::<Role>::namespaced(client.clone(), ns)
        .create(
            &PostParams::default(),
            &Role {
                metadata: meta(SA_NAME, ns),
                rules: Some(vec![PolicyRule {
                    api_groups: Some(vec![String::new()]),
                    resources: Some(vec!["pods".to_owned()]),
                    verbs: vec!["get".to_owned(), "list".to_owned(), "watch".to_owned()],
                    ..Default::default()
                }]),
            },
        )
        .await
        .unwrap();
    Api::<RoleBinding>::namespaced(client.clone(), ns)
        .create(
            &PostParams::default(),
            &RoleBinding {
                metadata: meta(SA_NAME, ns),
                role_ref: RoleRef {
                    api_group: Some("rbac.authorization.k8s.io".to_owned()),
                    kind: "Role".to_owned(),
                    name: SA_NAME.to_owned(),
                },
                subjects: Some(vec![Subject {
                    kind: "ServiceAccount".to_owned(),
                    name: SA_NAME.to_owned(),
                    namespace: Some(ns.to_owned()),
                    ..Default::default()
                }]),
            },
        )
        .await
        .unwrap();
    Api::<Pod>::namespaced(client.clone(), ns)
        .create(
            &PostParams::default(),
            &Pod {
                metadata: meta("pause", ns),
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: "pause".to_owned(),
                        image: Some("registry.k8s.io/pause:3.10".to_owned()),
                        ..Default::default()
                    }],
                    termination_grace_period_seconds: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

/// Waits until the restricted store lists exactly the pod in `ns` and is Ready.
async fn assert_restricted(manager: &ClusterManager, context: &str, ns: &str) -> StoreAccess {
    let spec = pod_spec(context);
    let page = poll_query(manager, &spec, "restricted Ready store with 1 pod", |p| {
        p.total == 1
            && p.access.first().is_some_and(|a| {
                a.access == StoreAccessState::Ready
                    && a.scope.as_ref().is_some_and(|s| {
                        s.namespaces
                            .iter()
                            .all(|n| n.access == StoreAccessState::Ready)
                    })
            })
    })
    .await;
    assert_eq!(page.rows[0].namespace.as_deref(), Some(ns));
    assert_eq!(page.rows[0].name, "pause");
    let access = pod_access(&page).clone();
    let scope = access.scope.clone().expect("restricted scope reported");
    assert_eq!(
        scope
            .namespaces
            .iter()
            .map(|n| n.namespace.as_str())
            .collect::<Vec<_>>(),
        vec![ns],
        "only the namespace the Role allows is watched"
    );
    access
}

async fn run(admin: Arc<ClusterManager>, context: String, ns: String, tmp: PathBuf) {
    let client = admin.connect_client(&context).await.unwrap();
    create_fixtures(&client, &ns).await;
    println!("created namespace {ns} with SA/Role/RoleBinding {SA_NAME} and pod pause");
    std::fs::create_dir_all(&tmp).unwrap();
    let sa_user = format!("system:serviceaccount:{ns}:{SA_NAME}");

    // The cluster-wide pod list is really forbidden for the SA.
    let sa_config = impersonating_kubeconfig(&context, &sa_user, None, &tmp.join("plain"));
    let sa_client = ClusterManager::new(KubeconfigSource::from_paths(vec![sa_config.clone()]))
        .connect_client(&context)
        .await
        .unwrap();
    let all = Api::<Pod>::all(sa_client).list(&Default::default()).await;
    assert!(
        matches!(&all, Err(kube::Error::Api(s)) if s.code == 403),
        "cluster-wide list must be 403, got {all:?}"
    );

    // 1. Configured namespaces: the allowed one is watched, `default` is denied.
    let configured = ClusterManager::new(KubeconfigSource::from_paths(vec![sa_config]));
    configured.set_watch_namespaces(&context, vec![ns.clone(), "default".to_owned()]);
    configured.ensure_watching(&context, "Pod").await.unwrap();
    let access = assert_restricted(&configured, &context, &ns).await;
    let scope = access.scope.unwrap();
    println!("configured: {:?} scope={scope:?}", access.access);
    assert_eq!(scope.source, NamespaceSource::Configured);
    assert_eq!(scope.denied, vec!["default".to_owned()]);
    assert!(!scope.truncated);
    drop(configured);

    // 2. Nothing configured, namespaces cannot be listed: the context namespace is used.
    let ctx_config = impersonating_kubeconfig(&context, &sa_user, Some(&ns), &tmp.join("ctx-ns"));
    let detected = ClusterManager::new(KubeconfigSource::from_paths(vec![ctx_config]));
    detected.ensure_watching(&context, "Pod").await.unwrap();
    let access = assert_restricted(&detected, &context, &ns).await;
    let scope = access.scope.unwrap();
    println!("context namespace: {:?} scope={scope:?}", access.access);
    assert_eq!(scope.source, NamespaceSource::ContextNamespace);
    assert!(scope.denied.is_empty());
    drop(detected);

    // 3. Without any accessible namespace the store still reports Forbidden honestly.
    let none_config = impersonating_kubeconfig(&context, &sa_user, None, &tmp.join("none"));
    let denied = ClusterManager::new(KubeconfigSource::from_paths(vec![none_config]));
    denied.ensure_watching(&context, "Pod").await.unwrap();
    let page = poll_query(&denied, &pod_spec(&context), "forbidden state", |p| {
        matches!(
            p.access.first().map(|a| &a.access),
            Some(StoreAccessState::Forbidden { .. })
        )
    })
    .await;
    assert_eq!(page.total, 0);
    assert!(pod_access(&page).scope.is_none());
    println!("no accessible namespace: {:?}", pod_access(&page).access);
}

#[tokio::test]
async fn live_namespace_fallback_on_cluster_wide_403() {
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
        .expect("current-context in the dev kubeconfig");
    assert_eq!(context, "kind-fleetglass-dev", "only the dev kind cluster");
    let manager = Arc::new(ClusterManager::new(source));

    let suffix = unique_suffix();
    let ns = format!("dev-fg0501-{suffix}");
    let tmp = std::env::temp_dir().join(format!("fg-0501-{suffix}"));

    let outcome = AssertUnwindSafe(run(
        Arc::clone(&manager),
        context.clone(),
        ns.clone(),
        tmp.clone(),
    ))
    .catch_unwind()
    .await;

    // Best-effort cleanup, also after a failed assertion. Deleting the namespace
    // removes the SA, Role, RoleBinding and pod with it.
    let _ = std::fs::remove_dir_all(&tmp);
    match manager.connect_client(&context).await {
        Ok(client) => match Api::<Namespace>::all(client)
            .delete(&ns, &DeleteParams::default())
            .await
        {
            Ok(_) => println!("deleted namespace {ns}"),
            Err(err) => eprintln!("cleanup: deleting namespace {ns} failed: {err}"),
        },
        Err(err) => eprintln!("cleanup: cannot connect to delete {ns}: {err}"),
    }

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
