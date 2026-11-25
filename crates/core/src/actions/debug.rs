use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{
    Container, EphemeralContainer, HostPathVolumeSource, Pod, PodSpec, SecurityContext, Toleration,
    Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, AttachParams, DeleteParams, Patch, PatchParams, PostParams};
use serde_json::json;
use tokio::sync::mpsc;

use crate::actions::enforce_write_safety;
use crate::api::debug::{DebugNodeRequest, DebugPodAction, DebugPodRequest, DebugPodResult};
use crate::api::{AuditEntry, ContextSettings};
use crate::audit::AuditLogStore;
use crate::cluster::ClusterManager;
use crate::error::{Error, Result};
use crate::exec::ExecSession;

const MAX_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const FATAL_WAITING_REASONS: [&str; 5] = [
    "ErrImagePull",
    "ImagePullBackOff",
    "InvalidImageName",
    "CreateContainerConfigError",
    "CreateContainerError",
];

/// Evaluation of an ephemeral container's current runtime status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EphemeralWaitState {
    Pending,
    Running,
    Failed(String),
}

/// Builds the shell-safe `kubectl debug` equivalent of a debug-container request.
#[must_use]
pub fn preview_debug_pod_kubectl(request: &DebugPodRequest) -> String {
    let mut command = format!(
        "kubectl --context {} -n {} debug {} -it --image={} --container={}",
        shell_quote(&request.context),
        shell_quote(&request.namespace),
        shell_quote(&format!("pod/{}", request.pod)),
        shell_quote(&request.image),
        shell_quote(&request.container_name),
    );

    if let Some(target) = request.target_container.as_deref() {
        command.push_str(" --target=");
        command.push_str(&shell_quote(target));
    }
    if !request.command.is_empty() {
        command.push_str(" --");
        for argument in &request.command {
            command.push(' ');
            command.push_str(&shell_quote(argument));
        }
    }
    command
}

/// Adds an ephemeral debug container and records every outcome in the local audit log.
///
/// Safety validation and context guards run before connecting, so rejected requests do
/// not cause a lazy context connection. Audit write failures are intentionally ignored.
pub async fn debug_pod_and_audit(
    manager: &ClusterManager,
    audit: Arc<AuditLogStore>,
    request: &DebugPodRequest,
    settings: &ContextSettings,
) -> Result<DebugPodResult> {
    let outcome = run_debug_pod(manager, request, settings).await;

    let entry = debug_audit_entry(manager, request, &outcome);
    let _ = tokio::task::spawn_blocking(move || audit.append_or_warn(&entry)).await;

    outcome
}

/// Stub for debugging a pod.
pub async fn debug_pod(
    _manager: &ClusterManager,
    action: &DebugPodAction,
) -> Result<String, Error> {
    Ok(format!("Debug pod {} in {}", action.pod, action.namespace))
}

/// Evaluates the status of `container_name` in `pod.status.ephemeral_container_statuses`.
#[must_use]
pub fn evaluate_ephemeral_status(pod: &Pod, container_name: &str) -> EphemeralWaitState {
    let Some(status) = pod
        .status
        .as_ref()
        .and_then(|s| s.ephemeral_container_statuses.as_deref())
        .and_then(|statuses| statuses.iter().find(|s| s.name == container_name))
    else {
        return EphemeralWaitState::Pending;
    };

    let Some(state) = status.state.as_ref() else {
        return EphemeralWaitState::Pending;
    };

    if state.running.is_some() {
        return EphemeralWaitState::Running;
    }

    if let Some(terminated) = state.terminated.as_ref() {
        let reason = terminated
            .reason
            .as_deref()
            .filter(|r| !r.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("exit {}", terminated.exit_code));
        let detail = match terminated
            .message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
        {
            Some(msg) => format!("{reason}: {msg}"),
            None => reason,
        };
        return EphemeralWaitState::Failed(detail);
    }

    if let Some(waiting) = state.waiting.as_ref()
        && let Some(reason) = waiting.reason.as_deref()
        && FATAL_WAITING_REASONS.contains(&reason)
    {
        let detail = match waiting.message.as_deref().filter(|m| !m.trim().is_empty()) {
            Some(msg) => format!("{reason}: {msg}"),
            None => reason.to_owned(),
        };
        return EphemeralWaitState::Failed(detail);
    }

    EphemeralWaitState::Pending
}

