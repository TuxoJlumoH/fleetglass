//! X.509 TLS Certificate parser and inspector (`04-07`).
//!
//! Parses PEM-encoded X.509 certificates and certificate chains into
//! structured [`CertificateInfo`] representations for the resource drawer and
//! Overview attention monitoring.

use std::net::{Ipv4Addr, Ipv6Addr};

use base64::Engine;
use k8s_openapi::jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::pem::Pem;
use x509_parser::prelude::*;

/// Structured inspection details for an X.509 certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct CertificateInfo {
    /// Context or identifier (e.g. "tls.crt", secret name, or webhook clientConfig).
    pub source: String,
    /// X.509 Subject distinguished name (e.g. `CN=example.com, O=Acme`).
    pub subject: String,
    /// X.509 Issuer distinguished name.
    pub issuer: String,
    /// Hex-formatted serial number (e.g. `04:A2:3B:...`).
    pub serial_number: String,
    /// Human-readable signature algorithm (e.g. `SHA256withRSA`, `ECDSA-with-SHA256`).
    pub signature_algorithm: String,
    /// Subject Alternative Names (DNS names, IP addresses).
    pub sans: Vec<String>,
    /// RFC 3339 not-before timestamp.
    pub not_before: String,
    /// RFC 3339 not-after timestamp.
    pub not_after: String,
    /// Days remaining until expiration (negative if expired).
    pub days_left: i32,
    /// Human-friendly status badge text (`Valid · 84d left`, `Expiring · 4d left`, `Expired · 2d ago`).
    pub validity_label: String,
    /// Badge tone: `"ok"` for ≥ 30d, `"warn"` for < 30d, `"err"` for < 7d or expired.
    pub validity_tone: String,
}

/// Checks whether a Secret or ConfigMap key name indicates a certificate file.
pub fn is_cert_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.ends_with(".crt")
        || lower.ends_with(".pem")
        || lower.ends_with("ca.crt")
        || lower == "tls.crt"
        || lower == "ca-bundle"
        || lower == "cabundle"
}

/// Parses all PEM-encoded `CERTIFICATE` blocks from `pem_data` using `now` as the reference time.
pub fn parse_pem_certificates(
    source: &str,
    pem_data: &[u8],
    now: Timestamp,
) -> Result<Vec<CertificateInfo>, String> {
    let mut certs = Vec::new();
    let mut count = 0;

    for (idx, pem_res) in Pem::iter_from_buffer(pem_data).enumerate() {
        let pem = pem_res.map_err(|e| format!("PEM decode error at block {idx}: {e}"))?;
        if pem.label != "CERTIFICATE" {
            continue;
        }

        let cert = pem
            .parse_x509()
            .map_err(|e| format!("X.509 parse error at block {idx}: {e}"))?;

        let block_source = if count == 0 {
            source.to_owned()
        } else {
            format!("{source} [chain #{}]", count + 1)
        };

        certs.push(extract_cert_info(&block_source, &cert, now));
        count += 1;
    }

    if certs.is_empty() {
        // Fall back to attempting raw DER certificate directly
        if let Ok((_, cert)) = x509_parser::parse_x509_certificate(pem_data) {
            certs.push(extract_cert_info(source, &cert, now));
        } else {
            return Err("No valid PEM CERTIFICATE blocks found".to_owned());
        }
    }

    Ok(certs)
}

