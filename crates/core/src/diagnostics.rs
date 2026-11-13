//! Structured kubeconfig context and RBAC diagnostics (P1-02).
//!
//! Never echoes tokens, client key material, or raw credential plugin output.

use std::time::Duration;

use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
};
use kube::api::{Api, PostParams};
use kube::config::Kubeconfig;

use crate::api::auth::DeviceCodeInfo;
use crate::api::{
    AuthRequirement, AuthType, DiagnosticReport, DiagnosticStatus, DiagnosticStep, RbacCheck,
};
use crate::cluster::{sanitize_error, sanitize_kube_error};
use crate::error::Error;
use crate::kubeconfig::auth_type;

/// Extracts device code authorization details (verification URL and user code) from CLI/plugin output (`04-02`).
pub fn parse_device_code(text: &str) -> Option<DeviceCodeInfo> {
    let url_re = regex::Regex::new(r#"https?://[^\s<>'")]+"#).ok()?;
    let mut urls: Vec<&str> = url_re.find_iter(text).map(|m| m.as_str()).collect();
    if urls.is_empty() {
        return None;
    }
    urls.sort_by_key(|u| {
        let lower = u.to_lowercase();
        if lower.contains("device") || lower.contains("login") {
            0
        } else if lower.contains("auth") {
            1
        } else {
            2
        }
    });
    let url = urls[0].to_string();

    // Pattern A: explicitly labeled: "enter [the] code[:] <code>" or "user code[:] <code>" or "verification code[:] <code>"
    let labeled_code_re = regex::Regex::new(r"(?i)(?:enter\s+(?:the\s+)?code[:\s]+|user\s+code[:\s]+|verification\s+code[:\s]+)\s*([A-Za-z0-9+/=_-]{4,32})").ok()?;
    if let Some(caps) = labeled_code_re.captures(text)
        && let Some(c) = caps.get(1)
    {
        let code = c.as_str().trim().to_string();
        let lower = code.to_lowercase();
        if !lower.starts_with("http") && lower != "below" && lower != "to" && lower != "and" {
            return Some(DeviceCodeInfo {
                url,
                user_code: code,
            });
        }
    }

    // Pattern B: hyphenated code e.g. ABCD-EFGH
    let hyphen_code_re = regex::Regex::new(r"\b([A-Z0-9]{4,8}-[A-Z0-9]{4,8})\b").ok()?;
    if let Some(caps) = hyphen_code_re.captures(text)
        && let Some(c) = caps.get(1)
    {
        return Some(DeviceCodeInfo {
            url,
            user_code: c.as_str().to_string(),
        });
    }

    // Pattern C: standalone alphanumeric token (like Azure's C8H29Y6X7) following "code" and "to authenticate"
    let azure_code_re =
        regex::Regex::new(r"(?i)code\s+([A-Z0-9]{6,16})\s+to\s+authenticate").ok()?;
    if let Some(caps) = azure_code_re.captures(text)
        && let Some(c) = caps.get(1)
    {
        return Some(DeviceCodeInfo {
            url,
            user_code: c.as_str().to_string(),
        });
    }

    None
}

/// Detects expired credentials from exec plugins and auth providers (`04-02`).
pub fn detect_auth_error(stderr: &str) -> Option<AuthRequirement> {
    let lower = stderr.to_lowercase();
    let device_code = parse_device_code(stderr);

    if lower.contains("sso session has expired")
        || lower.contains("aws sso login")
        || lower.contains("aws eks get-token")
    {
        return Some(AuthRequirement {
            provider: "aws-sso".to_owned(),
            login_command: "aws sso login".to_owned(),
            device_code,
        });
    }
    if lower.contains("token has expired")
        || lower.contains("gcloud auth login")
        || lower.contains("gke-gcloud-auth-plugin")
    {
        return Some(AuthRequirement {
            provider: "gcloud".to_owned(),
            login_command: "gcloud auth login".to_owned(),
            device_code,
        });
    }
    if lower.contains("aadsts7000215")
        || (lower.contains("az login") && lower.contains("expired"))
        || lower.contains("devicelogin")
        || lower.contains("az aks")
        || lower.contains("kubelogin")
    {
        return Some(AuthRequirement {
            provider: "az".to_owned(),
            login_command: "az login".to_owned(),
            device_code,
        });
    }
    if (lower.contains("oidc") || lower.contains("token")) && lower.contains("expired")
        || lower.contains("kubectl oidc-login")
    {
        return Some(AuthRequirement {
            provider: "kubelogin".to_owned(),
            login_command: "kubectl oidc-login".to_owned(),
            device_code,
        });
    }
    if device_code.is_some() {
        return Some(AuthRequirement {
            provider: "cloud-sso".to_owned(),
            login_command: "kubectl auth reconcile".to_owned(),
            device_code,
        });
    }
    None
}
pub const LIVE_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Common resource/verb pairs checked for a context's RBAC baseline.
pub const COMMON_RBAC_CHECKS: &[(&str, &str)] = &[
    ("pods", "list"),
    ("pods", "watch"),
    ("namespaces", "list"),
    ("nodes", "list"),
    ("events", "list"),
];

/// Inspects a named context in `config` and returns a structured [`DiagnosticReport`].
///
/// `shell_env_source` describes how the login-shell environment was captured (e.g.
/// `"login-shell"` or `"process-env-fallback"`) and is included when reporting
/// exec-plugin authentication.
pub fn diagnose_kubeconfig_context(
    config: &Kubeconfig,
    context_name: &str,
    shell_env_source: &str,
) -> DiagnosticReport {
    let named_context = config.contexts.iter().find(|c| c.name == context_name);

    let Some(named_context) = named_context else {
        return DiagnosticReport {
            context: context_name.to_owned(),
            steps: vec![
                DiagnosticStep {
                    name: "Context & Cluster".to_owned(),
                    status: DiagnosticStatus::Fail,
                    detail: format!("Context \"{context_name}\" is not defined in kubeconfig"),
                },
                DiagnosticStep {
                    name: "Authentication".to_owned(),
                    status: DiagnosticStatus::Warn,
                    detail: "No credentials configured because the context is missing".to_owned(),
                },
                DiagnosticStep {
                    name: "TLS Configuration".to_owned(),
                    status: DiagnosticStatus::Pass,
                    detail: "System root certificate authorities in use".to_owned(),
                },
            ],
            rbac: Vec::new(),
            auth_error: None,
        };
    };

    let ctx = named_context.context.as_ref();
    let cluster_name = ctx.map(|c| c.cluster.as_str()).unwrap_or_default();
    let cluster = config
        .clusters
        .iter()
        .find(|c| c.name == cluster_name)
        .and_then(|c| c.cluster.as_ref());

    // Step 1: Context & Cluster
    let step_cluster = match cluster {
        None => DiagnosticStep {
            name: "Context & Cluster".to_owned(),
            status: DiagnosticStatus::Fail,
            detail: format!(
                "Cluster \"{cluster_name}\" referenced by context \"{context_name}\" is not defined"
            ),
        },
        Some(cluster_cfg) => match cluster_cfg.server.as_deref() {
            Some(server) if is_valid_server_url(server) => DiagnosticStep {
                name: "Context & Cluster".to_owned(),
                status: DiagnosticStatus::Pass,
                detail: format!("Cluster \"{cluster_name}\" server URL is {server}"),
            },
            Some(server) => DiagnosticStep {
                name: "Context & Cluster".to_owned(),
                status: DiagnosticStatus::Fail,
                detail: format!(
                    "Cluster \"{cluster_name}\" has invalid server URL \"{server}\" (expected http:// or https:// with host)"
                ),
            },
            None => DiagnosticStep {
                name: "Context & Cluster".to_owned(),
                status: DiagnosticStatus::Fail,
                detail: format!("Cluster \"{cluster_name}\" has no server URL configured"),
            },
        },
    };

    // Step 2: Authentication
    let user_name = ctx
        .and_then(|c| c.user.as_deref())
        .filter(|u| !u.is_empty());
    let auth_info = user_name
        .and_then(|u| config.auth_infos.iter().find(|a| a.name == u))
        .and_then(|a| a.auth_info.as_ref());
    let classified_auth = auth_info.map_or(AuthType::None, auth_type);

    let step_auth = match classified_auth {
        AuthType::Exec {
            command: Some(command),
        } => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: format!(
                "Exec credential plugin \"{command}\" configured (env source: {shell_env_source})"
            ),
        },
        AuthType::Exec { command: None } => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Fail,
            detail: format!(
                "Exec credential plugin has no command configured (env source: {shell_env_source})"
            ),
        },
        AuthType::Token => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: "Bearer token authentication configured (redacted)".to_owned(),
        },
        AuthType::ClientCert => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: "Client certificate authentication configured".to_owned(),
        },
        AuthType::Oidc => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: "OIDC authentication provider configured".to_owned(),
        },
        AuthType::AuthProvider { name } => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: format!("Auth provider \"{name}\" configured"),
        },
        AuthType::Basic => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: "Basic authentication configured (redacted)".to_owned(),
        },
        AuthType::None => DiagnosticStep {
            name: "Authentication".to_owned(),
            status: DiagnosticStatus::Warn,
            detail: "No credentials configured for this context".to_owned(),
        },
    };

    // Step 3: TLS Configuration
    let step_tls = if cluster.and_then(|c| c.insecure_skip_tls_verify) == Some(true) {
        DiagnosticStep {
            name: "TLS Configuration".to_owned(),
            status: DiagnosticStatus::Warn,
            detail: "TLS verification is disabled (insecure-skip-tls-verify: true)".to_owned(),
        }
    } else if cluster.is_some_and(|c| {
        c.certificate_authority.is_some() || c.certificate_authority_data.is_some()
    }) {
        DiagnosticStep {
            name: "TLS Configuration".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: "Custom certificate authority configured".to_owned(),
        }
    } else {
        DiagnosticStep {
            name: "TLS Configuration".to_owned(),
            status: DiagnosticStatus::Pass,
            detail: "System root certificate authorities in use".to_owned(),
        }
    };

    DiagnosticReport {
        context: context_name.to_owned(),
        steps: vec![step_cluster, step_auth, step_tls],
        rbac: Vec::new(),
        auth_error: None,
    }
}

