//! Kubernetes resource schemas for the YAML editor (`05-04`).
//!
//! The schema of a kind comes from the API server's OpenAPI v3 document, so it covers
//! built-in kinds and CRDs alike and matches the cluster's version: `/openapi/v3` lists
//! the group-version documents, then `/openapi/v3/apis/<group>/<version>` (core group:
//! `/openapi/v3/api/v1`) holds `components.schemas`. The schema whose
//! `x-kubernetes-group-version-kind` matches is resolved into a self-contained,
//! size-bounded JSON schema.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use futures::AsyncReadExt;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::api::{ResourceSchemaResult, SchemaUnavailableReason};

/// Largest OpenAPI document read from the API server (core/v1 is about 1.6 MB).
pub const MAX_DOCUMENT_BYTES: u64 = 32 * 1024 * 1024;
/// Most schema nodes in one resolved schema (a Pod has about 1,800).
pub const MAX_SCHEMA_NODES: usize = 10_000;
/// Largest serialized schema sent to the UI (a Deployment is about 350 KB).
pub const MAX_SCHEMA_BYTES: usize = 2 * 1024 * 1024;
/// Deepest chain of `$ref`s followed before the node is cut.
pub const MAX_REF_DEPTH: usize = 32;
/// Longest description kept, in characters; longer ones are shortened.
pub const MAX_DESCRIPTION_CHARS: usize = 500;
/// Largest `default` value kept, in serialized bytes.
pub const MAX_DEFAULT_BYTES: usize = 256;
/// Most `enum` values kept; a longer enum is dropped (no completion, no check).
pub const MAX_ENUM_VALUES: usize = 200;
/// Group-version documents cached across all contexts.
pub const MAX_CACHED_GROUP_VERSIONS: usize = 32;

/// Schema keywords and Kubernetes extensions kept in a resolved schema.
const KEPT_KEYWORDS: &[&str] = &[
    "type",
    "format",
    "description",
    "default",
    "enum",
    "required",
    "nullable",
    "properties",
    "additionalProperties",
    "items",
    "allOf",
    "oneOf",
    "anyOf",
    "x-kubernetes-preserve-unknown-fields",
    "x-kubernetes-int-or-string",
    "x-kubernetes-embedded-resource",
];

const PRESERVE_UNKNOWN: &str = "x-kubernetes-preserve-unknown-fields";

/// Why fetching or resolving a schema failed.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("invalid apiVersion '{0}'")]
    InvalidApiVersion(String),
    #[error("invalid kind '{0}'")]
    InvalidKind(String),
    #[error("no permission to read the OpenAPI schema ({0})")]
    Forbidden(String),
    #[error("the API server does not serve OpenAPI v3")]
    Unsupported,
    #[error("the API server publishes no OpenAPI schema for {0}")]
    GroupVersionNotFound(String),
    #[error("no schema for kind {kind} in {api_version}")]
    KindNotFound { api_version: String, kind: String },
    #[error("not found on the API server ({0})")]
    HttpNotFound(String),
    #[error("OpenAPI document is larger than {0} bytes")]
    TooLarge(u64),
    #[error("schema for {0} is larger than {MAX_SCHEMA_BYTES} bytes")]
    SchemaTooLarge(String),
    #[error("unusable OpenAPI document: {0}")]
    BadDocument(String),
    #[error("cluster unreachable: {0}")]
    Unreachable(String),
}

impl SchemaError {
    /// IPC category of this error.
    #[must_use]
    pub fn reason(&self) -> SchemaUnavailableReason {
        match self {
            Self::Forbidden(_) => SchemaUnavailableReason::Forbidden,
            Self::Unsupported => SchemaUnavailableReason::Unsupported,
            Self::InvalidApiVersion(_)
            | Self::InvalidKind(_)
            | Self::GroupVersionNotFound(_)
            | Self::HttpNotFound(_)
            | Self::KindNotFound { .. } => SchemaUnavailableReason::NotFound,
            Self::TooLarge(_) | Self::SchemaTooLarge(_) | Self::BadDocument(_) => {
                SchemaUnavailableReason::BadResponse
            }
            Self::Unreachable(_) => SchemaUnavailableReason::Unreachable,
        }
    }

