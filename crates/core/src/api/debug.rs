use serde::{Deserialize, Serialize};

/// Request to add an ephemeral debug container to a pod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DebugPodRequest {
    pub context: String,
    pub namespace: String,
    pub pod: String,
    pub container_name: String,
    pub image: String,
    pub target_container: Option<String>,
    pub command: Vec<String>,
    pub confirmed_name: Option<String>,
}

/// Bounded result of adding an ephemeral debug container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DebugPodResult {
    pub container_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DebugOptions {
    pub image: String,
    pub target_container: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DebugPodAction {
    pub pod: String,
    pub namespace: String,
    pub options: DebugOptions,
}

/// Request to create a short-lived host-namespace debug shell on a node (`04-03d`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DebugNodeRequest {
    pub context: String,
    pub node: String,
    pub image: String,
    #[serde(default)]
    pub command: Vec<String>,
    pub confirmed_name: Option<String>,
    pub namespace: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// Result of creating a node debug pod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct DebugNodeResult {
    pub pod_name: String,
    pub namespace: String,
}
