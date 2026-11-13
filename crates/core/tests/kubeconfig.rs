//! Kubeconfig loading against fixture files only (never ~/.kube/config or the real env).

// Test crate: helpers outside #[test] fns may unwrap too.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use fleetglass_core::Error;
use fleetglass_core::api::{AuthType, ContextInfo, ContextList, StoreAccessState};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::diagnostics::diagnose_kubeconfig_context_live;
use fleetglass_core::kubeconfig::KubeconfigSource;
use fleetglass_core::projection::classify_kube_error;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn list(names: &[&str]) -> ContextList {
    KubeconfigSource::from_paths(names.iter().map(|n| fixture(n)).collect())
        .list_contexts()
        .unwrap()
}

fn ctx<'a>(list: &'a ContextList, name: &str) -> &'a ContextInfo {
    list.contexts
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("context {name} not listed"))
}

#[test]
fn two_files_merge_first_wins() {
    let list = list(&["merge-a.yaml", "merge-b.yaml"]);
    let names: Vec<_> = list.contexts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["dev", "prod", "shared"]);
    assert_eq!(list.current.as_deref(), Some("dev"));
    assert!(list.warnings.is_empty(), "{:?}", list.warnings);

    let shared = ctx(&list, "shared");
    assert_eq!(shared.namespace, None, "context from file b must not win");
    assert_eq!(
        shared.server.as_deref(),
        Some("https://shared-from-a.example.test")
    );
    assert_eq!(
        shared.auth,
        AuthType::Token,
        "user from file b must not win"
    );

    let dev = ctx(&list, "dev");
    assert!(dev.is_current);
    assert_eq!(dev.namespace.as_deref(), Some("team-a"));
    assert_eq!(dev.user.as_deref(), Some("dev-user"));
    assert_eq!(dev.cluster, "dev-cluster");

    let prod = ctx(&list, "prod");
    assert!(!prod.is_current);
    assert_eq!(prod.auth, AuthType::ClientCert);
}

#[test]
fn current_context_from_first_file_that_sets_it() {
    // oidc.yaml has no current-context, so merge-b's wins.
    let list = list(&["oidc.yaml", "merge-b.yaml", "merge-a.yaml"]);
    assert_eq!(list.current.as_deref(), Some("prod"));
    assert_eq!(
        ctx(&list, "shared").namespace.as_deref(),
        Some("from-b"),
        "merge-b is now first"
    );
}

#[test]
fn relative_cert_paths_resolve_against_the_file() {
    let loaded = KubeconfigSource::from_paths(vec![fixture("merge-b.yaml")])
        .load()
        .unwrap();
    let user = loaded
        .config
        .auth_infos
        .iter()
        .find(|u| u.name == "prod-user")
        .and_then(|u| u.auth_info.as_ref())
        .unwrap();
    assert_eq!(
        user.client_certificate.as_deref().map(PathBuf::from),
        Some(fixture("certs/prod.crt"))
    );
}

#[test]
fn missing_files_are_skipped_with_a_warning() {
    let missing = fixture("does-not-exist.yaml");
    let src = KubeconfigSource::from_paths(vec![
        missing.clone(),
        fixture("merge-a.yaml"),
        PathBuf::new(),
    ]);
    let loaded = src.load().unwrap();
    assert_eq!(loaded.files, [fixture("merge-a.yaml")]);
    let list = src.list_contexts().unwrap();
    assert_eq!(list.contexts.len(), 2);
    assert_eq!(list.warnings.len(), 1);
    assert!(list.warnings[0].contains(&missing.display().to_string()));
}

#[test]
fn only_missing_files_gives_empty_list() {
    let list = list(&["nope-1.yaml", "nope-2.yaml"]);
    assert!(list.contexts.is_empty());
    assert_eq!(list.current, None);
    assert_eq!(list.warnings.len(), 2);
}

#[test]
fn empty_file_is_an_empty_config() {
    let list = list(&["empty.yaml"]);
    assert!(list.contexts.is_empty());
    assert!(list.warnings.is_empty());
}

#[test]
fn exec_plugin_eks() {
    let list = list(&["exec-eks.yaml"]);
    let name = "arn:aws:eks:eu-west-1:123456789012:cluster/demo";
    let c = ctx(&list, name);
    assert!(c.is_current);
    assert_eq!(
        c.auth,
        AuthType::Exec {
            command: Some("aws".into())
        }
    );
    assert_eq!(
        c.server.as_deref(),
        Some("https://ABCDEF.gr7.eu-west-1.eks.amazonaws.com")
    );
}

#[test]
fn oidc_and_other_auth_providers() {
    let list = list(&["oidc.yaml"]);
    assert_eq!(ctx(&list, "oidc").auth, AuthType::Oidc);
    assert_eq!(
        ctx(&list, "gke-legacy").auth,
        AuthType::AuthProvider { name: "gcp".into() }
    );
    assert_eq!(list.current, None);
}

#[test]
fn cert_token_basic_none_and_dangling_refs() {
    let list = list(&["cert-token-basic.yaml"]);
    assert_eq!(ctx(&list, "cert").auth, AuthType::ClientCert);
    assert_eq!(ctx(&list, "token-file").auth, AuthType::Token);
    assert_eq!(ctx(&list, "basic").auth, AuthType::Basic);
    let anon = ctx(&list, "anonymous");
    assert_eq!(anon.auth, AuthType::None);
    assert_eq!(anon.user, None);

    let dangling = ctx(&list, "dangling");
    assert_eq!(dangling.auth, AuthType::None);
    assert_eq!(dangling.server, None);

    assert!(list.contexts.iter().all(|c| !c.is_current));
    let w = list.warnings.join("\n");
    assert!(w.contains("missing-cluster"), "{w}");
    assert!(w.contains("missing-user"), "{w}");
    assert!(w.contains("does-not-exist"), "{w}");
}

