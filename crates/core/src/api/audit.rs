use serde::{Deserialize, Serialize};

/// A single entry in the local action audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: u32,
    pub timestamp: String,
    pub cluster: String,
    pub user: Option<String>,
    pub action: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub kubectl: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Bounded window of audit log entries returned by an audit log query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct AuditLogPage {
    pub total: u32,
    pub offset: u32,
    pub entries: Vec<AuditEntry>,
}
