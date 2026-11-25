//! Git-shared team settings parser and merge engine (P3-02).

use std::path::{Path, PathBuf};

use crate::api::{ContextSettings, TeamSettingsFile, TeamSettingsSummary};
use crate::settings::SettingsStore;

/// Errors produced while parsing or merging team settings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Team settings input is malformed or invalid.
    #[error("invalid team settings input: {0}")]
    InvalidInput(String),

    /// Team settings file could not be read from disk.
    #[error("failed to read team settings file {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Core settings error while persisting settings.
    #[error(transparent)]
    Core(#[from] crate::error::Error),
}

/// Result alias for team settings operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
struct YamlLine<'a> {
    indent: usize,
    content: &'a str,
    raw: &'a str,
}

fn strip_yaml_comment(s: &str) -> Result<&str, Error> {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                return Ok(s[..i].trim_end());
            }
            _ => {}
        }
    }
    if in_single || in_double {
        return Err(Error::InvalidInput("unclosed quote in YAML string".into()));
    }
    Ok(s)
}

fn split_key_value(s: &str) -> Option<(&str, &str)> {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b':' if !in_single && !in_double => {
                if i + 1 == s.len() {
                    return Some((s[..i].trim(), ""));
                } else if bytes[i + 1].is_ascii_whitespace() {
                    return Some((s[..i].trim(), s[i + 1..].trim()));
                }
            }
            _ => {}
        }
    }
    None
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn split_flow_items(s: &str) -> Result<Vec<&str>, Error> {
    let mut items = Vec::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut depth_bracket = 0;
    let mut depth_brace = 0;
    let mut start = 0;
    let bytes = s.as_bytes();

    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'[' if !in_single && !in_double => depth_bracket += 1,
            b']' if !in_single && !in_double => {
                if depth_bracket == 0 {
                    return Err(Error::InvalidInput("unexpected ']' in flow item".into()));
                }
                depth_bracket -= 1;
            }
            b'{' if !in_single && !in_double => depth_brace += 1,
            b'}' if !in_single && !in_double => {
                if depth_brace == 0 {
                    return Err(Error::InvalidInput("unexpected '}' in flow item".into()));
                }
                depth_brace -= 1;
            }
            b',' if !in_single && !in_double && depth_bracket == 0 && depth_brace == 0 => {
                items.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if in_single || in_double || depth_bracket != 0 || depth_brace != 0 {
        return Err(Error::InvalidInput(
            "malformed flow collection syntax".into(),
        ));
    }
    let rest = s[start..].trim();
    if !rest.is_empty() {
        items.push(rest);
    }
    Ok(items)
}

fn parse_scalar(s: &str) -> Result<serde_json::Value, Error> {
    let s = s.trim();
    if s == "true" {
        return Ok(serde_json::Value::Bool(true));
    }
    if s == "false" {
        return Ok(serde_json::Value::Bool(false));
    }
    if s == "null" || s == "~" {
        return Ok(serde_json::Value::Null);
    }
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        return Ok(serde_json::Value::String(s[1..s.len() - 1].to_string()));
    }
    if s.starts_with('[') {
        if !s.ends_with(']') {
            return Err(Error::InvalidInput("unclosed flow list in YAML".into()));
        }
        let inner = s[1..s.len() - 1].trim();
        if inner.is_empty() {
            return Ok(serde_json::Value::Array(Vec::new()));
        }
        let mut items = Vec::new();
        for item in split_flow_items(inner)? {
            items.push(parse_scalar(item)?);
        }
        return Ok(serde_json::Value::Array(items));
    }
    if s.starts_with('{') {
        if !s.ends_with('}') {
            return Err(Error::InvalidInput("unclosed flow mapping in YAML".into()));
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
            return Ok(v);
        }
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok(serde_json::Value::Number(n.into()));
    }
    if let Ok(n) = s.parse::<u64>() {
        return Ok(serde_json::Value::Number(n.into()));
    }
    if let Ok(f) = s.parse::<f64>()
        && let Some(num) = serde_json::Number::from_f64(f)
    {
        return Ok(serde_json::Value::Number(num));
    }
    Ok(serde_json::Value::String(s.to_string()))
}

