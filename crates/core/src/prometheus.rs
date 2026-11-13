//! Optional Prometheus metrics (`P2-09`).
//!
//! Prometheus is reached **only** through the Kubernetes API server service proxy
//! (`/api/v1/namespaces/{ns}/services/{scheme}:{svc}:{port}/proxy/...`) using the
//! context's existing [`kube::Client`]. No other host is ever contacted
//! (non-negotiable #1). Path/PromQL building and response parsing are pure
//! functions so they can be unit-tested without a cluster.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::api::{
    MetricsRange, PodSeries, PodUsageSeries, PrometheusConfig, PrometheusDetection,
    PrometheusScheme, PrometheusUnavailableReason, UsagePoint,
};
use crate::error::{Error, Result};

/// Maximum number of pods per [`query_pod_usage`] call (the UI asks for the visible window).
pub const MAX_PODS_PER_QUERY: u32 = 100;

/// Timeout for a single detection probe, so a hanging candidate does not block detection.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Well-known in-cluster Prometheus services probed by [`detect_prometheus`], in order.
pub const CANDIDATES: &[(&str, &str, u16)] = &[
    ("monitoring", "prometheus-operated", 9090),
    ("monitoring", "prometheus-k8s", 9090),
    ("monitoring", "kube-prometheus-stack-prometheus", 9090),
    ("prometheus", "prometheus-server", 80),
];

const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Duration of `range` in seconds.
pub fn range_seconds(range: MetricsRange) -> u32 {
    match range {
        MetricsRange::OneHour => 3_600,
        MetricsRange::SixHours => 6 * 3_600,
        MetricsRange::OneDay => 24 * 3_600,
    }
}

/// Query step for `range`, chosen so every series has at most ~100 points.
pub fn step_seconds(range: MetricsRange) -> u32 {
    match range {
        MetricsRange::OneHour => 60,
        MetricsRange::SixHours => 300,
        MetricsRange::OneDay => 900,
    }
}

/// `true` if `s` is a valid RFC 1123 DNS label (namespace / service name).
fn is_dns_label(s: &str) -> bool {
    let bytes = s.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
}

/// Human-readable `namespace/service:port` of a config, used in messages.
pub fn target_label(cfg: &PrometheusConfig) -> String {
    format!("{}/{}:{}", cfg.namespace, cfg.service, cfg.port)
}

/// Checks that `cfg` describes a usable service reference (does not check `enabled`).
pub fn validate_config(
    cfg: &PrometheusConfig,
) -> std::result::Result<(), PrometheusUnavailableReason> {
    if !is_dns_label(&cfg.namespace) {
        return Err(PrometheusUnavailableReason::NotConfigured {
            message: format!("invalid namespace '{}'", cfg.namespace),
        });
    }
    if !is_dns_label(&cfg.service) {
        return Err(PrometheusUnavailableReason::NotConfigured {
            message: format!("invalid service name '{}'", cfg.service),
        });
    }
    if cfg.port == 0 {
        return Err(PrometheusUnavailableReason::NotConfigured {
            message: "service port must not be 0".to_owned(),
        });
    }
    Ok(())
}

/// API server service-proxy base path for `cfg`, without a trailing slash.
pub fn proxy_base_path(
    cfg: &PrometheusConfig,
) -> std::result::Result<String, PrometheusUnavailableReason> {
    validate_config(cfg)?;
    Ok(format!(
        "/api/v1/namespaces/{}/services/{}:{}:{}/proxy",
        cfg.namespace,
        cfg.scheme.as_str(),
        cfg.service,
        cfg.port
    ))
}

/// Path of Prometheus' `buildinfo` endpoint through the proxy.
pub fn buildinfo_path(
    cfg: &PrometheusConfig,
) -> std::result::Result<String, PrometheusUnavailableReason> {
    Ok(format!("{}/api/v1/status/buildinfo", proxy_base_path(cfg)?))
}

