//! Kubeconfig import for the Android app (task 03-02a): validate a kubeconfig file or a pasted
//! token, keep only what the phone can use, and store it encrypted.
//!
//! Platform-agnostic: the encryption is behind [`SecretVault`] (Android Keystore on the phone, a
//! fake in tests). The store is one sealed blob (JSON of a [`Kubeconfig`]); the decrypted copy is
//! cached in memory after the first read so the vault is not hit on every kubeconfig load.
//!
//! Nothing here logs or echoes kubeconfig contents: parse errors drop the source snippet and every
//! `Debug` impl prints names and paths only.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use kube::config::{AuthInfo, Cluster, Context, Kubeconfig, NamedAuthInfo, NamedCluster};

use crate::api::{AuthType, ImportCandidate, ImportPreview, ImportProblem};
use crate::error::{Error, Result};
use crate::kubeconfig::{auth_type, parse_error_message};

/// Encrypts/decrypts the imported-credentials blob (Android Keystore on the phone).
pub trait SecretVault: Send + Sync + 'static {
    fn seal(&self, plain: &[u8]) -> crate::Result<Vec<u8>>;
    fn open(&self, sealed: &[u8]) -> crate::Result<Vec<u8>>;
}

/// Largest kubeconfig text accepted for import (1 MiB).
pub const MAX_IMPORT_BYTES: usize = 1024 * 1024;

/// How strict an import is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportOptions {
    /// Refuse clusters without `certificate-authority-data` (unless they set
    /// `insecure-skip-tls-verify: true`). Android passes `true`: without a kubeconfig CA kube 4.2
    /// falls back to `rustls-platform-verifier`, which is not initialised there (ADR 0004).
    pub require_ca: bool,
}

/// Parses `yaml` and describes every context it contains, with the reason it cannot be imported
/// (if any). `existing` are the names already in the store, to flag replacements.
pub fn preview_import(
    yaml: &str,
    existing: &[String],
    opts: &ImportOptions,
) -> Result<ImportPreview> {
    let config = parse(yaml)?;
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    let mut contexts = Vec::new();

    for named in &config.contexts {
        if !seen.insert(named.name.as_str()) {
            warnings.push(format!(
                "duplicate context \"{}\" in the file; keeping the first entry",
                named.name
            ));
            continue;
        }
        let ctx = named.context.clone().unwrap_or_default();
        let cluster = find_cluster(&config, &ctx.cluster);
        let user = find_user(&config, &ctx);
        if cluster.is_some_and(|c| c.insecure_skip_tls_verify == Some(true)) {
            warnings.push(format!(
                "context \"{}\" skips TLS verification (insecure-skip-tls-verify): the server's \
                 identity is not checked, so use it only for test clusters",
                named.name
            ));
        }
        contexts.push(ImportCandidate {
            name: named.name.clone(),
            cluster: ctx.cluster.clone(),
            server: cluster
                .and_then(|c| c.server.clone())
                .filter(|s| !s.is_empty()),
            auth: user.ok().flatten().map_or(AuthType::None, auth_type),
            exists: existing.iter().any(|e| e == &named.name),
            problem: context_problem(&config, &ctx, opts),
        });
    }

    Ok(ImportPreview { contexts, warnings })
}

/// Returns a kubeconfig with only the contexts `names` and exactly the clusters and users they
/// reference. Fails if a name is not in the file or a named context has an [`ImportProblem`].
/// `current-context`, `preferences` and all `extensions` are dropped, as are file paths that inline
/// data already overrides.
pub fn extract_contexts(yaml: &str, names: &[String], opts: &ImportOptions) -> Result<Kubeconfig> {
    if names.is_empty() {
        return Err(Error::Import("no contexts selected".to_owned()));
    }
    let config = parse(yaml)?;
    let mut out = Kubeconfig {
        kind: Some("Config".to_owned()),
        api_version: Some("v1".to_owned()),
        ..Kubeconfig::default()
    };
    for name in names {
        if out.contexts.iter().any(|c| &c.name == name) {
            continue;
        }
        let named = config
            .contexts
            .iter()
            .find(|c| &c.name == name)
            .ok_or_else(|| Error::Import(format!("context \"{name}\" is not in the file")))?;
        let ctx = named.context.clone().unwrap_or_default();
        if let Some(problem) = context_problem(&config, &ctx, opts) {
            return Err(Error::Import(format!(
                "context \"{name}\" {}",
                problem.describe()
            )));
        }
        if !out.clusters.iter().any(|c| c.name == ctx.cluster)
            && let Some(cluster) = config.clusters.iter().find(|c| c.name == ctx.cluster)
        {
            let mut cluster = cluster.clone();
            if let Some(c) = cluster.cluster.as_mut() {
                strip_cluster(c);
            }
            out.clusters.push(cluster);
        }
        if let Some(user_name) = ctx.user.as_deref().filter(|u| !u.is_empty())
            && !out.auth_infos.iter().any(|u| u.name == user_name)
            && let Some(user) = config.auth_infos.iter().find(|u| u.name == user_name)
        {
            let mut user = user.clone();
            if let Some(info) = user.auth_info.as_mut() {
                strip_auth_info(info);
            }
            out.auth_infos.push(user);
        }
        let mut named = named.clone();
        if let Some(c) = named.context.as_mut() {
            c.extensions = None;
        }
        out.contexts.push(named);
    }
    Ok(out)
}

/// Checks every context of an already-built config (e.g. from [`token_kubeconfig`]) against
/// `opts`; the first problem becomes an [`Error::Import`].
pub fn validate(config: &Kubeconfig, opts: &ImportOptions) -> Result<()> {
    for named in &config.contexts {
        let ctx = named.context.clone().unwrap_or_default();
        if let Some(problem) = context_problem(config, &ctx, opts) {
            return Err(Error::Import(format!(
                "context \"{}\" {}",
                named.name,
                problem.describe()
            )));
        }
    }
    Ok(())
}

