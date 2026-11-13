//! Container file copy (`P2-06`), the `kubectl cp` equivalent.
//!
//! Like `kubectl cp`, files travel as a tar stream through `exec`: downloads run
//! `tar cf - -C <dir> <base>` and read stdout, uploads run `tar xf - -C <dir>`
//! and write stdin. The container therefore needs a `tar` binary.
//!
//! Both directions go through [`crate::exec::validate_exec_allowed`], so a
//! read-only context blocks uploads (and, because exec is blocked there, also
//! downloads). Archive contents are untrusted: entry paths are validated and the
//! total size is capped.

use std::io::Read;
use std::path::{Component, Path};

use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, AttachParams};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::api::{ContextSettings, ExecSpec};
use crate::error::{Error, Result};
use crate::exec::validate_exec_allowed;

/// Default cap on downloaded file content.
pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// Extra bytes allowed on the raw tar stream on top of the content cap
/// (headers, padding, end-of-archive blocks).
const TAR_OVERHEAD_ALLOWANCE: u64 = 1024 * 1024;
/// Stderr kept for error messages; the rest is drained and discarded.
const MAX_STDERR_BYTES: usize = 16 * 1024;

/// The pod (and optional container) a file copy targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCopyTarget {
    pub context: String,
    pub namespace: String,
    pub pod: String,
    pub container: Option<String>,
}

impl FileCopyTarget {
    fn container(&self) -> Option<&str> {
        self.container.as_deref().filter(|c| !c.is_empty())
    }

    fn exec_spec(&self, command: Vec<String>) -> ExecSpec {
        ExecSpec {
            context: self.context.clone(),
            namespace: self.namespace.clone(),
            pod: self.pod.clone(),
            container: self.container.clone(),
            command,
            cols: 0,
            rows: 0,
        }
    }
}

fn copy_err(message: impl Into<String>) -> Error {
    Error::FileCopy {
        message: message.into(),
    }
}

fn traversal(path: &str) -> Error {
    Error::PathTraversal {
        path: path.to_owned(),
    }
}

/// Validates an upload file name: one plain path segment (no separators,
/// no `.`/`..`, not empty).
fn validate_file_name(file_name: &str) -> Result<()> {
    if file_name.is_empty()
        || file_name == "."
        || file_name == ".."
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.contains('\0')
    {
        return Err(traversal(file_name));
    }
    Ok(())
}

/// Normalizes an archive entry path to `a/b/c`, rejecting absolute paths and
/// `..` components. Returns `None` for entries that name the archive root (`./`).
fn sanitize_entry_path(raw: &str) -> Result<Option<String>> {
    if raw.starts_with('/') || raw.starts_with('\\') || raw.contains('\0') {
        return Err(traversal(raw));
    }
    let mut parts = Vec::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| traversal(raw))?;
                // `..\x` style segments are traversal on Windows hosts.
                if part.split('\\').any(|p| p == "..") || part.contains(':') {
                    return Err(traversal(raw));
                }
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(traversal(raw));
            }
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(parts.join("/")))
}

/// Builds a tar archive with one regular file `file_name` (mode 0644) holding `bytes`.
pub fn build_upload_tar(file_name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    validate_file_name(file_name)?;
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(mtime);
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_data(&mut header, file_name, bytes)
        .map_err(|err| copy_err(format!("cannot build tar archive: {err}")))?;
    builder
        .into_inner()
        .map_err(|err| copy_err(format!("cannot finish tar archive: {err}")))
}

/// Extracts regular files from `tar_bytes` as `(relative path, contents)`.
///
/// - Rejects absolute entry paths and `..` components with [`Error::PathTraversal`].
/// - Stops with [`Error::SizeLimitExceeded`] once file contents pass `max_bytes`.
/// - Directories are implied by paths; symlinks, hard links and special files
///   are skipped because following them could escape the copied tree.
pub fn extract_download_tar(tar_bytes: &[u8], max_bytes: u64) -> Result<Vec<(String, Vec<u8>)>> {
    let mut archive = tar::Archive::new(tar_bytes);
    let entries = archive
        .entries()
        .map_err(|err| copy_err(format!("invalid tar archive: {err}")))?;
    let mut files = Vec::new();
    let mut total: u64 = 0;
    for entry in entries {
        let entry = entry.map_err(|err| copy_err(format!("invalid tar archive: {err}")))?;
        let raw_path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let Some(path) = sanitize_entry_path(&raw_path)? else {
            continue;
        };
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() {
            if !entry_type.is_dir() {
                tracing::debug!(path = %path, ?entry_type, "skipping non-regular tar entry");
            }
            continue;
        }
        let size = entry.size();
        total = total.saturating_add(size);
        if total > max_bytes {
            return Err(Error::SizeLimitExceeded {
                limit_bytes: max_bytes,
            });
        }
        let mut contents = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        // `take` guards against a header that understates the real data length.
        entry
            .take(size)
            .read_to_end(&mut contents)
            .map_err(|err| copy_err(format!("cannot read '{path}' from tar archive: {err}")))?;
        files.push((path, contents));
    }
    Ok(files)
}

