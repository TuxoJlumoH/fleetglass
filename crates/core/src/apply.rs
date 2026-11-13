//! Server-side dry-run apply and live apply for Kubernetes resources (`R-05`).
//!
//! Core write safety: every mutating action respects the per-context
//! `read_only` flag and requires typing the resource name on `protected` contexts.
//! Writes are executed using Server-Side Apply (`Patch::Apply`) with audit logging.

use std::sync::Arc;

use kube::api::{Api, ApiResource, DynamicObject, Patch, PatchParams};

use crate::actions::enforce_write_safety;
use crate::api::{AuditEntry, ContextSettings};
use crate::audit::AuditLogStore;
use crate::cluster::ClusterManager;
use crate::detail::{
    builtin_api_resource, parse_yaml_or_json, strip_managed_fields, value_to_yaml,
};
use crate::error::{Error, Result};

/// Returns the kubectl command preview for applying a manifest in `context`.
pub fn preview_kubectl_apply(context: &str, namespace: Option<&str>) -> String {
    match namespace.filter(|ns| !ns.is_empty()) {
        Some(ns) => format!("kubectl --context {context} -n {ns} apply -f -"),
        None => format!("kubectl --context {context} apply -f -"),
    }
}

/// Resolves an [`ApiResource`] from its `apiVersion` and `kind`.
pub async fn resolve_target_resource(
    manager: &ClusterManager,
    context: &str,
    api_version: &str,
    kind: &str,
) -> Result<ApiResource> {
    if let Some(ar) = builtin_api_resource(kind)
        && (ar.api_version == api_version || api_version.is_empty())
    {
        return Ok(ar);
    }

    if let Some(ar) = manager
        .discover_api_resource_with_version(context, Some(api_version), kind)
        .await?
    {
        return Ok(ar);
    }

    Err(Error::Kube(kube::Error::Api(Box::new(
        kube::core::Status {
            message: format!("resource kind '{kind}' ({api_version}) not found in cluster API"),
            reason: "NotFound".to_owned(),
            code: 404,
            ..Default::default()
        },
    ))))
}

/// Performs a server-side dry-run apply of `yaml` on `context` and returns the
/// resulting resource representation in cleaned YAML format without modifying
/// cluster state (`R-05`).
///
/// Returns [`Error::ReadOnlyContext`] if `settings.read_only` is true.
pub async fn dry_run_apply(
    manager: &ClusterManager,
    context: &str,
    yaml: &str,
    settings: &ContextSettings,
) -> Result<String> {
    if settings.read_only {
        return Err(Error::ReadOnlyContext {
            context: context.to_owned(),
        });
    }

    let val = parse_yaml_or_json(yaml).ok_or_else(|| {
        Error::Kube(kube::Error::Api(Box::new(kube::core::Status {
            message: "invalid YAML: unable to parse document".to_owned(),
            reason: "BadRequest".to_owned(),
            code: 400,
            ..Default::default()
        })))
    })?;

    let mut obj: DynamicObject = serde_json::from_value(val).map_err(kube::Error::SerdeError)?;

    let name = obj
        .metadata
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            Error::Kube(kube::Error::Api(Box::new(kube::core::Status {
                message: "resource metadata.name is required".to_owned(),
                reason: "BadRequest".to_owned(),
                code: 400,
                ..Default::default()
            })))
        })?;

    let (api_version, kind) = match &obj.types {
        Some(tm) if !tm.kind.is_empty() => (tm.api_version.clone(), tm.kind.clone()),
        _ => {
            return Err(Error::Kube(kube::Error::Api(Box::new(
                kube::core::Status {
                    message: "resource apiVersion and kind are required".to_owned(),
                    reason: "BadRequest".to_owned(),
                    code: 400,
                    ..Default::default()
                },
            ))));
        }
    };

    let ar = resolve_target_resource(manager, context, &api_version, &kind).await?;
    let client = manager.client_for(context).await?;

    let namespace = obj
        .metadata
        .namespace
        .as_deref()
        .filter(|ns| !ns.is_empty());
    let api: Api<DynamicObject> = match namespace {
        Some(ns) => Api::namespaced_with(client, ns, &ar),
        None => Api::all_with(client, &ar),
    };

    obj.metadata.managed_fields = None;
    let params = PatchParams::apply("fleetglass").force().dry_run();
    let mut applied = api
        .patch(&name, &params, &Patch::Apply(&obj))
        .await
        .map_err(Error::Kube)?;

    if applied.types.is_none() {
        applied.types = Some(kube::core::TypeMeta {
            api_version: ar.api_version.clone(),
            kind: ar.kind.clone(),
        });
    }

    let mut result_val = serde_json::to_value(&applied).map_err(kube::Error::SerdeError)?;
    strip_managed_fields(&mut result_val);
    Ok(value_to_yaml(&result_val))
}

