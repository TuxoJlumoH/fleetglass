//! In-cluster Helm v3 release reader (`P2-07`).
//!
//! Helm v3 stores releases as Kubernetes Secrets with label `owner=helm` and
//! name prefix `sh.helm.release.v1.`. The Secret data contains a base64-gzipped
//! JSON string in the `release` field.

use std::collections::BTreeMap;
use std::path::PathBuf;

use futures::{StreamExt, stream};
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Client;
use kube::api::{Api, ListParams};
use serde::Deserialize;

use crate::api::{HelmReleaseDetail, HelmReleasePage, HelmReleaseSummary, HelmRevisionDiff};
use crate::detail::compute_unified_diff;
use crate::error::{Error, Result};

fn helm_err(msg: impl Into<String>) -> Error {
    Error::KubeconfigParse {
        path: PathBuf::from("helm-release"),
        line: None,
        column: None,
        message: msg.into(),
    }
}

/// Upper bound on a decompressed Helm release payload. Secrets are capped at
/// 1 MiB by the API server, so anything inflating past this is malformed or hostile.
const MAX_DECOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// Decompresses a gzip-encoded byte slice (RFC 1952).
pub fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(MAX_DECOMPRESSED_BYTES + 1)
        .read_to_end(&mut out)
        .map_err(|err| helm_err(format!("invalid gzip payload: {err}")))?;
    if out.len() as u64 > MAX_DECOMPRESSED_BYTES {
        return Err(helm_err("decompressed helm release exceeds 64 MiB"));
    }
    Ok(out)
}

/// Decodes base64-encoded ASCII bytes.
fn decode_base64(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for &b in input {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a' + 26),
            b'0'..=b'9' => u32::from(b - b'0' + 52),
            b'+' => 62,
            b'/' => 63,
            _ => return Err(helm_err(format!("invalid base64 byte: {b}"))),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct RawHelmRelease {
    #[serde(default)]
    name: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    info: Option<RawHelmInfo>,
    #[serde(default)]
    chart: Option<RawHelmChart>,
    #[serde(default)]
    config: Option<serde_json::Value>,
    #[serde(default)]
    manifest: String,
}

#[derive(Debug, Deserialize)]
struct RawHelmInfo {
    #[serde(default)]
    last_deployed: Option<String>,
    #[serde(default)]
    first_deployed: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawHelmChart {
    #[serde(default)]
    metadata: Option<RawHelmMetadata>,
}

#[derive(Debug, Deserialize)]
struct RawHelmMetadata {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "appVersion")]
    app_version: Option<String>,
}

fn json_to_yaml(value: &serde_json::Value) -> String {
    let mut out = String::new();
    format_yaml_value(value, 0, &mut out);
    out
}

fn format_yaml_value(value: &serde_json::Value, indent: usize, out: &mut String) {
    let spaces = "  ".repeat(indent);
    match value {
        serde_json::Value::Null => out.push_str("null\n"),
        serde_json::Value::Bool(b) => {
            out.push_str(if *b { "true\n" } else { "false\n" });
        }
        serde_json::Value::Number(n) => {
            out.push_str(&format!("{n}\n"));
        }
        serde_json::Value::String(s) => {
            if s.contains('\n') {
                out.push_str("|\n");
                for line in s.lines() {
                    out.push_str(&spaces);
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            } else if s.is_empty()
                || s.contains(':')
                || s.contains('#')
                || s.starts_with(' ')
                || s.ends_with(' ')
            {
                out.push_str(&format!("\"{s}\"\n"));
            } else {
                out.push_str(s);
                out.push('\n');
            }
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]\n");
            } else {
                out.push('\n');
                for item in arr {
                    out.push_str(&spaces);
                    out.push_str("- ");
                    format_yaml_value(item, indent + 1, out);
                }
            }
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}\n");
            } else {
                if indent > 0 {
                    out.push('\n');
                }
                for (k, v) in map {
                    out.push_str(&spaces);
                    out.push_str(k);
                    out.push_str(": ");
                    format_yaml_value(v, indent + 1, out);
                }
            }
        }
    }
}

