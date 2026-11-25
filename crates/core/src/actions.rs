//! Mutating Kubernetes actions, `kubectl` command preview, and write-safety guards.
//!
//! Core write safety: every mutating action can be previewed as a
//! `kubectl` command, respects the per-context `read_only` flag, and requires
//! typing the resource name on `protected` contexts before any write reaches
//! `kube::Api`.

use std::sync::Arc;

use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::{
    ConfigMap, Namespace, Node, PersistentVolumeClaim, Pod, Secret, Service,
};
use k8s_openapi::api::networking::v1::Ingress;
use kube::Client;
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use serde_json::json;

use crate::api::{ActionRequest, AuditEntry, ContextSettings, MutatingAction};
use crate::audit::AuditLogStore;
use crate::cluster::ClusterManager;
use crate::error::{Error, Result};
pub mod debug;

pub use crate::api::ControllerRef;
pub use crate::apply::{apply_yaml, dry_run_apply, preview_kubectl_apply};

/// Returns the target resource name for a [`MutatingAction`].
pub fn action_target_name(action: &MutatingAction) -> &str {
    match action {
        MutatingAction::Delete { name, .. }
        | MutatingAction::Scale { name, .. }
        | MutatingAction::Restart { name, .. }
        | MutatingAction::FluxReconcile { name, .. }
        | MutatingAction::FluxSuspend { name, .. }
        | MutatingAction::ArgoRolloutPromote { name, .. }
        | MutatingAction::ArgoRolloutAbort { name, .. }
        | MutatingAction::ArgoRolloutRetry { name, .. }
        | MutatingAction::ArgoAppSync { name, .. }
        | MutatingAction::CertRenew { name, .. }
        | MutatingAction::ExternalSecretForceSync { name, .. } => name,
        MutatingAction::Cordon { node }
        | MutatingAction::Uncordon { node }
        | MutatingAction::Drain { node } => node,
    }
}

/// Returns the short audit-log label for a [`MutatingAction`] (`"delete"`, `"scale"`, ...).
pub fn action_label(action: &MutatingAction) -> &'static str {
    match action {
        MutatingAction::Delete { .. } => "delete",
        MutatingAction::Scale { .. } => "scale",
        MutatingAction::Restart { .. } => "restart",
        MutatingAction::Cordon { .. } => "cordon",
        MutatingAction::Uncordon { .. } => "uncordon",
        MutatingAction::Drain { .. } => "drain",
        MutatingAction::FluxReconcile { .. } => "flux_reconcile",
        MutatingAction::FluxSuspend { suspend: true, .. } => "flux_suspend",
        MutatingAction::FluxSuspend { suspend: false, .. } => "flux_resume",
        MutatingAction::ArgoRolloutPromote { full: true, .. } => "argo_rollout_promote_full",
        MutatingAction::ArgoRolloutPromote { full: false, .. } => "argo_rollout_promote",
        MutatingAction::ArgoRolloutAbort { .. } => "argo_rollout_abort",
        MutatingAction::ArgoRolloutRetry { .. } => "argo_rollout_retry",
        MutatingAction::ArgoAppSync { .. } => "argo_app_sync",
        MutatingAction::CertRenew { .. } => "cert_renew",
        MutatingAction::ExternalSecretForceSync { .. } => "externalsecret_force_sync",
    }
}

/// Returns the target resource kind for a [`MutatingAction`] (`"Node"` for node actions).
pub fn action_target_kind(action: &MutatingAction) -> &str {
    match action {
        MutatingAction::Delete { kind, .. }
        | MutatingAction::Scale { kind, .. }
        | MutatingAction::Restart { kind, .. }
        | MutatingAction::FluxReconcile { kind, .. }
        | MutatingAction::FluxSuspend { kind, .. } => kind,
        MutatingAction::ArgoRolloutPromote { .. }
        | MutatingAction::ArgoRolloutAbort { .. }
        | MutatingAction::ArgoRolloutRetry { .. } => "Rollout",
        MutatingAction::ArgoAppSync { .. } => "Application",
        MutatingAction::CertRenew { .. } => "Certificate",
        MutatingAction::ExternalSecretForceSync { .. } => "ExternalSecret",
        MutatingAction::Cordon { .. }
        | MutatingAction::Uncordon { .. }
        | MutatingAction::Drain { .. } => "Node",
    }
}

/// Returns the target namespace for a [`MutatingAction`] (`None` for cluster-scoped targets).
pub fn action_target_namespace(action: &MutatingAction) -> Option<&str> {
    match action {
        MutatingAction::Delete { namespace, .. } => {
            namespace.as_deref().filter(|ns| !ns.is_empty())
        }
        MutatingAction::Scale { namespace, .. }
        | MutatingAction::Restart { namespace, .. }
        | MutatingAction::FluxReconcile { namespace, .. }
        | MutatingAction::FluxSuspend { namespace, .. }
        | MutatingAction::ArgoRolloutPromote { namespace, .. }
        | MutatingAction::ArgoRolloutAbort { namespace, .. }
        | MutatingAction::ArgoRolloutRetry { namespace, .. }
        | MutatingAction::ArgoAppSync { namespace, .. }
        | MutatingAction::CertRenew { namespace, .. }
        | MutatingAction::ExternalSecretForceSync { namespace, .. } => Some(namespace),
        MutatingAction::Cordon { .. }
        | MutatingAction::Uncordon { .. }
        | MutatingAction::Drain { .. } => None,
    }
}

