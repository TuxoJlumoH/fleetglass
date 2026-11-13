//! R-12b live diagnostics integration tests.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_diagnostics -- --nocapture`
//!
//! Only `.dev/kubeconfig` is used (never `~/.kube/config`).

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fleetglass_core::api::DiagnosticStatus;
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::diagnostics::{COMMON_RBAC_CHECKS, diagnose_kubeconfig_context_live};
use fleetglass_core::kubeconfig::KubeconfigSource;

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{:x}{:x}",
            std::process::id(),
            nanos & 0xff_ffff
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_kubeconfig_0600(path: &std::path::Path, bytes: &[u8]) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).unwrap();
    }
}

#[tokio::test]
async fn live_diagnostics_against_kind_cluster_all_pass() {
    let kubeconfig_path = std::env::var("KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
        });

    let source = KubeconfigSource::from_paths(vec![kubeconfig_path]);
    let loaded = source.load().unwrap();
    let manager = ClusterManager::new(source);

    let context = "kind-fleetglass-dev";
    let client = manager.connect_client(context).await;
    let report =
        diagnose_kubeconfig_context_live(client, &loaded.config, context, "login-shell").await;

    assert_eq!(report.context, context);
    assert!(!report.steps.is_empty());

    // Asserts all diagnostic steps pass
    for step in &report.steps {
        assert_eq!(
            step.status,
            DiagnosticStatus::Pass,
            "step \"{}\" failed: {}",
            step.name,
            step.detail
        );
    }

    // Asserts API version is reported
    let reachability = report
        .steps
        .iter()
        .find(|s| s.name == "API Server Reachability")
        .expect("API Server Reachability step must exist");
    assert_eq!(reachability.status, DiagnosticStatus::Pass);
    assert!(
        reachability.detail.contains("v1."),
        "expected API version in detail, got: {}",
        reachability.detail
    );

    // Asserts RBAC contains common checks with allowed == true
    assert_eq!(report.rbac.len(), COMMON_RBAC_CHECKS.len());
    for check in &report.rbac {
        assert!(
            check.allowed,
            "expected RBAC check for {} {} to be allowed",
            check.resource, check.verb
        );
    }
    for &(resource, verb) in COMMON_RBAC_CHECKS {
        assert!(
            report
                .rbac
                .iter()
                .any(|c| c.resource == resource && c.verb == verb && c.allowed),
            "missing RBAC check for {resource} {verb}"
        );
    }
}

#[tokio::test]
async fn live_diagnostics_unreachable_port_fails_within_timeout() {
    let temp_dir = TempDir::new("fg-live-diag-unreachable");
    let kubeconfig_file = temp_dir.path().join("kubeconfig");

    let fixture = r#"apiVersion: v1
clusters:
- cluster:
    insecure-skip-tls-verify: true
    server: https://127.0.0.1:54321
  name: unreachable-cluster
contexts:
- context:
    cluster: unreachable-cluster
    user: unreachable-user
  name: unreachable-context
current-context: unreachable-context
kind: Config
users:
- name: unreachable-user
  user:
    token: secret-unreachable-token
"#;

    write_kubeconfig_0600(&kubeconfig_file, fixture.as_bytes());

    let source = KubeconfigSource::from_paths(vec![kubeconfig_file]);
    let loaded = source.load().unwrap();
    let manager = ClusterManager::new(source);

    let start = Instant::now();
    let client = manager.connect_client("unreachable-context").await;
    let report = diagnose_kubeconfig_context_live(
        client,
        &loaded.config,
        "unreachable-context",
        "process-env",
    )
    .await;
    let elapsed = start.elapsed();

    // Asserts reachability fails within timeout (under 10s) and does not hang
    assert!(
        elapsed < Duration::from_secs(10),
        "diagnostics took too long: {elapsed:?}"
    );

    assert_eq!(report.context, "unreachable-context");

    let reachability = report
        .steps
        .iter()
        .find(|s| s.name == "API Server Reachability")
        .expect("API Server Reachability step must exist");
    assert_eq!(reachability.status, DiagnosticStatus::Fail);

    // Non-negotiable: Never echo credentials, tokens, client key material, or raw secret envs!
    assert!(
        !reachability.detail.contains("secret-unreachable-token"),
        "credentials leaked in step detail: {}",
        reachability.detail
    );

    // Returns empty or partial RBAC on failure
    assert!(
        report.rbac.is_empty(),
        "expected empty RBAC on unreachable cluster, got: {:?}",
        report.rbac
    );
}

#[tokio::test]
async fn live_diagnostics_missing_context_fails() {
    let kubeconfig_path = std::env::var("KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig")
        });

    let source = KubeconfigSource::from_paths(vec![kubeconfig_path]);
    let loaded = source.load().unwrap();
    let manager = ClusterManager::new(source);

    let client = manager.connect_client("non-existent-context").await;
    let report = diagnose_kubeconfig_context_live(
        client,
        &loaded.config,
        "non-existent-context",
        "login-shell",
    )
    .await;

    assert_eq!(report.context, "non-existent-context");
    assert_eq!(report.steps[0].status, DiagnosticStatus::Fail);
    let reachability = report
        .steps
        .iter()
        .find(|s| s.name == "API Server Reachability")
        .expect("reachability step present");
    assert_eq!(reachability.status, DiagnosticStatus::Fail);
    assert!(report.rbac.is_empty());
}
