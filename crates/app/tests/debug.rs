use fleetglass_app::AppState;
use fleetglass_core::api::ContextSettings;
use fleetglass_core::api::debug::DebugPodRequest;
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::error::Error;
use fleetglass_core::kubeconfig::KubeconfigSource;
use std::sync::Arc;

#[test]
fn test_debug_pod_preview_and_safety() {
    let source = KubeconfigSource::from_paths(Vec::new());
    let manager = Arc::new(ClusterManager::new(source));
    let state = AppState::with_manager(Arc::clone(&manager));

    let request = DebugPodRequest {
        context: "test-ctx".to_string(),
        namespace: "default".to_string(),
        pod: "test-pod".to_string(),
        container_name: "debugger".to_string(),
        image: "busybox:1.37".to_string(),
        target_container: Some("app".to_string()),
        command: vec!["sh".to_string(), "-c".to_string(), "echo ok".to_string()],
        confirmed_name: Some("test-pod".to_string()),
    };

    let preview = fleetglass_core::actions::debug::preview_debug_pod_kubectl(&request);
    assert!(preview.contains("kubectl --context test-ctx -n default debug"));
    assert!(preview.contains("pod/test-pod"));
    assert!(preview.contains("--image=busybox:1.37"));
    assert!(preview.contains("--container=debugger"));
    assert!(preview.contains("--target=app"));

    // Verify read-only safety guard blocks before cluster access
    let readonly_settings = ContextSettings {
        read_only: true,
        ..ContextSettings::default()
    };
    let result = tauri::async_runtime::block_on(async {
        fleetglass_core::actions::debug::debug_pod_and_audit(
            &state.manager,
            Arc::clone(&state.audit),
            &request,
            &readonly_settings,
        )
        .await
    });
    assert!(matches!(result, Err(Error::ReadOnlyContext { .. })));
}

#[test]
fn test_debug_node_preview_and_safety() {
    use fleetglass_core::api::debug::DebugNodeRequest;

    let request = DebugNodeRequest {
        context: "prod-cluster".to_string(),
        node: "node-worker-1".to_string(),
        image: "busybox:1.37".to_string(),
        command: vec!["chroot".to_string(), "/host".to_string()],
        confirmed_name: Some("node-worker-1".to_string()),
        namespace: Some("default".to_string()),
        cols: None,
        rows: None,
    };

    let preview = fleetglass_core::actions::debug::preview_debug_node_kubectl(&request);
    assert_eq!(
        preview,
        "kubectl --context prod-cluster debug node/node-worker-1 -it --image=busybox:1.37 -- chroot /host"
    );

    let readonly_settings = ContextSettings {
        read_only: true,
        ..ContextSettings::default()
    };

    let guard_res = fleetglass_core::actions::enforce_write_safety(
        &request.context,
        &readonly_settings,
        &request.node,
        request.confirmed_name.as_deref(),
    );
    assert!(matches!(guard_res, Err(Error::ReadOnlyContext { .. })));

    let protected_settings = ContextSettings {
        protected: true,
        ..ContextSettings::default()
    };
    let unconfirmed_res = fleetglass_core::actions::enforce_write_safety(
        &request.context,
        &protected_settings,
        &request.node,
        Some("wrong-node"),
    );
    assert!(matches!(
        unconfirmed_res,
        Err(Error::ConfirmationRequired { .. })
    ));

    let confirmed_res = fleetglass_core::actions::enforce_write_safety(
        &request.context,
        &protected_settings,
        &request.node,
        Some("node-worker-1"),
    );
    assert!(confirmed_res.is_ok());
}
