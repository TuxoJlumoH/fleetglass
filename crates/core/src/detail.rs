//! Detail view helpers (`P1-05`), server-side dry-run diff (`P1-06`), and
//! additional built-in / CRD [`DynamicObject`] row projections (`P1-03`).

use k8s_openapi::api::core::v1::{ConfigMap, Event, Namespace, Node, Pod, Secret, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, ApiResource, DynamicObject, ListParams};
use kube::discovery::Discovery;
use serde_json::Value;

use crate::api::{
    Cell, CompareOptions, ConditionSummary, ContainerPortSummary, ContainerSummary, DiffResult,
    PodSummary, RelatedRef, ResourceDetail, ResourcePair, Row, RowStatus,
};
use crate::error::{Error, Result};

/// Removes `metadata.managedFields` from a Kubernetes JSON object so detail
/// YAML views stay clean and readable (`P1-05`).
pub fn strip_managed_fields(value: &mut Value) {
    if let Some(meta) = value
        .as_object_mut()
        .and_then(|obj| obj.get_mut("metadata"))
        .and_then(Value::as_object_mut)
    {
        meta.remove("managedFields");
    }
}

/// Alias for [`strip_managed_fields`].
pub fn strip_managed_fields_json(value: &mut Value) {
    strip_managed_fields(value);
}

/// Maps `meta.owner_references` to [`RelatedRef`] entries with `relationship: "owner"`.
pub fn extract_owner_refs(meta: &ObjectMeta) -> Vec<RelatedRef> {
    let namespace = meta.namespace.clone();
    meta.owner_references
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|owner| RelatedRef {
            kind: owner.kind.clone(),
            name: owner.name.clone(),
            namespace: namespace.clone(),
            uid: owner.uid.clone(),
            relationship: "owner".to_owned(),
        })
        .collect()
}

