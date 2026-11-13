//! Structured boolean, field qualifier, exact phrase, and regex filter parser and matcher (`04-06`).
//!
//! Upgrades the search query engine from plain substring matching to a bounded AST supporting:
//! - Boolean operators: `AND` (or whitespace), `OR`, `NOT` / `-` / `!`, and grouping parentheses `(...)`.
//! - Exact phrases: `"..."` (continuous case-insensitive substring).
//! - Bounded regular expressions: `/.../` (compiled with ReDoS size and DFA limits).
//! - Field qualifiers: `name:`, `ns:` / `namespace:`, `status:`, `node:`, `label:`, `image:`, `ip:`.
//! - Plain-text queries without operators work identically to previous versions.

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::api::{Cell, Row, RowStatus};
use crate::metrics::UNMEASURED;
use crate::query::contains_ignore_ascii_case;

const REGEX_SIZE_LIMIT: usize = 64 * 1024; // 64 KB
const REGEX_DFA_SIZE_LIMIT: usize = 128 * 1024; // 128 KB
const MAX_PARSE_DEPTH: usize = 32;

/// Parse error with an informative message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FilterError {
    pub message: String,
}

impl FilterError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FilterError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldTarget {
    Any,
    Name,
    Namespace,
    Status,
    Node,
    Label,
    Image,
    Ip,
    Is,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    /// Case-insensitive substring match.
    Substring(String),
    /// Case-insensitive exact phrase match (continuous substring preserving inner spaces).
    Exact(String),
    /// Bounded compiled regular expression.
    Regex(Regex),
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Pattern::Substring(a), Pattern::Substring(b)) => a == b,
            (Pattern::Exact(a), Pattern::Exact(b)) => a == b,
            (Pattern::Regex(a), Pattern::Regex(b)) => a.as_str() == b.as_str(),
            _ => false,
        }
    }
}