/// Polls pod status until the named ephemeral container reaches `Running`,
/// fails terminally, or `timeout` (capped at 60s) expires.
pub async fn wait_debug_container_running(
    manager: &ClusterManager,
    context: &str,
    namespace: &str,
    pod: &str,
    container_name: &str,
    timeout: Duration,
) -> Result<()> {
    for (field, value) in [
        ("context", context),
        ("namespace", namespace),
        ("pod", pod),
        ("container_name", container_name),
    ] {
        if value.trim().is_empty() {
            return Err(Error::InvalidDebugRequest { field });
        }
    }

    let deadline = tokio::time::Instant::now() + timeout.min(MAX_WAIT_TIMEOUT);
    let client = manager.connect_client(context).await?;
    let pods = Api::<Pod>::namespaced(client, namespace);

    loop {
        let pod_status = pods.get_status(pod).await?;
        match evaluate_ephemeral_status(&pod_status, container_name) {
            EphemeralWaitState::Running => return Ok(()),
            EphemeralWaitState::Failed(reason) => {
                return Err(Error::Exec {
                    message: format!(
                        "ephemeral container '{container_name}' in pod '{namespace}/{pod}' failed: {reason}"
                    ),
                });
            }
            EphemeralWaitState::Pending => {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(Error::Exec {
                        message: format!(
                            "timed out waiting for ephemeral container '{container_name}' in pod '{namespace}/{pod}'"
                        ),
                    });
                }
                tokio::time::sleep(WAIT_POLL_INTERVAL.min(remaining)).await;
            }
        }
    }
}

async fn run_debug_pod(
    manager: &ClusterManager,
    request: &DebugPodRequest,
    settings: &ContextSettings,
) -> Result<DebugPodResult> {
    validate_request(request)?;
    enforce_write_safety(
        &request.context,
        settings,
        &request.pod,
        request.confirmed_name.as_deref(),
    )?;

    let client = manager.connect_client(&request.context).await?;
    let ephemeral_container = EphemeralContainer {
        name: request.container_name.clone(),
        image: Some(request.image.clone()),
        target_container_name: request.target_container.clone(),
        command: (!request.command.is_empty()).then(|| request.command.clone()),
        stdin: Some(true),
        tty: Some(true),
        ..EphemeralContainer::default()
    };
    let patch = Patch::Strategic(json!({
        "spec": {
            "ephemeralContainers": [ephemeral_container]
        }
    }));

    Api::<Pod>::namespaced(client, &request.namespace)
        .patch_ephemeral_containers(&request.pod, &PatchParams::default(), &patch)
        .await?;

    Ok(DebugPodResult {
        container_name: request.container_name.clone(),
    })
}

fn debug_audit_entry(
    manager: &ClusterManager,
    request: &DebugPodRequest,
    outcome: &Result<DebugPodResult>,
) -> AuditEntry {
    AuditEntry {
        id: 0,
        timestamp: crate::audit::now_rfc3339(),
        cluster: request.context.clone(),
        user: manager.source().context_user(&request.context),
        action: "debug".to_owned(),
        kind: "Pod".to_owned(),
        namespace: Some(request.namespace.clone()),
        name: request.pod.clone(),
        kubectl: preview_debug_pod_kubectl(request),
        success: outcome.is_ok(),
        error: outcome.as_ref().err().map(crate::cluster::sanitize_error),
    }
}

fn validate_request(request: &DebugPodRequest) -> Result<()> {
    for (field, value) in [
        ("context", &request.context),
        ("namespace", &request.namespace),
        ("pod", &request.pod),
        ("container_name", &request.container_name),
        ("image", &request.image),
    ] {
        if value.trim().is_empty() {
            return Err(Error::InvalidDebugRequest { field });
        }
    }
    if request
        .target_container
        .as_deref()
        .is_some_and(|target| target.trim().is_empty())
    {
        return Err(Error::InvalidDebugRequest {
            field: "target_container",
        });
    }
    Ok(())
}

/// Builds the shell-safe `kubectl debug node/...` equivalent of a node debug request.
#[must_use]
pub fn preview_debug_node_kubectl(request: &DebugNodeRequest) -> String {
    let ns_flag = match request
        .namespace
        .as_deref()
        .filter(|n| !n.is_empty() && *n != "default")
    {
        Some(ns) => format!("-n {} ", shell_quote(ns)),
        None => String::new(),
    };
    let mut command = format!(
        "kubectl --context {} {}debug node/{} -it --image={}",
        shell_quote(&request.context),
        ns_flag,
        shell_quote(&request.node),
        shell_quote(&request.image),
    );
    if !request.command.is_empty() {
        command.push_str(" --");
        for argument in &request.command {
            command.push(' ');
            command.push_str(&shell_quote(argument));
        }
    }
    command
}

