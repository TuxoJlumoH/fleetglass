//! R-11 live pod logs integration test against kind cluster.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_logs -- --nocapture`
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

const CONTEXT: &str = "kind-fleetglass-dev";
const TIMEOUT: Duration = Duration::from_secs(60);

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

fn unique_uuid() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = u64::from(std::process::id());
    let hex = format!("{pid:016x}{nanos:016x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_pod_logs_follower() {
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let manager = ClusterManager::new(source);
    let client = manager
        .connect_client(CONTEXT)
        .await
        .expect("connect to kind-fleetglass-dev (is scripts/dev-cluster.sh up?)");

    let ns = format!("fg-r11-{}", unique_uuid());
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

    let outcome = AssertUnwindSafe(async {
        let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
        let counter_pod = Pod {
            metadata: ObjectMeta {
                name: Some("counter".to_owned()),
                namespace: Some(ns.clone()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                restart_policy: Some("Never".to_owned()),
                containers: vec![Container {
                    name: "counter".to_owned(),
                    image: Some("busybox:1.36".to_owned()),
                    command: Some(vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "i=1; while [ $i -le 10 ]; do echo \"counter line $i\"; i=$((i+1)); sleep 0.2; done; sleep 30".to_owned(),
                    ]),
                    ..Default::default()
                }],
                termination_grace_period_seconds: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        };

        pods.create(&PostParams::default(), &counter_pod)
            .await
            .expect("create counter pod");
        println!("created counter pod in {ns}");

        let target = WorkloadLogTarget {
            cluster: CONTEXT.to_owned(),
            namespace: ns.clone(),
            selector: WorkloadSelector::Pod {
                name: "counter".to_owned(),
            },
            container: None,
            tail_lines: None,
            since_seconds: None,
            previous: false,
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let follower = spawn_workload_log_follower(client.clone(), target, tx).await;

        let mut collected_lines = Vec::new();
        let timeout = tokio::time::sleep(TIMEOUT);
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = &mut timeout => {
                    panic!("timed out waiting for counter log lines; collected: {collected_lines:?}");
                }
                event = rx.recv() => {
                    let Some(event) = event else {
                        panic!("log stream closed unexpectedly; collected: {collected_lines:?}");
                    };
                    match event {
                        MuxLogEvent::Lines { lines } => {
                            for l in lines {
                                println!("received log line: {}", l.text.trim());
                                collected_lines.push(l.text);
                            }
                            if collected_lines.iter().any(|l| l.contains("counter line 1"))
                                && collected_lines.iter().any(|l| l.contains("counter line 2"))
                            {
                                break;
                            }
                        }
                        MuxLogEvent::PodJoined { pod } => {
                            println!("pod joined: {pod}");
                        }
                        MuxLogEvent::PodLeft { pod } => {
                            println!("pod left: {pod}");
                        }
                        MuxLogEvent::Error { message } => {
                            println!("mux error: {message}");
                        }
                    }
                }
            }
        }

        assert!(
            collected_lines.iter().any(|l| l.contains("counter line 1")),
            "missing 'counter line 1' in {collected_lines:?}"
        );
        assert!(
            collected_lines.iter().any(|l| l.contains("counter line 2")),
            "missing 'counter line 2' in {collected_lines:?}"
        );

        follower.abort();
    })
    .catch_unwind()
    .await;

    match namespaces.delete(&ns, &DeleteParams::default()).await {
        Ok(_) => println!("deleted namespace {ns}"),
        Err(err) => eprintln!("cleanup: deleting namespace {ns} failed: {err}"),
    }

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
