//! Custom table columns evaluated with the kubectl custom-columns path subset (`P2-08`).
//!
//! Supported syntax (optionally wrapped in `{...}` and/or prefixed with `$`):
//! - dotted fields: `.metadata.name` or `metadata.name`;
//! - bracketed keys: `['kubernetes.io/ingress.class']` or `["..."]`;
//! - array index: `[0]`, negative from the end: `[-1]`;
//! - wildcard: `[*]` over arrays (elements) or objects (values); results are joined with `,`.
//!
//! Expressions are compiled once with [`compile`] and evaluated per object without
//! further allocation beyond the output string.

use serde_json::Value;

use crate::api::{Cell, ColumnDef};
use crate::error::{Error, Result};

/// Text shown for a missing value, matching `kubectl get -o custom-columns`.
pub const NONE_VALUE: &str = "<none>";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Field(String),
    Index(i64),
    Wildcard,
}

/// A parsed custom-column path, ready to evaluate against many objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPath {
    expr: String,
    segments: Vec<Segment>,
}

impl CompiledPath {
    /// The original expression text.
    pub fn expr(&self) -> &str {
        &self.expr
    }

    /// The top-level object field this path starts with (e.g. `"metadata"`), if any.
    /// Lets typed projections serialize only the part of the object a column reads.
    pub fn root_field(&self) -> Option<&str> {
        match self.segments.first() {
            Some(Segment::Field(name)) => Some(name),
            _ => None,
        }
    }

    /// Evaluates the path over `obj`. Returns `None` when nothing (or only `null`) matched.
    pub fn evaluate(&self, obj: &Value) -> Option<String> {
        let mut out = String::new();
        let mut count = 0usize;
        walk(obj, &self.segments, &mut out, &mut count);
        (count > 0).then_some(out)
    }
}

fn walk(value: &Value, segments: &[Segment], out: &mut String, count: &mut usize) {
    let Some((first, rest)) = segments.split_first() else {
        render(value, out, count);
        return;
    };
    match first {
        Segment::Field(name) => {
            if let Some(next) = value.as_object().and_then(|map| map.get(name)) {
                walk(next, rest, out, count);
            }
        }
        Segment::Index(index) => {
            if let Some(items) = value.as_array() {
                let len = items.len() as i64;
                let resolved = if *index < 0 { len + index } else { *index };
                if (0..len).contains(&resolved)
                    && let Some(next) = items.get(resolved as usize)
                {
                    walk(next, rest, out, count);
                }
            }
        }
        Segment::Wildcard => match value {
            Value::Array(items) => {
                for item in items {
                    walk(item, rest, out, count);
                }
            }
            Value::Object(map) => {
                for item in map.values() {
                    walk(item, rest, out, count);
                }
            }
            _ => {}
        },
    }
}

fn render(value: &Value, out: &mut String, count: &mut usize) {
    use std::fmt::Write as _;

    let start = out.len();
    if *count > 0 {
        out.push(',');
    }
    let ok = match value {
        Value::Null => false,
        Value::String(s) => {
            out.push_str(s);
            true
        }
        Value::Bool(b) => write!(out, "{b}").is_ok(),
        Value::Number(n) => write!(out, "{n}").is_ok(),
        Value::Array(_) | Value::Object(_) => match serde_json::to_string(value) {
            Ok(json) => {
                out.push_str(&json);
                true
            }
            Err(_) => false,
        },
    };
    if ok {
        *count += 1;
    } else {
        out.truncate(start);
    }
}

fn invalid(expr: &str, reason: impl Into<String>) -> Error {
    Error::InvalidColumnPath {
        expr: expr.to_owned(),
        reason: reason.into(),
    }
}

/// Parses a kubectl custom-columns path expression.
pub fn compile(expr: &str) -> Result<CompiledPath> {
    let mut body = expr.trim();
    if let Some(inner) = body.strip_prefix('{') {
        body = inner
            .strip_suffix('}')
            .ok_or_else(|| invalid(expr, "unclosed '{'"))?
            .trim();
    }
    if let Some(inner) = body.strip_prefix('$') {
        body = inner;
    }
    if body.is_empty() || body == "." {
        return Err(invalid(expr, "expression is empty"));
    }

    let chars: Vec<char> = body.chars().collect();
    let mut segments = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '.' => {
                i += 1;
                let (name, next) = read_field(&chars, i);
                if name.is_empty() {
                    // `.[0]` is tolerated; `..` or a trailing `.` are not.
                    if chars.get(next) != Some(&'[') {
                        return Err(invalid(expr, format!("empty field name at position {i}")));
                    }
                } else {
                    segments.push(Segment::Field(name));
                }
                i = next;
            }
            '[' => {
                let (segment, next) = read_bracket(expr, &chars, i + 1)?;
                segments.push(segment);
                i = next;
            }
            ']' => return Err(invalid(expr, format!("unexpected ']' at position {i}"))),
            _ if i == 0 => {
                let (name, next) = read_field(&chars, i);
                segments.push(Segment::Field(name));
                i = next;
            }
            c => {
                return Err(invalid(expr, format!("unexpected '{c}' at position {i}")));
            }
        }
    }
    if segments.is_empty() {
        return Err(invalid(expr, "expression is empty"));
    }
    Ok(CompiledPath {
        expr: expr.to_owned(),
        segments,
    })
}