/// Splits a container path into `(dir, base)` for `tar -C <dir> <base>`.
fn split_container_path(path: &str) -> (String, String) {
    let trimmed = path.trim_end_matches('/');
    let (dir, base) = match trimmed.rsplit_once('/') {
        Some(("", base)) => ("/", base),
        Some((dir, base)) => (dir, base),
        None if trimmed.is_empty() && path.starts_with('/') => ("/", "."),
        None => (".", trimmed),
    };
    let base = if base.is_empty() { "." } else { base };
    // A leading `-` would be parsed by tar as an option.
    let base = if base.starts_with('-') {
        format!("./{base}")
    } else {
        base.to_owned()
    };
    (dir.to_owned(), base)
}

/// Command run in the container to stream `path` (file or directory) as a tar
/// archive on stdout: `tar cf - -C <dir> <base>`.
pub fn copy_from_pod_command(path: &str) -> Vec<String> {
    let (dir, base) = split_container_path(path);
    vec![
        "tar".to_owned(),
        "cf".to_owned(),
        "-".to_owned(),
        "-C".to_owned(),
        dir,
        base,
    ]
}

/// Command run in the container to unpack a tar archive from stdin into `dir`:
/// `tar xf - -C <dir>`.
pub fn copy_to_pod_command(dir: &str) -> Vec<String> {
    vec![
        "tar".to_owned(),
        "xf".to_owned(),
        "-".to_owned(),
        "-C".to_owned(),
        dir.to_owned(),
    ]
}

fn container_flag(target: &FileCopyTarget) -> String {
    target
        .container()
        .map(|c| format!(" -c {c}"))
        .unwrap_or_default()
}

/// `kubectl cp` equivalent of downloading `remote_path` to `local_path`.
pub fn preview_copy_from_pod(
    target: &FileCopyTarget,
    remote_path: &str,
    local_path: &str,
) -> String {
    format!(
        "kubectl --context {} -n {} cp {}:{}{} {}",
        target.context,
        target.namespace,
        target.pod,
        remote_path,
        container_flag(target),
        local_path
    )
}

/// `kubectl cp` equivalent of uploading `local_path` into `remote_dir/file_name`.
pub fn preview_copy_to_pod(
    target: &FileCopyTarget,
    local_path: &str,
    remote_dir: &str,
    file_name: &str,
) -> String {
    let dir = remote_dir.trim_end_matches('/');
    format!(
        "kubectl --context {} -n {} cp {} {}:{}/{}{}",
        target.context,
        target.namespace,
        local_path,
        target.pod,
        dir,
        file_name,
        container_flag(target)
    )
}

/// Reads at most `cap` bytes; `Ok(None)` when the stream is longer.
async fn read_capped(reader: impl AsyncRead + Unpin, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    reader
        .take(cap.saturating_add(1))
        .read_to_end(&mut buf)
        .await?;
    Ok((buf.len() as u64 <= cap).then_some(buf))
}

/// Keeps the first [`MAX_STDERR_BYTES`] of stderr and drains the rest so the
/// remote process never blocks on a full pipe.
async fn drain_stderr(mut reader: impl AsyncRead + Unpin) -> String {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_STDERR_BYTES.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    String::from_utf8_lossy(&kept).trim().to_owned()
}

/// Turns a non-success exec status into a [`Error::FileCopy`] naming stderr.
fn check_status(
    status: Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Status>,
    stderr: &str,
) -> Result<()> {
    let Some(status) = status else {
        return Ok(());
    };
    if status.status.as_deref() == Some("Failure") {
        let detail = if stderr.is_empty() {
            status.message.unwrap_or_default()
        } else {
            stderr.to_owned()
        };
        return Err(copy_err(format!("tar in container failed: {detail}")));
    }
    Ok(())
}

fn attach_params(target: &FileCopyTarget, stdin: bool) -> AttachParams {
    let ap = AttachParams::default()
        .stdin(stdin)
        .stdout(true)
        .stderr(true);
    match target.container() {
        Some(c) => ap.container(c),
        None => ap,
    }
}

