//! Kubeconfig discovery, loading and merging with kubectl semantics.
//!
//! - `KUBECONFIG` is split with the OS path separator (`:` on unix, `;` on Windows);
//!   empty entries are ignored and duplicate paths are read once.
//! - The first file to define a context/cluster/user name wins; later entries with the
//!   same name are discarded entirely. `current-context` comes from the first file that sets it.
//! - Missing files are skipped and reported as warnings; unreadable or invalid files are errors.
//! - If `KUBECONFIG` is unset or empty, `~/.kube/config` is used (missing = empty config + warning).
//!
//! Parsing and per-file merging is done by `kube::config::Kubeconfig`, whose `merge` implements
//! the same first-wins rules (verified by the fixture tests in `tests/kubeconfig.rs`).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kube::config::{AuthInfo, Kubeconfig, KubeconfigError};

use crate::api::{AuthType, ContextInfo, ContextList};
use crate::error::{Error, Result};
use crate::import::{ImportedStore, defined_and_referenced_names, rename_clashing};

/// Where to load kubeconfig files from. Construct with [`KubeconfigSource::from_env`] in the app
/// and [`KubeconfigSource::from_paths`] in tests, so tests never touch `~/.kube/config`.
///
/// Optionally also merges the encrypted store of imported contexts ([`ImportedStore`], Android),
/// after all files: on a name clash the file's context wins.
#[derive(Debug, Clone)]
pub struct KubeconfigSource {
    paths: Vec<PathBuf>,
    imported: Option<Arc<ImportedStore>>,
}

/// A merged kubeconfig plus what happened while loading it.
#[derive(Clone)]
pub struct LoadedKubeconfig {
    /// The merged config (first-wins). Contains credentials: never log it.
    pub config: Kubeconfig,
    /// Files that were actually read, in merge order.
    pub files: Vec<PathBuf>,
    /// Non-fatal, user-readable problems (e.g. missing files).
    pub warnings: Vec<String>,
}

