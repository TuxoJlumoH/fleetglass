//! Optional read-only Model Context Protocol (MCP) server engine (`P3-05`).
//!
//! Exposes a strictly read-only MCP server over stdin/stdout using JSON-RPC 2.0.
//! Supports `list_contexts`, `query_resources`, `compare_resources`, and `get_audit_log`.
//! Non-negotiables: Zero mutating tools, local audit logging for every MCP query,
//! no telemetry, and completely off by default.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::api::{Page, QuerySpec, StoreAccessState};
use crate::audit::AuditLogStore;
use crate::cluster::{ClusterManager, DEFAULT_REAP_INTERVAL};
use crate::kubeconfig::KubeconfigSource;

/// JSON-RPC 2.0 request payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcResponse {
    /// Creates a successful JSON-RPC 2.0 response.
    #[must_use]
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Creates an error JSON-RPC 2.0 response.
    #[must_use]
    pub fn error(id: Option<serde_json::Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Maximum line length accepted over stdio (10MB).
pub const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;

/// Maximum YAML payload length for resource comparison (5MB).
pub const MAX_COMPARE_YAML_BYTES: usize = 5 * 1024 * 1024;

/// Records a query into the local audit log if present (`action: "mcp:<tool>"`).
#[allow(clippy::too_many_arguments)]
fn record_audit(
    audit: Option<&AuditLogStore>,
    source: &KubeconfigSource,
    tool: &str,
    cluster: &str,
    kind: &str,
    namespace: Option<&str>,
    kubectl: &str,
    success: bool,
    error: Option<&str>,
) {
    let Some(store) = audit else { return };
    let user = if !cluster.is_empty() {
        source.context_user(cluster)
    } else {
        None
    };
    let entry = crate::api::AuditEntry {
        // Assigned by the store so ids stay unique across processes sharing the file.
        id: 0,
        timestamp: crate::audit::now_rfc3339(),
        cluster: cluster.to_string(),
        user,
        action: format!("mcp:{tool}"),
        kind: kind.to_string(),
        namespace: namespace.map(|s| s.to_string()),
        name: tool.to_string(),
        kubectl: kubectl.to_string(),
        success,
        error: error.map(|s| s.to_string()),
    };

    store.append_or_warn(&entry);
}

/// How long `query_resources` waits for a freshly started watcher's initial list.
pub const MCP_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Interval between store checks while waiting for watchers to settle.
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// Starts the watchers of `spec` and returns its page once no store is
/// [`StoreAccessState::Loading`] or [`StoreAccessState::Reconnecting`] any more,
/// or after `timeout` (then with those states, which [`access_summary`] reports).
async fn watch_and_query(manager: &ClusterManager, spec: &QuerySpec, timeout: Duration) -> Page {
    manager.ensure_watching_spec(spec).await;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let page = manager.query(spec).await;
        let loading = page.access.iter().any(|a| a.access.is_syncing());
        if !loading || tokio::time::Instant::now() >= deadline {
            return page;
        }
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// Human-readable note for stores that are not ready, so an empty `rows` is never
/// mistaken for "no objects" (non-negotiable 5). `None` when every store is ready.
fn access_summary(manager: &ClusterManager, page: &Page) -> Option<String> {
    if page.access.is_empty() {
        return Some("Not watched: no store matches this cluster and kind.".to_owned());
    }
    let notes: Vec<String> = page
        .access
        .iter()
        .filter_map(|a| {
            let note = match &a.access {
                StoreAccessState::Ready => return None,
                StoreAccessState::Loading { rows_so_far } => format!(
                    "still loading (initial list not finished, {rows_so_far} rows so far); retry shortly"
                ),
                // A restarted watcher of a synced store keeps the last complete list.
                StoreAccessState::Reconnecting => {
                    let rows = manager
                        .get_store(&a.cluster, &a.kind)
                        .map_or(0, |store| store.row_count());
                    format!("reconnecting (showing last known rows: {rows}); retry shortly")
                }
                StoreAccessState::Forbidden { message, .. }
                | StoreAccessState::NotFound { message, .. }
                | StoreAccessState::Unsupported { message, .. }
                | StoreAccessState::Error {
                    message,
                    partial_rows: None,
                } => message.clone(),
                StoreAccessState::Error {
                    message,
                    partial_rows: Some(n),
                } => format!(
                    "{message} (initial list failed: only {n} rows received before the error, incomplete)"
                ),
                StoreAccessState::AuthExpired {
                    hint,
                    login_command,
                    ..
                } => format!("auth expired: {hint} (command: {login_command})"),
            };
            Some(format!("- {} in {}: {note}", a.kind, a.cluster))
        })
        .collect();
    if notes.is_empty() {
        None
    } else {
        Some(format!(
            "Rows may be missing or incomplete:\n{}",
            notes.join("\n")
        ))
    }
}

/// Fetches workload or pod logs bounded by `tail_lines` (1..=500).
async fn fetch_mcp_workload_logs(
    client: &kube::Client,
    namespace: &str,
    workload_kind: &str,
    workload_name: &str,
    container: Option<&str>,
    tail_lines: u32,
) -> Result<String, String> {
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{Api, ListParams, LogParams};

    let pods_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let lp = LogParams {
        container: container.map(str::to_owned),
        tail_lines: Some(i64::from(tail_lines)),
        ..LogParams::default()
    };

    if workload_kind.eq_ignore_ascii_case("Pod") {
        return pods_api
            .logs(workload_name, &lp)
            .await
            .map_err(|err| format!("cannot read logs of pod {namespace}/{workload_name}: {err}"));
    }

    let selector = crate::logs_mux::resolve_label_selector(
        client,
        namespace,
        &crate::api::WorkloadSelector::Owner {
            kind: workload_kind.to_owned(),
            name: workload_name.to_owned(),
        },
    )
    .await?;

    let pod_list = pods_api
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(|err| format!("failed to list pods for {workload_kind}/{workload_name}: {err}"))?;

    if pod_list.items.is_empty() {
        return Ok(format!(
            "No pods found for {workload_kind}/{workload_name} in namespace {namespace}"
        ));
    }

    if pod_list.items.len() == 1 {
        let pod_name = pod_list.items[0]
            .metadata
            .name
            .as_deref()
            .unwrap_or(workload_name);
        return pods_api
            .logs(pod_name, &lp)
            .await
            .map_err(|err| format!("cannot read logs of pod {namespace}/{pod_name}: {err}"));
    }

    let mut all_lines: Vec<String> = Vec::new();
    for pod in &pod_list.items {
        let Some(pod_name) = pod.metadata.name.as_deref() else {
            continue;
        };
        match pods_api.logs(pod_name, &lp).await {
            Ok(log_content) => {
                for line in log_content.lines() {
                    all_lines.push(format!("[{pod_name}] {line}"));
                }
            }
            Err(err) => {
                all_lines.push(format!("[{pod_name}] <error reading logs: {err}>"));
            }
        }
    }

    if all_lines.len() > tail_lines as usize {
        let start = all_lines.len() - (tail_lines as usize);
        all_lines = all_lines[start..].to_vec();
    }

    Ok(all_lines.join("\n"))
}

enum ReadLineResult {
    Ok(String),
    ExceededLimit,
    Eof,
}

fn read_line_capped<R: std::io::BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<ReadLineResult> {
    let mut buf = Vec::new();
    let mut exceeded = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buf.is_empty() && !exceeded {
                return Ok(ReadLineResult::Eof);
            }
            break;
        }

        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if !exceeded && buf.len() + take <= max_bytes {
                buf.extend_from_slice(&available[..take]);
            } else {
                exceeded = true;
            }
            reader.consume(take);
            break;
        } else {
            let take = available.len();
            if !exceeded && buf.len() + take <= max_bytes {
                buf.extend_from_slice(available);
            } else {
                exceeded = true;
            }
            reader.consume(take);
        }
    }

    if exceeded {
        Ok(ReadLineResult::ExceededLimit)
    } else {
        Ok(ReadLineResult::Ok(
            String::from_utf8_lossy(&buf).into_owned(),
        ))
    }
}