impl Pattern {
    pub fn matches(&self, haystack: &str) -> bool {
        match self {
            Pattern::Substring(needle) => contains_ignore_ascii_case(haystack, needle),
            Pattern::Exact(phrase) => contains_ignore_ascii_case(haystack, phrase),
            Pattern::Regex(re) => re.is_match(haystack),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TermMatch {
    pub target: FieldTarget,
    pub pattern: Pattern,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterAst {
    And(Vec<FilterAst>),
    Or(Vec<FilterAst>),
    Not(Box<FilterAst>),
    Term(TermMatch),
}

impl FilterAst {
    pub fn matches(&self, row: &Row) -> bool {
        match self {
            FilterAst::Term(term) => matches_term(term, row),
            FilterAst::And(children) => children.iter().all(|c| c.matches(row)),
            FilterAst::Or(children) => children.iter().any(|c| c.matches(row)),
            FilterAst::Not(child) => !child.matches(row),
        }
    }
}

fn row_status_str(status: RowStatus) -> &'static str {
    match status {
        RowStatus::Healthy => "healthy",
        RowStatus::Pending => "pending",
        RowStatus::Degraded => "degraded",
        RowStatus::Failed => "failed",
        RowStatus::Completed => "completed",
        RowStatus::Terminating => "terminating",
        RowStatus::Unknown => "unknown",
    }
}

fn matches_term(term: &TermMatch, row: &Row) -> bool {
    match term.target {
        FieldTarget::Name => term.pattern.matches(&row.name),
        FieldTarget::Namespace => row
            .namespace
            .as_deref()
            .is_some_and(|ns| term.pattern.matches(ns)),
        FieldTarget::Status => {
            term.pattern.matches(row_status_str(row.status))
                || term.pattern.matches(&row.status_reason)
        }
        FieldTarget::Node => {
            if row.kind.eq_ignore_ascii_case("Node") && term.pattern.matches(&row.name) {
                return true;
            }
            if let Some(Cell::Text(node)) = row.cells.get(5)
                && term.pattern.matches(node)
            {
                return true;
            }
            row.cells.iter().any(|c| match c {
                Cell::Text(t) => term.pattern.matches(t),
                _ => false,
            })
        }
        FieldTarget::Label => row.labels.iter().any(|(k, v)| {
            term.pattern.matches(&format!("{k}={v}"))
                || term.pattern.matches(k)
                || term.pattern.matches(v)
        }),
        FieldTarget::Image | FieldTarget::Ip => row.cells.iter().any(|c| match c {
            Cell::Text(t) => term.pattern.matches(t),
            _ => false,
        }),
        FieldTarget::Is => {
            let pat = match &term.pattern {
                Pattern::Substring(s) | Pattern::Exact(s) => s.to_ascii_lowercase(),
                Pattern::Regex(r) => r.as_str().to_ascii_lowercase(),
            };
            match pat.as_str() {
                "near-limit" | "nearlimit" => has_near_limit(row),
                "over-request" | "overrequest" => has_over_request(row),
                "unused" | "over-provisioned" | "overprovisioned" => has_unused_request(row, 20),
                "missing-limits" | "missing-resources" | "nolimits" => has_missing_resources(row),
                _ => false,
            }
        }
        FieldTarget::Any => {
            if term.pattern.matches(&row.name) {
                return true;
            }
            if row
                .namespace
                .as_deref()
                .is_some_and(|ns| term.pattern.matches(ns))
            {
                return true;
            }
            if term.pattern.matches(&row.status_reason) {
                return true;
            }
            if term.pattern.matches(row_status_str(row.status)) {
                return true;
            }
            row.cells.iter().any(|c| match c {
                Cell::Text(t) => term.pattern.matches(t),
                _ => false,
            })
        }
    }
}

fn has_near_limit(row: &Row) -> bool {
    row.cells.iter().any(|c| match c {
        Cell::Usage(u) => u.limit.is_some_and(|lim| {
            lim > 0 && u.value != UNMEASURED && (u64::from(u.value) * 100 / u64::from(lim) >= 80)
        }),
        _ => false,
    })
}

fn has_over_request(row: &Row) -> bool {
    row.cells.iter().any(|c| match c {
        Cell::Usage(u) => u
            .request
            .is_some_and(|req| req > 0 && u.value != UNMEASURED && u.value > req),
        _ => false,
    })
}

fn has_unused_request(row: &Row, threshold_pct: u8) -> bool {
    row.cells.iter().any(|c| match c {
        Cell::Usage(u) => u.request.is_some_and(|req| {
            req > 0
                && u.value != UNMEASURED
                && (u64::from(u.value) * 100 / u64::from(req) < u64::from(threshold_pct))
        }),
        _ => false,
    })
}

fn has_missing_resources(row: &Row) -> bool {
    if !row.kind.eq_ignore_ascii_case("Pod") || row.status == RowStatus::Completed {
        return false;
    }
    row.cells.iter().any(|c| match c {
        Cell::Usage(u) => u.limit.is_none() || u.request.is_none(),
        _ => false,
    })
}

/// Parsed filter ready for fast execution against rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    ast: FilterAst,
}

impl Filter {
    /// Parses a filter string into an executable [`Filter`].
    pub fn parse(query: &str) -> Result<Self, FilterError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(FilterError::new("Empty query"));
        }
        let tokens = tokenize(trimmed)?;
        if tokens.is_empty() {
            return Err(FilterError::new("Empty query"));
        }
        let mut pos = 0;
        let ast = parse_expr(&tokens, &mut pos, 0)?;
        if pos < tokens.len() {
            return Err(FilterError::new("Unexpected token at end of query"));
        }
        Ok(Filter { ast })
    }

    /// Evaluates the filter against a single [`Row`].
    pub fn matches(&self, row: &Row) -> bool {
        self.ast.matches(row)
    }
}

/// Validates whether a query string parses as a valid filter.
pub fn validate_filter(query: &str) -> Result<(), String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    Filter::parse(trimmed).map(|_| ()).map_err(|e| e.message)
}

// ---------------------------------------------------------------------------
// Lexer / Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum PatternToken {
    Word(String),
    Exact(String),
    Regex(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Term {
        target: FieldTarget,
        pattern: PatternToken,
    },
}