/// Builds a one-context kubeconfig for "paste a token": context, cluster and user are all named
/// `name`. `ca_pem` is stored base64-encoded in `certificate-authority-data`.
pub fn token_kubeconfig(
    name: &str,
    server: &str,
    token: &str,
    ca_pem: Option<&str>,
    insecure_skip_tls_verify: bool,
) -> Result<Kubeconfig> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Import("the context name is empty".to_owned()));
    }
    let server = server.trim();
    validate_server(server)?;
    let token = token.trim();
    if token.is_empty() {
        return Err(Error::Import("the token is empty".to_owned()));
    }
    // Never echo the token: it is the secret.
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Import(
            "the token contains spaces or line breaks; paste only the token".to_owned(),
        ));
    }
    let ca_pem = ca_pem.map(str::trim).filter(|ca| !ca.is_empty());
    if let Some(ca) = ca_pem
        && !(ca.starts_with("-----BEGIN CERTIFICATE-----")
            && ca.ends_with("-----END CERTIFICATE-----"))
    {
        return Err(Error::Import(
            "the CA certificate is not in PEM form (-----BEGIN CERTIFICATE----- ... \
             -----END CERTIFICATE-----)"
                .to_owned(),
        ));
    }
    if ca_pem.is_some() && insecure_skip_tls_verify {
        return Err(Error::Import(
            "choose either a CA certificate or skipping TLS verification, not both".to_owned(),
        ));
    }

    // `AuthInfo::token` is a `secrecy::SecretString`; build it through serde so core does not
    // need its own `secrecy` dependency.
    let auth_info: AuthInfo = serde_json::from_value(serde_json::json!({ "token": token }))
        .map_err(|_| Error::Import("cannot build the user entry".to_owned()))?;
    let cluster = Cluster {
        server: Some(server.to_owned()),
        insecure_skip_tls_verify: insecure_skip_tls_verify.then_some(true),
        certificate_authority_data: ca_pem.map(|ca| {
            base64::engine::general_purpose::STANDARD.encode(format!("{ca}\n").as_bytes())
        }),
        ..Cluster::default()
    };
    Ok(Kubeconfig {
        kind: Some("Config".to_owned()),
        api_version: Some("v1".to_owned()),
        clusters: vec![NamedCluster {
            name: name.to_owned(),
            cluster: Some(cluster),
            ..NamedCluster::default()
        }],
        auth_infos: vec![NamedAuthInfo {
            name: name.to_owned(),
            auth_info: Some(auth_info),
            ..NamedAuthInfo::default()
        }],
        contexts: vec![kube::config::NamedContext {
            name: name.to_owned(),
            context: Some(Context {
                cluster: name.to_owned(),
                user: Some(name.to_owned()),
                ..Context::default()
            }),
            ..kube::config::NamedContext::default()
        }],
        ..Kubeconfig::default()
    })
}

/// How long [`ImportedStore::load`] keeps reporting a failed read/open without retrying it (each
/// open is a Keystore call on Android, and `load` runs on every client build).
pub const OPEN_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Encrypted store of imported contexts: one file holding the vault-sealed JSON of a
/// [`Kubeconfig`]. Thread-safe; the decrypted config is cached after the first read.
///
/// The cache (and the open-failure backoff) is per instance: the app must create **one** store per
/// path and share it as an `Arc` (the same `Arc` in [`crate::kubeconfig::KubeconfigSource`] and in
/// the import/remove commands). Two instances on one path would not see each other's writes and
/// the later `add` would overwrite the earlier one's contexts.
pub struct ImportedStore {
    path: PathBuf,
    vault: Arc<dyn SecretVault>,
    /// `None` until first read. Contains credentials: never log it.
    cache: RwLock<Option<Kubeconfig>>,
    /// Last failed read/open (time, message without data); `load` does not retry it for
    /// `retry_after`.
    open_failure: Mutex<Option<(Instant, String)>>,
    retry_after: Duration,
}

impl fmt::Debug for ImportedStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportedStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ImportedStore {
    /// Store at `path` (created on the first write), sealed with `vault`.
    pub fn new(path: PathBuf, vault: Arc<dyn SecretVault>) -> Self {
        Self {
            path,
            vault,
            cache: RwLock::new(None),
            open_failure: Mutex::new(None),
            retry_after: OPEN_RETRY_AFTER,
        }
    }

    /// File holding the sealed blob.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The imported config (empty if nothing was imported yet).
    ///
    /// After a failed read/open, returns that error again without retrying for
    /// [`OPEN_RETRY_AFTER`]; [`Self::add`] and [`Self::remove`] always retry.
    pub fn load(&self) -> Result<Kubeconfig> {
        if let Some(config) = self
            .cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            return Ok(config.clone());
        }
        let mut guard = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none()
            && let Some((at, message)) = self
                .open_failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            && at.elapsed() < self.retry_after
        {
            return Err(Error::Vault(message.clone()));
        }
        Ok(self.cached(&mut guard)?.clone())
    }

    /// Adds every context of `config` (with the clusters/users it references) and returns their
    /// names. A context name that is already imported is an error unless `replace_existing`;
    /// replacing drops the old context and, if nothing else uses them, its cluster and user.
    /// Imported clusters/users whose names are still used by other contexts are renamed.
    pub fn add(&self, config: Kubeconfig, replace_existing: bool) -> Result<Vec<String>> {
        let mut seen = HashSet::new();
        for named in &config.contexts {
            if named.name.is_empty() {
                return Err(Error::Import("a context has an empty name".to_owned()));
            }
            if !seen.insert(named.name.as_str()) {
                return Err(Error::Import(format!(
                    "context \"{}\" appears twice",
                    named.name
                )));
            }
        }
        if config.contexts.is_empty() {
            return Err(Error::Import("nothing to import: no contexts".to_owned()));
        }
        // Every reference must resolve inside `config`: a dangling name would otherwise bind to a
        // cluster/user of an earlier import.
        for named in &config.contexts {
            let ctx = named.context.clone().unwrap_or_default();
            let problem = if !config.clusters.iter().any(|c| c.name == ctx.cluster) {
                Some(ImportProblem::MissingCluster)
            } else if find_user(&config, &ctx).is_err() {
                Some(ImportProblem::MissingUser)
            } else {
                None
            };
            if let Some(problem) = problem {
                return Err(Error::Import(format!(
                    "context \"{}\" {}",
                    named.name,
                    problem.describe()
                )));
            }
        }

        let mut guard = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = self.cached(&mut guard)?.clone();

        if !replace_existing
            && let Some(clash) = config
                .contexts
                .iter()
                .find(|c| next.contexts.iter().any(|e| e.name == c.name))
        {
            return Err(Error::Import(format!(
                "context \"{}\" is already imported",
                clash.name
            )));
        }
        next.contexts
            .retain(|e| !config.contexts.iter().any(|c| c.name == e.name));
        garbage_collect(&mut next);

        let Kubeconfig {
            mut clusters,
            mut auth_infos,
            mut contexts,
            ..
        } = config;
        // Only keep entries the new contexts reference, so a renamed duplicate never leaks in.
        clusters.retain(|c| {
            contexts
                .iter()
                .any(|x| x.context.as_ref().is_some_and(|ctx| ctx.cluster == c.name))
        });
        auth_infos.retain(|u| {
            contexts.iter().any(|x| {
                x.context
                    .as_ref()
                    .is_some_and(|ctx| ctx.user.as_deref() == Some(u.name.as_str()))
            })
        });
        dedupe_by_name(&mut clusters, |c| &c.name);
        dedupe_by_name(&mut auth_infos, |u| &u.name);

        let (reserved_clusters, reserved_users) = defined_and_referenced_names(&next);
        rename_clashing(
            &mut clusters,
            &mut auth_infos,
            &mut contexts,
            &reserved_clusters,
            &reserved_users,
            unique_name,
        );

        let names = contexts.iter().map(|c| c.name.clone()).collect();
        next.clusters.append(&mut clusters);
        next.auth_infos.append(&mut auth_infos);
        next.contexts.append(&mut contexts);

        self.persist(&next)?;
        *guard = Some(next);
        Ok(names)
    }

    /// Removes an imported context (and its cluster/user when unused). `false` if it was not there.
    pub fn remove(&self, context: &str) -> Result<bool> {
        let mut guard = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = self.cached(&mut guard)?.clone();
        let before = next.contexts.len();
        next.contexts.retain(|c| c.name != context);
        if next.contexts.len() == before {
            return Ok(false);
        }
        garbage_collect(&mut next);
        self.persist(&next)?;
        *guard = Some(next);
        Ok(true)
    }

    /// Names of the imported contexts, in import order.
    pub fn context_names(&self) -> Result<Vec<String>> {
        Ok(self.load()?.contexts.into_iter().map(|c| c.name).collect())
    }

    /// Drops all imported contexts: deletes the store file (and a stale temp file) and forgets the
    /// cache and any remembered open failure. Used when the sealed file can no longer be opened
    /// (e.g. the vault key is gone), since [`Self::add`] and [`Self::remove`] must read the
    /// existing file first.
    pub fn reset(&self) -> Result<()> {
        let mut guard = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        for path in [self.path.clone(), self.tmp_path()] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(Error::Vault(format!(
                        "cannot delete {}: {err}",
                        path.display()
                    )));
                }
            }
        }
        sync_parent_dir(&self.path);
        *guard = Some(Kubeconfig::default());
        *self
            .open_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }

    fn tmp_path(&self) -> PathBuf {
        let mut tmp_name = self.path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        self.path.with_file_name(tmp_name)
    }

    /// Fills the cache from disk if needed. A failure is remembered (see [`Self::load`]); a
    /// success clears it.
    fn cached<'a>(&self, guard: &'a mut Option<Kubeconfig>) -> Result<&'a Kubeconfig> {
        if guard.is_none() {
            let result = self.read_from_disk();
            let mut failure = self
                .open_failure
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match result {
                Ok(config) => {
                    *failure = None;
                    *guard = Some(config);
                }
                Err(err) => {
                    let message = match &err {
                        Error::Vault(message) => message.clone(),
                        other => other.to_string(),
                    };
                    *failure = Some((Instant::now(), message));
                    return Err(err);
                }
            }
        }
        guard
            .as_ref()
            .ok_or_else(|| Error::Vault("imported credentials are not loaded".to_owned()))
    }

    fn read_from_disk(&self) -> Result<Kubeconfig> {
        let sealed = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Kubeconfig::default());
            }
            Err(err) => {
                return Err(Error::Vault(format!(
                    "cannot read {}: {err}",
                    self.path.display()
                )));
            }
        };
        let plain = self.vault.open(&sealed)?;
        // serde_json errors can quote values, so report a fixed message.
        serde_json::from_slice(&plain).map_err(|_| {
            Error::Vault(format!(
                "the imported credentials in {} are damaged; remove the file and import again",
                self.path.display()
            ))
        })
    }

    /// serde_json -> seal -> write temp file (0600) in the same directory -> rename.
    fn persist(&self, config: &Kubeconfig) -> Result<()> {
        let plain = serde_json::to_vec(config)
            .map_err(|_| Error::Vault("cannot serialize the imported credentials".to_owned()))?;
        let sealed = self.vault.seal(&plain)?;
        let io_err = |what: &str, err: std::io::Error| {
            Error::Vault(format!("cannot {what} {}: {err}", self.path.display()))
        };
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| io_err("create the directory of", e))?;
        }
        let tmp_path = self.tmp_path();
        // A stale temp file (crash, other umask) would keep its old mode: always create afresh.
        match std::fs::remove_file(&tmp_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err("remove the stale temporary file of", err)),
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let result = options
            .open(&tmp_path)
            .and_then(|mut file| {
                file.write_all(&sealed)?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp_path, &self.path));
        if let Err(err) = result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(io_err("write", err));
        }
        sync_parent_dir(&self.path);
        Ok(())
    }
}

