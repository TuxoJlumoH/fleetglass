//! 03-04a previous-container logs against the kind dev cluster.
//!
//! Uses the long-running `fg-demo/crashloop` pod of `scripts/dev-cluster.sh`
//! (prints "crashing on purpose" and exits, so it always has a previous
//! instance once it restarted).
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test previous_logs -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`).

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::time::Duration;

use fleetglass_core::api::{MuxLogEvent, WorkloadLogTarget, WorkloadSelector};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::logs_mux::spawn_workload_log_follower;
use futures::FutureExt;
use k8s_openapi::api::core::v1::{Container, Namespace, Pod, PodSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{DeleteParams, PostParams};
use kube::runtime::wait::{await_condition, conditions};

const CONTEXT: &str = "kind-fleetglass-dev";
const NAMESPACE: &str = "fg-demo";
const TIMEOUT: Duration = Duration::from_secs(30);

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

fn target(selector: WorkloadSelector, container: Option<&str>) -> WorkloadLogTarget {
    WorkloadLogTarget {
        cluster: CONTEXT.to_owned(),
        namespace: NAMESPACE.to_owned(),
        selector,
        container: container.map(str::to_owned),
        tail_lines: Some(100),
        since_seconds: None,
        previous: true,
    }
}

/// Collects every event until the follower ends (one-shot: the channel closes).
async fn collect(target: WorkloadLogTarget) -> Vec<MuxLogEvent> {
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let manager = ClusterManager::new(source);
    let client = manager
        .connect_client(CONTEXT)
        .await
        .expect("connect to kind-fleetglass-dev (is scripts/dev-cluster.sh up?)");
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let follower = spawn_workload_log_follower(client, target, tx).await;
    let mut events = Vec::new();
    tokio::time::timeout(TIMEOUT, async {
        while let Some(event) = rx.recv().await {
            println!("event: {event:?}");
            events.push(event);
        }
    })
    .await
    .expect("previous-logs stream must end on its own (no follow)");
    follower.await.expect("follower task");
    events
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn previous_logs_of_crashloop_pod() {
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let manager = ClusterManager::new(source);
    let client = manager.connect_client(CONTEXT).await.expect("connect");
    let ns = format!(
        "fg-prev-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
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

    let outcome = AssertUnwindSafe(async {
        let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
        let pod = Pod {
            metadata: ObjectMeta {
                name: Some("crashy".to_owned()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: vec![Container {
                    name: "main".to_owned(),
                    image: Some("busybox:1.36".to_owned()),
                    command: Some(vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "echo 'crashing on purpose'; exit 1".to_owned(),
                    ]),
                    ..Default::default()
                }],
                restart_policy: Some("Always".to_owned()),
                termination_grace_period_seconds: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };
        pods.create(&PostParams::default(), &pod)
            .await
            .expect("create pod");

        // Wait for at least one restart
        let restart_check = async {
            loop {
                let p = pods.get("crashy").await.unwrap();
                if p.status
                    .as_ref()
                    .and_then(|s| s.container_statuses.as_ref())
                    .is_some_and(|cs| cs[0].restart_count > 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(30), restart_check)
            .await
            .expect("pod did not restart");

        let mut t = target(
            WorkloadSelector::Pod {
                name: "crashy".to_owned(),
            },
            None,
        );
        t.namespace = ns.clone();
        let events = collect(t).await;
        let lines: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                MuxLogEvent::Lines { lines } => Some(lines),
                _ => None,
            })
            .flatten()
            .collect();
        let has_expected_line = lines
            .iter()
            .any(|l| l.text.contains("crashing on purpose") && l.pod == "crashy");
        let has_gc_error = events.iter().any(|e| match e {
            MuxLogEvent::Error { message } => {
                message.contains("No previous container logs for main (the container was removed)")
            }
            _ => false,
        });

        assert!(
            has_expected_line || has_gc_error,
            "Expected either log lines or GC error, got: {events:?}"
        );
    })
    .catch_unwind()
    .await;

    if let Err(err) = namespaces.delete(&ns, &DeleteParams::default()).await {
        eprintln!("cleanup: deleting namespace {ns} failed: {err}");
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn previous_logs_without_a_previous_instance_is_readable() {
    // A fresh pod that never restarted has no previous container instance.
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let manager = ClusterManager::new(source);
    let client = manager.connect_client(CONTEXT).await.expect("connect");
    let ns = format!(
        "fg-prev-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
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

    let outcome = AssertUnwindSafe(async {
        let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
        let pod = Pod {
            metadata: ObjectMeta {
                name: Some("sleeper".to_owned()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: vec![Container {
                    name: "main".to_owned(),
                    image: Some("busybox:1.36".to_owned()),
                    command: Some(vec!["sleep".to_owned(), "3600".to_owned()]),
                    ..Default::default()
                }],
                termination_grace_period_seconds: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };
        pods.create(&PostParams::default(), &pod)
            .await
            .expect("create pod");
        let running = await_condition(pods.clone(), "sleeper", conditions::is_pod_running());
        tokio::time::timeout(Duration::from_secs(60), running)
            .await
            .expect("pod running in time")
            .expect("watch pod");

        let mut t = target(
            WorkloadSelector::Pod {
                name: "sleeper".to_owned(),
            },
            None,
        );
        t.namespace = ns.clone();
        let events = collect(t).await;
        let errors: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                MuxLogEvent::Error { message } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            errors,
            vec!["No previous container logs for main"],
            "{events:?}"
        );
    })
    .catch_unwind()
    .await;

    if let Err(err) = namespaces.delete(&ns, &DeleteParams::default()).await {
        eprintln!("cleanup: deleting namespace {ns} failed: {err}");
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn previous_logs_with_workload_selector_is_rejected() {
    let events = collect(target(
        WorkloadSelector::Labels {
            selector: "app=crashloop".to_owned(),
        },
        None,
    ))
    .await;
    assert!(
        matches!(events.as_slice(), [MuxLogEvent::Error { message }] if message.contains("single pod")),
        "{events:?}"
    );
}