fn tokenize(input: &str) -> Result<Vec<Token>, FilterError> {
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let (_, c) = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if c == '(' {
            tokens.push(Token::LParen);
            i += 1;
            continue;
        }

        if c == ')' {
            tokens.push(Token::RParen);
            i += 1;
            continue;
        }

        if c == '!' {
            tokens.push(Token::Not);
            i += 1;
            continue;
        }

        // Check for leading negation `-` (not preceded by alphanumeric, followed by non-whitespace)
        if c == '-'
            && i + 1 < chars.len()
            && !chars[i + 1].1.is_whitespace()
            && chars[i + 1].1 != ')'
        {
            tokens.push(Token::Not);
            i += 1;
            continue;
        }

        // Check for field qualifier prefixes
        let remaining_str = &input[chars[i].0..];
        let qualifier = match_qualifier_prefix(remaining_str);

        if let Some((target, prefix_len)) = qualifier {
            i += prefix_len;
            if i >= chars.len() {
                return Err(FilterError::new("Missing value after field qualifier"));
            }
            let pattern = read_pattern(input, &chars, &mut i)?;
            tokens.push(Token::Term { target, pattern });
            continue;
        }

        // Check for standalone pattern (quoted string or regex or word)
        if c == '"' {
            let pattern = read_exact_phrase(input, &chars, &mut i)?;
            tokens.push(Token::Term {
                target: FieldTarget::Any,
                pattern,
            });
            continue;
        }

        if c == '/' {
            let pattern = read_regex(input, &chars, &mut i)?;
            tokens.push(Token::Term {
                target: FieldTarget::Any,
                pattern,
            });
            continue;
        }

        // Read unquoted word
        let start_byte = chars[i].0;
        while i < chars.len() {
            let (_, ch) = chars[i];
            if ch.is_whitespace() || ch == '(' || ch == ')' {
                break;
            }
            i += 1;
        }
        let end_byte = if i < chars.len() {
            chars[i].0
        } else {
            input.len()
        };
        let word = &input[start_byte..end_byte];

        if word.eq_ignore_ascii_case("AND") {
            tokens.push(Token::And);
        } else if word.eq_ignore_ascii_case("OR") {
            tokens.push(Token::Or);
        } else if word.eq_ignore_ascii_case("NOT") {
            tokens.push(Token::Not);
        } else {
            tokens.push(Token::Term {
                target: FieldTarget::Any,
                pattern: PatternToken::Word(word.to_owned()),
            });
        }
    }

    Ok(tokens)
}

fn match_qualifier_prefix(s: &str) -> Option<(FieldTarget, usize)> {
    let lower = s.to_ascii_lowercase();
    let qualifiers = [
        ("name:", FieldTarget::Name),
        ("namespace:", FieldTarget::Namespace),
        ("ns:", FieldTarget::Namespace),
        ("status:", FieldTarget::Status),
        ("node:", FieldTarget::Node),
        ("label:", FieldTarget::Label),
        ("image:", FieldTarget::Image),
        ("ip:", FieldTarget::Ip),
        ("is:", FieldTarget::Is),
    ];
    for (prefix, target) in qualifiers {
        if lower.starts_with(prefix) {
            let prefix_char_len = prefix.chars().count();
            return Some((target, prefix_char_len));
        }
    }
    None
}

fn read_pattern(
    input: &str,
    chars: &[(usize, char)],
    i: &mut usize,
) -> Result<PatternToken, FilterError> {
    if *i >= chars.len() {
        return Err(FilterError::new("Missing value for field qualifier"));
    }
    let (_, c) = chars[*i];
    if c == '"' {
        read_exact_phrase(input, chars, i)
    } else if c == '/' {
        read_regex(input, chars, i)
    } else {
        let start_byte = chars[*i].0;
        while *i < chars.len() {
            let (_, ch) = chars[*i];
            if ch.is_whitespace() || ch == '(' || ch == ')' {
                break;
            }
            *i += 1;
        }
        let end_byte = if *i < chars.len() {
            chars[*i].0
        } else {
            input.len()
        };
        let word = input[start_byte..end_byte].to_owned();
        if word.is_empty() {
            return Err(FilterError::new("Missing value for field qualifier"));
        }
        Ok(PatternToken::Word(word))
    }
}

fn read_exact_phrase(
    input: &str,
    chars: &[(usize, char)],
    i: &mut usize,
) -> Result<PatternToken, FilterError> {
    // Consume opening quote
    *i += 1;
    let start_byte = if *i < chars.len() {
        chars[*i].0
    } else {
        return Err(FilterError::new("Unclosed quote in search query"));
    };

    let mut closed = false;
    let mut end_byte = start_byte;

    while *i < chars.len() {
        let (byte_idx, ch) = chars[*i];
        if ch == '"' {
            closed = true;
            end_byte = byte_idx;
            *i += 1;
            break;
        }
        *i += 1;
    }

    if !closed {
        return Err(FilterError::new("Unclosed quote in search query"));
    }

    Ok(PatternToken::Exact(input[start_byte..end_byte].to_owned()))
}