/// Performs live diagnostics against the Kubernetes API server for `context_name`.
///
/// Runs the static kubeconfig checks first, then validates API server reachability and
/// version, and executes baseline RBAC checks ([`COMMON_RBAC_CHECKS`]).
///
/// If the client fails to connect or reachability fails, adds a Fail step and returns
/// empty RBAC checks.
///
/// Never echoes credentials, tokens, client key material, or raw secret envs.
pub async fn diagnose_kubeconfig_context_live(
    client: Result<kube::Client, Error>,
    config: &Kubeconfig,
    context_name: &str,
    shell_env_source: &str,
) -> DiagnosticReport {
    let mut report = diagnose_kubeconfig_context(config, context_name, shell_env_source);

    let cluster_server_valid = report
        .steps
        .first()
        .is_some_and(|s| s.status == DiagnosticStatus::Pass);

    let client = match client {
        Ok(c) if cluster_server_valid => c,
        Ok(_) => return report,
        Err(err) => {
            if let Error::Kube(kube::Error::Auth(kube::client::AuthError::AuthExecRun {
                out,
                ..
            })) = &err
            {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                report.auth_error = detect_auth_error(&format!("{stderr}\n{stdout}"));
            } else {
                let err_str = sanitize_error(&err);
                report.auth_error = detect_auth_error(&err_str);
            }
            report.steps.push(DiagnosticStep {
                name: "API Server Reachability".to_owned(),
                status: DiagnosticStatus::Fail,
                detail: sanitize_error(&err),
            });
            return report;
        }
    };

    let reachability_step =
        match tokio::time::timeout(LIVE_CHECK_TIMEOUT, client.apiserver_version()).await {
            Ok(Ok(info)) => DiagnosticStep {
                name: "API Server Reachability".to_owned(),
                status: DiagnosticStatus::Pass,
                detail: format!("API server version {}", info.git_version),
            },
            Ok(Err(err)) => DiagnosticStep {
                name: "API Server Reachability".to_owned(),
                status: DiagnosticStatus::Fail,
                detail: sanitize_kube_error(&err),
            },
            Err(_) => DiagnosticStep {
                name: "API Server Reachability".to_owned(),
                status: DiagnosticStatus::Fail,
                detail: "API server reachability timed out after 5 seconds".to_owned(),
            },
        };

    let reachable = reachability_step.status == DiagnosticStatus::Pass;
    report.steps.push(reachability_step);

    if !reachable {
        return report;
    }

    let api: Api<SelfSubjectAccessReview> = Api::all(client);
    let rbac_futs = COMMON_RBAC_CHECKS.iter().map(|&(resource, verb)| {
        let api = api.clone();
        async move {
            let review = build_access_review(resource, verb, None);
            let allowed = match tokio::time::timeout(
                LIVE_CHECK_TIMEOUT,
                api.create(&PostParams::default(), &review),
            )
            .await
            {
                Ok(Ok(res)) => res.status.as_ref().is_some_and(|s| s.allowed),
                _ => false,
            };
            RbacCheck {
                resource: resource.to_owned(),
                verb: verb.to_owned(),
                allowed,
            }
        }
    });

    report.rbac = futures::future::join_all(rbac_futs).await;
    report
}

