//! R-06 live mutating actions against the kind dev cluster: scale 3→1, rollout
//! restart, delete a pod, and the read-only / protected-context guards.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_actions -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`), whatever `KUBECONFIG` says.
//! Everything happens in a temporary `fg-r06-<suffix>` namespace that is deleted
//! (together with the temporary audit-log directory) even when an assertion fails.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetglass_core::Error;
use fleetglass_core::actions::{execute_action, execute_and_audit};
use fleetglass_core::api::{ActionRequest, ContextSettings, MutatingAction};
use fleetglass_core::audit::AuditLogStore;
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use futures::FutureExt;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{Container, Namespace, Pod, PodSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::Client;
use kube::api::{Api, DeleteParams, ListParams, PostParams};

const CONTEXT: &str = "kind-fleetglass-dev";
const TIMEOUT: Duration = Duration::from_secs(60);
const RESTARTED_AT: &str = "kubectl.kubernetes.io/restartedAt";

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

/// A Deployment of `pause` pods (the image every kind node already has).
fn test_deployment(namespace: &str, name: &str, replicas: i32) -> Deployment {
    let labels = BTreeMap::from([("app".to_owned(), name.to_owned())]);
    Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            namespace: Some(namespace.to_owned()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(replicas),
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
        }),
        ..Default::default()
    }
}

fn request(action: MutatingAction, confirmed_name: Option<&str>) -> ActionRequest {
    ActionRequest {
        context: CONTEXT.to_owned(),
        action,
        confirmed_name: confirmed_name.map(str::to_owned),
    }
}