/// Builds the audit-log entry (P3-01) for an attempted action and its outcome.
///
/// Rejected attempts (read-only, missing confirmation, connection or API errors) are
/// recorded with `success: false` and the error message, so the log shows every attempt.
/// The message is sanitised: exec credential plugin failures never write their command,
/// environment or output (which can carry tokens) into the audit file.
/// `id` is left at 0; [`AuditLogStore::append`] assigns it.
pub fn audit_entry(request: &ActionRequest, outcome: &Result<()>) -> AuditEntry {
    let action = &request.action;
    AuditEntry {
        id: 0,
        timestamp: crate::audit::now_rfc3339(),
        cluster: request.context.clone(),
        user: None,
        action: action_label(action).to_owned(),
        kind: action_target_kind(action).to_owned(),
        namespace: action_target_namespace(action).map(str::to_owned),
        name: action_target_name(action).to_owned(),
        kubectl: preview_kubectl(&request.context, action),
        success: outcome.is_ok(),
        error: outcome.as_ref().err().map(crate::cluster::sanitize_error),
    }
}

/// Runs a mutating action end to end and records the attempt in `audit`.
///
/// The read-only / protected-context guards are checked before connecting, so a
/// rejection never touches the API server (and never connects to an unopened context);
/// [`execute_action`] re-checks them before writing. Every attempt, including
/// rejections and connection failures, is appended to the audit log. Audit I/O failures
/// are only logged, so the returned result is always the action's own outcome.
pub async fn execute_and_audit(
    manager: &ClusterManager,
    audit: Arc<AuditLogStore>,
    request: &ActionRequest,
    settings: &ContextSettings,
) -> Result<()> {
    let outcome = run_guarded(manager, request, settings).await;

    let mut entry = audit_entry(request, &outcome);
    entry.user = manager.source().context_user(&request.context);
    // A join error means the append panicked; it must not mask the action's result either.
    let _ = tokio::task::spawn_blocking(move || audit.append_or_warn(&entry)).await;

    outcome
}

async fn run_guarded(
    manager: &ClusterManager,
    request: &ActionRequest,
    settings: &ContextSettings,
) -> Result<()> {
    enforce_write_safety(
        &request.context,
        settings,
        action_target_name(&request.action),
        request.confirmed_name.as_deref(),
    )?;
    let client = manager.connect_client(&request.context).await?;
    execute_action(client, request, settings).await
}

/// Generates the exact `kubectl --context <context> ...` command equivalent to `action`.
pub fn preview_kubectl(context: &str, action: &MutatingAction) -> String {
    match action {
        MutatingAction::Delete {
            kind,
            namespace,
            name,
        } => {
            let kind_lower = kind.to_ascii_lowercase();
            match namespace.as_deref().filter(|ns| !ns.is_empty()) {
                Some(ns) => {
                    format!("kubectl --context {context} -n {ns} delete {kind_lower} {name}")
                }
                None => format!("kubectl --context {context} delete {kind_lower} {name}"),
            }
        }
        MutatingAction::Scale {
            kind,
            namespace,
            name,
            replicas,
        } => {
            let kind_lower = kind.to_ascii_lowercase();
            format!(
                "kubectl --context {context} -n {namespace} scale {kind_lower}/{name} --replicas={replicas}"
            )
        }
        MutatingAction::Restart {
            kind,
            namespace,
            name,
        } => {
            let kind_lower = kind.to_ascii_lowercase();
            format!(
                "kubectl --context {context} -n {namespace} rollout restart {kind_lower}/{name}"
            )
        }
        MutatingAction::Cordon { node } => {
            format!("kubectl --context {context} cordon {node}")
        }
        MutatingAction::Uncordon { node } => {
            format!("kubectl --context {context} uncordon {node}")
        }
        MutatingAction::Drain { node } => {
            format!(
                "kubectl --context {context} drain {node} --ignore-daemonsets --delete-emptydir-data"
            )
        }
        MutatingAction::FluxReconcile {
            kind,
            namespace,
            name,
        } => {
            let kind_lower = kind.to_ascii_lowercase();
            format!(
                "kubectl --context {context} -n {namespace} annotate {kind_lower} {name} reconcile.fluxcd.io/requestedAt=\"$(date +%s)\" --overwrite"
            )
        }
        MutatingAction::FluxSuspend {
            kind,
            namespace,
            name,
            suspend,
        } => {
            let kind_lower = kind.to_ascii_lowercase();
            format!(
                "kubectl --context {context} -n {namespace} patch {kind_lower} {name} --type=merge -p '{{\"spec\":{{\"suspend\":{suspend}}}}}'"
            )
        }
        MutatingAction::ArgoRolloutPromote {
            namespace,
            name,
            full,
        } => {
            if *full {
                format!(
                    "kubectl --context {context} -n {namespace} argo rollouts promote {name} --full"
                )
            } else {
                format!("kubectl --context {context} -n {namespace} argo rollouts promote {name}")
            }
        }
        MutatingAction::ArgoRolloutAbort { namespace, name } => {
            format!("kubectl --context {context} -n {namespace} argo rollouts abort {name}")
        }
        MutatingAction::ArgoRolloutRetry { namespace, name } => {
            format!("kubectl --context {context} -n {namespace} argo rollouts retry rollout {name}")
        }
        MutatingAction::ArgoAppSync { namespace, name } => {
            format!(
                "kubectl --context {context} -n {namespace} patch application {name} --type=merge -p '{{\"operation\":{{\"sync\":{{}}}}}}'"
            )
        }
        MutatingAction::CertRenew { namespace, name } => {
            format!("kubectl --context {context} -n {namespace} cert-manager renew {name}")
        }
        MutatingAction::ExternalSecretForceSync { namespace, name } => {
            format!(
                "kubectl --context {context} -n {namespace} annotate externalsecret {name} force-sync=\"$(date +%s)\" --overwrite"
            )
        }
    }
}

