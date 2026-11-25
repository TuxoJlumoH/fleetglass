//! Error type returned by IPC commands.

use serde::{Deserialize, Serialize};

use crate::Error;

/// Category of an [`IpcError`], so the UI can pick a state without parsing messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// A kubeconfig file is not valid YAML or not a valid kubeconfig.
    KubeconfigInvalid,
    /// A kubeconfig file exists but cannot be read (permissions, not a file, encoding).
    KubeconfigUnreadable,
    /// `KUBECONFIG` is unset and the home directory is unknown.
    HomeDirNotFound,
    /// Error reported by the Kubernetes client.
    Kubernetes,
    /// Mutating action blocked because the target context is configured as read-only.
    ReadOnlyContext,
    /// Mutating action on a protected context requires typing the target resource name.
    ConfirmationRequired,
    /// User-supplied input (such as a search regular expression) is invalid.
    InvalidInput,
    /// Bug or unexpected failure inside Fleetglass (e.g. a panicked worker task).
    Internal,
    /// Prometheus is disabled for the context or its service reference is invalid.
    PrometheusNotConfigured,
    /// The configured Prometheus service does not exist or has no ready endpoints.
    PrometheusNotFound,
    /// RBAC denies `services/proxy` to the Prometheus service.
    PrometheusForbidden,
    /// Prometheus answered with an error or an unusable response.
    PrometheusBadResponse,
    /// The opt-in update check is turned off in settings (the default).
    UpdateCheckDisabled,
    /// The update check is unavailable in this build or the endpoint request failed.
    UpdateCheckFailed,
}

/// Error returned by every fallible IPC command.
///
/// `message` is user-readable and never contains secrets (core errors do not
/// echo kubeconfig contents).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    pub kind: ErrorKind,
    pub message: String,
}

impl IpcError {
    /// An [`ErrorKind::Internal`] error with the given message.
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Internal,
            message: message.into(),
        }
    }

    /// An [`ErrorKind::InvalidInput`] error with the given message.
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::InvalidInput,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IpcError {}

impl From<&Error> for IpcError {
    fn from(err: &Error) -> Self {
        // Exhaustive on purpose: a new core variant must pick a kind here.
        let kind = match err {
            Error::KubeconfigParse { .. } | Error::Kubeconfig { .. } => {
                ErrorKind::KubeconfigInvalid
            }
            Error::KubeconfigRead { .. } => ErrorKind::KubeconfigUnreadable,
            Error::HomeDirNotFound => ErrorKind::HomeDirNotFound,
            Error::Kube(_) => ErrorKind::Kubernetes,
            Error::ReadOnlyContext { .. } => ErrorKind::ReadOnlyContext,
            Error::ConfirmationRequired { .. } => ErrorKind::ConfirmationRequired,
            Error::InvalidRegex { .. }
            | Error::InvalidBase64 { .. }
            | Error::InvalidColumnPath { .. }
            | Error::InvalidDebugRequest { .. }
            | Error::InvalidBrowserUrl { .. }
            | Error::InvalidTerminalSize { .. } => ErrorKind::InvalidInput,
            Error::PathTraversal { .. } | Error::SizeLimitExceeded { .. } => {
                ErrorKind::InvalidInput
            }
            Error::UnsupportedKind { .. } => ErrorKind::InvalidInput,
            Error::Exec { .. }
            | Error::FileCopy { .. }
            | Error::WatcherEvicted { .. }
            | Error::TargetNotFound { .. } => ErrorKind::Kubernetes,
            Error::PrometheusUnavailable { reason } => match reason {
                super::PrometheusUnavailableReason::NotConfigured { .. } => {
                    ErrorKind::PrometheusNotConfigured
                }
                super::PrometheusUnavailableReason::ServiceNotFound { .. } => {
                    ErrorKind::PrometheusNotFound
                }
                super::PrometheusUnavailableReason::Forbidden { .. } => {
                    ErrorKind::PrometheusForbidden
                }
                super::PrometheusUnavailableReason::BadResponse { .. } => {
                    ErrorKind::PrometheusBadResponse
                }
            },
            Error::PrometheusTooManyPods { .. } => ErrorKind::InvalidInput,
            Error::Import(_) => ErrorKind::InvalidInput,
            Error::Vault(_) => ErrorKind::Internal,
        };
        Self {
            kind,
            message: crate::cluster::sanitize_error(err),
        }
    }
}