/// Handles a single line of JSON-RPC protocol exchange for the MCP server.
///
/// Returns `Some(response_json)` when a response must be sent, or `None` for
/// notifications and empty lines.
///
/// `query_resources` starts the watcher of the requested `(cluster, kind)` (read-only
/// list + watch) and waits up to [`MCP_SETTLE_TIMEOUT`] for its initial list, so a
/// cluster the app never opened returns its objects or an honest access state
/// (forbidden, connection error, still loading) instead of an empty page.
pub async fn handle_json_rpc(
    line: &str,
    manager: &ClusterManager,
    source: &KubeconfigSource,
    audit: Option<&AuditLogStore>,
) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if line.len() > MAX_LINE_BYTES {
        record_audit(
            audit,
            source,
            "unknown",
            "",
            "",
            None,
            "",
            false,
            Some("Parse error: request line exceeds 10MB limit"),
        );
        let err_resp =
            RpcResponse::error(None, -32700, "Parse error: request line exceeds 10MB limit");
        return serde_json::to_string(&err_resp).ok();
    }

    let req: RpcRequest = match serde_json::from_str(trimmed) {
        Ok(r) => r,
        Err(err) => {
            let err_resp = RpcResponse::error(None, -32700, format!("Parse error: {err}"));
            return serde_json::to_string(&err_resp).ok();
        }
    };

    match req.method.as_str() {
        "initialize" => {
            let result = serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "fleetglass-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            });
            let resp = RpcResponse::success(req.id, result);
            serde_json::to_string(&resp).ok()
        }
        "notifications/initialized" | "initialized" => None,
        "tools/list" => {
            let tools = serde_json::json!([
                {
                    "name": "list_contexts",
                    "description": "List all available kubeconfig contexts and current context",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "query_resources",
                    "description": "Query resources from a cluster (read-only windowed query)",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "cluster": {
                                "type": "string",
                                "description": "Target cluster or context name"
                            },
                            "kind": {
                                "type": "string",
                                "description": "Kubernetes resource kind (e.g. Pod, Deployment, Service)"
                            },
                            "namespace": {
                                "type": "string",
                                "description": "Optional namespace filter"
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Maximum number of rows to return (max 100)",
                                "maximum": 100
                            }
                        },
                        "required": ["cluster", "kind"]
                    }
                },
                {
                    "name": "get_resource_yaml",
                    "description": "Fetch the YAML definition of a Kubernetes resource (secrets are redacted)",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "cluster": {
                                "type": "string",
                                "description": "Target cluster or context name"
                            },
                            "kind": {
                                "type": "string",
                                "description": "Kubernetes resource kind (e.g. Pod, Deployment, Service, Secret)"
                            },
                            "name": {
                                "type": "string",
                                "description": "Resource name"
                            },
                            "namespace": {
                                "type": "string",
                                "description": "Optional namespace (required for namespaced resources)"
                            }
                        },
                        "required": ["cluster", "kind", "name"]
                    }
                },
                {
                    "name": "get_workload_logs",
                    "description": "Fetch log lines for a Kubernetes workload or pod",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "cluster": {
                                "type": "string",
                                "description": "Target cluster or context name"
                            },
                            "namespace": {
                                "type": "string",
                                "description": "Kubernetes namespace"
                            },
                            "workloadKind": {
                                "type": "string",
                                "description": "Workload kind (e.g. Pod, Deployment, StatefulSet, DaemonSet)"
                            },
                            "workloadName": {
                                "type": "string",
                                "description": "Name of the workload or pod"
                            },
                            "container": {
                                "type": "string",
                                "description": "Optional container name"
                            },
                            "tailLines": {
                                "type": "integer",
                                "description": "Number of lines to return (1-500, default 100)",
                                "minimum": 1,
                                "maximum": 500
                            }
                        },
                        "required": ["cluster", "namespace", "workloadKind", "workloadName"]
                    }
                },
                {
                    "name": "diff_resources",
                    "description": "Compare two Kubernetes YAML resource definitions and produce a unified diff",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "leftYaml": {
                                "type": "string",
                                "description": "Baseline resource YAML"
                            },
                            "rightYaml": {
                                "type": "string",
                                "description": "Target resource YAML"
                            }
                        },
                        "required": ["leftYaml", "rightYaml"]
                    }
                },
                {
                    "name": "get_audit_log",
                    "description": "Query the local append-only audit log entries",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "offset": {
                                "type": "integer",
                                "description": "Pagination offset"
                            },
                            "limit": {
                                "type": "integer",
                                "description": "Maximum entries to return (max 100)",
                                "maximum": 100
                            }
                        }
                    }
                }
            ]);
            let resp = RpcResponse::success(req.id, serde_json::json!({ "tools": tools }));
            serde_json::to_string(&resp).ok()
        }
        "tools/call" => {
            let params_obj = req.params.as_ref().and_then(|p| p.as_object());
            let tool_name = params_obj
                .and_then(|o| o.get("name"))
                .and_then(|v| v.as_str());

            let Some(tool_name) = tool_name else {
                record_audit(
                    audit,
                    source,
                    "unknown",
                    "",
                    "",
                    None,
                    "",
                    false,
                    Some("Invalid params: missing tool 'name' field"),
                );
                let err_resp =
                    RpcResponse::error(req.id, -32602, "Invalid params: missing tool 'name' field");
                return serde_json::to_string(&err_resp).ok();
            };

            let empty_map = serde_json::Map::new();
            let args = params_obj
                .and_then(|o| o.get("arguments"))
                .and_then(|v| v.as_object())
                .or(params_obj)
                .unwrap_or(&empty_map);

            match tool_name {
                "list_contexts" => {
                    let context_list = match source.list_contexts() {
                        Ok(list) => list,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                "list_contexts",
                                "",
                                "context",
                                None,
                                "kubectl config get-contexts",
                                false,
                                Some(&err.to_string()),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to list contexts: {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    let mut text = String::new();
                    text.push_str("| Current | Context | Cluster | User | Namespace |\n");
                    text.push_str("|:---:|---|---|---|---|\n");
                    for ctx in &context_list.contexts {
                        let current_marker = if ctx.is_current { "*" } else { " " };
                        let user = ctx.user.as_deref().unwrap_or("-");
                        let ns = ctx.namespace.as_deref().unwrap_or("-");
                        text.push_str(&format!(
                            "| {} | {} | {} | {} | {} |\n",
                            current_marker, ctx.name, ctx.cluster, user, ns
                        ));
                    }
                    if !context_list.warnings.is_empty() {
                        text.push_str("\nWarnings:\n");
                        for w in &context_list.warnings {
                            text.push_str(&format!("- {w}\n"));
                        }
                    }

                    record_audit(
                        audit,
                        source,
                        "list_contexts",
                        "",
                        "context",
                        None,
                        "kubectl config get-contexts",
                        true,
                        None,
                    );

                    let result = serde_json::json!({
                        "content": [
                            {
                                "type": "text",
                                "text": text
                            }
                        ]
                    });
                    let resp = RpcResponse::success(req.id, result);
                    serde_json::to_string(&resp).ok()
                }
                "query_resources" => {
                    let cluster = args.get("cluster").and_then(|v| v.as_str());
                    let kind = args.get("kind").and_then(|v| v.as_str());
                    let namespace = args.get("namespace").and_then(|v| v.as_str());
                    let limit = args
                        .get("limit")
                        .and_then(|v| v.as_u64())
                        .map(|l| u32::try_from(l).unwrap_or(100).clamp(1, 100))
                        .unwrap_or(50);

                    let (cluster, kind) = match (cluster, kind) {
                        (Some(c), Some(k)) => (c, k),
                        _ => {
                            record_audit(
                                audit,
                                source,
                                "query_resources",
                                cluster.unwrap_or(""),
                                kind.unwrap_or(""),
                                namespace,
                                "",
                                false,
                                Some("Invalid params: 'cluster' and 'kind' are required"),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32602,
                                "Invalid params: 'cluster' and 'kind' are required",
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    let kubectl_cmd = match namespace {
                        Some(ns) => format!("kubectl get {kind} -n {ns} --context {cluster}"),
                        None => format!("kubectl get {kind} -A --context {cluster}"),
                    };

                    let spec = QuerySpec {
                        clusters: vec![cluster.to_string()],
                        kinds: vec![kind.to_string()],
                        namespaces: namespace.map(|ns| vec![ns.to_string()]).unwrap_or_default(),
                        label_selector: None,
                        text_filter: None,
                        sort: None,
                        offset: 0,
                        limit,
                        statuses: Vec::new(),
                    };
                    let page = watch_and_query(manager, &spec, MCP_SETTLE_TIMEOUT).await;
                    let summary = access_summary(manager, &page);

                    let text = match serde_json::to_string_pretty(&page) {
                        Ok(json) => json,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                "query_resources",
                                cluster,
                                kind,
                                namespace,
                                &kubectl_cmd,
                                false,
                                Some(&err.to_string()),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to serialize query page: {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    record_audit(
                        audit,
                        source,
                        "query_resources",
                        cluster,
                        kind,
                        namespace,
                        &kubectl_cmd,
                        true,
                        None,
                    );

                    let mut content = vec![serde_json::json!({ "type": "text", "text": text })];
                    if let Some(summary) = summary {
                        content.push(serde_json::json!({ "type": "text", "text": summary }));
                    }
                    let result = serde_json::json!({ "content": content });
                    let resp = RpcResponse::success(req.id, result);
                    serde_json::to_string(&resp).ok()
                }
                "get_resource_yaml" => {
                    let cluster = args.get("cluster").and_then(|v| v.as_str());
                    let kind = args.get("kind").and_then(|v| v.as_str());
                    let name = args.get("name").and_then(|v| v.as_str());
                    let namespace = args.get("namespace").and_then(|v| v.as_str());

                    let (cluster, kind, name) = match (cluster, kind, name) {
                        (Some(c), Some(k), Some(n)) => (c, k, n),
                        _ => {
                            record_audit(
                                audit,
                                source,
                                "get_resource_yaml",
                                cluster.unwrap_or(""),
                                kind.unwrap_or(""),
                                namespace,
                                "",
                                false,
                                Some("Invalid params: 'cluster', 'kind', and 'name' are required"),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32602,
                                "Invalid params: 'cluster', 'kind', and 'name' are required",
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    let kubectl_cmd = match namespace {
                        Some(ns) => {
                            format!("kubectl get {kind} {name} -n {ns} -o yaml --context {cluster}")
                        }
                        None => format!("kubectl get {kind} {name} -o yaml --context {cluster}"),
                    };

                    let client = match manager.connect_client(cluster).await {
                        Ok(c) => c,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                "get_resource_yaml",
                                cluster,
                                kind,
                                namespace,
                                &kubectl_cmd,
                                false,
                                Some(&err.to_string()),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to connect to cluster '{cluster}': {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    let yaml_res = crate::detail::fetch_resource_yaml_redacted(
                        &client, kind, namespace, name, true,
                    )
                    .await;

                    let yaml = match yaml_res {
                        Ok(y) => y,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                "get_resource_yaml",
                                cluster,
                                kind,
                                namespace,
                                &kubectl_cmd,
                                false,
                                Some(&err.to_string()),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to fetch YAML for {kind}/{name}: {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    record_audit(
                        audit,
                        source,
                        "get_resource_yaml",
                        cluster,
                        kind,
                        namespace,
                        &kubectl_cmd,
                        true,
                        None,
                    );

                    let result = serde_json::json!({
                        "content": [
                            {
                                "type": "text",
                                "text": yaml
                            }
                        ]
                    });
                    let resp = RpcResponse::success(req.id, result);
                    serde_json::to_string(&resp).ok()
                }
                "get_workload_logs" => {
                    let cluster = args.get("cluster").and_then(|v| v.as_str());
                    let namespace = args.get("namespace").and_then(|v| v.as_str());
                    let workload_kind = args
                        .get("workloadKind")
                        .or_else(|| args.get("workload_kind"))
                        .and_then(|v| v.as_str());
                    let workload_name = args
                        .get("workloadName")
                        .or_else(|| args.get("workload_name"))
                        .and_then(|v| v.as_str());
                    let container = args.get("container").and_then(|v| v.as_str());
                    let tail_lines = args
                        .get("tailLines")
                        .or_else(|| args.get("tail_lines"))
                        .and_then(|v| v.as_u64())
                        .map(|t| u32::try_from(t).unwrap_or(500).clamp(1, 500))
                        .unwrap_or(100);

                    let (cluster, namespace, workload_kind, workload_name) = match (
                        cluster,
                        namespace,
                        workload_kind,
                        workload_name,
                    ) {
                        (Some(c), Some(ns), Some(k), Some(n)) => (c, ns, k, n),
                        _ => {
                            record_audit(
                                audit,
                                source,
                                "get_workload_logs",
                                cluster.unwrap_or(""),
                                workload_kind.unwrap_or(""),
                                namespace,
                                "",
                                false,
                                Some(
                                    "Invalid params: 'cluster', 'namespace', 'workloadKind', and 'workloadName' are required",
                                ),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32602,
                                "Invalid params: 'cluster', 'namespace', 'workloadKind', and 'workloadName' are required",
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    let container_arg = container.map(|c| format!(" -c {c}")).unwrap_or_default();
                    let kubectl_cmd = format!(
                        "kubectl logs {workload_kind}/{workload_name} -n {namespace}{container_arg} --tail={tail_lines} --context {cluster}"
                    );

                    let client = match manager.connect_client(cluster).await {
                        Ok(c) => c,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                "get_workload_logs",
                                cluster,
                                workload_kind,
                                Some(namespace),
                                &kubectl_cmd,
                                false,
                                Some(&err.to_string()),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to connect to cluster '{cluster}': {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    let logs_res = fetch_mcp_workload_logs(
                        &client,
                        namespace,
                        workload_kind,
                        workload_name,
                        container,
                        tail_lines,
                    )
                    .await;

                    let logs = match logs_res {
                        Ok(l) => l,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                "get_workload_logs",
                                cluster,
                                workload_kind,
                                Some(namespace),
                                &kubectl_cmd,
                                false,
                                Some(&err),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to fetch workload logs: {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    record_audit(
                        audit,
                        source,
                        "get_workload_logs",
                        cluster,
                        workload_kind,
                        Some(namespace),
                        &kubectl_cmd,
                        true,
                        None,
                    );

                    let result = serde_json::json!({
                        "content": [
                            {
                                "type": "text",
                                "text": logs
                            }
                        ]
                    });
                    let resp = RpcResponse::success(req.id, result);
                    serde_json::to_string(&resp).ok()
                }
                "diff_resources" | "compare_resources" => {
                    let left_yaml = args
                        .get("leftYaml")
                        .or_else(|| args.get("left_yaml"))
                        .and_then(|v| v.as_str());
                    let right_yaml = args
                        .get("rightYaml")
                        .or_else(|| args.get("right_yaml"))
                        .and_then(|v| v.as_str());

                    let (left, right) = match (left_yaml, right_yaml) {
                        (Some(l), Some(r)) => (l, r),
                        _ => {
                            record_audit(
                                audit,
                                source,
                                tool_name,
                                "",
                                "diff",
                                None,
                                "diff",
                                false,
                                Some("Invalid params: 'leftYaml' and 'rightYaml' are required"),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32602,
                                "Invalid params: 'leftYaml' and 'rightYaml' are required",
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    if left.len() > MAX_COMPARE_YAML_BYTES || right.len() > MAX_COMPARE_YAML_BYTES {
                        record_audit(
                            audit,
                            source,
                            tool_name,
                            "",
                            "diff",
                            None,
                            "diff",
                            false,
                            Some("YAML payload exceeds 5MB limit"),
                        );
                        let resp = RpcResponse::error(
                            req.id,
                            -32602,
                            "Invalid params: 'leftYaml' and 'rightYaml' must each not exceed 5MB",
                        );
                        return serde_json::to_string(&resp).ok();
                    }

                    let diff_result = crate::detail::compare_resources(left, right);
                    let text = match serde_json::to_string_pretty(&diff_result) {
                        Ok(json) => json,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                tool_name,
                                "",
                                "diff",
                                None,
                                "diff",
                                false,
                                Some(&err.to_string()),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to serialize diff result: {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    record_audit(
                        audit, source, tool_name, "", "diff", None, "diff", true, None,
                    );

                    let result = serde_json::json!({
                        "content": [
                            {
                                "type": "text",
                                "text": text
                            }
                        ]
                    });
                    let resp = RpcResponse::success(req.id, result);
                    serde_json::to_string(&resp).ok()
                }
                "get_audit_log" => {
                    let offset = args
                        .get("offset")
                        .and_then(|v| v.as_u64())
                        .map(|o| u32::try_from(o).unwrap_or(0))
                        .unwrap_or(0);
                    let limit = args
                        .get("limit")
                        .and_then(|v| v.as_u64())
                        .map(|l| u32::try_from(l).unwrap_or(100).clamp(1, 100))
                        .unwrap_or(50);

                    let page = match audit {
                        Some(store) => match store.query(offset, limit, None, None) {
                            Ok(page) => page,
                            Err(err) => {
                                record_audit(
                                    audit,
                                    source,
                                    "get_audit_log",
                                    "",
                                    "audit",
                                    None,
                                    "fleetglass audit log",
                                    false,
                                    Some(&err.to_string()),
                                );
                                let resp = RpcResponse::error(
                                    req.id,
                                    -32603,
                                    format!("Failed to query audit log: {err}"),
                                );
                                return serde_json::to_string(&resp).ok();
                            }
                        },
                        None => crate::api::AuditLogPage {
                            total: 0,
                            offset,
                            entries: Vec::new(),
                        },
                    };

                    let text = match serde_json::to_string_pretty(&page) {
                        Ok(json) => json,
                        Err(err) => {
                            record_audit(
                                audit,
                                source,
                                "get_audit_log",
                                "",
                                "audit",
                                None,
                                "fleetglass audit log",
                                false,
                                Some(&err.to_string()),
                            );
                            let resp = RpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to serialize audit log page: {err}"),
                            );
                            return serde_json::to_string(&resp).ok();
                        }
                    };

                    record_audit(
                        audit,
                        source,
                        "get_audit_log",
                        "",
                        "audit",
                        None,
                        "fleetglass audit log",
                        true,
                        None,
                    );

                    let result = serde_json::json!({
                        "content": [
                            {
                                "type": "text",
                                "text": text
                            }
                        ]
                    });
                    let resp = RpcResponse::success(req.id, result);
                    serde_json::to_string(&resp).ok()
                }
                // Any tool not in the explicit read-only list is rejected with MethodNotFound (-32601)
                _ => {
                    let msg = format!("Method not found: unknown or mutating tool '{tool_name}'");
                    record_audit(
                        audit,
                        source,
                        tool_name,
                        "",
                        "",
                        None,
                        "",
                        false,
                        Some(&msg),
                    );
                    let resp = RpcResponse::error(req.id, -32601, msg);
                    serde_json::to_string(&resp).ok()
                }
            }
        }
        _ => {
            let resp = RpcResponse::error(
                req.id,
                -32601,
                format!("Method not found: '{}'", req.method),
            );
            serde_json::to_string(&resp).ok()
        }
    }
}

/// Runs the Model Context Protocol (MCP) server over standard input and output.
///
/// Reads lines from stdin, processes each line via [`handle_json_rpc`],
/// writes responses to stdout followed by a newline and flush, and exits
/// cleanly on EOF.
///
/// Stdin is read on a blocking thread; each request is handled on the async
/// runtime (watchers started by `query_resources` run there and are stopped by
/// the idle reaper or when the server exits).
pub async fn run_stdio_mcp_server(
    manager: Arc<ClusterManager>,
    source: KubeconfigSource,
    audit: Option<Arc<AuditLogStore>>,
) -> std::io::Result<()> {
    let runtime = tokio::runtime::Handle::current();
    let reaper = tokio::spawn(manager.idle_reaper(DEFAULT_REAP_INTERVAL));
    let result = tokio::task::spawn_blocking(move || {
        use std::io::Write;

        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        let mut reader = stdin.lock();

        loop {
            match read_line_capped(&mut reader, MAX_LINE_BYTES)? {
                ReadLineResult::Ok(line) => {
                    let audit_ref = audit.as_deref();
                    let response =
                        runtime.block_on(handle_json_rpc(&line, &manager, &source, audit_ref));
                    if let Some(resp) = response {
                        stdout.write_all(resp.as_bytes())?;
                        stdout.write_all(b"\n")?;
                        stdout.flush()?;
                    }
                }
                ReadLineResult::ExceededLimit => {
                    record_audit(
                        audit.as_deref(),
                        &source,
                        "unknown",
                        "",
                        "",
                        None,
                        "",
                        false,
                        Some("Request line exceeds 10MB limit"),
                    );
                    let err_resp = RpcResponse::error(
                        None,
                        -32700,
                        "Parse error: request line exceeds 10MB limit",
                    );
                    if let Ok(resp) = serde_json::to_string(&err_resp) {
                        stdout.write_all(resp.as_bytes())?;
                        stdout.write_all(b"\n")?;
                        stdout.flush()?;
                    }
                }
                ReadLineResult::Eof => break,
            }
        }
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(std::io::Error::other);
    reaper.abort();
    result?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Page, Row, RowStatus};

    fn fixture(name: &str) -> std::path::PathBuf {
        crate::test_fixture(name)
    }

    fn test_source() -> KubeconfigSource {
        KubeconfigSource::from_paths(vec![fixture("merge-a.yaml")])
    }

    #[tokio::test]
    async fn test_initialize_handshake() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

        let resp_str = handle_json_rpc(req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json response");

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(resp["result"]["serverInfo"]["name"], "fleetglass-mcp");
        assert_eq!(
            resp["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn test_initialized_notification_returns_none() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

        let resp = handle_json_rpc(req, &manager, &source, None).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn test_tools_list_returns_schemas() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());
        let req = r#"{"jsonrpc":"2.0","id":"msg-42","method":"tools/list","params":{}}"#;

        let resp_str = handle_json_rpc(req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], "msg-42");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 6);

        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"list_contexts"));
        assert!(names.contains(&"query_resources"));
        assert!(names.contains(&"get_resource_yaml"));
        assert!(names.contains(&"get_workload_logs"));
        assert!(names.contains(&"diff_resources"));
        assert!(names.contains(&"get_audit_log"));

        for tool in tools {
            assert!(tool["inputSchema"].is_object());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[tokio::test]
    async fn test_tools_call_list_contexts_returns_markdown_text() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());
        let req = r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"list_contexts","arguments":{}}}"#;

        let resp_str = handle_json_rpc(req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], 10);
        let content = resp["result"]["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");

        let text = content[0]["text"].as_str().expect("text string");
        assert!(text.contains("| Current | Context | Cluster | User | Namespace |"));
        assert!(text.contains("dev"));
        assert!(text.contains("dev-cluster"));
        assert!(text.contains('*'));
    }

    #[tokio::test]
    async fn test_tools_call_query_resources_returns_window_of_rows() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        let store = manager.get_or_create_store("dev-cluster", "Pod");
        store.upsert_row(Row {
            uid: "pod-uid-1".to_string(),
            cluster: "dev-cluster".to_string(),
            kind: "Pod".to_string(),
            namespace: Some("team-a".to_string()),
            name: "nginx-mcp".to_string(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_string(),
            cells: Vec::new(),
            labels: Vec::new(),
            custom: Vec::new(),
        });

        let req = r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"query_resources","arguments":{"cluster":"dev-cluster","kind":"Pod","limit":10}}}"#;

        let resp_str = handle_json_rpc(req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], 11);
        let content = resp["result"]["content"].as_array().expect("content array");
        assert_eq!(content[0]["type"], "text");

        let text = content[0]["text"].as_str().expect("text string");
        let page: Page = serde_json::from_str(text).expect("serialized Page");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].name, "nginx-mcp");
        assert_eq!(page.rows[0].cluster, "dev-cluster");

        // "dev-cluster" is no kubeconfig context: the connect error is reported
        // in `access` and in a second, human-readable content item.
        assert_eq!(page.access.len(), 1);
        assert!(
            matches!(page.access[0].access, StoreAccessState::Error { .. }),
            "{:?}",
            page.access
        );
        assert_eq!(content.len(), 2);
        let note = content[1]["text"].as_str().expect("note");
        assert!(note.contains("Pod in dev-cluster"), "{note}");
    }

    /// Kubeconfig whose only context `closed` points at a closed local port.
    fn closed_port_source(dir: &std::path::Path) -> KubeconfigSource {
        std::fs::create_dir_all(dir).expect("dir");
        let path = dir.join("kubeconfig");
        let content = "apiVersion: v1\nkind: Config\ncurrent-context: closed\n\
             clusters:\n- name: closed\n  cluster:\n    server: https://127.0.0.1:1\n\
             contexts:\n- name: closed\n  context:\n    cluster: closed\n    user: u\n\
             users:\n- name: u\n  user:\n    token: not-a-secret\n";
        std::fs::write(&path, content).expect("write kubeconfig");
        KubeconfigSource::from_paths(vec![path])
    }

    #[tokio::test]
    async fn test_query_resources_starts_watcher_and_reports_error_state() {
        let dir = std::env::temp_dir().join(format!("fg-mcp-closed-test-{}", std::process::id()));
        let source = closed_port_source(&dir);
        let manager = ClusterManager::new(source.clone());

        let req = r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"query_resources","arguments":{"cluster":"closed","kind":"pods"}}}"#;
        let resp_str = handle_json_rpc(req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");
        let content = resp["result"]["content"].as_array().expect("content array");
        let page: Page =
            serde_json::from_str(content[0]["text"].as_str().expect("text")).expect("Page");

        // The handler started the watcher itself (no prior app query).
        let store = manager.get_store("closed", "Pod").expect("store created");
        assert_eq!(store.watchers_started(), 1);
        assert_eq!(page.total, 0);
        assert_eq!(page.access.len(), 1);
        assert!(
            matches!(&page.access[0].access, StoreAccessState::Error { message, .. } if !message.is_empty()),
            "{:?}",
            page.access
        );
        let note = content[1]["text"].as_str().expect("note");
        assert!(note.starts_with("Rows may be missing"), "{note}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_access_summary_reports_loading_and_ready() {
        use crate::api::StoreAccess;
        let manager = ClusterManager::new(test_source());
        let mut page = Page {
            total: 0,
            offset: 0,
            limit: 10,
            rows: Vec::new(),
            version: 1,
            access: Vec::new(),
            status_counts: crate::api::StatusCounts::default(),
        };
        assert!(
            access_summary(&manager, &page)
                .expect("note")
                .starts_with("Not watched")
        );
        page.access.push(StoreAccess {
            cluster: "c".to_owned(),
            kind: "Pod".to_owned(),
            access: StoreAccessState::Ready,
            rows: 0,
            metrics: None,
            paused: false,
        });
        assert_eq!(access_summary(&manager, &page), None);
        page.access[0].access = StoreAccessState::Loading { rows_so_far: 7 };
        assert!(
            access_summary(&manager, &page)
                .expect("note")
                .contains("Pod in c: still loading (initial list not finished, 7 rows so far)")
        );

        page.access[0].access = StoreAccessState::Error {
            message: "list timed out".to_owned(),
            partial_rows: Some(3),
        };
        let note = access_summary(&manager, &page).expect("note");
        assert!(
            note.contains("list timed out (initial list failed: only 3 rows received"),
            "{note}"
        );

        // A synced store whose watcher restarted: Loading again, rows kept.
        let store = manager.get_or_create_store("c", "Pod");
        store.upsert_row(Row {
            uid: "u1".to_owned(),
            cluster: "c".to_owned(),
            kind: "Pod".to_owned(),
            namespace: None,
            name: "p".to_owned(),
            status: RowStatus::Healthy,
            status_reason: "Running".to_owned(),
            cells: Vec::new(),
            labels: Vec::new(),
            custom: Vec::new(),
        });
        store.set_access(StoreAccessState::Ready);
        store.set_access(StoreAccessState::Reconnecting);
        page.access[0].access = StoreAccessState::Reconnecting;
        let note = access_summary(&manager, &page).expect("note");
        assert!(
            note.contains("Pod in c: reconnecting (showing last known rows: 1)"),
            "{note}"
        );
    }

    #[tokio::test]
    async fn test_tools_call_compare_resources_returns_diff_result() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "compare_resources",
                "arguments": {
                    "leftYaml": "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\ndata:\n  k: v1\n",
                    "rightYaml": "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\ndata:\n  k: v2\n"
                }
            }
        })
        .to_string();

        let resp_str = handle_json_rpc(&req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], 12);
        let content = resp["result"]["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");

        let text = content[0]["text"].as_str().expect("text string");
        let diff: crate::api::DiffResult = serde_json::from_str(text).expect("DiffResult");
        assert!(!diff.identical);
        assert!(diff.unified_diff.contains("-  k: v1") || diff.unified_diff.contains("k: v1"));
        assert!(diff.unified_diff.contains("+  k: v2") || diff.unified_diff.contains("k: v2"));
    }

    #[tokio::test]
    async fn test_unknown_method_returns_json_rpc_error() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());
        let req = r#"{"jsonrpc":"2.0","id":99,"method":"unknown/method","params":{}}"#;

        let resp_str = handle_json_rpc(req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], 99);
        assert!(resp["result"].is_null());
        assert_eq!(resp["error"]["code"], -32601);
        assert!(
            resp["error"]["message"]
                .as_str()
                .expect("msg")
                .contains("Method not found")
        );
    }

    #[tokio::test]
    async fn test_unknown_tool_and_mutating_tool_rejected() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        // Unknown tool
        let req = r#"{"jsonrpc":"2.0","id":100,"method":"tools/call","params":{"name":"unknown_tool","arguments":{}}}"#;
        let resp_str = handle_json_rpc(req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");
        assert_eq!(resp["id"], 100);
        assert_eq!(resp["error"]["code"], -32601);

        // Mutating tools must be rejected with MethodNotFound (-32601)
        for mutating in &[
            "delete_resource",
            "apply_resource",
            "scale_workload",
            "patch_resource",
        ] {
            let req = format!(
                r#"{{"jsonrpc":"2.0","id":101,"method":"tools/call","params":{{"name":"{mutating}","arguments":{{}}}}}}"#
            );
            let resp_str = handle_json_rpc(&req, &manager, &source, None)
                .await
                .expect("response");
            let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");
            assert_eq!(resp["id"], 101);
            assert_eq!(resp["error"]["code"], -32601);
        }
    }

    #[tokio::test]
    async fn test_malformed_json_returns_parse_error() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());
        let line = "{ malformed json ";

        let resp_str = handle_json_rpc(line, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], serde_json::Value::Null);
        assert_eq!(resp["error"]["code"], -32700);
        assert!(
            resp["error"]["message"]
                .as_str()
                .expect("msg")
                .contains("Parse error")
        );
    }

    #[tokio::test]
    async fn test_audit_logging_on_tool_call() {
        let dir = std::env::temp_dir().join(format!("fg-mcp-audit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("created dir");
        let log_file = dir.join("audit.jsonl");

        let audit_store = AuditLogStore::new(log_file);
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        let req =
            r#"{"jsonrpc":"2.0","id":200,"method":"tools/call","params":{"name":"list_contexts"}}"#;
        let _ = handle_json_rpc(req, &manager, &source, Some(&audit_store)).await;

        let page = audit_store.query(0, 10, None, None).expect("query audit");
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].action, "mcp:list_contexts");
        assert_eq!(page.entries[0].user, None);

        // Tool call with cluster context sets user from kubeconfig context
        let req_query = r#"{"jsonrpc":"2.0","id":202,"method":"tools/call","params":{"name":"query_resources","arguments":{"cluster":"dev","kind":"pods"}}}"#;
        let _ = handle_json_rpc(req_query, &manager, &source, Some(&audit_store)).await;
        let page_after_query = audit_store.query(0, 10, None, None).expect("query audit");
        assert_eq!(page_after_query.entries[0].action, "mcp:query_resources");
        assert_eq!(
            page_after_query.entries[0].user.as_deref(),
            Some("dev-user")
        );

        // Now test get_audit_log tool via MCP
        let req_audit = r#"{"jsonrpc":"2.0","id":201,"method":"tools/call","params":{"name":"get_audit_log","arguments":{"limit":10}}}"#;
        let resp_str = handle_json_rpc(req_audit, &manager, &source, Some(&audit_store))
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");
        assert_eq!(resp["id"], 201);
        let text = resp["result"]["content"][0]["text"].as_str().expect("text");
        let audit_page: crate::api::AuditLogPage = serde_json::from_str(text).expect("audit page");
        assert_eq!(audit_page.total, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_diff_resources_alias_works() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 15,
            "method": "tools/call",
            "params": {
                "name": "diff_resources",
                "arguments": {
                    "leftYaml": "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n",
                    "rightYaml": "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n"
                }
            }
        })
        .to_string();

        let resp_str = handle_json_rpc(&req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], 15);
        let text = resp["result"]["content"][0]["text"].as_str().expect("text");
        let diff: crate::api::DiffResult = serde_json::from_str(text).expect("DiffResult");
        assert!(diff.identical);
    }

    #[tokio::test]
    async fn test_compare_resources_rejects_payload_over_5mb() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        let huge_yaml = "a".repeat(5 * 1024 * 1024 + 1);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 16,
            "method": "tools/call",
            "params": {
                "name": "diff_resources",
                "arguments": {
                    "leftYaml": huge_yaml,
                    "rightYaml": "apiVersion: v1\n"
                }
            }
        })
        .to_string();

        let resp_str = handle_json_rpc(&req, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["id"], 16);
        assert_eq!(resp["error"]["code"], -32602);
        assert!(
            resp["error"]["message"]
                .as_str()
                .expect("msg")
                .contains("5MB")
        );
    }

    #[tokio::test]
    async fn test_request_line_over_10mb_rejected() {
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        let huge_line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"list_contexts","padding":"{}"}}}}"#,
            "x".repeat(10 * 1024 * 1024 + 10)
        );

        let resp_str = handle_json_rpc(&huge_line, &manager, &source, None)
            .await
            .expect("response");
        let resp: serde_json::Value = serde_json::from_str(&resp_str).expect("valid json");

        assert_eq!(resp["error"]["code"], -32700);
        assert!(
            resp["error"]["message"]
                .as_str()
                .expect("msg")
                .contains("10MB")
        );
    }

    #[test]
    fn test_read_line_capped_limits() {
        use std::io::Cursor;

        // Normal line
        let mut cursor = Cursor::new(b"hello world\nsecond line\n");
        let res = read_line_capped(&mut cursor, 100).expect("read");
        match res {
            ReadLineResult::Ok(line) => assert_eq!(line.trim(), "hello world"),
            _ => panic!("expected normal line"),
        }

        // Second line
        let res2 = read_line_capped(&mut cursor, 100).expect("read");
        match res2 {
            ReadLineResult::Ok(line) => assert_eq!(line.trim(), "second line"),
            _ => panic!("expected normal line"),
        }

        // EOF
        let res_eof = read_line_capped(&mut cursor, 100).expect("read");
        assert!(matches!(res_eof, ReadLineResult::Eof));

        // Oversized line
        let mut oversized = Cursor::new(b"12345678901234567890\nnext line\n");
        let res_over = read_line_capped(&mut oversized, 10).expect("read");
        assert!(matches!(res_over, ReadLineResult::ExceededLimit));

        // Ensure stream consumed until newline and can read next line cleanly
        let res_after = read_line_capped(&mut oversized, 100).expect("read");
        match res_after {
            ReadLineResult::Ok(line) => assert_eq!(line.trim(), "next line"),
            _ => panic!("expected next line after oversized"),
        }
    }

    #[tokio::test]
    async fn test_audit_logging_every_tool_call_branch() {
        let dir =
            std::env::temp_dir().join(format!("fg-mcp-audit-branch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("created dir");
        let log_file = dir.join("audit.jsonl");

        let audit_store = AuditLogStore::new(log_file);
        let source = test_source();
        let manager = ClusterManager::new(source.clone());

        // 1. Missing tool name
        let req_missing_name = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}}"#;
        let _ = handle_json_rpc(req_missing_name, &manager, &source, Some(&audit_store)).await;

        // 2. Unknown tool
        let req_unknown =
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"non_existent"}}"#;
        let _ = handle_json_rpc(req_unknown, &manager, &source, Some(&audit_store)).await;

        // 3. Mutating tool
        let req_mutating =
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"delete_resource"}}"#;
        let _ = handle_json_rpc(req_mutating, &manager, &source, Some(&audit_store)).await;

        // 4. Missing required args in query_resources
        let req_query_bad = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"query_resources","arguments":{}}}"#;
        let _ = handle_json_rpc(req_query_bad, &manager, &source, Some(&audit_store)).await;

        // 5. Missing required args in get_resource_yaml
        let req_yaml_bad = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_resource_yaml","arguments":{}}}"#;
        let _ = handle_json_rpc(req_yaml_bad, &manager, &source, Some(&audit_store)).await;

        // 6. Missing required args in get_workload_logs
        let req_logs_bad = r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_workload_logs","arguments":{}}}"#;
        let _ = handle_json_rpc(req_logs_bad, &manager, &source, Some(&audit_store)).await;

        let page = audit_store.query(0, 10, None, None).expect("query audit");
        assert_eq!(page.total, 6);

        // Every failed attempt was logged with success == false and an error string
        for entry in &page.entries {
            assert!(!entry.success);
            assert!(entry.error.is_some());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