/// Produces a clean line-by-line `-`/`+`/` ` diff of `before` and `after` for
/// server-side dry-run previews (`P1-06`).
pub fn compute_unified_diff(before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let n = before_lines.len();
    let m = after_lines.len();

    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if before_lines[i] == after_lines[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut out = Vec::with_capacity(n + m);
    let mut i = 0;
    let mut j = 0;
    while i < n && j < m {
        if before_lines[i] == after_lines[j] {
            out.push(format!(" {}", before_lines[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(format!("-{}", before_lines[i]));
            i += 1;
        } else {
            out.push(format!("+{}", after_lines[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(format!("-{}", before_lines[i]));
        i += 1;
    }
    while j < m {
        out.push(format!("+{}", after_lines[j]));
        j += 1;
    }

    out.join("\n")
}

/// Alias for [`compute_unified_diff`].
pub fn compute_line_diff(before: &str, after: &str) -> String {
    compute_unified_diff(before, after)
}

/// Normalizes a Kubernetes resource JSON value for comparison across clusters or
/// namespaces with options (`P2-02`, `04-04`).
pub fn normalize_for_comparison_with_options(value: &mut Value, options: &CompareOptions) {
    match value {
        Value::Object(obj) => {
            let is_secret = obj
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|k| k.eq_ignore_ascii_case("Secret"));

            if is_secret && !options.reveal_secrets {
                if let Some(data) = obj.get_mut("data").and_then(Value::as_object_mut) {
                    for v in data.values_mut() {
                        *v = Value::String("<redacted>".to_owned());
                    }
                }
                if let Some(string_data) = obj.get_mut("stringData").and_then(Value::as_object_mut)
                {
                    for v in string_data.values_mut() {
                        *v = Value::String("<redacted>".to_owned());
                    }
                }
            }

            if !options.include_metadata_and_status {
                obj.remove("status");
                if let Some(meta) = obj.get_mut("metadata").and_then(Value::as_object_mut) {
                    meta.remove("managedFields");
                    meta.remove("uid");
                    meta.remove("resourceVersion");
                    meta.remove("generation");
                    meta.remove("creationTimestamp");
                    meta.remove("selfLink");

                    if let Some(labels) = meta.get_mut("labels").and_then(Value::as_object_mut) {
                        labels.remove("pod-template-hash");
                        labels.remove("controller-revision-hash");
                    }
                    if let Some(annotations) =
                        meta.get_mut("annotations").and_then(Value::as_object_mut)
                    {
                        annotations.remove("deployment.kubernetes.io/revision");
                    }
                }

                // Also strip template hashes in spec.template.metadata
                if let Some(labels) = obj
                    .get_mut("spec")
                    .and_then(Value::as_object_mut)
                    .and_then(|s| s.get_mut("template"))
                    .and_then(Value::as_object_mut)
                    .and_then(|t| t.get_mut("metadata"))
                    .and_then(Value::as_object_mut)
                    .and_then(|m| m.get_mut("labels"))
                    .and_then(Value::as_object_mut)
                {
                    labels.remove("pod-template-hash");
                    labels.remove("controller-revision-hash");
                }
            }

            if let Some(items) = obj.get_mut("items").and_then(Value::as_array_mut) {
                for item in items {
                    normalize_for_comparison_with_options(item, options);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_for_comparison_with_options(item, options);
            }
        }
        _ => {}
    }
}

/// Normalizes a Kubernetes resource JSON value for comparison across clusters or
/// namespaces (`P2-02`).
pub fn normalize_for_comparison(value: &mut Value) {
    normalize_for_comparison_with_options(value, &CompareOptions::default());
}

/// Collapses identical context blocks in a unified diff, keeping a few lines of context around hunks (`04-04`).
pub fn collapse_diff_context(diff: &str, context_lines: usize) -> String {
    let lines: Vec<&str> = diff.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut keep = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with('+') || line.starts_with('-') {
            keep[i] = true;
            for offset in 1..=context_lines {
                if i >= offset {
                    keep[i - offset] = true;
                }
                if i + offset < lines.len() {
                    keep[i + offset] = true;
                }
            }
        }
    }

    let mut out = Vec::new();
    let mut collapsed = false;

    for (i, line) in lines.iter().enumerate() {
        if keep[i] {
            collapsed = false;
            out.push((*line).to_string());
        } else if !collapsed {
            collapsed = true;
            out.push("@@ ... @@".to_string());
        }
    }

    out.join("\n")
}

/// Compares two Kubernetes resource definitions (in YAML or JSON format) across
/// clusters or namespaces (`P2-02`).
pub fn compare_resources(left_yaml: &str, right_yaml: &str) -> DiffResult {
    compare_resources_with_options(left_yaml, right_yaml, CompareOptions::default())
}

/// Compares two Kubernetes resource definitions with normalization and diff options (`04-04`).
pub fn compare_resources_with_options(
    left_yaml: &str,
    right_yaml: &str,
    options: CompareOptions,
) -> DiffResult {
    let left_trimmed = left_yaml.trim();
    let right_trimmed = right_yaml.trim();

    if left_trimmed.is_empty() && right_trimmed.is_empty() {
        return DiffResult {
            unified_diff: String::new(),
            identical: true,
        };
    }

    let mut left_val = parse_yaml_or_json(left_trimmed);
    let mut right_val = parse_yaml_or_json(right_trimmed);

    match (&mut left_val, &mut right_val) {
        (Some(l), Some(r)) => {
            normalize_for_comparison_with_options(l, &options);
            normalize_for_comparison_with_options(r, &options);
            let identical = l == r;
            let left_norm = value_to_yaml(l);
            let right_norm = value_to_yaml(r);
            let raw_diff = compute_unified_diff(left_norm.trim(), right_norm.trim());
            let unified_diff = if options.only_differences && !identical {
                collapse_diff_context(&raw_diff, 3)
            } else {
                raw_diff
            };
            DiffResult {
                unified_diff,
                identical,
            }
        }
        _ => {
            let raw_diff = compute_unified_diff(left_trimmed, right_trimmed);
            let identical = left_trimmed == right_trimmed;
            let unified_diff = if options.only_differences && !identical {
                collapse_diff_context(&raw_diff, 3)
            } else {
                raw_diff
            };
            DiffResult {
                unified_diff,
                identical,
            }
        }
    }
}

/// Formats a [`serde_json::Value`] into a deterministic, standard YAML string.
pub fn value_to_yaml(value: &Value) -> String {
    let mut out = String::new();
    format_yaml_value(value, 0, &mut out);
    out
}

fn format_yaml_value(value: &Value, indent: usize, out: &mut String) {
    let spaces = "  ".repeat(indent);
    match value {
        Value::Null => out.push_str("null\n"),
        Value::Bool(b) => {
            out.push_str(if *b { "true\n" } else { "false\n" });
        }
        Value::Number(n) => {
            out.push_str(&format!("{n}\n"));
        }
        Value::String(s) => {
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
                || s == "true"
                || s == "false"
                || s == "null"
                || s.parse::<f64>().is_ok()
            {
                out.push_str(&format!("\"{s}\"\n"));
            } else {
                out.push_str(s);
                out.push('\n');
            }
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]\n");
            } else {
                out.push('\n');
                for item in arr {
                    out.push_str(&spaces);
                    out.push_str("- ");
                    format_array_item(item, indent + 1, out);
                }
            }
        }
        Value::Object(map) => {
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

fn format_array_item(item: &Value, indent: usize, out: &mut String) {
    if let Value::Object(map) = item {
        if map.is_empty() {
            out.push_str("{}\n");
            return;
        }
        let spaces = "  ".repeat(indent);
        let mut first = true;
        for (k, v) in map {
            if first {
                out.push_str(k);
                out.push_str(": ");
                format_yaml_value(v, indent, out);
                first = false;
            } else {
                out.push_str(&spaces);
                out.push_str(k);
                out.push_str(": ");
                format_yaml_value(v, indent, out);
            }
        }
    } else {
        format_yaml_value(item, indent, out);
    }
}

fn strip_yaml_comment(s: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                return s[..i].trim_end();
            }
            _ => {}
        }
    }
    s
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

fn parse_scalar(s: &str) -> Value {
    let s = s.trim();
    if s == "true" {
        Value::Bool(true)
    } else if s == "false" {
        Value::Bool(false)
    } else if s == "null" || s == "~" {
        Value::Null
    } else if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        Value::String(s[1..s.len() - 1].to_string())
    } else if let Ok(n) = s.parse::<i64>() {
        Value::Number(n.into())
    } else if let Ok(n) = s.parse::<u64>() {
        Value::Number(n.into())
    } else if let Ok(f) = s.parse::<f64>() {
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(s.to_string()))
    } else if s == "{}" {
        Value::Object(serde_json::Map::new())
    } else if s == "[]" {
        Value::Array(Vec::new())
    } else if let Ok(v) = serde_json::from_str::<Value>(s) {
        v
    } else {
        Value::String(s.to_string())
    }
}

#[derive(Clone, Copy, Debug)]
struct ParsedYamlLine<'a> {
    indent: usize,
    content: &'a str,
    raw: &'a str,
}

fn parse_yaml(input: &str) -> Option<Value> {
    let mut lines = Vec::new();
    for raw in input.lines() {
        let trimmed_content = strip_yaml_comment(raw.trim_start());
        if trimmed_content == "---" || trimmed_content == "..." {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        lines.push(ParsedYamlLine {
            indent,
            content: trimmed_content,
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
        return None;
    }

    let first_non_empty = lines.iter().position(|l| !l.content.is_empty())?;
    let start_indent = lines[first_non_empty].indent;
    let (val, _) = parse_yaml_block(&lines, first_non_empty, start_indent);
    Some(val)
}

fn parse_yaml_block(
    lines: &[ParsedYamlLine<'_>],
    mut idx: usize,
    current_indent: usize,
) -> (Value, usize) {
    if idx >= lines.len() {
        return (Value::Null, idx);
    }

    while idx < lines.len() && lines[idx].content.is_empty() {
        idx += 1;
    }
    if idx >= lines.len() || lines[idx].indent < current_indent {
        return (Value::Null, idx);
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
                    let (child_val, end_idx) = parse_yaml_block(lines, next_idx, child_indent);
                    arr.push(child_val);
                    idx = end_idx;
                } else {
                    arr.push(Value::Null);
                    idx += 1;
                }
            } else if let Some((k, v_str)) = split_key_value(item_text) {
                let mut map = serde_json::Map::new();
                let key_str = unquote(k).to_string();
                if v_str == "|" || v_str == "|-" || v_str == ">" || v_str == ">-" {
                    let mut block_lines = Vec::new();
                    let mut scan_idx = idx + 1;
                    while scan_idx < lines.len()
                        && (lines[scan_idx].content.is_empty()
                            || lines[scan_idx].indent > current_indent)
                    {
                        block_lines.push(lines[scan_idx].raw.trim_start());
                        scan_idx += 1;
                    }
                    map.insert(key_str, Value::String(block_lines.join("\n")));
                    idx = scan_idx;
                } else if v_str.is_empty() {
                    let next_idx = idx + 1;
                    if next_idx < lines.len() && lines[next_idx].indent > current_indent {
                        let child_indent = lines[next_idx].indent;
                        let (child_val, end_idx) = parse_yaml_block(lines, next_idx, child_indent);
                        map.insert(key_str, child_val);
                        idx = end_idx;
                    } else {
                        map.insert(key_str, Value::Null);
                        idx += 1;
                    }
                } else {
                    map.insert(key_str, parse_scalar(v_str));
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
                        let sub_k_str = unquote(sub_k).to_string();
                        if sub_v == "|" || sub_v == "|-" || sub_v == ">" || sub_v == ">-" {
                            let mut block_lines = Vec::new();
                            let mut scan_idx = idx + 1;
                            while scan_idx < lines.len()
                                && (lines[scan_idx].content.is_empty()
                                    || lines[scan_idx].indent > lines[idx].indent)
                            {
                                block_lines.push(lines[scan_idx].raw.trim_start());
                                scan_idx += 1;
                            }
                            map.insert(sub_k_str, Value::String(block_lines.join("\n")));
                            idx = scan_idx;
                        } else if sub_v.is_empty() {
                            let next_idx = idx + 1;
                            if next_idx < lines.len() && lines[next_idx].indent > lines[idx].indent
                            {
                                let (child_val, end_idx) =
                                    parse_yaml_block(lines, next_idx, lines[next_idx].indent);
                                map.insert(sub_k_str, child_val);
                                idx = end_idx;
                            } else {
                                map.insert(sub_k_str, Value::Null);
                                idx += 1;
                            }
                        } else {
                            map.insert(sub_k_str, parse_scalar(sub_v));
                            idx += 1;
                        }
                    } else {
                        idx += 1;
                    }
                }

                arr.push(Value::Object(map));
            } else {
                arr.push(parse_scalar(item_text));
                idx += 1;
            }
        }
        (Value::Array(arr), idx)
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
            if lines[idx].indent != current_indent {
                break;
            }

            if let Some((k, v_str)) = split_key_value(lines[idx].content) {
                let key_str = unquote(k).to_string();
                if v_str == "|" || v_str == "|-" || v_str == ">" || v_str == ">-" {
                    let mut block_lines = Vec::new();
                    let mut scan_idx = idx + 1;
                    while scan_idx < lines.len()
                        && (lines[scan_idx].content.is_empty()
                            || lines[scan_idx].indent > current_indent)
                    {
                        block_lines.push(lines[scan_idx].raw.trim_start());
                        scan_idx += 1;
                    }
                    map.insert(key_str, Value::String(block_lines.join("\n")));
                    idx = scan_idx;
                } else if v_str.is_empty() {
                    let next_idx = idx + 1;
                    if next_idx < lines.len() && lines[next_idx].indent > current_indent {
                        let child_indent = lines[next_idx].indent;
                        let (child_val, end_idx) = parse_yaml_block(lines, next_idx, child_indent);
                        map.insert(key_str, child_val);
                        idx = end_idx;
                    } else {
                        map.insert(key_str, Value::Null);
                        idx += 1;
                    }
                } else {
                    map.insert(key_str, parse_scalar(v_str));
                    idx += 1;
                }
            } else {
                if map.is_empty() {
                    return (parse_scalar(lines[idx].content), idx + 1);
                }
                break;
            }
        }
        (Value::Object(map), idx)
    }
}

pub(crate) fn parse_yaml_or_json(s: &str) -> Option<Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        return Some(val);
    }
    parse_yaml(trimmed)
}

fn extract_meta_fields(
    meta: &ObjectMeta,
) -> (
    String,
    String,
    Option<String>,
    Vec<(String, String)>,
    String,
) {
    let uid = meta.uid.clone().unwrap_or_default();
    let name = meta.name.clone().unwrap_or_default();
    let namespace = meta.namespace.clone();
    let mut labels: Vec<(String, String)> = meta
        .labels
        .as_ref()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    labels.sort();
    let created = meta
        .creation_timestamp
        .as_ref()
        .map(|ts| ts.0.to_string())
        .unwrap_or_default();
    (uid, name, namespace, labels, created)
}

fn classify_phase_str(phase: &str) -> RowStatus {
    match phase {
        "Running" | "Active" | "Ready" | "Bound" | "Available" | "Healthy" | "Normal" => {
            RowStatus::Healthy
        }
        "Pending" | "Progressing" | "Creating" => RowStatus::Pending,
        "Succeeded" | "Completed" | "Complete" => RowStatus::Completed,
        "Failed" | "Error" => RowStatus::Failed,
        "Degraded" | "Warning" => RowStatus::Degraded,
        "Terminating" => RowStatus::Terminating,
        _ => RowStatus::Unknown,
    }
}