fn read_regex(
    input: &str,
    chars: &[(usize, char)],
    i: &mut usize,
) -> Result<PatternToken, FilterError> {
    // Consume opening slash
    *i += 1;
    let start_byte = if *i < chars.len() {
        chars[*i].0
    } else {
        return Err(FilterError::new(
            "Unclosed regular expression in search query",
        ));
    };

    let mut closed = false;
    let mut end_byte = start_byte;
    let mut escaped = false;

    while *i < chars.len() {
        let (byte_idx, ch) = chars[*i];
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '/' {
            closed = true;
            end_byte = byte_idx;
            *i += 1;
            break;
        }
        *i += 1;
    }

    if !closed {
        return Err(FilterError::new(
            "Unclosed regular expression in search query",
        ));
    }

    Ok(PatternToken::Regex(input[start_byte..end_byte].to_owned()))
}

// ---------------------------------------------------------------------------
// Recursive Descent Parser
// ---------------------------------------------------------------------------

fn can_start_expr(token: &Token) -> bool {
    matches!(token, Token::LParen | Token::Not | Token::Term { .. })
}

fn parse_expr(tokens: &[Token], pos: &mut usize, depth: usize) -> Result<FilterAst, FilterError> {
    if depth > MAX_PARSE_DEPTH {
        return Err(FilterError::new(
            "Query expression nesting depth exceeded limit",
        ));
    }
    let mut left = parse_and_expr(tokens, pos, depth + 1)?;

    while *pos < tokens.len() && tokens[*pos] == Token::Or {
        *pos += 1;
        let right = parse_and_expr(tokens, pos, depth + 1)?;
        left = match left {
            FilterAst::Or(mut list) => {
                list.push(right);
                FilterAst::Or(list)
            }
            other => FilterAst::Or(vec![other, right]),
        };
    }

    Ok(left)
}

fn parse_and_expr(
    tokens: &[Token],
    pos: &mut usize,
    depth: usize,
) -> Result<FilterAst, FilterError> {
    let mut left = parse_unary_expr(tokens, pos, depth + 1)?;

    while *pos < tokens.len() {
        if tokens[*pos] == Token::And {
            *pos += 1;
            let right = parse_unary_expr(tokens, pos, depth + 1)?;
            left = match left {
                FilterAst::And(mut list) => {
                    list.push(right);
                    FilterAst::And(list)
                }
                other => FilterAst::And(vec![other, right]),
            };
        } else if can_start_expr(&tokens[*pos]) {
            // Implicit AND between adjacent terms/groups
            let right = parse_unary_expr(tokens, pos, depth + 1)?;
            left = match left {
                FilterAst::And(mut list) => {
                    list.push(right);
                    FilterAst::And(list)
                }
                other => FilterAst::And(vec![other, right]),
            };
        } else {
            break;
        }
    }

    Ok(left)
}

fn parse_unary_expr(
    tokens: &[Token],
    pos: &mut usize,
    depth: usize,
) -> Result<FilterAst, FilterError> {
    if *pos < tokens.len() && tokens[*pos] == Token::Not {
        *pos += 1;
        let inner = parse_unary_expr(tokens, pos, depth + 1)?;
        Ok(FilterAst::Not(Box::new(inner)))
    } else {
        parse_primary_expr(tokens, pos, depth + 1)
    }
}

fn parse_primary_expr(
    tokens: &[Token],
    pos: &mut usize,
    depth: usize,
) -> Result<FilterAst, FilterError> {
    if *pos >= tokens.len() {
        return Err(FilterError::new("Unexpected end of query"));
    }

    match &tokens[*pos] {
        Token::LParen => {
            *pos += 1;
            let expr = parse_expr(tokens, pos, depth + 1)?;
            if *pos >= tokens.len() || tokens[*pos] != Token::RParen {
                return Err(FilterError::new("Unclosed parenthesis"));
            }
            *pos += 1;
            Ok(expr)
        }
        Token::Term { target, pattern } => {
            *pos += 1;
            let compiled = compile_pattern(pattern)?;
            Ok(FilterAst::Term(TermMatch {
                target: *target,
                pattern: compiled,
            }))
        }
        Token::RParen => Err(FilterError::new("Unexpected closing parenthesis")),
        Token::And | Token::Or => Err(FilterError::new("Unexpected boolean operator")),
        Token::Not => Err(FilterError::new("Unexpected negation operator")),
    }
}