impl fmt::Debug for LoadedKubeconfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedKubeconfig")
            .field("files", &self.files)
            .field("warnings", &self.warnings)
            .field("current_context", &self.config.current_context)
            .field(
                "contexts",
                &self
                    .config
                    .contexts
                    .iter()
                    .map(|c| &c.name)
                    .collect::<Vec<_>>(),
            )
            .field(
                "clusters",
                &self
                    .config
                    .clusters
                    .iter()
                    .map(|c| &c.name)
                    .collect::<Vec<_>>(),
            )
            .field(
                "users",
                &self
                    .config
                    .auth_infos
                    .iter()
                    .map(|u| &u.name)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl KubeconfigSource {
    /// Source from the process environment: `KUBECONFIG`, else `~/.kube/config`.
    pub fn from_env() -> Result<Self> {
        let value = std::env::var_os("KUBECONFIG");
        let home = std::env::home_dir();
        Self::from_kubeconfig_var(value.as_deref(), home.as_deref())
    }

    /// Source from an explicit `KUBECONFIG` value and home directory (pure; used by `from_env`).
    pub fn from_kubeconfig_var(value: Option<&OsStr>, home: Option<&Path>) -> Result<Self> {
        let paths = value.map(split_kubeconfig_var).unwrap_or_default();
        if !paths.is_empty() {
            return Ok(Self::from_paths(paths));
        }
        let home = home.ok_or(Error::HomeDirNotFound)?;
        Ok(Self::from_paths(vec![home.join(".kube").join("config")]))
    }

    /// Source from an explicit list of files, merged in order. Empty and duplicate paths are dropped.
    pub fn from_paths(paths: Vec<PathBuf>) -> Self {
        let mut seen = HashSet::new();
        let paths = paths
            .into_iter()
            .filter(|p| !p.as_os_str().is_empty())
            .filter(|p| seen.insert(p.clone()))
            .collect();
        Self {
            paths,
            imported: None,
        }
    }

    /// Also merge the contexts of `store` (after the files).
    pub fn with_imported(mut self, store: Arc<ImportedStore>) -> Self {
        self.imported = Some(store);
        self
    }

    /// The store of imported contexts, if any.
    pub fn imported(&self) -> Option<&Arc<ImportedStore>> {
        self.imported.as_ref()
    }

    /// Files that will be merged, in order.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Read and merge all files.
    pub fn load(&self) -> Result<LoadedKubeconfig> {
        let mut merged = Kubeconfig::default();
        let mut files = Vec::new();
        let mut warnings = Vec::new();

        for path in &self.paths {
            let mut next = match Kubeconfig::read_from(path) {
                Ok(config) => config,
                Err(KubeconfigError::ReadConfig(err, _))
                    if err.kind() == std::io::ErrorKind::NotFound =>
                {
                    warnings.push(format!(
                        "kubeconfig file {} does not exist; skipped",
                        path.display()
                    ));
                    continue;
                }
                Err(err) => return Err(map_read_error(path, err)),
            };
            // kubectl ignores `kind`/`apiVersion` when merging files; kube's merge would
            // reject a mismatch, so keep only the first file's values.
            next.kind = None;
            next.api_version = None;
            merged = merged.merge(next).map_err(|source| Error::Kubeconfig {
                path: path.clone(),
                source,
            })?;
            files.push(path.clone());
        }

        if let Some(store) = &self.imported {
            match store.load() {
                Ok(imported) => merged = merge_imported(merged, imported, &mut warnings),
                // Keep the file contexts usable; the warning explains the missing ones.
                Err(err) => warnings.push(format!("imported contexts are unavailable: {err}")),
            }
        }

        Ok(LoadedKubeconfig {
            config: merged,
            files,
            warnings,
        })
    }

    /// Load, merge and list contexts (sorted by name).
    pub fn list_contexts(&self) -> Result<ContextList> {
        let loaded = self.load()?;
        Ok(context_list(&loaded.config, loaded.warnings))
    }

    /// Returns the configured user for `context_name`, if defined and non-empty.
    pub fn context_user(&self, context_name: &str) -> Option<String> {
        self.load().ok().and_then(|loaded| {
            loaded
                .config
                .contexts
                .into_iter()
                .find(|c| c.name == context_name)
                .and_then(|c| c.context)
                .and_then(|ctx| ctx.user)
                .filter(|u| !u.is_empty())
        })
    }
}

/// Convenience: list contexts from `KUBECONFIG` / `~/.kube/config`.
pub fn list_contexts() -> Result<ContextList> {
    KubeconfigSource::from_env()?.list_contexts()
}

/// Split a `KUBECONFIG` value with the OS path-list separator, dropping empty entries.
pub fn split_kubeconfig_var(value: &OsStr) -> Vec<PathBuf> {
    std::env::split_paths(value)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// Build the IPC context list from a merged kubeconfig.
pub fn context_list(config: &Kubeconfig, mut warnings: Vec<String>) -> ContextList {
    let current = config.current_context.clone().filter(|c| !c.is_empty());
    let mut seen = HashSet::new();
    let mut contexts = Vec::new();

    // Duplicates inside one file: keep the first, like the cross-file rule.
    for named in &config.contexts {
        if !seen.insert(named.name.as_str()) {
            warnings.push(format!(
                "duplicate context \"{}\" in kubeconfig; keeping first entry",
                named.name
            ));
            continue;
        }
        let ctx = named.context.clone().unwrap_or_default();
        let cluster = config
            .clusters
            .iter()
            .find(|c| c.name == ctx.cluster)
            .map(|c| c.cluster.clone().unwrap_or_default());
        if cluster.is_none() {
            warnings.push(format!(
                "context \"{}\" refers to cluster \"{}\", which is not defined",
                named.name, ctx.cluster
            ));
        }
        let user = ctx.user.clone().filter(|u| !u.is_empty());
        let auth_info = match &user {
            Some(user_name) => {
                let found = config.auth_infos.iter().find(|a| &a.name == user_name);
                if found.is_none() {
                    warnings.push(format!(
                        "context \"{}\" refers to user \"{user_name}\", which is not defined",
                        named.name
                    ));
                }
                found.and_then(|a| a.auth_info.as_ref())
            }
            None => None,
        };

        contexts.push(ContextInfo {
            name: named.name.clone(),
            cluster: ctx.cluster.clone(),
            user,
            namespace: ctx.namespace.clone().filter(|n| !n.is_empty()),
            server: cluster.and_then(|c| c.server),
            auth: auth_info.map_or(AuthType::None, auth_type),
            is_current: current.as_deref() == Some(named.name.as_str()),
        });
    }

    if let Some(cur) = &current
        && !contexts.iter().any(|c| &c.name == cur)
    {
        warnings.push(format!(
            "current-context \"{cur}\" is not defined in the kubeconfig"
        ));
    }

    contexts.sort_by(|a, b| a.name.cmp(&b.name));
    ContextList {
        contexts,
        current,
        warnings,
    }
}

/// Classify a user entry. Order follows what actually authenticates first in practice:
/// exec plugin, auth-provider, client certificate, bearer token, basic auth.
pub fn auth_type(info: &AuthInfo) -> AuthType {
    if let Some(exec) = &info.exec {
        return AuthType::Exec {
            command: exec.command.clone().filter(|c| !c.is_empty()),
        };
    }
    if let Some(provider) = &info.auth_provider {
        return if provider.name == "oidc" {
            AuthType::Oidc
        } else {
            AuthType::AuthProvider {
                name: provider.name.clone(),
            }
        };
    }
    if info.client_certificate.is_some() || info.client_certificate_data.is_some() {
        return AuthType::ClientCert;
    }
    if info.token.is_some() || info.token_file.is_some() {
        return AuthType::Token;
    }
    if info.username.is_some() || info.password.is_some() {
        return AuthType::Basic;
    }
    AuthType::None
}

/// Appends the imported config to the file config. Files win on context-name clashes (with a
/// warning). Imported clusters/users whose names are taken by the files are renamed in this merged
/// view, so an imported context never picks up another file's credentials.
fn merge_imported(
    mut merged: Kubeconfig,
    mut imported: Kubeconfig,
    warnings: &mut Vec<String>,
) -> Kubeconfig {
    let file_contexts: HashSet<String> = merged.contexts.iter().map(|c| c.name.clone()).collect();
    imported.contexts.retain(|c| {
        if file_contexts.contains(&c.name) {
            warnings.push(format!(
                "imported context \"{}\" is hidden by a context of the same name in the \
                 kubeconfig files",
                c.name
            ));
            false
        } else {
            true
        }
    });

    // Reserve what the files define *and* reference: a dangling file reference must stay
    // dangling rather than bind to an imported entry.
    let (reserved_clusters, reserved_users) = defined_and_referenced_names(&merged);
    rename_clashing(
        &mut imported.clusters,
        &mut imported.auth_infos,
        &mut imported.contexts,
        &reserved_clusters,
        &reserved_users,
        unique_name,
    );

    merged.clusters.append(&mut imported.clusters);
    merged.auth_infos.append(&mut imported.auth_infos);
    merged.contexts.append(&mut imported.contexts);
    merged
}

fn unique_name(base: &str, taken: &HashSet<String>) -> String {
    let mut candidate = format!("{base} (imported)");
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{base} (imported {n})");
        n += 1;
    }
    candidate
}

/// User-readable description of a kubeconfig parse error that never echoes the input (the
/// snippet form would show file contents, which may include tokens or keys).
pub(crate) fn parse_error_message(err: &KubeconfigError) -> String {
    match err {
        KubeconfigError::Parse(parse) => {
            let plain = parse.without_snippet();
            let location = plain.location();
            let line = location.map(|l| l.line()).filter(|l| *l > 0);
            let column = location.map(|l| l.column()).filter(|c| *c > 0);
            let message = strip_location_suffix(&plain.to_string());
            match (line, column) {
                (Some(l), Some(c)) => format!("{message} (line {l}, column {c})"),
                (Some(l), None) => format!("{message} (line {l})"),
                _ => message,
            }
        }
        // Remaining variants from parsing YAML text (kind/apiVersion mismatch between
        // documents) carry no file contents.
        other => other.to_string(),
    }
}

fn map_read_error(path: &Path, err: KubeconfigError) -> Error {
    match err {
        KubeconfigError::ReadConfig(source, _) => Error::KubeconfigRead {
            path: path.to_path_buf(),
            source,
        },
        KubeconfigError::Parse(parse) => {
            // Plain message with location only: the snippet form would echo file
            // contents, which may include tokens or keys.
            let plain = parse.without_snippet();
            let location = plain.location();
            Error::KubeconfigParse {
                path: path.to_path_buf(),
                line: location.map(|l| l.line()).filter(|l| *l > 0),
                column: location.map(|l| l.column()).filter(|c| *c > 0),
                message: strip_location_suffix(&plain.to_string()),
            }
        }
        source => Error::Kubeconfig {
            path: path.to_path_buf(),
            source,
        },
    }
}

/// serde-saphyr appends " at line X, column Y" to messages; we report location separately.
///
/// Also drops anything that may quote the input: serde's `invalid type: string "eyJ…", expected
/// struct AuthInfo` (a token pasted as `user: <token>`) becomes `invalid type, expected struct
/// AuthInfo`; unknown-variant names and any remaining "…" content become `<redacted>`.
fn strip_location_suffix(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or_default().trim();
    let stripped = match first_line.rfind(" at line ") {
        Some(idx) => &first_line[..idx],
        None => first_line,
    };
    let generic = ["invalid type:", "invalid value:"]
        .into_iter()
        .find(|kind| stripped.contains(kind))
        .map(|kind| {
            let kind = kind.trim_end_matches(':');
            match stripped.rfind(", expected ") {
                Some(idx) => format!("{kind}{}", &stripped[idx..]),
                None => kind.to_owned(),
            }
        });
    let message = generic.as_deref().unwrap_or(stripped);
    // Backticks usually hold schema names (`missing field `server``); only variants come from
    // the input.
    let message = if message.contains("unknown variant ") {
        redact_delimited(message, '`')
    } else {
        message.to_owned()
    };
    redact_delimited(&message, '"')
}

/// Replaces every `q…q` span with `q<redacted>q`; an unmatched opening `q` drops the rest.
fn redact_delimited(s: &str, q: char) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find(q) {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + q.len_utf8()..];
        match after_open.find(q) {
            Some(close) => {
                out.push(q);
                out.push_str("<redacted>");
                out.push(q);
                rest = &after_open[close + q.len_utf8()..];
            }
            None => {
                out.push(q);
                out.push_str("<redacted>");
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use kube::config::{NamedAuthInfo, NamedContext};

    use super::*;

    #[test]
    fn unset_or_empty_kubeconfig_falls_back_to_home() {
        let home = Path::new("/home/test");
        let expected = vec![PathBuf::from("/home/test/.kube/config")];
        let src = KubeconfigSource::from_kubeconfig_var(None, Some(home)).unwrap();
        assert_eq!(src.paths(), expected.as_slice());
        let empty = std::env::join_paths(["", ""]).unwrap();
        let src = KubeconfigSource::from_kubeconfig_var(Some(&empty), Some(home)).unwrap();
        assert_eq!(src.paths(), expected.as_slice());
    }

    #[test]
    fn no_home_and_no_kubeconfig_is_an_error() {
        let err = KubeconfigSource::from_kubeconfig_var(None, None).unwrap_err();
        assert!(matches!(err, Error::HomeDirNotFound));
    }

    #[test]
    fn kubeconfig_var_split_ignores_empty_and_duplicate_entries() {
        let value = std::env::join_paths(["/a", "", "/b", "/a"]).unwrap();
        let src = KubeconfigSource::from_kubeconfig_var(Some(&value), None).unwrap();
        assert_eq!(src.paths(), [PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn strip_location_suffix_keeps_message() {
        assert_eq!(
            strip_location_suffix("did not find expected key at line 3, column 5"),
            "did not find expected key"
        );
        assert_eq!(strip_location_suffix("plain"), "plain");
    }

    #[test]
    fn loaded_kubeconfig_debug_redacts_secrets() {
        let loaded = LoadedKubeconfig {
            config: Kubeconfig {
                auth_infos: vec![NamedAuthInfo {
                    name: "alice".into(),
                    auth_info: Some(AuthInfo {
                        token: Some("super-secret-token-value".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
            files: vec![PathBuf::from("/tmp/cfg")],
            warnings: Vec::new(),
        };
        assert!(!format!("{loaded:?}").contains("super-secret-token-value"));
    }

    #[test]
    fn strip_location_suffix_redacts_unknown_variant_value() {
        assert!(
            !strip_location_suffix(
                "unknown variant `my-secret-val`, expected one of `Never`, `Always` at line 4, column 2"
            )
            .contains("my-secret-val")
        );
    }

    #[test]
    fn strip_location_suffix_drops_quoted_values_of_type_errors() {
        let secret = "eyJhbGciOiJSUzI1NiJ9.c2VjcmV0";
        for (raw, expected) in [
            (
                format!(
                    "invalid type: string \"{secret}\", expected struct AuthInfo at line 3, column 11"
                ),
                "invalid type, expected struct AuthInfo",
            ),
            (
                format!("invalid value: string \"{secret}\", expected a boolean"),
                "invalid value, expected a boolean",
            ),
            (
                format!("some custom error about \"{secret}\" here"),
                "some custom error about \"<redacted>\" here",
            ),
            (
                format!("unterminated \"{secret}"),
                "unterminated \"<redacted>",
            ),
        ] {
            let message = strip_location_suffix(&raw);
            assert_eq!(message, expected);
            let err = Error::KubeconfigParse {
                path: PathBuf::from("/tmp/cfg"),
                line: Some(3),
                column: Some(11),
                message,
            };
            assert!(!err.to_string().contains(secret), "{err}");
            assert!(!format!("{err:?}").contains(secret), "{err:?}");
        }
        // Schema names in backticks stay readable.
        assert_eq!(
            strip_location_suffix("missing field `server`"),
            "missing field `server`"
        );
    }

    fn named_cluster(name: &str, server: &str) -> kube::config::NamedCluster {
        kube::config::NamedCluster {
            name: name.into(),
            cluster: Some(kube::config::Cluster {
                server: Some(server.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn named_user(name: &str, token: &str) -> NamedAuthInfo {
        NamedAuthInfo {
            name: name.into(),
            auth_info: Some(AuthInfo {
                token: Some(token.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn named_context(name: &str, cluster: &str, user: &str) -> NamedContext {
        NamedContext {
            name: name.into(),
            context: Some(kube::config::Context {
                cluster: cluster.into(),
                user: Some(user.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// (server, token) the context resolves to in `config`.
    fn resolve(config: &Kubeconfig, ctx: &str) -> (Option<String>, Option<String>) {
        let c = config
            .contexts
            .iter()
            .find(|c| c.name == ctx)
            .and_then(|c| c.context.clone())
            .unwrap();
        let server = config
            .clusters
            .iter()
            .find(|x| x.name == c.cluster)
            .and_then(|x| x.cluster.as_ref()?.server.clone());
        let token = config
            .auth_infos
            .iter()
            .find(|u| Some(&u.name) == c.user.as_ref())
            .and_then(|u| {
                let json = serde_json::to_value(u.auth_info.as_ref()?).ok()?;
                json.get("token")?.as_str().map(str::to_owned)
            });
        (server, token)
    }

    #[test]
    fn merge_imported_does_not_chain_renames() {
        let files = Kubeconfig {
            clusters: vec![named_cluster("x", "https://file")],
            auth_infos: vec![named_user("x", "t-file")],
            contexts: vec![named_context("file", "x", "x")],
            ..Default::default()
        };
        let imported = Kubeconfig {
            clusters: vec![
                named_cluster("x", "https://c1"),
                named_cluster("x (imported)", "https://c2"),
            ],
            auth_infos: vec![named_user("x", "t-c1"), named_user("x (imported)", "t-c2")],
            contexts: vec![
                named_context("c1", "x", "x"),
                named_context("c2", "x (imported)", "x (imported)"),
            ],
            ..Default::default()
        };
        let mut warnings = Vec::new();
        let merged = merge_imported(files, imported, &mut warnings);
        let s = |v: &str| Some(v.to_owned());
        assert_eq!(resolve(&merged, "file"), (s("https://file"), s("t-file")));
        assert_eq!(resolve(&merged, "c1"), (s("https://c1"), s("t-c1")));
        assert_eq!(resolve(&merged, "c2"), (s("https://c2"), s("t-c2")));
        let users: HashSet<_> = merged.auth_infos.iter().map(|u| &u.name).collect();
        assert_eq!(users.len(), 3);
        let clusters: HashSet<_> = merged.clusters.iter().map(|c| &c.name).collect();
        assert_eq!(clusters.len(), 3);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn merge_imported_keeps_dangling_file_references_dangling() {
        // The file context refers to cluster "gone" / user "gone", which no file defines.
        let files = Kubeconfig {
            contexts: vec![named_context("file", "gone", "gone")],
            ..Default::default()
        };
        let imported = Kubeconfig {
            clusters: vec![named_cluster("gone", "https://imported")],
            auth_infos: vec![named_user("gone", "t-imported")],
            contexts: vec![named_context("phone", "gone", "gone")],
            ..Default::default()
        };
        let merged = merge_imported(files, imported, &mut Vec::new());
        assert_eq!(resolve(&merged, "file"), (None, None));
        assert_eq!(
            resolve(&merged, "phone"),
            (
                Some("https://imported".to_owned()),
                Some("t-imported".to_owned())
            )
        );
        let list = context_list(&merged, Vec::new());
        let file = list.contexts.iter().find(|c| c.name == "file").unwrap();
        assert_eq!(file.server, None);
        assert_eq!(file.auth, AuthType::None);
        assert!(
            list.warnings
                .iter()
                .any(|w| w.contains("context \"file\" refers to cluster \"gone\""))
        );
    }

    #[test]
    fn duplicate_context_name_emits_warning() {
        let cfg = Kubeconfig {
            contexts: vec![
                NamedContext {
                    name: "dup".into(),
                    context: None,
                    ..Default::default()
                },
                NamedContext {
                    name: "dup".into(),
                    context: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let list = context_list(&cfg, Vec::new());
        assert!(
            list.warnings
                .iter()
                .any(|w| w.contains("duplicate context \"dup\""))
        );
    }

    #[test]
    fn context_user_resolves_user_from_named_context() {
        let fixture_path = crate::test_fixture("cert-token-basic.yaml");
        let source = KubeconfigSource::from_paths(vec![fixture_path]);
        assert_eq!(source.context_user("cert"), Some("cert-user".to_string()));
        assert_eq!(source.context_user("anonymous"), None);
        assert_eq!(source.context_user("non-existent"), None);
    }
}
