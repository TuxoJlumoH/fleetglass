//! Local port-forwarding manager (`PortForwardManager`, P1-10, 04-08).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

use crate::api::PortForwardInfo;
use crate::error::{Error, Result};

struct ConnGuard(Arc<AtomicU32>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct ActiveForward {
    info: PortForwardInfo,
    active_conns: Arc<AtomicU32>,
    total_conns: Arc<AtomicU64>,
    error: Arc<RwLock<Option<String>>>,
    stop_tx: tokio::sync::watch::Sender<bool>,
    abort: AbortHandle,
}

/// Manages active local port-forward listeners and their background tasks.
#[derive(Debug, Default)]
pub struct PortForwardManager {
    next_id: AtomicU32,
    active: Mutex<BTreeMap<u32, ActiveForward>>,
}

impl PortForwardManager {
    /// Creates an empty [`PortForwardManager`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `127.0.0.1:<requested_local_port>` (or an ephemeral port when `0`),
    /// spawns a background accept loop, and registers the active forward.
    ///
    /// If `client` is provided (`Some`), accepted TCP connections are forwarded
    /// to the target pod via Kubernetes port-forward WebSocket stream.
    /// If `None`, connections are accepted and discarded (unit testing mode).
    pub async fn start_local(
        &self,
        context: String,
        namespace: String,
        pod: String,
        remote_port: u16,
        requested_local_port: u16,
        client: Option<kube::Client>,
    ) -> Result<PortForwardInfo> {
        let bind_label = PathBuf::from(format!("127.0.0.1:{requested_local_port}"));
        let listener = TcpListener::bind(("127.0.0.1", requested_local_port))
            .await
            .map_err(|source| Error::KubeconfigRead {
                path: bind_label.clone(),
                source,
            })?;
        let local_port = listener
            .local_addr()
            .map_err(|source| Error::KubeconfigRead {
                path: bind_label,
                source,
            })?
            .port();

        let id = self.next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let info = PortForwardInfo {
            id,
            context,
            namespace: namespace.clone(),
            pod: pod.clone(),
            remote_port,
            local_port,
            browser_url: format!("http://127.0.0.1:{local_port}"),
            error: None,
            active_connections: 0,
            total_connections: 0,
        };

        let active_conns = Arc::new(AtomicU32::new(0));
        let total_conns = Arc::new(AtomicU64::new(0));
        let active_conns_for_task = Arc::clone(&active_conns);
        let total_conns_for_task = Arc::clone(&total_conns);

        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let error_slot = Arc::new(RwLock::new(None));
        let error_for_task = Arc::clone(&error_slot);

        let join_handle = tokio::spawn(async move {
            loop {
                if *stop_rx.borrow() {
                    break;
                }

                let mut stop_accept = stop_rx.clone();
                let accept_res = tokio::select! {
                    res = listener.accept() => res,
                    _ = stop_accept.changed() => break,
                };

                let (mut local_stream, _peer_addr) = match accept_res {
                    Ok(conn) => conn,
                    Err(err) => {
                        tracing::debug!(?err, "port-forward listener accept failed or closed");
                        break;
                    }
                };

                total_conns_for_task.fetch_add(1, Ordering::Relaxed);
                active_conns_for_task.fetch_add(1, Ordering::Relaxed);
                let conn_guard = ConnGuard(Arc::clone(&active_conns_for_task));

                let Some(client) = client.clone() else {
                    // Unit test mock mode without cluster: keep stream until remote closes
                    tokio::spawn(async move {
                        let _guard = conn_guard;
                        let mut buf = [0u8; 1024];
                        let _ = local_stream.read(&mut buf).await;
                    });
                    continue;
                };

                let namespace = namespace.clone();
                let pod = pod.clone();
                let error_slot = Arc::clone(&error_for_task);
                let mut stop_conn = stop_rx.clone();

                tokio::spawn(async move {
                    let _guard = conn_guard;
                    if *stop_conn.borrow() {
                        return;
                    }

                    let pods: Api<Pod> = Api::namespaced(client, &namespace);
                    let mut pf = match pods.portforward(&pod, &[remote_port]).await {
                        Ok(pf) => pf,
                        Err(err) => {
                            let msg = format!("{err}");
                            tracing::warn!(?err, %namespace, %pod, remote_port, "failed to port-forward to pod");
                            if let Ok(mut slot) = error_slot.write() {
                                *slot = Some(msg);
                            }
                            return;
                        }
                    };

                    let mut upstream = match pf.take_stream(remote_port) {
                        Some(stream) => stream,
                        None => {
                            let msg = format!("port {remote_port} not found in stream");
                            tracing::warn!(%namespace, %pod, remote_port, "{msg}");
                            if let Ok(mut slot) = error_slot.write() {
                                *slot = Some(msg);
                            }
                            return;
                        }
                    };

                    // Cleared error on successfully establishing the upstream stream
                    if let Ok(mut slot) = error_slot.write() {
                        *slot = None;
                    }

                    let err_fut = pf.take_error(remote_port);

                    tokio::select! {
                        _ = stop_conn.changed() => {},
                        copy_res = tokio::io::copy_bidirectional(&mut local_stream, &mut upstream) => {
                            if let Err(err) = copy_res {
                                tracing::debug!(?err, %namespace, %pod, remote_port, "port-forward stream closed with error");
                            }
                        }
                        server_err = async {
                            match err_fut {
                                Some(fut) => match fut.await {
                                    Some(err) => err,
                                    None => futures::future::pending().await,
                                },
                                None => futures::future::pending().await,
                            }
                        } => {
                            tracing::warn!(%server_err, %namespace, %pod, remote_port, "port-forward received error from pod");
                            if let Ok(mut slot) = error_slot.write() {
                                *slot = Some(server_err);
                            }
                        }
                    }
                });
            }
        });

        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        active.insert(
            id,
            ActiveForward {
                info: info.clone(),
                active_conns,
                total_conns,
                error: error_slot,
                stop_tx,
                abort: join_handle.abort_handle(),
            },
        );

        Ok(info)
    }

    /// Returns the active port-forward with `id`, if found.
    pub fn get(&self, id: u32) -> Option<PortForwardInfo> {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        active.get(&id).map(|f| {
            let mut info = f.info.clone();
            if let Ok(guard) = f.error.read() {
                info.error = guard.clone();
            }
            info.active_connections = f.active_conns.load(Ordering::Relaxed);
            info.total_connections = f.total_conns.load(Ordering::Relaxed);
            info
        })
    }

    /// Lists all active port-forwards in ID order.
    pub fn list(&self) -> Vec<PortForwardInfo> {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        active
            .values()
            .map(|f| {
                let mut info = f.info.clone();
                if let Ok(guard) = f.error.read() {
                    info.error = guard.clone();
                }
                info.active_connections = f.active_conns.load(Ordering::Relaxed);
                info.total_connections = f.total_conns.load(Ordering::Relaxed);
                info
            })
            .collect()
    }

    /// Stops and removes the port-forward with `id`. Returns `true` if found.
    pub fn stop(&self, id: u32) -> bool {
        let removed = {
            let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            active.remove(&id)
        };
        if let Some(entry) = removed {
            let _ = entry.stop_tx.send(true);
            entry.abort.abort();
            true
        } else {
            false
        }
    }
}

/// Resolves a port-forward target (`Pod`, `Service`, `Deployment`) to a ready backing pod name (`04-08`).
pub async fn resolve_target_pod(
    client: &kube::Client,
    namespace: &str,
    target_kind: &str,
    target_name: &str,
) -> Result<String> {
    match target_kind.to_ascii_lowercase().as_str() {
        "pod" => Ok(target_name.to_owned()),
        "service" | "svc" => {
            let svcs: Api<k8s_openapi::api::core::v1::Service> =
                Api::namespaced(client.clone(), namespace);
            let svc = svcs.get(target_name).await.map_err(Error::Kube)?;
            let selector = svc.spec.and_then(|s| s.selector).unwrap_or_default();
            if selector.is_empty() {
                return Err(Error::TargetNotFound {
                    kind: "Service".to_owned(),
                    name: target_name.to_owned(),
                    message: "service has no selector".to_owned(),
                });
            }
            let label_selector = selector
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            find_ready_pod_by_selector(client, namespace, &label_selector).await
        }
        "deployment" | "deploy" => {
            let deploys: Api<k8s_openapi::api::apps::v1::Deployment> =
                Api::namespaced(client.clone(), namespace);
            let deploy = deploys.get(target_name).await.map_err(Error::Kube)?;
            let match_labels = deploy
                .spec
                .and_then(|s| s.selector.match_labels)
                .unwrap_or_default();
            if match_labels.is_empty() {
                return Err(Error::TargetNotFound {
                    kind: "Deployment".to_owned(),
                    name: target_name.to_owned(),
                    message: "deployment has no selector matchLabels".to_owned(),
                });
            }
            let label_selector = match_labels
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            find_ready_pod_by_selector(client, namespace, &label_selector).await
        }
        _ => Ok(target_name.to_owned()),
    }
}

async fn find_ready_pod_by_selector(
    client: &kube::Client,
    namespace: &str,
    label_selector: &str,
) -> Result<String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let lp = kube::api::ListParams::default().labels(label_selector);
    let list = pods.list(&lp).await.map_err(Error::Kube)?;

    // First try a ready pod
    for pod in &list.items {
        let is_ready = pod
            .status
            .as_ref()
            .and_then(|st| st.conditions.as_ref())
            .is_some_and(|conds| {
                conds
                    .iter()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
            });
        if is_ready && let Some(ref name) = pod.metadata.name {
            return Ok(name.clone());
        }
    }
    // Fall back to any pod in list
    if let Some(first) = list.items.into_iter().next()
        && let Some(name) = first.metadata.name
    {
        return Ok(name);
    }
    Err(Error::TargetNotFound {
        kind: "Pod".to_owned(),
        name: format!("backing {label_selector}"),
        message: "no matching ready pod found".to_owned(),
    })
}

