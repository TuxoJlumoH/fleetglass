//! R-10 live container exec integration tests against kind dev cluster.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_exec -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`).

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fleetglass_core::api::{ContextSettings, ExecSpec};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::error::Error;
use fleetglass_core::exec::start_exec_session;
use fleetglass_core::kubeconfig::KubeconfigSource;
use futures::FutureExt;
use k8s_openapi::api::core::v1::{Container, Pod, PodSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{DeleteParams, PostParams};
use tokio::sync::mpsc;

const CONTEXT: &str = "kind-fleetglass-dev";
const NAMESPACE: &str = "fg-demo";

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{:x}", std::process::id(), nanos)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_exec_interactive_shell_and_guards() {
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let manager = ClusterManager::new(source);
    let client = manager
        .connect_client(CONTEXT)
        .await
        .expect("connect to kind-fleetglass-dev (is scripts/dev-cluster.sh up?)");

    let pods: Api<Pod> = Api::namespaced(client.clone(), NAMESPACE);
    let pod_name = format!("fg-exec-test-{}", unique_suffix());

    let test_pod = Pod {
        metadata: ObjectMeta {
            name: Some(pod_name.clone()),
            namespace: Some(NAMESPACE.to_owned()),
            labels: Some(BTreeMap::from([(
                "app".to_owned(),
                "fg-exec-test".to_owned(),
            )])),
            ..Default::default()
        },
        spec: Some(PodSpec {
            restart_policy: Some("Never".to_owned()),
            containers: vec![Container {
                name: "sh".to_owned(),
                image: Some("busybox:1.36".to_owned()),
                command: Some(vec!["sleep".to_owned(), "3600".to_owned()]),
                ..Default::default()
            }],
            termination_grace_period_seconds: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    };

    pods.create(&PostParams::default(), &test_pod)
        .await
        .expect("create test pod");
    println!("created test pod {pod_name} in {NAMESPACE}");

    // Wait for pod to be running
    let start = Instant::now();
    let timeout = Duration::from_secs(60);
    loop {
        let is_running = pods
            .get(&pod_name)
            .await
            .ok()
            .and_then(|p| p.status)
            .and_then(|s| s.phase)
            .as_deref()
            == Some("Running");
        if is_running {
            break;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for test pod {pod_name} to be Running"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!("test pod {pod_name} is Running");

    let outcome = AssertUnwindSafe(async {
        // 1. Read-only context rejects exec session
        let ro_settings = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };
        let spec = ExecSpec {
            context: CONTEXT.to_owned(),
            namespace: NAMESPACE.to_owned(),
            pod: pod_name.clone(),
            container: Some("sh".to_owned()),
            command: vec!["/bin/sh".to_owned()],
            cols: 80,
            rows: 24,
        };
        let (ro_tx, _ro_rx) = mpsc::channel(16);
        let ro_err = start_exec_session(&client, &spec, &ro_settings, None, ro_tx)
            .await
            .unwrap_err();
        assert!(
            matches!(ro_err, Error::ReadOnlyContext { ref context } if context == CONTEXT),
            "expected ReadOnlyContext, got: {ro_err:?}"
        );
        println!("verified read-only context rejected exec session");

        // 2. Protected context rejects exec session without confirmation or with wrong name
        let protected_settings = ContextSettings {
            protected: true,
            ..ContextSettings::default()
        };
        let (prot_tx, _prot_rx) = mpsc::channel(16);
        let prot_no_confirm =
            start_exec_session(&client, &spec, &protected_settings, None, prot_tx)
                .await
                .unwrap_err();
        assert!(
            matches!(&prot_no_confirm, Error::ConfirmationRequired { context, expected_name }
                if context == CONTEXT && expected_name == &pod_name),
            "expected ConfirmationRequired for missing confirmed_name, got: {prot_no_confirm:?}"
        );

        let (prot_tx2, _prot_rx2) = mpsc::channel(16);
        let prot_wrong_confirm = start_exec_session(
            &client,
            &spec,
            &protected_settings,
            Some("wrong-pod-name"),
            prot_tx2,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&prot_wrong_confirm, Error::ConfirmationRequired { context, expected_name }
                if context == CONTEXT && expected_name == &pod_name),
            "expected ConfirmationRequired for wrong confirmed_name, got: {prot_wrong_confirm:?}"
        );
        println!("verified protected context rejected unconfirmed exec session");

        // 3. Protected context succeeds when confirmed_name matches pod_name
        let (prot_ok_tx, _prot_ok_rx) = mpsc::channel(16);
        let prot_session = start_exec_session(
            &client,
            &spec,
            &protected_settings,
            Some(&pod_name),
            prot_ok_tx,
        )
        .await
        .expect("start exec session on protected context with matching confirmed_name");
        prot_session.abort();
        println!("verified protected context allowed exec with matching confirmed_name");

        // 4. Interactive exec shell: send "echo hi\n" and assert output contains "hi"
        let open_settings = ContextSettings::default();
        let (out_tx, mut out_rx) = mpsc::channel(64);
        let session = start_exec_session(&client, &spec, &open_settings, None, out_tx)
            .await
            .expect("start interactive exec session");

        session
            .send_stdin(b"echo hi\n")
            .await
            .expect("send echo hi into stdin");

        let mut collected = String::new();
        let timeout = tokio::time::sleep(Duration::from_secs(15));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                _ = &mut timeout => {
                    panic!("timed out waiting for 'hi' in exec output; collected: {collected:?}");
                }
                chunk = out_rx.recv() => {
                    match chunk {
                        Some(text) => {
                            collected.push_str(&text);
                            if collected.contains("hi") {
                                break;
                            }
                        }
                        None => {
                            panic!("exec output channel closed unexpectedly; collected: {collected:?}");
                        }
                    }
                }
            }
        }

        assert!(collected.contains("hi"), "exec output did not contain 'hi'");
        println!("verified interactive exec shell received: {collected:?}");

        // Abort session
        session.abort();
    })
    .catch_unwind()
    .await;

    // Cleanup pod
    let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
    println!("cleaned up pod {pod_name}");

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
