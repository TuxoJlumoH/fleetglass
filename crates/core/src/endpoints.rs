//! Browser endpoints of Ingress and Gateway API objects for the detail drawer (`05-05`).
//!
//! Endpoints are computed from typed objects (`k8s_openapi` `IngressSpec`, and
//! serde views of the Gateway API `Gateway` / `HTTPRoute` specs), never from YAML
//! text. Each URL is run through [`crate::browser::validate_browser_url`]; URLs
//! that would be rejected at launch (wildcard hosts, IP hosts) are kept as
//! non-openable entries so the UI can show them without a dead button.

use std::collections::HashSet;

use k8s_openapi::api::networking::v1::IngressSpec;
use serde::Deserialize;
use serde_json::Value;

use crate::api::ResourceEndpoint;
use crate::browser::{audit_safe_input, validate_browser_url};

/// Maximum number of endpoints returned per object.
pub const MAX_ENDPOINTS: usize = 50;

const GATEWAY_API_GROUP: &str = "gateway.networking.k8s.io";

/// Endpoints for `value` (the object's JSON) and whether the list was capped.
///
/// Returns an empty list for kinds other than Ingress, Gateway and HTTPRoute.
pub fn resource_endpoints(kind: &str, value: &Value) -> (Vec<ResourceEndpoint>, bool) {
    let mut out = Collector::default();
    let spec = value.get("spec").cloned().unwrap_or(Value::Null);
    let object_kind = value.get("kind").and_then(Value::as_str).unwrap_or(kind);
    let group = value
        .get("apiVersion")
        .and_then(Value::as_str)
        .and_then(|v| v.split_once('/').map(|(g, _)| g));
    let is_gateway_api = group.is_none_or(|g| g == GATEWAY_API_GROUP);

    let is_ingress =
        object_kind.eq_ignore_ascii_case("Ingress") || kind.eq_ignore_ascii_case("ingresses");
    let gateway_kind = is_gateway_api.then(|| object_kind.to_ascii_lowercase());
    match (is_ingress, gateway_kind.as_deref()) {
        (true, _) => {
            if let Ok(spec) = serde_json::from_value::<IngressSpec>(spec) {
                ingress_endpoints(&spec, &mut out);
            }
        }
        (false, Some("gateway")) => {
            if let Ok(spec) = serde_json::from_value::<GatewaySpec>(spec) {
                gateway_endpoints(&spec, &mut out);
            }
        }
        (false, Some("httproute")) => {
            if let Ok(spec) = serde_json::from_value::<HttpRouteSpec>(spec) {
                http_route_endpoints(&spec, &mut out);
            }
        }
        _ => {}
    }

    out.finish()
}

#[derive(Default)]
struct Collector {
    items: Vec<ResourceEndpoint>,
    seen: HashSet<String>,
    truncated: bool,
}

impl Collector {
    fn push(&mut self, scheme: &str, host: &str, port: Option<u16>, path: &str, source: String) {
        if self.truncated {
            return;
        }
        let default_port = if scheme == "https" { 443 } else { 80 };
        let port = port
            .filter(|p| *p != default_port)
            .map(|p| format!(":{p}"))
            .unwrap_or_default();
        let raw = format!("{scheme}://{host}{port}{path}");
        let (url, openable) = match validate_browser_url(&raw) {
            Ok(parsed) => (parsed.as_str().to_owned(), true),
            Err(_) => (audit_safe_input(&raw), false),
        };
        if !self.seen.insert(url.clone()) {
            return;
        }
        if self.items.len() == MAX_ENDPOINTS {
            self.truncated = true;
            return;
        }
        self.items.push(ResourceEndpoint {
            url,
            openable,
            source,
        });
    }

    fn finish(self) -> (Vec<ResourceEndpoint>, bool) {
        (self.items, self.truncated)
    }
}

/// A literal URL path, or `None` for paths that are patterns (regex, empty).
fn literal_path(path: Option<&str>) -> Option<&str> {
    let path = path.unwrap_or("/");
    let literal = path.starts_with('/')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '.' | '_' | '~' | '%'));
    literal.then_some(path)
}