    fn from_kube(err: &kube::Error) -> Self {
        match err {
            kube::Error::Api(status) if status.code == 401 || status.code == 403 => {
                Self::Forbidden(crate::cluster::sanitize_kube_error(err))
            }
            kube::Error::Api(status) if status.code == 404 => {
                Self::HttpNotFound(crate::cluster::sanitize_kube_error(err))
            }
            kube::Error::Api(_) | kube::Error::SerdeError(_) | kube::Error::FromUtf8(_) => {
                Self::BadDocument(crate::cluster::sanitize_kube_error(err))
            }
            _ => Self::Unreachable(crate::cluster::sanitize_kube_error(err)),
        }
    }
}

impl From<SchemaError> for ResourceSchemaResult {
    fn from(err: SchemaError) -> Self {
        Self::Unavailable {
            reason: err.reason(),
            message: err.to_string(),
        }
    }
}

/// A resolved, self-contained schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSchema {
    pub schema: Value,
    /// `true` when the node cap cut part of the schema.
    pub truncated: bool,
}

/// Serializes `resolved` for IPC within `max_bytes`. Descriptions are dropped first;
/// returns `None` if it is still too large. The flag is `true` when anything was cut.
pub fn serialize_bounded(resolved: &ResolvedSchema, max_bytes: usize) -> Option<(String, bool)> {
    let text = serde_json::to_string(&resolved.schema).ok()?;
    if text.len() <= max_bytes {
        return Some((text, resolved.truncated));
    }
    let mut lean = resolved.schema.clone();
    strip_descriptions(&mut lean);
    let text = serde_json::to_string(&lean).ok()?;
    (text.len() <= max_bytes).then_some((text, true))
}

fn strip_descriptions(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            obj.remove("description");
            for (key, child) in obj.iter_mut() {
                // Property names are user data, not schema keywords.
                if key == "properties" {
                    if let Value::Object(props) = child {
                        props.values_mut().for_each(strip_descriptions);
                    }
                } else if key != "default" && key != "enum" {
                    strip_descriptions(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_descriptions),
        _ => {}
    }
}

/// `apiVersion` split into group and version; the core group is `""`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupVersion {
    pub group: String,
    pub version: String,
}

impl GroupVersion {
    /// Parses `v1` or `apps/v1`.
    pub fn parse(api_version: &str) -> Result<Self, SchemaError> {
        let invalid = || SchemaError::InvalidApiVersion(api_version.to_owned());
        let (group, version) = match api_version.split_once('/') {
            Some((g, v)) => (g, v),
            None => ("", api_version),
        };
        // DNS-subdomain shape: dot-separated, non-empty labels of [a-z0-9-].
        let valid_segment = |s: &str| {
            s.len() <= 253
                && s.split('.').all(|label| {
                    !label.is_empty()
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                })
        };
        if !valid_segment(version) || (!group.is_empty() && !valid_segment(group)) {
            return Err(invalid());
        }
        if api_version.contains('/') && group.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            group: group.to_owned(),
            version: version.to_owned(),
        })
    }

    /// Key of this group-version in the `/openapi/v3` index (`api/v1`, `apis/apps/v1`).
    #[must_use]
    pub fn index_key(&self) -> String {
        if self.group.is_empty() {
            format!("api/{}", self.version)
        } else {
            format!("apis/{}/{}", self.group, self.version)
        }
    }

    fn api_version(&self) -> String {
        if self.group.is_empty() {
            self.version.clone()
        } else {
            format!("{}/{}", self.group, self.version)
        }
    }
}