/// Reads a bare field name starting at `start`; stops at `.`, `[` or `]`.
fn read_field(chars: &[char], start: usize) -> (String, usize) {
    let mut end = start;
    while end < chars.len() && !matches!(chars[end], '.' | '[' | ']') {
        end += 1;
    }
    (chars[start..end].iter().collect(), end)
}

/// Reads the inside of `[...]` starting just after `[`; returns the index after `]`.
fn read_bracket(expr: &str, chars: &[char], start: usize) -> Result<(Segment, usize)> {
    match chars.get(start) {
        None => Err(invalid(expr, "unclosed '['")),
        Some(&quote @ ('\'' | '"')) => {
            let key_start = start + 1;
            let mut end = key_start;
            while end < chars.len() && chars[end] != quote {
                end += 1;
            }
            if end >= chars.len() {
                return Err(invalid(expr, format!("unclosed {quote} quote")));
            }
            if chars.get(end + 1) != Some(&']') {
                return Err(invalid(expr, "expected ']' after quoted key"));
            }
            let key: String = chars[key_start..end].iter().collect();
            if key.is_empty() {
                return Err(invalid(expr, "empty quoted key"));
            }
            Ok((Segment::Field(key), end + 2))
        }
        Some(_) => {
            let mut end = start;
            while end < chars.len() && chars[end] != ']' {
                end += 1;
            }
            if end >= chars.len() {
                return Err(invalid(expr, "unclosed '['"));
            }
            let inner: String = chars[start..end].iter().collect();
            let inner = inner.trim();
            let segment = if inner == "*" {
                Segment::Wildcard
            } else {
                inner.parse::<i64>().map(Segment::Index).map_err(|_| {
                    invalid(
                        expr,
                        format!("'[{inner}]' is not an index, '*' or a quoted key"),
                    )
                })?
            };
            Ok((segment, end + 1))
        }
    }
}

/// Evaluates every compiled column over `obj` into text cells (`<none>` when missing).
pub fn evaluate_custom_cells(obj: &Value, cols: &[CompiledPath]) -> Vec<Cell> {
    cols.iter()
        .map(|col| Cell::Text(col.evaluate(obj).unwrap_or_else(|| NONE_VALUE.to_owned())))
        .collect()
}

/// The compiled custom columns of one resource kind, in layout order.
///
/// `Row::custom[i]` holds the value of `defs()[i]` / `paths()[i]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomColumns {
    defs: Vec<ColumnDef>,
    paths: Vec<CompiledPath>,
}