/// Generates a valid DNS-1123 label pod name for a node debug pod.
#[must_use]
pub fn generate_node_debug_pod_name(node: &str) -> String {
    let sanitized: String = node
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut name = format!("node-debugger-{}-{:x}", sanitized, epoch % 0xffffff);
    if name.len() > 63 {
        name.truncate(63);
    }
    name
}

/// Constructs the host-namespace debug pod definition targeting `request.node`.
#[must_use]
pub fn build_node_debug_pod(request: &DebugNodeRequest, pod_name: &str) -> Pod {
    let namespace = request
        .namespace
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or("default");
    Pod {
        metadata: ObjectMeta {
            name: Some(pod_name.to_owned()),
            namespace: Some(namespace.to_owned()),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".to_string(),
                    "fleetglass".to_string(),
                ),
                ("fleetglass.io/debug-node".to_string(), request.node.clone()),
            ])),
            ..Default::default()
        },
        spec: Some(PodSpec {
            node_name: Some(request.node.clone()),
            host_pid: Some(true),
            host_network: Some(true),
            host_ipc: Some(true),
            restart_policy: Some("Never".to_string()),
            tolerations: Some(vec![Toleration {
                operator: Some("Exists".to_string()),
                ..Default::default()
            }]),
            volumes: Some(vec![Volume {
                name: "host-root".to_string(),
                host_path: Some(HostPathVolumeSource {
                    path: "/".to_string(),
                    type_: Some("Directory".to_string()),
                }),
                ..Default::default()
            }]),
            containers: vec![Container {
                name: "debugger".to_string(),
                image: Some(request.image.clone()),
                command: if request.command.is_empty() {
                    None
                } else {
                    Some(request.command.clone())
                },
                stdin: Some(true),
                stdin_once: Some(false),
                tty: Some(true),
                volume_mounts: Some(vec![VolumeMount {
                    name: "host-root".to_string(),
                    mount_path: "/host".to_string(),
                    ..Default::default()
                }]),
                security_context: Some(SecurityContext {
                    privileged: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub fn validate_node_debug_request(request: &DebugNodeRequest) -> Result<()> {
    for (field, value) in [
        ("context", &request.context),
        ("node", &request.node),
        ("image", &request.image),
    ] {
        if value.trim().is_empty() {
            return Err(Error::InvalidDebugRequest { field });
        }
    }
    Ok(())
}

struct PodCleanupGuard {
    pods: Api<Pod>,
    pod_name: String,
}

impl Drop for PodCleanupGuard {
    fn drop(&mut self) {
        let pods = self.pods.clone();
        let pod_name = self.pod_name.clone();
        tokio::spawn(async move {
            let dp = DeleteParams::default();
            let _ = pods.delete(&pod_name, &dp).await;
        });
    }
}

/// Deletes a node debug pod from `namespace`.
pub async fn delete_node_debug_pod(
    client: &kube::Client,
    namespace: &str,
    pod_name: &str,
) -> Result<()> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let _ = pods.delete(pod_name, &DeleteParams::default()).await;
    Ok(())
}

/// Starts a node host debug shell session:
/// 1. Validates write safety and request fields.
/// 2. Creates a short-lived host-namespace debug pod on `request.node`.
/// 3. Waits for the container to become `Running`.
/// 4. Attaches an interactive stdin/stdout session.
/// 5. Automatically deletes the debug pod when the session ends or is dropped.
pub async fn start_node_debug_session(
    client: &kube::Client,
    request: &DebugNodeRequest,
    settings: &ContextSettings,
    output_tx: mpsc::Sender<String>,
) -> Result<(String, ExecSession)> {
    validate_node_debug_request(request)?;
    enforce_write_safety(
        &request.context,
        settings,
        &request.node,
        request.confirmed_name.as_deref(),
    )?;

    let namespace = request
        .namespace
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or("default");
    let pod_name = generate_node_debug_pod_name(&request.node);
    let pod_def = build_node_debug_pod(request, &pod_name);
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);

    pods.create(&PostParams::default(), &pod_def).await?;

    let deadline = tokio::time::Instant::now() + MAX_WAIT_TIMEOUT;
    loop {
        match pods.get_status(&pod_name).await {
            Ok(p) => {
                if let Some(status) = p.status {
                    if let Some(phase) = status.phase.as_deref() {
                        if phase == "Running" {
                            break;
                        }
                        if phase == "Failed" {
                            let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
                            return Err(Error::Exec {
                                message: format!("node debug pod '{pod_name}' failed to start"),
                            });
                        }
                    }
                    let fatal_reason = status
                        .container_statuses
                        .as_deref()
                        .and_then(|cs| cs.iter().find(|c| c.name == "debugger"))
                        .and_then(|c| c.state.as_ref())
                        .and_then(|s| s.waiting.as_ref())
                        .and_then(|w| w.reason.as_deref())
                        .filter(|r| FATAL_WAITING_REASONS.contains(r));

                    if let Some(reason) = fatal_reason {
                        let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
                        return Err(Error::Exec {
                            message: format!("node debug container failed: {reason}"),
                        });
                    }
                }
            }
            Err(e) => {
                let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
                return Err(Error::from(e));
            }
        }

        if tokio::time::Instant::now() >= deadline {
            let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
            return Err(Error::Exec {
                message: format!("timed out waiting for node debug pod '{pod_name}' to start"),
            });
        }
        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
    }

    let ap = AttachParams::default()
        .container("debugger")
        .stdin(true)
        .stdout(true)
        .stderr(false)
        .tty(true);

    let attached = match pods.attach(&pod_name, &ap).await {
        Ok(a) => a,
        Err(e) => {
            let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
            return Err(Error::from(e));
        }
    };

    let cleanup_guard = PodCleanupGuard {
        pods: pods.clone(),
        pod_name: pod_name.clone(),
    };

    let session = crate::exec::spawn_session_pumps(
        attached,
        request.cols.unwrap_or(80),
        request.rows.unwrap_or(24),
        output_tx,
        Some(Box::new(cleanup_guard)),
    )
    .await?;

    Ok((pod_name, session))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> DebugPodRequest {
        DebugPodRequest {
            context: "prod-eu".into(),
            namespace: "payments".into(),
            pod: "checkout-1".into(),
            container_name: "debugger".into(),
            image: "busybox:1.37".into(),
            target_container: Some("app".into()),
            command: vec!["sh".into(), "-c".into(), "echo hello".into()],
            confirmed_name: Some("checkout-1".into()),
        }
    }

    #[test]
    fn preview_debug_pod_formats_and_shell_quotes_dynamic_values() {
        assert_eq!(
            preview_debug_pod_kubectl(&request()),
            "kubectl --context prod-eu -n payments debug pod/checkout-1 -it --image=busybox:1.37 --container=debugger --target=app -- sh -c 'echo hello'"
        );

        let mut unsafe_request = request();
        unsafe_request.context = "prod; rm -rf /".into();
        unsafe_request.namespace = "team space".into();
        unsafe_request.pod = "pod'quoted".into();
        unsafe_request.image = "registry/debug image".into();
        unsafe_request.target_container = None;
        unsafe_request.command = vec!["$(whoami)".into(), "".into()];
        assert_eq!(
            preview_debug_pod_kubectl(&unsafe_request),
            "kubectl --context 'prod; rm -rf /' -n 'team space' debug 'pod/pod'\"'\"'quoted' -it --image='registry/debug image' --container=debugger -- '$(whoami)' ''"
        );
    }

    #[test]
    fn preview_omits_optional_target_and_empty_command() {
        let mut request = request();
        request.target_container = None;
        request.command.clear();
        assert_eq!(
            preview_debug_pod_kubectl(&request),
            "kubectl --context prod-eu -n payments debug pod/checkout-1 -it --image=busybox:1.37 --container=debugger"
        );
    }

    #[test]
    fn request_validation_rejects_empty_required_fields() {
        let mut request = request();
        request.image = " \t".into();
        assert!(matches!(
            validate_request(&request),
            Err(Error::InvalidDebugRequest { field: "image" })
        ));

        request.image = "busybox".into();
        request.target_container = Some(String::new());
        assert!(matches!(
            validate_request(&request),
            Err(Error::InvalidDebugRequest {
                field: "target_container"
            })
        ));
    }

    #[tokio::test]
    async fn readonly_rejection_happens_before_connect_and_is_audited() {
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-debug-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
        let manager =
            ClusterManager::new(crate::kubeconfig::KubeconfigSource::from_paths(Vec::new()));
        let settings = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };

        let result = debug_pod_and_audit(&manager, Arc::clone(&audit), &request(), &settings).await;
        assert!(matches!(result, Err(Error::ReadOnlyContext { .. })));

        let page = audit.query(0, 10, None, None).expect("read audit log");
        assert_eq!(page.total, 1);
        assert!(!page.entries[0].success);
        assert!(
            page.entries[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("read-only"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_debug_node_kubectl_formatting() {
        let req = DebugNodeRequest {
            context: "prod-eu".into(),
            node: "worker-1".into(),
            image: "busybox:1.37".into(),
            command: vec![],
            confirmed_name: None,
            namespace: None,
            cols: None,
            rows: None,
        };
        assert_eq!(
            preview_debug_node_kubectl(&req),
            "kubectl --context prod-eu debug node/worker-1 -it --image=busybox:1.37"
        );

        let custom_req = DebugNodeRequest {
            context: "prod".into(),
            node: "k8s-node.internal".into(),
            image: "nicolaka/netshoot".into(),
            command: vec!["chroot".into(), "/host".into()],
            confirmed_name: Some("k8s-node.internal".into()),
            namespace: Some("ops-debug".into()),
            cols: None,
            rows: None,
        };
        assert_eq!(
            preview_debug_node_kubectl(&custom_req),
            "kubectl --context prod -n ops-debug debug node/k8s-node.internal -it --image=nicolaka/netshoot -- chroot /host"
        );
    }

    #[test]
    fn build_node_debug_pod_spec_has_host_namespaces_and_mount() {
        let req = DebugNodeRequest {
            context: "prod".into(),
            node: "control-plane".into(),
            image: "busybox:1.37".into(),
            command: vec!["/bin/sh".into()],
            confirmed_name: None,
            namespace: Some("kube-system".into()),
            cols: None,
            rows: None,
        };
        let pod = build_node_debug_pod(&req, "node-debugger-test");
        assert_eq!(pod.metadata.name.as_deref(), Some("node-debugger-test"));
        assert_eq!(pod.metadata.namespace.as_deref(), Some("kube-system"));

        let spec = pod.spec.expect("pod spec");
        assert_eq!(spec.node_name.as_deref(), Some("control-plane"));
        assert_eq!(spec.host_pid, Some(true));
        assert_eq!(spec.host_network, Some(true));
        assert_eq!(spec.host_ipc, Some(true));
        assert_eq!(spec.restart_policy.as_deref(), Some("Never"));

        let container = &spec.containers[0];
        assert_eq!(container.name, "debugger");
        assert_eq!(container.image.as_deref(), Some("busybox:1.37"));
        assert_eq!(container.command, Some(vec!["/bin/sh".into()]));
        assert_eq!(container.stdin, Some(true));
        assert_eq!(container.tty, Some(true));

        let sec = container
            .security_context
            .as_ref()
            .expect("security context");
        assert_eq!(sec.privileged, Some(true));

        let vm = &container.volume_mounts.as_ref().expect("mounts")[0];
        assert_eq!(vm.name, "host-root");
        assert_eq!(vm.mount_path, "/host");
    }

    #[test]
    fn validate_node_debug_request_validations() {
        let mut req = DebugNodeRequest {
            context: "".into(),
            node: "n1".into(),
            image: "img".into(),
            command: vec![],
            confirmed_name: None,
            namespace: None,
            cols: None,
            rows: None,
        };
        assert!(matches!(
            validate_node_debug_request(&req),
            Err(Error::InvalidDebugRequest { field: "context" })
        ));

        req.context = "c1".into();
        req.node = " ".into();
        assert!(matches!(
            validate_node_debug_request(&req),
            Err(Error::InvalidDebugRequest { field: "node" })
        ));

        req.node = "n1".into();
        req.image = "".into();
        assert!(matches!(
            validate_node_debug_request(&req),
            Err(Error::InvalidDebugRequest { field: "image" })
        ));
    }

    #[test]
    fn generate_node_debug_pod_name_is_dns1123() {
        let name = generate_node_debug_pod_name("Worker.Node.East-1");
        assert!(name.starts_with("node-debugger-worker-node-east-1-"));
        assert!(name.len() <= 63);
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }

    #[test]
    fn node_debug_request_accepts_optional_dimensions() {
        let req: DebugNodeRequest = serde_json::from_value(serde_json::json!({
            "context": "prod",
            "node": "worker-1",
            "image": "busybox:1.37",
            "confirmedName": null,
            "namespace": null,
            "cols": 100,
            "rows": 30,
        }))
        .unwrap();
        assert_eq!((req.cols, req.rows), (Some(100), Some(30)));

        let legacy: DebugNodeRequest = serde_json::from_value(serde_json::json!({
            "context": "prod",
            "node": "worker-1",
            "image": "busybox:1.37",
            "confirmedName": null,
            "namespace": null,
        }))
        .unwrap();
        assert_eq!((legacy.cols, legacy.rows), (None, None));
    }
}
