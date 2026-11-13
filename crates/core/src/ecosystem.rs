//! Ecosystem CRD row projection and status classification (`P3-03`).
//!
//! Provides specialized projections for Kubernetes ecosystem resources:
//! - Gateway API: `Gateway`, `HTTPRoute`, `GRPCRoute`
//! - Argo CD: `Application`, `AppProject`
//! - Flux: `Kustomization`, `HelmRelease`
//! - Karpenter: `NodePool`, `NodeClaim`
//!
//! Any unrecognized CRD falls back to [`crate::detail::project_dynamic_object`].

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::DynamicObject;
use serde_json::Value;

use crate::api::{Cell, Row, RowStatus};
use crate::detail::project_dynamic_object;

/// Returns true if `kind` is a recognized ecosystem CRD.
#[must_use]
pub fn is_ecosystem_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Gateway"
            | "HTTPRoute"
            | "GRPCRoute"
            | "GatewayClass"
            | "Application"
            | "AppProject"
            | "Rollout"
            | "AnalysisRun"
            | "Kustomization"
            | "HelmRelease"
            | "NodePool"
            | "NodeClaim"
            | "Certificate"
            | "CertificateRequest"
            | "Issuer"
            | "ClusterIssuer"
            | "ExternalSecret"
            | "SecretStore"
            | "ClusterSecretStore"
    )
}

/// Projects a Kubernetes ecosystem [`DynamicObject`] into a compact [`Row`].
///
/// If `kind` is not a recognized ecosystem kind, falls back to
/// [`project_dynamic_object`].
pub fn project_ecosystem_object(obj: &DynamicObject, cluster: &str, kind: &str) -> Row {
    match kind {
        "Gateway" => project_gateway(obj, cluster),
        "HTTPRoute" | "GRPCRoute" => project_gateway_route(obj, cluster, kind),
        "GatewayClass" => project_gateway_class(obj, cluster),
        "Application" => project_argo_application(obj, cluster),
        "AppProject" => project_argo_project(obj, cluster),
        "Rollout" => project_argo_rollout(obj, cluster),
        "AnalysisRun" => project_argo_analysis_run(obj, cluster),
        "Kustomization" | "HelmRelease" => project_flux_resource(obj, cluster, kind),
        "NodePool" | "NodeClaim" => project_karpenter_resource(obj, cluster, kind),
        "Certificate" => project_cert_manager_certificate(obj, cluster),
        "CertificateRequest" => project_cert_manager_certificate_request(obj, cluster),
        "Issuer" | "ClusterIssuer" => project_cert_manager_issuer(obj, cluster, kind),
        "ExternalSecret" => project_external_secret(obj, cluster),
        "SecretStore" | "ClusterSecretStore" => project_secret_store(obj, cluster, kind),
        _ => project_dynamic_object(obj, cluster, kind),
    }
}

fn extract_meta_fields(
    meta: &ObjectMeta,
) -> (
    String,
    String,
    Option<String>,
    Vec<(String, String)>,
    String,
) {
    let uid = meta.uid.clone().unwrap_or_default();
    let name = meta.name.clone().unwrap_or_default();
    let namespace = meta.namespace.clone();
    let mut labels: Vec<(String, String)> = meta
        .labels
        .as_ref()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    labels.sort();
    let created = meta
        .creation_timestamp
        .as_ref()
        .map(|ts| ts.0.to_string())
        .unwrap_or_default();
    (uid, name, namespace, labels, created)
}

struct ConditionView<'a> {
    type_: &'a str,
    status: &'a str,
    reason: Option<&'a str>,
    message: Option<&'a str>,
}

fn parse_conditions(val: &Value) -> Vec<ConditionView<'_>> {
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let type_ = item.get("type").and_then(Value::as_str)?;
            let status = item.get("status").and_then(Value::as_str)?;
            let reason = item.get("reason").and_then(Value::as_str);
            let message = item.get("message").and_then(Value::as_str);
            Some(ConditionView {
                type_,
                status,
                reason,
                message,
            })
        })
        .collect()
}

fn classify_phase_str(phase: &str) -> RowStatus {
    match phase {
        "Running" | "Active" | "Ready" | "Bound" | "Available" | "Healthy" | "Normal" => {
            RowStatus::Healthy
        }
        "Pending" | "Progressing" | "Creating" => RowStatus::Pending,
        "Succeeded" | "Completed" | "Complete" => RowStatus::Completed,
        "Failed" | "Error" => RowStatus::Failed,
        "Degraded" | "Warning" => RowStatus::Degraded,
        "Terminating" => RowStatus::Terminating,
        _ => RowStatus::Unknown,
    }
}