impl From<Error> for IpcError {
    fn from(err: Error) -> Self {
        Self::from(&err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn maps_core_errors_to_kinds_with_display_message() {
        let parse = Error::KubeconfigParse {
            path: PathBuf::from("/tmp/kc.yaml"),
            line: Some(3),
            column: Some(1),
            message: "bad".into(),
        };
        let ipc = IpcError::from(&parse);
        assert_eq!(ipc.kind, ErrorKind::KubeconfigInvalid);
        assert_eq!(ipc.message, parse.to_string());

        let read = Error::KubeconfigRead {
            path: PathBuf::from("/tmp/kc.yaml"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(IpcError::from(read).kind, ErrorKind::KubeconfigUnreadable);
        assert_eq!(
            IpcError::from(Error::HomeDirNotFound).kind,
            ErrorKind::HomeDirNotFound
        );
        assert_eq!(
            IpcError::from(Error::ReadOnlyContext {
                context: "prod".into()
            })
            .kind,
            ErrorKind::ReadOnlyContext
        );
        assert_eq!(
            IpcError::from(Error::ConfirmationRequired {
                context: "prod".into(),
                expected_name: "api".into(),
            })
            .kind,
            ErrorKind::ConfirmationRequired
        );
        assert_eq!(
            IpcError::from(Error::InvalidRegex {
                pattern: "[".into(),
                message: "unclosed".into(),
            })
            .kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            IpcError::from(Error::InvalidBase64 { key: "pw".into() }).kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            IpcError::from(Error::InvalidColumnPath {
                expr: "[".into(),
                reason: "unclosed".into(),
            })
            .kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            IpcError::from(Error::PathTraversal {
                path: "../etc".into()
            })
            .kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            IpcError::from(Error::SizeLimitExceeded { limit_bytes: 1 }).kind,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            IpcError::from(Error::Exec {
                message: "stream closed".into()
            })
            .kind,
            ErrorKind::Kubernetes
        );
    }

    #[test]
    fn maps_prometheus_errors_to_kinds() {
        use crate::api::PrometheusUnavailableReason as R;
        let cases = [
            (
                R::NotConfigured {
                    message: "off".into(),
                },
                ErrorKind::PrometheusNotConfigured,
            ),
            (
                R::ServiceNotFound {
                    target: "m/p:9090".into(),
                    message: "nf".into(),
                },
                ErrorKind::PrometheusNotFound,
            ),
            (
                R::Forbidden {
                    target: "m/p:9090".into(),
                    message: "no".into(),
                },
                ErrorKind::PrometheusForbidden,
            ),
            (
                R::BadResponse {
                    message: "x".into(),
                },
                ErrorKind::PrometheusBadResponse,
            ),
        ];
        for (reason, kind) in cases {
            assert_eq!(
                IpcError::from(Error::PrometheusUnavailable { reason }).kind,
                kind
            );
        }
        assert_eq!(
            IpcError::from(Error::PrometheusTooManyPods {
                requested: 101,
                max: 100
            })
            .kind,
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn serializes_as_camel_case() {
        let json = serde_json::to_value(IpcError {
            kind: ErrorKind::HomeDirNotFound,
            message: "m".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "homeDirNotFound", "message": "m" })
        );
    }

    #[test]
    fn sanitizes_exec_plugin_secrets_in_ipc_error() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        use std::process::{ExitStatus, Output};

        #[cfg(unix)]
        let status = ExitStatus::from_raw(1 << 8);
        #[cfg(windows)]
        let status = ExitStatus::from_raw(1);

        let secret = "SECRET_TOKEN_XYZ123";
        let auth_err = kube::client::AuthError::AuthExecRun {
            cmd: format!("aws-iam-authenticator --token {secret}"),
            out: Box::new(Output {
                status,
                stdout: format!("leaked stdout {secret}").into_bytes(),
                stderr: format!("leaked stderr {secret}").into_bytes(),
            }),
        };
        let core_err = Error::Kube(kube::Error::Auth(auth_err));
        let ipc = IpcError::from(&core_err);
        assert_eq!(ipc.kind, ErrorKind::Kubernetes);
        assert!(
            !ipc.message.contains(secret),
            "secret leaked in IPC error message: {}",
            ipc.message
        );
        assert!(
            ipc.message
                .contains("authentication failed: exec credential plugin failed"),
            "unexpected message: {}",
            ipc.message
        );
    }
}