fn parse_yaml_block(
    lines: &[YamlLine<'_>],
    mut idx: usize,
    current_indent: usize,
) -> Result<(serde_json::Value, usize), Error> {
    if idx >= lines.len() {
        return Ok((serde_json::Value::Null, idx));
    }

    while idx < lines.len() && lines[idx].content.is_empty() {
        idx += 1;
    }
    if idx >= lines.len() || lines[idx].indent < current_indent {
        return Ok((serde_json::Value::Null, idx));
    }

    let is_array = lines[idx].content.starts_with("- ") || lines[idx].content == "-";
    if is_array {
        let mut arr = Vec::new();
        while idx < lines.len() {
            if lines[idx].content.is_empty() {
                idx += 1;
                continue;
            }
            if lines[idx].indent < current_indent {
                break;
            }
            if lines[idx].indent != current_indent
                || (!lines[idx].content.starts_with("- ") && lines[idx].content != "-")
            {
                break;
            }

            let item_text = if lines[idx].content == "-" {
                ""
            } else {
                lines[idx].content[2..].trim()
            };

            if item_text.is_empty() {
                let next_idx = idx + 1;
                if next_idx < lines.len() && lines[next_idx].indent > current_indent {
                    let child_indent = lines[next_idx].indent;
                    let (child_val, end_idx) = parse_yaml_block(lines, next_idx, child_indent)?;
                    arr.push(child_val);
                    idx = end_idx;
                } else {
                    arr.push(serde_json::Value::Null);
                    idx += 1;
                }
            } else if let Some((k, v_str)) = split_key_value(item_text) {
                let key_str = unquote(k).trim().to_string();
                if key_str.is_empty() {
                    return Err(Error::InvalidInput(format!(
                        "empty key in line '{}'",
                        lines[idx].raw
                    )));
                }

                let mut map = serde_json::Map::new();
                if v_str.is_empty() {
                    let next_idx = idx + 1;
                    if next_idx < lines.len() && lines[next_idx].indent > current_indent {
                        let child_indent = lines[next_idx].indent;
                        let (child_val, end_idx) = parse_yaml_block(lines, next_idx, child_indent)?;
                        map.insert(key_str, child_val);
                        idx = end_idx;
                    } else {
                        map.insert(key_str, serde_json::Value::Null);
                        idx += 1;
                    }
                } else {
                    map.insert(key_str, parse_scalar(v_str)?);
                    idx += 1;
                }

                while idx < lines.len() {
                    if lines[idx].content.is_empty() {
                        idx += 1;
                        continue;
                    }
                    if lines[idx].indent <= current_indent {
                        break;
                    }
                    if let Some((sub_k, sub_v)) = split_key_value(lines[idx].content) {
                        let sub_k_str = unquote(sub_k).trim().to_string();
                        if sub_k_str.is_empty() {
                            return Err(Error::InvalidInput(format!(
                                "empty key in line '{}'",
                                lines[idx].raw
                            )));
                        }
                        if sub_v.is_empty() {
                            let next_idx = idx + 1;
                            if next_idx < lines.len() && lines[next_idx].indent > lines[idx].indent
                            {
                                let (child_val, end_idx) =
                                    parse_yaml_block(lines, next_idx, lines[next_idx].indent)?;
                                map.insert(sub_k_str, child_val);
                                idx = end_idx;
                            } else {
                                map.insert(sub_k_str, serde_json::Value::Null);
                                idx += 1;
                            }
                        } else {
                            map.insert(sub_k_str, parse_scalar(sub_v)?);
                            idx += 1;
                        }
                    } else {
                        return Err(Error::InvalidInput(format!(
                            "expected key-value mapping in array item on line '{}'",
                            lines[idx].raw
                        )));
                    }
                }

                arr.push(serde_json::Value::Object(map));
            } else {
                arr.push(parse_scalar(item_text)?);
                idx += 1;
            }
        }
        Ok((serde_json::Value::Array(arr), idx))
    } else {
        let mut map = serde_json::Map::new();
        while idx < lines.len() {
            if lines[idx].content.is_empty() {
                idx += 1;
                continue;
            }
            if lines[idx].indent < current_indent {
                break;
            }
            if lines[idx].indent > current_indent {
                return Err(Error::InvalidInput(format!(
                    "unexpected indentation on line '{}'",
                    lines[idx].raw
                )));
            }

            if let Some((k, v_str)) = split_key_value(lines[idx].content) {
                let key_str = unquote(k).trim().to_string();
                if key_str.is_empty() {
                    return Err(Error::InvalidInput(format!(
                        "empty key in line '{}'",
                        lines[idx].raw
                    )));
                }

                if v_str.is_empty() {
                    let next_idx = idx + 1;
                    if next_idx < lines.len() && lines[next_idx].indent > current_indent {
                        let child_indent = lines[next_idx].indent;
                        let (child_val, end_idx) = parse_yaml_block(lines, next_idx, child_indent)?;
                        map.insert(key_str, child_val);
                        idx = end_idx;
                    } else {
                        map.insert(key_str, serde_json::Value::Null);
                        idx += 1;
                    }
                } else {
                    map.insert(key_str, parse_scalar(v_str)?);
                    idx += 1;
                }
            } else {
                return Err(Error::InvalidInput(format!(
                    "expected 'key: value' on line '{}'",
                    lines[idx].raw
                )));
            }
        }
        Ok((serde_json::Value::Object(map), idx))
    }
}

fn parse_yaml(input: &str) -> Result<serde_json::Value, Error> {
    let mut lines = Vec::new();
    for raw in input.lines() {
        let leading_spaces = raw.chars().take_while(|c| *c == ' ').count();
        if raw[leading_spaces..].starts_with('\t') {
            return Err(Error::InvalidInput(
                "tabs are not allowed for indentation in YAML".into(),
            ));
        }
        let stripped = strip_yaml_comment(raw.trim_start())?;
        if stripped == "---" || stripped == "..." {
            continue;
        }
        lines.push(YamlLine {
            indent: leading_spaces,
            content: stripped,
            raw,
        });
    }

    while let Some(last) = lines.last() {
        if last.content.is_empty() {
            lines.pop();
        } else {
            break;
        }
    }

    if lines.is_empty() {
        return Err(Error::InvalidInput(
            "empty team settings YAML document".into(),
        ));
    }

    let first_non_empty = match lines.iter().position(|l| !l.content.is_empty()) {
        Some(pos) => pos,
        None => {
            return Err(Error::InvalidInput(
                "empty team settings YAML document".into(),
            ));
        }
    };

    let start_indent = lines[first_non_empty].indent;
    if start_indent != 0 {
        return Err(Error::InvalidInput(format!(
            "invalid root indentation: expected 0, found {start_indent}"
        )));
    }

    let (val, _) = parse_yaml_block(&lines, first_non_empty, start_indent)?;
    Ok(val)
}

fn normalize_and_deserialize(mut val: serde_json::Value) -> Result<TeamSettingsFile, Error> {
    let obj = match val.as_object_mut() {
        Some(o) => o,
        None => {
            return Err(Error::InvalidInput(
                "team settings root must be a YAML mapping".into(),
            ));
        }
    };

    // 1. Validate version == 1
    match obj.get("version") {
        Some(serde_json::Value::Number(n)) if n.as_u64() == Some(1) => {}
        Some(serde_json::Value::Number(n)) => {
            return Err(Error::InvalidInput(format!(
                "unsupported team settings version: {n}, expected 1"
            )));
        }
        Some(other) => {
            return Err(Error::InvalidInput(format!(
                "invalid version field in team settings: expected integer 1, found {other}"
            )));
        }
        None => {
            return Err(Error::InvalidInput(
                "missing version field in team settings (expected version: 1)".into(),
            ));
        }
    }

    // 2. Normalize groups
    if let Some(groups_val) = obj.get_mut("groups") {
        let groups_arr = match groups_val.as_array_mut() {
            Some(arr) => arr,
            None => {
                return Err(Error::InvalidInput(
                    "groups field must be a list of cluster groups".into(),
                ));
            }
        };
        for g in groups_arr.iter_mut() {
            if let Some(g_obj) = g.as_object_mut()
                && !g_obj.contains_key("contexts")
            {
                g_obj.insert("contexts".to_string(), serde_json::Value::Array(Vec::new()));
            }
        }
    }

    // 3. Normalize contexts
    if let Some(contexts_val) = obj.get_mut("contexts") {
        let contexts_map = match contexts_val.as_object_mut() {
            Some(m) => m,
            None => {
                return Err(Error::InvalidInput(
                    "contexts field must be a mapping of context names to settings".into(),
                ));
            }
        };
        for (_ctx_name, ctx_val) in contexts_map.iter_mut() {
            if let Some(c_obj) = ctx_val.as_object_mut() {
                // Alias: color -> colour
                if let Some(color) = c_obj.remove("color") {
                    c_obj.entry("colour".to_string()).or_insert(color);
                }
                // Alias: read_only -> readOnly
                if let Some(ro) = c_obj.remove("read_only") {
                    c_obj.entry("readOnly".to_string()).or_insert(ro);
                }
                // Provide defaults for missing fields so ContextSettings deserializes cleanly
                c_obj
                    .entry("colour".to_string())
                    .or_insert(serde_json::Value::Null);
                c_obj
                    .entry("tags".to_string())
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                c_obj
                    .entry("readOnly".to_string())
                    .or_insert(serde_json::Value::Bool(false));
                c_obj
                    .entry("protected".to_string())
                    .or_insert(serde_json::Value::Bool(false));
                c_obj
                    .entry("pinned".to_string())
                    .or_insert(serde_json::Value::Bool(false));
            }
        }
    }

    serde_json::from_value::<TeamSettingsFile>(val).map_err(|e| Error::InvalidInput(e.to_string()))
}

/// Parses and validates a team settings YAML string.
///
/// Requires `version == 1` and produces a [`TeamSettingsFile`].
pub fn parse_team_settings_yaml(yaml: &str) -> Result<TeamSettingsFile> {
    let trimmed = yaml.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(
            "empty team settings YAML document".into(),
        ));
    }

    if trimmed.starts_with('{')
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return normalize_and_deserialize(val);
    }

    let val = parse_yaml(trimmed)?;
    normalize_and_deserialize(val)
}