fn ingress_endpoints(spec: &IngressSpec, out: &mut Collector) {
    let tls_hosts: HashSet<&str> = spec
        .tls
        .iter()
        .flatten()
        .flat_map(|tls| tls.hosts.iter().flatten())
        .map(String::as_str)
        .collect();

    for rule in spec.rules.iter().flatten() {
        // Hostless rules match any host; there is no URL to open without the LB address.
        let Some(host) = rule.host.as_deref().filter(|h| !h.is_empty()) else {
            continue;
        };
        let scheme = if tls_hosts.contains(host) {
            "https"
        } else {
            "http"
        };
        let source = format!("rule {host}");
        let paths = rule.http.as_ref().map(|http| http.paths.as_slice());
        match paths {
            Some(paths) if !paths.is_empty() => {
                for p in paths {
                    let path = literal_path(p.path.as_deref()).unwrap_or("/");
                    out.push(scheme, host, None, path, source.clone());
                }
            }
            _ => out.push(scheme, host, None, "/", source),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GatewaySpec {
    #[serde(default)]
    listeners: Vec<GatewayListener>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GatewayListener {
    #[serde(default)]
    name: String,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    port: Option<i64>,
    #[serde(default)]
    protocol: String,
}

fn gateway_endpoints(spec: &GatewaySpec, out: &mut Collector) {
    for listener in &spec.listeners {
        let Some(host) = listener.hostname.as_deref().filter(|h| !h.is_empty()) else {
            continue;
        };
        let scheme = match listener.protocol.to_ascii_uppercase().as_str() {
            "HTTP" => "http",
            "HTTPS" | "TLS" => "https",
            _ => continue,
        };
        let port = listener.port.and_then(|p| u16::try_from(p).ok());
        out.push(
            scheme,
            host,
            port,
            "/",
            format!("listener {}", listener.name),
        );
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpRouteSpec {
    #[serde(default)]
    hostnames: Vec<String>,
    #[serde(default)]
    rules: Vec<HttpRouteRule>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpRouteRule {
    #[serde(default)]
    matches: Vec<HttpRouteMatch>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpRouteMatch {
    #[serde(default)]
    path: Option<HttpPathMatch>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpPathMatch {
    /// Gateway API default: `PathPrefix`.
    #[serde(default, rename = "type")]
    type_: Option<String>,
    /// Gateway API default: `/`.
    #[serde(default)]
    value: Option<String>,
}

fn http_route_endpoints(spec: &HttpRouteSpec, out: &mut Collector) {
    // An HTTPRoute does not say which listener (http or https) serves it; the
    // parent Gateway does. Use http and let the server redirect if needed.
    let mut paths: Vec<&str> = Vec::new();
    for m in spec.rules.iter().flat_map(|r| r.matches.iter()) {
        let Some(path) = &m.path else {
            paths.push("/");
            continue;
        };
        match path.type_.as_deref().unwrap_or("PathPrefix") {
            "PathPrefix" | "Exact" => {
                if let Some(p) = literal_path(path.value.as_deref()) {
                    paths.push(p);
                }
            }
            _ => {}
        }
    }
    if paths.is_empty() {
        paths.push("/");
    }
    for host in spec.hostnames.iter().filter(|h| !h.is_empty()) {
        for path in &paths {
            out.push("http", host, None, path, format!("hostname {host}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn urls(kind: &str, value: &Value) -> Vec<(String, bool)> {
        let (eps, truncated) = resource_endpoints(kind, value);
        assert!(!truncated);
        eps.into_iter().map(|e| (e.url, e.openable)).collect()
    }

    fn open(url: &str) -> (String, bool) {
        (url.to_owned(), true)
    }

    fn backend() -> Value {
        json!({ "service": { "name": "web", "port": { "number": 80 } } })
    }

    #[test]
    fn ingress_rules_and_paths() {
        let obj = json!({
            "apiVersion": "networking.k8s.io/v1",
            "kind": "Ingress",
            "metadata": { "name": "demo-ingress" },
            "spec": { "rules": [
                { "host": "app.example.com", "http": { "paths": [
                    { "path": "/api", "pathType": "Prefix", "backend": backend() },
                    { "path": "/login", "pathType": "Prefix", "backend": backend() },
                ]}},
                { "host": "other.example.com" },
            ]},
        });
        assert_eq!(
            urls("Ingress", &obj),
            vec![
                open("http://app.example.com/api"),
                open("http://app.example.com/login"),
                open("http://other.example.com/"),
            ]
        );
    }

    #[test]
    fn ingress_tls_hosts_use_https() {
        let obj = json!({
            "apiVersion": "networking.k8s.io/v1",
            "kind": "Ingress",
            "spec": {
                "tls": [{ "hosts": ["secure.example.com"], "secretName": "tls-secret" }],
                "rules": [
                    { "host": "secure.example.com", "http": { "paths": [
                        { "path": "/dashboard", "pathType": "Prefix", "backend": backend() },
                    ]}},
                    { "host": "plain.example.com", "http": { "paths": [
                        { "path": "/", "pathType": "Prefix", "backend": backend() },
                    ]}},
                ],
            },
        });
        assert_eq!(
            urls("Ingress", &obj),
            vec![
                open("https://secure.example.com/dashboard"),
                open("http://plain.example.com/"),
            ]
        );
    }

    #[test]
    fn ingress_multi_host_tls_hostless_rules_regex_paths_and_wildcards() {
        let obj = json!({
            "apiVersion": "networking.k8s.io/v1",
            "kind": "Ingress",
            "spec": {
                "tls": [{ "hosts": ["a.example.com", "b.example.com"] }],
                "rules": [
                    { "http": { "paths": [
                        { "path": "/hostless", "pathType": "Prefix", "backend": backend() },
                    ]}},
                    { "host": "a.example.com" },
                    { "host": "b.example.com", "http": { "paths": [
                        { "path": "/api(/|$)(.*)", "pathType": "ImplementationSpecific", "backend": backend() },
                    ]}},
                    { "host": "*.wild.example.com" },
                ],
            },
        });
        assert_eq!(
            urls("ingresses", &obj),
            vec![
                open("https://a.example.com/"),
                open("https://b.example.com/"),
                ("http://*.wild.example.com/".to_owned(), false),
            ]
        );
    }

    #[test]
    fn ingress_status_addresses_are_not_endpoints() {
        let obj = json!({
            "apiVersion": "networking.k8s.io/v1",
            "kind": "Ingress",
            "spec": {},
            "status": { "loadBalancer": { "ingress": [
                { "ip": "203.0.113.10" },
                { "hostname": "lb.example.com" },
            ]}},
        });
        assert!(urls("Ingress", &obj).is_empty());
    }

    #[test]
    fn gateway_listeners_with_ports_protocols_and_wildcards() {
        let obj = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "spec": {
                "gatewayClassName": "example",
                "listeners": [
                    { "name": "wildcard", "port": 80, "protocol": "HTTP", "hostname": "*.example.com" },
                    { "name": "web", "port": 8080, "protocol": "HTTP", "hostname": "api.example.com" },
                    { "name": "secure", "port": 443, "protocol": "HTTPS", "hostname": "secure.example.com" },
                    { "name": "custom-tls", "port": 8443, "protocol": "TLS", "hostname": "alt.example.com" },
                    { "name": "no-host", "port": 80, "protocol": "HTTP" },
                    { "name": "tcp", "port": 5432, "protocol": "TCP", "hostname": "db.example.com" },
                ],
            },
        });
        assert_eq!(
            urls("Gateway", &obj),
            vec![
                ("http://*.example.com/".to_owned(), false),
                open("http://api.example.com:8080/"),
                open("https://secure.example.com/"),
                open("https://alt.example.com:8443/"),
            ]
        );
    }

    #[test]
    fn gateway_nested_tls_certificate_refs_and_allowed_routes() {
        let obj = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "spec": { "listeners": [
                {
                    "name": "https-first",
                    "tls": {
                        "mode": "Terminate",
                        "certificateRefs": [
                            { "group": "", "kind": "Secret", "name": "tls-secret-1" },
                            { "name": "tls-secret-2" },
                        ],
                    },
                    "allowedRoutes": { "namespaces": {
                        "from": "Selector",
                        "selector": { "matchExpressions": [
                            { "key": "env", "operator": "In", "values": ["prod", "staging"] },
                        ]},
                    }},
                    "hostname": "secure.example.com",
                    "port": 443,
                    "protocol": "HTTPS",
                },
                {
                    "name": "http-second",
                    "allowedRoutes": { "namespaces": { "from": "All" } },
                    "port": 8080,
                    "protocol": "HTTP",
                    "hostname": "api.example.com",
                },
            ]},
        });
        assert_eq!(
            urls("Gateway", &obj),
            vec![
                open("https://secure.example.com/"),
                open("http://api.example.com:8080/"),
            ]
        );
    }

    #[test]
    fn http_route_hostnames_and_path_matches() {
        let obj = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "HTTPRoute",
            "spec": {
                "hostnames": ["app.example.com", "service.internal.net"],
                "rules": [{ "matches": [
                    {
                        "path": { "type": "PathPrefix", "value": "/v1/api" },
                        "headers": [{ "name": "x-request-path", "value": "/ignored-header-value" }],
                        "queryParams": [{ "name": "redirect", "value": "/ignored-query-value" }],
                    },
                    { "path": { "type": "Exact", "value": "/health" } },
                    { "path": { "type": "RegularExpression", "value": "/re/.*" } },
                ]}],
            },
            "status": { "parents": [{ "conditions": [
                { "type": "ResolvedRefs", "status": "True", "message": "/ignored-status-path" },
            ]}]},
        });
        assert_eq!(
            urls("HTTPRoute", &obj),
            vec![
                open("http://app.example.com/v1/api"),
                open("http://app.example.com/health"),
                open("http://service.internal.net/v1/api"),
                open("http://service.internal.net/health"),
            ]
        );
    }

    #[test]
    fn http_route_without_matches_uses_root_and_without_hostnames_is_empty() {
        let obj = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "HTTPRoute",
            "spec": {
                "hostnames": ["app.example.com"],
                "rules": [{ "backendRefs": [{ "name": "web", "port": 80 }] }],
            },
        });
        assert_eq!(
            urls("HTTPRoute", &obj),
            vec![open("http://app.example.com/")]
        );

        let hostless = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "HTTPRoute",
            "spec": { "rules": [] },
        });
        assert!(urls("HTTPRoute", &hostless).is_empty());
    }

    #[test]
    fn other_kinds_and_foreign_gateway_groups_are_ignored() {
        assert!(urls("ConfigMap", &json!({ "kind": "ConfigMap", "data": {} })).is_empty());
        assert!(urls("Ingress", &Value::Null).is_empty());
        let foreign = json!({
            "apiVersion": "networking.example.io/v1",
            "kind": "Gateway",
            "spec": { "listeners": [
                { "name": "web", "port": 80, "protocol": "HTTP", "hostname": "app.example.com" },
            ]},
        });
        assert!(urls("Gateway", &foreign).is_empty());
    }

    #[test]
    fn malformed_spec_yields_no_endpoints() {
        let obj = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "spec": { "listeners": "not-a-list" },
        });
        assert!(urls("Gateway", &obj).is_empty());
    }

    #[test]
    fn caps_at_max_endpoints() {
        let hostnames: Vec<String> = (0..MAX_ENDPOINTS + 5)
            .map(|i| format!("h{i}.example.com"))
            .collect();
        let value = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "HTTPRoute",
            "spec": { "hostnames": hostnames },
        });
        let (eps, truncated) = resource_endpoints("HTTPRoute", &value);
        assert_eq!(eps.len(), MAX_ENDPOINTS);
        assert!(truncated);
        assert_eq!(eps[0].url, "http://h0.example.com/");
    }
}