/// Parses raw Helm v3 release data (`raw_base64_gzip`) into [`HelmReleaseDetail`].
///
/// Accepts base64-encoded ASCII bytes, raw gzip bytes, or uncompressed JSON.
pub fn parse_helm_release_data(raw_base64_gzip: &[u8]) -> Result<HelmReleaseDetail> {
    let decompressed = if raw_base64_gzip.starts_with(b"{") {
        raw_base64_gzip.to_vec()
    } else if raw_base64_gzip.starts_with(&[0x1f, 0x8b]) {
        decompress_gzip(raw_base64_gzip)?
    } else {
        let decoded = decode_base64(raw_base64_gzip)?;
        if decoded.starts_with(&[0x1f, 0x8b]) {
            decompress_gzip(&decoded)?
        } else {
            decoded
        }
    };

    let raw_release: RawHelmRelease =
        serde_json::from_slice(&decompressed).map_err(|err| Error::KubeconfigParse {
            path: PathBuf::from("helm-release"),
            line: None,
            column: None,
            message: format!("failed to parse helm release JSON: {err}"),
        })?;

    let info = raw_release.info.unwrap_or(RawHelmInfo {
        last_deployed: None,
        first_deployed: None,
        status: None,
        notes: None,
    });
    let meta = raw_release
        .chart
        .and_then(|c| c.metadata)
        .unwrap_or(RawHelmMetadata {
            name: None,
            version: None,
            app_version: None,
        });

    let updated = info
        .last_deployed
        .or(info.first_deployed)
        .unwrap_or_default();
    let status = info.status.unwrap_or_else(|| "unknown".to_owned());
    let chart = match (&meta.name, &meta.version) {
        (Some(name), Some(version)) if !version.is_empty() => format!("{name}-{version}"),
        (Some(name), _) => name.clone(),
        _ => String::new(),
    };
    let app_version = meta.app_version.unwrap_or_default();
    let notes = info.notes.unwrap_or_default();

    let values_yaml = match raw_release.config {
        Some(ref val) if val.is_object() && !val.as_object().is_none_or(|m| m.is_empty()) => {
            json_to_yaml(val)
        }
        Some(ref val) if val.is_string() => val.as_str().unwrap_or_default().to_owned(),
        _ => String::new(),
    };

    let summary = HelmReleaseSummary {
        name: raw_release.name,
        namespace: raw_release.namespace,
        revision: raw_release.version,
        updated,
        status,
        chart,
        app_version,
    };

    Ok(HelmReleaseDetail {
        summary,
        values_yaml,
        manifest_yaml: raw_release.manifest,
        notes,
    })
}

/// Extracts [`HelmReleaseSummary`] directly from a Kubernetes Helm release [`Secret`].
pub fn parse_helm_release_secret(secret: &Secret) -> Result<HelmReleaseSummary> {
    let detail = parse_helm_release_secret_detail(secret)?;
    Ok(detail.summary)
}

/// Extracts [`HelmReleaseDetail`] directly from a Kubernetes Helm release [`Secret`].
pub fn parse_helm_release_secret_detail(secret: &Secret) -> Result<HelmReleaseDetail> {
    let release_bytes = secret
        .data
        .as_ref()
        .and_then(|d| d.get("release"))
        .map(|b| b.0.clone())
        .or_else(|| {
            secret
                .string_data
                .as_ref()
                .and_then(|d| d.get("release"))
                .map(|s| s.as_bytes().to_vec())
        });

    let bytes = release_bytes.ok_or_else(|| Error::KubeconfigParse {
        path: PathBuf::from("helm-secret"),
        line: None,
        column: None,
        message: "Secret does not contain 'release' data".to_owned(),
    })?;

    let mut detail = parse_helm_release_data(&bytes)?;
    if detail.summary.namespace.is_empty()
        && let Some(ns) = &secret.metadata.namespace
    {
        detail.summary.namespace = ns.clone();
    }
    Ok(detail)
}

/// Label selector matching Helm v3 release Secrets.
pub const HELM_RELEASE_LABEL_SELECTOR: &str = "owner=helm";
/// Field selector matching Helm v3 release Secrets.
pub const HELM_RELEASE_FIELD_SELECTOR: &str = "type=helm.sh/release.v1";
/// Upper bound on releases returned by [`list_helm_releases`]; callers window it further.
pub const MAX_HELM_RELEASES: usize = 10_000;
/// Upper bound on revisions returned by [`helm_release_history`].
pub const MAX_HELM_HISTORY: usize = 1_000;
/// Maximum page size accepted by [`window_helm_releases`].
pub const MAX_HELM_PAGE_LIMIT: u32 = 1_000;
/// Page size used when listing Secrets from the API server.
const LIST_PAGE_SIZE: u32 = 500;
/// Concurrent Secret GETs while fetching the latest revision of each release.
const FETCH_CONCURRENCY: usize = 8;