#[test]
fn invalid_yaml_names_file_and_location() {
    let path = fixture("invalid.yaml");
    let err = KubeconfigSource::from_paths(vec![fixture("merge-a.yaml"), path.clone()])
        .list_contexts()
        .unwrap_err();
    let Error::KubeconfigParse {
        path: err_path,
        line,
        ..
    } = &err
    else {
        panic!("expected KubeconfigParse, got {err:?}");
    };
    assert_eq!(err_path, &path);
    assert!(line.is_some(), "line should be known: {err}");
    let msg = err.to_string();
    assert!(msg.contains("invalid.yaml"), "{msg}");
    assert!(msg.contains("line "), "{msg}");
    assert!(!msg.contains('\n'), "single-line message expected: {msg}");
}

#[test]
fn wrong_shape_is_a_parse_error() {
    let err = KubeconfigSource::from_paths(vec![fixture("wrong-shape.yaml")])
        .load()
        .unwrap_err();
    assert!(matches!(err, Error::KubeconfigParse { .. }), "{err:?}");
    assert!(err.to_string().contains("wrong-shape.yaml"));
}

#[cfg(unix)]
#[test]
fn unreadable_path_is_a_read_error() {
    // A directory exists but is not a readable file.
    let err = KubeconfigSource::from_paths(vec![fixture("")])
        .load()
        .unwrap_err();
    assert!(matches!(err, Error::KubeconfigRead { .. }), "{err:?}");
}

#[test]
fn ipc_json_shape() {
    let list = list(&["exec-eks.yaml", "merge-a.yaml"]);
    let json = serde_json::to_value(&list).unwrap();
    let first = &json["contexts"][0];
    assert_eq!(first["isCurrent"], true);
    assert_eq!(first["auth"]["type"], "exec");
    assert_eq!(first["auth"]["command"], "aws");
    assert!(json["warnings"].is_array());
    let dev = json["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "dev")
        .unwrap();
    assert_eq!(dev["auth"], serde_json::json!({ "type": "token" }));
    assert!(
        !json.to_string().contains("fixture-token"),
        "no secrets in IPC"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn mock_expiring_exec_plugin_detects_auth_and_recovers_live() {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = std::env::temp_dir().join(format!("fg-mock-exec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let state_file = dir.join("logged_in");
    let script_path = dir.join("mock-plugin.sh");
    let kubeconfig_path = dir.join("kubeconfig.yaml");

    let script_content = format!(
        r#"#!/bin/sh
if [ ! -f "{}" ]; then
  echo "error: SSO session has expired. Please login again using 'aws sso login'." >&2
  echo "To sign in, use a web browser to open the page https://device.sso.us-east-1.amazonaws.com/ and enter the code: BCDF-GHJK" >&2
  exit 1
else
  echo '{{"apiVersion":"client.authentication.k8s.io/v1beta1","kind":"ExecCredential","status":{{"token":"mock-token"}}}}'
  exit 0
fi
"#,
        state_file.display()
    );

    let mut script_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(&script_path)
        .unwrap();
    script_file.write_all(script_content.as_bytes()).unwrap();
    drop(script_file);

    let kubeconfig_content = format!(
        r#"apiVersion: v1
clusters:
- cluster:
    insecure-skip-tls-verify: true
    server: https://127.0.0.1:6443
  name: mock-cluster
contexts:
- context:
    cluster: mock-cluster
    user: mock-user
  name: mock-expiring
current-context: mock-expiring
kind: Config
users:
- name: mock-user
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: "{}"
      interactiveMode: Never
"#,
        script_path.display()
    );

    let mut kc_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&kubeconfig_path)
        .unwrap();
    kc_file.write_all(kubeconfig_content.as_bytes()).unwrap();
    drop(kc_file);

    let source = KubeconfigSource::from_paths(vec![kubeconfig_path.clone()]);
    let loaded = source.load().unwrap();
    let manager = ClusterManager::new(source);

    // Initial attempt: plugin fails because state_file does not exist yet
    let client_res = manager.connect_client("mock-expiring").await;
    let Err(err) = client_res else {
        panic!("initial connection must fail due to expired SSO");
    };

    // Verify projection classifies it as StoreAccessState::AuthExpired
    if let Error::Kube(kube_err) = &err {
        let access = classify_kube_error(kube_err, "pods", None);
        assert!(
            matches!(access, Some(StoreAccessState::AuthExpired { ref provider, .. }) if provider == "aws-sso"),
            "expected AuthExpired, got: {access:?}"
        );
    }

    // Verify live diagnostic detects auth error and parsed device code
    let report =
        diagnose_kubeconfig_context_live(Err(err), &loaded.config, "mock-expiring", "process-env")
            .await;

    let auth_error = report.auth_error.expect("auth_error must be populated");
    assert_eq!(auth_error.provider, "aws-sso");
    assert_eq!(auth_error.login_command, "aws sso login");
    let dc = auth_error.device_code.expect("device code must be parsed");
    assert_eq!(dc.url, "https://device.sso.us-east-1.amazonaws.com/");
    assert_eq!(dc.user_code, "BCDF-GHJK");

    // Now simulate successful sign-in by writing the state file
    std::fs::write(&state_file, "ok").unwrap();

    // Call resync_context to invalidate the client cache
    manager.resync_context("mock-expiring");

    // Next connection attempt succeeds without restarting the app!
    let recovered_client = manager.connect_client("mock-expiring").await;
    assert!(
        recovered_client.is_ok(),
        "recovered connection must succeed after auth refresh"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