/// Builds a Kubernetes [`SelfSubjectAccessReview`] request for `resource` and `verb`.
pub fn build_access_review(
    resource: &str,
    verb: &str,
    namespace: Option<&str>,
) -> SelfSubjectAccessReview {
    SelfSubjectAccessReview {
        spec: SelfSubjectAccessReviewSpec {
            resource_attributes: Some(ResourceAttributes {
                resource: Some(resource.to_owned()),
                verb: Some(verb.to_owned()),
                namespace: namespace.map(str::to_owned),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn is_valid_server_url(server: &str) -> bool {
    let trimmed = server.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"));
    let Some(rest) = rest else {
        return false;
    };
    let host_port = rest.split('/').next().unwrap_or_default();
    let host = host_port.split(':').next().unwrap_or_default();
    !host.is_empty() && !host.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use kube::config::{
        AuthInfo, Cluster, Context, ExecConfig, Kubeconfig, NamedAuthInfo, NamedCluster,
        NamedContext,
    };

    use super::*;

    fn base_config() -> Kubeconfig {
        Kubeconfig {
            clusters: vec![NamedCluster {
                name: "prod-cluster".to_owned(),
                cluster: Some(Cluster {
                    server: Some("https://127.0.0.1:6443".to_owned()),
                    certificate_authority_data: Some("LS0tLS1CRUdJTi...".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            auth_infos: vec![NamedAuthInfo {
                name: "prod-user".to_owned(),
                auth_info: Some(AuthInfo {
                    token: Some("super-secret-bearer-token".to_owned().into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            contexts: vec![NamedContext {
                name: "prod".to_owned(),
                context: Some(Context {
                    cluster: "prod-cluster".to_owned(),
                    user: Some("prod-user".to_owned()),
                    namespace: Some("default".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            current_context: Some("prod".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn valid_context_passes_all_steps_and_redacts_token() {
        let cfg = base_config();
        let report = diagnose_kubeconfig_context(&cfg, "prod", "login-shell");
        assert_eq!(report.context, "prod");
        assert_eq!(report.steps.len(), 3);
        assert_eq!(report.steps[0].name, "Context & Cluster");
        assert_eq!(report.steps[0].status, DiagnosticStatus::Pass);
        assert!(report.steps[0].detail.contains("https://127.0.0.1:6443"));

        assert_eq!(report.steps[1].name, "Authentication");
        assert_eq!(report.steps[1].status, DiagnosticStatus::Pass);
        assert!(!report.steps[1].detail.contains("super-secret-bearer-token"));

        assert_eq!(report.steps[2].name, "TLS Configuration");
        assert_eq!(report.steps[2].status, DiagnosticStatus::Pass);
        assert!(
            report.steps[2]
                .detail
                .contains("Custom certificate authority")
        );
    }

    #[test]
    fn missing_cluster_fails_context_and_cluster_step() {
        let mut cfg = base_config();
        cfg.clusters.clear();
        let report = diagnose_kubeconfig_context(&cfg, "prod", "login-shell");
        assert_eq!(report.steps[0].name, "Context & Cluster");
        assert_eq!(report.steps[0].status, DiagnosticStatus::Fail);
        assert!(report.steps[0].detail.contains("prod-cluster"));
    }

    #[test]
    fn exec_plugin_reports_command_and_shell_env_source() {
        let mut cfg = base_config();
        cfg.auth_infos[0].auth_info = Some(AuthInfo {
            exec: Some(ExecConfig {
                command: Some("aws".to_owned()),
                api_version: Some("client.authentication.k8s.io/v1beta1".to_owned()),
                args: Some(vec!["eks".to_owned(), "get-token".to_owned()]),
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = diagnose_kubeconfig_context(&cfg, "prod", "login-shell (/bin/zsh)");
        assert_eq!(report.steps[1].name, "Authentication");
        assert_eq!(report.steps[1].status, DiagnosticStatus::Pass);
        assert!(report.steps[1].detail.contains("aws"));
        assert!(report.steps[1].detail.contains("login-shell (/bin/zsh)"));
    }

    #[test]
    fn insecure_tls_and_missing_auth_emit_warnings() {
        let mut cfg = base_config();
        cfg.clusters[0]
            .cluster
            .as_mut()
            .unwrap()
            .insecure_skip_tls_verify = Some(true);
        cfg.auth_infos[0].auth_info = Some(AuthInfo::default());

        let report = diagnose_kubeconfig_context(&cfg, "prod", "process-env");
        assert_eq!(report.steps[1].status, DiagnosticStatus::Warn);
        assert_eq!(report.steps[2].name, "TLS Configuration");
        assert_eq!(report.steps[2].status, DiagnosticStatus::Warn);
        assert!(report.steps[2].detail.contains("insecure-skip-tls-verify"));
    }

    #[test]
    fn build_access_review_populates_resource_attributes() {
        assert_eq!(COMMON_RBAC_CHECKS.len(), 5);
        let review = build_access_review("pods", "list", Some("kube-system"));
        let attrs = review.spec.resource_attributes.unwrap();
        assert_eq!(attrs.resource.as_deref(), Some("pods"));
        assert_eq!(attrs.verb.as_deref(), Some("list"));
        assert_eq!(attrs.namespace.as_deref(), Some("kube-system"));
    }

    #[tokio::test]
    async fn detects_aws_sso_expiration_in_stderr() {
        let stderr = "error: SSO session has expired. Please login again using 'aws sso login'.";
        let auth_req = detect_auth_error(stderr).expect("Should detect SSO expiration");
        assert_eq!(auth_req.provider, "aws-sso");
        assert_eq!(auth_req.login_command, "aws sso login");
        assert!(auth_req.device_code.is_none());
    }

    #[test]
    fn parses_aws_sso_device_code_prompt() {
        let text = "To sign in, use a web browser to open the page https://device.sso.us-east-1.amazonaws.com/ and enter the code: BCDF-GHJK";
        let dc = parse_device_code(text).expect("should parse aws device code");
        assert_eq!(dc.url, "https://device.sso.us-east-1.amazonaws.com/");
        assert_eq!(dc.user_code, "BCDF-GHJK");

        let auth = detect_auth_error(&format!("aws sso login required\n{text}")).expect("detected");
        assert_eq!(auth.provider, "aws-sso");
        assert_eq!(auth.device_code, Some(dc));
    }

    #[test]
    fn parses_azure_device_code_prompt() {
        let text = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code C8H29Y6X7 to authenticate.";
        let dc = parse_device_code(text).expect("should parse azure device code");
        assert_eq!(dc.url, "https://microsoft.com/devicelogin");
        assert_eq!(dc.user_code, "C8H29Y6X7");

        let auth = detect_auth_error(&format!("kubelogin error: AADSTS7000215\n{text}"))
            .expect("detected");
        assert_eq!(auth.provider, "az");
        assert_eq!(auth.login_command, "az login");
        assert_eq!(auth.device_code, Some(dc));
    }

    #[test]
    fn parses_gcp_and_oidc_prompts() {
        let gcp_err =
            "gke-gcloud-auth-plugin: token has expired. Run 'gcloud auth login' to refresh.";
        let gcp_auth = detect_auth_error(gcp_err).expect("gcloud detected");
        assert_eq!(gcp_auth.provider, "gcloud");
        assert_eq!(gcp_auth.login_command, "gcloud auth login");

        let oidc_text = "kubectl oidc-login: Open https://accounts.google.com/o/oauth2/device/code and enter user code: 1234-5678";
        let oidc_auth = detect_auth_error(oidc_text).expect("oidc detected");
        assert_eq!(oidc_auth.provider, "kubelogin");
        assert_eq!(oidc_auth.login_command, "kubectl oidc-login");
        assert!(oidc_auth.device_code.is_some());
        let dc = oidc_auth.device_code.unwrap();
        assert_eq!(dc.url, "https://accounts.google.com/o/oauth2/device/code");
        assert_eq!(dc.user_code, "1234-5678");
    }

    #[tokio::test]
    async fn live_diagnostic_with_error_client_appends_reachability_fail_step() {
        let cfg = base_config();
        let report = diagnose_kubeconfig_context_live(
            Err(Error::ReadOnlyContext {
                context: "prod".to_owned(),
            }),
            &cfg,
            "prod",
            "process-env",
        )
        .await;

        assert_eq!(report.steps.len(), 4);
        assert_eq!(report.steps[3].name, "API Server Reachability");
        assert_eq!(report.steps[3].status, DiagnosticStatus::Fail);
        assert!(report.steps[3].detail.contains("read-only"));
        assert!(report.rbac.is_empty());
    }

    #[tokio::test]
    async fn live_diagnostic_with_invalid_cluster_server_skips_live_check() {
        let mut cfg = base_config();
        cfg.clusters[0].cluster.as_mut().unwrap().server = Some("not-a-valid-url".to_owned());

        let report = diagnose_kubeconfig_context_live(
            Err(Error::ReadOnlyContext {
                context: "prod".to_owned(),
            }),
            &cfg,
            "prod",
            "process-env",
        )
        .await;

        assert_eq!(report.steps.len(), 4);
        assert_eq!(report.steps[0].status, DiagnosticStatus::Fail);
        assert_eq!(report.steps[3].status, DiagnosticStatus::Fail);
        assert!(report.rbac.is_empty());
    }
}