/// Identity of one Helm release revision, taken from Secret metadata only
/// (no payload decoding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelmRevisionKey {
    pub namespace: String,
    pub release: String,
    pub revision: u32,
    /// Name of the Secret holding this revision.
    pub secret_name: String,
}

/// Name of the Secret Helm v3 uses for `release` at `revision`.
pub fn helm_release_secret_name(release: &str, revision: u32) -> String {
    format!("sh.helm.release.v1.{release}.v{revision}")
}

/// Reads release name and revision from Helm's `name`/`version` labels, falling
/// back to the `sh.helm.release.v1.<name>.v<rev>` Secret name. `None` when the
/// Secret does not look like a Helm v3 release.
pub fn helm_revision_key(meta: &ObjectMeta) -> Option<HelmRevisionKey> {
    let secret_name = meta.name.clone()?;
    let labels = meta.labels.as_ref();
    let from_labels = labels.and_then(|l| {
        let release = l.get("name")?.clone();
        let revision = l.get("version")?.parse::<u32>().ok()?;
        Some((release, revision))
    });
    let (release, revision) = match from_labels {
        Some(found) => found,
        None => {
            let rest = secret_name.strip_prefix("sh.helm.release.v1.")?;
            let (release, rev) = rest.rsplit_once(".v")?;
            (release.to_owned(), rev.parse::<u32>().ok()?)
        }
    };
    if release.is_empty() {
        return None;
    }
    Some(HelmRevisionKey {
        namespace: meta.namespace.clone().unwrap_or_default(),
        release,
        revision,
        secret_name,
    })
}

/// Keeps only the highest revision per `(namespace, release)`, sorted by
/// `(namespace, release)`.
pub fn latest_revisions(keys: impl IntoIterator<Item = HelmRevisionKey>) -> Vec<HelmRevisionKey> {
    let mut latest: BTreeMap<(String, String), HelmRevisionKey> = BTreeMap::new();
    for key in keys {
        let slot = (key.namespace.clone(), key.release.clone());
        match latest.get(&slot) {
            Some(existing) if existing.revision >= key.revision => {}
            _ => {
                latest.insert(slot, key);
            }
        }
    }
    latest.into_values().collect()
}

/// Keeps only the highest revision per `(namespace, name)` of already parsed
/// summaries, sorted by `(namespace, name)`.
pub fn latest_release_summaries(
    summaries: impl IntoIterator<Item = HelmReleaseSummary>,
) -> Vec<HelmReleaseSummary> {
    let mut latest: BTreeMap<(String, String), HelmReleaseSummary> = BTreeMap::new();
    for summary in summaries {
        let slot = (summary.namespace.clone(), summary.name.clone());
        match latest.get(&slot) {
            Some(existing) if existing.revision >= summary.revision => {}
            _ => {
                latest.insert(slot, summary);
            }
        }
    }
    latest.into_values().collect()
}

/// Sorts release revisions newest first and caps them at [`MAX_HELM_HISTORY`].
pub fn sort_history_desc(mut revisions: Vec<HelmReleaseSummary>) -> Vec<HelmReleaseSummary> {
    revisions.sort_by_key(|s| std::cmp::Reverse(s.revision));
    revisions.truncate(MAX_HELM_HISTORY);
    revisions
}

/// Returns one window of `releases`; `limit` is clamped to [`MAX_HELM_PAGE_LIMIT`].
pub fn window_helm_releases(
    releases: &[HelmReleaseSummary],
    offset: u32,
    limit: u32,
) -> HelmReleasePage {
    let limit = limit.min(MAX_HELM_PAGE_LIMIT) as usize;
    let start = (offset as usize).min(releases.len());
    let end = start.saturating_add(limit).min(releases.len());
    HelmReleasePage {
        total: u32::try_from(releases.len()).unwrap_or(u32::MAX),
        offset,
        releases: releases[start..end].to_vec(),
    }
}

