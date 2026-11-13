//! IPC types for importing a kubeconfig into the encrypted store (Android, task 03-02).

use serde::{Deserialize, Serialize};

use super::AuthType;

/// What an import would bring in, shown before the user confirms. Bounded: the input is capped at
/// [`crate::import::MAX_IMPORT_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    /// One entry per context in the file, in file order.
    pub contexts: Vec<ImportCandidate>,
    /// Non-fatal, user-readable notes (e.g. contexts that skip TLS verification).
    pub warnings: Vec<String>,
}

/// One context of a kubeconfig offered for import. Never carries secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub name: String,
    /// Cluster entry name referenced by the context.
    pub cluster: String,
    pub server: Option<String>,
    pub auth: AuthType,
    /// A context with this name is already imported (importing replaces it only when asked).
    pub exists: bool,
    /// Why this context cannot be imported; `None` = importable.
    pub problem: Option<ImportProblem>,
}

/// Why a context cannot be imported.
///
/// Serialized as `{ "type": "execPlugin", "command": "aws" }`, `{ "type": "noServer" }`, ...
/// [`ImportProblem::describe`] gives the user-facing sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImportProblem {
    /// The user runs an exec credential plugin (`aws`, `gke-gcloud-auth-plugin`, `kubelogin`, ...):
    /// there is no shell or plugin binary on the phone.
    ExecPlugin { command: Option<String> },
    /// The user uses a legacy auth-provider other than `oidc` (e.g. `gcp`, `azure`), which needs
    /// cloud tooling that is not available on the phone.
    AuthProvider { name: String },
    /// The user uses the `oidc` auth-provider without an `id-token`. Only the id-token is used
    /// (it is not refreshed: the refresh-token flow is not built in), so there is nothing to
    /// authenticate with.
    OidcWithoutIdToken,
    /// The user has a client certificate without its key, or a key without its certificate
    /// (inline or as a file).
    IncompleteClientCertificate,
    /// A credential or CA is a path to a file (`certificate-authority`, `client-certificate`,
    /// `client-key`, `tokenFile`) without inline data: the file is on another machine.
    /// `field` is the kubeconfig key.
    FileReference { field: String },
    /// The cluster has neither `certificate-authority-data` nor `insecure-skip-tls-verify: true`,
    /// so the server certificate cannot be verified on Android (no platform verifier, ADR 0004).
    NoCertificateAuthority,
    /// The context refers to a cluster entry that is not in the file.
    MissingCluster,
    /// The context refers to a user entry that is not in the file.
    MissingUser,
    /// The cluster entry has no `server` URL.
    NoServer,
}

impl ImportProblem {
    /// User-facing explanation, phrased to follow "Context \"x\" ...".
    pub fn describe(&self) -> String {
        const ALTERNATIVE: &str = "create a token or client-certificate context for the phone";
        match self {
            Self::ExecPlugin {
                command: Some(command),
            } => format!(
                "uses the exec credential plugin `{command}`, which cannot run on Android; \
                 {ALTERNATIVE}"
            ),
            Self::ExecPlugin { command: None } => {
                format!(
                    "uses an exec credential plugin, which cannot run on Android; {ALTERNATIVE}"
                )
            }
            Self::AuthProvider { name } => format!(
                "uses the `{name}` auth-provider, which cannot run on Android (only `oidc` with an \
                 id-token is supported); {ALTERNATIVE}"
            ),
            Self::OidcWithoutIdToken => format!(
                "uses the `oidc` auth-provider without an id-token; on Android only an id-token \
                 already in the kubeconfig is used and it is not refreshed automatically (log in \
                 on a computer, import the kubeconfig with the current id-token and re-import it \
                 when it expires), or {ALTERNATIVE}"
            ),
            Self::IncompleteClientCertificate => "has a client certificate without its key, or a \
                 key without its certificate; both client-certificate-data and client-key-data \
                 are needed"
                .to_owned(),
            Self::FileReference { field } => format!(
                "refers to a file in `{field}`, which only exists on the machine the kubeconfig \
                 came from; export it with the data embedded \
                 (`kubectl config view --raw --flatten --minify`) and import that"
            ),
            Self::NoCertificateAuthority => "has no certificate-authority-data for its cluster, \
                 so the server certificate cannot be verified on Android; add the cluster's CA \
                 certificate (or, for test clusters only, insecure-skip-tls-verify: true)"
                .to_owned(),
            Self::MissingCluster => {
                "refers to a cluster that is not defined in the file".to_owned()
            }
            Self::MissingUser => "refers to a user that is not defined in the file".to_owned(),
            Self::NoServer => "has no server URL for its cluster".to_owned(),
        }
    }
}
