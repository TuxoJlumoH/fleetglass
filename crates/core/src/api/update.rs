//! Opt-in update check types (`P2-10`).

use serde::{Deserialize, Serialize};

/// A newer signed release announced by the configured update endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// Announced version, e.g. `0.5.0`.
    pub version: String,
    /// Release notes from the update manifest, if any.
    pub notes: Option<String>,
}
