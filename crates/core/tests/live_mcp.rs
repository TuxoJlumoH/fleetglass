//! R-01c live test: MCP `query_resources` starts its own watcher (no prior app
//! query) and returns pods from the `fleetglass-dev` kind cluster; a
//! ServiceAccount without list rights gets the forbidden state in both the MCP
//! answer and `ClusterManager::query_apps` (`AppPage::access`).
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_mcp -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`), whatever `KUBECONFIG` says.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetglass_core::api::{AppQuerySpec, Page, StoreAccessState};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::mcp::handle_json_rpc;
use futures::FutureExt;
use k8s_openapi::api::core::v1::{Namespace, ServiceAccount};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{DeleteParams, PostParams};

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

/// Writes a copy of `.dev/kubeconfig` (0600, it holds the admin credentials)
/// whose context user impersonates `user`.
fn impersonating_kubeconfig(context: &str, user: &str, dir: &Path) -> PathBuf {
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
    let bytes = serde_json::to_vec(&config).unwrap();
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&bytes).unwrap();
    }
    #[cfg(not(unix))]
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Calls the MCP `query_resources` tool; returns the page and the optional access note.
async fn mcp_query(
    manager: &ClusterManager,
    source: &KubeconfigSource,
    cluster: &str,
    kind: &str,
    namespace: &str,
) -> (Page, Option<String>) {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "query_resources",
            "arguments": { "cluster": cluster, "kind": kind, "namespace": namespace, "limit": 100 }
        }
    })
    .to_string();
    let resp = handle_json_rpc(&req, manager, source, None)
        .await
        .expect("response");
    let resp: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(resp["error"].is_null(), "MCP error: {resp}");
    let content = resp["result"]["content"].as_array().expect("content");
    let page: Page = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    let note = content
        .get(1)
        .and_then(|c| c["text"].as_str())
        .map(str::to_owned);
    (page, note)
}

async fn run(admin: &ClusterManager, context: String, ns: String, tmp: PathBuf) {
    // 1. A fresh manager: nothing was queried or watched before the MCP call.
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let fresh = ClusterManager::new(source.clone());
    assert!(fresh.get_store(&context, "Pod").is_none());
    let started = Instant::now();
    let (page, note) = mcp_query(&fresh, &source, &context, "pods", "kube-system").await;
    println!(
        "MCP query_resources after {:?}: total={} access={:?} note={note:?} pods={:?}",
        started.elapsed(),
        page.total,
        page.access,
        page.rows.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
    assert_eq!(page.access.len(), 1);
    assert_eq!(page.access[0].access, StoreAccessState::Ready);
    assert!(note.is_none(), "ready page has no access note: {note:?}");
    assert!(page.total > 0, "kube-system has pods in kind");
    assert!(
        page.rows
            .iter()
            .all(|r| r.kind == "Pod" && r.cluster == context)
    );
    assert!(
        page.rows
            .iter()
            .any(|r| r.name.starts_with("kube-apiserver-")),
        "expected kube-apiserver pod"
    );
    assert_eq!(
        fresh.get_store(&context, "Pod").unwrap().watchers_started(),
        1
    );
    assert_eq!(fresh.client_builds(), 1);

    // 2. A ServiceAccount without list rights: forbidden in MCP and in query_apps.
    let client = admin.connect_client(&context).await.unwrap();
    Api::<Namespace>::all(client.clone())
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
    Api::<ServiceAccount>::namespaced(client, &ns)
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
    let denied_source =
        KubeconfigSource::from_paths(vec![impersonating_kubeconfig(&context, &sa_user, &tmp)]);

    let denied = ClusterManager::new(denied_source.clone());
    let (page, note) = mcp_query(&denied, &denied_source, &context, "Pod", &ns).await;
    println!(
        "MCP as {sa_user}: total={} access={:?} note={note:?}",
        page.total, page.access
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
    let note = note.expect("forbidden page carries an access note");
    assert!(note.contains("No permission to list pods"), "{note}");

    // query_apps, as the `query_apps` command runs it (ensure pod watchers, then group).
    let apps_manager = ClusterManager::new(denied_source);
    apps_manager
        .ensure_watching_pairs(std::slice::from_ref(&context), &["Pod"])
        .await;
    let spec = AppQuerySpec {
        clusters: vec![context.clone()],
        namespaces: Vec::new(),
        text_filter: None,
        offset: 0,
        limit: 50,
    };
    let start = Instant::now();
    let apps = loop {
        let apps = apps_manager.query_apps(&spec);
        if matches!(
            apps.access.first().map(|a| &a.access),
            Some(StoreAccessState::Forbidden { .. })
        ) {
            break apps;
        }
        assert!(
            start.elapsed() < WAIT,
            "timed out waiting for forbidden in query_apps: {:?}",
            apps.access
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    println!(
        "query_apps as {sa_user} after {:?}: total={} access={:?}",
        start.elapsed(),
        apps.total,
        apps.access
    );
    assert_eq!(apps.total, 0);
    assert_eq!(apps.access.len(), 6);
    assert!(apps.access.iter().any(|a| a.kind == "Pod"));
}

#[tokio::test]
async fn live_mcp_query_resources_watches_and_reports_forbidden() {
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
    let admin = ClusterManager::new(source);

    let suffix = unique_suffix();
    let ns = format!("fg-r01c-{suffix}");
    let tmp = std::env::temp_dir().join(format!("fg-r01c-{suffix}"));

    let outcome = AssertUnwindSafe(run(&admin, context.clone(), ns.clone(), tmp.clone()))
        .catch_unwind()
        .await;

    // Best-effort cleanup (namespace holds the ServiceAccount), also after a failure.
    let _ = std::fs::remove_dir_all(&tmp);
    match admin.connect_client(&context).await {
        Ok(client) => match Api::<Namespace>::all(client)
            .delete(&ns, &DeleteParams::default())
            .await
        {
            Ok(_) => println!("deleted namespace {ns}"),
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(err) => eprintln!("cleanup: deleting namespace {ns} failed: {err}"),
        },
        Err(err) => eprintln!("cleanup: cannot connect to delete {ns}: {err}"),
    }

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