/// Enforces `read_only` and `protected` safety guards before any write reaches `kube::Api`.
///
/// - If `settings.read_only` is `true`, returns [`Error::ReadOnlyContext`].
/// - If `settings.protected` is `true` and `confirmed_name != Some(target_name)`,
///   returns [`Error::ConfirmationRequired`].
pub fn enforce_write_safety(
    context: &str,
    settings: &ContextSettings,
    target_name: &str,
    confirmed_name: Option<&str>,
) -> Result<()> {
    if settings.effective_read_only() {
        return Err(Error::ReadOnlyContext {
            context: context.to_owned(),
        });
    }
    if settings.protected && confirmed_name != Some(target_name) {
        return Err(Error::ConfirmationRequired {
            context: context.to_owned(),
            expected_name: target_name.to_owned(),
        });
    }
    Ok(())
}

/// Executes a [`MutatingAction`] after verifying write-safety guards for `request.context`.
pub async fn execute_action(
    client: Client,
    request: &ActionRequest,
    settings: &ContextSettings,
) -> Result<()> {
    let target = action_target_name(&request.action);
    enforce_write_safety(
        &request.context,
        settings,
        target,
        request.confirmed_name.as_deref(),
    )?;

    match &request.action {
        MutatingAction::Delete {
            kind,
            namespace,
            name,
        } => {
            delete_resource(
                client,
                &request.context,
                settings,
                kind,
                namespace.as_deref(),
                name,
                request.confirmed_name.as_deref(),
            )
            .await
        }
        MutatingAction::Scale {
            kind,
            namespace,
            name,
            replicas,
        } => {
            scale_workload(
                client,
                &request.context,
                settings,
                kind,
                namespace,
                name,
                *replicas,
                request.confirmed_name.as_deref(),
            )
            .await
        }
        MutatingAction::Restart {
            kind,
            namespace,
            name,
        } => {
            restart_workload(
                client,
                &request.context,
                settings,
                kind,
                namespace,
                name,
                request.confirmed_name.as_deref(),
            )
            .await
        }
        MutatingAction::Cordon { node } => {
            cordon_node(
                client,
                &request.context,
                settings,
                node,
                request.confirmed_name.as_deref(),
            )
            .await
        }
        MutatingAction::Uncordon { node } => {
            uncordon_node(
                client,
                &request.context,
                settings,
                node,
                request.confirmed_name.as_deref(),
            )
            .await
        }
        MutatingAction::Drain { node } => {
            drain_node(
                client,
                &request.context,
                settings,
                node,
                request.confirmed_name.as_deref(),
            )
            .await
        }
        MutatingAction::FluxReconcile {
            kind,
            namespace,
            name,
        } => {
            execute_flux_reconcile(client, &request.context, settings, kind, namespace, name).await
        }
        MutatingAction::FluxSuspend {
            kind,
            namespace,
            name,
            suspend,
        } => {
            execute_flux_suspend(
                client,
                &request.context,
                settings,
                kind,
                namespace,
                name,
                *suspend,
            )
            .await
        }
        MutatingAction::ArgoRolloutPromote {
            namespace,
            name,
            full,
        } => {
            execute_argo_rollout_promote(client, &request.context, settings, namespace, name, *full)
                .await
        }
        MutatingAction::ArgoRolloutAbort { namespace, name } => {
            execute_argo_rollout_abort(client, &request.context, settings, namespace, name).await
        }
        MutatingAction::ArgoRolloutRetry { namespace, name } => {
            execute_argo_rollout_retry(client, &request.context, settings, namespace, name).await
        }
        MutatingAction::ArgoAppSync { namespace, name } => {
            execute_argo_app_sync(client, &request.context, settings, namespace, name).await
        }
        MutatingAction::CertRenew { namespace, name } => {
            execute_cert_renew(client, &request.context, settings, namespace, name).await
        }
        MutatingAction::ExternalSecretForceSync { namespace, name } => {
            execute_externalsecret_force_sync(client, &request.context, settings, namespace, name)
                .await
        }
    }
}