/// Polls `check` every 250 ms until it returns `Some`, panicking after [`TIMEOUT`].
async fn wait_for<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(value) = check().await {
            println!("{what}: ok after {:?}", start.elapsed());
            return value;
        }
        assert!(
            start.elapsed() < TIMEOUT,
            "timed out after {TIMEOUT:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Live, non-terminating pods of the deployment, as `(name, uid)`.
async fn live_pods(pods: &Api<Pod>, app: &str) -> Vec<(String, String)> {
    pods.list(&ListParams::default().labels(&format!("app={app}")))
        .await
        .expect("list deployment pods")
        .items
        .into_iter()
        .filter(|p| p.metadata.deletion_timestamp.is_none())
        .map(|p| (p.metadata.name.unwrap(), p.metadata.uid.unwrap()))
        .collect()
}

async fn run(
    manager: &ClusterManager,
    client: Client,
    ns: String,
    suffix: String,
    audit_dir: PathBuf,
) {
    let deploy_name = format!("web-{suffix}");
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    let open = ContextSettings::default();

    deployments
        .create(
            &PostParams::default(),
            &test_deployment(&ns, &deploy_name, 3),
        )
        .await
        .expect("create test deployment");
    wait_for("deployment ready with 3 replicas", || async {
        let d = deployments.get(&deploy_name).await.ok()?;
        (d.status?.ready_replicas == Some(3)).then_some(())
    })
    .await;

    // Verify resolve_pod_controller resolves Pod -> ReplicaSet -> Deployment
    let initial_pods = live_pods(&pods, &deploy_name).await;
    let (pod_for_controller, _) = &initial_pods[0];
    let resolved =
        fleetglass_core::actions::resolve_pod_controller(client.clone(), &ns, pod_for_controller)
            .await
            .expect("resolve_pod_controller should succeed")
            .expect("controller should be present");
    assert_eq!(resolved.kind, "Deployment");
    assert_eq!(resolved.name, deploy_name);
    assert_eq!(resolved.namespace, ns);

    // 1. Scale 3 → 1.
    let scale = MutatingAction::Scale {
        kind: "Deployment".to_owned(),
        namespace: ns.clone(),
        name: deploy_name.clone(),
        replicas: 1,
    };
    execute_action(client.clone(), &request(scale, None), &open)
        .await
        .expect("scale deployment 3 -> 1");
    let spec_replicas = deployments
        .get(&deploy_name)
        .await
        .unwrap()
        .spec
        .and_then(|s| s.replicas);
    assert_eq!(spec_replicas, Some(1));
    wait_for("deployment down to 1 live pod", || async {
        let d = deployments.get(&deploy_name).await.ok()?;
        let status = d.status?;
        let pods_left = live_pods(&pods, &deploy_name).await.len();
        (status.replicas == Some(1) && status.ready_replicas == Some(1) && pods_left == 1)
            .then_some(())
    })
    .await;

    // 2. Rollout restart sets the pod-template annotation and rolls the pod.
    let template_annotation = |d: Deployment| {
        d.spec
            .and_then(|s| s.template.metadata)
            .and_then(|m| m.annotations)
            .and_then(|mut a| a.remove(RESTARTED_AT))
    };
    let before = deployments.get(&deploy_name).await.unwrap();
    assert_eq!(template_annotation(before.clone()), None);
    let generation_before = before.metadata.generation.unwrap_or_default();
    let pods_before_restart = live_pods(&pods, &deploy_name).await;

    let restart = MutatingAction::Restart {
        kind: "Deployment".to_owned(),
        namespace: ns.clone(),
        name: deploy_name.clone(),
    };
    execute_action(client.clone(), &request(restart, None), &open)
        .await
        .expect("restart deployment");
    let after = deployments.get(&deploy_name).await.unwrap();
    assert!(after.metadata.generation.unwrap_or_default() > generation_before);
    let restarted_at = template_annotation(after).expect("restartedAt annotation set");
    assert!(
        restarted_at.parse::<k8s_openapi::jiff::Timestamp>().is_ok(),
        "restartedAt should be RFC 3339 like kubectl writes, got {restarted_at}"
    );
    println!("restartedAt = {restarted_at}");
    let old_uids: Vec<String> = pods_before_restart.into_iter().map(|(_, u)| u).collect();
    wait_for("restart replaced the pod", || async {
        let d = deployments.get(&deploy_name).await.ok()?;
        let status = d.status?;
        let now = live_pods(&pods, &deploy_name).await;
        let rolled = now.len() == 1 && !old_uids.contains(&now[0].1);
        (rolled && status.updated_replicas == Some(1) && status.ready_replicas == Some(1))
            .then_some(())
    })
    .await;

    // 3. Delete the deployment's pod: it disappears and the ReplicaSet makes a new one.
    let (victim, victim_uid) = live_pods(&pods, &deploy_name).await.remove(0);
    let delete_pod = MutatingAction::Delete {
        kind: "Pod".to_owned(),
        namespace: Some(ns.clone()),
        name: victim.clone(),
    };
    execute_action(client.clone(), &request(delete_pod, None), &open)
        .await
        .expect("delete pod");
    wait_for("deleted pod gone", || async {
        match pods.get(&victim).await {
            Err(kube::Error::Api(e)) if e.code == 404 => Some(()),
            Ok(p) if p.metadata.uid.as_deref() != Some(victim_uid.as_str()) => Some(()),
            _ => None,
        }
    })
    .await;
    wait_for("replacement pod with a new uid, ready", || async {
        let now = live_pods(&pods, &deploy_name).await;
        let ready = deployments
            .get(&deploy_name)
            .await
            .ok()?
            .status?
            .ready_replicas;
        (now.len() == 1 && now[0].1 != victim_uid && ready == Some(1)).then_some(())
    })
    .await;

    // 4. Guards: rejected before any write, and the rejection is auditable.
    let snapshot = deployments.get(&deploy_name).await.unwrap();
    let pods_snapshot = live_pods(&pods, &deploy_name).await;
    let protected = ContextSettings {
        protected: true,
        ..ContextSettings::default()
    };
    let read_only = ContextSettings {
        read_only: true,
        ..ContextSettings::default()
    };
    let scale_to_3 = MutatingAction::Scale {
        kind: "Deployment".to_owned(),
        namespace: ns.clone(),
        name: deploy_name.clone(),
        replicas: 3,
    };
    let delete_deploy = MutatingAction::Delete {
        kind: "Deployment".to_owned(),
        namespace: Some(ns.clone()),
        name: deploy_name.clone(),
    };

    let audit = Arc::new(AuditLogStore::new(audit_dir.join("audit.jsonl")));

    for (settings, action, confirm) in [
        (&protected, &scale_to_3, None),
        (&protected, &scale_to_3, Some("wrong-name")),
        (&protected, &delete_deploy, None),
        (&protected, &delete_deploy, Some("wrong-name")),
    ] {
        let req = request(action.clone(), confirm);
        let outcome = execute_and_audit(manager, Arc::clone(&audit), &req, settings).await;
        assert!(
            matches!(
                &outcome,
                Err(Error::ConfirmationRequired { context, expected_name })
                    if context == CONTEXT && *expected_name == deploy_name
            ),
            "protected context with confirm={confirm:?} must require the typed name, got {outcome:?}"
        );
    }
    println!("protected context rejected missing/wrong typed name");

    // Read-only wins even when the typed name is correct.
    for action in [&scale_to_3, &delete_deploy] {
        let req = request(action.clone(), Some(&deploy_name));
        let outcome = execute_and_audit(manager, Arc::clone(&audit), &req, &read_only).await;
        assert!(
            matches!(&outcome, Err(Error::ReadOnlyContext { context }) if context == CONTEXT),
            "read-only context must reject, got {outcome:?}"
        );
    }
    println!("read-only context rejected writes");

    let unchanged = deployments.get(&deploy_name).await.unwrap();
    // `generation` only moves on spec writes (status updates by the controller don't count).
    assert_eq!(
        unchanged.metadata.generation, snapshot.metadata.generation,
        "rejected actions must not change the deployment spec"
    );
    assert_eq!(unchanged.spec.and_then(|s| s.replicas), Some(1));
    assert_eq!(live_pods(&pods, &deploy_name).await, pods_snapshot);

    let page = audit.query(0, 100, Some(CONTEXT), None).unwrap();
    assert_eq!(page.total, 6);
    assert!(page.entries.iter().all(|e| !e.success
        && e.error.is_some()
        && e.name == deploy_name
        && e.user.as_deref() == Some("kind-fleetglass-dev")));
    let mut ids: Vec<u32> = page.entries.iter().map(|e| e.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=6).collect::<Vec<u32>>());

    // 5. Protected context with the exact typed name goes through (and is audited).
    execute_and_audit(
        manager,
        Arc::clone(&audit),
        &request(delete_deploy, Some(&deploy_name)),
        &protected,
    )
    .await
    .expect("protected context with the correct typed name must succeed");
    let newest = audit.query(0, 1, Some(CONTEXT), None).unwrap().entries;
    assert!(
        newest[0].success
            && newest[0].id == 7
            && newest[0].action == "delete"
            && newest[0].user.as_deref() == Some("kind-fleetglass-dev")
    );
    wait_for("deployment deleted", || async {
        match deployments.get(&deploy_name).await {
            Err(kube::Error::Api(e)) if e.code == 404 => Some(()),
            _ => None,
        }
    })
    .await;
}

#[tokio::test]
async fn live_actions_scale_restart_delete_and_guards() {
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let manager = ClusterManager::new(source);
    let client = manager
        .connect_client(CONTEXT)
        .await
        .expect("connect to kind-fleetglass-dev (is scripts/dev-cluster.sh up?)");

    let suffix = unique_suffix();
    let ns = format!("fg-r06-{suffix}");
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
        .expect("create test namespace");
    println!("created namespace {ns}");

    let audit_dir = std::env::temp_dir().join(format!("fleetglass-live-actions-{suffix}"));
    let outcome = AssertUnwindSafe(run(
        &manager,
        client.clone(),
        ns.clone(),
        suffix,
        audit_dir.clone(),
    ))
    .catch_unwind()
    .await;

    if let Err(err) = std::fs::remove_dir_all(&audit_dir)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("cleanup: removing {} failed: {err}", audit_dir.display());
    }

    match namespaces.delete(&ns, &DeleteParams::default()).await {
        Ok(_) => println!("deleted namespace {ns}"),
        Err(err) => eprintln!("cleanup: deleting namespace {ns} failed: {err}"),
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
