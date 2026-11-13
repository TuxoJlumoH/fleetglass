//! IPC types for the optional Prometheus metrics integration (`P2-09`).
//!
//! Prometheus is only ever reached through the Kubernetes API server service
//! proxy of the selected context (non-negotiable #1), never via an external URL.

use serde::{Deserialize, Serialize};

/// URL scheme used by the API server proxy to reach the Prometheus service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "lowercase")]
pub enum PrometheusScheme {
    #[default]
    Http,
    Https,
}

impl PrometheusScheme {
    /// The scheme as used in a service proxy path segment (`http` / `https`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// Per-context Prometheus settings persisted in `prometheus.json`. Disabled by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PrometheusConfig {
    pub enabled: bool,
    /// Namespace of the Prometheus `Service`.
    pub namespace: String,
    /// Name of the Prometheus `Service`.
    pub service: String,
    /// Service port (not the container port).
    pub port: u16,
    pub scheme: PrometheusScheme,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            namespace: "monitoring".to_owned(),
            service: "prometheus-operated".to_owned(),
            port: 9090,
            scheme: PrometheusScheme::Http,
        }
    }
}

/// Result of probing well-known Prometheus services through the API server proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PrometheusDetection {
    /// A candidate answered `/api/v1/status/buildinfo`. `config.enabled` is `true`;
    /// the UI still has to save it explicitly.
    Found {
        config: PrometheusConfig,
        version: Option<String>,
    },
    /// None of the candidates exist (`namespace/service:port` entries that were tried).
    NotFound { tried: Vec<String> },
    /// At least one candidate exists but the user may not use `services/proxy`.
    Forbidden { message: String },
    /// The API server could not be reached or returned an unexpected failure.
    Error { message: String },
}

/// Time range of a Prometheus usage query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum MetricsRange {
    OneHour,
    SixHours,
    OneDay,
}

/// One sample: seconds since [`PodUsageSeries::start`] and a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct UsagePoint {
    pub offset_seconds: u32,
    pub value: u32,
}

/// CPU (millicores) and memory working set (MiB) series of one pod.
///
/// Empty vectors mean Prometheus has no samples for the pod in the range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PodSeries {
    pub pod: String,
    pub cpu_millicores: Vec<UsagePoint>,
    pub memory_mib: Vec<UsagePoint>,
}

/// Usage series for at most 100 pods of one namespace (the visible table window).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct PodUsageSeries {
    /// RFC 3339 timestamp of offset 0.
    pub start: String,
    pub step_seconds: u32,
    /// Same order as the requested pods.
    pub pods: Vec<PodSeries>,
}

/// Why Prometheus metrics are unavailable, for honest UI messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PrometheusUnavailableReason {
    /// Disabled for this context, or the saved service reference is invalid.
    NotConfigured { message: String },
    /// The API server proxy answered 404/503 for the configured service.
    ServiceNotFound { target: String, message: String },
    /// RBAC denies `services/proxy` (403).
    Forbidden { target: String, message: String },
    /// Prometheus answered, but not with a usable result.
    BadResponse { message: String },
}

impl std::fmt::Display for PrometheusUnavailableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured { message } => write!(f, "not configured: {message}"),
            Self::ServiceNotFound { target, message } => {
                write!(f, "service {target} not found: {message}")
            }
            Self::Forbidden { target, message } => {
                write!(f, "no permission to proxy to service {target}: {message}")
            }
            Self::BadResponse { message } => write!(f, "bad response: {message}"),
        }
    }
}