/// Projects any CRD or dynamic Kubernetes resource into a compact [`Row`] (`P1-03`).
pub fn project_dynamic_object(obj: &DynamicObject, cluster: &str, kind: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&obj.metadata);

    let (status_reason, status_phase) = if obj.metadata.deletion_timestamp.is_some() {
        ("Terminating".to_owned(), RowStatus::Terminating)
    } else if let Some(phase) = obj
        .data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        (phase.to_owned(), classify_phase_str(phase))
    } else {
        ("Active".to_owned(), RowStatus::Healthy)
    };

    let cells = vec![
        Cell::Status {
            label: status_reason.clone(),
            phase: status_phase,
        },
        Cell::Timestamp(created),
    ];

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason,
        cells,
        labels,
        custom: Vec::new(),
    }
}

/// Projects a [`Service`] into a compact [`Row`] (`P1-03`).
pub fn project_service(svc: &Service, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&svc.metadata);
    let svc_type = svc
        .spec
        .as_ref()
        .and_then(|s| s.type_.clone())
        .unwrap_or_else(|| "ClusterIP".to_owned());
    let cluster_ip = svc
        .spec
        .as_ref()
        .and_then(|s| s.cluster_ip.clone())
        .unwrap_or_default();

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Service".to_owned(),
        namespace,
        name,
        status: RowStatus::Healthy,
        status_reason: svc_type.clone(),
        cells: vec![
            Cell::Text(svc_type),
            Cell::Text(cluster_ip),
            Cell::Timestamp(created),
        ],
        labels,
        custom: Vec::new(),
    }
}

/// Default column headers for projected [`Node`] rows, in cell order. `CPU` and
/// `Memory` (02-01) carry metrics-server usage with `allocatable` as the limit.
pub const NODE_COLUMNS: &[&str] = &["Status", "CreationTimestamp", "CPU", "Memory"];

/// Projects a [`Node`] into a compact [`Row`] (`P1-03`).
pub fn project_node(node: &Node, cluster: &str) -> Row {
    let [cpu, memory] = crate::metrics::node_usage_cells(node);
    let (uid, name, namespace, labels, created) = extract_meta_fields(&node.metadata);
    let unschedulable = node
        .spec
        .as_ref()
        .and_then(|s| s.unschedulable)
        .unwrap_or(false);
    let ready = node
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[])
        .iter()
        .any(|c| c.type_ == "Ready" && c.status == "True");

    let (status_reason, status_phase) = match (ready, unschedulable) {
        (true, false) => ("Ready".to_owned(), RowStatus::Healthy),
        (true, true) => ("Ready,SchedulingDisabled".to_owned(), RowStatus::Degraded),
        (false, true) => ("NotReady,SchedulingDisabled".to_owned(), RowStatus::Failed),
        (false, false) => ("NotReady".to_owned(), RowStatus::Failed),
    };

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Node".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason: status_reason.clone(),
        cells: vec![
            Cell::Status {
                label: status_reason,
                phase: status_phase,
            },
            Cell::Timestamp(created),
            cpu,
            memory,
        ],
        labels,
        custom: Vec::new(),
    }
}

/// Projects a [`Namespace`] into a compact [`Row`] (`P1-03`).
pub fn project_namespace(ns: &Namespace, cluster: &str) -> Row {
    let (uid, name, _ns, labels, created) = extract_meta_fields(&ns.metadata);
    let phase_str = ns
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("Active");
    let status_phase = classify_phase_str(phase_str);

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Namespace".to_owned(),
        namespace: None,
        name,
        status: status_phase,
        status_reason: phase_str.to_owned(),
        cells: vec![
            Cell::Status {
                label: phase_str.to_owned(),
                phase: status_phase,
            },
            Cell::Timestamp(created),
        ],
        labels,
        custom: Vec::new(),
    }
}

/// Projects a [`ConfigMap`] into a compact [`Row`] (`P1-03`).
pub fn project_config_map(cm: &ConfigMap, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&cm.metadata);
    let data_count = cm
        .data
        .as_ref()
        .map_or(0, |m| i32::try_from(m.len()).unwrap_or(i32::MAX));

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "ConfigMap".to_owned(),
        namespace,
        name,
        status: RowStatus::Healthy,
        status_reason: "Active".to_owned(),
        cells: vec![Cell::Int(data_count), Cell::Timestamp(created)],
        labels,
        custom: Vec::new(),
    }
}

/// Projects a [`Secret`] into a compact [`Row`] (`P1-03`).
pub fn project_secret(secret: &Secret, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&secret.metadata);
    let secret_type = secret.type_.clone().unwrap_or_else(|| "Opaque".to_owned());
    let data_count = secret
        .data
        .as_ref()
        .map_or(0, |m| i32::try_from(m.len()).unwrap_or(i32::MAX));

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Secret".to_owned(),
        namespace,
        name,
        status: RowStatus::Healthy,
        status_reason: secret_type.clone(),
        cells: vec![
            Cell::Text(secret_type),
            Cell::Int(data_count),
            Cell::Timestamp(created),
        ],
        labels,
        custom: Vec::new(),
    }
}

/// Decodes Kubernetes [`Secret`] byte data into plain UTF-8 strings (`P2-06`).
///
/// Replaces non-UTF-8 bytes with lossy representation.
pub fn decode_secret_data(
    data: &std::collections::BTreeMap<String, k8s_openapi::ByteString>,
) -> std::collections::BTreeMap<String, String> {
    data.iter()
        .map(|(k, v)| {
            let decoded = match std::str::from_utf8(&v.0) {
                Ok(valid) => valid.to_owned(),
                Err(_) => String::from_utf8_lossy(&v.0).into_owned(),
            };
            (k.clone(), decoded)
        })
        .collect()
}

/// Decodes base64-encoded secret values (as they appear in a Secret's `data`)
/// into lossy UTF-8 plaintext (`P2-06`).
///
/// Uses the standard alphabet with padding, like the Kubernetes API. An invalid
/// value yields [`Error::InvalidBase64`] naming the key, never the value.
pub fn decode_secret_values(
    data: &std::collections::BTreeMap<String, String>,
) -> Result<std::collections::BTreeMap<String, String>> {
    let bytes = data
        .iter()
        .map(|(k, v)| {
            // `ByteString`'s serde impl is k8s-openapi's base64 codec; reusing it
            // avoids a direct base64 dependency in core.
            serde_json::from_value::<k8s_openapi::ByteString>(Value::String(v.clone()))
                .map(|b| (k.clone(), b))
                .map_err(|_| Error::InvalidBase64 { key: k.clone() })
        })
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
    Ok(decode_secret_data(&bytes))
}

/// Encodes plaintext secret values into base64 strings suitable for a Secret's
/// `data` field (`P2-06`).
pub fn encode_secret_values(
    data: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    encode_secret_data(data)
        .into_iter()
        .map(|(k, b)| {
            let encoded = match serde_json::to_value(&b) {
                Ok(Value::String(s)) => s,
                // `ByteString` always serializes to a JSON string.
                _ => String::new(),
            };
            (k, encoded)
        })
        .collect()
}