/// Line diffs of values and manifest from revision `a` (before) to `b` (after).
pub fn diff_helm_revisions(a: &HelmReleaseDetail, b: &HelmReleaseDetail) -> HelmRevisionDiff {
    HelmRevisionDiff {
        values_diff: compute_unified_diff(&a.values_yaml, &b.values_yaml),
        manifest_diff: compute_unified_diff(&a.manifest_yaml, &b.manifest_yaml),
    }
}

/// The `helm` command equivalent to rolling `name` back to `revision`.
///
/// Preview only: Fleetglass does not execute rollbacks.
pub fn preview_helm_rollback(namespace: &str, name: &str, revision: u32) -> String {
    format!("helm rollback {name} {revision} -n {namespace}")
}

fn helm_secrets_api(client: &Client, namespace: Option<&str>) -> Api<Secret> {
    match namespace.filter(|ns| !ns.is_empty()) {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    }
}

/// Lists Helm release Secret metadata (payloads are not transferred), following
/// continue tokens.
async fn list_revision_keys(api: &Api<Secret>, labels: &str) -> Result<Vec<HelmRevisionKey>> {
    let mut keys = Vec::new();
    let mut continue_token: Option<String> = None;
    loop {
        let mut lp = ListParams::default()
            .labels(labels)
            .fields(HELM_RELEASE_FIELD_SELECTOR)
            .limit(LIST_PAGE_SIZE);
        if let Some(token) = continue_token.as_deref() {
            lp = lp.continue_token(token);
        }
        let page = api.list_metadata(&lp).await?;
        keys.extend(
            page.items
                .iter()
                .filter_map(|m| helm_revision_key(&m.metadata)),
        );
        continue_token = page.metadata.continue_.filter(|t| !t.is_empty());
        if continue_token.is_none() {
            break;
        }
    }
    Ok(keys)
}

/// Fetches and parses the Secrets for `keys`. Revisions deleted in the meantime
/// and payloads that fail to parse are skipped (logged), other API errors fail.
async fn fetch_summaries(
    client: &Client,
    keys: Vec<HelmRevisionKey>,
) -> Result<Vec<HelmReleaseSummary>> {
    let results: Vec<Result<Option<HelmReleaseSummary>>> = stream::iter(keys)
        .map(|key| {
            let api: Api<Secret> = Api::namespaced(client.clone(), &key.namespace);
            async move {
                let Some(secret) = api.get_opt(&key.secret_name).await? else {
                    return Ok(None);
                };
                match parse_helm_release_secret(&secret) {
                    Ok(summary) => Ok(Some(summary)),
                    Err(err) => {
                        tracing::warn!(
                            namespace = %key.namespace,
                            secret = %key.secret_name,
                            error = %err,
                            "skipping unparsable helm release secret"
                        );
                        Ok(None)
                    }
                }
            }
        })
        .buffered(FETCH_CONCURRENCY)
        .collect()
        .await;
    let mut out = Vec::with_capacity(results.len());
    for result in results {
        if let Some(summary) = result? {
            out.push(summary);
        }
    }
    Ok(out)
}

/// Lists Helm v3 releases (latest revision each), sorted by `(namespace, name)`
/// and capped at [`MAX_HELM_RELEASES`]. `None` lists all namespaces.
///
/// Grouping happens on Secret metadata first, so only one payload per release
/// is downloaded and decoded. Page the result with [`window_helm_releases`].
pub async fn list_helm_releases(
    client: &Client,
    namespace: Option<&str>,
) -> Result<Vec<HelmReleaseSummary>> {
    let api = helm_secrets_api(client, namespace);
    let keys = list_revision_keys(&api, HELM_RELEASE_LABEL_SELECTOR).await?;
    let mut latest = latest_revisions(keys);
    if latest.len() > MAX_HELM_RELEASES {
        tracing::warn!(
            total = latest.len(),
            cap = MAX_HELM_RELEASES,
            "helm release list truncated"
        );
        latest.truncate(MAX_HELM_RELEASES);
    }
    let summaries = fetch_summaries(client, latest).await?;
    // Re-group on parsed data in case labels and payload disagree.
    Ok(latest_release_summaries(summaries))
}