/// Applies `yaml` to `context` using Server-Side Apply (`Patch::Apply`) with
/// force ownership, enforcing write-safety guards and auditing the outcome (`R-05`).
///
/// Every attempt (including write-guard rejections and connection errors) is
/// recorded in the audit log.
pub async fn apply_yaml(
    manager: &ClusterManager,
    audit: Arc<AuditLogStore>,
    context: &str,
    yaml: &str,
    confirmed_name: Option<String>,
    settings: &ContextSettings,
) -> Result<String> {
    let val = parse_yaml_or_json(yaml).ok_or_else(|| {
        Error::Kube(kube::Error::Api(Box::new(kube::core::Status {
            message: "invalid YAML: unable to parse document".to_owned(),
            reason: "BadRequest".to_owned(),
            code: 400,
            ..Default::default()
        })))
    })?;

    let mut obj: DynamicObject = serde_json::from_value(val).map_err(kube::Error::SerdeError)?;

    let name = obj
        .metadata
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            Error::Kube(kube::Error::Api(Box::new(kube::core::Status {
                message: "resource metadata.name is required".to_owned(),
                reason: "BadRequest".to_owned(),
                code: 400,
                ..Default::default()
            })))
        })?;

    let (api_version, kind) = match &obj.types {
        Some(tm) if !tm.kind.is_empty() => (tm.api_version.clone(), tm.kind.clone()),
        _ => {
            return Err(Error::Kube(kube::Error::Api(Box::new(
                kube::core::Status {
                    message: "resource apiVersion and kind are required".to_owned(),
                    reason: "BadRequest".to_owned(),
                    code: 400,
                    ..Default::default()
                },
            ))));
        }
    };

    let namespace = obj
        .metadata
        .namespace
        .as_deref()
        .filter(|ns| !ns.is_empty())
        .map(str::to_owned);

    // Enforce write safety guards (read-only and protected confirmation)
    if let Err(guard_err) =
        enforce_write_safety(context, settings, &name, confirmed_name.as_deref())
    {
        let entry = AuditEntry {
            id: 0,
            timestamp: crate::audit::now_rfc3339(),
            cluster: context.to_owned(),
            user: manager.source().context_user(context),
            action: "apply".to_owned(),
            kind: kind.clone(),
            namespace: namespace.clone(),
            name: name.clone(),
            kubectl: preview_kubectl_apply(context, namespace.as_deref()),
            success: false,
            error: Some(crate::cluster::sanitize_error(&guard_err)),
        };
        let _ = tokio::task::spawn_blocking(move || audit.append_or_warn(&entry)).await;
        return Err(guard_err);
    }

    let ar = match resolve_target_resource(manager, context, &api_version, &kind).await {
        Ok(ar) => ar,
        Err(err) => {
            let entry = AuditEntry {
                id: 0,
                timestamp: crate::audit::now_rfc3339(),
                cluster: context.to_owned(),
                user: manager.source().context_user(context),
                action: "apply".to_owned(),
                kind: kind.clone(),
                namespace: namespace.clone(),
                name: name.clone(),
                kubectl: preview_kubectl_apply(context, namespace.as_deref()),
                success: false,
                error: Some(crate::cluster::sanitize_error(&err)),
            };
            let _ = tokio::task::spawn_blocking(move || audit.append_or_warn(&entry)).await;
            return Err(err);
        }
    };

    let client = match manager.client_for(context).await {
        Ok(c) => c,
        Err(err) => {
            let entry = AuditEntry {
                id: 0,
                timestamp: crate::audit::now_rfc3339(),
                cluster: context.to_owned(),
                user: manager.source().context_user(context),
                action: "apply".to_owned(),
                kind: kind.clone(),
                namespace: namespace.clone(),
                name: name.clone(),
                kubectl: preview_kubectl_apply(context, namespace.as_deref()),
                success: false,
                error: Some(crate::cluster::sanitize_error(&err)),
            };
            let _ = tokio::task::spawn_blocking(move || audit.append_or_warn(&entry)).await;
            return Err(err);
        }
    };

    let api: Api<DynamicObject> = match namespace.as_deref() {
        Some(ns) => Api::namespaced_with(client, ns, &ar),
        None => Api::all_with(client, &ar),
    };

    obj.metadata.managed_fields = None;
    let params = PatchParams::apply("fleetglass").force();
    let patch_res = api.patch(&name, &params, &Patch::Apply(&obj)).await;

    let success = patch_res.is_ok();
    let error = patch_res
        .as_ref()
        .err()
        .map(crate::cluster::sanitize_kube_error);

    let entry = AuditEntry {
        id: 0,
        timestamp: crate::audit::now_rfc3339(),
        cluster: context.to_owned(),
        user: manager.source().context_user(context),
        action: "apply".to_owned(),
        kind: kind.clone(),
        namespace: namespace.clone(),
        name: name.clone(),
        kubectl: preview_kubectl_apply(context, namespace.as_deref()),
        success,
        error,
    };
    let _ = tokio::task::spawn_blocking(move || audit.append_or_warn(&entry)).await;

    let mut applied = patch_res.map_err(Error::Kube)?;

    if applied.types.is_none() {
        applied.types = Some(kube::core::TypeMeta {
            api_version: ar.api_version.clone(),
            kind: ar.kind.clone(),
        });
    }

    let mut result_val = serde_json::to_value(&applied).map_err(kube::Error::SerdeError)?;
    strip_managed_fields(&mut result_val);
    Ok(value_to_yaml(&result_val))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kubeconfig::KubeconfigSource;

    #[test]
    fn test_preview_kubectl_apply_formatting() {
        assert_eq!(
            preview_kubectl_apply("prod-eu", Some("default")),
            "kubectl --context prod-eu -n default apply -f -"
        );
        assert_eq!(
            preview_kubectl_apply("prod-eu", None),
            "kubectl --context prod-eu apply -f -"
        );
        assert_eq!(
            preview_kubectl_apply("prod-eu", Some("")),
            "kubectl --context prod-eu apply -f -"
        );
    }

    #[tokio::test]
    async fn test_resolve_builtin_resources() {
        let manager = ClusterManager::new(KubeconfigSource::from_paths(Vec::new()));
        let ar = resolve_target_resource(&manager, "test-ctx", "v1", "ConfigMap")
            .await
            .unwrap();
        assert_eq!(ar.kind, "ConfigMap");
        assert_eq!(ar.plural, "configmaps");
        assert_eq!(ar.api_version, "v1");

        let ar_dep = resolve_target_resource(&manager, "test-ctx", "apps/v1", "Deployment")
            .await
            .unwrap();
        assert_eq!(ar_dep.kind, "Deployment");
        assert_eq!(ar_dep.plural, "deployments");
    }

    #[tokio::test]
    async fn test_dry_run_apply_read_only_guard() {
        let manager = ClusterManager::new(KubeconfigSource::from_paths(Vec::new()));
        let ro_settings = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };
        let yaml = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm1\n";
        let res = dry_run_apply(&manager, "prod", yaml, &ro_settings).await;
        assert!(matches!(res, Err(Error::ReadOnlyContext { context }) if context == "prod"));
    }

    #[tokio::test]
    async fn test_dry_run_apply_invalid_yaml() {
        let manager = ClusterManager::new(KubeconfigSource::from_paths(Vec::new()));
        let settings = ContextSettings::default();
        let res = dry_run_apply(&manager, "dev", "not: valid: yaml: [", &settings).await;
        assert!(res.is_err());

        let res_missing_name = dry_run_apply(
            &manager,
            "dev",
            "apiVersion: v1\nkind: ConfigMap\n",
            &settings,
        )
        .await;
        assert!(res_missing_name.is_err());
    }

    #[tokio::test]
    async fn test_apply_yaml_guards_and_auditing() {
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-apply-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLogStore::new(dir.join("audit.jsonl")));
        let manager = ClusterManager::new(KubeconfigSource::from_paths(Vec::new()));

        let yaml =
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: my-cm\n  namespace: default\n";

        // Read-only rejection
        let ro_settings = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };
        let res_ro = apply_yaml(
            &manager,
            Arc::clone(&audit),
            "prod",
            yaml,
            None,
            &ro_settings,
        )
        .await;
        assert!(matches!(res_ro, Err(Error::ReadOnlyContext { context }) if context == "prod"));

        // Protected confirmation required
        let protected_settings = ContextSettings {
            protected: true,
            ..ContextSettings::default()
        };
        let res_prot = apply_yaml(
            &manager,
            Arc::clone(&audit),
            "prod",
            yaml,
            None,
            &protected_settings,
        )
        .await;
        assert!(
            matches!(res_prot, Err(Error::ConfirmationRequired { expected_name, .. }) if expected_name == "my-cm")
        );

        // Check audit log recorded both rejections
        let page = audit.query(0, 10, Some("prod"), Some("apply")).unwrap();
        assert_eq!(page.total, 2);
        assert!(page.entries.iter().all(|e| !e.success));
        assert_eq!(page.entries[0].name, "my-cm");
        assert_eq!(page.entries[0].kind, "ConfigMap");
        assert_eq!(page.entries[0].namespace.as_deref(), Some("default"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
