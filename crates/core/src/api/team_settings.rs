//! IPC types for Git-shared team settings (P3-02).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ClusterGroup, ContextSettings};

/// Root structure of a shared `.fleetglass.yaml` or `team-settings.yaml` file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct TeamSettingsFile {
    pub version: u32,
    #[serde(default)]
    pub groups: Vec<ClusterGroup>,
    #[serde(default)]
    pub contexts: BTreeMap<String, ContextSettings>,
}

/// Summary returned after reloading team settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct TeamSettingsSummary {
    pub loaded_path: String,
    pub applied_groups: u32,
    pub applied_contexts: u32,
    pub warnings: Vec<String>,
}
