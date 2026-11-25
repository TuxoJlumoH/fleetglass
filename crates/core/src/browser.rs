//! Browser URL validation for the desktop browser opener (`05-05`).
//!
//! The opener never goes through a shell: it runs `xdg-open`, `open` or
//! `rundll32 url.dll,FileProtocolHandler` with the URL as a single argv entry.
//! Safety therefore comes from a real parse with [`url::Url`], not from a
//! character blocklist, and the launcher receives the parser's normalised
//! serialisation, never the raw input.
//!
//! Accepted URLs:
//! - `http`/`https` only, no embedded credentials;
//! - a loopback host (`127.0.0.0/8`, `[::1]` or `localhost`) with an explicit
//!   non-zero port (port-forwards), or
//! - an RFC 1123 DNS name (Ingress / Gateway / HTTPRoute hostnames). IP literals
//!   other than loopback are rejected: Kubernetes forbids them as Ingress and
//!   Gateway hostnames.

use url::{Host, Url};

use crate::error::{Error, Result};

/// Maximum permissible URL length for browser launch.
const MAX_URL_LENGTH: usize = 2048;

/// Maximum number of characters of rejected input kept in the audit log.
const MAX_AUDIT_INPUT_CHARS: usize = 200;

fn invalid(reason: impl Into<String>) -> Error {
    Error::InvalidBrowserUrl {
        reason: reason.into(),
    }
}

/// Validates `raw_url` for the system browser and returns the parsed URL.
///
/// Callers must pass [`Url::as_str`] of the result to the launcher.
pub fn validate_browser_url(raw_url: &str) -> Result<Url> {
    if raw_url.is_empty() {
        return Err(invalid("URL cannot be empty"));
    }
    if raw_url.len() > MAX_URL_LENGTH {
        return Err(invalid(format!(
            "URL length ({} bytes) exceeds limit of {MAX_URL_LENGTH}",
            raw_url.len()
        )));
    }
    // The URL parser silently strips leading/trailing whitespace and interior
    // tabs/newlines, so reject them on the raw input.
    if raw_url.chars().any(char::is_control) {
        return Err(invalid("URL must not contain control characters"));
    }
    if raw_url.chars().any(char::is_whitespace) {
        return Err(invalid("URL must not contain whitespace"));
    }

    let parsed = Url::parse(raw_url).map_err(|err| invalid(format!("malformed URL: {err}")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(invalid(format!(
                "unsupported scheme '{other}': only http and https are permitted"
            )));
        }
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid("embedded credentials in URL are forbidden"));
    }

    if parsed.port() == Some(0) {
        return Err(invalid("port 0 is not permitted"));
    }

    match parsed.host() {
        None => return Err(invalid("URL is missing a host")),
        Some(host) if is_loopback_host(&host) => {
            // `Url::port` is `None` for the scheme's default port, so look at the raw authority.
            if parsed.port().is_none() && !has_explicit_port(raw_url) {
                return Err(invalid(
                    "loopback endpoint must specify an explicit port (e.g. http://127.0.0.1:<port>)",
                ));
            }
        }
        Some(Host::Domain(domain)) => {
            if !is_valid_dns_name(domain) {
                return Err(invalid(format!(
                    "host '{domain}' is not a valid Ingress/Gateway hostname"
                )));
            }
        }
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => {
            return Err(invalid(
                "only loopback IP addresses are permitted; Ingress/Gateway endpoints must use a hostname",
            ));
        }
    }

    // Guaranteed by the scheme check; keep it explicit because the launcher
    // takes the URL as a bare argv entry.
    if parsed.as_str().starts_with('-') {
        return Err(invalid("URL must not start with '-'"));
    }

    Ok(parsed)
}

/// Whether `host` is loopback: `localhost` or an IP for which `is_loopback()` holds.
fn is_loopback_host<S: AsRef<str>>(host: &Host<S>) -> bool {
    match host {
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
        Host::Domain(domain) => domain.as_ref().eq_ignore_ascii_case("localhost"),
    }
}

/// Whether two validated URLs point at the same `(host, port)` endpoint.
///
/// Used to attribute a launch to an active port-forward regardless of path,
/// query or trailing `/`.
pub fn same_endpoint(a: &Url, b: &Url) -> bool {
    a.host() == b.host() && a.port_or_known_default() == b.port_or_known_default()
}