/// Encodes plaintext string values into Kubernetes [`k8s_openapi::ByteString`] values (`P2-06`).
pub fn encode_secret_data(
    data: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, k8s_openapi::ByteString> {
    data.iter()
        .map(|(k, v)| (k.clone(), k8s_openapi::ByteString(v.as_bytes().to_vec())))
        .collect()
}

pub(crate) fn event_timestamp(ev: &Event) -> Option<String> {
    ev.last_timestamp
        .as_ref()
        .map(|t| t.0.to_string())
        .or_else(|| ev.event_time.as_ref().map(|t| t.0.to_string()))
        .or_else(|| ev.first_timestamp.as_ref().map(|t| t.0.to_string()))
        .or_else(|| {
            ev.metadata
                .creation_timestamp
                .as_ref()
                .map(|t| t.0.to_string())
        })
}

/// Projects an [`Event`] into a compact [`Row`] (`P1-03`).
pub fn project_event(ev: &Event, cluster: &str) -> Row {
    let (uid, name, namespace, labels, created) = extract_meta_fields(&ev.metadata);
    let event_type = ev.type_.clone().unwrap_or_else(|| "Normal".to_owned());
    let reason = ev.reason.clone().unwrap_or_default();
    let message = ev.message.clone().unwrap_or_default();
    let status_phase = classify_phase_str(&event_type);
    let involved_kind = ev.involved_object.kind.clone().unwrap_or_default();
    let involved_name = ev.involved_object.name.clone().unwrap_or_default();
    let count = ev.count.unwrap_or(1);
    let last_ts = event_timestamp(ev).unwrap_or_else(|| created.clone());

    Row {
        uid,
        cluster: cluster.to_owned(),
        kind: "Event".to_owned(),
        namespace,
        name,
        status: status_phase,
        status_reason: reason.clone(),
        cells: vec![
            Cell::Status {
                label: event_type,
                phase: status_phase,
            },
            Cell::Text(reason),
            Cell::Text(message),
            Cell::Timestamp(created),
            Cell::Text(involved_kind),
            Cell::Text(involved_name),
            Cell::Int(count),
            Cell::Timestamp(last_ts),
        ],
        labels,
        custom: Vec::new(),
    }
}

pub(crate) fn builtin_api_resource(kind: &str) -> Option<ApiResource> {
    match kind {
        "Pod" | "pod" | "pods" => Some(ApiResource::erase::<k8s_openapi::api::core::v1::Pod>(&())),
        "Deployment" | "deployment" | "deployments" => {
            Some(ApiResource::erase::<k8s_openapi::api::apps::v1::Deployment>(&()))
        }
        "StatefulSet" | "statefulset" | "statefulsets" => {
            Some(ApiResource::erase::<k8s_openapi::api::apps::v1::StatefulSet>(&()))
        }
        "DaemonSet" | "daemonset" | "daemonsets" => {
            Some(ApiResource::erase::<k8s_openapi::api::apps::v1::DaemonSet>(&()))
        }
        "Job" | "job" | "jobs" => Some(ApiResource::erase::<k8s_openapi::api::batch::v1::Job>(&())),
        "CronJob" | "cronjob" | "cronjobs" => {
            Some(ApiResource::erase::<k8s_openapi::api::batch::v1::CronJob>(&()))
        }
        "Service" | "service" | "services" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::Service>(&()))
        }
        "Ingress" | "ingress" | "ingresses" => {
            Some(ApiResource::erase::<k8s_openapi::api::networking::v1::Ingress>(&()))
        }
        "ConfigMap" | "configmap" | "configmaps" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::ConfigMap>(&()))
        }
        "Secret" | "secret" | "secrets" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::Secret>(&()))
        }
        "Node" | "node" | "nodes" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::Node>(&()))
        }
        "Namespace" | "namespace" | "namespaces" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::Namespace>(&()))
        }
        "PersistentVolumeClaim"
        | "persistentvolumeclaim"
        | "persistentvolumeclaims"
        | "pvc"
        | "pvcs" => Some(ApiResource::erase::<
            k8s_openapi::api::core::v1::PersistentVolumeClaim,
        >(&())),
        "PersistentVolume" | "persistentvolume" | "persistentvolumes" | "pv" | "pvs" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::PersistentVolume>(&()))
        }
        "Event" | "event" | "events" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::Event>(&()))
        }
        "ServiceAccount" | "serviceaccount" | "serviceaccounts" => {
            Some(ApiResource::erase::<k8s_openapi::api::core::v1::ServiceAccount>(&()))
        }
        "Role" | "role" | "roles" => {
            Some(ApiResource::erase::<k8s_openapi::api::rbac::v1::Role>(&()))
        }
        "ClusterRole" | "clusterrole" | "clusterroles" => {
            Some(ApiResource::erase::<k8s_openapi::api::rbac::v1::ClusterRole>(&()))
        }
        "RoleBinding" | "rolebinding" | "rolebindings" => {
            Some(ApiResource::erase::<k8s_openapi::api::rbac::v1::RoleBinding>(&()))
        }
        "ClusterRoleBinding" | "clusterrolebinding" | "clusterrolebindings" => {
            Some(ApiResource::erase::<
                k8s_openapi::api::rbac::v1::ClusterRoleBinding,
            >(&()))
        }
        "CustomResourceDefinition"
        | "customresourcedefinition"
        | "customresourcedefinitions"
        | "crd"
        | "crds" => Some(ApiResource::erase::<
            k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
        >(&())),
        _ => None,
    }
}

pub(crate) async fn resolve_api_resource(client: &kube::Client, kind: &str) -> Result<ApiResource> {
    if let Some(ar) = builtin_api_resource(kind) {
        return Ok(ar);
    }

    let discovery = match Discovery::new(client.clone()).run_aggregated().await {
        Ok(d) => d,
        Err(_) => Discovery::new(client.clone())
            .run()
            .await
            .map_err(Error::Kube)?,
    };

    let mut all_resources = Vec::new();
    for group in discovery.groups() {
        for (ar, _caps) in group.recommended_resources() {
            all_resources.push(ar);
        }
    }

    for ar in &all_resources {
        if ar.kind == kind {
            return Ok(ar.clone());
        }
    }
    for ar in &all_resources {
        if ar.kind.eq_ignore_ascii_case(kind) || ar.plural.eq_ignore_ascii_case(kind) {
            return Ok(ar.clone());
        }
    }

    Err(Error::Kube(kube::Error::Api(Box::new(
        kube::core::Status {
            message: format!("resource kind '{kind}' not found in cluster API"),
            reason: "NotFound".to_owned(),
            code: 404,
            ..Default::default()
        },
    ))))
}

