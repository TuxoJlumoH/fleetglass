//! R-05 live integration test: dry_run_apply and apply_yaml against kind-fleetglass-dev.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_apply -- --nocapture`

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use futures::FutureExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Api;
use kube::api::{DeleteParams, PostParams};

use fleetglass_core::actions::{apply_yaml, dry_run_apply};
use fleetglass_core::api::ContextSettings;
use fleetglass_core::audit::AuditLogStore;
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::error::Error;
use fleetglass_core::kubeconfig::KubeconfigSource;

const CM_NAME: &str = "fg-r05-test";
const NAMESPACE: &str = "default";

fn dev_kubeconfig() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
}

#[tokio::test]
async fn live_apply_lifecycle() {
    let kubeconfig = dev_kubeconfig();
    assert!(
        kubeconfig.is_file(),
        "{} missing: run scripts/dev-cluster.sh up",
        kubeconfig.display()
    );

    let source = KubeconfigSource::from_paths(vec![kubeconfig]);
    let context = source
        .load()
        .unwrap()
        .config
        .current_context
        .expect("current-context in .dev/kubeconfig");
    assert_eq!(context, "kind-fleetglass-dev", "only the dev kind cluster");
    let manager = Arc::new(ClusterManager::new(source));
    let client = manager.client_for(&context).await.unwrap();

    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), NAMESPACE);

    // Clean up any stale ConfigMap from previous runs
    let _ = cms.delete(CM_NAME, &DeleteParams::default()).await;

    // 1. Create ConfigMap fg-r05-test in namespace default with initial data foo: "bar"
    let mut initial_data = BTreeMap::new();
    initial_data.insert("foo".to_owned(), "bar".to_owned());
    let initial_cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(CM_NAME.to_owned()),
            namespace: Some(NAMESPACE.to_owned()),
            ..Default::default()
        },
        data: Some(initial_data),
        ..Default::default()
    };
    cms.create(&PostParams::default(), &initial_cm)
        .await
        .expect("create initial ConfigMap");

    let audit_dir = std::env::temp_dir().join(format!(
        "fleetglass-live-apply-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&audit_dir).unwrap();
    let audit = Arc::new(AuditLogStore::new(audit_dir.join("audit.jsonl")));

    let outcome = AssertUnwindSafe(run_test(
        &manager,
        cms.clone(),
        Arc::clone(&audit),
        &context,
    ))
    .catch_unwind()
    .await;

    // Cleanup ConfigMap
    let _ = cms.delete(CM_NAME, &DeleteParams::default()).await;
    let _ = std::fs::remove_dir_all(&audit_dir);

    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn run_test(
    manager: &ClusterManager,
    cms: Api<ConfigMap>,
    audit: Arc<AuditLogStore>,
    context: &str,
) {
    let yaml_baz = r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: fg-r05-test
  namespace: default
data:
  foo: "baz"
"#;

    // 2. dry_run_apply: verifies success, returned YAML has "baz", but cluster ConfigMap still has "bar"
    let dry_run_out = dry_run_apply(manager, context, yaml_baz, &ContextSettings::default())
        .await
        .expect("dry_run_apply should succeed");
    assert!(
        dry_run_out.contains("baz"),
        "returned dry-run YAML should have 'baz', got:\n{dry_run_out}"
    );

    let cluster_cm = cms.get(CM_NAME).await.expect("cluster ConfigMap exists");
    assert_eq!(
        cluster_cm
            .data
            .as_ref()
            .and_then(|d| d.get("foo"))
            .map(|s| s.as_str()),
        Some("bar"),
        "cluster ConfigMap must still have 'bar' after dry-run apply"
    );

    // 3. With ContextSettings { read_only: true, .. }: verifies dry_run_apply and apply_yaml return ReadOnlyContext
    let ro_settings = ContextSettings {
        read_only: true,
        ..ContextSettings::default()
    };

    let ro_dry_run = dry_run_apply(manager, context, yaml_baz, &ro_settings).await;
    assert!(
        matches!(&ro_dry_run, Err(Error::ReadOnlyContext { context: c }) if c == context),
        "dry_run_apply on read-only context must return ReadOnlyContext, got {ro_dry_run:?}"
    );

    let ro_apply = apply_yaml(
        manager,
        Arc::clone(&audit),
        context,
        yaml_baz,
        None,
        &ro_settings,
    )
    .await;
    assert!(
        matches!(&ro_apply, Err(Error::ReadOnlyContext { context: c }) if c == context),
        "apply_yaml on read-only context must return ReadOnlyContext, got {ro_apply:?}"
    );

    // 4. With ContextSettings { protected: true, .. }: verifies apply_yaml requires exact name in confirmed_name
    let protected_settings = ContextSettings {
        protected: true,
        ..ContextSettings::default()
    };

    let prot_no_confirm = apply_yaml(
        manager,
        Arc::clone(&audit),
        context,
        yaml_baz,
        None,
        &protected_settings,
    )
    .await;
    assert!(
        matches!(&prot_no_confirm, Err(Error::ConfirmationRequired { expected_name, .. }) if expected_name == CM_NAME),
        "apply_yaml on protected context without confirmed_name must return ConfirmationRequired, got {prot_no_confirm:?}"
    );

    let prot_wrong_confirm = apply_yaml(
        manager,
        Arc::clone(&audit),
        context,
        yaml_baz,
        Some("wrong-name".to_string()),
        &protected_settings,
    )
    .await;
    assert!(
        matches!(&prot_wrong_confirm, Err(Error::ConfirmationRequired { expected_name, .. }) if expected_name == CM_NAME),
        "apply_yaml on protected context with wrong confirmed_name must return ConfirmationRequired, got {prot_wrong_confirm:?}"
    );

    // 5. apply_yaml: live ConfigMap is updated to "baz"
    let apply_out = apply_yaml(
        manager,
        Arc::clone(&audit),
        context,
        yaml_baz,
        Some(CM_NAME.to_string()),
        &protected_settings,
    )
    .await
    .expect("apply_yaml with confirmed_name must succeed");
    assert!(
        apply_out.contains("baz"),
        "applied YAML should contain 'baz', got:\n{apply_out}"
    );

    let live_cm = cms.get(CM_NAME).await.expect("cluster ConfigMap exists");
    assert_eq!(
        live_cm
            .data
            .as_ref()
            .and_then(|d| d.get("foo"))
            .map(|s| s.as_str()),
        Some("baz"),
        "live ConfigMap must be updated to 'baz'"
    );

    // 6. Verifies audit log entry
    let audit_page = audit
        .query(0, 100, Some(context), Some("apply"))
        .expect("query audit log");
    assert!(
        audit_page.total >= 4,
        "expected at least 4 audit entries (ro rejected, 2x protected rejected, 1x successful apply), got {}",
        audit_page.total
    );

    // Most recent entry should be the successful apply
    let success_entry = audit_page
        .entries
        .iter()
        .find(|e| e.success)
        .expect("should have a successful apply audit entry");
    assert_eq!(success_entry.action, "apply");
    assert_eq!(success_entry.kind, "ConfigMap");
    assert_eq!(success_entry.name, CM_NAME);
    assert_eq!(success_entry.namespace.as_deref(), Some(NAMESPACE));
    assert!(
        success_entry.kubectl.contains("kubectl") && success_entry.kubectl.contains("apply"),
        "kubectl preview: {}",
        success_entry.kubectl
    );
    assert_eq!(success_entry.user.as_deref(), Some("kind-fleetglass-dev"));

    // Rejected entries should have success == false
    let rejected_entries: Vec<_> = audit_page.entries.iter().filter(|e| !e.success).collect();
    assert!(
        rejected_entries.len() >= 3,
        "should have at least 3 rejected audit entries"
    );
    for entry in rejected_entries {
        assert_eq!(entry.action, "apply");
        assert_eq!(entry.name, CM_NAME);
        assert!(entry.error.is_some());
    }
}
