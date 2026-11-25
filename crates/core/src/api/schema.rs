//! IPC types for Kubernetes resource schemas used by the YAML editor (`05-04`).

use serde::{Deserialize, Serialize};

/// Why a resource schema could not be provided. The editor then validates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum SchemaUnavailableReason {
    /// RBAC denies reading `/openapi/v3` (403/401).
    Forbidden,
    /// The API server does not publish the group-version or the kind.
    NotFound,
    /// The API server does not serve OpenAPI v3 (Kubernetes < 1.24 or disabled).
    Unsupported,
    /// The cluster could not be reached or the client could not be built.
    Unreachable,
    /// The API server answered with a document Fleetglass cannot use.
    BadResponse,
}

/// JSON schema of one resource kind, taken from the cluster's OpenAPI v3 document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ResourceSchemaResult {
    #[serde(rename_all = "camelCase")]
    Available {
        api_version: String,
        kind: String,
        /// Self-contained JSON schema (all `$ref`s resolved), serialized as JSON text
        /// (specta cannot export `serde_json::Value`). Nodes cut for cycles or the size
        /// cap carry `x-kubernetes-preserve-unknown-fields: true`. At most
        /// `MAX_SCHEMA_BYTES` (2 MiB).
        schema_json: String,
        /// `true` when part of the schema was cut by the node cap or its descriptions
        /// were dropped by the byte cap.
        truncated: bool,
    },
    #[serde(rename_all = "camelCase")]
    Unavailable {
        reason: SchemaUnavailableReason,
        /// User-readable explanation, e.g. "no permission to read /openapi/v3".
        message: String,
    },
}