fn compile_pattern(token: &PatternToken) -> Result<Pattern, FilterError> {
    match token {
        PatternToken::Word(s) => Ok(Pattern::Substring(s.to_ascii_lowercase())),
        PatternToken::Exact(s) => Ok(Pattern::Exact(s.to_ascii_lowercase())),
        PatternToken::Regex(r) => {
            let compiled = RegexBuilder::new(r)
                .case_insensitive(true)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
                .build()
                .map_err(|e| FilterError::new(format!("Invalid regular expression: {e}")))?;
            Ok(Pattern::Regex(compiled))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_row(name: &str, namespace: Option<&str>, status: RowStatus, reason: &str) -> Row {
        Row {
            uid: format!("uid-{name}"),
            cluster: "dev".to_owned(),
            kind: "Pod".to_owned(),
            namespace: namespace.map(str::to_owned),
            name: name.to_owned(),
            status,
            status_reason: reason.to_owned(),
            cells: vec![
                Cell::Text(format!("{name}-container")),
                Cell::Text("10.244.0.5".to_owned()),
                Cell::Text("node-worker-1".to_owned()),
            ],
            labels: vec![("app".to_owned(), name.to_owned())],
            custom: Vec::new(),
        }
    }

    #[test]
    fn parses_plain_text_term() {
        let f = Filter::parse("web").unwrap();
        let r1 = test_row("web-0", Some("default"), RowStatus::Healthy, "Running");
        let r2 = test_row("api-0", Some("default"), RowStatus::Healthy, "Running");
        assert!(f.matches(&r1));
        assert!(!f.matches(&r2));
    }

    #[test]
    fn parses_field_qualifier_name_and_ns() {
        let f = Filter::parse("name:web-0 ns:default").unwrap();
        let r1 = test_row("web-0", Some("default"), RowStatus::Healthy, "Running");
        let r2 = test_row("web-0", Some("kube-system"), RowStatus::Healthy, "Running");
        assert!(f.matches(&r1));
        assert!(!f.matches(&r2));
    }

    #[test]
    fn parses_status_qualifier() {
        let f1 = Filter::parse("status:healthy").unwrap();
        let f2 = Filter::parse("status:CrashLoopBackOff").unwrap();
        let r1 = test_row("web-0", Some("default"), RowStatus::Healthy, "Running");
        let r2 = test_row(
            "web-1",
            Some("default"),
            RowStatus::Failed,
            "CrashLoopBackOff",
        );
        assert!(f1.matches(&r1));
        assert!(!f1.matches(&r2));
        assert!(!f2.matches(&r1));
        assert!(f2.matches(&r2));
    }

    #[test]
    fn parses_boolean_precedence_and_or() {
        // "web OR api AND prod" should be parsed as "web OR (api AND prod)"
        let f = Filter::parse("web OR api AND prod").unwrap();
        let r_web = test_row("web", Some("staging"), RowStatus::Healthy, "Running");
        let r_api_prod = test_row("api", Some("prod"), RowStatus::Healthy, "Running");
        let r_api_stage = test_row("api", Some("staging"), RowStatus::Healthy, "Running");

        assert!(f.matches(&r_web));
        assert!(f.matches(&r_api_prod));
        assert!(!f.matches(&r_api_stage));
    }

    #[test]
    fn parses_parentheses_grouping() {
        let f = Filter::parse("(web OR api) AND prod").unwrap();
        let r_web_stage = test_row("web", Some("staging"), RowStatus::Healthy, "Running");
        let r_web_prod = test_row("web", Some("prod"), RowStatus::Healthy, "Running");
        let r_api_prod = test_row("api", Some("prod"), RowStatus::Healthy, "Running");

        assert!(!f.matches(&r_web_stage));
        assert!(f.matches(&r_web_prod));
        assert!(f.matches(&r_api_prod));
    }

    #[test]
    fn parses_negation_not_bang_and_minus() {
        let f1 = Filter::parse("NOT status:failed").unwrap();
        let f2 = Filter::parse("!status:failed").unwrap();
        let f3 = Filter::parse("-status:failed").unwrap();

        let r_ok = test_row("web-0", Some("default"), RowStatus::Healthy, "Running");
        let r_fail = test_row("web-1", Some("default"), RowStatus::Failed, "Error");

        assert!(f1.matches(&r_ok));
        assert!(!f1.matches(&r_fail));
        assert!(f2.matches(&r_ok));
        assert!(!f2.matches(&r_fail));
        assert!(f3.matches(&r_ok));
        assert!(!f3.matches(&r_fail));
    }

    #[test]
    fn parses_exact_phrase() {
        let f = Filter::parse("\"CrashLoopBackOff container failure\"").unwrap();
        let r1 = test_row(
            "web-0",
            Some("default"),
            RowStatus::Failed,
            "CrashLoopBackOff container failure",
        );
        let r2 = test_row(
            "web-0",
            Some("default"),
            RowStatus::Failed,
            "container failure CrashLoopBackOff",
        );

        assert!(f.matches(&r1));
        assert!(!f.matches(&r2));
    }

    #[test]
    fn parses_bounded_regex() {
        let f = Filter::parse("/^web-[a-f0-9]{3}/").unwrap();
        let r1 = test_row("web-abc-0", Some("default"), RowStatus::Healthy, "Running");
        let r2 = test_row("api-abc-0", Some("default"), RowStatus::Healthy, "Running");

        assert!(f.matches(&r1));
        assert!(!f.matches(&r2));
    }

    #[test]
    fn handles_label_qualifier() {
        let f1 = Filter::parse("label:app=frontend").unwrap();
        let f2 = Filter::parse("label:tier").unwrap();
        let f3 = Filter::parse("label:database").unwrap();
        let mut r = test_row("web-0", Some("default"), RowStatus::Healthy, "Running");
        r.labels = vec![
            ("app".to_owned(), "frontend".to_owned()),
            ("tier".to_owned(), "web".to_owned()),
        ];

        assert!(f1.matches(&r));
        assert!(f2.matches(&r));
        assert!(!f3.matches(&r));
    }

    #[test]
    fn rejects_unclosed_parenthesis() {
        let err = Filter::parse("(web OR api").unwrap_err();
        assert!(err.message.contains("Unclosed parenthesis"));
    }

    #[test]
    fn rejects_unclosed_quote() {
        let err = Filter::parse("\"CrashLoop").unwrap_err();
        assert!(err.message.contains("Unclosed quote"));
    }

    #[test]
    fn rejects_unclosed_regex() {
        let err = Filter::parse("/^web-[0-9]+").unwrap_err();
        assert!(err.message.contains("Unclosed regular expression"));
    }

    #[test]
    fn parses_is_right_sizing_qualifier() {
        use crate::api::UsageCell;

        let f_near = Filter::parse("is:near-limit").unwrap();
        let f_over = Filter::parse("is:over-request").unwrap();
        let f_unused = Filter::parse("is:unused").unwrap();
        let f_missing = Filter::parse("is:missing-limits").unwrap();

        let mut r_near = test_row("near-pod", Some("default"), RowStatus::Healthy, "Running");
        r_near.cells.push(Cell::Usage(Box::new(UsageCell {
            value: 850,
            formatted: "850m".to_owned(),
            request: Some(500),
            limit: Some(1000),
        })));

        let mut r_over = test_row("over-pod", Some("default"), RowStatus::Healthy, "Running");
        r_over.cells.push(Cell::Usage(Box::new(UsageCell {
            value: 600,
            formatted: "600m".to_owned(),
            request: Some(500),
            limit: Some(1000),
        })));

        let mut r_unused = test_row("unused-pod", Some("default"), RowStatus::Healthy, "Running");
        r_unused.cells.push(Cell::Usage(Box::new(UsageCell {
            value: 50,
            formatted: "50m".to_owned(),
            request: Some(1000),
            limit: Some(2000),
        })));

        let mut r_missing = test_row(
            "missing-pod",
            Some("default"),
            RowStatus::Healthy,
            "Running",
        );
        r_missing.cells.push(Cell::Usage(Box::new(UsageCell {
            value: 100,
            formatted: "100m".to_owned(),
            request: None,
            limit: None,
        })));

        assert!(f_near.matches(&r_near));
        assert!(!f_near.matches(&r_unused));

        assert!(f_over.matches(&r_over));
        assert!(!f_over.matches(&r_unused));

        assert!(f_unused.matches(&r_unused));
        assert!(!f_unused.matches(&r_near));

        assert!(f_missing.matches(&r_missing));
        assert!(!f_missing.matches(&r_near));
    }
}
