//! R-08 live port-forward integration tests against the kind dev cluster.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_portforward -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`).

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::portforward::PortForwardManager;
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use kube::api::{DeleteParams, ListParams, PostParams};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

const CONTEXT: &str = "kind-fleetglass-dev";
const NAMESPACE: &str = "fg-demo";

#[tokio::test]
async fn live_portforward_http_request_and_stop() {
    let source = KubeconfigSource::from_paths(vec![dev_kubeconfig()]);
    let cluster_mgr = ClusterManager::new(source);
    let client = cluster_mgr.connect_client(CONTEXT).await.unwrap();

    let pods_api: Api<Pod> = Api::namespaced(client.clone(), NAMESPACE);

    // Look for an existing running pod with port 80 in fg-demo (e.g. deploy/web)
    let lp = ListParams::default().labels("app=web");
    let pod_list = pods_api.list(&lp).await.unwrap();
    let existing_pod = pod_list.items.into_iter().find(|p| {
        p.status
            .as_ref()
            .and_then(|s| s.phase.as_deref())
            .map(|phase| phase == "Running")
            .unwrap_or(false)
    });

    let (pod_name, remote_port, created_ephemeral) = if let Some(p) = existing_pod {
        (p.metadata.name.unwrap(), 80u16, false)
    } else {
        // Fallback: create an ephemeral nginx pod
        let pod_name = format!("fg-pf-test-{}", std::process::id());
        let pod_yaml = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": pod_name,
                "labels": { "app": "fg-pf-test" }
            },
            "spec": {
                "containers": [{
                    "name": "nginx",
                    "image": "nginx:1.27-alpine",
                    "ports": [{ "containerPort": 80 }]
                }]
            }
        });
        let pod: Pod = serde_json::from_value(pod_yaml).unwrap();
        pods_api.create(&PostParams::default(), &pod).await.unwrap();

        // Wait for pod to be running
        let start = Instant::now();
        let timeout = Duration::from_secs(60);
        loop {
            let is_running = pods_api
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
                "timed out waiting for ephemeral test pod {pod_name} to be Running"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        (pod_name, 80u16, true)
    };

    let pf_manager = PortForwardManager::new();

    // 1. Start port forward with local port 0 (ephemeral).
    let info = pf_manager
        .start_local(
            CONTEXT.to_owned(),
            NAMESPACE.to_owned(),
            pod_name.clone(),
            remote_port,
            0,
            Some(client.clone()),
        )
        .await
        .unwrap();

    assert!(info.local_port > 0);
    assert_eq!(info.remote_port, remote_port);
    assert_eq!(
        info.browser_url,
        format!("http://127.0.0.1:{}", info.local_port)
    );
    assert_eq!(info.error, None);

    let list = pf_manager.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, info.id);

    // 2. Connect to 127.0.0.1:<local_port> via TCP and send HTTP GET.
    let mut stream = TcpStream::connect(("127.0.0.1", info.local_port))
        .await
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let response_str = String::from_utf8_lossy(&response);

    assert!(
        response_str.contains("HTTP/1.1 200 OK")
            || response_str.contains("200 OK")
            || response_str.contains("Welcome to nginx!"),
        "expected HTTP 200 OK or nginx welcome from port-forward, got:\n{response_str}"
    );

    // 3. Stop the port-forward and assert the local port is closed.
    assert!(pf_manager.stop(info.id));
    assert_eq!(pf_manager.list().len(), 0);

    // Give the listener task a moment to fully drop the socket
    tokio::time::sleep(Duration::from_millis(150)).await;

    let post_stop_conn = TcpStream::connect(("127.0.0.1", info.local_port)).await;
    assert!(
        post_stop_conn.is_err(),
        "expected local port {} to be closed after stop, but connect succeeded",
        info.local_port
    );

    // 4. Test error state recording on nonexistent pod
    let bad_info = pf_manager
        .start_local(
            CONTEXT.to_owned(),
            NAMESPACE.to_owned(),
            "nonexistent-pod-fleetglass-test".to_owned(),
            remote_port,
            0,
            Some(client.clone()),
        )
        .await
        .unwrap();

    // Connect to trigger portforward request
    if let Ok(mut bad_stream) = TcpStream::connect(("127.0.0.1", bad_info.local_port)).await {
        let _ = bad_stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await;
        let mut buf = [0u8; 128];
        let _ = bad_stream.read(&mut buf).await;
    }

    // Wait briefly for error slot to be populated
    let start = Instant::now();
    let mut recorded_err = None;
    while start.elapsed() < Duration::from_secs(5) {
        if let Some(err) = pf_manager.get(bad_info.id).and_then(|i| i.error) {
            recorded_err = Some(err);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        recorded_err.is_some(),
        "expected error to be recorded on active forward for nonexistent pod"
    );
    assert!(pf_manager.stop(bad_info.id));

    // Cleanup ephemeral pod if one was created
    if created_ephemeral {
        let _ = pods_api.delete(&pod_name, &DeleteParams::default()).await;
    }
}
