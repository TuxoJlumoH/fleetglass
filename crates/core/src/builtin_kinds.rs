//! Built-in Kubernetes kind projections (`04-05a`).
//!
//! Provides typed projections for expanded built-in Kubernetes resources:
//! - Workloads & Discovery: `ReplicaSet`, `EndpointSlice`, `HorizontalPodAutoscaler`, `PodDisruptionBudget`
//! - Quotas & Scheduling: `ResourceQuota`, `LimitRange`, `PriorityClass`, `RuntimeClass`, `Lease`
//! - Storage: `PersistentVolume`, `StorageClass`, `VolumeAttachment`
//! - Access Control (RBAC): `ServiceAccount`, `Role`, `ClusterRole`, `RoleBinding`, `ClusterRoleBinding`
//! - Policy & Admission: `NetworkPolicy`, `MutatingWebhookConfiguration`, `ValidatingWebhookConfiguration`, `ValidatingAdmissionPolicy`, `ValidatingAdmissionPolicyBinding`

use k8s_openapi::api::admissionregistration::v1::{
    MutatingWebhookConfiguration, ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding,
    ValidatingWebhookConfiguration,
};
use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::api::core::v1::{LimitRange, PersistentVolume, ResourceQuota, ServiceAccount};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::node::v1::RuntimeClass;
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding, Role, RoleBinding};
use k8s_openapi::api::scheduling::v1::PriorityClass;
use k8s_openapi::api::storage::v1::{StorageClass, VolumeAttachment};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

use crate::api::{Cell, Row, RowStatus};

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

// ---------------------------------------------------------------------------
// Workloads & Discovery
// ---------------------------------------------------------------------------