fn validate_kind(kind: &str) -> Result<(), SchemaError> {
    if kind.is_empty() || kind.len() > 253 || !kind.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(SchemaError::InvalidKind(kind.to_owned()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct OpenApiIndex {
    #[serde(default)]
    paths: HashMap<String, IndexEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexEntry {
    #[serde(rename = "serverRelativeURL")]
    server_relative_url: String,
}

#[derive(Deserialize)]
struct OpenApiDocument {
    #[serde(default)]
    components: Option<Components>,
}

#[derive(Deserialize)]
struct Components {
    #[serde(default)]
    schemas: Map<String, Value>,
}

/// Parses a group-version document and keeps only `components.schemas`.
pub fn parse_document(body: &[u8]) -> Result<Map<String, Value>, SchemaError> {
    let doc: OpenApiDocument =
        serde_json::from_slice(body).map_err(|e| SchemaError::BadDocument(e.to_string()))?;
    Ok(doc.components.map(|c| c.schemas).unwrap_or_default())
}

/// Name of the schema in `schemas` whose `x-kubernetes-group-version-kind` is `gv`/`kind`.
pub fn find_gvk<'a>(
    schemas: &'a Map<String, Value>,
    gv: &GroupVersion,
    kind: &str,
) -> Option<&'a str> {
    // Prefer a schema that declares a single GVK: shared types such as DeleteOptions
    // list one entry per group-version.
    let mut fallback = None;
    for (name, schema) in schemas {
        let Some(gvks) = schema
            .get("x-kubernetes-group-version-kind")
            .and_then(Value::as_array)
        else {
            continue;
        };
        let matches = gvks.iter().any(|g| {
            g.get("group").and_then(Value::as_str).unwrap_or("") == gv.group
                && g.get("version").and_then(Value::as_str) == Some(gv.version.as_str())
                && g.get("kind").and_then(Value::as_str) == Some(kind)
        });
        if matches {
            if gvks.len() == 1 {
                return Some(name.as_str());
            }
            fallback.get_or_insert(name.as_str());
        }
    }
    fallback
}

/// Resolves `schemas[root]` into a self-contained schema (see the module docs).
pub fn resolve_schema(schemas: &Map<String, Value>, root: &str) -> Option<ResolvedSchema> {
    let schema = schemas.get(root)?;
    let mut resolver = Resolver {
        schemas,
        stack: vec![root],
        nodes: 0,
        truncated: false,
    };
    let schema = resolver.node(schema, 0);
    Some(ResolvedSchema {
        schema,
        truncated: resolver.truncated,
    })
}

struct Resolver<'a> {
    schemas: &'a Map<String, Value>,
    /// `$ref` names being expanded, outermost first (cycle detection).
    stack: Vec<&'a str>,
    nodes: usize,
    truncated: bool,
}

fn permissive(description: Option<&Value>) -> Value {
    let mut node = Map::new();
    if let Some(d) = description {
        node.insert("description".to_owned(), d.clone());
    }
    node.insert(PRESERVE_UNKNOWN.to_owned(), Value::Bool(true));
    Value::Object(node)
}

fn ref_name(reference: &str) -> Option<&str> {
    reference.strip_prefix("#/components/schemas/")
}

impl<'a> Resolver<'a> {
    fn node(&mut self, value: &'a Value, depth: usize) -> Value {
        let Some(obj) = value.as_object() else {
            // `true`/`false` schemas and malformed nodes accept anything.
            return permissive(None);
        };
        if self.nodes >= MAX_SCHEMA_NODES {
            self.truncated = true;
            return permissive(None);
        }
        self.nodes += 1;

        // `{"$ref": X}` and the Kubernetes wrapper `{"allOf": [{"$ref": X}], "description": ..}`
        // are inlined; sibling keywords (description, default) override the target's.
        let single_all_of = obj
            .get("allOf")
            .and_then(Value::as_array)
            .filter(|a| a.len() == 1)
            .and_then(|a| a.first());
        let inline_target = obj.get("$ref").map(|r| (r, true)).or_else(|| {
            single_all_of
                .and_then(|inner| inner.get("$ref"))
                .map(|r| (r, false))
        });

        if let Some((reference, is_direct)) = inline_target {
            let target = reference.as_str().and_then(ref_name);
            let mut base = match target {
                Some(name) if self.stack.contains(&name) || depth >= MAX_REF_DEPTH => {
                    permissive(None)
                }
                Some(name) => match self.schemas.get_key_value(name) {
                    Some((key, schema)) => {
                        self.stack.push(key.as_str());
                        let resolved = self.node(schema, depth + 1);
                        self.stack.pop();
                        resolved
                    }
                    None => permissive(None),
                },
                None => permissive(None),
            };
            if let Value::Object(base_obj) = &mut base {
                for (key, val) in obj {
                    if key == "$ref" || (!is_direct && key == "allOf") {
                        continue;
                    }
                    if let Some(kept) = self.keyword(key, val, depth) {
                        base_obj.insert(key.clone(), kept);
                    }
                }
            }
            return base;
        }

        let mut out = Map::new();
        for (key, val) in obj {
            if let Some(kept) = self.keyword(key, val, depth) {
                out.insert(key.clone(), kept);
            }
        }
        Value::Object(out)
    }

    fn keyword(&mut self, key: &str, val: &'a Value, depth: usize) -> Option<Value> {
        if !KEPT_KEYWORDS.contains(&key) {
            return None;
        }
        match key {
            "properties" => {
                let props = val.as_object()?;
                let mut out = Map::new();
                for (name, prop) in props {
                    out.insert(name.clone(), self.node(prop, depth + 1));
                }
                Some(Value::Object(out))
            }
            "items" => Some(self.node(val, depth + 1)),
            "additionalProperties" => match val {
                Value::Bool(b) => Some(Value::Bool(*b)),
                other => Some(self.node(other, depth + 1)),
            },
            "allOf" | "oneOf" | "anyOf" => {
                let branches = val.as_array()?;
                Some(Value::Array(
                    branches.iter().map(|b| self.node(b, depth + 1)).collect(),
                ))
            }
            "description" => {
                let text = val.as_str()?;
                Some(Value::String(shorten(text, MAX_DESCRIPTION_CHARS)))
            }
            "default" => {
                let size = serde_json::to_string(val).map(|s| s.len()).ok()?;
                (size <= MAX_DEFAULT_BYTES).then(|| val.clone())
            }
            "enum" => {
                let values = val.as_array()?;
                (values.len() <= MAX_ENUM_VALUES).then(|| val.clone())
            }
            _ => Some(val.clone()),
        }
    }
}

fn shorten(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}…", text[..idx].trim_end()),
        None => text.to_owned(),
    }
}