/// Bounded, control-character-free rendering of untrusted input for the audit log.
pub fn audit_safe_input(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .take(MAX_AUDIT_INPUT_CHARS)
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect();
    if raw.chars().count() > MAX_AUDIT_INPUT_CHARS {
        out.push('…');
    }
    out
}

/// Whether `raw_url` names an explicit non-zero port in its authority.
fn has_explicit_port(raw_url: &str) -> bool {
    let Some((_, rest)) = raw_url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // IPv6 literals contain ':' inside brackets; only look after the closing bracket.
    let after_host = match authority.rfind(']') {
        Some(idx) => &authority[idx + 1..],
        None => authority,
    };
    after_host
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .is_some_and(|port| port > 0)
}

/// Whether `host` is a valid RFC 1123 DNS subdomain (as Kubernetes validates hostnames).
pub(crate) fn is_valid_dns_name(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.starts_with(|c: char| c.is_ascii_alphanumeric())
            && label.ends_with(|c: char| c.is_ascii_alphanumeric())
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(url: &str) -> String {
        match validate_browser_url(url) {
            Ok(parsed) => parsed.as_str().to_owned(),
            Err(err) => panic!("expected {url} to be accepted: {err}"),
        }
    }

    fn rejected(url: &str) -> String {
        match validate_browser_url(url) {
            Ok(parsed) => panic!("expected {url} to be rejected, got {parsed}"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn accepts_loopback_with_port_and_normalises() {
        assert_eq!(ok("http://127.0.0.1:8080"), "http://127.0.0.1:8080/");
        assert_eq!(ok("http://localhost:3000"), "http://localhost:3000/");
        assert_eq!(ok("https://localhost:8443"), "https://localhost:8443/");
        assert_eq!(
            ok("http://127.0.0.1:8080/dashboard"),
            "http://127.0.0.1:8080/dashboard"
        );
        assert_eq!(ok("http://127.0.0.2:8080"), "http://127.0.0.2:8080/");
        assert_eq!(ok("http://127.1:8080"), "http://127.0.0.1:8080/");
        assert_eq!(ok("http://LOCALHOST:80"), "http://localhost/");
        assert_eq!(ok("http://127.0.0.1:80/"), "http://127.0.0.1/");
        assert_eq!(ok("https://localhost:443"), "https://localhost/");
    }

    #[test]
    fn accepts_ipv6_loopback() {
        assert_eq!(ok("http://[::1]:8080"), "http://[::1]:8080/");
        assert_eq!(ok("http://[0:0:0:0:0:0:0:1]:9000/x"), "http://[::1]:9000/x");
        assert!(rejected("http://[::1]").contains("explicit port"));
        assert!(rejected("http://[::]:8080").contains("loopback"));
        assert!(rejected("http://[fe80::1]:8080").contains("loopback"));
    }

    #[test]
    fn accepts_query_strings_and_percent_encoding() {
        assert_eq!(
            ok("http://127.0.0.1:8080/search?a=1&b=2"),
            "http://127.0.0.1:8080/search?a=1&b=2"
        );
        assert_eq!(
            ok("https://app.example.com/p%20q?x=%26&y=a%2Fb#frag"),
            "https://app.example.com/p%20q?x=%26&y=a%2Fb#frag"
        );
        // Characters a shell would interpret are inert in a single argv entry; the
        // parser percent-encodes what is not allowed in a URL.
        assert_eq!(
            ok("http://localhost:8080/a\"b<c>`d`"),
            "http://localhost:8080/a%22b%3Cc%3E%60d%60"
        );
        assert_eq!(
            ok("http://localhost:8080/$(id);x|y"),
            "http://localhost:8080/$(id);x|y"
        );
    }

    #[test]
    fn accepts_ingress_and_gateway_hostnames() {
        assert_eq!(ok("http://app.example.com"), "http://app.example.com/");
        assert_eq!(
            ok("https://Secure.Example.com/login"),
            "https://secure.example.com/login"
        );
        assert_eq!(
            ok("https://api.staging.internal.net:8443/docs"),
            "https://api.staging.internal.net:8443/docs"
        );
        assert_eq!(ok("http://my-service:8080"), "http://my-service:8080/");
    }

    #[test]
    fn rejects_loopback_without_port() {
        for url in [
            "http://127.0.0.1",
            "http://localhost",
            "https://localhost/",
            "http://127.0.0.2",
        ] {
            assert!(rejected(url).contains("explicit port"), "{url}");
        }
    }

    #[test]
    fn rejects_unspecified_and_non_loopback_ips() {
        assert!(rejected("http://0.0.0.0:8080").contains("loopback"));
        assert!(rejected("http://0.0.0.0").contains("loopback"));
        assert!(rejected("http://10.0.0.1:8080").contains("loopback"));
        assert!(rejected("https://203.0.113.7/login").contains("loopback"));
    }

    #[test]
    fn rejects_port_zero() {
        for url in [
            "http://127.0.0.1:0",
            "http://localhost:0",
            "http://app.example.com:0",
        ] {
            assert!(rejected(url).contains("port 0"), "{url}");
        }
    }

    #[test]
    fn rejects_whitespace_and_control_characters() {
        for url in [
            "http://127.0.0.1:8080 && whoami",
            "http://127.0.0.1:8080\nnewline",
            "http://127.0.0.1:8080\rcarriage",
            "http://127.0.0.1:8080\tx",
            "http://127.0.0.1:8080\0null",
            " http://127.0.0.1:8080",
            "http://127.0.0.1:8080\u{a0}",
            "http://127.0.0.1:8080\u{85}",
        ] {
            rejected(url);
        }
    }

    #[test]
    fn rejects_injection_in_authority() {
        for url in [
            "http://127.0.0.1:8080;echo",
            "http://127.0.0.1:8080`whoami`",
            "http://127.0.0.1:8080$(id)",
            "http://127.0.0.1:8080|calc",
            "http://app.example.com&calc",
            "http://app$(id).example.com",
        ] {
            rejected(url);
        }
    }

    #[test]
    fn rejects_dangerous_schemes_and_option_like_input() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "ftp://localhost:21",
            "tel:123456",
            "-a http://127.0.0.1:8080",
            "--help",
            "//127.0.0.1:8080",
        ] {
            rejected(url);
        }
    }

    #[test]
    fn rejects_credentials() {
        assert!(rejected("http://user:pass@127.0.0.1:8080").contains("credentials"));
        assert!(rejected("http://user@app.example.com").contains("credentials"));
    }

    #[test]
    fn rejects_invalid_hostnames() {
        for url in [
            "http://-leading-hyphen.com",
            "http://trailing-hyphen-.com",
            "http://*.example.com",
            "http://invalid_char.com",
            "http://app.example.com.",
        ] {
            rejected(url);
        }
    }

    #[test]
    fn rejects_empty_or_oversized() {
        rejected("");
        rejected(&format!("http://localhost:8080/{}", "a".repeat(3000)));
    }

    #[test]
    fn same_endpoint_ignores_path_and_trailing_slash() {
        let pf = validate_browser_url("http://127.0.0.1:8080").unwrap();
        let slash = validate_browser_url("http://127.0.0.1:8080/").unwrap();
        let path = validate_browser_url("http://127.0.0.1:8080/x?y=1").unwrap();
        let other_port = validate_browser_url("http://127.0.0.1:8081").unwrap();
        let other_host = validate_browser_url("http://localhost:8080").unwrap();
        assert!(same_endpoint(&pf, &slash));
        assert!(same_endpoint(&pf, &path));
        assert!(!same_endpoint(&pf, &other_port));
        assert!(!same_endpoint(&pf, &other_host));
    }

    #[test]
    fn audit_safe_input_truncates_and_strips_controls() {
        assert_eq!(audit_safe_input("a\nb"), "a\u{FFFD}b");
        let long = "x".repeat(5000);
        let safe = audit_safe_input(&long);
        assert_eq!(safe.chars().count(), MAX_AUDIT_INPUT_CHARS + 1);
        assert!(safe.ends_with('…'));
    }

    #[test]
    fn dns_name_rules() {
        assert!(is_valid_dns_name("a.b-c.d"));
        assert!(is_valid_dns_name("xn--bcher-kva.example"));
        assert!(!is_valid_dns_name("a..b"));
        assert!(!is_valid_dns_name(&"a".repeat(64)));
    }
}