/// Deletes a resource by kind, optional namespace, and name after checking write safety.
pub async fn delete_resource(
    client: Client,
    context: &str,
    settings: &ContextSettings,
    kind: &str,
    namespace: Option<&str>,
    name: &str,
    confirmed_name: Option<&str>,
) -> Result<()> {
    enforce_write_safety(context, settings, name, confirmed_name)?;
    let dp = DeleteParams::default();
    let ns = namespace.unwrap_or("default");

    match kind.to_ascii_lowercase().as_str() {
        "pod" | "pods" => {
            Api::<Pod>::namespaced(client, ns).delete(name, &dp).await?;
        }
        "deployment" | "deployments" => {
            Api::<Deployment>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "statefulset" | "statefulsets" => {
            Api::<StatefulSet>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "daemonset" | "daemonsets" => {
            Api::<DaemonSet>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "service" | "services" => {
            Api::<Service>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "configmap" | "configmaps" => {
            Api::<ConfigMap>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "secret" | "secrets" => {
            Api::<Secret>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "job" | "jobs" => {
            Api::<Job>::namespaced(client, ns).delete(name, &dp).await?;
        }
        "cronjob" | "cronjobs" => {
            Api::<CronJob>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "ingress" | "ingresses" => {
            Api::<Ingress>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "persistentvolumeclaim" | "persistentvolumeclaims" | "pvc" => {
            Api::<PersistentVolumeClaim>::namespaced(client, ns)
                .delete(name, &dp)
                .await?;
        }
        "namespace" | "namespaces" => {
            Api::<Namespace>::all(client).delete(name, &dp).await?;
        }
        "node" | "nodes" => {
            Api::<Node>::all(client).delete(name, &dp).await?;
        }
        _ => {
            return Err(Error::UnsupportedKind {
                kind: kind.to_string(),
                action: "delete".to_string(),
            });
        }
    }
    Ok(())
}

/// Scales a `Deployment` or `StatefulSet` to `replicas` after checking write safety.
#[allow(clippy::too_many_arguments)]
pub async fn scale_workload(
    client: Client,
    context: &str,
    settings: &ContextSettings,
    kind: &str,
    namespace: &str,
    name: &str,
    replicas: u32,
    confirmed_name: Option<&str>,
) -> Result<()> {
    enforce_write_safety(context, settings, name, confirmed_name)?;
    let patch = Patch::Merge(json!({
        "spec": {
            "replicas": replicas
        }
    }));
    let pp = PatchParams::default();

    match kind.to_ascii_lowercase().as_str() {
        "deployment" | "deployments" => {
            Api::<Deployment>::namespaced(client, namespace)
                .patch(name, &pp, &patch)
                .await?;
        }
        "statefulset" | "statefulsets" => {
            Api::<StatefulSet>::namespaced(client, namespace)
                .patch(name, &pp, &patch)
                .await?;
        }
        _ => {
            return Err(Error::UnsupportedKind {
                kind: kind.to_string(),
                action: "scale".to_string(),
            });
        }
    }
    Ok(())
}

/// Performs a rollout restart on a `Deployment`, `StatefulSet`, or `DaemonSet` by
/// patching `kubectl.kubernetes.io/restartedAt` on the pod template metadata.
pub async fn restart_workload(
    client: Client,
    context: &str,
    settings: &ContextSettings,
    kind: &str,
    namespace: &str,
    name: &str,
    confirmed_name: Option<&str>,
) -> Result<()> {
    enforce_write_safety(context, settings, name, confirmed_name)?;
    // Same RFC 3339 format `kubectl rollout restart` writes.
    let restarted_at = crate::audit::now_rfc3339();
    let patch = Patch::Merge(json!({
        "spec": {
            "template": {
                "metadata": {
                    "annotations": {
                        "kubectl.kubernetes.io/restartedAt": restarted_at
                    }
                }
            }
        }
    }));
    let pp = PatchParams::default();

    match kind.to_ascii_lowercase().as_str() {
        "deployment" | "deployments" => {
            Api::<Deployment>::namespaced(client, namespace)
                .patch(name, &pp, &patch)
                .await?;
        }
        "statefulset" | "statefulsets" => {
            Api::<StatefulSet>::namespaced(client, namespace)
                .patch(name, &pp, &patch)
                .await?;
        }
        "daemonset" | "daemonsets" => {
            Api::<DaemonSet>::namespaced(client, namespace)
                .patch(name, &pp, &patch)
                .await?;
        }
        _ => {
            return Err(Error::UnsupportedKind {
                kind: kind.to_string(),
                action: "restart".to_string(),
            });
        }
    }
    Ok(())
}

/// Cordons `node` (`spec.unschedulable = true`) after checking write safety.
pub async fn cordon_node(
    client: Client,
    context: &str,
    settings: &ContextSettings,
    node: &str,
    confirmed_name: Option<&str>,
) -> Result<()> {
    enforce_write_safety(context, settings, node, confirmed_name)?;
    let patch = Patch::Merge(json!({
        "spec": {
            "unschedulable": true
        }
    }));
    Api::<Node>::all(client)
        .patch(node, &PatchParams::default(), &patch)
        .await?;
    Ok(())
}

/// Uncordons `node` (`spec.unschedulable = false`) after checking write safety.
pub async fn uncordon_node(
    client: Client,
    context: &str,
    settings: &ContextSettings,
    node: &str,
    confirmed_name: Option<&str>,
) -> Result<()> {
    enforce_write_safety(context, settings, node, confirmed_name)?;
    let patch = Patch::Merge(json!({
        "spec": {
            "unschedulable": false
        }
    }));
    Api::<Node>::all(client)
        .patch(node, &PatchParams::default(), &patch)
        .await?;
    Ok(())
}

/// Cordons `node` and deletes non-DaemonSet pods scheduled on it after checking write safety.
pub async fn drain_node(
    client: Client,
    context: &str,
    settings: &ContextSettings,
    node: &str,
    confirmed_name: Option<&str>,
) -> Result<()> {
    enforce_write_safety(context, settings, node, confirmed_name)?;
    cordon_node(client.clone(), context, settings, node, confirmed_name).await?;

    let pods = Api::<Pod>::all(client.clone())
        .list(&ListParams::default().fields(&format!("spec.nodeName={node}")))
        .await?;

    let dp = DeleteParams::default();
    for pod in pods.items {
        let is_daemonset = pod
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| owners.iter().any(|o| o.kind == "DaemonSet"));
        let is_mirror = pod
            .metadata
            .annotations
            .as_ref()
            .is_some_and(|ann| ann.contains_key("kubernetes.io/config.mirror"));
        if is_daemonset || is_mirror {
            continue;
        }
        if let (Some(ns), Some(pod_name)) = (pod.metadata.namespace, pod.metadata.name) {
            Api::<Pod>::namespaced(client.clone(), &ns)
                .delete(&pod_name, &dp)
                .await?;
        }
    }
    Ok(())
}

/// Resolves a pod's top-level workload controller by following `ownerReferences`
/// (e.g. Pod -> ReplicaSet -> Deployment, Pod -> StatefulSet, Pod -> DaemonSet, Pod -> Job -> CronJob).
///
/// Returns:
/// - `Ok(Some(ControllerRef))` if a controller was resolved.
/// - `Ok(None)` if the pod has no owner references (standalone pod).
/// - `Err` if fetching the pod or an intermediate controller fails.
pub async fn resolve_pod_controller(
    client: Client,
    namespace: &str,
    pod_name: &str,
) -> Result<Option<ControllerRef>> {
    let pod = Api::<Pod>::namespaced(client.clone(), namespace)
        .get(pod_name)
        .await?;

    let owners = pod.metadata.owner_references.as_deref().unwrap_or(&[]);
    let controller = owners
        .iter()
        .find(|o| o.controller == Some(true))
        .or_else(|| owners.first());

    let Some(owner) = controller else {
        return Ok(None);
    };

    match owner.kind.as_str() {
        "ReplicaSet" => {
            match Api::<ReplicaSet>::namespaced(client.clone(), namespace)
                .get(&owner.name)
                .await
            {
                Ok(rs) => {
                    let rs_owners = rs.metadata.owner_references.as_deref().unwrap_or(&[]);
                    let parent = rs_owners
                        .iter()
                        .find(|o| o.controller == Some(true))
                        .or_else(|| rs_owners.first());
                    if let Some(parent) = parent {
                        Ok(Some(ControllerRef {
                            kind: parent.kind.clone(),
                            name: parent.name.clone(),
                            namespace: namespace.to_string(),
                        }))
                    } else {
                        Ok(Some(ControllerRef {
                            kind: "ReplicaSet".to_string(),
                            name: owner.name.clone(),
                            namespace: namespace.to_string(),
                        }))
                    }
                }
                Err(_) => Ok(Some(ControllerRef {
                    kind: "ReplicaSet".to_string(),
                    name: owner.name.clone(),
                    namespace: namespace.to_string(),
                })),
            }
        }
        "Job" => {
            match Api::<Job>::namespaced(client.clone(), namespace)
                .get(&owner.name)
                .await
            {
                Ok(job) => {
                    let job_owners = job.metadata.owner_references.as_deref().unwrap_or(&[]);
                    let parent = job_owners
                        .iter()
                        .find(|o| o.controller == Some(true))
                        .or_else(|| job_owners.first());
                    if let Some(parent) = parent {
                        Ok(Some(ControllerRef {
                            kind: parent.kind.clone(),
                            name: parent.name.clone(),
                            namespace: namespace.to_string(),
                        }))
                    } else {
                        Ok(Some(ControllerRef {
                            kind: "Job".to_string(),
                            name: owner.name.clone(),
                            namespace: namespace.to_string(),
                        }))
                    }
                }
                Err(_) => Ok(Some(ControllerRef {
                    kind: "Job".to_string(),
                    name: owner.name.clone(),
                    namespace: namespace.to_string(),
                })),
            }
        }
        other => Ok(Some(ControllerRef {
            kind: other.to_string(),
            name: owner.name.clone(),
            namespace: namespace.to_string(),
        })),
    }
}

pub(crate) async fn resolve_ecosystem_api_resource(
    client: &Client,
    kind: &str,
) -> Result<kube::api::ApiResource> {
    if let Ok(ar) = crate::detail::resolve_api_resource(client, kind).await {
        return Ok(ar);
    }
    let (group, version, plural) = match kind {
        "Kustomization" => ("kustomize.toolkit.fluxcd.io", "v1", "kustomizations"),
        "HelmRelease" => ("helm.toolkit.fluxcd.io", "v2", "helmreleases"),
        "Rollout" => ("argoproj.io", "v1alpha1", "rollouts"),
        "AnalysisRun" => ("argoproj.io", "v1alpha1", "analysisruns"),
        "Application" => ("argoproj.io", "v1alpha1", "applications"),
        "AppProject" => ("argoproj.io", "v1alpha1", "appprojects"),
        "Certificate" => ("cert-manager.io", "v1", "certificates"),
        "CertificateRequest" => ("cert-manager.io", "v1", "certificaterequests"),
        "Issuer" => ("cert-manager.io", "v1", "issuers"),
        "ClusterIssuer" => ("cert-manager.io", "v1", "clusterissuers"),
        "ExternalSecret" => ("external-secrets.io", "v1beta1", "externalsecrets"),
        "SecretStore" => ("external-secrets.io", "v1beta1", "secretstores"),
        "ClusterSecretStore" => ("external-secrets.io", "v1beta1", "clustersecretstores"),
        _ => {
            return Err(Error::Kube(kube::Error::Api(Box::new(
                kube::core::Status {
                    message: format!("resource kind '{kind}' not found in cluster API"),
                    reason: "NotFound".to_owned(),
                    code: 404,
                    ..Default::default()
                },
            ))));
        }
    };
    Ok(kube::api::ApiResource {
        group: group.to_owned(),
        version: version.to_owned(),
        api_version: format!("{group}/{version}"),
        kind: kind.to_owned(),
        plural: plural.to_owned(),
    })
}

async fn execute_flux_reconcile(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    kind: &str,
    namespace: &str,
    name: &str,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, kind).await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    let patch = json!({
        "metadata": {
            "annotations": {
                "reconcile.fluxcd.io/requestedAt": crate::audit::now_rfc3339()
            }
        }
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

async fn execute_flux_suspend(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    kind: &str,
    namespace: &str,
    name: &str,
    suspend: bool,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, kind).await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    let patch = json!({
        "spec": {
            "suspend": suspend
        }
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

async fn execute_argo_rollout_promote(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    namespace: &str,
    name: &str,
    full: bool,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, "Rollout").await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    if full {
        let patch = json!({
            "status": {
                "promoteFull": true
            }
        });
        if api
            .patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .is_err()
        {
            let spec_patch = json!({
                "spec": {
                    "paused": false
                }
            });
            api.patch(name, &PatchParams::default(), &Patch::Merge(&spec_patch))
                .await
                .map_err(Error::Kube)?;
        }
    } else {
        let patch = json!({
            "status": {
                "pauseConditions": null
            }
        });
        if api
            .patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .is_err()
        {
            let spec_patch = json!({
                "spec": {
                    "paused": false
                }
            });
            api.patch(name, &PatchParams::default(), &Patch::Merge(&spec_patch))
                .await
                .map_err(Error::Kube)?;
        }
    }
    Ok(())
}

async fn execute_argo_rollout_abort(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    namespace: &str,
    name: &str,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, "Rollout").await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    let patch = json!({
        "status": {
            "abort": true
        }
    });
    if api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .is_err()
    {
        let spec_patch = json!({
            "spec": {
                "paused": true
            }
        });
        api.patch(name, &PatchParams::default(), &Patch::Merge(&spec_patch))
            .await
            .map_err(Error::Kube)?;
    }
    Ok(())
}

async fn execute_argo_rollout_retry(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    namespace: &str,
    name: &str,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, "Rollout").await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    let patch = json!({
        "status": {
            "abort": false
        }
    });
    if api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .is_err()
    {
        let spec_patch = json!({
            "spec": {
                "paused": false
            }
        });
        api.patch(name, &PatchParams::default(), &Patch::Merge(&spec_patch))
            .await
            .map_err(Error::Kube)?;
    }
    Ok(())
}

async fn execute_argo_app_sync(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    namespace: &str,
    name: &str,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, "Application").await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    let patch = json!({
        "operation": {
            "sync": {
                "prune": true
            }
        }
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

async fn execute_cert_renew(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    namespace: &str,
    name: &str,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, "Certificate").await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    let now = crate::audit::now_rfc3339();
    let status_patch = json!({
        "status": {
            "conditions": [
                {
                    "type": "Issuing",
                    "status": "True",
                    "reason": "ManuallyTriggered",
                    "message": "Certificate renewal requested via Fleetglass",
                    "lastTransitionTime": now
                }
            ]
        }
    });
    let _ = api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(&status_patch))
        .await;
    let annot_patch = json!({
        "metadata": {
            "annotations": {
                "cert-manager.io/requested-at": now
            }
        }
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&annot_patch))
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

async fn execute_externalsecret_force_sync(
    client: Client,
    _context: &str,
    _settings: &ContextSettings,
    namespace: &str,
    name: &str,
) -> Result<()> {
    let ar = resolve_ecosystem_api_resource(&client, "ExternalSecret").await?;
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client, namespace, &ar);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    let patch = json!({
        "metadata": {
            "annotations": {
                "force-sync": now_secs
            }
        }
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_kubectl_formats_all_six_actions() {
        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::Delete {
                    kind: "Pod".into(),
                    namespace: Some("payments".into()),
                    name: "api-7f8b9".into(),
                }
            ),
            "kubectl --context prod-eu -n payments delete pod api-7f8b9"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::Delete {
                    kind: "Namespace".into(),
                    namespace: None,
                    name: "sandbox".into(),
                }
            ),
            "kubectl --context prod-eu delete namespace sandbox"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::Scale {
                    kind: "Deployment".into(),
                    namespace: "payments".into(),
                    name: "checkout".into(),
                    replicas: 5,
                }
            ),
            "kubectl --context prod-eu -n payments scale deployment/checkout --replicas=5"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::Restart {
                    kind: "Deployment".into(),
                    namespace: "payments".into(),
                    name: "checkout".into(),
                }
            ),
            "kubectl --context prod-eu -n payments rollout restart deployment/checkout"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::Cordon {
                    node: "worker-01".into(),
                }
            ),
            "kubectl --context prod-eu cordon worker-01"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::Uncordon {
                    node: "worker-01".into(),
                }
            ),
            "kubectl --context prod-eu uncordon worker-01"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::Drain {
                    node: "worker-01".into(),
                }
            ),
            "kubectl --context prod-eu drain worker-01 --ignore-daemonsets --delete-emptydir-data"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::FluxReconcile {
                    kind: "Kustomization".into(),
                    namespace: "flux-system".into(),
                    name: "infra".into(),
                }
            ),
            "kubectl --context prod-eu -n flux-system annotate kustomization infra reconcile.fluxcd.io/requestedAt=\"$(date +%s)\" --overwrite"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::FluxSuspend {
                    kind: "HelmRelease".into(),
                    namespace: "apps".into(),
                    name: "web".into(),
                    suspend: true,
                }
            ),
            "kubectl --context prod-eu -n apps patch helmrelease web --type=merge -p '{\"spec\":{\"suspend\":true}}'"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::ArgoRolloutPromote {
                    namespace: "apps".into(),
                    name: "web-ro".into(),
                    full: false,
                }
            ),
            "kubectl --context prod-eu -n apps argo rollouts promote web-ro"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::ArgoRolloutPromote {
                    namespace: "apps".into(),
                    name: "web-ro".into(),
                    full: true,
                }
            ),
            "kubectl --context prod-eu -n apps argo rollouts promote web-ro --full"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::ArgoRolloutAbort {
                    namespace: "apps".into(),
                    name: "web-ro".into(),
                }
            ),
            "kubectl --context prod-eu -n apps argo rollouts abort web-ro"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::ArgoRolloutRetry {
                    namespace: "apps".into(),
                    name: "web-ro".into(),
                }
            ),
            "kubectl --context prod-eu -n apps argo rollouts retry rollout web-ro"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::ArgoAppSync {
                    namespace: "argocd".into(),
                    name: "guestbook".into(),
                }
            ),
            "kubectl --context prod-eu -n argocd patch application guestbook --type=merge -p '{\"operation\":{\"sync\":{}}}'"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::CertRenew {
                    namespace: "ingress".into(),
                    name: "tls-cert".into(),
                }
            ),
            "kubectl --context prod-eu -n ingress cert-manager renew tls-cert"
        );

        assert_eq!(
            preview_kubectl(
                "prod-eu",
                &MutatingAction::ExternalSecretForceSync {
                    namespace: "backend".into(),
                    name: "db-pass".into(),
                }
            ),
            "kubectl --context prod-eu -n backend annotate externalsecret db-pass force-sync=\"$(date +%s)\" --overwrite"
        );
    }

    #[test]
    fn enforce_write_safety_rejects_read_only_context() {
        let settings = ContextSettings {
            read_only: true,
            protected: false,
            ..ContextSettings::default()
        };
        let err =
            enforce_write_safety("prod-eu", &settings, "api-pod", Some("api-pod")).unwrap_err();
        assert!(matches!(
            err,
            Error::ReadOnlyContext { ref context } if context == "prod-eu"
        ));
    }

    #[test]
    fn enforce_write_safety_requires_exact_confirmation_on_protected_context() {
        let settings = ContextSettings {
            read_only: false,
            protected: true,
            ..ContextSettings::default()
        };

        let missing = enforce_write_safety("prod-eu", &settings, "checkout", None).unwrap_err();
        assert!(matches!(
            missing,
            Error::ConfirmationRequired {
                ref context,
                ref expected_name
            } if context == "prod-eu" && expected_name == "checkout"
        ));

        let wrong =
            enforce_write_safety("prod-eu", &settings, "checkout", Some("wrong-name")).unwrap_err();
        assert!(matches!(
            wrong,
            Error::ConfirmationRequired {
                ref context,
                ref expected_name
            } if context == "prod-eu" && expected_name == "checkout"
        ));

        enforce_write_safety("prod-eu", &settings, "checkout", Some("checkout")).unwrap();
    }

    #[test]
    fn audit_entry_records_target_and_outcome() {
        let request = ActionRequest {
            context: "prod-eu".into(),
            action: MutatingAction::Scale {
                kind: "Deployment".into(),
                namespace: "payments".into(),
                name: "checkout".into(),
                replicas: 1,
            },
            confirmed_name: Some("checkout".into()),
        };
        let ok = audit_entry(&request, &Ok(()));
        assert_eq!(ok.cluster, "prod-eu");
        assert_eq!(ok.action, "scale");
        assert_eq!(ok.kind, "Deployment");
        assert_eq!(ok.namespace.as_deref(), Some("payments"));
        assert_eq!(ok.name, "checkout");
        assert_eq!(
            ok.kubectl,
            "kubectl --context prod-eu -n payments scale deployment/checkout --replicas=1"
        );
        assert!(ok.success);
        assert_eq!(ok.error, None);
        assert!(ok.timestamp.ends_with('Z'));

        let cordon = ActionRequest {
            context: "prod-eu".into(),
            action: MutatingAction::Cordon {
                node: "worker-01".into(),
            },
            confirmed_name: None,
        };
        let err = Err(Error::ReadOnlyContext {
            context: "prod-eu".into(),
        });
        let failed = audit_entry(&cordon, &err);
        assert_eq!(failed.action, "cordon");
        assert_eq!(failed.kind, "Node");
        assert_eq!(failed.namespace, None);
        assert_eq!(failed.name, "worker-01");
        assert!(!failed.success);
        assert!(failed.error.as_deref().unwrap().contains("prod-eu"));
    }

    #[test]
    fn audit_entry_redacts_exec_plugin_secrets() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;

        #[cfg(unix)]
        let status = std::process::ExitStatus::from_raw(1 << 8);
        #[cfg(not(unix))]
        let status = std::process::ExitStatus::default();

        let secret = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let auth_err = kube::client::AuthError::AuthExecRun {
            cmd: format!("aws-iam-authenticator token -i cluster --env {secret}"),
            out: Box::new(std::process::Output {
                status,
                stdout: format!("{{\"token\":\"k8s-aws-v1.{secret}\"}}").into_bytes(),
                stderr: format!("error: {secret} was invalid").into_bytes(),
            }),
        };
        let outcome = Err(Error::Kube(kube::Error::Auth(auth_err)));
        // Sanity check: the unsanitised Display really does carry the secret.
        assert!(
            outcome
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("wJalrXUtnFEMI")
        );

        let request = ActionRequest {
            context: "eks-prod".into(),
            action: MutatingAction::Delete {
                kind: "Pod".into(),
                namespace: Some("payments".into()),
                name: "checkout-1".into(),
            },
            confirmed_name: None,
        };
        let entry = audit_entry(&request, &outcome);
        let error = entry.error.expect("failed outcome records an error");
        assert!(!entry.success);
        for leaked in [
            "wJalrXUtnFEMI",
            "AWS_SECRET_ACCESS_KEY",
            "aws-iam-authenticator",
        ] {
            assert!(!error.contains(leaked), "{leaked} leaked: {error}");
        }
        assert!(
            error.contains("authentication failed: exec credential plugin failed"),
            "message should still explain the failure: {error}"
        );
        let line = serde_json::to_string(&AuditEntry {
            error: Some(error),
            ..entry
        })
        .unwrap();
        assert!(!line.contains("wJalrXUtnFEMI"));
    }

    #[tokio::test]
    async fn execute_and_audit_records_guard_rejections_without_connecting() {
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-actions-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
        // No kubeconfig at all: any connect attempt would fail with a kubeconfig error.
        let manager =
            ClusterManager::new(crate::kubeconfig::KubeconfigSource::from_paths(Vec::new()));
        let request = ActionRequest {
            context: "prod-eu".into(),
            action: MutatingAction::Delete {
                kind: "Pod".into(),
                namespace: Some("payments".into()),
                name: "checkout-1".into(),
            },
            confirmed_name: Some("checkout-1".into()),
        };

        let read_only = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };
        let outcome = execute_and_audit(&manager, Arc::clone(&audit), &request, &read_only).await;
        assert!(matches!(outcome, Err(Error::ReadOnlyContext { .. })));

        let protected = ContextSettings {
            protected: true,
            ..ContextSettings::default()
        };
        let unconfirmed = ActionRequest {
            confirmed_name: None,
            ..request.clone()
        };
        let outcome =
            execute_and_audit(&manager, Arc::clone(&audit), &unconfirmed, &protected).await;
        assert!(matches!(outcome, Err(Error::ConfirmationRequired { .. })));

        // Guards pass, connecting fails: still audited, with the connection error.
        let outcome = execute_and_audit(
            &manager,
            Arc::clone(&audit),
            &request,
            &ContextSettings::default(),
        )
        .await;
        assert!(outcome.is_err());

        let page = audit.query(0, 10, None, None).unwrap();
        assert_eq!(page.total, 3);
        assert!(page.entries.iter().all(|e| !e.success && e.error.is_some()));
        let ids: Vec<u32> = page.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![3, 2, 1]);
        assert_eq!(
            page.entries[0].kubectl,
            "kubectl --context prod-eu -n payments delete pod checkout-1"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enforce_write_safety_allows_unrestricted_context_without_confirmation() {
        let settings = ContextSettings::default();
        enforce_write_safety("dev-local", &settings, "any-pod", None).unwrap();
    }

    #[tokio::test]
    async fn execute_and_audit_populates_user_from_kubeconfig_context() {
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-actions-user-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
        let fixture_path = crate::test_fixture("cert-token-basic.yaml");
        let manager = ClusterManager::new(crate::kubeconfig::KubeconfigSource::from_paths(vec![
            fixture_path,
        ]));
        let request = ActionRequest {
            context: "cert".into(),
            action: MutatingAction::Delete {
                kind: "Pod".into(),
                namespace: Some("default".into()),
                name: "test-pod".into(),
            },
            confirmed_name: Some("test-pod".into()),
        };

        let read_only = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };
        let _ = execute_and_audit(&manager, Arc::clone(&audit), &request, &read_only).await;

        let page = audit.query(0, 10, None, None).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].user.as_deref(), Some("cert-user"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unsupported_kinds_return_typed_error() {
        let config = kube::Config::new("https://127.0.0.1:1".parse().expect("valid test uri"));
        let client = Client::try_from(config).expect("client");
        let settings = ContextSettings::default();

        let del_err = delete_resource(
            client.clone(),
            "closed",
            &settings,
            "UnsupportedCustomType",
            Some("default"),
            "my-item",
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            del_err,
            Error::UnsupportedKind { ref kind, ref action } if kind == "UnsupportedCustomType" && action == "delete"
        ));

        let scale_err = scale_workload(
            client.clone(),
            "closed",
            &settings,
            "Pod",
            "default",
            "my-pod",
            2,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            scale_err,
            Error::UnsupportedKind { ref kind, ref action } if kind == "Pod" && action == "scale"
        ));

        let restart_err = restart_workload(
            client, "closed", &settings, "Service", "default", "my-svc", None,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            restart_err,
            Error::UnsupportedKind { ref kind, ref action } if kind == "Service" && action == "restart"
        ));
    }
}
