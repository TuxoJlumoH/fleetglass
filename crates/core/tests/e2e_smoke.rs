//! Phase 1 End-to-End smoke test exercising the full headless-core workflow:
//! kubeconfig merge + diagnostics (P1-02), multi-cluster watch store + query engine (P1-03/P1-04),
//! detail YAML managedFields stripping + dry-run diff (P1-05/P1-06), log ring buffer + regex search (P1-08),
//! exec command validation (P1-09), write-safety guards + kubectl preview (P1-07), and port-forwarding (P1-10).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use fleetglass_core::actions::{enforce_write_safety, preview_kubectl};
use fleetglass_core::api::{
    ContextSettings, DiagnosticStatus, ExecSpec, MutatingAction, QuerySpec, RowStatus,
    SortDirection, SortField, SortSpec,
};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::detail::{compute_unified_diff, strip_managed_fields};
use fleetglass_core::diagnostics::diagnose_kubeconfig_context;
use fleetglass_core::exec::{preview_exec_kubectl, validate_exec_allowed};
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::logs::LogBuffer;
use fleetglass_core::portforward::PortForwardManager;
use k8s_openapi::api::core::v1::{
    Container, ContainerState, ContainerStateWaiting, ContainerStatus, Pod, PodSpec, PodStatus,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn synthetic_pod(name: &str, namespace: &str, phase: &str, waiting_reason: Option<&str>) -> Pod {
    let mut labels = BTreeMap::new();
    labels.insert("app".to_owned(), "web".to_owned());
    Pod {
        metadata: ObjectMeta {
            uid: Some(format!("uid-{namespace}-{name}")),
            name: Some(name.to_owned()),
            namespace: Some(namespace.to_owned()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(PodSpec {
            containers: vec![Container {
                name: "main".to_owned(),
                image: Some("nginx:1.27-alpine".to_owned()),
                ..Default::default()
            }],
            node_name: Some("node-1".to_owned()),
            ..Default::default()
        }),
        status: Some(PodStatus {
            phase: Some(phase.to_owned()),
            container_statuses: Some(vec![ContainerStatus {
                name: "main".to_owned(),
                ready: waiting_reason.is_none() && phase == "Running",
                restart_count: if waiting_reason.is_some() { 4 } else { 0 },
                state: waiting_reason.map(|reason| ContainerState {
                    waiting: Some(ContainerStateWaiting {
                        reason: Some(reason.to_owned()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        }),
    }
}

#[tokio::test]
async fn end_to_end_core_workflow_smoke() {
    // 1. P0-06 & P1-02: Load merged kubeconfig fixtures and run connection diagnostics.
    let source =
        KubeconfigSource::from_paths(vec![fixture("merge-a.yaml"), fixture("merge-b.yaml")]);
    let loaded = source.load().unwrap();
    let list = source.list_contexts().unwrap();
    assert!(!list.contexts.is_empty());

    let report = diagnose_kubeconfig_context(&loaded.config, "dev", "login-shell");
    assert_eq!(report.context, "dev");
    assert!(
        report
            .steps
            .iter()
            .all(|s| s.status == DiagnosticStatus::Pass)
    );

    // 2. S-01, S-02, P1-03 & P1-04: Populate ClusterManager store and run filtered/sorted query.
    let manager = Arc::new(ClusterManager::new(source));
    let store = manager.get_or_create_store("dev", "Pod");
    store.upsert_pod(&synthetic_pod("web-0", "default", "Running", None));
    store.upsert_pod(&synthetic_pod(
        "web-crash",
        "default",
        "Running",
        Some("CrashLoopBackOff"),
    ));
    store.upsert_pod(&synthetic_pod("worker-0", "jobs", "Pending", None));

    let page = manager
        .query(&QuerySpec {
            clusters: vec!["dev".into()],
            kinds: vec!["Pod".into()],
            namespaces: vec!["default".into()],
            label_selector: Some("app=web".into()),
            text_filter: Some("crash".into()),
            sort: Some(SortSpec {
                field: SortField::Name,
                direction: SortDirection::Asc,
            }),
            offset: 0,
            limit: 50,
            statuses: Vec::new(),
        })
        .await;
    assert_eq!(page.total, 1);
    assert_eq!(page.rows[0].name, "web-crash");
    assert_eq!(page.rows[0].status_reason, "CrashLoopBackOff");
    assert_eq!(page.rows[0].status, RowStatus::Failed);

    // 3. P1-05 & P1-06: Strip managedFields and compute dry-run YAML diff.
    let mut raw_pod = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": "web-0",
            "managedFields": [{ "manager": "kubelet" }]
        }
    });
    strip_managed_fields(&mut raw_pod);
    assert!(raw_pod["metadata"].get("managedFields").is_none());

    let diff = compute_unified_diff(
        "replicas: 2\nimage: nginx:1.26\n",
        "replicas: 3\nimage: nginx:1.26\n",
    );
    assert!(diff.contains("-replicas: 2"));
    assert!(diff.contains("+replicas: 3"));

    // 4. P1-08: Log ring buffer + regex search.
    let mut logs = LogBuffer::new(100, None);
    logs.push_line("2026-10-31T18:00:00Z INFO starting server");
    logs.push_line("2026-10-31T18:00:01Z ERROR upstream timeout after 5000ms");
    let matched = logs.search_regex("ERROR.*timeout", 0, 50).unwrap();
    assert_eq!(matched.matched_lines, 1);
    assert!(matched.lines[0].text.contains("upstream timeout"));

    // 5. P1-07 & P1-09: Exec validation and mutating action write-safety + kubectl preview.
    let exec_spec = ExecSpec {
        context: "prod".into(),
        namespace: "default".into(),
        pod: "web-0".into(),
        container: Some("main".into()),
        command: vec![],
        cols: 80,
        rows: 24,
    };
    let ro_settings = ContextSettings {
        read_only: true,
        ..Default::default()
    };
    assert!(validate_exec_allowed(&exec_spec, &ro_settings, None).is_err());

    let protected_settings = ContextSettings {
        read_only: false,
        protected: true,
        ..Default::default()
    };
    assert_eq!(
        validate_exec_allowed(&exec_spec, &protected_settings, Some("web-0")).unwrap(),
        vec!["/bin/sh".to_owned()]
    );
    assert_eq!(
        preview_exec_kubectl(&exec_spec),
        "kubectl --context prod -n default exec -it web-0 -c main -- /bin/sh"
    );

    let delete_action = MutatingAction::Delete {
        kind: "pod".into(),
        namespace: Some("default".into()),
        name: "web-0".into(),
    };
    assert_eq!(
        preview_kubectl("prod", &delete_action),
        "kubectl --context prod -n default delete pod web-0"
    );
    assert!(enforce_write_safety("prod", &ro_settings, "web-0", Some("web-0")).is_err());
    assert!(enforce_write_safety("prod", &protected_settings, "web-0", Some("wrong")).is_err());
    assert!(enforce_write_safety("prod", &protected_settings, "web-0", Some("web-0")).is_ok());

    // 6. P1-10: Port-forward manager ephemeral bind, list, and stop.
    let pf = PortForwardManager::new();
    let info = pf
        .start_local("dev".into(), "default".into(), "web-0".into(), 80, 0, None)
        .await
        .unwrap();
    assert!(info.local_port > 0);
    assert_eq!(pf.list().len(), 1);
    assert!(pf.stop(info.id));
    assert!(pf.list().is_empty());
}