fn extract_cert_info(source: &str, cert: &X509Certificate<'_>, now: Timestamp) -> CertificateInfo {
    let subject = cert.subject().to_string();
    let issuer = cert.issuer().to_string();
    let serial_number = format_serial(cert.raw_serial());
    let signature_algorithm = human_sig_alg(&cert.signature_algorithm.algorithm.to_string());

    let mut sans = Vec::new();
    for ext in cert.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for name in &san.general_names {
                match name {
                    GeneralName::DNSName(dns) => sans.push((*dns).to_owned()),
                    GeneralName::IPAddress(raw) => {
                        if raw.len() == 4 {
                            let ip = Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]);
                            sans.push(ip.to_string());
                        } else if raw.len() == 16 {
                            let mut octets = [0u8; 16];
                            octets.copy_from_slice(raw);
                            let ip = Ipv6Addr::from(octets);
                            sans.push(ip.to_string());
                        } else {
                            sans.push(format!("IP:{}", hex::encode(raw)));
                        }
                    }
                    GeneralName::URI(uri) => sans.push((*uri).to_owned()),
                    GeneralName::RFC822Name(email) => sans.push((*email).to_owned()),
                    _ => {}
                }
            }
        }
    }

    let not_before_ts = cert.validity().not_before.to_datetime();
    let not_after_ts = cert.validity().not_after.to_datetime();

    let not_before_sec = not_before_ts.unix_timestamp();
    let not_after_sec = not_after_ts.unix_timestamp();

    let not_before = Timestamp::from_second(not_before_sec)
        .map(|t| t.to_string())
        .unwrap_or_else(|_| format!("{not_before_sec}"));
    let not_after = Timestamp::from_second(not_after_sec)
        .map(|t| t.to_string())
        .unwrap_or_else(|_| format!("{not_after_sec}"));

    let now_sec = now.as_second();
    let diff_sec = not_after_sec - now_sec;
    let days_left = i32::try_from(diff_sec / 86400).unwrap_or(0);

    let (validity_label, validity_tone) = if diff_sec < 0 {
        let days_ago = (-diff_sec) / 86400;
        if days_ago == 0 {
            ("Expired today".to_owned(), "err".to_owned())
        } else {
            (format!("Expired · {days_ago}d ago"), "err".to_owned())
        }
    } else if days_left < 7 {
        (format!("Expiring · {days_left}d left"), "err".to_owned())
    } else if days_left < 30 {
        (format!("Expiring · {days_left}d left"), "warn".to_owned())
    } else {
        (format!("Valid · {days_left}d left"), "ok".to_owned())
    };

    CertificateInfo {
        source: source.to_owned(),
        subject,
        issuer,
        serial_number,
        signature_algorithm,
        sans,
        not_before,
        not_after,
        days_left,
        validity_label,
        validity_tone,
    }
}

/// Maps standard OID strings to readable algorithm names.
fn human_sig_alg(oid: &str) -> String {
    match oid {
        "1.2.840.113549.1.1.11" => "SHA256withRSA".to_owned(),
        "1.2.840.113549.1.1.12" => "SHA384withRSA".to_owned(),
        "1.2.840.113549.1.1.13" => "SHA512withRSA".to_owned(),
        "1.2.840.113549.1.1.5" => "SHA1withRSA".to_owned(),
        "1.2.840.10045.4.3.2" => "ECDSA-with-SHA256".to_owned(),
        "1.2.840.10045.4.3.3" => "ECDSA-with-SHA384".to_owned(),
        "1.2.840.10045.4.3.4" => "ECDSA-with-SHA512".to_owned(),
        "1.3.101.112" => "Ed25519".to_owned(),
        "1.3.101.113" => "Ed448".to_owned(),
        other => other.to_owned(),
    }
}