impl Drop for PortForwardManager {
    fn drop(&mut self) {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        for entry in active.values() {
            let _ = entry.stop_tx.send(true);
            entry.abort.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starts_lists_and_stops_local_port_forward() {
        let manager = PortForwardManager::new();
        let info = manager
            .start_local(
                "fleetglass-dev".to_owned(),
                "fg-demo".to_owned(),
                "web-0".to_owned(),
                8080,
                0,
                None,
            )
            .await
            .unwrap();

        assert!(info.local_port > 0);
        assert_eq!(info.remote_port, 8080);
        assert_eq!(
            info.browser_url,
            format!("http://127.0.0.1:{}", info.local_port)
        );
        assert_eq!(info.error, None);
        assert_eq!(info.active_connections, 0);
        assert_eq!(info.total_connections, 0);

        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed, vec![info.clone()]);

        assert_eq!(manager.get(info.id), Some(info.clone()));

        assert!(manager.stop(info.id));
        assert!(!manager.stop(info.id));
        assert!(manager.list().is_empty());
        assert_eq!(manager.get(info.id), None);
    }

    #[tokio::test]
    async fn active_and_total_connections_telemetry() {
        let manager = PortForwardManager::new();
        let info = manager
            .start_local(
                "dev".to_owned(),
                "default".to_owned(),
                "api-0".to_owned(),
                80,
                0,
                None,
            )
            .await
            .unwrap();

        assert_eq!(info.active_connections, 0);
        assert_eq!(info.total_connections, 0);

        // Connect a stream to the local port
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", info.local_port))
            .await
            .unwrap();

        // Allow accept loop to process
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let cur = manager.get(info.id).unwrap();
        assert_eq!(cur.active_connections, 1);
        assert_eq!(cur.total_connections, 1);

        // Close stream
        use tokio::io::AsyncWriteExt;
        let _ = stream.shutdown().await;
        drop(stream);

        // Allow drop to propagate
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let after = manager.get(info.id).unwrap();
        assert_eq!(after.active_connections, 0);
        assert_eq!(after.total_connections, 1);
    }
}