struct CachedGroupVersion {
    /// `serverRelativeURL` from the index; it carries the document hash, so a change
    /// (new CRD version, upgrade) invalidates the entry.
    url: String,
    schemas: Arc<Map<String, Value>>,
    last_used: u64,
}

/// Cache of parsed group-version documents, keyed by (context, group-version).
#[derive(Default)]
pub struct SchemaCache {
    inner: Mutex<CacheInner>,
}

#[derive(Default)]
struct CacheInner {
    entries: HashMap<(String, String), CachedGroupVersion>,
    tick: u64,
}

impl SchemaCache {
    fn get(&self, context: &str, gv_key: &str, url: &str) -> Option<Arc<Map<String, Value>>> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.tick += 1;
        let tick = inner.tick;
        let entry = inner
            .entries
            .get_mut(&(context.to_owned(), gv_key.to_owned()))?;
        if entry.url != url {
            return None;
        }
        entry.last_used = tick;
        Some(Arc::clone(&entry.schemas))
    }

    fn insert(&self, context: &str, gv_key: &str, url: String, schemas: Arc<Map<String, Value>>) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.tick += 1;
        let tick = inner.tick;
        let key = (context.to_owned(), gv_key.to_owned());
        if !inner.entries.contains_key(&key) && inner.entries.len() >= MAX_CACHED_GROUP_VERSIONS {
            let oldest = inner
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                inner.entries.remove(&oldest);
            }
        }
        inner.entries.insert(
            key,
            CachedGroupVersion {
                url,
                schemas,
                last_used: tick,
            },
        );
    }

    /// Drops every cached document of `context` (reconnect, kubeconfig change).
    pub fn invalidate_context(&self, context: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.entries.retain(|(ctx, _), _| ctx != context);
    }

    /// Number of cached group-version documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }

    /// `true` when nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// GETs an API server path (with optional query string), reading at most