/// Downloads `remote_path` (a file or directory) from the container and returns
/// its regular files as `(relative path, contents)`, capped at `max_bytes` of content.
pub async fn copy_from_pod(
    client: &Client,
    settings: &ContextSettings,
    target: &FileCopyTarget,
    remote_path: &str,
    max_bytes: u64,
) -> Result<Vec<(String, Vec<u8>)>> {
    if remote_path.trim_end_matches('/').is_empty() && !remote_path.starts_with('/') {
        return Err(traversal(remote_path));
    }
    let spec = target.exec_spec(copy_from_pod_command(remote_path));
    let command = validate_exec_allowed(&spec, settings, None)?;

    let pods: Api<Pod> = Api::namespaced(client.clone(), &target.namespace);
    let mut attached = pods
        .exec(&target.pod, command, &attach_params(target, false))
        .await?;
    let stdout = attached
        .stdout()
        .ok_or_else(|| copy_err("exec stdout unavailable"))?;
    let stderr = attached
        .stderr()
        .ok_or_else(|| copy_err("exec stderr unavailable"))?;
    let status = attached.take_status();

    let raw_cap = max_bytes.saturating_add(TAR_OVERHEAD_ALLOWANCE);
    let (tar_bytes, stderr) = tokio::join!(read_capped(stdout, raw_cap), drain_stderr(stderr));
    let tar_bytes = match tar_bytes {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            attached.abort();
            return Err(Error::SizeLimitExceeded {
                limit_bytes: max_bytes,
            });
        }
        Err(err) => {
            attached.abort();
            return Err(copy_err(format!("reading tar stream failed: {err}")));
        }
    };
    let status = match status {
        Some(fut) => fut.await,
        None => None,
    };
    check_status(status, &stderr)?;
    attached
        .join()
        .await
        .map_err(|err| copy_err(format!("exec stream failed: {err}")))?;
    extract_download_tar(&tar_bytes, max_bytes)
}