/// Gateway API: `Gateway`.
///
/// Evaluates `status.conditions`:
/// - `Programmed == True` -> Healthy
/// - `Accepted == True` -> Progressing (`RowStatus::Pending`)
/// - else -> Degraded
///
/// Extracts hostnames and addresses as a cell.
fn project_gateway(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else {
        let conditions = obj
            .data
            .get("status")
            .and_then(|s| s.get("conditions"))
            .map(parse_conditions)
            .unwrap_or_default();

        let programmed = conditions
            .iter()
            .find(|c| c.type_ == "Programmed" && c.status == "True");
        let accepted = conditions
            .iter()
            .find(|c| c.type_ == "Accepted" && c.status == "True");

        if let Some(prog) = programmed {
            (
                RowStatus::Healthy,
                prog.reason.unwrap_or("Programmed").to_owned(),
            )
        } else if let Some(acc) = accepted {
            (
                RowStatus::Pending,
                acc.reason.unwrap_or("Accepted").to_owned(),
            )
        } else {
            let false_cond = conditions.iter().find(|c| c.status == "False");
            let reason = false_cond
                .and_then(|c| c.reason.or(c.message))
                .unwrap_or("Degraded")
                .to_owned();
            (RowStatus::Degraded, reason)
        }
    };

    // Extract hostnames and addresses
    let mut parts: Vec<String> = Vec::new();

    // 1. status.addresses
    if let Some(addrs) = obj
        .data
        .get("status")
        .and_then(|s| s.get("addresses"))
        .and_then(Value::as_array)
    {
        for a in addrs {
            if let Some(v) = a.get("value").and_then(Value::as_str) {
                let v_str = v.to_owned();
                if !parts.contains(&v_str) {
                    parts.push(v_str);
                }
            } else if let Some(s) = a.as_str() {
                let s_str = s.to_owned();
                if !parts.contains(&s_str) {
                    parts.push(s_str);
                }
            }
        }
    }

    // 2. spec.addresses fallback/additional
    if let Some(addrs) = obj
        .data
        .get("spec")
        .and_then(|s| s.get("addresses"))
        .and_then(Value::as_array)
    {
        for a in addrs {
            if let Some(v) = a.get("value").and_then(Value::as_str) {
                let v_str = v.to_owned();
                if !parts.contains(&v_str) {
                    parts.push(v_str);
                }
            } else if let Some(s) = a.as_str() {
                let s_str = s.to_owned();
                if !parts.contains(&s_str) {
                    parts.push(s_str);
                }
            }
        }
    }

    // 3. spec.listeners[].hostname
    if let Some(listeners) = obj
        .data
        .get("spec")
        .and_then(|s| s.get("listeners"))
        .and_then(Value::as_array)
    {
        for l in listeners {
            if let Some(h) = l.get("hostname").and_then(Value::as_str) {
                let h_str = h.to_owned();
                if !parts.contains(&h_str) {
                    parts.push(h_str);
                }
            }
        }
    }

    let hostnames_or_addresses = if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(", ")
    };

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(hostnames_or_addresses),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Gateway".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Gateway API: `HTTPRoute`, `GRPCRoute`.
///
/// Evaluates `status.parents[].conditions` or `status.conditions`:
/// - `Accepted == True` -> Healthy
/// - else -> Degraded
///
/// Extracts `parentRefs` as a cell.
fn project_gateway_route(obj: &DynamicObject, cluster: &str, kind: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else {
        let mut accepted = false;
        let mut reason: Option<String> = None;

        if let Some(parents) = obj
            .data
            .get("status")
            .and_then(|s| s.get("parents"))
            .and_then(Value::as_array)
        {
            for parent in parents {
                if let Some(conds) = parent.get("conditions") {
                    let parsed = parse_conditions(conds);
                    if let Some(c) = parsed
                        .iter()
                        .find(|c| c.type_ == "Accepted" && c.status == "True")
                    {
                        accepted = true;
                        reason = c.reason.map(str::to_owned);
                        break;
                    }
                    if reason.is_none() {
                        reason = parsed
                            .iter()
                            .find(|c| c.status == "False")
                            .and_then(|c| c.reason)
                            .map(str::to_owned);
                    }
                }
            }
        }

        if !accepted {
            let top_conditions = obj
                .data
                .get("status")
                .and_then(|s| s.get("conditions"))
                .map(parse_conditions)
                .unwrap_or_default();
            if let Some(c) = top_conditions
                .iter()
                .find(|c| c.type_ == "Accepted" && c.status == "True")
            {
                accepted = true;
                reason = c.reason.map(str::to_owned);
            } else if reason.is_none() {
                reason = top_conditions
                    .iter()
                    .find(|c| c.status == "False")
                    .and_then(|c| c.reason)
                    .map(str::to_owned);
            }
        }

        if accepted {
            (
                RowStatus::Healthy,
                reason.unwrap_or_else(|| "Accepted".to_owned()),
            )
        } else {
            (
                RowStatus::Degraded,
                reason.unwrap_or_else(|| "NotAccepted".to_owned()),
            )
        }
    };

    // Extract parentRefs
    let mut parent_refs = Vec::new();
    if let Some(refs) = obj
        .data
        .get("spec")
        .and_then(|s| s.get("parentRefs"))
        .and_then(Value::as_array)
    {
        for r in refs {
            if let Some(n) = r.get("name").and_then(Value::as_str) {
                let ns = r.get("namespace").and_then(Value::as_str);
                let formatted = match ns {
                    Some(namespace_str) => format!("{namespace_str}/{n}"),
                    None => n.to_owned(),
                };
                if !parent_refs.contains(&formatted) {
                    parent_refs.push(formatted);
                }
            } else if let Some(s) = r.as_str() {
                let s_str = s.to_owned();
                if !parent_refs.contains(&s_str) {
                    parent_refs.push(s_str);
                }
            }
        }
    }

    if parent_refs.is_empty()
        && let Some(parents) = obj
            .data
            .get("status")
            .and_then(|s| s.get("parents"))
            .and_then(Value::as_array)
    {
        for p in parents {
            if let Some(pr) = p.get("parentRef")
                && let Some(n) = pr.get("name").and_then(Value::as_str)
            {
                let ns = pr.get("namespace").and_then(Value::as_str);
                let formatted = match ns {
                    Some(namespace_str) => format!("{namespace_str}/{n}"),
                    None => n.to_owned(),
                };
                if !parent_refs.contains(&formatted) {
                    parent_refs.push(formatted);
                }
            }
        }
    }

    let parent_refs_str = if parent_refs.is_empty() {
        "-".to_owned()
    } else {
        parent_refs.join(", ")
    };

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(parent_refs_str),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Argo CD: `Application`.
///
/// Reads `.status.health.status` ("Healthy" -> Healthy, "Degraded" -> Failed, "Progressing" -> Progressing)
/// and `.status.sync.status` ("Synced", "OutOfSync").
/// Cells include health status and sync status.
fn project_argo_application(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let (status_phase, status_reason, sync_str) = if obj.metadata.deletion_timestamp.is_some() {
        (
            RowStatus::Terminating,
            "Terminating".to_owned(),
            "Unknown".to_owned(),
        )
    } else {
        let health_str = obj
            .data
            .get("status")
            .and_then(|s| s.get("health"))
            .and_then(|h| h.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown");

        let sync = obj
            .data
            .get("status")
            .and_then(|s| s.get("sync"))
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_owned();

        let phase = match health_str {
            "Healthy" => RowStatus::Healthy,
            "Degraded" => RowStatus::Failed,
            "Progressing" => RowStatus::Pending,
            "Suspended" => RowStatus::Degraded,
            "Missing" => RowStatus::Failed,
            "Unknown" => RowStatus::Unknown,
            other => classify_phase_str(other),
        };

        (phase, health_str.to_owned(), sync)
    };

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(sync_str),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Application".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Argo CD: `AppProject`.
fn project_argo_project(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else {
        (RowStatus::Healthy, "Active".to_owned())
    };

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "AppProject".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Flux: `Kustomization`, `HelmRelease`.
///
/// Evaluates `.spec.suspend` (if true -> Degraded / "Suspended")
/// and `.status.conditions` (`type == "Ready"`, `status == "True"` -> Healthy, else Failed).
/// Cells include Ready condition reason and suspension flag.
fn project_flux_resource(obj: &DynamicObject, cluster: &str, kind: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let suspended = obj
        .data
        .get("spec")
        .and_then(|s| s.get("suspend"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let conditions = obj
        .data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .map(parse_conditions)
        .unwrap_or_default();

    let ready_cond = conditions.iter().find(|c| c.type_ == "Ready");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if suspended {
        (RowStatus::Degraded, "Suspended".to_owned())
    } else if let Some(ready) = ready_cond {
        if ready.status == "True" {
            (
                RowStatus::Healthy,
                ready.reason.unwrap_or("Ready").to_owned(),
            )
        } else {
            (
                RowStatus::Failed,
                ready.reason.unwrap_or("NotReady").to_owned(),
            )
        }
    } else {
        (RowStatus::Failed, "NotReady".to_owned())
    };

    let ready_reason = ready_cond.and_then(|c| c.reason).unwrap_or("-").to_owned();
    let suspension_flag = if suspended { "true" } else { "false" }.to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(ready_reason),
        Cell::Text(suspension_flag),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Karpenter: `NodePool`, `NodeClaim`.
///
/// Evaluates `.status.conditions` (`Ready == True` -> Healthy, else Progressing/Degraded).
fn project_karpenter_resource(obj: &DynamicObject, cluster: &str, kind: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let conditions = obj
        .data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .map(parse_conditions)
        .unwrap_or_default();

    let ready_cond = conditions.iter().find(|c| c.type_ == "Ready");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if let Some(ready) = ready_cond {
        if ready.status == "True" {
            (
                RowStatus::Healthy,
                ready.reason.unwrap_or("Ready").to_owned(),
            )
        } else if ready.status == "Unknown"
            || ready
                .reason
                .is_some_and(|r| r == "Pending" || r == "Progressing" || r == "Creating")
        {
            (
                RowStatus::Pending,
                ready.reason.unwrap_or("Progressing").to_owned(),
            )
        } else {
            (
                RowStatus::Degraded,
                ready.reason.unwrap_or("NotReady").to_owned(),
            )
        }
    } else {
        (RowStatus::Degraded, "NotReady".to_owned())
    };

    let ready_reason = ready_cond.and_then(|c| c.reason).unwrap_or("-").to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(ready_reason),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Gateway API: `GatewayClass`.
///
/// Evaluates `status.conditions`: `Accepted == True` -> Healthy, else Degraded.
/// Cells: Status, Controller (`spec.controllerName`), Age.
fn project_gateway_class(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else {
        let conditions = obj
            .data
            .get("status")
            .and_then(|s| s.get("conditions"))
            .map(parse_conditions)
            .unwrap_or_default();

        let accepted = conditions
            .iter()
            .find(|c| c.type_ == "Accepted" && c.status == "True");

        if let Some(acc) = accepted {
            (
                RowStatus::Healthy,
                acc.reason.unwrap_or("Accepted").to_owned(),
            )
        } else {
            let reason = conditions
                .iter()
                .find(|c| c.type_ == "Accepted" && c.status == "False")
                .and_then(|c| c.reason)
                .unwrap_or("NotAccepted")
                .to_owned();
            (RowStatus::Degraded, reason)
        }
    };

    let controller = obj
        .data
        .get("spec")
        .and_then(|s| s.get("controllerName"))
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(controller),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "GatewayClass".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// cert-manager: `Certificate`.
///
/// Evaluates conditions (`Ready == True` -> Healthy, `Issuing == True` -> Progressing, else Degraded/Failed).
/// Cells: Status, Ready (`True`/`False`), Secret (`spec.secretName`), Issuer (`spec.issuerRef.name`), NotAfter (`status.notAfter`), Age.
fn project_cert_manager_certificate(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let conditions = obj
        .data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .map(parse_conditions)
        .unwrap_or_default();

    let ready_cond = conditions.iter().find(|c| c.type_ == "Ready");
    let issuing_cond = conditions.iter().find(|c| c.type_ == "Issuing");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if let Some(ready) = ready_cond {
        if ready.status == "True" {
            (
                RowStatus::Healthy,
                ready.reason.unwrap_or("Ready").to_owned(),
            )
        } else if let Some(issuing) = issuing_cond
            && issuing.status == "True"
        {
            (
                RowStatus::Pending,
                issuing.reason.unwrap_or("Issuing").to_owned(),
            )
        } else {
            (
                RowStatus::Failed,
                ready.reason.unwrap_or("NotReady").to_owned(),
            )
        }
    } else if let Some(issuing) = issuing_cond
        && issuing.status == "True"
    {
        (
            RowStatus::Pending,
            issuing.reason.unwrap_or("Issuing").to_owned(),
        )
    } else {
        (RowStatus::Pending, "Pending".to_owned())
    };

    let ready_str = ready_cond.map(|c| c.status).unwrap_or("-").to_owned();

    let secret_name = obj
        .data
        .get("spec")
        .and_then(|s| s.get("secretName"))
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_owned();

    let issuer_name = obj
        .data
        .get("spec")
        .and_then(|s| s.get("issuerRef"))
        .and_then(|r| r.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_owned();

    let not_after = obj
        .data
        .get("status")
        .and_then(|s| s.get("notAfter"))
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(ready_str),
        Cell::Text(secret_name),
        Cell::Text(issuer_name),
        Cell::Text(not_after),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Certificate".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// cert-manager: `CertificateRequest`.
///
/// Evaluates conditions (`Denied == True` -> Failed, `Ready == True` -> Healthy, `Approved == True` -> Pending).
/// Cells: Status, Approved (`True`/`False`), Ready (`True`/`False`), Issuer, Age.
fn project_cert_manager_certificate_request(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let conditions = obj
        .data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .map(parse_conditions)
        .unwrap_or_default();

    let ready_cond = conditions.iter().find(|c| c.type_ == "Ready");
    let approved_cond = conditions.iter().find(|c| c.type_ == "Approved");
    let denied_cond = conditions.iter().find(|c| c.type_ == "Denied");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if let Some(denied) = denied_cond
        && denied.status == "True"
    {
        (
            RowStatus::Failed,
            denied.reason.unwrap_or("Denied").to_owned(),
        )
    } else if let Some(ready) = ready_cond {
        if ready.status == "True" {
            (
                RowStatus::Healthy,
                ready.reason.unwrap_or("Issued").to_owned(),
            )
        } else {
            (
                RowStatus::Failed,
                ready.reason.unwrap_or("NotReady").to_owned(),
            )
        }
    } else if let Some(approved) = approved_cond
        && approved.status == "True"
    {
        (
            RowStatus::Pending,
            approved.reason.unwrap_or("Approved").to_owned(),
        )
    } else {
        (RowStatus::Pending, "Pending".to_owned())
    };

    let approved_str = approved_cond.map(|c| c.status).unwrap_or("-").to_owned();
    let ready_str = ready_cond.map(|c| c.status).unwrap_or("-").to_owned();

    let issuer_name = obj
        .data
        .get("spec")
        .and_then(|s| s.get("issuerRef"))
        .and_then(|r| r.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(approved_str),
        Cell::Text(ready_str),
        Cell::Text(issuer_name),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "CertificateRequest".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// cert-manager: `Issuer`, `ClusterIssuer`.
///
/// Evaluates condition `Ready == True` -> Healthy.
/// Cells: Status, Ready (`True`/`False`), Type (e.g. "ACME", "CA", "Vault", "SelfSigned"), Age.
fn project_cert_manager_issuer(obj: &DynamicObject, cluster: &str, kind: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let conditions = obj
        .data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .map(parse_conditions)
        .unwrap_or_default();

    let ready_cond = conditions.iter().find(|c| c.type_ == "Ready");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if let Some(ready) = ready_cond {
        if ready.status == "True" {
            (
                RowStatus::Healthy,
                ready.reason.unwrap_or("Ready").to_owned(),
            )
        } else {
            (
                RowStatus::Failed,
                ready.reason.unwrap_or("NotReady").to_owned(),
            )
        }
    } else {
        (RowStatus::Unknown, "Unknown".to_owned())
    };

    let ready_str = ready_cond.map(|c| c.status).unwrap_or("-").to_owned();

    let issuer_type = obj
        .data
        .get("spec")
        .and_then(|s| s.as_object())
        .and_then(|spec_map| {
            if spec_map.contains_key("acme") {
                Some("ACME")
            } else if spec_map.contains_key("ca") {
                Some("CA")
            } else if spec_map.contains_key("vault") {
                Some("Vault")
            } else if spec_map.contains_key("selfSigned") {
                Some("SelfSigned")
            } else if spec_map.contains_key("venafi") {
                Some("Venafi")
            } else {
                spec_map.keys().next().map(|s| s.as_str())
            }
        })
        .unwrap_or("-")
        .to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(ready_str),
        Cell::Text(issuer_type),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// External Secrets: `ExternalSecret`.
///
/// Evaluates condition `Ready == True` -> Healthy, else Failed.
/// Cells: Status, Store (`spec.secretStoreRef.name`), Target (`spec.target.name`), Refresh (`status.refreshTime`), Age.
fn project_external_secret(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let conditions = obj
        .data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .map(parse_conditions)
        .unwrap_or_default();

    let ready_cond = conditions.iter().find(|c| c.type_ == "Ready");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if let Some(ready) = ready_cond {
        if ready.status == "True" {
            (
                RowStatus::Healthy,
                ready.reason.unwrap_or("Ready").to_owned(),
            )
        } else {
            (
                RowStatus::Failed,
                ready.reason.unwrap_or("SecretSyncedError").to_owned(),
            )
        }
    } else {
        (RowStatus::Pending, "Pending".to_owned())
    };

    let store_name = obj
        .data
        .get("spec")
        .and_then(|s| s.get("secretStoreRef"))
        .and_then(|r| r.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_owned();

    let target_name = obj
        .data
        .get("spec")
        .and_then(|s| s.get("target"))
        .and_then(|t| t.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(if name.is_empty() { "-" } else { &name })
        .to_owned();

    let refresh_time = obj
        .data
        .get("status")
        .and_then(|s| s.get("refreshTime"))
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(store_name),
        Cell::Text(target_name),
        Cell::Text(refresh_time),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "ExternalSecret".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// External Secrets: `SecretStore`, `ClusterSecretStore`.
///
/// Evaluates condition `Ready == True` -> Healthy.
/// Cells: Status, Provider (e.g. AWS, Vault, GCP, Azure), Age.
fn project_secret_store(obj: &DynamicObject, cluster: &str, kind: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let conditions = obj
        .data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .map(parse_conditions)
        .unwrap_or_default();

    let ready_cond = conditions.iter().find(|c| c.type_ == "Ready");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if let Some(ready) = ready_cond {
        if ready.status == "True" {
            (
                RowStatus::Healthy,
                ready.reason.unwrap_or("Ready").to_owned(),
            )
        } else {
            (
                RowStatus::Failed,
                ready.reason.unwrap_or("NotReady").to_owned(),
            )
        }
    } else {
        (RowStatus::Pending, "Pending".to_owned())
    };

    let provider = obj
        .data
        .get("spec")
        .and_then(|s| s.get("provider"))
        .and_then(|p| p.as_object())
        .and_then(|m| m.keys().next().map(|k| k.as_str()))
        .unwrap_or("-")
        .to_owned();

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(provider),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Argo Rollouts: `Rollout`.
///
/// Evaluates abort flag, pause conditions, and phase ("Healthy", "Degraded", "Progressing", "Paused").
/// Cells: Status, Strategy ("canary" / "blueGreen"), Step (e.g. "2/5"), Replicas (e.g. "3/3"), Age.
fn project_argo_rollout(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let is_aborted = obj
        .data
        .get("status")
        .and_then(|s| s.get("abort"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let is_paused = obj
        .data
        .get("status")
        .and_then(|s| s.get("pauseConditions"))
        .and_then(Value::as_array)
        .map(|arr| !arr.is_empty())
        .unwrap_or(false);

    let phase_str = obj
        .data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str);

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else if is_aborted {
        (RowStatus::Failed, "Aborted".to_owned())
    } else if is_paused {
        (RowStatus::Pending, "Paused".to_owned())
    } else if let Some(p) = phase_str {
        match p {
            "Healthy" => (RowStatus::Healthy, "Healthy".to_owned()),
            "Degraded" => (RowStatus::Failed, "Degraded".to_owned()),
            "Progressing" => (RowStatus::Pending, "Progressing".to_owned()),
            "Paused" => (RowStatus::Pending, "Paused".to_owned()),
            other => (classify_phase_str(other), other.to_owned()),
        }
    } else {
        (RowStatus::Healthy, "Active".to_owned())
    };

    let strategy = if obj
        .data
        .get("spec")
        .and_then(|s| s.get("strategy"))
        .and_then(|st| st.get("blueGreen"))
        .is_some()
    {
        "blueGreen".to_owned()
    } else {
        "canary".to_owned()
    };

    let total_steps = obj
        .data
        .get("spec")
        .and_then(|s| s.get("strategy"))
        .and_then(|st| st.get("canary"))
        .and_then(|c| c.get("steps"))
        .and_then(Value::as_array)
        .map(|arr| arr.len());

    let current_step = obj
        .data
        .get("status")
        .and_then(|s| s.get("currentStepIndex"))
        .and_then(Value::as_u64);

    let step_str = match (current_step, total_steps) {
        (Some(idx), Some(total)) => format!("{}/{}", idx + 1, total),
        (None, Some(total)) => format!("0/{total}"),
        _ => "-".to_owned(),
    };

    let desired_replicas = obj
        .data
        .get("spec")
        .and_then(|s| s.get("replicas"))
        .and_then(Value::as_i64)
        .unwrap_or(1);

    let ready_replicas = obj
        .data
        .get("status")
        .and_then(|s| s.get("readyReplicas"))
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let replicas_str = format!("{ready_replicas}/{desired_replicas}");

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(strategy),
        Cell::Text(step_str),
        Cell::Text(replicas_str),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Rollout".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Argo Rollouts: `AnalysisRun`.
///
/// Evaluates `status.phase`: "Successful" -> Healthy, "Running" -> Pending, "Failed"|"Error" -> Failed.
/// Cells: Status, Phase, Age.
fn project_argo_analysis_run(obj: &DynamicObject, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let phase_str = obj
        .data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str)
        .unwrap_or("Pending");

    let (status_phase, status_reason) = if obj.metadata.deletion_timestamp.is_some() {
        (RowStatus::Terminating, "Terminating".to_owned())
    } else {
        match phase_str {
            "Successful" => (RowStatus::Healthy, "Successful".to_owned()),
            "Running" => (RowStatus::Pending, "Running".to_owned()),
            "Failed" => (RowStatus::Failed, "Failed".to_owned()),
            "Error" => (RowStatus::Failed, "Error".to_owned()),
            "Inconclusive" => (RowStatus::Degraded, "Inconclusive".to_owned()),
            other => (classify_phase_str(other), other.to_owned()),
        }
    };

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Text(phase_str.to_owned()),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "AnalysisRun".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_programmed_true_is_healthy() {
        let val = serde_json::json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "metadata": {
                "name": "prod-gw",
                "namespace": "gateway-system",
                "uid": "gw-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "gatewayClassName": "eg",
                "listeners": [
                    {
                        "name": "https",
                        "hostname": "example.com",
                        "port": 443,
                        "protocol": "HTTPS"
                    }
                ]
            },
            "status": {
                "conditions": [
                    {
                        "type": "Programmed",
                        "status": "True",
                        "reason": "Programmed",
                        "message": "Gateway programmed"
                    },
                    {
                        "type": "Accepted",
                        "status": "True",
                        "reason": "Accepted"
                    }
                ],
                "addresses": [
                    {
                        "type": "IPAddress",
                        "value": "203.0.113.1"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Gateway");

        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Programmed");
        assert_eq!(row.name, "prod-gw");
        assert_eq!(row.namespace.as_deref(), Some("gateway-system"));
        assert_eq!(row.custom, Vec::<Cell>::new());

        // Hostnames/addresses cell
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s.contains("203.0.113.1") && s.contains("example.com"),
            _ => false,
        }));
    }

    #[test]
    fn gateway_accepted_only_is_progressing() {
        let val = serde_json::json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "metadata": {
                "name": "staging-gw",
                "uid": "gw-2"
            },
            "status": {
                "conditions": [
                    {
                        "type": "Accepted",
                        "status": "True",
                        "reason": "Accepted"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Gateway");

        assert_eq!(row.status, RowStatus::Pending);
        assert_eq!(row.status_reason, "Accepted");
    }

    #[test]
    fn gateway_not_programmed_or_accepted_is_degraded() {
        let val = serde_json::json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "metadata": {
                "name": "failing-gw",
                "uid": "gw-3"
            },
            "status": {
                "conditions": [
                    {
                        "type": "Programmed",
                        "status": "False",
                        "reason": "AddressNotAssigned"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Gateway");

        assert_eq!(row.status, RowStatus::Degraded);
        assert_eq!(row.status_reason, "AddressNotAssigned");
    }

    #[test]
    fn httproute_accepted_true_is_healthy() {
        let val = serde_json::json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "HTTPRoute",
            "metadata": {
                "name": "web-route",
                "namespace": "default",
                "uid": "route-1"
            },
            "spec": {
                "parentRefs": [
                    {
                        "name": "prod-gw",
                        "namespace": "gateway-system"
                    }
                ]
            },
            "status": {
                "parents": [
                    {
                        "parentRef": {
                            "name": "prod-gw",
                            "namespace": "gateway-system"
                        },
                        "conditions": [
                            {
                                "type": "Accepted",
                                "status": "True",
                                "reason": "Accepted"
                            }
                        ]
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "HTTPRoute");

        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Accepted");
        assert_eq!(row.custom, Vec::<Cell>::new());

        // parentRefs extracted as cell
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s.contains("gateway-system/prod-gw"),
            _ => false,
        }));
    }

    #[test]
    fn httproute_top_conditions_accepted_is_healthy() {
        let val = serde_json::json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "GRPCRoute",
            "metadata": {
                "name": "grpc-route",
                "uid": "route-2"
            },
            "spec": {
                "parentRefs": [
                    { "name": "prod-gw" }
                ]
            },
            "status": {
                "conditions": [
                    {
                        "type": "Accepted",
                        "status": "True",
                        "reason": "RouteAccepted"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "GRPCRoute");

        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "RouteAccepted");
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s == "prod-gw",
            _ => false,
        }));
    }

    #[test]
    fn argo_application_healthy_and_synced_is_healthy() {
        let val = serde_json::json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Application",
            "metadata": {
                "name": "guestbook",
                "namespace": "argocd",
                "uid": "argo-1"
            },
            "status": {
                "health": {
                    "status": "Healthy"
                },
                "sync": {
                    "status": "Synced"
                }
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Application");

        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Healthy");
        assert_eq!(row.custom, Vec::<Cell>::new());
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s == "Synced",
            _ => false,
        }));
    }

    #[test]
    fn argo_application_degraded_is_failed() {
        let val = serde_json::json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Application",
            "metadata": {
                "name": "frontend",
                "namespace": "argocd",
                "uid": "argo-2"
            },
            "status": {
                "health": {
                    "status": "Degraded",
                    "message": "Deployment has degraded"
                },
                "sync": {
                    "status": "OutOfSync"
                }
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Application");

        assert_eq!(row.status, RowStatus::Failed);
        assert_eq!(row.status_reason, "Degraded");
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s == "OutOfSync",
            _ => false,
        }));
    }

    #[test]
    fn argo_application_progressing_is_pending() {
        let val = serde_json::json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Application",
            "metadata": {
                "name": "deploying",
                "uid": "argo-3"
            },
            "status": {
                "health": {
                    "status": "Progressing"
                },
                "sync": {
                    "status": "Synced"
                }
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Application");

        assert_eq!(row.status, RowStatus::Pending);
        assert_eq!(row.status_reason, "Progressing");
    }

    #[test]
    fn flux_kustomization_suspended_is_degraded() {
        let val = serde_json::json!({
            "apiVersion": "kustomize.toolkit.fluxcd.io/v1",
            "kind": "Kustomization",
            "metadata": {
                "name": "infra",
                "namespace": "flux-system",
                "uid": "flux-1"
            },
            "spec": {
                "suspend": true
            },
            "status": {
                "conditions": [
                    {
                        "type": "Ready",
                        "status": "True",
                        "reason": "ReconciliationSucceeded"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Kustomization");

        assert_eq!(row.status, RowStatus::Degraded);
        assert_eq!(row.status_reason, "Suspended");
        assert_eq!(row.custom, Vec::<Cell>::new());

        // Suspension flag cell and ready reason cell
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s == "true",
            _ => false,
        }));
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s == "ReconciliationSucceeded",
            _ => false,
        }));
    }

    #[test]
    fn flux_kustomization_ready_true_is_healthy() {
        let val = serde_json::json!({
            "apiVersion": "kustomize.toolkit.fluxcd.io/v1",
            "kind": "Kustomization",
            "metadata": {
                "name": "workloads",
                "namespace": "flux-system",
                "uid": "flux-2"
            },
            "spec": {
                "suspend": false
            },
            "status": {
                "conditions": [
                    {
                        "type": "Ready",
                        "status": "True",
                        "reason": "ReconciliationSucceeded"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "Kustomization");

        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "ReconciliationSucceeded");
        assert!(row.cells.iter().any(|c| match c {
            Cell::Text(s) => s == "false",
            _ => false,
        }));
    }

    #[test]
    fn flux_helmrelease_ready_false_is_failed() {
        let val = serde_json::json!({
            "apiVersion": "helm.toolkit.fluxcd.io/v2beta2",
            "kind": "HelmRelease",
            "metadata": {
                "name": "redis",
                "uid": "hr-1"
            },
            "status": {
                "conditions": [
                    {
                        "type": "Ready",
                        "status": "False",
                        "reason": "InstallFailed"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "HelmRelease");

        assert_eq!(row.status, RowStatus::Failed);
        assert_eq!(row.status_reason, "InstallFailed");
    }

    #[test]
    fn karpenter_nodepool_ready_true_is_healthy() {
        let val = serde_json::json!({
            "apiVersion": "karpenter.sh/v1",
            "kind": "NodePool",
            "metadata": {
                "name": "general",
                "uid": "np-1"
            },
            "status": {
                "conditions": [
                    {
                        "type": "Ready",
                        "status": "True",
                        "reason": "Ready"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "NodePool");

        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Ready");
        assert_eq!(row.custom, Vec::<Cell>::new());
    }

    #[test]
    fn karpenter_nodeclaim_not_ready_is_degraded() {
        let val = serde_json::json!({
            "apiVersion": "karpenter.sh/v1",
            "kind": "NodeClaim",
            "metadata": {
                "name": "general-abcde",
                "uid": "nc-1"
            },
            "status": {
                "conditions": [
                    {
                        "type": "Ready",
                        "status": "False",
                        "reason": "NodeNotReady"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "cluster-1", "NodeClaim");

        assert_eq!(row.status, RowStatus::Degraded);
        assert_eq!(row.status_reason, "NodeNotReady");
    }

    #[test]
    fn unknown_crd_fallback() {
        let val = serde_json::json!({
            "apiVersion": "cert-manager.io/v1",
            "kind": "Certificate",
            "metadata": {
                "name": "tls-secret",
                "namespace": "ingress",
                "uid": "cert-1"
            },
            "status": {
                "phase": "Ready"
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();

        assert!(!is_ecosystem_kind("CustomFoo"));
        let row = project_ecosystem_object(&obj, "cluster-1", "CustomFoo");

        assert_eq!(row.kind, "CustomFoo");
        assert_eq!(row.name, "tls-secret");
        assert_eq!(row.namespace.as_deref(), Some("ingress"));
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Ready");
        assert_eq!(row.custom, Vec::<Cell>::new());
    }

    #[test]
    fn gateway_class_accepted_projection() {
        let val = serde_json::json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "GatewayClass",
            "metadata": {
                "name": "cilium",
                "uid": "gc-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "controllerName": "io.cilium/gateway-controller"
            },
            "status": {
                "conditions": [
                    {
                        "type": "Accepted",
                        "status": "True",
                        "reason": "Accepted"
                    }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "GatewayClass");
        assert_eq!(row.kind, "GatewayClass");
        assert_eq!(row.name, "cilium");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Accepted");
        assert_eq!(
            row.cells[1],
            Cell::Text("io.cilium/gateway-controller".to_owned())
        );
    }

    #[test]
    fn cert_manager_certificate_projection() {
        let val = serde_json::json!({
            "apiVersion": "cert-manager.io/v1",
            "kind": "Certificate",
            "metadata": {
                "name": "tls-cert",
                "namespace": "frontend",
                "uid": "cert-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "secretName": "tls-secret",
                "issuerRef": { "name": "letsencrypt-prod" }
            },
            "status": {
                "notAfter": "2026-04-01T00:00:00Z",
                "conditions": [
                    { "type": "Ready", "status": "True", "reason": "Ready" }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "Certificate");
        assert_eq!(row.kind, "Certificate");
        assert_eq!(row.name, "tls-cert");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.cells[1], Cell::Text("True".to_owned()));
        assert_eq!(row.cells[2], Cell::Text("tls-secret".to_owned()));
        assert_eq!(row.cells[3], Cell::Text("letsencrypt-prod".to_owned()));
        assert_eq!(row.cells[4], Cell::Text("2026-04-01T00:00:00Z".to_owned()));
    }

    #[test]
    fn cert_manager_certificate_request_projection() {
        let val = serde_json::json!({
            "apiVersion": "cert-manager.io/v1",
            "kind": "CertificateRequest",
            "metadata": {
                "name": "tls-cert-req",
                "namespace": "frontend",
                "uid": "cr-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "issuerRef": { "name": "letsencrypt-prod" }
            },
            "status": {
                "conditions": [
                    { "type": "Approved", "status": "True", "reason": "Approved" },
                    { "type": "Ready", "status": "True", "reason": "Issued" }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "CertificateRequest");
        assert_eq!(row.kind, "CertificateRequest");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Issued");
        assert_eq!(row.cells[1], Cell::Text("True".to_owned()));
        assert_eq!(row.cells[2], Cell::Text("True".to_owned()));
        assert_eq!(row.cells[3], Cell::Text("letsencrypt-prod".to_owned()));
    }

    #[test]
    fn cert_manager_issuer_projection() {
        let val = serde_json::json!({
            "apiVersion": "cert-manager.io/v1",
            "kind": "ClusterIssuer",
            "metadata": {
                "name": "letsencrypt-prod",
                "uid": "ci-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "acme": {
                    "email": "ops@example.com"
                }
            },
            "status": {
                "conditions": [
                    { "type": "Ready", "status": "True", "reason": "Verified" }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "ClusterIssuer");
        assert_eq!(row.kind, "ClusterIssuer");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.cells[1], Cell::Text("True".to_owned()));
        assert_eq!(row.cells[2], Cell::Text("ACME".to_owned()));
    }

    #[test]
    fn external_secret_projection() {
        let val = serde_json::json!({
            "apiVersion": "external-secrets.io/v1beta1",
            "kind": "ExternalSecret",
            "metadata": {
                "name": "db-pass",
                "namespace": "backend",
                "uid": "es-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "secretStoreRef": { "name": "aws-secrets" },
                "target": { "name": "db-secret" }
            },
            "status": {
                "refreshTime": "2026-01-01T01:00:00Z",
                "conditions": [
                    { "type": "Ready", "status": "True", "reason": "SecretSynced" }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "ExternalSecret");
        assert_eq!(row.kind, "ExternalSecret");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "SecretSynced");
        assert_eq!(row.cells[1], Cell::Text("aws-secrets".to_owned()));
        assert_eq!(row.cells[2], Cell::Text("db-secret".to_owned()));
        assert_eq!(row.cells[3], Cell::Text("2026-01-01T01:00:00Z".to_owned()));
    }

    #[test]
    fn secret_store_projection() {
        let val = serde_json::json!({
            "apiVersion": "external-secrets.io/v1beta1",
            "kind": "SecretStore",
            "metadata": {
                "name": "vault-backend",
                "namespace": "backend",
                "uid": "ss-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "provider": {
                    "vault": {}
                }
            },
            "status": {
                "conditions": [
                    { "type": "Ready", "status": "True", "reason": "Valid" }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "SecretStore");
        assert_eq!(row.kind, "SecretStore");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.cells[1], Cell::Text("vault".to_owned()));
    }

    #[test]
    fn argo_rollout_projection() {
        let val = serde_json::json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Rollout",
            "metadata": {
                "name": "web-rollout",
                "namespace": "production",
                "uid": "ro-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "spec": {
                "replicas": 5,
                "strategy": {
                    "canary": {
                        "steps": [
                            { "setWeight": 20 },
                            { "pause": { "duration": "1h" } },
                            { "setWeight": 50 }
                        ]
                    }
                }
            },
            "status": {
                "phase": "Progressing",
                "currentStepIndex": 1,
                "readyReplicas": 5,
                "pauseConditions": [
                    { "reason": "CanaryPauseStep" }
                ]
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "Rollout");
        assert_eq!(row.kind, "Rollout");
        assert_eq!(row.status, RowStatus::Pending);
        assert_eq!(row.status_reason, "Paused");
        assert_eq!(row.cells[1], Cell::Text("canary".to_owned()));
        assert_eq!(row.cells[2], Cell::Text("2/3".to_owned()));
        assert_eq!(row.cells[3], Cell::Text("5/5".to_owned()));
    }

    #[test]
    fn argo_analysis_run_projection() {
        let val = serde_json::json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "AnalysisRun",
            "metadata": {
                "name": "web-analysis-1",
                "namespace": "production",
                "uid": "ar-1",
                "creationTimestamp": "2026-01-01T00:00:00Z"
            },
            "status": {
                "phase": "Successful"
            }
        });
        let obj: DynamicObject = serde_json::from_value(val).unwrap();
        let row = project_ecosystem_object(&obj, "dev-1", "AnalysisRun");
        assert_eq!(row.kind, "AnalysisRun");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Successful");
        assert_eq!(row.cells[1], Cell::Text("Successful".to_owned()));
    }

    #[test]
    fn is_ecosystem_kind_matches_known_kinds() {
        let known = [
            "Gateway",
            "HTTPRoute",
            "GRPCRoute",
            "GatewayClass",
            "Application",
            "AppProject",
            "Rollout",
            "AnalysisRun",
            "Kustomization",
            "HelmRelease",
            "NodePool",
            "NodeClaim",
            "Certificate",
            "CertificateRequest",
            "Issuer",
            "ClusterIssuer",
            "ExternalSecret",
            "SecretStore",
            "ClusterSecretStore",
        ];
        for k in known {
            assert!(is_ecosystem_kind(k), "expected {k} to be recognized");
        }

        let unknown = ["Deployment", "Pod", "Service", "Unknown", "CustomFoo"];
        for u in unknown {
            assert!(!is_ecosystem_kind(u), "expected {u} to NOT be recognized");
        }
    }
}