/// Makes the rename durable. Best effort: the data file itself is already synced.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    {
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        if let Err(err) = std::fs::File::open(parent).and_then(|dir| dir.sync_all()) {
            tracing::debug!(dir = %parent.display(), %err, "fsync of the imported-store directory failed");
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn parse(yaml: &str) -> Result<Kubeconfig> {
    if yaml.len() > MAX_IMPORT_BYTES {
        return Err(Error::Import(format!(
            "the kubeconfig is larger than the {} KiB limit",
            MAX_IMPORT_BYTES / 1024
        )));
    }
    Kubeconfig::from_yaml(yaml).map_err(|err| {
        Error::Import(format!(
            "the kubeconfig is not valid: {}",
            parse_error_message(&err)
        ))
    })
}

fn find_cluster<'a>(config: &'a Kubeconfig, name: &str) -> Option<&'a Cluster> {
    config
        .clusters
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| c.cluster.as_ref())
}

/// `Ok(None)` = the context has no user (anonymous); `Err(())` = it names a user that is missing.
fn find_user<'a>(
    config: &'a Kubeconfig,
    ctx: &Context,
) -> std::result::Result<Option<&'a AuthInfo>, ()> {
    match ctx.user.as_deref().filter(|u| !u.is_empty()) {
        None => Ok(None),
        Some(name) => config
            .auth_infos
            .iter()
            .find(|u| u.name == name)
            .map(|u| u.auth_info.as_ref())
            .ok_or(()),
    }
}

/// The first reason the context cannot be used on the phone, if any.
fn context_problem(
    config: &Kubeconfig,
    ctx: &Context,
    opts: &ImportOptions,
) -> Option<ImportProblem> {
    let Some(cluster) = config.clusters.iter().find(|c| c.name == ctx.cluster) else {
        return Some(ImportProblem::MissingCluster);
    };
    let Ok(user) = find_user(config, ctx) else {
        return Some(ImportProblem::MissingUser);
    };
    let cluster = cluster.cluster.clone().unwrap_or_default();
    if cluster.server.as_deref().is_none_or(str::is_empty) {
        return Some(ImportProblem::NoServer);
    }
    if let Some(info) = user {
        if let Some(exec) = &info.exec {
            return Some(ImportProblem::ExecPlugin {
                command: exec.command.clone().filter(|c| !c.is_empty()),
            });
        }
        if let Some(provider) = &info.auth_provider {
            if provider.name != "oidc" {
                return Some(ImportProblem::AuthProvider {
                    name: provider.name.clone(),
                });
            }
            // kube is built without its `oidc` feature: only `id-token` is used, never refreshed.
            if provider
                .config
                .get("id-token")
                .is_none_or(|t| t.trim().is_empty())
            {
                return Some(ImportProblem::OidcWithoutIdToken);
            }
            if provider.config.contains_key("idp-certificate-authority")
                && !provider
                    .config
                    .contains_key("idp-certificate-authority-data")
            {
                return Some(file_ref("idp-certificate-authority"));
            }
        }
        let has_cert = info.client_certificate.is_some() || info.client_certificate_data.is_some();
        let has_key = info.client_key.is_some() || info.client_key_data.is_some();
        if has_cert != has_key {
            return Some(ImportProblem::IncompleteClientCertificate);
        }
        if info.client_certificate.is_some() && info.client_certificate_data.is_none() {
            return Some(file_ref("client-certificate"));
        }
        if info.client_key.is_some() && info.client_key_data.is_none() {
            return Some(file_ref("client-key"));
        }
        if info.token_file.is_some() && info.token.is_none() {
            return Some(file_ref("tokenFile"));
        }
    }
    let has_ca_data = cluster
        .certificate_authority_data
        .as_deref()
        .is_some_and(|d| !d.is_empty());
    if cluster.certificate_authority.is_some() && !has_ca_data {
        return Some(file_ref("certificate-authority"));
    }
    if opts.require_ca && !has_ca_data && cluster.insecure_skip_tls_verify != Some(true) {
        return Some(ImportProblem::NoCertificateAuthority);
    }
    None
}