/// All stored revisions of release `name` in `namespace`, newest first, capped
/// at [`MAX_HELM_HISTORY`].
pub async fn helm_release_history(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Vec<HelmReleaseSummary>> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let labels = format!("{HELM_RELEASE_LABEL_SELECTOR},name={name}");
    let mut keys = list_revision_keys(&api, &labels).await?;
    keys.retain(|k| k.release == name);
    keys.sort_by_key(|k| std::cmp::Reverse(k.revision));
    keys.truncate(MAX_HELM_HISTORY);
    let summaries = fetch_summaries(client, keys).await?;
    Ok(sort_history_desc(summaries))
}

/// Full detail (values, manifest, notes) of one release revision, for the
/// detail view and [`diff_helm_revisions`].
pub async fn get_helm_release_detail(
    client: &Client,
    namespace: &str,
    name: &str,
    revision: u32,
) -> Result<HelmReleaseDetail> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api.get(&helm_release_secret_name(name, revision)).await?;
    parse_helm_release_secret_detail(&secret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn gzip_compress_uncompressed(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        // Gzip header (10 bytes)
        out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]);
        // DEFLATE uncompressed block: BFINAL=1, BTYPE=00 -> 0x01
        let len = data.len() as u16;
        let nlen = !len;
        out.push(0x01);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(data);
        // CRC32 and ISIZE
        let crc = compute_crc32(data);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }

    fn compute_crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB8_8320;
                } else {
                    crc >>= 1;
                }
            }
        }
        !crc
    }

    fn encode_base64_str(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0];
            let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
            let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
            out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(TABLE[(triple & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    #[test]
    fn parse_helm_release_uncompressed_roundtrip() {
        let json_payload = serde_json::json!({
            "name": "redis-cluster",
            "namespace": "databases",
            "version": 3,
            "info": {
                "first_deployed": "2026-05-01T10:00:00Z",
                "last_deployed": "2026-05-02T15:30:00Z",
                "status": "deployed",
                "notes": "Redis is operational."
            },
            "chart": {
                "metadata": {
                    "name": "redis",
                    "version": "17.3.1",
                    "appVersion": "7.0.5"
                }
            },
            "config": {
                "cluster": {
                    "enabled": true,
                    "nodes": 6
                }
            },
            "manifest": "---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: redis-cfg\n"
        });

        let json_bytes = serde_json::to_vec(&json_payload).unwrap();
        let gz = gzip_compress_uncompressed(&json_bytes);
        let b64 = encode_base64_str(&gz);

        let detail = parse_helm_release_data(b64.as_bytes()).unwrap();
        assert_eq!(detail.summary.name, "redis-cluster");
        assert_eq!(detail.summary.namespace, "databases");
        assert_eq!(detail.summary.revision, 3);
        assert_eq!(detail.summary.status, "deployed");
        assert_eq!(detail.summary.updated, "2026-05-02T15:30:00Z");
        assert_eq!(detail.summary.chart, "redis-17.3.1");
        assert_eq!(detail.summary.app_version, "7.0.5");
        assert!(detail.values_yaml.contains("nodes: 6"));
        assert!(detail.manifest_yaml.contains("redis-cfg"));
        assert_eq!(detail.notes, "Redis is operational.");
    }

    #[test]
    fn parse_helm_release_dynamic_huffman_fixture() {
        // Real standard gzip dynamic Huffman encoded Helm release payload
        let fixture_b64 = "H4sIAAAAAAAC/32QQUvEMBCF/0rM2ZSmCx5y3ZMXERQPUpChma7BdhKStFiW/nc7Wbu4FyGXvHnz3secJcGI0gg5LopOjr7lvZCOer9pZ9m7mPKHxTD4BS3bmrp5ULpWB/2qa1Pze+eVAf4xNn+MKUOeEjuu7k0ln7GIT8wgXBIRwS53ct2G3SfEXHhGzGAhQ/ns5FfsGWNynljTVVMdWIMQ3m5kXdVyLameencqSXEjcR0c/UTc0zAmxtl1WMZ5CaXoOEwpY3x8vgSMQK7HxBtSKdUSBPfbZcSsW/pyZI14uSS1tMObloRgeCP2o7d0y6/5JJsjBSgMMkRvpy7zdP0B4slo/7QBAAA=";

        let detail = parse_helm_release_data(fixture_b64.as_bytes()).unwrap();
        assert_eq!(detail.summary.name, "my-nginx");
        assert_eq!(detail.summary.namespace, "production");
        assert_eq!(detail.summary.revision, 1);
        assert_eq!(detail.summary.status, "deployed");
        assert_eq!(detail.summary.chart, "nginx-1.2.3");
        assert_eq!(detail.summary.app_version, "1.21.0");
        assert_eq!(detail.summary.updated, "2026-10-31T12:00:00Z");
        assert_eq!(detail.notes, "Nginx is ready!");
        assert!(detail.values_yaml.contains("replicaCount: 2"));
        assert!(detail.manifest_yaml.contains("kind: Service"));
    }

    #[test]
    fn parse_helm_release_secret_helper() {
        let fixture_b64 = "H4sIAAAAAAAC/32QQUvEMBCF/0rM2ZSmCx5y3ZMXERQPUpChma7BdhKStFiW/nc7Wbu4FyGXvHnz3secJcGI0gg5LopOjr7lvZCOer9pZ9m7mPKHxTD4BS3bmrp5ULpWB/2qa1Pze+eVAf4xNn+MKUOeEjuu7k0ln7GIT8wgXBIRwS53ct2G3SfEXHhGzGAhQ/ns5FfsGWNynljTVVMdWIMQ3m5kXdVyLameencqSXEjcR0c/UTc0zAmxtl1WMZ5CaXoOEwpY3x8vgSMQK7HxBtSKdUSBPfbZcSsW/pyZI14uSS1tMObloRgeCP2o7d0y6/5JJsjBSgMMkRvpy7zdP0B4slo/7QBAAA=";

        let mut data = BTreeMap::new();
        data.insert(
            "release".to_owned(),
            k8s_openapi::ByteString(fixture_b64.as_bytes().to_vec()),
        );

        let secret = Secret {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("sh.helm.release.v1.my-nginx.v1".to_owned()),
                namespace: Some("production".to_owned()),
                ..Default::default()
            },
            data: Some(data),
            ..Default::default()
        };

        let summary = parse_helm_release_secret(&secret).unwrap();
        assert_eq!(summary.name, "my-nginx");
        assert_eq!(summary.namespace, "production");
        assert_eq!(summary.revision, 1);
        assert_eq!(summary.chart, "nginx-1.2.3");
    }

    #[test]
    fn parse_helm_release_malformed() {
        assert!(parse_helm_release_data(b"not-valid-base64-or-gzip!!!").is_err());
        assert!(parse_helm_release_data(&[0x1f, 0x8b, 0x00]).is_err());
    }

    #[test]
    fn decompress_gzip_rejects_corrupt_stream() {
        let mut gz = gzip_compress_uncompressed(b"{\"name\":\"x\"}");
        let last = gz.len() - 5;
        gz[last] ^= 0xFF; // corrupt CRC32
        assert!(decompress_gzip(&gz).is_err());
    }

    fn meta(name: &str, ns: &str, labels: &[(&str, &str)]) -> ObjectMeta {
        ObjectMeta {
            name: Some(name.to_owned()),
            namespace: Some(ns.to_owned()),
            labels: if labels.is_empty() {
                None
            } else {
                Some(
                    labels
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                )
            },
            ..Default::default()
        }
    }

    fn summary(ns: &str, name: &str, revision: u32) -> HelmReleaseSummary {
        HelmReleaseSummary {
            name: name.to_owned(),
            namespace: ns.to_owned(),
            revision,
            updated: String::new(),
            status: "deployed".to_owned(),
            chart: "c-1.0.0".to_owned(),
            app_version: "1".to_owned(),
        }
    }

    #[test]
    fn helm_revision_key_prefers_labels_and_falls_back_to_secret_name() {
        let labelled = meta(
            "sh.helm.release.v1.web.v7",
            "apps",
            &[("owner", "helm"), ("name", "web"), ("version", "7")],
        );
        let key = helm_revision_key(&labelled).unwrap();
        assert_eq!(key.release, "web");
        assert_eq!(key.revision, 7);
        assert_eq!(key.namespace, "apps");
        assert_eq!(key.secret_name, "sh.helm.release.v1.web.v7");

        // Release names may contain ".v"; the revision is the last segment.
        let unlabelled = meta("sh.helm.release.v1.my.very.app.v12", "apps", &[]);
        let key = helm_revision_key(&unlabelled).unwrap();
        assert_eq!(key.release, "my.very.app");
        assert_eq!(key.revision, 12);

        assert!(helm_revision_key(&meta("unrelated-secret", "apps", &[])).is_none());
        assert!(helm_revision_key(&meta("sh.helm.release.v1.x.vNaN", "apps", &[])).is_none());
    }

    #[test]
    fn latest_revisions_keeps_highest_per_namespace_and_name() {
        let key = |ns: &str, rel: &str, rev: u32| HelmRevisionKey {
            namespace: ns.to_owned(),
            release: rel.to_owned(),
            revision: rev,
            secret_name: helm_release_secret_name(rel, rev),
        };
        let latest = latest_revisions(vec![
            key("b", "web", 1),
            key("a", "web", 3),
            key("a", "web", 10),
            key("a", "web", 2),
            key("b", "web", 4),
            key("a", "db", 1),
        ]);
        let got: Vec<_> = latest
            .iter()
            .map(|k| (k.namespace.as_str(), k.release.as_str(), k.revision))
            .collect();
        assert_eq!(got, vec![("a", "db", 1), ("a", "web", 10), ("b", "web", 4)]);
        assert_eq!(latest[1].secret_name, "sh.helm.release.v1.web.v10");
    }

    #[test]
    fn latest_release_summaries_and_history_sorting() {
        let latest = latest_release_summaries(vec![
            summary("ns", "web", 2),
            summary("ns", "web", 5),
            summary("other", "web", 1),
        ]);
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].revision, 5);
        assert_eq!(latest[1].namespace, "other");

        let history = sort_history_desc(vec![
            summary("ns", "web", 2),
            summary("ns", "web", 9),
            summary("ns", "web", 4),
        ]);
        let revs: Vec<u32> = history.iter().map(|s| s.revision).collect();
        assert_eq!(revs, vec![9, 4, 2]);
    }

    #[test]
    fn window_helm_releases_is_bounded_and_clamped() {
        let all: Vec<_> = (0..2500)
            .map(|i| summary("ns", &format!("r{i:04}"), 1))
            .collect();

        let page = window_helm_releases(&all, 10, 5);
        assert_eq!(page.total, 2500);
        assert_eq!(page.offset, 10);
        assert_eq!(page.releases.len(), 5);
        assert_eq!(page.releases[0].name, "r0010");

        let clamped = window_helm_releases(&all, 0, 50_000);
        assert_eq!(clamped.releases.len(), MAX_HELM_PAGE_LIMIT as usize);

        let past_end = window_helm_releases(&all, 2498, 100);
        assert_eq!(past_end.releases.len(), 2);
        let beyond = window_helm_releases(&all, u32::MAX, u32::MAX);
        assert!(beyond.releases.is_empty());
        assert_eq!(beyond.total, 2500);
    }

    #[test]
    fn diff_helm_revisions_diffs_values_and_manifest() {
        let a = HelmReleaseDetail {
            summary: summary("ns", "web", 1),
            values_yaml: "replicaCount: 1\nimage: nginx\n".to_owned(),
            manifest_yaml: "kind: Service\nname: web\n".to_owned(),
            notes: String::new(),
        };
        let b = HelmReleaseDetail {
            summary: summary("ns", "web", 2),
            values_yaml: "replicaCount: 3\nimage: nginx\n".to_owned(),
            manifest_yaml: "kind: Service\nname: web\n".to_owned(),
            notes: String::new(),
        };
        let diff = diff_helm_revisions(&a, &b);
        assert_eq!(
            diff.values_diff,
            "-replicaCount: 1\n+replicaCount: 3\n image: nginx"
        );
        assert_eq!(diff.manifest_diff, " kind: Service\n name: web");
    }

    #[test]
    fn preview_helm_rollback_formats_command() {
        assert_eq!(
            preview_helm_rollback("payments", "checkout", 4),
            "helm rollback checkout 4 -n payments"
        );
        assert_eq!(
            helm_release_secret_name("checkout", 4),
            "sh.helm.release.v1.checkout.v4"
        );
    }
}