/// Path (with query string) of a `query_range` call through the proxy.
pub fn query_range_path(
    cfg: &PrometheusConfig,
    query: &str,
    start: i64,
    end: i64,
    step: u32,
) -> std::result::Result<String, PrometheusUnavailableReason> {
    Ok(format!(
        "{}/api/v1/query_range?query={}&start={start}&end={end}&step={step}",
        proxy_base_path(cfg)?,
        percent_encode(query)
    ))
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
fn percent_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
    out
}

/// Escapes RE2 metacharacters so `s` matches literally inside a PromQL regex.
pub fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escapes `s` for use inside a double-quoted PromQL string literal.
pub fn escape_label_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Label matchers shared by both queries: namespace, anchored pod alternation, real containers.
fn selector(namespace: &str, pods: &[String]) -> String {
    let alternation = pods
        .iter()
        .map(|p| escape_regex(p))
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "namespace=\"{}\",pod=~\"{}\",container!=\"\"",
        escape_label_value(namespace),
        escape_label_value(&alternation)
    )
}

/// PromQL for per-pod CPU usage in cores.
pub fn cpu_query(namespace: &str, pods: &[String]) -> String {
    format!(
        "sum by (pod) (rate(container_cpu_usage_seconds_total{{{}}}[5m]))",
        selector(namespace, pods)
    )
}

/// PromQL for per-pod memory working set in bytes.
pub fn memory_query(namespace: &str, pods: &[String]) -> String {
    format!(
        "sum by (pod) (container_memory_working_set_bytes{{{}}})",
        selector(namespace, pods)
    )
}

/// Rejects requests for more than [`MAX_PODS_PER_QUERY`] pods.
pub fn check_pod_cap(pods: &[String]) -> Result<()> {
    let requested = u32::try_from(pods.len()).unwrap_or(u32::MAX);
    if requested > MAX_PODS_PER_QUERY {
        return Err(Error::PrometheusTooManyPods {
            requested,
            max: MAX_PODS_PER_QUERY,
        });
    }
    Ok(())
}

