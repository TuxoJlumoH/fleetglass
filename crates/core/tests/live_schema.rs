//! 05-04 live test: resource schemas from the API server's OpenAPI v3 document, for a
//! built-in kind (apps/v1 Deployment) and a CRD created (and deleted) by the test.
//!
//! Run: `KUBECONFIG=$PWD/.dev/kubeconfig cargo test -p fleetglass-core --features integration --test live_schema -- --nocapture`
//!
//! Uses `KUBECONFIG` if set, else `.dev/kubeconfig` (never `~/.kube/config`), and only
//! the `kind-fleetglass-dev` context.

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetglass_core::api::{ResourceSchemaResult, SchemaUnavailableReason};
use fleetglass_core::cluster::ClusterManager;
use fleetglass_core::kubeconfig::KubeconfigSource;
use futures::FutureExt;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{Api, DeleteParams, PostParams};
use kube::runtime::wait::{await_condition, conditions};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(60);

fn kubeconfig_path() -> PathBuf {
    std::env::var("KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/kubeconfig"))
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}{:x}", std::process::id(), nanos & 0xff_ffff)
}

fn available(result: ResourceSchemaResult) -> (Value, bool) {
    match result {
        ResourceSchemaResult::Available {
            schema_json,
            truncated,
            ..
        } => (serde_json::from_str(&schema_json).unwrap(), truncated),
        other => panic!("expected an available schema, got {other:?}"),
    }
}

async fn deployment_schema(manager: &ClusterManager, context: &str) {
    let started = Instant::now();
    let (schema, truncated) = available(
        manager
            .resource_schema(context, "apps/v1", "Deployment")
            .await,
    );
    println!(
        "apps/v1 Deployment: {} bytes in {:?}, truncated={truncated}",
        serde_json::to_string(&schema).unwrap().len(),
        started.elapsed()
    );
    assert!(!truncated);
    let text = serde_json::to_string(&schema).unwrap();
    assert!(!text.contains("$ref"));
    let spec = &schema["properties"]["spec"];
    assert_eq!(spec["properties"]["replicas"]["type"], "integer");
    let containers =
        &spec["properties"]["template"]["properties"]["spec"]["properties"]["containers"];
    assert_eq!(containers["type"], "array");
    assert_eq!(
        containers["items"]["properties"]["imagePullPolicy"]["type"],
        "string"
    );
    assert_eq!(
        schema["properties"]["metadata"]["properties"]["labels"]["additionalProperties"]["type"],
        "string"
    );
    assert_eq!(manager.schema_cache_len(), 1);

    // Second call is served from the cache (same document hash).
    let again = manager
        .resource_schema(context, "apps/v1", "Deployment")
        .await;
    assert_eq!(available(again).0, schema);
    assert_eq!(manager.schema_cache_len(), 1);

    // Unknown kind / group-version: honest "unavailable", no error.
    for (api_version, kind) in [("apps/v1", "NoSuchKind"), ("nope.example.com/v1", "Thing")] {
        match manager.resource_schema(context, api_version, kind).await {
            ResourceSchemaResult::Unavailable { reason, .. } => {
                assert_eq!(
                    reason,
                    SchemaUnavailableReason::NotFound,
                    "{api_version} {kind}"
                );
            }
            other => panic!("expected unavailable for {api_version} {kind}, got {other:?}"),
        }
    }
}

async fn crd_schema(manager: &ClusterManager, context: &str, group: &str, crd_name: &str) {
    let client = manager.connect_client(context).await.unwrap();
    let crds: Api<CustomResourceDefinition> = Api::all(client);
    let crd: CustomResourceDefinition = serde_json::from_value(serde_json::json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": { "name": crd_name },
        "spec": {
            "group": group,
            "names": { "kind": "SchemaWidget", "plural": "schemawidgets", "singular": "schemawidget" },
            "scope": "Namespaced",
            "versions": [{
                "name": "v1",
                "served": true,
                "storage": true,
                "schema": { "openAPIV3Schema": {
                    "type": "object",
                    "properties": {
                        "spec": {
                            "type": "object",
                            "required": ["size"],
                            "properties": {
                                "size": { "x-kubernetes-int-or-string": true, "anyOf": [{ "type": "integer" }, { "type": "string" }] },
                                "mode": { "type": "string", "enum": ["fast", "safe"], "description": "How to run." },
                                "config": { "type": "object", "x-kubernetes-preserve-unknown-fields": true }
                            }
                        }
                    }
                }}
            }]
        }
    }))
    .unwrap();
    crds.create(&PostParams::default(), &crd).await.unwrap();
    tokio::time::timeout(
        WAIT,
        await_condition(crds.clone(), crd_name, conditions::is_crd_established()),
    )
    .await
    .expect("CRD established in time")
    .unwrap();

    // The OpenAPI document of a new group-version is published asynchronously.
    let api_version = format!("{group}/v1");
    let started = Instant::now();
    let schema = loop {
        match manager
            .resource_schema(context, &api_version, "SchemaWidget")
            .await
        {
            ResourceSchemaResult::Available { schema_json, .. } => {
                break serde_json::from_str::<Value>(&schema_json).unwrap();
            }
            ResourceSchemaResult::Unavailable { reason, message } => {
                assert_eq!(reason, SchemaUnavailableReason::NotFound, "{message}");
                assert!(
                    started.elapsed() < WAIT,
                    "CRD schema not published: {message}"
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    };
    println!(
        "{api_version} SchemaWidget published after {:?}",
        started.elapsed()
    );
    let spec = &schema["properties"]["spec"];
    assert_eq!(spec["required"], serde_json::json!(["size"]));
    assert_eq!(
        spec["properties"]["size"]["x-kubernetes-int-or-string"],
        true
    );
    assert_eq!(
        spec["properties"]["mode"]["enum"],
        serde_json::json!(["fast", "safe"])
    );
    assert_eq!(spec["properties"]["mode"]["description"], "How to run.");
    assert_eq!(
        spec["properties"]["config"]["x-kubernetes-preserve-unknown-fields"],
        true
    );
    // The server adds the standard object fields to CRD schemas.
    assert!(schema["properties"]["metadata"].is_object());
    assert_eq!(schema["properties"]["apiVersion"]["type"], "string");

    // Reconnecting drops the cached documents of the context.
    assert!(manager.schema_cache_len() >= 1);
    manager.resync_context(context);
    assert_eq!(manager.schema_cache_len(), 0);
}

#[tokio::test]
async fn live_resource_schema_builtin_and_crd() {
    let kubeconfig = kubeconfig_path();
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
        .expect("current-context in the kubeconfig");
    assert_eq!(context, "kind-fleetglass-dev", "only the dev kind cluster");
    let manager = Arc::new(ClusterManager::new(source));

    let group = format!("fg{}.schema.test", unique_suffix());
    let crd_name = format!("schemawidgets.{group}");

    let outcome = AssertUnwindSafe(async {
        deployment_schema(&manager, &context).await;
        crd_schema(&manager, &context, &group, &crd_name).await;
    })
    .catch_unwind()
    .await;

    if let Ok(client) = manager.connect_client(&context).await {
        let crds: Api<CustomResourceDefinition> = Api::all(client);
        let _ = crds.delete(&crd_name, &DeleteParams::default()).await;
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