/// Formats a raw big-endian byte slice serial number into `XX:XX:XX:...`.
fn format_serial(raw: &[u8]) -> String {
    if raw.is_empty() {
        return "00".to_owned();
    }
    raw.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Inspects a Kubernetes Secret value for certificates.
pub fn inspect_secret_certificates(
    source_prefix: &str,
    secret_val: &Value,
    now: Timestamp,
) -> Vec<CertificateInfo> {
    let mut certs = Vec::new();

    if let Some(data) = secret_val.get("data").and_then(Value::as_object) {
        for (key, val) in data {
            if is_cert_key(key)
                && let Some(s) = val.as_str()
            {
                let cleaned = s.replace(['\n', '\r', ' '], "");
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(cleaned) {
                    let source_name = format!("{source_prefix}/{key}");
                    if let Ok(mut parsed) = parse_pem_certificates(&source_name, &bytes, now) {
                        certs.append(&mut parsed);
                    }
                }
            }
        }
    }

    if let Some(string_data) = secret_val.get("stringData").and_then(Value::as_object) {
        for (key, val) in string_data {
            if is_cert_key(key)
                && let Some(s) = val.as_str()
            {
                let source_name = format!("{source_prefix}/{key}");
                if let Ok(mut parsed) = parse_pem_certificates(&source_name, s.as_bytes(), now) {
                    certs.append(&mut parsed);
                }
            }
        }
    }

    certs
}

/// Inspects a resource (Secret, Ingress, Certificate, WebhookConfiguration) for certificates.
pub async fn inspect_resource_certificates(
    client: &kube::Client,
    kind: &str,
    namespace: Option<&str>,
    val: &Value,
) -> Vec<CertificateInfo> {
    let now = Timestamp::now();
    let mut certs = Vec::new();

    if kind.eq_ignore_ascii_case("Secret") {
        let name = val
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("Secret");
        return inspect_secret_certificates(name, val, now);
    }

    if kind.eq_ignore_ascii_case("Ingress")
        && let Some(tls_list) = val
            .get("spec")
            .and_then(|s| s.get("tls"))
            .and_then(Value::as_array)
    {
        for entry in tls_list {
            if let Some(secret_name) = entry.get("secretName").and_then(Value::as_str)
                && let Ok(secret_obj) =
                    crate::detail::fetch_dynamic_object(client, "Secret", namespace, secret_name)
                        .await
                && let Ok(secret_val) = serde_json::to_value(&secret_obj)
            {
                let mut sc =
                    inspect_secret_certificates(&format!("Secret/{secret_name}"), &secret_val, now);
                certs.append(&mut sc);
            }
        }
    }

    if kind.eq_ignore_ascii_case("Certificate")
        && let Some(secret_name) = val
            .get("spec")
            .and_then(|s| s.get("secretName"))
            .and_then(Value::as_str)
        && let Ok(secret_obj) =
            crate::detail::fetch_dynamic_object(client, "Secret", namespace, secret_name).await
        && let Ok(secret_val) = serde_json::to_value(&secret_obj)
    {
        let mut sc =
            inspect_secret_certificates(&format!("Secret/{secret_name}"), &secret_val, now);
        certs.append(&mut sc);
    }

    if (kind.eq_ignore_ascii_case("MutatingWebhookConfiguration")
        || kind.eq_ignore_ascii_case("ValidatingWebhookConfiguration"))
        && let Some(webhooks) = val.get("webhooks").and_then(Value::as_array)
    {
        for hook in webhooks {
            let hook_name = hook
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("webhook");
            if let Some(ca_bundle) = hook
                .get("clientConfig")
                .and_then(|c| c.get("caBundle"))
                .and_then(Value::as_str)
            {
                let cleaned = ca_bundle.replace(['\n', '\r', ' '], "");
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(cleaned) {
                    let source_name = format!("{hook_name} caBundle");
                    if let Ok(mut parsed) = parse_pem_certificates(&source_name, &bytes, now) {
                        certs.append(&mut parsed);
                    }
                }
            }
        }
    }

    certs
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDaDCCAlCgAwIBAgIUHGExBrbup0G5kwUjr2ula9keG9EwDQYJKoZIhvcNAQEL
BQAwKzEUMBIGA1UEAwwLZXhhbXBsZS5jb20xEzARBgNVBAoMCkZsZWV0Z2xhc3Mw
HhcNMjYxMTEzMTEwNjM1WhcNMjcxMTEzMTEwNjM1WjArMRQwEgYDVQQDDAtleGFt
cGxlLmNvbTETMBEGA1UECgwKRmxlZXRnbGFzczCCASIwDQYJKoZIhvcNAQEBBQAD
ggEPADCCAQoCggEBALK6jUC5cNy6alOSd+k3Lp5xynMVMB8+OPLGLa6VSzy8d/mm
ayMfEdHksgMQz25bVlEKwNM5fxim2h+1RkktGEIXppLzZueZ215ols53yTQcmFWh
UwxtLsAGObohakKaVVcqY1WlUFbtf/6CU0bxHwYhfz6c/rVr2jmGCTIkLFYozhwM
DoR/ZlXwtslkkSXRK/ZL8m+3roFr+B2YkQcFRqVFIbvPC84W4o+oP8qYrL7ncmvh
J3yAy2I3ZvFmAXulRerJNQOuzzUJs7gDxr0K5/hTm+blxTqAkPiwxwugPL5Tpjya
ZKlKwE0hvT98HLZ7VrLsKvCDy/e+RbnJtrDJrQsCAwEAAaOBgzCBgDAdBgNVHQ4E
FgQUAKDBAKHrQ3iwuY96i7DeS0N8jDMwHwYDVR0jBBgwFoAUAKDBAKHrQ3iwuY96
i7DeS0N8jDMwDwYDVR0TAQH/BAUwAwEB/zAtBgNVHREEJjAkggtleGFtcGxlLmNv
bYIPYXBpLmV4YW1wbGUuY29thwQKAAABMA0GCSqGSIb3DQEBCwUAA4IBAQCGcp+t
sqcqK7EUBD40z2eFLLwXQe0iYZT4xEMas6ODSn1YmFiCGMybAlzZRM/2NCHbP4Bn
tRBeErz9KGIITskI2V82CW2rwAVvCQhkIRTG2f5L6217fbdolrOWmxdpIaWNFpA1
5ByNMI17zeIc4ne3jubdcXCQylCu0I8/+ugrbUXhmX2awzsDnMkJvrODGpNubKlc
9bpNsFg3m6n+fYjuovPhUYzl538BF9CgCURGAUnzQ4wrwdTomxGgoo+Fl2ehA5wJ
N4lqb7cKFg9wIrx/8G24vlBJKHZsBjHDnORjEAxTesJxGJ2YRpCs5w5IgwhK8uqI
iwzu94Qzg5fa7r8/
-----END CERTIFICATE-----
"#;

    const SAMPLE_CA_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDTTCCAjWgAwIBAgIUBfTRuMvAMzRbPQW8x4T/yALVQN8wDQYJKoZIhvcNAQEL
BQAwNjEbMBkGA1UEAwwSRmxlZXRnbGFzcyBSb290IENBMRcwFQYDVQQKDA5GbGVl
dGdsYXNzIE9yZzAeFw0yNjExMTMxMTA2NDBaFw0yODExMTIxMTA2NDBaMDYxGzAZ
BgNVBAMMEkZsZWV0Z2xhc3MgUm9vdCBDQTEXMBUGA1UECgwORmxlZXRnbGFzcyBP
cmcwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQCOlII+GQVQErm0zQ21
0Vv0xHyxUEKnk25pyg0CyhkJIKcyeyp+BykTeQD9OanO5aJgZxgnj7hJvhfjy8xv
JwbtWnDtoLFWRbKYb+gYYyyDkdDjDBMAZbYA/SFNOt6GXPw4xSVaXWyPxAUVnAJ+
qormNu4GdSnwXPIcRO5bo88vsmpo4aLRM79WGj5WRtuIjHx5kQopm+3Y6iWpiJHv
LenKQtqaPV0pWqBJEfyV8kYRbuERWt7++sOQK85rfjiUiNH7HE+kbquj2k5ntMBc
/r+DmuqD7zRUyEH66PDzT1x2j+2kQaR3daORdpZJ8+qvi9muoapH9bTKl24Idk0C
wS2DAgMBAAGjUzBRMB0GA1UdDgQWBBTLwu/Jg4CH5OVlnbu2SQGt+75yDDAfBgNV
HSMEGDAWgBTLwu/Jg4CH5OVlnbu2SQGt+75yDDAPBgNVHRMBAf8EBTADAQH/MA0G
CSqGSIb3DQEBCwUAA4IBAQAjqR2L9rtZhZAxV4Fps/KJDC7NDCDcqSZ1GFw09L3O
mDMoTLzfSGy2ADgnoUBEHOI2DDWc0IKjBGypYa1frYQZ+9jqH2lcverizSeRq26R
RDYLLKdVRaaHTBsCDPTm2DLVyMpPTTF3s5+YK9VZq/WnG1lpw/7jWYL1911Ea8QF
oMhTYPWDII9EFLPCbcXpj71pNYXPHU00JHZKbeBYyH05IoZ3rt8RR3B4Eb9yzyK1
FFQhXG62vcyKqYaowj42d/wOP9J+eoQaDbJW3rFbA0MEl3SrCe/VEgUAu7qGga3d
vD8csLdg/URDwLi2WpmFe+1f6jo2dAHJ7+5pWUDEtAPN
-----END CERTIFICATE-----
"#;

    #[test]
    fn parses_single_valid_pem() {
        let now = "2026-11-13T12:00:00Z".parse::<Timestamp>().unwrap();
        let certs = parse_pem_certificates("tls.crt", SAMPLE_CERT_PEM.as_bytes(), now).unwrap();
        assert_eq!(certs.len(), 1);
        let cert = &certs[0];
        assert_eq!(cert.source, "tls.crt");
        assert!(cert.subject.contains("CN=example.com"));
        assert!(cert.subject.contains("O=Fleetglass"));
        assert!(cert.issuer.contains("CN=example.com"));
        assert_eq!(cert.signature_algorithm, "SHA256withRSA");
        assert_eq!(
            cert.sans,
            vec!["example.com", "api.example.com", "10.0.0.1"]
        );
        assert_eq!(cert.not_before, "2026-11-13T11:06:35Z");
        assert_eq!(cert.not_after, "2027-11-13T11:06:35Z");
        assert!(cert.days_left >= 364);
        assert_eq!(cert.validity_tone, "ok");
        assert!(cert.validity_label.starts_with("Valid · 36"));
    }

    #[test]
    fn parses_chained_pem() {
        let now = "2026-11-13T12:00:00Z".parse::<Timestamp>().unwrap();
        let chain = format!("{SAMPLE_CERT_PEM}\n{SAMPLE_CA_PEM}");
        let certs = parse_pem_certificates("bundle.crt", chain.as_bytes(), now).unwrap();
        assert_eq!(certs.len(), 2);
        assert_eq!(certs[0].source, "bundle.crt");
        assert_eq!(certs[1].source, "bundle.crt [chain #2]");
        assert!(certs[1].subject.contains("Fleetglass Root CA"));
        assert_eq!(certs[1].validity_tone, "ok");
    }

    #[test]
    fn parses_expiring_cert_amber_and_red() {
        // Sample cert expires 2027-11-13.
        // Test 20 days before expiry (< 30d -> amber "warn")
        let now_amber = "2027-10-24T11:06:35Z".parse::<Timestamp>().unwrap();
        let certs =
            parse_pem_certificates("tls.crt", SAMPLE_CERT_PEM.as_bytes(), now_amber).unwrap();
        assert_eq!(certs[0].days_left, 20);
        assert_eq!(certs[0].validity_tone, "warn");
        assert_eq!(certs[0].validity_label, "Expiring · 20d left");

        // Test 4 days before expiry (< 7d -> red "err")
        let now_red = "2027-11-09T11:06:35Z".parse::<Timestamp>().unwrap();
        let certs_red =
            parse_pem_certificates("tls.crt", SAMPLE_CERT_PEM.as_bytes(), now_red).unwrap();
        assert_eq!(certs_red[0].days_left, 4);
        assert_eq!(certs_red[0].validity_tone, "err");
        assert_eq!(certs_red[0].validity_label, "Expiring · 4d left");
    }

    #[test]
    fn parses_expired_cert_red() {
        let now_expired = "2027-11-15T11:06:35Z".parse::<Timestamp>().unwrap();
        let certs =
            parse_pem_certificates("tls.crt", SAMPLE_CERT_PEM.as_bytes(), now_expired).unwrap();
        assert!(certs[0].days_left < 0);
        assert_eq!(certs[0].validity_tone, "err");
        assert_eq!(certs[0].validity_label, "Expired · 2d ago");
    }

    #[test]
    fn parses_malformed_pem_gracefully() {
        let now = Timestamp::now();
        let res = parse_pem_certificates("test.crt", b"invalid not a pem", now);
        assert!(res.is_err());
    }

    #[test]
    fn inspects_secret_with_base64_tls_crt() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(SAMPLE_CERT_PEM);
        let secret = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "type": "kubernetes.io/tls",
            "metadata": {
                "name": "my-tls-secret",
                "namespace": "prod"
            },
            "data": {
                "tls.crt": b64,
                "tls.key": "dGVzdC1rZXk="
            }
        });

        let now = "2026-11-13T12:00:00Z".parse::<Timestamp>().unwrap();
        let certs = inspect_secret_certificates("my-tls-secret", &secret, now);
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].source, "my-tls-secret/tls.crt");
        assert_eq!(
            certs[0].sans,
            vec!["example.com", "api.example.com", "10.0.0.1"]
        );
    }
}
