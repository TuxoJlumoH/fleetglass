//! Custom JSONPath column types (`P2-08`).

use serde::{Deserialize, Serialize};

/// A user-defined table column evaluated with a kubectl custom-columns path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ColumnDef {
    pub id: String,
    pub title: String,
    /// kubectl custom-columns expression, e.g. `.metadata.annotations['a/b']`.
    pub json_path: String,
    pub width: Option<u32>,
}

/// Per-kind table layout: visible column ids, custom columns and widths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct ColumnLayout {
    pub kind: String,
    /// Column ids in display order (built-in and custom).
    pub visible: Vec<String>,
    pub custom: Vec<ColumnDef>,
    /// `(column id, width in px)` pairs.
    pub widths: Vec<(String, u32)>,
}