pub fn project_replica_set(rs: &ReplicaSet, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&rs.metadata);

    let desired_i32 = rs
        .spec
        .as_ref()
        .and_then(|s| s.replicas)
        .unwrap_or(0)
        .max(0);
    let ready_i32 = rs
        .status
        .as_ref()
        .and_then(|s| s.ready_replicas)
        .unwrap_or(0)
        .max(0);
    let current_i32 = rs.status.as_ref().map(|s| s.replicas).unwrap_or(0).max(0);

    let desired = u32::try_from(desired_i32).unwrap_or(0);
    let ready = u32::try_from(ready_i32).unwrap_or(0);

    let (status_reason, status_phase) = if rs.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if desired == 0 && current_i32 == 0 {
        ("Scaled down".to_owned(), RowStatus::Healthy)
    } else if ready == desired {
        ("Ready".to_owned(), RowStatus::Healthy)
    } else if ready == 0 && desired > 0 {
        ("Pending".to_owned(), RowStatus::Pending)
    } else {
        ("Progressing".to_owned(), RowStatus::Pending)
    };

    let cells = vec![
        Cell::Ready {
            ready,
            total: desired,
        },
        Cell::Int(current_i32),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "ReplicaSet".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_endpoint_slice(eps: &EndpointSlice, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&eps.metadata);

    let address_type = eps.address_type.clone();
    let ports_summary = eps
        .ports
        .as_ref()
        .map(|ports| {
            if ports.is_empty() {
                "—".to_owned()
            } else {
                ports
                    .iter()
                    .map(|p| {
                        let proto = p.protocol.as_deref().unwrap_or("TCP");
                        match p.port {
                            Some(port) => format!("{port}/{proto}"),
                            None => proto.to_owned(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
        .unwrap_or_else(|| "—".to_owned());

    let count = eps.endpoints.as_ref().map(Vec::len).unwrap_or(0);
    let ep_summary = if count == 1 {
        "1 endpoint".to_owned()
    } else {
        format!("{count} endpoints")
    };

    let (status_reason, status_phase) = if eps.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(address_type),
        Cell::Text(ports_summary),
        Cell::Text(ep_summary),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "EndpointSlice".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_hpa(hpa: &HorizontalPodAutoscaler, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&hpa.metadata);

    let reference = format!(
        "{}/{}",
        hpa.spec.scale_target_ref.kind, hpa.spec.scale_target_ref.name
    );

    let min_pods = hpa.spec.min_replicas.unwrap_or(1);
    let max_pods = hpa.spec.max_replicas;
    let current_replicas = hpa
        .status
        .as_ref()
        .and_then(|s| s.current_replicas)
        .unwrap_or(0);

    let (status_reason, status_phase) = if hpa.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(reference),
        Cell::Int(min_pods),
        Cell::Int(max_pods),
        Cell::Int(current_replicas),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "HorizontalPodAutoscaler".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_pdb(pdb: &PodDisruptionBudget, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&pdb.metadata);

    let min_available = pdb
        .spec
        .as_ref()
        .and_then(|s| s.min_available.as_ref())
        .map(|v| match v {
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(i) => i.to_string(),
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(s) => s.clone(),
        })
        .unwrap_or_else(|| "N/A".to_owned());

    let max_unavailable = pdb
        .spec
        .as_ref()
        .and_then(|s| s.max_unavailable.as_ref())
        .map(|v| match v {
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(i) => i.to_string(),
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(s) => s.clone(),
        })
        .unwrap_or_else(|| "N/A".to_owned());

    let allowed = pdb
        .status
        .as_ref()
        .and_then(|s| s.disruptions_allowed)
        .unwrap_or(0);
    let current_healthy = pdb
        .status
        .as_ref()
        .and_then(|s| s.current_healthy)
        .unwrap_or(0);
    let desired_healthy = pdb
        .status
        .as_ref()
        .and_then(|s| s.desired_healthy)
        .unwrap_or(0);

    let (status_reason, status_phase) = if pdb.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if allowed == 0 && desired_healthy > 0 {
        ("Restricted".to_owned(), RowStatus::Degraded)
    } else {
        ("Allowed".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(min_available),
        Cell::Text(max_unavailable),
        Cell::Int(allowed),
        Cell::Int(current_healthy),
        Cell::Int(desired_healthy),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "PodDisruptionBudget".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Quotas & Scheduling
// ---------------------------------------------------------------------------

pub fn project_resource_quota(rq: &ResourceQuota, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&rq.metadata);

    let (status_reason, status_phase) = if rq.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
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
        kind: "ResourceQuota".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_limit_range(lr: &LimitRange, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&lr.metadata);

    let (status_reason, status_phase) = if lr.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
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
        kind: "LimitRange".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_priority_class(pc: &PriorityClass, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&pc.metadata);

    let value = pc.value.unwrap_or(0);
    let global_default = pc.global_default.unwrap_or(false);
    let preemption_policy = pc
        .preemption_policy
        .as_deref()
        .unwrap_or("PreemptLowerPriority")
        .to_owned();

    let (status_reason, status_phase) = if pc.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Int(value),
        Cell::Text(if global_default { "true" } else { "false" }.to_owned()),
        Cell::Text(preemption_policy),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "PriorityClass".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_runtime_class(rc: &RuntimeClass, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&rc.metadata);

    let handler = rc.handler.clone();

    let (status_reason, status_phase) = if rc.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(handler),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "RuntimeClass".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_lease(lease: &Lease, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&lease.metadata);

    let holder = lease
        .spec
        .as_ref()
        .and_then(|s| s.holder_identity.clone())
        .unwrap_or_else(|| "—".to_owned());
    let duration = lease
        .spec
        .as_ref()
        .and_then(|s| s.lease_duration_seconds)
        .unwrap_or(0);

    let (status_reason, status_phase) = if lease.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(holder),
        Cell::Int(duration),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Lease".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

pub fn project_pv(pv: &PersistentVolume, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&pv.metadata);

    let capacity = pv
        .spec
        .as_ref()
        .and_then(|s| s.capacity.as_ref())
        .and_then(|c| c.get("storage"))
        .map(|q| q.0.clone())
        .unwrap_or_else(|| "—".to_owned());

    let access_modes = pv
        .spec
        .as_ref()
        .and_then(|s| s.access_modes.as_ref())
        .map(|modes| modes.join(","))
        .unwrap_or_else(|| "—".to_owned());

    let reclaim_policy = pv
        .spec
        .as_ref()
        .and_then(|s| s.persistent_volume_reclaim_policy.clone())
        .unwrap_or_else(|| "Retain".to_owned());

    let phase_str = pv
        .status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Available".to_owned());

    let claim = pv
        .spec
        .as_ref()
        .and_then(|s| s.claim_ref.as_ref())
        .map(|c| {
            format!(
                "{}/{}",
                c.namespace.as_deref().unwrap_or(""),
                c.name.as_deref().unwrap_or("")
            )
        })
        .unwrap_or_else(|| "—".to_owned());

    let storage_class = pv
        .spec
        .as_ref()
        .and_then(|s| s.storage_class_name.clone())
        .unwrap_or_else(|| "—".to_owned());

    let status_phase = match phase_str.as_str() {
        "Bound" | "Available" => RowStatus::Healthy,
        "Released" | "Pending" => RowStatus::Pending,
        "Failed" => RowStatus::Failed,
        _ => RowStatus::Unknown,
    };

    let cells = vec![
        Cell::Text(capacity),
        Cell::Text(access_modes),
        Cell::Text(reclaim_policy),
        Cell::Status {
            label: phase_str.clone(),
            phase: status_phase,
        },
        Cell::Text(claim),
        Cell::Text(storage_class),
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "PersistentVolume".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason: phase_str,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_storage_class(sc: &StorageClass, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&sc.metadata);

    let provisioner = sc.provisioner.clone();
    let reclaim = sc.reclaim_policy.as_deref().unwrap_or("Delete").to_owned();
    let binding_mode = sc
        .volume_binding_mode
        .as_deref()
        .unwrap_or("Immediate")
        .to_owned();
    let allow_expansion = sc.allow_volume_expansion.unwrap_or(false);

    let (status_reason, status_phase) = if sc.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(provisioner),
        Cell::Text(reclaim),
        Cell::Text(binding_mode),
        Cell::Text(if allow_expansion { "true" } else { "false" }.to_owned()),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "StorageClass".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_volume_attachment(va: &VolumeAttachment, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&va.metadata);

    let attacher = va.spec.attacher.clone();
    let node = va.spec.node_name.clone();
    let pv = va
        .spec
        .source
        .persistent_volume_name
        .clone()
        .unwrap_or_else(|| "—".to_owned());
    let attached = va.status.as_ref().map(|s| s.attached).unwrap_or(false);

    let (status_reason, status_phase) = if va.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if attached {
        ("Attached".to_owned(), RowStatus::Healthy)
    } else {
        ("Attaching".to_owned(), RowStatus::Pending)
    };

    let cells = vec![
        Cell::Text(attacher),
        Cell::Text(node),
        Cell::Text(pv),
        Cell::Text(if attached { "true" } else { "false" }.to_owned()),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "VolumeAttachment".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Access Control (RBAC)
// ---------------------------------------------------------------------------

pub fn project_service_account(sa: &ServiceAccount, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&sa.metadata);

    let secret_count = sa.secrets.as_ref().map(Vec::len).unwrap_or(0);

    let (status_reason, status_phase) = if sa.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Int(i32::try_from(secret_count).unwrap_or(0)),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "ServiceAccount".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_role(role: &Role, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&role.metadata);

    let (status_reason, status_phase) = if role.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
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
        kind: "Role".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_cluster_role(cr: &ClusterRole, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&cr.metadata);

    let (status_reason, status_phase) = if cr.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
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
        kind: "ClusterRole".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_role_binding(rb: &RoleBinding, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&rb.metadata);

    let role_ref = format!("{}/{}", rb.role_ref.kind, rb.role_ref.name);
    let subjects = rb
        .subjects
        .as_ref()
        .map(|subs| {
            if subs.is_empty() {
                "—".to_owned()
            } else {
                subs.iter()
                    .map(|s| format!("{}:{}", s.kind, s.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
        .unwrap_or_else(|| "—".to_owned());

    let (status_reason, status_phase) = if rb.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(role_ref),
        Cell::Text(subjects),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "RoleBinding".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_cluster_role_binding(crb: &ClusterRoleBinding, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&crb.metadata);

    let role_ref = format!("{}/{}", crb.role_ref.kind, crb.role_ref.name);
    let subjects = crb
        .subjects
        .as_ref()
        .map(|subs| {
            if subs.is_empty() {
                "—".to_owned()
            } else {
                subs.iter()
                    .map(|s| format!("{}:{}", s.kind, s.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
        .unwrap_or_else(|| "—".to_owned());

    let (status_reason, status_phase) = if crb.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(role_ref),
        Cell::Text(subjects),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "ClusterRoleBinding".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Policy & Admission
// ---------------------------------------------------------------------------

pub fn project_network_policy(np: &NetworkPolicy, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&np.metadata);

    let selector = np
        .spec
        .as_ref()
        .and_then(|s| s.pod_selector.as_ref())
        .and_then(|sel| sel.match_labels.as_ref())
        .map(|m| {
            if m.is_empty() {
                "<all>".to_owned()
            } else {
                m.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
        .unwrap_or_else(|| "<all>".to_owned());

    let (status_reason, status_phase) = if np.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(selector),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "NetworkPolicy".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_mutating_webhook_config(mwc: &MutatingWebhookConfiguration, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&mwc.metadata);

    let webhook_count = mwc.webhooks.as_ref().map(Vec::len).unwrap_or(0);

    let (status_reason, status_phase) = if mwc.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Int(i32::try_from(webhook_count).unwrap_or(0)),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "MutatingWebhookConfiguration".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_validating_webhook_config(
    vwc: &ValidatingWebhookConfiguration,
    cluster: &str,
) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&vwc.metadata);

    let webhook_count = vwc.webhooks.as_ref().map(Vec::len).unwrap_or(0);

    let (status_reason, status_phase) = if vwc.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Int(i32::try_from(webhook_count).unwrap_or(0)),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "ValidatingWebhookConfiguration".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_validating_admission_policy(vap: &ValidatingAdmissionPolicy, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&vap.metadata);

    let (status_reason, status_phase) = if vap.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
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
        kind: "ValidatingAdmissionPolicy".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

pub fn project_validating_admission_policy_binding(
    vapb: &ValidatingAdmissionPolicyBinding,
    cluster: &str,
) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&vapb.metadata);

    let policy_name = vapb.spec.policy_name.clone();

    let (status_reason, status_phase) = if vapb.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Text(policy_name),
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "ValidatingAdmissionPolicyBinding".to_owned(),
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
    fn test_project_replica_set() {
        let rs = ReplicaSet {
            metadata: ObjectMeta {
                name: Some("rs-1".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::apps::v1::ReplicaSetSpec {
                replicas: Some(3),
                ..Default::default()
            }),
            status: Some(k8s_openapi::api::apps::v1::ReplicaSetStatus {
                replicas: 3,
                ready_replicas: Some(3),
                ..Default::default()
            }),
        };

        let row = project_replica_set(&rs, "kind-test");
        assert_eq!(row.kind, "ReplicaSet");
        assert_eq!(row.name, "rs-1");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Ready");
        assert_eq!(row.cells[0], Cell::Ready { ready: 3, total: 3 });
        assert_eq!(row.cells[1], Cell::Int(3));
    }

    #[test]
    fn test_project_pv_and_sc() {
        let pv = PersistentVolume {
            metadata: ObjectMeta {
                name: Some("pv-data".into()),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::core::v1::PersistentVolumeSpec {
                capacity: Some(
                    [(
                        "storage".into(),
                        k8s_openapi::apimachinery::pkg::api::resource::Quantity("10Gi".into()),
                    )]
                    .into(),
                ),
                persistent_volume_reclaim_policy: Some("Retain".into()),
                storage_class_name: Some("standard".into()),
                ..Default::default()
            }),
            status: Some(k8s_openapi::api::core::v1::PersistentVolumeStatus {
                phase: Some("Bound".into()),
                ..Default::default()
            }),
        };

        let row = project_pv(&pv, "kind-test");
        assert_eq!(row.name, "pv-data");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.cells[0], Cell::Text("10Gi".into()));
        assert_eq!(row.cells[2], Cell::Text("Retain".into()));
    }
}
