//! Typed, user-explainable errors for the core.

use std::path::PathBuf;

/// Errors produced by `fleetglass-core`.
///
/// Variants for auth, TLS, DNS/unreachable, RBAC forbidden, not found and
/// "API not installed" are added as the cluster layer lands.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Error returned by the Kubernetes client.
    #[error("kubernetes client error: {0}")]
    Kube(#[from] kube::Error),

    /// A kubeconfig file exists but could not be read (permissions, not a file, encoding).
    #[error("cannot read kubeconfig file {}: {source}", .path.display())]
    KubeconfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A kubeconfig file is not valid YAML or does not have the kubeconfig shape.
    #[error(
        "kubeconfig file {}{} is invalid: {message}",
        .path.display(),
        location_suffix(*.line, *.column)
    )]
    KubeconfigParse {
        path: PathBuf,
        /// 1-based line of the problem, when known.
        line: Option<u64>,
        /// 1-based column of the problem, when known.
        column: Option<u64>,
        /// Parser message without a source snippet (snippets could echo secrets).
        message: String,
    },

    /// Any other problem reported by kube while loading a kubeconfig file.
    #[error("cannot load kubeconfig file {}: {source}", .path.display())]
    Kubeconfig {
        path: PathBuf,
        #[source]
        source: kube::config::KubeconfigError,
    },

    /// `KUBECONFIG` is unset and the home directory is unknown, so `~/.kube/config` cannot be located.
    #[error(
        "cannot locate ~/.kube/config: KUBECONFIG is not set and the home directory is unknown"
    )]
    HomeDirNotFound,

    /// Mutating action rejected because the target context is configured as read-only.
    #[error("context '{context}' is read-only; mutating actions are blocked")]
    ReadOnlyContext { context: String },

    /// Mutating action on a protected context requires typing the exact resource name.
    #[error(
        "context '{context}' is protected; type '{expected_name}' to confirm this mutating action"
    )]
    ConfirmationRequired {
        context: String,
        expected_name: String,
    },

    /// User-supplied regular expression could not be compiled.
    #[error("invalid regular expression '{pattern}': {message}")]
    InvalidRegex { pattern: String, message: String },

    /// A secret value supplied for decoding is not valid base64. The value itself
    /// is never echoed because it may be secret.
    #[error("value for key '{key}' is not valid base64")]
    InvalidBase64 { key: String },

    /// A custom column JSONPath expression could not be parsed.
    #[error("invalid column path '{expr}': {reason}")]
    InvalidColumnPath { expr: String, reason: String },

    /// A file-copy path or archive entry escapes its target directory (`..` or absolute).
    #[error("unsafe path '{path}': absolute paths and '..' components are not allowed")]
    PathTraversal { path: String },

    /// A file copy exceeded its configured size limit.
    #[error("file copy exceeds the size limit of {limit_bytes} bytes")]
    SizeLimitExceeded { limit_bytes: u64 },

    /// Building, reading or streaming a file-copy tar archive failed
    /// (for example `tar` is missing in the container).
    #[error("file copy failed: {message}")]
    FileCopy { message: String },

    /// Prometheus metrics cannot be shown (not configured, service missing, RBAC, bad response).
    #[error("Prometheus metrics unavailable: {reason}")]
    PrometheusUnavailable {
        reason: crate::api::PrometheusUnavailableReason,
    },

    /// More pods were requested from Prometheus than one call allows (the UI asks per window).
    #[error("too many pods for one Prometheus query: {requested} (max {max})")]
    PrometheusTooManyPods { requested: u32, max: u32 },

    /// A watcher could not be started because its store was repeatedly evicted during initialization.
    #[error(
        "failed to start watcher for {kind} in cluster '{cluster}': store was evicted repeatedly"
    )]
    WatcherEvicted { cluster: String, kind: String },

    /// A mutating action was requested for an unsupported resource kind.
    #[error("unsupported resource kind '{kind}' for action '{action}'")]
    UnsupportedKind { kind: String, action: String },

    /// An interactive container execution session failed or its stream closed.
    #[error("exec error: {message}")]
    Exec { message: String },

    /// Sealing or opening the imported-credentials store failed (Android Keystore on the phone).
    /// The message describes the failure only; it never contains the data.
    #[error("credential store error: {0}")]
    Vault(String),

    /// A kubeconfig offered for import, or an import/remove request, was rejected
    /// (invalid YAML, unsupported context, duplicate name, invalid token form, size limit).
    /// Never contains file contents or secrets.
    #[error("import failed: {0}")]
    Import(String),

    /// A debug container request is missing or has an invalid field.
    #[error("invalid debug container request: field '{field}' is required and cannot be empty")]
    InvalidDebugRequest { field: &'static str },

    /// A requested port-forward target or backing pod was not found.
    #[error("target '{kind}/{name}' was not found: {message}")]
    TargetNotFound {
        kind: String,
        name: String,
        message: String,
    },
}

fn location_suffix(line: Option<u64>, column: Option<u64>) -> String {
    match (line, column) {
        (Some(l), Some(c)) => format!(" (line {l}, column {c})"),
        (Some(l), None) => format!(" (line {l})"),
        _ => String::new(),
    }
}

/// Result alias using the core [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