fn file_ref(field: &str) -> ImportProblem {
    ImportProblem::FileReference {
        field: field.to_owned(),
    }
}

/// Drops extensions and file paths that the inline data overrides (they point at another machine).
fn strip_cluster(cluster: &mut Cluster) {
    cluster.extensions = None;
    if cluster.certificate_authority_data.is_some() {
        cluster.certificate_authority = None;
    }
}

fn strip_auth_info(info: &mut AuthInfo) {
    info.extensions = None;
    if info.client_certificate_data.is_some() {
        info.client_certificate = None;
    }
    if info.client_key_data.is_some() {
        info.client_key = None;
    }
    if info.token.is_some() {
        info.token_file = None;
    }
}

fn validate_server(server: &str) -> Result<()> {
    let Some(rest) = server.strip_prefix("https://") else {
        return Err(Error::Import(
            "the server URL must start with https://".to_owned(),
        ));
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || host.starts_with(':') || host.contains('@') {
        return Err(Error::Import("the server URL has no host".to_owned()));
    }
    if server.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Import("the server URL contains spaces".to_owned()));
    }
    Ok(())
}

/// Drops clusters and users that no context references.
fn garbage_collect(config: &mut Kubeconfig) {
    let clusters: HashSet<&str> = config
        .contexts
        .iter()
        .filter_map(|c| c.context.as_ref())
        .map(|c| c.cluster.as_str())
        .collect();
    let users: HashSet<&str> = config
        .contexts
        .iter()
        .filter_map(|c| c.context.as_ref())
        .filter_map(|c| c.user.as_deref())
        .collect();
    config
        .clusters
        .retain(|c| clusters.contains(c.name.as_str()));
    config
        .auth_infos
        .retain(|u| users.contains(u.name.as_str()));
}

/// Cluster and user names that `config` defines or that its contexts reference (a dangling
/// reference is reserved too, so nothing added later can make it resolve).
pub(crate) fn defined_and_referenced_names(
    config: &Kubeconfig,
) -> (HashSet<String>, HashSet<String>) {
    let mut clusters: HashSet<String> = config.clusters.iter().map(|c| c.name.clone()).collect();
    let mut users: HashSet<String> = config.auth_infos.iter().map(|u| u.name.clone()).collect();
    for ctx in config.contexts.iter().filter_map(|c| c.context.as_ref()) {
        clusters.insert(ctx.cluster.clone());
        if let Some(user) = ctx.user.as_deref().filter(|u| !u.is_empty()) {
            users.insert(user.to_owned());
        }
    }
    (clusters, users)
}

/// Renames incoming clusters/users (and the incoming contexts' references) whose names are in
/// `reserved_*`. The old -> new maps are computed once, with every reserved and every incoming
/// name taken up front, and each context is rewritten once from its original reference, so a
/// rename can never land on (and take over the references of) another incoming entry.
pub(crate) fn rename_clashing(
    clusters: &mut [NamedCluster],
    users: &mut [NamedAuthInfo],
    contexts: &mut [kube::config::NamedContext],
    reserved_clusters: &HashSet<String>,
    reserved_users: &HashSet<String>,
    fresh_name: fn(&str, &HashSet<String>) -> String,
) {
    let ctxs = || contexts.iter().filter_map(|c| c.context.as_ref());
    let cluster_map = rename_map(
        clusters
            .iter()
            .map(|c| c.name.as_str())
            .chain(ctxs().map(|c| c.cluster.as_str())),
        reserved_clusters,
        fresh_name,
    );
    let user_map = rename_map(
        users.iter().map(|u| u.name.as_str()).chain(
            ctxs()
                .filter_map(|c| c.user.as_deref())
                .filter(|u| !u.is_empty()),
        ),
        reserved_users,
        fresh_name,
    );
    for cluster in clusters.iter_mut() {
        if let Some(new_name) = cluster_map.get(&cluster.name) {
            cluster.name.clone_from(new_name);
        }
    }
    for user in users.iter_mut() {
        if let Some(new_name) = user_map.get(&user.name) {
            user.name.clone_from(new_name);
        }
    }
    for ctx in contexts.iter_mut().filter_map(|c| c.context.as_mut()) {
        if let Some(new_name) = cluster_map.get(&ctx.cluster) {
            ctx.cluster.clone_from(new_name);
        }
        if let Some(new_name) = ctx.user.as_ref().and_then(|u| user_map.get(u)) {
            ctx.user = Some(new_name.clone());
        }
    }
}

fn rename_map<'a>(
    incoming: impl Iterator<Item = &'a str>,
    reserved: &HashSet<String>,
    fresh_name: fn(&str, &HashSet<String>) -> String,
) -> HashMap<String, String> {
    let incoming: Vec<&str> = incoming.collect();
    let mut taken = reserved.clone();
    taken.extend(incoming.iter().map(|n| (*n).to_owned()));
    let mut map = HashMap::new();
    for name in incoming {
        if reserved.contains(name) && !map.contains_key(name) {
            let new_name = fresh_name(name, &taken);
            taken.insert(new_name.clone());
            map.insert(name.to_owned(), new_name);
        }
    }
    map
}

fn dedupe_by_name<T>(items: &mut Vec<T>, name: impl Fn(&T) -> &String) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(name(item).clone()));
}