/// `[start, end]` (Unix seconds) for `range` ending at `now`, aligned to the step
/// so consecutive refreshes hit the same sample timestamps.
pub fn aligned_window(now: i64, range: MetricsRange) -> (i64, i64) {
    let step = i64::from(step_seconds(range));
    let end = now - now.rem_euclid(step);
    (end - i64::from(range_seconds(range)), end)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromEnvelope {
    status: String,
    #[serde(default)]
    data: Option<serde_json::Value>,
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatrixData {
    result_type: String,
    result: Vec<MatrixSeries>,
}

#[derive(Debug, Deserialize)]
struct MatrixSeries {
    #[serde(default)]
    metric: BTreeMap<String, String>,
    #[serde(default)]
    values: Vec<(f64, String)>,
}

fn bad_response(message: impl Into<String>) -> PrometheusUnavailableReason {
    PrometheusUnavailableReason::BadResponse {
        message: message.into(),
    }
}

/// Parses the Prometheus JSON envelope and returns `data` on `"status": "success"`.
fn parse_envelope(
    body: &str,
) -> std::result::Result<serde_json::Value, PrometheusUnavailableReason> {
    let env: PromEnvelope = serde_json::from_str(body)
        .map_err(|e| bad_response(format!("response is not Prometheus JSON: {e}")))?;
    if env.status != "success" {
        return Err(bad_response(format!(
            "Prometheus error ({}): {}",
            env.error_type.as_deref().unwrap_or("unknown"),
            env.error.as_deref().unwrap_or("no message")
        )));
    }
    env.data
        .ok_or_else(|| bad_response("Prometheus response has no data"))
}

/// Parses a `query_range` matrix response into per-pod points.
///
/// Timestamps become offsets from `start` (points before `start` are dropped);
/// values are multiplied by `scale`, rounded and saturated to `u32`. `NaN`, `Inf`
/// and negative samples are skipped.
pub fn parse_matrix(
    body: &str,
    start: i64,
    scale: f64,
) -> std::result::Result<BTreeMap<String, Vec<UsagePoint>>, PrometheusUnavailableReason> {
    let data = parse_envelope(body)?;
    let matrix: MatrixData = serde_json::from_value(data)
        .map_err(|e| bad_response(format!("unexpected query_range data: {e}")))?;
    if matrix.result_type != "matrix" {
        return Err(bad_response(format!(
            "expected a matrix result, got '{}'",
            matrix.result_type
        )));
    }

    let mut out = BTreeMap::new();
    for series in matrix.result {
        let Some(pod) = series.metric.get("pod") else {
            continue;
        };
        let points: Vec<UsagePoint> = series
            .values
            .iter()
            .filter_map(|(ts, raw)| {
                let value: f64 = raw.parse().ok()?;
                if !value.is_finite() || value < 0.0 || !ts.is_finite() {
                    return None;
                }
                let offset = u32::try_from((ts.round() as i64).checked_sub(start)?).ok()?;
                // `as` saturates for out-of-range floats.
                let scaled = (value * scale).round() as u32;
                Some(UsagePoint {
                    offset_seconds: offset,
                    value: scaled,
                })
            })
            .collect();
        out.insert(pod.clone(), points);
    }
    Ok(out)
}

/// Parses a `buildinfo` response and returns the Prometheus version, if reported.
pub fn parse_buildinfo(
    body: &str,
) -> std::result::Result<Option<String>, PrometheusUnavailableReason> {
    let data = parse_envelope(body)?;
    Ok(data
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}

/// Classifies a proxy failure. Returns `None` for errors that are not about
/// Prometheus (API server unreachable, auth, TLS), which stay plain kube errors.
pub fn classify_proxy_error(
    err: &kube::Error,
    target: &str,
) -> Option<PrometheusUnavailableReason> {
    let kube::Error::Api(status) = err else {
        return None;
    };
    let message = status.message.clone();
    match status.code {
        // 404: no such service. 503: service has no ready endpoints.
        404 | 503 => Some(PrometheusUnavailableReason::ServiceNotFound {
            target: target.to_owned(),
            message,
        }),
        403 => Some(PrometheusUnavailableReason::Forbidden {
            target: target.to_owned(),
            message,
        }),
        // Auth failures are about the context, not Prometheus.
        401 => None,
        code => {
            // Prometheus' own errors (400 bad_data, 422 execution, ...) arrive as the raw body.
            let detail = match parse_envelope(&message) {
                Err(PrometheusUnavailableReason::BadResponse { message }) => message,
                _ => message,
            };
            Some(bad_response(format!("HTTP {code} from {target}: {detail}")))
        }
    }
}

fn unavailable(reason: PrometheusUnavailableReason) -> Error {
    Error::PrometheusUnavailable { reason }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn rfc3339(unix_seconds: i64) -> String {
    k8s_openapi::jiff::Timestamp::from_second(unix_seconds)
        .map(|ts| ts.to_string())
        .unwrap_or_else(|_| unix_seconds.to_string())
}

// ---------------------------------------------------------------------------
// Network (API server proxy only)
// ---------------------------------------------------------------------------

/// GETs `path` (an API server path with optional query string) and returns the body.
async fn proxy_get(client: &kube::Client, path: &str) -> std::result::Result<String, kube::Error> {
    // `Request::get` appends `/{name}` verbatim, which lets us keep a pre-encoded query string.
    let (dir, leaf) = path.rsplit_once('/').unwrap_or(("", path));
    let req = kube::core::Request::new(dir)
        .get(leaf, &kube::api::GetParams::default())
        .map_err(kube::Error::BuildRequest)?;
    client.request_text(req).await
}

/// Probes [`CANDIDATES`] through the API server proxy and returns the first one
/// that answers `/api/v1/status/buildinfo`. Read-only; call only on user request.
pub async fn detect_prometheus(client: &kube::Client) -> PrometheusDetection {
    let mut tried = Vec::with_capacity(CANDIDATES.len());
    let mut forbidden: Option<String> = None;
    let mut other_error: Option<String> = None;

    for &(namespace, service, port) in CANDIDATES {
        let cfg = PrometheusConfig {
            enabled: true,
            namespace: namespace.to_owned(),
            service: service.to_owned(),
            port,
            scheme: PrometheusScheme::Http,
        };
        let target = target_label(&cfg);
        tried.push(target.clone());
        let Ok(path) = buildinfo_path(&cfg) else {
            continue;
        };

        match tokio::time::timeout(PROBE_TIMEOUT, proxy_get(client, &path)).await {
            Err(_) => {
                other_error.get_or_insert_with(|| format!("{target}: timed out"));
            }
            Ok(Ok(body)) => match parse_buildinfo(&body) {
                Ok(version) => {
                    return PrometheusDetection::Found {
                        config: cfg,
                        version,
                    };
                }
                Err(reason) => {
                    tracing::debug!(%target, %reason, "candidate is not Prometheus");
                }
            },
            Ok(Err(err)) => match classify_proxy_error(&err, &target) {
                Some(PrometheusUnavailableReason::ServiceNotFound { .. }) => {}
                Some(PrometheusUnavailableReason::Forbidden { message, .. }) => {
                    forbidden.get_or_insert_with(|| format!("{target}: {message}"));
                }
                Some(reason) => {
                    other_error.get_or_insert_with(|| reason.to_string());
                }
                None => {
                    // The API server itself failed; the other candidates will fail the same way.
                    return PrometheusDetection::Error {
                        message: err.to_string(),
                    };
                }
            },
        }
    }

    if let Some(message) = forbidden {
        PrometheusDetection::Forbidden { message }
    } else if let Some(message) = other_error {
        PrometheusDetection::Error { message }
    } else {
        PrometheusDetection::NotFound { tried }
    }
}

async fn run_range_query(
    client: &kube::Client,
    cfg: &PrometheusConfig,
    query: &str,
    window: (i64, i64),
    step: u32,
    scale: f64,
) -> Result<BTreeMap<String, Vec<UsagePoint>>> {
    let target = target_label(cfg);
    let path = query_range_path(cfg, query, window.0, window.1, step).map_err(unavailable)?;
    let body = proxy_get(client, &path).await.map_err(|err| {
        match classify_proxy_error(&err, &target) {
            Some(reason) => unavailable(reason),
            None => Error::Kube(err),
        }
    })?;
    parse_matrix(&body, window.0, scale).map_err(unavailable)
}

/// CPU (millicores) and memory working set (MiB) series for up to
/// [`MAX_PODS_PER_QUERY`] pods of `namespace` over `range`, via the API server proxy.
pub async fn query_pod_usage(
    client: &kube::Client,
    cfg: &PrometheusConfig,
    namespace: &str,
    pods: &[String],
    range: MetricsRange,
) -> Result<PodUsageSeries> {
    if !cfg.enabled {
        return Err(unavailable(PrometheusUnavailableReason::NotConfigured {
            message: "Prometheus is not enabled for this context".to_owned(),
        }));
    }
    validate_config(cfg).map_err(unavailable)?;
    check_pod_cap(pods)?;

    let step = step_seconds(range);
    let window = aligned_window(now_unix_seconds(), range);
    let start = rfc3339(window.0);
    if pods.is_empty() {
        return Ok(PodUsageSeries {
            start,
            step_seconds: step,
            pods: Vec::new(),
        });
    }

    let cpu_q = cpu_query(namespace, pods);
    let mem_q = memory_query(namespace, pods);
    let (cpu, memory) = futures::future::join(
        run_range_query(client, cfg, &cpu_q, window, step, 1000.0),
        run_range_query(client, cfg, &mem_q, window, step, 1.0 / BYTES_PER_MIB),
    )
    .await;
    let (mut cpu, mut memory) = (cpu?, memory?);

    let pods = pods
        .iter()
        .map(|pod| PodSeries {
            pod: pod.clone(),
            cpu_millicores: cpu.remove(pod).unwrap_or_default(),
            memory_mib: memory.remove(pod).unwrap_or_default(),
        })
        .collect();
    Ok(PodUsageSeries {
        start,
        step_seconds: step,
        pods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PrometheusConfig {
        PrometheusConfig {
            enabled: true,
            namespace: "monitoring".into(),
            service: "prometheus-k8s".into(),
            port: 9090,
            scheme: PrometheusScheme::Http,
        }
    }

    fn api_error(code: u16, message: &str) -> kube::Error {
        kube::Error::Api(Box::new(kube::core::Status {
            code,
            message: message.to_owned(),
            ..Default::default()
        }))
    }

    #[test]
    fn builds_service_proxy_paths() {
        assert_eq!(
            buildinfo_path(&cfg()).unwrap(),
            "/api/v1/namespaces/monitoring/services/http:prometheus-k8s:9090/proxy/api/v1/status/buildinfo"
        );
        let https = PrometheusConfig {
            scheme: PrometheusScheme::Https,
            ..cfg()
        };
        let path = query_range_path(&https, "up{a=\"b\"}", 100, 200, 60).unwrap();
        assert_eq!(
            path,
            "/api/v1/namespaces/monitoring/services/https:prometheus-k8s:9090/proxy/api/v1/query_range?query=up%7Ba%3D%22b%22%7D&start=100&end=200&step=60"
        );
    }

    #[test]
    fn rejects_invalid_service_references() {
        for bad in [
            PrometheusConfig {
                namespace: "../etc".into(),
                ..cfg()
            },
            PrometheusConfig {
                service: "svc/x".into(),
                ..cfg()
            },
            PrometheusConfig {
                service: String::new(),
                ..cfg()
            },
            PrometheusConfig { port: 0, ..cfg() },
        ] {
            assert!(matches!(
                proxy_base_path(&bad),
                Err(PrometheusUnavailableReason::NotConfigured { .. })
            ));
        }
    }

    #[test]
    fn escapes_regex_and_label_values() {
        assert_eq!(escape_regex("web-7f9c.abc"), "web-7f9c\\.abc");
        assert_eq!(
            escape_regex("a+b*(c)|[d]{e}^$?\\"),
            "a\\+b\\*\\(c\\)\\|\\[d\\]\\{e\\}\\^\\$\\?\\\\"
        );
        assert_eq!(escape_label_value("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn builds_promql_with_escaped_pods() {
        let pods = vec!["api-1.x".to_owned(), "db\"0".to_owned()];
        assert_eq!(
            cpu_query("prod", &pods),
            "sum by (pod) (rate(container_cpu_usage_seconds_total{namespace=\"prod\",pod=~\"api-1\\\\.x|db\\\"0\",container!=\"\"}[5m]))"
        );
        assert_eq!(
            memory_query("prod", &pods[..1]),
            "sum by (pod) (container_memory_working_set_bytes{namespace=\"prod\",pod=~\"api-1\\\\.x\",container!=\"\"})"
        );
    }

    #[test]
    fn selects_step_with_at_most_about_100_points() {
        assert_eq!(step_seconds(MetricsRange::OneHour), 60);
        assert_eq!(step_seconds(MetricsRange::SixHours), 300);
        assert_eq!(step_seconds(MetricsRange::OneDay), 900);
        for r in [
            MetricsRange::OneHour,
            MetricsRange::SixHours,
            MetricsRange::OneDay,
        ] {
            assert!(range_seconds(r) / step_seconds(r) < 100);
            let (start, end) = aligned_window(1_700_000_123, r);
            assert_eq!(end % i64::from(step_seconds(r)), 0);
            assert_eq!(end - start, i64::from(range_seconds(r)));
            assert!(end <= 1_700_000_123);
        }
    }

    #[test]
    fn caps_pods_per_query_at_100() {
        let pods: Vec<String> = (0..100).map(|i| format!("p{i}")).collect();
        assert!(check_pod_cap(&pods).is_ok());
        let pods: Vec<String> = (0..101).map(|i| format!("p{i}")).collect();
        assert!(matches!(
            check_pod_cap(&pods),
            Err(Error::PrometheusTooManyPods {
                requested: 101,
                max: 100
            })
        ));
    }

    #[test]
    fn parses_matrix_fixture() {
        let body = r#"{
          "status": "success",
          "data": {
            "resultType": "matrix",
            "result": [
              { "metric": { "pod": "api-1" },
                "values": [[1000, "0.25"], [1060.0, "0.5004"], [1120, "NaN"], [940, "1"]] },
              { "metric": { "pod": "db-0" },
                "values": [[1000, "1.5"], [1060, "+Inf"], [1120, "-1"]] },
              { "metric": {}, "values": [[1000, "1"]] }
            ]
          }
        }"#;
        let cpu = parse_matrix(body, 1000, 1000.0).unwrap();
        assert_eq!(cpu.len(), 2);
        assert_eq!(
            cpu["api-1"],
            vec![
                UsagePoint {
                    offset_seconds: 0,
                    value: 250
                },
                UsagePoint {
                    offset_seconds: 60,
                    value: 500
                },
            ]
        );
        assert_eq!(
            cpu["db-0"],
            vec![UsagePoint {
                offset_seconds: 0,
                value: 1500
            }]
        );

        let mem_body = r#"{"status":"success","data":{"resultType":"matrix","result":[
            {"metric":{"pod":"api-1"},"values":[[1000,"134217728"]]}]}}"#;
        let mem = parse_matrix(mem_body, 1000, 1.0 / BYTES_PER_MIB).unwrap();
        assert_eq!(mem["api-1"][0].value, 128);
    }

    #[test]
    fn parses_empty_matrix() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
        assert!(parse_matrix(body, 0, 1.0).unwrap().is_empty());
    }

    #[test]
    fn reports_prometheus_error_status_json() {
        let body = r#"{"status":"error","errorType":"bad_data","error":"parse error at char 5"}"#;
        match parse_matrix(body, 0, 1.0) {
            Err(PrometheusUnavailableReason::BadResponse { message }) => {
                assert!(message.contains("bad_data"));
                assert!(message.contains("parse error at char 5"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            parse_matrix("<html>nginx</html>", 0, 1.0),
            Err(PrometheusUnavailableReason::BadResponse { .. })
        ));
        let vector = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        assert!(matches!(
            parse_matrix(vector, 0, 1.0),
            Err(PrometheusUnavailableReason::BadResponse { .. })
        ));
    }

    #[test]
    fn parses_buildinfo() {
        let body = r#"{"status":"success","data":{"version":"2.53.0","revision":"x"}}"#;
        assert_eq!(parse_buildinfo(body).unwrap(), Some("2.53.0".to_owned()));
    }

    #[test]
    fn classifies_proxy_errors() {
        let t = "monitoring/prometheus-k8s:9090";
        assert!(matches!(
            classify_proxy_error(&api_error(404, "services \"x\" not found"), t),
            Some(PrometheusUnavailableReason::ServiceNotFound { .. })
        ));
        assert!(matches!(
            classify_proxy_error(&api_error(503, "no endpoints available"), t),
            Some(PrometheusUnavailableReason::ServiceNotFound { .. })
        ));
        assert!(matches!(
            classify_proxy_error(&api_error(403, "forbidden"), t),
            Some(PrometheusUnavailableReason::Forbidden { .. })
        ));
        assert_eq!(
            classify_proxy_error(&api_error(401, "Unauthorized"), t),
            None
        );
        match classify_proxy_error(
            &api_error(
                400,
                r#"{"status":"error","errorType":"bad_data","error":"bad"}"#,
            ),
            t,
        ) {
            Some(PrometheusUnavailableReason::BadResponse { message }) => {
                assert!(message.contains("HTTP 400"));
                assert!(message.contains("bad_data"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn config_defaults_to_disabled() {
        assert!(!PrometheusConfig::default().enabled);
        assert!(validate_config(&PrometheusConfig::default()).is_ok());
    }
}