/// Reads a team settings YAML file from `path` and merges it into `settings`.
///
/// - Merges cluster groups: adds new groups from team settings; if a group name
///   already exists locally, keeps local definition and records a warning.
/// - Merges context settings: if context has no local settings in `settings.entries`,
///   applies team settings; if it exists locally, preserves local `read_only` / `protected` /
///   `color` unless local is unset/default.
/// - Returns a [`TeamSettingsSummary`].
pub fn merge_team_settings(settings: &SettingsStore, path: &Path) -> Result<TeamSettingsSummary> {
    let raw = std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file = parse_team_settings_yaml(&raw)?;

    let mut applied_groups: u32 = 0;
    let mut warnings: Vec<String> = Vec::new();

    // 1. Merge cluster groups
    for group in file.groups {
        if settings.get_cluster_group(&group.name).is_some() {
            warnings.push(format!(
                "cluster group '{}' already exists locally; keeping local definition",
                group.name
            ));
        } else {
            settings.set_cluster_group(group)?;
            applied_groups += 1;
        }
    }

    // 2. Merge context settings
    let local_entries = settings.get_all();
    let mut applied_contexts: u32 = 0;

    for (context_name, team_ctx) in file.contexts {
        if let Some(local_ctx) = local_entries.get(&context_name) {
            let colour = match &local_ctx.colour {
                Some(c) if !c.is_empty() => Some(c.clone()),
                _ => team_ctx.colour.clone(),
            };
            let read_only = if local_ctx.read_only {
                true
            } else {
                team_ctx.read_only
            };
            let protected = if local_ctx.protected {
                true
            } else {
                team_ctx.protected
            };
            let pinned = if local_ctx.pinned {
                true
            } else {
                team_ctx.pinned
            };
            let mut tags = local_ctx.tags.clone();
            if tags.is_empty() {
                tags = team_ctx.tags.clone();
            } else {
                for tag in &team_ctx.tags {
                    if !tags.contains(tag) {
                        tags.push(tag.clone());
                    }
                }
            }

            let merged = ContextSettings {
                colour,
                tags,
                read_only,
                protected,
                pinned,
                unlocked_until: None,
                watch_namespaces: if local_ctx.watch_namespaces.is_empty() {
                    team_ctx.watch_namespaces.clone()
                } else {
                    local_ctx.watch_namespaces.clone()
                },
            };
            settings.set(&context_name, merged)?;
            applied_contexts += 1;
        } else {
            settings.set(&context_name, team_ctx)?;
            applied_contexts += 1;
        }
    }

    Ok(TeamSettingsSummary {
        loaded_path: path.display().to_string(),
        applied_groups,
        applied_contexts,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ClusterGroup;

    fn temp_test_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fleetglass-team-settings-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_valid_yaml_with_groups_and_contexts() {
        let yaml = r##"
# Team settings for Fleetglass
version: 1
groups:
  - name: staging
    contexts:
      - kind-dev
      - minikube
  - name: production
    contexts:
      - prod-us
contexts:
  kind-dev:
    colour: "#10b981"
    tags:
      - dev
      - internal
    readOnly: false
    protected: false
    pinned: true
  prod-us:
    color: "#ef4444"
    read_only: true
    protected: true
"##;

        let file = parse_team_settings_yaml(yaml).expect("parse valid team settings YAML");
        assert_eq!(file.version, 1);
        assert_eq!(file.groups.len(), 2);
        assert_eq!(file.groups[0].name, "staging");
        assert_eq!(file.groups[0].contexts, vec!["kind-dev", "minikube"]);
        assert_eq!(file.groups[1].name, "production");
        assert_eq!(file.groups[1].contexts, vec!["prod-us"]);

        assert_eq!(file.contexts.len(), 2);
        let kind_ctx = file.contexts.get("kind-dev").unwrap();
        assert_eq!(kind_ctx.colour.as_deref(), Some("#10b981"));
        assert_eq!(kind_ctx.tags, vec!["dev", "internal"]);
        assert!(!kind_ctx.read_only);
        assert!(!kind_ctx.protected);
        assert!(kind_ctx.pinned);

        let prod_ctx = file.contexts.get("prod-us").unwrap();
        assert_eq!(prod_ctx.colour.as_deref(), Some("#ef4444"));
        assert!(prod_ctx.read_only);
        assert!(prod_ctx.protected);
        assert!(!prod_ctx.pinned);
    }

    #[test]
    fn merge_adds_new_groups_and_applies_context_settings() {
        let dir = temp_test_dir("merge-new");
        let settings_path = dir.join("context-settings.json");
        let store = SettingsStore::new(settings_path);

        let yaml = r##"
version: 1
groups:
  - name: dev-cluster
    contexts:
      - dev-1
      - dev-2
  - name: prod-cluster
    contexts:
      - prod-1
contexts:
  dev-1:
    colour: "#22c55e"
    tags:
      - dev
    readOnly: false
    protected: false
    pinned: true
  prod-1:
    colour: "#ef4444"
    readOnly: true
    protected: true
"##;
        let yaml_path = dir.join("team-settings.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();

        let summary = merge_team_settings(&store, &yaml_path).expect("merge team settings");
        assert_eq!(summary.loaded_path, yaml_path.display().to_string());
        assert_eq!(summary.applied_groups, 2);
        assert_eq!(summary.applied_contexts, 2);
        assert!(summary.warnings.is_empty());

        let groups = store.get_cluster_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            store.get_cluster_group("dev-cluster").unwrap().contexts,
            vec!["dev-1", "dev-2"]
        );
        assert_eq!(
            store.get_cluster_group("prod-cluster").unwrap().contexts,
            vec!["prod-1"]
        );

        let dev1 = store.get("dev-1");
        assert_eq!(dev1.colour.as_deref(), Some("#22c55e"));
        assert_eq!(dev1.tags, vec!["dev"]);
        assert!(!dev1.read_only);
        assert!(dev1.pinned);

        let prod1 = store.get("prod-1");
        assert_eq!(prod1.colour.as_deref(), Some("#ef4444"));
        assert!(prod1.read_only);
        assert!(prod1.protected);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conflict_resolution_preserves_local_group_and_settings() {
        let dir = temp_test_dir("conflict");
        let settings_path = dir.join("context-settings.json");
        let store = SettingsStore::new(settings_path);

        // Pre-populate local store with group "staging" and context "prod-us"
        store
            .set_cluster_group(ClusterGroup {
                name: "staging".to_string(),
                contexts: vec!["local-stage-1".to_string()],
            })
            .unwrap();

        store
            .set(
                "prod-us",
                ContextSettings {
                    colour: Some("#0000ff".to_string()),
                    tags: vec!["custom".to_string()],
                    read_only: true,
                    protected: false,
                    pinned: false,
                    unlocked_until: None,
                    watch_namespaces: Vec::new(),
                },
            )
            .unwrap();

        let yaml = r##"
version: 1
groups:
  - name: staging
    contexts:
      - team-stage-1
      - team-stage-2
  - name: production
    contexts:
      - prod-us
contexts:
  prod-us:
    colour: "#ff0000"
    tags:
      - team-prod
    readOnly: false
    protected: true
  dev-us:
    colour: "#00ff00"
    tags:
      - dev
"##;
        let yaml_path = dir.join("team-settings.yaml");
        std::fs::write(&yaml_path, yaml).unwrap();

        let summary = merge_team_settings(&store, &yaml_path).expect("merge with conflicts");
        // "staging" exists locally so 1 group applied ("production"), 1 conflict warning
        assert_eq!(summary.applied_groups, 1);
        assert_eq!(summary.applied_contexts, 2);
        assert_eq!(summary.warnings.len(), 1);
        assert!(
            summary.warnings[0].contains("staging")
                && summary.warnings[0].contains("already exists locally"),
            "warning should mention staging group conflict, was: {}",
            summary.warnings[0]
        );

        // Verify local group definition was kept
        let staging = store.get_cluster_group("staging").unwrap();
        assert_eq!(staging.contexts, vec!["local-stage-1"]);

        // Verify new group "production" was added
        let production = store.get_cluster_group("production").unwrap();
        assert_eq!(production.contexts, vec!["prod-us"]);

        // Verify context "prod-us" preserves local colour and read_only, but applies team protected
        let prod_ctx = store.get("prod-us");
        assert_eq!(prod_ctx.colour.as_deref(), Some("#0000ff"));
        assert!(prod_ctx.read_only);
        assert!(prod_ctx.protected);
        assert!(prod_ctx.tags.contains(&"custom".to_string()));
        assert!(prod_ctx.tags.contains(&"team-prod".to_string()));

        // Verify new context "dev-us" was applied
        let dev_ctx = store.get("dev-us");
        assert_eq!(dev_ctx.colour.as_deref(), Some("#00ff00"));
        assert_eq!(dev_ctx.tags, vec!["dev"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_version_or_malformed_yaml_returns_typed_error() {
        // Missing version
        let err = parse_team_settings_yaml("groups: []").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        // Version != 1
        let err = parse_team_settings_yaml("version: 2\ngroups: []").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        let err = parse_team_settings_yaml("version: 0\ngroups: []").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        let err = parse_team_settings_yaml("version: 'v1'\ngroups: []").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        // Empty input
        let err = parse_team_settings_yaml("").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        let err = parse_team_settings_yaml("   \n# comment only\n").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        // Malformed YAML
        let err = parse_team_settings_yaml("version: 1\ngroups: [unclosed").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        let err = parse_team_settings_yaml("version: 1\n\ttabs_not_allowed: true").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        let err = parse_team_settings_yaml(": invalid : yaml :").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));

        let err = parse_team_settings_yaml("version: 1\n  invalid_root_indent: true").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