fn unique_name(base: &str, taken: &HashSet<String>) -> String {
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or_else(|| base.to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::kubeconfig::KubeconfigSource;

    const ANDROID: ImportOptions = ImportOptions { require_ca: true };
    const DESKTOP: ImportOptions = ImportOptions { require_ca: false };
    const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBfakefakefake\n-----END CERTIFICATE-----";

    /// XORs the bytes and adds a header; counts calls to `open`.
    #[derive(Default)]
    struct FakeVault {
        opens: AtomicUsize,
    }

    const HEADER: &[u8] = b"FAKEVAULT1";

    impl SecretVault for FakeVault {
        fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
            let mut out = HEADER.to_vec();
            out.extend(plain.iter().map(|b| b ^ 0x5a));
            Ok(out)
        }

        fn open(&self, sealed: &[u8]) -> Result<Vec<u8>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let body = sealed
                .strip_prefix(HEADER)
                .ok_or_else(|| Error::Vault("not sealed by this vault".to_owned()))?;
            Ok(body.iter().map(|b| b ^ 0x5a).collect())
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "fleetglass-import-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn store_path(&self) -> PathBuf {
            self.0.join("nested").join("imported.bin")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mixed() -> String {
        std::fs::read_to_string(crate::test_fixture("import-mixed.yaml")).unwrap()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn candidate<'a>(preview: &'a ImportPreview, name: &str) -> &'a ImportCandidate {
        preview
            .contexts
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no candidate {name}"))
    }

    fn token(config: &Kubeconfig, user: &str) -> Option<String> {
        let info = config.auth_infos.iter().find(|u| u.name == user)?;
        let json = serde_json::to_value(info.auth_info.as_ref()?).ok()?;
        json.get("token")?.as_str().map(str::to_owned)
    }

    #[test]
    fn preview_reports_every_problem() {
        let preview = preview_import(&mixed(), &names(&["token-ok"]), &ANDROID).unwrap();
        let problem = |name: &str| candidate(&preview, name).problem.clone();

        assert_eq!(problem("token-ok"), None);
        assert_eq!(problem("cert-ok"), None);
        assert_eq!(problem("oidc"), None);
        assert_eq!(problem("insecure"), None);
        assert_eq!(
            problem("exec"),
            Some(ImportProblem::ExecPlugin {
                command: Some("aws".into())
            })
        );
        assert_eq!(
            problem("gcp"),
            Some(ImportProblem::AuthProvider { name: "gcp".into() })
        );
        for (ctx, field) in [
            ("ca-file", "certificate-authority"),
            ("cert-file", "client-certificate"),
            ("key-file", "client-key"),
            ("token-file", "tokenFile"),
        ] {
            assert_eq!(
                problem(ctx),
                Some(ImportProblem::FileReference {
                    field: field.into()
                }),
                "{ctx}"
            );
        }
        assert_eq!(
            problem("no-ca"),
            Some(ImportProblem::NoCertificateAuthority)
        );
        assert_eq!(
            problem("missing-cluster"),
            Some(ImportProblem::MissingCluster)
        );
        assert_eq!(problem("missing-user"), Some(ImportProblem::MissingUser));
        assert_eq!(problem("no-server"), Some(ImportProblem::NoServer));
        assert_eq!(
            problem("oidc-no-id-token"),
            Some(ImportProblem::OidcWithoutIdToken)
        );
        assert_eq!(
            problem("oidc-idp-ca-file"),
            Some(ImportProblem::FileReference {
                field: "idp-certificate-authority".into()
            })
        );
        for ctx in ["cert-without-key", "key-file-without-cert"] {
            assert_eq!(
                problem(ctx),
                Some(ImportProblem::IncompleteClientCertificate),
                "{ctx}"
            );
        }
        assert_eq!(candidate(&preview, "oidc").auth, AuthType::Oidc);

        let ok = candidate(&preview, "token-ok");
        assert!(ok.exists);
        assert_eq!(ok.auth, AuthType::Token);
        assert_eq!(ok.cluster, "secure");
        assert_eq!(
            ok.server.as_deref(),
            Some("https://secure.example.test:6443")
        );
        assert!(!candidate(&preview, "cert-ok").exists);
        assert_eq!(candidate(&preview, "cert-ok").auth, AuthType::ClientCert);
        assert_eq!(
            candidate(&preview, "exec").auth,
            AuthType::Exec {
                command: Some("aws".into())
            }
        );

        assert_eq!(preview.warnings.len(), 1, "{:?}", preview.warnings);
        assert!(preview.warnings[0].contains("\"insecure\""));
        assert!(preview.warnings[0].contains("insecure-skip-tls-verify"));
        // Preview never carries credentials.
        let json = serde_json::to_string(&preview).unwrap();
        assert!(!json.contains("fixture-secret-token"));
        assert!(!json.contains("fixture-id-token"));
    }

    #[test]
    fn ca_is_only_required_when_asked() {
        let preview = preview_import(&mixed(), &[], &DESKTOP).unwrap();
        assert_eq!(candidate(&preview, "no-ca").problem, None);
        // A CA *file* is still meaningless on another machine.
        assert!(matches!(
            candidate(&preview, "ca-file").problem,
            Some(ImportProblem::FileReference { .. })
        ));
    }

    #[test]
    fn describe_is_clear() {
        let exec = ImportProblem::ExecPlugin {
            command: Some("aws".into()),
        }
        .describe();
        assert!(exec.contains("`aws`"));
        assert!(exec.contains("cannot run on Android"));
        assert!(exec.contains("token or client-certificate"));
        let oidc = ImportProblem::OidcWithoutIdToken.describe();
        assert!(oidc.contains("id-token"), "{oidc}");
        assert!(oidc.contains("not refreshed"), "{oidc}");
        assert!(oidc.contains("re-import"), "{oidc}");
        let provider = ImportProblem::AuthProvider { name: "gcp".into() }.describe();
        assert!(
            provider.contains("only `oidc` with an id-token"),
            "{provider}"
        );
        assert!(
            ImportProblem::IncompleteClientCertificate
                .describe()
                .contains("client-key-data")
        );
        for problem in [
            ImportProblem::ExecPlugin { command: None },
            ImportProblem::AuthProvider { name: "gcp".into() },
            ImportProblem::OidcWithoutIdToken,
            ImportProblem::IncompleteClientCertificate,
            ImportProblem::FileReference {
                field: "client-key".into(),
            },
            ImportProblem::NoCertificateAuthority,
            ImportProblem::MissingCluster,
            ImportProblem::MissingUser,
            ImportProblem::NoServer,
        ] {
            assert!(!problem.describe().is_empty());
        }
    }

    #[test]
    fn problem_serializes_tagged() {
        let json = serde_json::to_value(ImportProblem::FileReference {
            field: "tokenFile".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "fileReference", "field": "tokenFile" })
        );
        assert_eq!(
            serde_json::to_value(ImportProblem::NoCertificateAuthority).unwrap(),
            serde_json::json!({ "type": "noCertificateAuthority" })
        );
        assert_eq!(
            serde_json::to_value(ImportProblem::OidcWithoutIdToken).unwrap(),
            serde_json::json!({ "type": "oidcWithoutIdToken" })
        );
        assert_eq!(
            serde_json::to_value(ImportProblem::IncompleteClientCertificate).unwrap(),
            serde_json::json!({ "type": "incompleteClientCertificate" })
        );
    }

    #[test]
    fn parse_errors_do_not_echo_contents() {
        let yaml = "users:\n  - name: u\n    user:\n      token: [leaked-secret-value\n";
        let err = preview_import(yaml, &[], &ANDROID).unwrap_err();
        assert!(matches!(err, Error::Import(_)));
        assert!(!err.to_string().contains("leaked-secret-value"), "{err}");
    }

    #[test]
    fn size_cap() {
        let mut big = mixed();
        big.push('\n');
        big.push_str(&"#".repeat(MAX_IMPORT_BYTES));
        let err = preview_import(&big, &[], &ANDROID).unwrap_err();
        assert!(err.to_string().contains("1024 KiB"), "{err}");
        assert!(extract_contexts(&big, &names(&["token-ok"]), &ANDROID).is_err());
        // Exactly at the cap is fine.
        let at_cap = format!("{}{}", "#".repeat(MAX_IMPORT_BYTES - 1), "\n");
        assert_eq!(at_cap.len(), MAX_IMPORT_BYTES);
        assert!(preview_import(&at_cap, &[], &ANDROID).is_ok());
    }

    #[test]
    fn extract_keeps_only_referenced_entries() {
        let config =
            extract_contexts(&mixed(), &names(&["token-ok", "cert-ok"]), &ANDROID).unwrap();
        let ctx: Vec<_> = config.contexts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(ctx, ["token-ok", "cert-ok"]);
        let clusters: Vec<_> = config.clusters.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(clusters, ["secure"]);
        let users: Vec<_> = config.auth_infos.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(users, ["token-user", "cert-user"]);
        assert_eq!(config.current_context, None);
        assert!(config.preferences.is_none());
        assert!(config.extensions.is_none());
        assert!(
            config.clusters[0]
                .cluster
                .as_ref()
                .unwrap()
                .extensions
                .is_none()
        );
        assert!(
            config.contexts[0]
                .context
                .as_ref()
                .unwrap()
                .extensions
                .is_none()
        );
        assert_eq!(
            token(&config, "token-user").as_deref(),
            Some("fixture-secret-token")
        );
    }

    #[test]
    fn extract_rejects_problem_and_unknown_contexts() {
        let err = extract_contexts(&mixed(), &names(&["token-ok", "exec"]), &ANDROID).unwrap_err();
        assert!(err.to_string().contains("context \"exec\""), "{err}");
        assert!(err.to_string().contains("`aws`"), "{err}");
        let err = extract_contexts(&mixed(), &names(&["nope"]), &ANDROID).unwrap_err();
        assert!(err.to_string().contains("not in the file"), "{err}");
        assert!(extract_contexts(&mixed(), &[], &ANDROID).is_err());
        // Desktop-style options accept the CA-less context.
        assert!(extract_contexts(&mixed(), &names(&["no-ca"]), &DESKTOP).is_ok());
    }

    #[test]
    fn token_kubeconfig_builds_and_validates() {
        let config = token_kubeconfig(
            " phone ",
            "https://10.0.0.1:6443",
            "  abc.def  \n",
            Some(CA_PEM),
            false,
        )
        .unwrap();
        assert_eq!(config.contexts[0].name, "phone");
        let ctx = config.contexts[0].context.as_ref().unwrap();
        assert_eq!(ctx.cluster, "phone");
        assert_eq!(ctx.user.as_deref(), Some("phone"));
        let cluster = config.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(cluster.server.as_deref(), Some("https://10.0.0.1:6443"));
        let ca = base64::engine::general_purpose::STANDARD
            .decode(cluster.certificate_authority_data.as_deref().unwrap())
            .unwrap();
        assert_eq!(String::from_utf8(ca).unwrap(), format!("{CA_PEM}\n"));
        assert_eq!(token(&config, "phone").as_deref(), Some("abc.def"));
        validate(&config, &ANDROID).unwrap();

        let insecure = token_kubeconfig("t", "https://h", "tok", None, true).unwrap();
        assert_eq!(
            insecure.clusters[0]
                .cluster
                .as_ref()
                .unwrap()
                .insecure_skip_tls_verify,
            Some(true)
        );
        validate(&insecure, &ANDROID).unwrap();
        let no_ca = token_kubeconfig("t", "https://h", "tok", None, false).unwrap();
        assert!(validate(&no_ca, &ANDROID).is_err());
        assert!(validate(&no_ca, &DESKTOP).is_ok());

        let bad = [
            ("", "https://h", "tok", None, false),
            ("  ", "https://h", "tok", None, false),
            ("n", "http://h", "tok", None, false),
            ("n", "h:6443", "tok", None, false),
            ("n", "https://", "tok", None, false),
            ("n", "https:///path", "tok", None, false),
            ("n", "https://h", "  ", None, false),
            ("n", "https://h", "two words", None, false),
            ("n", "https://h", "tok", Some("not a pem"), false),
            ("n", "https://h", "tok", Some(CA_PEM), true),
        ];
        for (name, server, tok, ca, insecure) in bad {
            let err = token_kubeconfig(name, server, tok, ca, insecure).unwrap_err();
            assert!(matches!(err, Error::Import(_)), "{name} {server}");
            assert!(!err.to_string().contains("two words"));
        }
    }

    #[test]
    fn store_add_replace_remove_gc_and_reload() {
        let dir = TempDir::new("store");
        let vault = Arc::new(FakeVault::default());
        let store = ImportedStore::new(dir.store_path(), vault.clone());
        assert!(store.context_names().unwrap().is_empty());
        assert!(!dir.store_path().exists());

        let first = extract_contexts(&mixed(), &names(&["token-ok", "cert-ok"]), &ANDROID).unwrap();
        assert_eq!(
            store.add(first.clone(), false).unwrap(),
            names(&["token-ok", "cert-ok"])
        );
        // Same name without replace is refused and changes nothing.
        let err = store.add(first, false).unwrap_err();
        assert!(err.to_string().contains("already imported"), "{err}");
        assert_eq!(
            store.context_names().unwrap(),
            names(&["token-ok", "cert-ok"])
        );

        // Replace token-ok with a pasted token whose cluster/user are also named "token-ok".
        let replacement = token_kubeconfig(
            "token-ok",
            "https://new.example.test",
            "new-token",
            Some(CA_PEM),
            false,
        )
        .unwrap();
        assert_eq!(store.add(replacement, true).unwrap(), names(&["token-ok"]));
        let config = store.load().unwrap();
        assert_eq!(token(&config, "token-ok").as_deref(), Some("new-token"));
        // token-user is no longer referenced: garbage-collected. "secure" is still used by cert-ok.
        let users: Vec<_> = config.auth_infos.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(users, ["cert-user", "token-ok"]);
        let clusters: Vec<_> = config.clusters.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(clusters, ["secure", "token-ok"]);

        assert!(store.remove("cert-ok").unwrap());
        assert!(!store.remove("cert-ok").unwrap());
        let config = store.load().unwrap();
        assert_eq!(config.contexts.len(), 1);
        let clusters: Vec<_> = config.clusters.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(clusters, ["token-ok"]);
        let users: Vec<_> = config.auth_infos.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(users, ["token-ok"]);

        // A new instance on the same path reads it back through the vault, once.
        let vault2 = Arc::new(FakeVault::default());
        let reopened = ImportedStore::new(dir.store_path(), vault2.clone());
        assert_eq!(reopened.context_names().unwrap(), names(&["token-ok"]));
        assert_eq!(
            token(&reopened.load().unwrap(), "token-ok").as_deref(),
            Some("new-token")
        );
        assert_eq!(
            vault2.opens.load(Ordering::SeqCst),
            1,
            "cached after first read"
        );
        // The original store never reopened the file: it only wrote it.
        assert_eq!(vault.opens.load(Ordering::SeqCst), 0);

        // The sealed file never contains a token in plain text, and is owner-only.
        let on_disk = std::fs::read(dir.store_path()).unwrap();
        assert!(on_disk.starts_with(HEADER));
        for secret in ["new-token", "fixture-secret-token", "token-ok"] {
            assert!(
                !on_disk
                    .windows(secret.len())
                    .any(|w| w == secret.as_bytes()),
                "{secret} in plain text"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.store_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(!dir.store_path().with_file_name("imported.bin.tmp").exists());
        assert!(!format!("{store:?}").contains("new-token"));
    }

    #[test]
    fn store_renames_clashing_cluster_and_user_of_other_contexts() {
        let dir = TempDir::new("rename");
        let store = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        let a =
            token_kubeconfig("shared", "https://a.example.test", "token-a", None, true).unwrap();
        store.add(a, false).unwrap();
        // Another context whose cluster/user carry the same names as the first one's.
        let mut b =
            token_kubeconfig("shared", "https://b.example.test", "token-b", None, true).unwrap();
        b.contexts[0].name = "other".into();
        store.add(b, false).unwrap();

        let source = KubeconfigSource::from_paths(Vec::new()).with_imported(Arc::new(
            ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default())),
        ));
        let list = source.list_contexts().unwrap();
        let server = |name: &str| {
            list.contexts
                .iter()
                .find(|c| c.name == name)
                .and_then(|c| c.server.clone())
        };
        assert_eq!(server("shared").as_deref(), Some("https://a.example.test"));
        assert_eq!(server("other").as_deref(), Some("https://b.example.test"));
        assert_eq!(source.context_user("other").as_deref(), Some("shared-2"));
        let loaded = source.load().unwrap();
        assert_eq!(token(&loaded.config, "shared").as_deref(), Some("token-a"));
        assert_eq!(
            token(&loaded.config, "shared-2").as_deref(),
            Some("token-b")
        );
    }

    #[test]
    fn store_reports_vault_errors() {
        let dir = TempDir::new("corrupt");
        std::fs::create_dir_all(dir.store_path().parent().unwrap()).unwrap();
        std::fs::write(dir.store_path(), b"garbage").unwrap();
        let store = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        assert!(matches!(store.load(), Err(Error::Vault(_))));
        // The file layer stays usable; the problem becomes a warning.
        let source = KubeconfigSource::from_paths(vec![crate::test_fixture("merge-a.yaml")])
            .with_imported(Arc::new(store));
        let loaded = source.load().unwrap();
        assert!(!loaded.config.contexts.is_empty());
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("imported contexts are unavailable"))
        );
    }

    #[test]
    fn source_merges_imported_after_files() {
        let dir = TempDir::new("merge");
        let store = Arc::new(ImportedStore::new(
            dir.store_path(),
            Arc::new(FakeVault::default()),
        ));
        // "dev" also exists in merge-a.yaml; "phone" only here.
        store
            .add(
                token_kubeconfig("dev", "https://imported.example.test", "t1", None, true).unwrap(),
                false,
            )
            .unwrap();
        store
            .add(
                token_kubeconfig("phone", "https://phone.example.test", "t2", None, true).unwrap(),
                false,
            )
            .unwrap();

        let fixture = crate::test_fixture("merge-a.yaml");
        let plain = KubeconfigSource::from_paths(vec![fixture.clone()]);
        assert!(plain.imported().is_none());
        let file_dev = plain
            .list_contexts()
            .unwrap()
            .contexts
            .into_iter()
            .find(|c| c.name == "dev")
            .unwrap();

        let source =
            KubeconfigSource::from_paths(vec![fixture.clone()]).with_imported(store.clone());
        assert!(Arc::ptr_eq(source.imported().unwrap(), &store));
        let loaded = source.load().unwrap();
        assert_eq!(loaded.files, vec![fixture]);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("imported context \"dev\" is hidden")),
            "{:?}",
            loaded.warnings
        );
        let list = source.list_contexts().unwrap();
        let dev = list.contexts.iter().find(|c| c.name == "dev").unwrap();
        assert_eq!(dev, &file_dev, "the file's context wins");
        assert_eq!(list.contexts.iter().filter(|c| c.name == "dev").count(), 1);
        let phone = list.contexts.iter().find(|c| c.name == "phone").unwrap();
        assert_eq!(phone.server.as_deref(), Some("https://phone.example.test"));
        assert_eq!(phone.auth, AuthType::Token);
        assert_eq!(source.context_user("phone").as_deref(), Some("phone"));
        // Files-only order is unchanged: imported contexts come after them.
        let last = loaded.config.contexts.last().unwrap();
        assert_eq!(last.name, "phone");
    }

    #[test]
    fn imported_entries_never_borrow_file_clusters_or_users() {
        let dir = TempDir::new("borrow");
        let store = Arc::new(ImportedStore::new(
            dir.store_path(),
            Arc::new(FakeVault::default()),
        ));
        // Imported cluster/user named like merge-a.yaml's "dev-cluster"/"dev-user".
        let mut config =
            token_kubeconfig("laptop", "https://laptop.example.test", "t3", None, true).unwrap();
        config.clusters[0].name = "dev-cluster".into();
        config.auth_infos[0].name = "dev-user".into();
        let ctx = config.contexts[0].context.as_mut().unwrap();
        ctx.cluster = "dev-cluster".into();
        ctx.user = Some("dev-user".into());
        store.add(config, false).unwrap();

        let source = KubeconfigSource::from_paths(vec![crate::test_fixture("merge-a.yaml")])
            .with_imported(store);
        let list = source.list_contexts().unwrap();
        let laptop = list.contexts.iter().find(|c| c.name == "laptop").unwrap();
        assert_eq!(
            laptop.server.as_deref(),
            Some("https://laptop.example.test")
        );
        assert_eq!(laptop.cluster, "dev-cluster (imported)");
        assert_eq!(laptop.user.as_deref(), Some("dev-user (imported)"));
        let loaded = source.load().unwrap();
        assert_eq!(
            token(&loaded.config, "dev-user (imported)").as_deref(),
            Some("t3")
        );
        // The file's entries are untouched.
        let dev = list.contexts.iter().find(|c| c.name == "dev").unwrap();
        assert_eq!(dev.cluster, "dev-cluster");
    }

    /// Token of the user that context `ctx` resolves to.
    fn context_token(config: &Kubeconfig, ctx: &str) -> Option<String> {
        let user = config
            .contexts
            .iter()
            .find(|c| c.name == ctx)?
            .context
            .as_ref()?
            .user
            .clone()?;
        token(config, &user)
    }

    fn server_of(config: &Kubeconfig, ctx: &str) -> Option<String> {
        let cluster = &config
            .contexts
            .iter()
            .find(|c| c.name == ctx)?
            .context
            .as_ref()?
            .cluster;
        find_cluster(config, cluster)?.server.clone()
    }

    #[test]
    fn store_add_does_not_chain_renames() {
        let dir = TempDir::new("chain");
        let store = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        // Store: context "old" -> cluster "a" / user "a".
        let mut old =
            token_kubeconfig("a", "https://old.example.test", "t-old", None, true).unwrap();
        old.contexts[0].name = "old".into();
        store.add(old, false).unwrap();

        // Incoming: c1 -> a/a, c2 -> a-2/a-2. Renaming "a" must not pick "a-2", and c1 must not
        // follow a later rename of "a-2".
        let yaml = r#"
clusters:
  - name: a
    cluster: { server: "https://c1.example.test", insecure-skip-tls-verify: true }
  - name: a-2
    cluster: { server: "https://c2.example.test", insecure-skip-tls-verify: true }
users:
  - name: a
    user: { token: t-c1 }
  - name: a-2
    user: { token: t-c2 }
contexts:
  - name: c1
    context: { cluster: a, user: a }
  - name: c2
    context: { cluster: a-2, user: a-2 }
"#;
        let incoming = extract_contexts(yaml, &names(&["c1", "c2"]), &ANDROID).unwrap();
        store.add(incoming, false).unwrap();

        let config = store.load().unwrap();
        assert_eq!(context_token(&config, "old").as_deref(), Some("t-old"));
        assert_eq!(context_token(&config, "c1").as_deref(), Some("t-c1"));
        assert_eq!(context_token(&config, "c2").as_deref(), Some("t-c2"));
        assert_eq!(
            server_of(&config, "old").as_deref(),
            Some("https://old.example.test")
        );
        assert_eq!(
            server_of(&config, "c1").as_deref(),
            Some("https://c1.example.test")
        );
        assert_eq!(
            server_of(&config, "c2").as_deref(),
            Some("https://c2.example.test")
        );
        // Names stay unique.
        let mut users: Vec<_> = config.auth_infos.iter().map(|u| u.name.as_str()).collect();
        users.sort_unstable();
        users.dedup();
        assert_eq!(users.len(), 3);
        let mut clusters: Vec<_> = config.clusters.iter().map(|c| c.name.as_str()).collect();
        clusters.sort_unstable();
        clusters.dedup();
        assert_eq!(clusters.len(), 3);
    }

    #[test]
    fn store_add_rejects_dangling_references() {
        let dir = TempDir::new("dangling");
        let store = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        store
            .add(
                token_kubeconfig("old", "https://old.example.test", "t-old", None, true).unwrap(),
                false,
            )
            .unwrap();

        // User "old" is defined only by the earlier import.
        let mut no_user =
            token_kubeconfig("new", "https://n.example.test", "t", None, true).unwrap();
        no_user.auth_infos.clear();
        no_user.contexts[0].context.as_mut().unwrap().user = Some("old".into());
        let err = store.add(no_user, false).unwrap_err();
        assert!(matches!(err, Error::Import(_)), "{err}");
        assert!(err.to_string().contains("context \"new\""), "{err}");
        assert!(err.to_string().contains("user"), "{err}");

        // Cluster "old" likewise.
        let mut no_cluster =
            token_kubeconfig("new", "https://n.example.test", "t", None, true).unwrap();
        no_cluster.clusters.clear();
        no_cluster.contexts[0].context.as_mut().unwrap().cluster = "old".into();
        let err = store.add(no_cluster, false).unwrap_err();
        assert!(matches!(err, Error::Import(_)), "{err}");
        assert!(err.to_string().contains("cluster"), "{err}");

        // An empty user means anonymous, which is fine.
        let mut anonymous =
            token_kubeconfig("anon", "https://n.example.test", "t", None, true).unwrap();
        anonymous.auth_infos.clear();
        anonymous.contexts[0].context.as_mut().unwrap().user = Some(String::new());
        store.add(anonymous, false).unwrap();

        assert_eq!(store.context_names().unwrap(), names(&["old", "anon"]));
    }

    #[test]
    fn failed_open_is_not_retried_by_load_for_a_while() {
        let dir = TempDir::new("backoff");
        std::fs::create_dir_all(dir.store_path().parent().unwrap()).unwrap();
        std::fs::write(dir.store_path(), b"garbage").unwrap();
        let vault = Arc::new(FakeVault::default());
        let store = ImportedStore::new(dir.store_path(), vault.clone());
        let opens = || vault.opens.load(Ordering::SeqCst);

        assert!(matches!(store.load(), Err(Error::Vault(_))));
        assert_eq!(opens(), 1);
        // load() (every client build / context_user) does not hit the vault again ...
        for _ in 0..3 {
            assert!(matches!(store.load(), Err(Error::Vault(_))));
        }
        assert!(store.context_names().is_err());
        let source = KubeconfigSource::from_paths(Vec::new()).with_imported(Arc::new(
            ImportedStore::new(dir.store_path(), vault.clone()),
        ));
        assert!(
            source
                .load()
                .unwrap()
                .warnings
                .iter()
                .any(|w| w.contains("imported contexts are unavailable"))
        );
        assert!(
            source
                .load()
                .unwrap()
                .warnings
                .iter()
                .any(|w| w.contains("imported contexts are unavailable")),
            "the warning stays while the failure is cached"
        );
        assert_eq!(opens(), 2, "one open for the second store, then cached");

        // ... but add/remove retry immediately.
        let config = token_kubeconfig("x", "https://x.example.test", "t", None, true).unwrap();
        assert!(store.add(config.clone(), false).is_err());
        assert!(store.remove("x").is_err());
        assert_eq!(opens(), 4);

        // A successful retry clears the failure.
        std::fs::remove_file(dir.store_path()).unwrap();
        assert!(!store.remove("x").unwrap());
        assert!(store.open_failure.lock().unwrap().is_none());
        assert!(store.load().unwrap().contexts.is_empty());

        // After the backoff, load retries.
        std::fs::write(dir.store_path(), b"garbage").unwrap();
        let mut quick = ImportedStore::new(dir.store_path(), vault.clone());
        quick.retry_after = Duration::ZERO;
        let before = opens();
        assert!(quick.load().is_err());
        assert!(quick.load().is_err());
        assert_eq!(opens(), before + 2);
    }

    #[test]
    fn reset_recovers_a_store_that_cannot_be_opened() {
        let dir = TempDir::new("reset");
        std::fs::create_dir_all(dir.store_path().parent().unwrap()).unwrap();
        // Sealed with a key that is gone: the fake vault cannot open it.
        std::fs::write(dir.store_path(), b"garbage").unwrap();
        let tmp = dir.store_path().with_file_name("imported.bin.tmp");
        std::fs::write(&tmp, b"stale").unwrap();
        let store = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        let config = token_kubeconfig("new", "https://n.example.test", "t", None, true).unwrap();

        assert!(matches!(store.load(), Err(Error::Vault(_))));
        assert!(matches!(
            store.add(config.clone(), false),
            Err(Error::Vault(_))
        ));

        store.reset().unwrap();
        assert!(!dir.store_path().exists());
        assert!(!tmp.exists());
        assert!(store.open_failure.lock().unwrap().is_none());
        assert!(store.load().unwrap().contexts.is_empty());

        assert_eq!(store.add(config, false).unwrap(), names(&["new"]));
        assert_eq!(store.context_names().unwrap(), names(&["new"]));
        // A fresh instance reads the new file from disk.
        let reopened = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        assert_eq!(reopened.context_names().unwrap(), names(&["new"]));
    }

    #[test]
    fn reset_without_a_file_is_ok() {
        let dir = TempDir::new("reset-missing");
        let store = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        store.reset().unwrap();
        store.reset().unwrap();
        assert!(store.load().unwrap().contexts.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn persist_replaces_a_stale_temp_file_with_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = TempDir::new("stale-tmp");
        let tmp = dir.store_path().with_file_name("imported.bin.tmp");
        std::fs::create_dir_all(tmp.parent().unwrap()).unwrap();
        std::fs::write(&tmp, b"stale").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();

        let store = ImportedStore::new(dir.store_path(), Arc::new(FakeVault::default()));
        store
            .add(
                token_kubeconfig("x", "https://x.example.test", "t", None, true).unwrap(),
                false,
            )
            .unwrap();
        let mode = std::fs::metadata(dir.store_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!tmp.exists());
    }

    #[test]
    fn wrong_typed_token_is_not_echoed() {
        let secret = "eyJhbGciOiJSUzI1NiJ9.c2VjcmV0LXBheWxvYWQ";
        for yaml in [
            format!("users:\n  - name: u\n    user: {secret}\n"),
            format!("users:\n  - name: u\n    user: \"{secret}\"\n"),
            format!("users:\n  - name: u\n    user:\n      token: [{secret}]\n"),
        ] {
            let err = preview_import(&yaml, &[], &ANDROID).unwrap_err();
            assert!(matches!(err, Error::Import(_)));
            assert!(!err.to_string().contains(secret), "{err}");
            assert!(!format!("{err:?}").contains(secret), "{err:?}");
            assert!(err.to_string().contains("line"), "{err}");
        }
    }
}