/// [`MAX_DOCUMENT_BYTES`].
async fn get_bounded(client: &kube::Client, path: &str) -> Result<Vec<u8>, SchemaError> {
    // `Request::get` appends `/{leaf}` verbatim, which keeps a pre-encoded query string.
    let (dir, leaf) = path.rsplit_once('/').unwrap_or(("", path));
    let req = kube::core::Request::new(dir)
        .get(leaf, &kube::api::GetParams::default())
        .map_err(|e| SchemaError::BadDocument(e.to_string()))?;
    let stream = client
        .request_stream(req)
        .await
        .map_err(|e| SchemaError::from_kube(&e))?;
    let mut body = Vec::new();
    stream
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut body)
        .await
        .map_err(|e| SchemaError::Unreachable(e.to_string()))?;
    if body.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(SchemaError::TooLarge(MAX_DOCUMENT_BYTES));
    }
    Ok(body)
}

/// Fetches and resolves the schema of `api_version`/`kind` from the cluster behind `client`.
pub async fn fetch_resource_schema(
    client: &kube::Client,
    cache: &SchemaCache,
    context: &str,
    api_version: &str,
    kind: &str,
) -> Result<ResolvedSchema, SchemaError> {
    let gv = GroupVersion::parse(api_version)?;
    validate_kind(kind)?;

    let index_body = match get_bounded(client, "/openapi/v3").await {
        Err(SchemaError::HttpNotFound(_)) => {
            return Err(SchemaError::Unsupported);
        }
        other => other?,
    };
    let index: OpenApiIndex =
        serde_json::from_slice(&index_body).map_err(|e| SchemaError::BadDocument(e.to_string()))?;
    let gv_key = gv.index_key();
    let url = index
        .paths
        .get(&gv_key)
        .map(|e| e.server_relative_url.clone())
        .ok_or_else(|| SchemaError::GroupVersionNotFound(gv.api_version()))?;
    if !url.starts_with("/openapi/v3/") || url.contains("..") {
        return Err(SchemaError::BadDocument(format!(
            "unexpected document URL for {}",
            gv.api_version()
        )));
    }

    let schemas = match cache.get(context, &gv_key, &url) {
        Some(schemas) => schemas,
        None => {
            let body = get_bounded(client, &url).await?;
            let schemas = Arc::new(parse_document(&body)?);
            cache.insert(context, &gv_key, url, Arc::clone(&schemas));
            schemas
        }
    };

    let not_found = || SchemaError::KindNotFound {
        api_version: gv.api_version(),
        kind: kind.to_owned(),
    };
    let name = find_gvk(&schemas, &gv, kind).ok_or_else(not_found)?;
    resolve_schema(&schemas, name).ok_or_else(not_found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Map<String, Value> {
        let body = std::fs::read(crate::test_fixture("openapi-v3-apps-v1.json")).unwrap();
        parse_document(&body).unwrap()
    }

    fn apps_v1() -> GroupVersion {
        GroupVersion::parse("apps/v1").unwrap()
    }

    #[test]
    fn parses_api_versions_and_index_keys() {
        let core = GroupVersion::parse("v1").unwrap();
        assert_eq!(core.group, "");
        assert_eq!(core.index_key(), "api/v1");
        assert_eq!(apps_v1().index_key(), "apis/apps/v1");
        assert_eq!(
            GroupVersion::parse("stable.example.com/v1beta1")
                .unwrap()
                .index_key(),
            "apis/stable.example.com/v1beta1"
        );
        for bad in [
            "",
            "/v1",
            "apps/",
            "a/b/c",
            "Apps/v1",
            "../v1",
            "apps/v1?x=1",
        ] {
            assert!(GroupVersion::parse(bad).is_err(), "{bad} must be rejected");
        }
        assert!(validate_kind("Deployment").is_ok());
        assert!(validate_kind("Deploy/ment").is_err());
        assert!(validate_kind("").is_err());
    }

    #[test]
    fn finds_schema_by_group_version_kind() {
        let schemas = fixture();
        assert_eq!(
            find_gvk(&schemas, &apps_v1(), "Deployment"),
            Some("io.k8s.api.apps.v1.Deployment")
        );
        // DeleteOptions lists many GVKs; a single-GVK schema would win, but it is the only one.
        assert_eq!(
            find_gvk(&schemas, &apps_v1(), "DeleteOptions"),
            Some("io.k8s.apimachinery.pkg.apis.meta.v1.DeleteOptions")
        );
        assert_eq!(find_gvk(&schemas, &apps_v1(), "StatefulSet"), None);
        let v2 = GroupVersion::parse("apps/v2").unwrap();
        assert_eq!(find_gvk(&schemas, &v2, "Deployment"), None);
    }

    #[test]
    fn resolves_refs_into_a_self_contained_schema() {
        let schemas = fixture();
        let resolved = resolve_schema(&schemas, "io.k8s.api.apps.v1.Deployment").unwrap();
        assert!(!resolved.truncated);
        let text = serde_json::to_string(&resolved.schema).unwrap();
        assert!(!text.contains("$ref"), "unresolved $ref left: {text}");
        assert!(!text.contains("x-kubernetes-group-version-kind"));
        assert!(!text.contains("x-kubernetes-patch-strategy"));

        let s = &resolved.schema;
        assert_eq!(s["required"], json!(["spec"]));
        let spec = &s["properties"]["spec"];
        // The `allOf: [$ref]` wrapper is inlined and keeps the sibling description.
        assert_eq!(
            spec["description"],
            "Specification of the desired behavior of the Deployment."
        );
        assert_eq!(spec["type"], "object");
        assert_eq!(spec["properties"]["replicas"]["type"], "integer");
        assert_eq!(spec["required"], json!(["selector", "template"]));
        let container = &spec["properties"]["template"]["properties"]["spec"]["properties"]["containers"]
            ["items"];
        assert_eq!(container["required"], json!(["name"]));
        assert_eq!(
            container["properties"]["imagePullPolicy"]["enum"],
            json!(["Always", "IfNotPresent", "Never"])
        );
        let port = &container["properties"]["ports"]["items"]["properties"]["containerPort"];
        assert_eq!(port["format"], "int32");
        // IntOrString keeps its oneOf and format.
        let max_surge = &spec["properties"]["strategy"]["properties"]["rollingUpdate"]["properties"]
            ["maxSurge"];
        assert_eq!(max_surge["format"], "int-or-string");
        assert_eq!(
            max_surge["oneOf"],
            json!([{ "type": "integer" }, { "type": "string" }])
        );
        // Free-form maps keep additionalProperties.
        assert_eq!(
            s["properties"]["metadata"]["properties"]["labels"]["additionalProperties"]["type"],
            "string"
        );
        // Status is resolved too (the UI decides not to lint it).
        assert_eq!(
            s["properties"]["status"]["properties"]["replicas"]["type"],
            "integer"
        );
    }

    #[test]
    fn keeps_kubernetes_extensions_of_custom_resources() {
        let schemas = fixture();
        let gv = GroupVersion::parse("example.fleetglass.dev/v1").unwrap();
        let name = find_gvk(&schemas, &gv, "Widget").unwrap();
        let s = resolve_schema(&schemas, name).unwrap().schema;
        let spec = &s["properties"]["spec"]["properties"];
        assert_eq!(spec["size"]["x-kubernetes-int-or-string"], true);
        assert_eq!(spec["config"][PRESERVE_UNKNOWN], true);
        assert_eq!(spec["template"]["x-kubernetes-embedded-resource"], true);
        assert_eq!(spec["mode"]["enum"], json!(["fast", "safe"]));
        assert_eq!(spec["mode"]["default"], "safe");
        assert_eq!(s["properties"]["spec"]["required"], json!(["size"]));
    }

    #[test]
    fn cuts_reference_cycles_with_a_permissive_node() {
        let schemas = fixture();
        let resolved = resolve_schema(&schemas, "io.example.v1.JSONSchemaProps").unwrap();
        let s = &resolved.schema;
        assert_eq!(s["properties"]["type"]["type"], "string");
        // Self-reference: cut at the first revisit and marked permissive.
        let nested = &s["properties"]["properties"]["additionalProperties"];
        assert_eq!(nested[PRESERVE_UNKNOWN], true);
        assert!(nested.get("properties").is_none());
        let not = &s["properties"]["not"];
        assert_eq!(not[PRESERVE_UNKNOWN], true);
        assert_eq!(not["description"], "Inverted schema.");
        let items = &s["properties"]["items"]["items"];
        assert_eq!(items[PRESERVE_UNKNOWN], true);
    }

    #[test]
    fn cuts_deep_reference_chains_at_the_depth_limit() {
        let mut schemas = Map::new();
        for i in 0..(MAX_REF_DEPTH + 10) {
            schemas.insert(
                format!("n{i}"),
                json!({
                    "type": "object",
                    "properties": { "next": { "$ref": format!("#/components/schemas/n{}", i + 1) } }
                }),
            );
        }
        let resolved = resolve_schema(&schemas, "n0").unwrap();
        let mut node = &resolved.schema;
        let mut levels = 0;
        while let Some(next) = node.get("properties").and_then(|p| p.get("next")) {
            node = next;
            levels += 1;
        }
        assert!(levels <= MAX_REF_DEPTH + 1, "followed {levels} levels");
        assert_eq!(node[PRESERVE_UNKNOWN], true);
    }

    #[test]
    fn caps_nodes_and_description_length() {
        let mut props = Map::new();
        for i in 0..(MAX_SCHEMA_NODES + 100) {
            props.insert(format!("f{i}"), json!({ "type": "string" }));
        }
        let long = "x".repeat(MAX_DESCRIPTION_CHARS * 3);
        let mut schemas = Map::new();
        schemas.insert(
            "Big".to_owned(),
            json!({ "type": "object", "description": long, "properties": props }),
        );
        let resolved = resolve_schema(&schemas, "Big").unwrap();
        assert!(resolved.truncated);
        let out_props = resolved.schema["properties"].as_object().unwrap();
        let typed = out_props
            .values()
            .filter(|p| p.get("type").is_some())
            .count();
        assert!(typed < MAX_SCHEMA_NODES, "{typed} nodes kept");
        assert!(out_props.values().any(|p| p[PRESERVE_UNKNOWN] == true));
        let desc = resolved.schema["description"].as_str().unwrap();
        assert_eq!(desc.chars().count(), MAX_DESCRIPTION_CHARS + 1);
        assert!(desc.ends_with('…'));

        // Byte cap: descriptions go first, then the schema is refused.
        let (_, cut) = serialize_bounded(&resolved, usize::MAX).unwrap();
        assert!(cut);
        let fixture_resolved = resolve_schema(&fixture(), "io.k8s.api.apps.v1.Deployment").unwrap();
        let (full, cut) = serialize_bounded(&fixture_resolved, usize::MAX).unwrap();
        assert!(!cut);
        let (lean, cut) = serialize_bounded(&fixture_resolved, full.len() - 1).unwrap();
        assert!(cut);
        assert!(!lean.contains("\"description\""));
        assert!(lean.contains("\"replicas\""));
        assert!(serialize_bounded(&fixture_resolved, 100).is_none());

        let fixture_schema = resolve_schema(&fixture(), "io.k8s.api.apps.v1.Deployment").unwrap();
        let bytes = serde_json::to_vec(&fixture_schema.schema).unwrap().len();
        assert!(bytes < 64 * 1024, "fixture schema is {bytes} bytes");
    }

    #[test]
    fn drops_large_defaults_and_enums_and_unknown_refs_are_permissive() {
        let mut schemas = Map::new();
        let big_enum: Vec<Value> = (0..=MAX_ENUM_VALUES).map(|i| json!(i)).collect();
        schemas.insert(
            "T".to_owned(),
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "string", "default": "x".repeat(MAX_DEFAULT_BYTES) },
                    "b": { "type": "integer", "enum": big_enum },
                    "c": { "$ref": "#/components/schemas/Missing" },
                    "d": { "$ref": "https://example.com/elsewhere" },
                    "e": { "type": "string", "default": "ok", "readOnly": true }
                }
            }),
        );
        let s = resolve_schema(&schemas, "T").unwrap().schema;
        let p = &s["properties"];
        assert!(p["a"].get("default").is_none());
        assert!(p["b"].get("enum").is_none());
        assert_eq!(p["c"][PRESERVE_UNKNOWN], true);
        assert_eq!(p["d"][PRESERVE_UNKNOWN], true);
        assert_eq!(p["e"], json!({ "type": "string", "default": "ok" }));
        assert!(resolve_schema(&schemas, "Nope").is_none());
    }

    #[test]
    fn cache_reuses_by_url_evicts_oldest_and_invalidates_per_context() {
        let cache = SchemaCache::default();
        let doc = Arc::new(Map::new());
        cache.insert("a", "apis/apps/v1", "/u?hash=1".into(), Arc::clone(&doc));
        assert!(cache.get("a", "apis/apps/v1", "/u?hash=1").is_some());
        assert!(cache.get("a", "apis/apps/v1", "/u?hash=2").is_none());
        assert!(cache.get("b", "apis/apps/v1", "/u?hash=1").is_none());

        for i in 0..MAX_CACHED_GROUP_VERSIONS {
            cache.insert("b", &format!("gv{i}"), "/u".into(), Arc::clone(&doc));
        }
        assert_eq!(cache.len(), MAX_CACHED_GROUP_VERSIONS);
        // The entry of "a" was the least recently used and is gone.
        assert!(cache.get("a", "apis/apps/v1", "/u?hash=1").is_none());

        cache.invalidate_context("b");
        assert!(cache.is_empty());
    }

    #[test]
    fn maps_errors_to_unavailable_reasons() {
        let forbidden = kube::Error::Api(Box::new(kube::core::Status {
            code: 403,
            message: "forbidden".into(),
            ..Default::default()
        }));
        assert_eq!(
            SchemaError::from_kube(&forbidden).reason(),
            SchemaUnavailableReason::Forbidden
        );
        let not_found = kube::Error::Api(Box::new(kube::core::Status {
            code: 404,
            ..Default::default()
        }));
        assert_eq!(
            SchemaError::from_kube(&not_found).reason(),
            SchemaUnavailableReason::NotFound
        );
        let unavailable = kube::Error::Api(Box::new(kube::core::Status {
            code: 503,
            ..Default::default()
        }));
        assert_eq!(
            SchemaError::from_kube(&unavailable).reason(),
            SchemaUnavailableReason::BadResponse
        );
        let result: ResourceSchemaResult = SchemaError::KindNotFound {
            api_version: "apps/v1".into(),
            kind: "Nope".into(),
        }
        .into();
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({
                "state": "unavailable",
                "reason": "notFound",
                "message": "no schema for kind Nope in apps/v1"
            })
        );
    }

    #[test]
    fn parse_document_rejects_garbage_and_ignores_paths() {
        assert!(matches!(
            parse_document(b"not json"),
            Err(SchemaError::BadDocument(_))
        ));
        let empty = parse_document(br#"{"paths": {"/x": {}}}"#).unwrap();
        assert!(empty.is_empty());
    }
}