/// Uploads `bytes` as `remote_dir/file_name` in the container, overwriting an
/// existing file. Blocked on read-only contexts.
pub async fn copy_to_pod(
    client: &Client,
    settings: &ContextSettings,
    target: &FileCopyTarget,
    remote_dir: &str,
    file_name: &str,
    bytes: &[u8],
) -> Result<()> {
    if remote_dir.is_empty() {
        return Err(traversal(remote_dir));
    }
    let spec = target.exec_spec(copy_to_pod_command(remote_dir));
    let command = validate_exec_allowed(&spec, settings, None)?;
    let archive = build_upload_tar(file_name, bytes)?;

    let pods: Api<Pod> = Api::namespaced(client.clone(), &target.namespace);
    let mut attached = pods
        .exec(&target.pod, command, &attach_params(target, true))
        .await?;
    let mut stdin = attached
        .stdin()
        .ok_or_else(|| copy_err("exec stdin unavailable"))?;
    let stdout = attached
        .stdout()
        .ok_or_else(|| copy_err("exec stdout unavailable"))?;
    let stderr = attached
        .stderr()
        .ok_or_else(|| copy_err("exec stderr unavailable"))?;
    let status = attached.take_status();

    let write = async move {
        stdin.write_all(&archive).await?;
        stdin.shutdown().await
    };
    let (written, _stdout, stderr) =
        tokio::join!(write, drain_stderr(stdout), drain_stderr(stderr));
    if let Err(err) = written {
        attached.abort();
        return Err(copy_err(format!(
            "writing tar stream failed: {err}{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(" ({stderr})")
            }
        )));
    }
    let status = match status {
        Some(fut) => fut.await,
        None => None,
    };
    check_status(status, &stderr)?;
    attached
        .join()
        .await
        .map_err(|err| copy_err(format!("exec stream failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(container: Option<&str>) -> FileCopyTarget {
        FileCopyTarget {
            context: "prod-eu".to_owned(),
            namespace: "payments".to_owned(),
            pod: "api-7f8b9-x12".to_owned(),
            container: container.map(str::to_owned),
        }
    }

    /// Builds an archive with raw (unvalidated) entry names, as a hostile container could.
    fn raw_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries {
            let mut header = tar::Header::new_old();
            {
                let bytes = header.as_old_mut();
                bytes.name[..name.len()].copy_from_slice(name.as_bytes());
            }
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            out.extend_from_slice(header.as_bytes());
            out.extend_from_slice(data);
            let pad = (512 - data.len() % 512) % 512;
            out.extend(std::iter::repeat_n(0u8, pad));
        }
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }

    #[test]
    fn tar_round_trip() {
        let payload = b"hello from fleetglass\n".repeat(100);
        let archive = build_upload_tar("notes.txt", &payload).unwrap();
        let files = extract_download_tar(&archive, 1 << 20).unwrap();
        assert_eq!(files, vec![("notes.txt".to_owned(), payload)]);

        let empty = build_upload_tar("empty", b"").unwrap();
        assert_eq!(
            extract_download_tar(&empty, 0).unwrap(),
            vec![("empty".to_owned(), Vec::new())]
        );
    }

    #[test]
    fn extract_keeps_directory_trees_and_strips_dot_prefix() {
        let archive = raw_tar(&[("./etc/app/a.conf", b"a"), ("etc/app/b.conf", b"bb")]);
        let files = extract_download_tar(&archive, 10).unwrap();
        assert_eq!(
            files,
            vec![
                ("etc/app/a.conf".to_owned(), b"a".to_vec()),
                ("etc/app/b.conf".to_owned(), b"bb".to_vec()),
            ]
        );
    }

    #[test]
    fn extract_rejects_path_traversal() {
        for bad in [
            "../evil",
            "a/../../evil",
            "/etc/passwd",
            "ok/..",
            "..\\evil",
        ] {
            let archive = raw_tar(&[(bad, b"x")]);
            let err = extract_download_tar(&archive, 1024).unwrap_err();
            assert!(
                matches!(err, Error::PathTraversal { ref path } if path == bad),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn upload_rejects_unsafe_file_names() {
        for bad in ["", ".", "..", "a/b", "../x", "/abs", "a\\b"] {
            assert!(
                matches!(
                    build_upload_tar(bad, b"x"),
                    Err(Error::PathTraversal { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn extract_stops_past_size_limit() {
        let archive = raw_tar(&[("a", &[1u8; 600]), ("b", &[2u8; 600])]);
        assert_eq!(extract_download_tar(&archive, 1200).unwrap().len(), 2);
        let err = extract_download_tar(&archive, 1000).unwrap_err();
        assert!(matches!(
            err,
            Error::SizeLimitExceeded { limit_bytes: 1000 }
        ));
        let err = extract_download_tar(&archive, 10).unwrap_err();
        assert!(matches!(err, Error::SizeLimitExceeded { limit_bytes: 10 }));
    }

    #[test]
    fn extract_rejects_garbage() {
        assert!(matches!(
            extract_download_tar(&[7u8; 600], 1024),
            Err(Error::FileCopy { .. })
        ));
    }

    #[test]
    fn copy_commands_format() {
        assert_eq!(
            copy_from_pod_command("/var/log/app.log"),
            ["tar", "cf", "-", "-C", "/var/log", "app.log"]
        );
        assert_eq!(
            copy_from_pod_command("/etc/nginx/"),
            ["tar", "cf", "-", "-C", "/etc", "nginx"]
        );
        assert_eq!(
            copy_from_pod_command("/data"),
            ["tar", "cf", "-", "-C", "/", "data"]
        );
        assert_eq!(
            copy_from_pod_command("relative.txt"),
            ["tar", "cf", "-", "-C", ".", "relative.txt"]
        );
        assert_eq!(
            copy_from_pod_command("/tmp/-rf"),
            ["tar", "cf", "-", "-C", "/tmp", "./-rf"]
        );
        assert_eq!(
            copy_to_pod_command("/tmp/upload"),
            ["tar", "xf", "-", "-C", "/tmp/upload"]
        );
    }

    #[test]
    fn kubectl_previews_format_with_and_without_container() {
        assert_eq!(
            preview_copy_from_pod(&target(None), "/var/log/app.log", "./app.log"),
            "kubectl --context prod-eu -n payments cp api-7f8b9-x12:/var/log/app.log ./app.log"
        );
        assert_eq!(
            preview_copy_from_pod(&target(Some("app")), "/var/log/app.log", "./app.log"),
            "kubectl --context prod-eu -n payments cp api-7f8b9-x12:/var/log/app.log -c app ./app.log"
        );
        assert_eq!(
            preview_copy_to_pod(&target(Some("app")), "./cfg.yaml", "/tmp/", "cfg.yaml"),
            "kubectl --context prod-eu -n payments cp ./cfg.yaml api-7f8b9-x12:/tmp/cfg.yaml -c app"
        );
    }

    #[test]
    fn read_only_context_blocks_copy_exec() {
        let settings = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };
        let spec = target(None).exec_spec(copy_to_pod_command("/tmp"));
        assert!(matches!(
            validate_exec_allowed(&spec, &settings, None),
            Err(Error::ReadOnlyContext { ref context }) if context == "prod-eu"
        ));
        let ok = validate_exec_allowed(&spec, &ContextSettings::default(), None).unwrap();
        assert_eq!(ok, copy_to_pod_command("/tmp"));
    }
}