impl CustomColumns {
    /// Compiles every `json_path`; the first invalid one is returned as
    /// [`Error::InvalidColumnPath`].
    pub fn compile(defs: Vec<ColumnDef>) -> Result<Self> {
        let paths = defs
            .iter()
            .map(|def| compile(&def.json_path))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { defs, paths })
    }

    /// Column definitions in layout order.
    pub fn defs(&self) -> &[ColumnDef] {
        &self.defs
    }

    /// Compiled paths, index-aligned with [`Self::defs`].
    pub fn paths(&self) -> &[CompiledPath] {
        &self.paths
    }

    /// `true` when the kind has no custom columns.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Index of the column evaluating `expr`, if any.
    pub fn position_of_expr(&self, expr: &str) -> Option<usize> {
        self.paths.iter().position(|p| p.expr() == expr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    fn pod() -> Value {
        json!({
            "metadata": {
                "name": "web-0",
                "namespace": "prod",
                "annotations": {
                    "kubernetes.io/ingress.class": "nginx",
                    "fleetglass.dev/owner": "team-a"
                },
                "labels": { "app": "web" }
            },
            "spec": {
                "nodeName": "node-1",
                "containers": [
                    { "name": "app", "image": "nginx:1.27", "ports": [{ "containerPort": 80 }] },
                    { "name": "sidecar", "image": "envoy:1.30" }
                ]
            },
            "status": {
                "phase": "Running",
                "hostNetwork": false,
                "containerStatuses": [{ "restartCount": 3, "ready": true }]
            }
        })
    }

    fn eval(expr: &str) -> Option<String> {
        compile(expr).unwrap().evaluate(&pod())
    }

    #[test]
    fn nested_fields_with_and_without_leading_dot() {
        assert_eq!(eval(".metadata.name").as_deref(), Some("web-0"));
        assert_eq!(eval("metadata.namespace").as_deref(), Some("prod"));
        assert_eq!(eval("{.spec.nodeName}").as_deref(), Some("node-1"));
        assert_eq!(eval("$.status.phase").as_deref(), Some("Running"));
    }

    #[test]
    fn scalars_and_compound_values_render_like_kubectl() {
        assert_eq!(
            eval(".status.containerStatuses[0].restartCount").as_deref(),
            Some("3")
        );
        assert_eq!(eval(".status.hostNetwork").as_deref(), Some("false"));
        assert_eq!(
            eval(".metadata.labels").as_deref(),
            Some(r#"{"app":"web"}"#)
        );
        assert_eq!(
            eval(".spec.containers[0].ports").as_deref(),
            Some(r#"[{"containerPort":80}]"#)
        );
    }

    #[test]
    fn bracketed_annotation_keys_with_dots_and_slashes() {
        assert_eq!(
            eval(".metadata.annotations['kubernetes.io/ingress.class']").as_deref(),
            Some("nginx")
        );
        assert_eq!(
            eval(r#"metadata.annotations["fleetglass.dev/owner"]"#).as_deref(),
            Some("team-a")
        );
    }

    #[test]
    fn array_index_and_negative_index() {
        assert_eq!(
            eval(".spec.containers[0].image").as_deref(),
            Some("nginx:1.27")
        );
        assert_eq!(eval(".spec.containers[1].name").as_deref(), Some("sidecar"));
        assert_eq!(
            eval(".spec.containers[-1].name").as_deref(),
            Some("sidecar")
        );
        assert_eq!(eval(".spec.containers[5].name"), None);
    }

    #[test]
    fn wildcard_joins_with_comma() {
        assert_eq!(
            eval(".spec.containers[*].name").as_deref(),
            Some("app,sidecar")
        );
        assert_eq!(
            eval(".spec.containers[*].image").as_deref(),
            Some("nginx:1.27,envoy:1.30")
        );
        // Elements lacking the field are skipped, like kubectl.
        assert_eq!(
            eval(".spec.containers[*].ports[0].containerPort").as_deref(),
            Some("80")
        );
    }

    #[test]
    fn missing_values_become_none_cells() {
        let cols = vec![
            compile(".metadata.name").unwrap(),
            compile(".metadata.annotations['missing/key']").unwrap(),
            compile(".spec.missing[*].name").unwrap(),
        ];
        assert_eq!(
            evaluate_custom_cells(&pod(), &cols),
            vec![
                Cell::Text("web-0".to_owned()),
                Cell::Text(NONE_VALUE.to_owned()),
                Cell::Text(NONE_VALUE.to_owned()),
            ]
        );
        assert_eq!(compile(".a").unwrap().evaluate(&json!({ "a": null })), None);
    }

    #[test]
    fn invalid_expressions_are_typed_errors() {
        for expr in [
            "",
            "   ",
            ".",
            "{}",
            ".metadata.annotations['kubernetes.io/ingress.class'",
            ".metadata.annotations['unterminated",
            ".spec.containers[0",
            ".spec.containers[",
            ".spec.containers[abc]",
            ".metadata..name",
            ".metadata.",
            ".metadata]",
            "{.metadata.name",
        ] {
            match compile(expr) {
                Err(Error::InvalidColumnPath { expr: e, reason }) => {
                    assert_eq!(e, expr);
                    assert!(!reason.is_empty());
                }
                other => panic!("expected InvalidColumnPath for {expr:?}, got {other:?}"),
            }
        }
    }

    fn synthetic_pod(i: usize) -> Value {
        json!({
            "metadata": {
                "name": format!("pod-{i}"),
                "namespace": format!("ns-{}", i % 50),
                "uid": format!("uid-{i}"),
                "labels": { "app": format!("app-{}", i % 100), "tier": "backend" },
                "annotations": {
                    "kubernetes.io/ingress.class": "nginx",
                    "prometheus.io/scrape": "true"
                }
            },
            "spec": {
                "nodeName": format!("node-{}", i % 200),
                "containers": [
                    { "name": "app", "image": format!("registry.local/app:{}", i % 10) },
                    { "name": "sidecar", "image": "envoy:1.30" }
                ]
            },
            "status": {
                "phase": "Running",
                "podIP": format!("10.0.{}.{}", (i / 256) % 256, i % 256),
                "containerStatuses": [{ "restartCount": i % 7, "ready": true }]
            }
        })
    }

    #[test]
    fn perf_three_columns_over_50k_pods() {
        let pods: Vec<Value> = (0..50_000).map(synthetic_pod).collect();
        let cols = vec![
            compile(".metadata.annotations['kubernetes.io/ingress.class']").unwrap(),
            compile(".spec.containers[*].image").unwrap(),
            compile(".status.containerStatuses[0].restartCount").unwrap(),
        ];

        let start = Instant::now();
        let mut none = 0usize;
        for pod in &pods {
            let cells = evaluate_custom_cells(pod, &cols);
            none += cells
                .iter()
                .filter(|c| **c == Cell::Text(NONE_VALUE.to_owned()))
                .count();
        }
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        println!("custom columns 3x50k elapsed: {elapsed_ms:.2} ms");

        assert_eq!(none, 0);
        let budget_ms = if cfg!(not(debug_assertions)) {
            200.0
        } else {
            2000.0
        };
        assert!(
            elapsed_ms < budget_ms,
            "3 custom columns over 50k pods took {elapsed_ms:.2} ms, exceeding {budget_ms:.0} ms"
        );
    }
}