pub(crate) async fn fetch_dynamic_object(
    client: &kube::Client,
    kind: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<DynamicObject> {
    let ar = resolve_api_resource(client, kind).await?;
    let api: Api<DynamicObject> = match namespace {
        Some(ns) if !ns.is_empty() => Api::namespaced_with(client.clone(), ns, &ar),
        _ => Api::all_with(client.clone(), &ar),
    };
    let mut obj = api.get(name).await.map_err(Error::Kube)?;
    if obj.types.is_none() {
        obj.types = Some(kube::core::TypeMeta {
            api_version: ar.api_version.clone(),
            kind: ar.kind.clone(),
        });
    }
    Ok(obj)
}

/// Redacts all values under `data` and `stringData` in a Secret's JSON representation.
pub fn redact_secret_value(val: &mut Value) {
    if let Some(obj) = val.as_object_mut() {
        if let Some(data) = obj.get_mut("data").and_then(|v| v.as_object_mut()) {
            for v in data.values_mut() {
                *v = Value::String("[REDACTED]".to_string());
            }
        }
        if let Some(sdata) = obj.get_mut("stringData").and_then(|v| v.as_object_mut()) {
            for v in sdata.values_mut() {
                *v = Value::String("[REDACTED]".to_string());
            }
        }
    }
}

/// Fetches the cleaned YAML representation of a Kubernetes resource (`R-03`),
/// optionally redacting secret data keys.
pub async fn fetch_resource_yaml_redacted(
    client: &kube::Client,
    kind: &str,
    namespace: Option<&str>,
    name: &str,
    redact_secret: bool,
) -> Result<String> {
    let obj = fetch_dynamic_object(client, kind, namespace, name).await?;
    let mut val = serde_json::to_value(&obj).map_err(kube::Error::SerdeError)?;
    strip_managed_fields(&mut val);
    let is_secret = kind.eq_ignore_ascii_case("Secret")
        || kind.eq_ignore_ascii_case("secrets")
        || val
            .get("kind")
            .and_then(|k| k.as_str())
            .map(|k| k.eq_ignore_ascii_case("Secret"))
            .unwrap_or(false);
    if redact_secret && is_secret {
        redact_secret_value(&mut val);
    }
    Ok(value_to_yaml(&val))
}

/// Fetches the cleaned YAML representation of a Kubernetes resource (`R-03`).
pub async fn fetch_resource_yaml(
    client: &kube::Client,
    kind: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<String> {
    fetch_resource_yaml_redacted(client, kind, namespace, name, false).await
}

/// Fetches detailed resource information, including cleaned YAML and related events (`R-03`).
pub async fn fetch_resource_detail(
    client: &kube::Client,
    cluster: &str,
    kind: &str,
    namespace: Option<&str>,
    name: &str,
) -> Result<ResourceDetail> {
    let obj = fetch_dynamic_object(client, kind, namespace, name).await?;
    let uid = obj.metadata.uid.clone().unwrap_or_default();
    let res_name = obj.metadata.name.clone().unwrap_or_else(|| name.to_owned());
    let res_namespace = obj
        .metadata
        .namespace
        .clone()
        .or_else(|| namespace.map(str::to_owned));
    let related = extract_owner_refs(&obj.metadata);

    let mut val = serde_json::to_value(&obj).map_err(kube::Error::SerdeError)?;
    strip_managed_fields(&mut val);
    let yaml = value_to_yaml(&val);

    let event_api: Api<Event> = match namespace {
        Some(ns) if !ns.is_empty() => Api::namespaced(client.clone(), ns),
        _ => Api::all(client.clone()),
    };

    let field_selector = format!("involvedObject.name={name}");
    let lp = ListParams::default().fields(&field_selector);

    let event_list = match event_api.list(&lp).await {
        Ok(list) => list.items,
        Err(err) => {
            tracing::warn!("failed to list events for {kind}/{name}: {err}");
            Vec::new()
        }
    };

    let mut matching_events: Vec<Event> = event_list
        .into_iter()
        .filter(|ev| {
            ev.involved_object.name.as_deref() == Some(name)
                && ev
                    .involved_object
                    .kind
                    .as_deref()
                    .is_none_or(|k| k.is_empty() || k.eq_ignore_ascii_case(kind))
        })
        .collect();

    matching_events.sort_by(|a, b| {
        let ts_a = event_timestamp(a);
        let ts_b = event_timestamp(b);
        ts_b.cmp(&ts_a)
    });

    let events_truncated = matching_events.len() > 50;

    let events = matching_events
        .into_iter()
        .take(50)
        .map(|ev| project_event(&ev, cluster))
        .collect();

    let pod = if kind.eq_ignore_ascii_case("Pod") {
        match serde_json::from_value::<Pod>(val.clone()) {
            Ok(pod) => Some(build_pod_summary(&pod)),
            Err(err) => {
                tracing::warn!("failed to deserialize Pod for summary: {err}");
                None
            }
        }
    } else {
        None
    };

    let certificates =
        crate::cert::inspect_resource_certificates(client, kind, res_namespace.as_deref(), &val)
            .await;

    let (endpoints, endpoints_truncated) = crate::endpoints::resource_endpoints(kind, &val);

    Ok(ResourceDetail {
        uid,
        cluster: cluster.to_owned(),
        kind: kind.to_owned(),
        namespace: res_namespace,
        name: res_name,
        yaml,
        related,
        events,
        events_truncated,
        pod,
        certificates,
        endpoints,
        endpoints_truncated,
    })
}

/// Formats a Pod [`k8s_openapi::api::core::v1::Toleration`] into a readable string
/// such as `"key=value:effect"` or `"key:effect"`, omitting empty parts cleanly.
pub fn format_toleration(tol: &k8s_openapi::api::core::v1::Toleration) -> String {
    let key = tol.key.as_deref().filter(|s| !s.is_empty());
    let value = tol.value.as_deref().filter(|s| !s.is_empty());
    let effect = tol.effect.as_deref().filter(|s| !s.is_empty());

    let kv = match (key, value) {
        (Some(k), Some(v)) => format!("{k}={v}"),
        (Some(k), None) => k.to_string(),
        (None, Some(v)) => v.to_string(),
        (None, None) => String::new(),
    };

    match (kv.is_empty(), effect) {
        (false, Some(eff)) => format!("{kv}:{eff}"),
        (false, None) => kv,
        (true, Some(eff)) => format!(":{eff}"),
        (true, None) => tol.operator.clone().unwrap_or_default(),
    }
}

/// Builds a rich [`PodSummary`] for the Pod detail view drawer (`02-05a`).
pub fn build_pod_summary(pod: &Pod) -> PodSummary {
    let spec = pod.spec.as_ref();
    let status = pod.status.as_ref();

    let node = spec.and_then(|s| s.node_name.clone()).unwrap_or_default();
    let pod_ip = status.and_then(|s| s.pod_ip.clone()).unwrap_or_default();
    let qos_class = status.and_then(|s| s.qos_class.clone()).unwrap_or_default();
    let service_account = spec
        .and_then(|s| {
            s.service_account_name
                .clone()
                .or_else(|| s.service_account.clone())
        })
        .unwrap_or_default();
    let priority_class = spec.and_then(|s| s.priority_class_name.clone());

    let conditions: Vec<ConditionSummary> = status
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[])
        .iter()
        .map(|c| ConditionSummary {
            type_: c.type_.clone(),
            status: c.status.clone(),
            reason: c.reason.clone().filter(|s| !s.is_empty()),
            message: c.message.clone().filter(|s| !s.is_empty()),
            last_transition: c.last_transition_time.as_ref().map(|t| t.0.to_string()),
        })
        .collect();

    let tolerations: Vec<String> = spec
        .and_then(|s| s.tolerations.as_deref())
        .unwrap_or(&[])
        .iter()
        .map(format_toleration)
        .collect();

    let init_containers = spec
        .and_then(|s| s.init_containers.as_deref())
        .unwrap_or(&[])
        .iter()
        .map(|c| (c, true));
    let app_containers = spec
        .map(|s| s.containers.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|c| (c, false));

    let init_statuses = status
        .and_then(|s| s.init_container_statuses.as_deref())
        .unwrap_or(&[]);
    let app_statuses = status
        .and_then(|s| s.container_statuses.as_deref())
        .unwrap_or(&[]);

    let containers: Vec<ContainerSummary> = init_containers
        .chain(app_containers)
        .map(|(c, is_init)| {
            let container_status = if is_init {
                init_statuses.iter().find(|s| s.name == c.name)
            } else {
                app_statuses.iter().find(|s| s.name == c.name)
            };

            let (state, state_reason) = if let Some(cs) = container_status {
                if let Some(state) = &cs.state {
                    if state.running.is_some() {
                        ("Running".to_string(), None)
                    } else if let Some(w) = &state.waiting {
                        (
                            "Waiting".to_string(),
                            w.reason.clone().filter(|s| !s.is_empty()),
                        )
                    } else if let Some(t) = &state.terminated {
                        let reason = match t.reason.as_deref().filter(|s| !s.is_empty()) {
                            Some(r) => r.to_string(),
                            None => format!("exit {}", t.exit_code),
                        };
                        ("Terminated".to_string(), Some(reason))
                    } else {
                        ("Waiting".to_string(), None)
                    }
                } else {
                    ("Waiting".to_string(), None)
                }
            } else {
                ("Waiting".to_string(), None)
            };

            let ready = container_status.map(|s| s.ready).unwrap_or(false);
            let restarts = container_status.map(|s| s.restart_count).unwrap_or(0);
            let last_termination = container_status
                .and_then(|cs| cs.last_state.as_ref())
                .and_then(|ls| ls.terminated.as_ref())
                .map(|t| {
                    let code = t.exit_code;
                    match t.reason.as_deref().filter(|r| !r.is_empty()) {
                        Some(reason) => format!("{reason} ({code})"),
                        None => format!("exit {code}"),
                    }
                });

            let ports: Vec<ContainerPortSummary> = c
                .ports
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|p| ContainerPortSummary {
                    name: p.name.clone().filter(|s| !s.is_empty()),
                    container_port: p.container_port,
                    protocol: p.protocol.clone().filter(|s| !s.is_empty()),
                })
                .collect();

            let requests = ResourcePair {
                cpu: c
                    .resources
                    .as_ref()
                    .and_then(|r| r.requests.as_ref())
                    .and_then(|req| req.get("cpu"))
                    .map(|q| q.0.clone())
                    .filter(|s| !s.is_empty()),
                memory: c
                    .resources
                    .as_ref()
                    .and_then(|r| r.requests.as_ref())
                    .and_then(|req| req.get("memory"))
                    .map(|q| q.0.clone())
                    .filter(|s| !s.is_empty()),
            };

            let limits = ResourcePair {
                cpu: c
                    .resources
                    .as_ref()
                    .and_then(|r| r.limits.as_ref())
                    .and_then(|lim| lim.get("cpu"))
                    .map(|q| q.0.clone())
                    .filter(|s| !s.is_empty()),
                memory: c
                    .resources
                    .as_ref()
                    .and_then(|r| r.limits.as_ref())
                    .and_then(|lim| lim.get("memory"))
                    .map(|q| q.0.clone())
                    .filter(|s| !s.is_empty()),
            };

            ContainerSummary {
                name: c.name.clone(),
                image: c.image.clone().unwrap_or_default(),
                init: is_init,
                state,
                state_reason,
                ready,
                restarts,
                last_termination,
                ports,
                requests,
                limits,
            }
        })
        .collect();

    let mut uses = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let namespace = pod.metadata.namespace.clone();

    let mut add_ref = |kind: &str, name: &str| {
        if uses.len() >= 100 {
            return;
        }
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return;
        }
        if seen.insert((kind.to_owned(), trimmed.to_owned())) {
            uses.push(RelatedRef {
                kind: kind.to_owned(),
                name: trimmed.to_owned(),
                namespace: namespace.clone(),
                uid: String::new(),
                relationship: "uses".to_owned(),
            });
        }
    };

    if let Some(spec) = spec {
        // 1. ConfigMaps and Secrets from volumes
        if let Some(volumes) = &spec.volumes {
            for vol in volumes {
                if let Some(cm) = &vol.config_map {
                    add_ref("ConfigMap", &cm.name);
                }
                if let Some(sec) = &vol.secret
                    && let Some(name) = &sec.secret_name
                {
                    add_ref("Secret", name);
                }
                if let Some(proj) = &vol.projected
                    && let Some(sources) = &proj.sources
                {
                    for src in sources {
                        if let Some(cm) = &src.config_map {
                            add_ref("ConfigMap", &cm.name);
                        }
                        if let Some(sec) = &src.secret {
                            add_ref("Secret", &sec.name);
                        }
                    }
                }
            }
        }

        // 2 & 3. Containers env_from and env.value_from
        let all_containers = spec
            .init_containers
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .chain(spec.containers.iter());

        for c in all_containers {
            if let Some(env_from) = &c.env_from {
                for ef in env_from {
                    if let Some(cm) = &ef.config_map_ref {
                        add_ref("ConfigMap", &cm.name);
                    }
                    if let Some(sec) = &ef.secret_ref {
                        add_ref("Secret", &sec.name);
                    }
                }
            }
            if let Some(env) = &c.env {
                for ev in env {
                    if let Some(vf) = &ev.value_from {
                        if let Some(cm) = &vf.config_map_key_ref {
                            add_ref("ConfigMap", &cm.name);
                        }
                        if let Some(sec) = &vf.secret_key_ref {
                            add_ref("Secret", &sec.name);
                        }
                    }
                }
            }
        }

        // 4. PVCs from volumes
        if let Some(volumes) = &spec.volumes {
            for vol in volumes {
                if let Some(pvc) = &vol.persistent_volume_claim {
                    add_ref("PersistentVolumeClaim", &pvc.claim_name);
                }
            }
        }

        // 5. ServiceAccount from spec.service_account_name (if not default/empty)
        let sa = spec
            .service_account_name
            .as_deref()
            .or(spec.service_account.as_deref());
        if let Some(sa) = sa
            && !sa.is_empty()
            && sa != "default"
        {
            add_ref("ServiceAccount", sa);
        }
    }

    PodSummary {
        node,
        pod_ip,
        qos_class,
        service_account,
        priority_class,
        conditions,
        tolerations,
        containers,
        uses,
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
    use serde_json::json;

    use super::*;

    #[test]
    fn strip_managed_fields_removes_only_managed_fields() {
        let mut obj = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "api-pod",
                "namespace": "prod",
                "managedFields": [
                    { "manager": "kubelet", "operation": "Update" }
                ]
            },
            "spec": { "nodeName": "node-1" }
        });

        strip_managed_fields(&mut obj);

        assert!(obj["metadata"].get("managedFields").is_none());
        assert_eq!(obj["metadata"]["name"], "api-pod");
        assert_eq!(obj["spec"]["nodeName"], "node-1");
    }

    #[test]
    fn redact_secret_value_replaces_data_and_string_data() {
        let mut secret = json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": "my-secret", "namespace": "default" },
            "data": {
                "password": "supersecretpassword",
                "token": "tokendata"
            },
            "stringData": {
                "plain": "secretvalue"
            }
        });

        redact_secret_value(&mut secret);

        assert_eq!(secret["data"]["password"], "[REDACTED]");
        assert_eq!(secret["data"]["token"], "[REDACTED]");
        assert_eq!(secret["stringData"]["plain"], "[REDACTED]");
        assert_eq!(secret["metadata"]["name"], "my-secret");
    }

    #[test]
    fn secret_redaction_handles_plural_or_object_kind() {
        let mut secret = json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": "my-secret" },
            "data": { "key": "val" }
        });
        let is_secret = "secrets".eq_ignore_ascii_case("Secret")
            || "secrets".eq_ignore_ascii_case("secrets")
            || secret
                .get("kind")
                .and_then(|k| k.as_str())
                .map(|k| k.eq_ignore_ascii_case("Secret"))
                .unwrap_or(false);
        assert!(is_secret);
        if is_secret {
            redact_secret_value(&mut secret);
        }
        assert_eq!(secret["data"]["key"], "[REDACTED]");
    }

    #[test]
    fn extract_owner_refs_maps_owner_references() {
        let meta = ObjectMeta {
            name: Some("api-7f8b9-x12".into()),
            namespace: Some("payments".into()),
            owner_references: Some(vec![OwnerReference {
                api_version: "apps/v1".into(),
                kind: "ReplicaSet".into(),
                name: "api-7f8b9".into(),
                uid: "rs-uid-123".into(),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let refs = extract_owner_refs(&meta);
        assert_eq!(
            refs,
            vec![RelatedRef {
                kind: "ReplicaSet".into(),
                name: "api-7f8b9".into(),
                namespace: Some("payments".into()),
                uid: "rs-uid-123".into(),
                relationship: "owner".into(),
            }]
        );
    }

    #[test]
    fn compute_unified_diff_emits_added_removed_and_context_lines() {
        let before = "apiVersion: apps/v1\nkind: Deployment\nspec:\n  replicas: 2";
        let after = "apiVersion: apps/v1\nkind: Deployment\nspec:\n  replicas: 5";

        let diff = compute_unified_diff(before, after);
        let lines: Vec<&str> = diff.lines().collect();
        assert_eq!(
            lines,
            vec![
                " apiVersion: apps/v1",
                " kind: Deployment",
                " spec:",
                "-  replicas: 2",
                "+  replicas: 5",
            ]
        );
    }

    #[test]
    fn project_dynamic_object_projects_synthetic_crd_row() {
        let crd_json = json!({
            "apiVersion": "cert-manager.io/v1",
            "kind": "Certificate",
            "metadata": {
                "uid": "cert-uid-1",
                "name": "tls-ingress",
                "namespace": "ingress",
                "creationTimestamp": "2025-02-01T12:00:00Z",
                "labels": {
                    "env": "prod",
                    "app": "gateway"
                }
            },
            "spec": {
                "secretName": "tls-secret"
            },
            "status": {
                "phase": "Ready"
            }
        });

        let dyn_obj: DynamicObject = serde_json::from_value(crd_json).unwrap();
        let row = project_dynamic_object(&dyn_obj, "prod-eu", "Certificate");

        assert_eq!(row.uid, "cert-uid-1");
        assert_eq!(row.cluster, "prod-eu");
        assert_eq!(row.kind, "Certificate");
        assert_eq!(row.namespace.as_deref(), Some("ingress"));
        assert_eq!(row.name, "tls-ingress");
        assert_eq!(row.status, RowStatus::Healthy);
        assert_eq!(row.status_reason, "Ready");
        assert_eq!(
            row.labels,
            vec![
                ("app".to_owned(), "gateway".to_owned()),
                ("env".to_owned(), "prod".to_owned()),
            ]
        );
        assert_eq!(
            row.cells,
            vec![
                Cell::Status {
                    label: "Ready".to_owned(),
                    phase: RowStatus::Healthy,
                },
                Cell::Timestamp("2025-02-01T12:00:00Z".to_owned()),
            ]
        );
    }

    #[test]
    fn test_secret_data_encode_decode_roundtrip() {
        let mut original = std::collections::BTreeMap::new();
        original.insert("username".to_owned(), "admin".to_owned());
        original.insert("password".to_owned(), "s3cr3t!@#$%^&*()".to_owned());
        original.insert("multiline".to_owned(), "line1\nline2\nline3\n".to_owned());
        original.insert("unicode".to_owned(), "Hello 世界 🚀".to_owned());

        let encoded = encode_secret_data(&original);
        assert_eq!(encoded.len(), 4);
        assert_eq!(encoded.get("username").unwrap().0, b"admin");

        let decoded = decode_secret_data(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_secret_data_lossy_binary() {
        let mut data = std::collections::BTreeMap::new();
        // Invalid UTF-8 bytes: 0xFF, 0xFE
        data.insert(
            "binary".to_owned(),
            k8s_openapi::ByteString(vec![0xFF, 0xFE, b'a', b'b']),
        );

        let decoded = decode_secret_data(&data);
        let val = decoded.get("binary").unwrap();
        assert!(val.ends_with("ab"));
        assert!(val.contains('\u{FFFD}'));
    }

    #[test]
    fn secret_values_base64_roundtrip_and_errors() {
        let mut plain = std::collections::BTreeMap::new();
        plain.insert("username".to_owned(), "admin".to_owned());
        plain.insert("unicode".to_owned(), "Hello 世界".to_owned());
        plain.insert("empty".to_owned(), String::new());

        let encoded = encode_secret_values(&plain);
        assert_eq!(encoded["username"], "YWRtaW4=");
        assert_eq!(encoded["empty"], "");
        assert_eq!(decode_secret_values(&encoded).unwrap(), plain);

        let mut binary = std::collections::BTreeMap::new();
        binary.insert("bin".to_owned(), "//5hYg==".to_owned()); // 0xFF 0xFE 'a' 'b'
        let decoded = decode_secret_values(&binary).unwrap();
        assert!(decoded["bin"].contains('\u{FFFD}') && decoded["bin"].ends_with("ab"));

        let mut bad = std::collections::BTreeMap::new();
        bad.insert("ok".to_owned(), "YWRtaW4=".to_owned());
        bad.insert("broken".to_owned(), "not base64!".to_owned());
        let err = decode_secret_values(&bad).unwrap_err();
        assert!(matches!(&err, Error::InvalidBase64 { key } if key == "broken"));
        assert!(!err.to_string().contains("not base64!"));
    }

    #[test]
    fn normalize_for_comparison_strips_metadata_fields_and_status() {
        let mut val = json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "app-config",
                "namespace": "prod",
                "uid": "1111-2222-3333",
                "resourceVersion": "987654",
                "generation": 3,
                "creationTimestamp": "2025-01-01T12:00:00Z",
                "selfLink": "/api/v1/namespaces/prod/configmaps/app-config",
                "managedFields": [
                    { "manager": "kubectl", "operation": "Apply" }
                ],
                "labels": {
                    "env": "production"
                }
            },
            "data": {
                "DATABASE_URL": "postgres://db:5432"
            },
            "status": {
                "phase": "Active"
            }
        });

        normalize_for_comparison(&mut val);

        assert!(val.get("status").is_none());
        let meta = val.get("metadata").expect("metadata exists");
        assert!(meta.get("managedFields").is_none());
        assert!(meta.get("uid").is_none());
        assert!(meta.get("resourceVersion").is_none());
        assert!(meta.get("generation").is_none());
        assert!(meta.get("creationTimestamp").is_none());
        assert!(meta.get("selfLink").is_none());
        assert_eq!(meta["name"], "app-config");
        assert_eq!(meta["namespace"], "prod");
        assert_eq!(meta["labels"]["env"], "production");
        assert_eq!(val["data"]["DATABASE_URL"], "postgres://db:5432");
    }

    #[test]
    fn compare_resources_reports_identical_when_only_runtime_metadata_differs() {
        let cluster_a_yaml = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: payments-api
  namespace: prod
  uid: "cluster-a-uid-111"
  resourceVersion: "12345"
  generation: 2
  creationTimestamp: "2025-01-01T00:00:00Z"
  managedFields:
    - manager: argocd
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: payments
          image: payments:v2.1
status:
  availableReplicas: 3
"#;

        let cluster_b_yaml = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: payments-api
  namespace: prod
  uid: "cluster-b-uid-999"
  resourceVersion: "99999"
  generation: 5
  creationTimestamp: "2025-02-15T10:30:00Z"
  selfLink: "/apis/apps/v1/namespaces/prod/deployments/payments-api"
  managedFields:
    - manager: kubectl
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: payments
          image: payments:v2.1
status:
  availableReplicas: 1
"#;

        let result = compare_resources(cluster_a_yaml, cluster_b_yaml);
        assert!(result.identical);
        assert!(
            !result
                .unified_diff
                .lines()
                .any(|l| l.starts_with('+') || l.starts_with('-'))
        );
    }

    #[test]
    fn compare_resources_highlights_configuration_differences() {
        let staging_yaml = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: staging
  uid: "uid-staging"
spec:
  replicas: 1
"#;

        let prod_yaml = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  namespace: prod
  uid: "uid-prod"
spec:
  replicas: 5
"#;

        let result = compare_resources(staging_yaml, prod_yaml);
        assert!(!result.identical);
        assert!(result.unified_diff.contains("-  namespace: staging"));
        assert!(result.unified_diff.contains("+  namespace: prod"));
        assert!(result.unified_diff.contains("-  replicas: 1"));
        assert!(result.unified_diff.contains("+  replicas: 5"));
        assert!(!result.unified_diff.contains("uid"));
    }

    #[test]
    fn compare_resources_handles_json_input() {
        let json_left = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"cfg","uid":"1"},"data":{"k":"v1"}}"#;
        let json_right = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"cfg","uid":"2"},"data":{"k":"v2"}}"#;

        let result = compare_resources(json_left, json_right);
        assert!(!result.identical);
        assert!(result.unified_diff.contains("-  k: v1"));
        assert!(result.unified_diff.contains("+  k: v2"));
        assert!(!result.unified_diff.contains("uid"));
    }

    #[test]
    fn compare_resources_empty_inputs() {
        let res = compare_resources("", "");
        assert!(res.identical);
        assert!(res.unified_diff.is_empty());
    }

    #[test]
    fn compare_resources_options_metadata_status_and_hashes() {
        let y1 = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  uid: "uid-1"
  resourceVersion: "100"
  labels:
    app: web
    pod-template-hash: "abc123"
  annotations:
    deployment.kubernetes.io/revision: "1"
status:
  replicas: 1
"#;
        let y2 = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  uid: "uid-2"
  resourceVersion: "200"
  labels:
    app: web
    pod-template-hash: "def456"
  annotations:
    deployment.kubernetes.io/revision: "2"
status:
  replicas: 2
"#;

        // Default: stripped -> identical
        let def = compare_resources_with_options(y1, y2, CompareOptions::default());
        assert!(def.identical);

        // With include_metadata_and_status: true -> differences preserved
        let with_meta = compare_resources_with_options(
            y1,
            y2,
            CompareOptions {
                include_metadata_and_status: true,
                ..Default::default()
            },
        );
        assert!(!with_meta.identical);
        assert!(with_meta.unified_diff.contains("uid-1"));
        assert!(with_meta.unified_diff.contains("uid-2"));
        assert!(with_meta.unified_diff.contains("pod-template-hash"));
    }

    #[test]
    fn compare_resources_secret_redaction() {
        let sec1 = r#"
apiVersion: v1
kind: Secret
metadata:
  name: db-cred
data:
  password: "supersecretbase64"
"#;
        let sec2 = r#"
apiVersion: v1
kind: Secret
metadata:
  name: db-cred
data:
  password: "anothersecretbase64"
"#;

        // Default: redacted -> both "<redacted>" -> identical
        let redacted = compare_resources_with_options(sec1, sec2, CompareOptions::default());
        assert!(redacted.identical);
        assert!(!redacted.unified_diff.contains("supersecretbase64"));
        assert!(!redacted.unified_diff.contains("anothersecretbase64"));

        // With reveal_secrets: true -> diff shows actual values
        let revealed = compare_resources_with_options(
            sec1,
            sec2,
            CompareOptions {
                reveal_secrets: true,
                ..Default::default()
            },
        );
        assert!(!revealed.identical);
        assert!(revealed.unified_diff.contains("supersecretbase64"));
        assert!(revealed.unified_diff.contains("anothersecretbase64"));
    }

    #[test]
    fn compare_resources_only_differences_collapses_context() {
        let left = r#"
apiVersion: v1
kind: ConfigMap
metadata:
  name: big-cfg
data:
  k01: v
  k02: v
  k03: v
  k04: v
  k05: v
  k06: v
  k07: v
  k08: v
  k09: v
  k10: v_old
  k11: v
  k12: v
  k13: v
  k14: v
  k15: v
  k16: v
  k17: v
  k18: v
  k19: v
  k20: v
"#;
        let right = r#"
apiVersion: v1
kind: ConfigMap
metadata:
  name: big-cfg
data:
  k01: v
  k02: v
  k03: v
  k04: v
  k05: v
  k06: v
  k07: v
  k08: v
  k09: v
  k10: v_new
  k11: v
  k12: v
  k13: v
  k14: v
  k15: v
  k16: v
  k17: v
  k18: v
  k19: v
  k20: v
"#;

        let res = compare_resources_with_options(
            left,
            right,
            CompareOptions {
                only_differences: true,
                ..Default::default()
            },
        );
        assert!(!res.identical);
        assert!(res.unified_diff.contains("@@ ... @@"));
        assert!(res.unified_diff.contains("-  k10: v_old"));
        assert!(res.unified_diff.contains("+  k10: v_new"));
    }

    #[test]
    fn format_toleration_various_combinations() {
        use k8s_openapi::api::core::v1::Toleration;

        let t1 = Toleration {
            key: Some("app".to_string()),
            value: Some("web".to_string()),
            effect: Some("NoSchedule".to_string()),
            ..Default::default()
        };
        assert_eq!(format_toleration(&t1), "app=web:NoSchedule");

        let t2 = Toleration {
            key: Some("node.kubernetes.io/not-ready".to_string()),
            effect: Some("NoExecute".to_string()),
            ..Default::default()
        };
        assert_eq!(
            format_toleration(&t2),
            "node.kubernetes.io/not-ready:NoExecute"
        );

        let t3 = Toleration {
            key: Some("dedicated".to_string()),
            value: Some("gpu".to_string()),
            ..Default::default()
        };
        assert_eq!(format_toleration(&t3), "dedicated=gpu");

        let t4 = Toleration {
            key: Some("foo".to_string()),
            ..Default::default()
        };
        assert_eq!(format_toleration(&t4), "foo");

        let t5 = Toleration {
            effect: Some("NoSchedule".to_string()),
            ..Default::default()
        };
        assert_eq!(format_toleration(&t5), ":NoSchedule");

        let t6 = Toleration {
            operator: Some("Exists".to_string()),
            ..Default::default()
        };
        assert_eq!(format_toleration(&t6), "Exists");
    }

    #[test]
    fn build_pod_summary_web_fixture() {
        let pod_json = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "web-649df675cb-abcde",
                "namespace": "fg-demo",
                "uid": "uid-web-123"
            },
            "spec": {
                "nodeName": "kind-worker",
                "serviceAccountName": "web-sa",
                "priorityClassName": "high-priority",
                "tolerations": [
                    { "key": "dedicated", "value": "web", "effect": "NoSchedule" },
                    { "key": "node.kubernetes.io/not-ready", "operator": "Exists", "effect": "NoExecute" }
                ],
                "initContainers": [
                    {
                        "name": "init-db",
                        "image": "busybox:1.36",
                        "command": ["sh", "-c", "echo ready"]
                    }
                ],
                "containers": [
                    {
                        "name": "nginx",
                        "image": "nginx:1.27-alpine",
                        "ports": [
                            { "name": "http", "containerPort": 80, "protocol": "TCP" }
                        ],
                        "envFrom": [
                            { "configMapRef": { "name": "web-config" } },
                            { "secretRef": { "name": "web-env-secret" } }
                        ],
                        "env": [
                            {
                                "name": "WEB_USER",
                                "valueFrom": { "secretKeyRef": { "name": "web-secret", "key": "username" } }
                            },
                            {
                                "name": "API_URL",
                                "valueFrom": { "configMapKeyRef": { "name": "api-cm", "key": "url" } }
                            }
                        ],
                        "resources": {
                            "requests": { "cpu": "10m", "memory": "16Mi" },
                            "limits": { "cpu": "100m", "memory": "64Mi" }
                        }
                    }
                ],
                "volumes": [
                    {
                        "name": "html",
                        "configMap": { "name": "web-config" }
                    },
                    {
                        "name": "data",
                        "persistentVolumeClaim": { "claimName": "web-pvc" }
                    },
                    {
                        "name": "secret-vol",
                        "secret": { "secretName": "web-vol-secret" }
                    },
                    {
                        "name": "proj",
                        "projected": {
                            "sources": [
                                { "configMap": { "name": "proj-cm" } },
                                { "secret": { "name": "proj-sec" } }
                            ]
                        }
                    }
                ]
            },
            "status": {
                "podIP": "10.244.1.15",
                "qosClass": "Burstable",
                "conditions": [
                    { "type": "PodScheduled", "status": "True", "lastTransitionTime": "2025-01-15T10:00:00Z" },
                    { "type": "Initialized", "status": "True", "lastTransitionTime": "2025-01-15T10:00:05Z" },
                    { "type": "ContainersReady", "status": "True", "lastTransitionTime": "2025-01-15T10:00:10Z" },
                    { "type": "Ready", "status": "True", "lastTransitionTime": "2025-01-15T10:00:10Z" }
                ],
                "initContainerStatuses": [
                    {
                        "name": "init-db",
                        "ready": true,
                        "restartCount": 0,
                        "state": {
                            "terminated": { "exitCode": 0, "reason": "Completed" }
                        }
                    }
                ],
                "containerStatuses": [
                    {
                        "name": "nginx",
                        "ready": true,
                        "restartCount": 2,
                        "state": {
                            "running": { "startedAt": "2025-01-15T10:00:10Z" }
                        },
                        "lastState": {
                            "terminated": { "exitCode": 1, "reason": "Error" }
                        }
                    }
                ]
            }
        });

        let pod: Pod = serde_json::from_value(pod_json).unwrap();
        let summary = build_pod_summary(&pod);

        assert_eq!(summary.node, "kind-worker");
        assert_eq!(summary.pod_ip, "10.244.1.15");
        assert_eq!(summary.qos_class, "Burstable");
        assert_eq!(summary.service_account, "web-sa");
        assert_eq!(summary.priority_class.as_deref(), Some("high-priority"));

        assert_eq!(summary.conditions.len(), 4);
        assert_eq!(summary.conditions[0].type_, "PodScheduled");
        assert_eq!(summary.conditions[0].status, "True");
        assert_eq!(
            summary.conditions[0].last_transition.as_deref(),
            Some("2025-01-15T10:00:00Z")
        );
        assert_eq!(summary.conditions[3].type_, "Ready");

        assert_eq!(
            summary.tolerations,
            vec![
                "dedicated=web:NoSchedule",
                "node.kubernetes.io/not-ready:NoExecute"
            ]
        );

        assert_eq!(summary.containers.len(), 2);

        // Container 0: init container init-db
        let c0 = &summary.containers[0];
        assert_eq!(c0.name, "init-db");
        assert_eq!(c0.image, "busybox:1.36");
        assert!(c0.init);
        assert_eq!(c0.state, "Terminated");
        assert_eq!(c0.state_reason.as_deref(), Some("Completed"));
        assert!(c0.ready);
        assert_eq!(c0.restarts, 0);
        assert_eq!(c0.last_termination, None);

        // Container 1: app container nginx
        let c1 = &summary.containers[1];
        assert_eq!(c1.name, "nginx");
        assert_eq!(c1.image, "nginx:1.27-alpine");
        assert!(!c1.init);
        assert_eq!(c1.state, "Running");
        assert_eq!(c1.state_reason, None);
        assert!(c1.ready);
        assert_eq!(c1.restarts, 2);
        assert_eq!(c1.last_termination.as_deref(), Some("Error (1)"));

        assert_eq!(c1.ports.len(), 1);
        assert_eq!(c1.ports[0].name.as_deref(), Some("http"));
        assert_eq!(c1.ports[0].container_port, 80);
        assert_eq!(c1.ports[0].protocol.as_deref(), Some("TCP"));

        assert_eq!(c1.requests.cpu.as_deref(), Some("10m"));
        assert_eq!(c1.requests.memory.as_deref(), Some("16Mi"));
        assert_eq!(c1.limits.cpu.as_deref(), Some("100m"));
        assert_eq!(c1.limits.memory.as_deref(), Some("64Mi"));

        // uses references:
        // 1. Volumes: ConfigMap web-config, Secret web-vol-secret, ConfigMap proj-cm, Secret proj-sec
        // 2. envFrom: ConfigMap web-config (deduped), Secret web-env-secret
        // 3. env valueFrom: Secret web-secret, ConfigMap api-cm
        // 4. PVC: web-pvc
        // 5. ServiceAccount: web-sa
        let uses_pairs: Vec<(&str, &str)> = summary
            .uses
            .iter()
            .map(|r| (r.kind.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(
            uses_pairs,
            vec![
                ("ConfigMap", "web-config"),
                ("Secret", "web-vol-secret"),
                ("ConfigMap", "proj-cm"),
                ("Secret", "proj-sec"),
                ("Secret", "web-env-secret"),
                ("Secret", "web-secret"),
                ("ConfigMap", "api-cm"),
                ("PersistentVolumeClaim", "web-pvc"),
                ("ServiceAccount", "web-sa"),
            ]
        );

        for u in &summary.uses {
            assert_eq!(u.namespace.as_deref(), Some("fg-demo"));
            assert_eq!(u.relationship, "uses");
            assert_eq!(u.uid, "");
        }
    }

    #[test]
    fn build_pod_summary_waiting_crashloop_and_caps() {
        let pod_json = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "crashloop",
                "namespace": "default"
            },
            "spec": {
                "serviceAccountName": "default", // Should be omitted from uses!
                "containers": [
                    {
                        "name": "main",
                        "image": "busybox:1.36"
                    }
                ]
            },
            "status": {
                "containerStatuses": [
                    {
                        "name": "main",
                        "ready": false,
                        "restartCount": 5,
                        "state": {
                            "waiting": { "reason": "CrashLoopBackOff" }
                        },
                        "lastState": {
                            "terminated": { "exitCode": 137 } // No reason -> exit 137
                        }
                    }
                ]
            }
        });

        let pod: Pod = serde_json::from_value(pod_json).unwrap();
        let summary = build_pod_summary(&pod);

        assert_eq!(summary.service_account, "default");
        assert!(summary.uses.is_empty()); // "default" SA is skipped

        let c = &summary.containers[0];
        assert_eq!(c.state, "Waiting");
        assert_eq!(c.state_reason.as_deref(), Some("CrashLoopBackOff"));
        assert_eq!(c.last_termination.as_deref(), Some("exit 137"));
        assert!(!c.ready);
        assert_eq!(c.restarts, 5);
    }

    #[test]
    fn build_pod_summary_uses_cap_at_100() {
        let mut volumes = Vec::new();
        for i in 0..120 {
            volumes.push(json!({
                "name": format!("vol-{i}"),
                "configMap": { "name": format!("cm-{i}") }
            }));
        }

        let pod_json = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": { "name": "many-vols" },
            "spec": {
                "containers": [{ "name": "app", "image": "busybox" }],
                "volumes": volumes
            }
        });

        let pod: Pod = serde_json::from_value(pod_json).unwrap();
        let summary = build_pod_summary(&pod);
        assert_eq!(summary.uses.len(), 100);
    }
}
