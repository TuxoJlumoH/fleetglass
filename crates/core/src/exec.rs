//! Interactive container shell execution (`P1-09`, `R-10`) command specification,
//! `read_only` context guard, `kubectl exec` preview formatting, and live interactive sessions.

use futures::SinkExt;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, AttachParams};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::api::{AttachSpec, ContextSettings, ExecSpec};
use crate::error::{Error, Result};

/// Returns `custom.to_vec()` if non-empty, otherwise defaults to `vec!["/bin/sh"]`.
pub fn default_shell_command(custom: &[String]) -> Vec<String> {
    if custom.is_empty() {
        vec!["/bin/sh".to_owned()]
    } else {
        custom.to_vec()
    }
}

/// Validates that `exec` is permitted on `spec.context` under `settings` (`P1-09`, `V-02`).
///
/// Enforces write safety via [`crate::actions::enforce_write_safety`]:
/// - If `settings.read_only` is `true`, returns [`Error::ReadOnlyContext`].
/// - If `settings.protected` is `true` and `confirmed_name != Some(&spec.pod)`,
///   returns [`Error::ConfirmationRequired`].
/// - Otherwise returns `Ok(default_shell_command(&spec.command))`.
pub fn validate_exec_allowed(
    spec: &ExecSpec,
    settings: &ContextSettings,
    confirmed_name: Option<&str>,
) -> Result<Vec<String>> {
    crate::actions::enforce_write_safety(&spec.context, settings, &spec.pod, confirmed_name)?;
    Ok(default_shell_command(&spec.command))
}

/// Alias for [`validate_exec_allowed`].
pub fn prepare_exec_command(
    spec: &ExecSpec,
    settings: &ContextSettings,
    confirmed_name: Option<&str>,
) -> Result<Vec<String>> {
    validate_exec_allowed(spec, settings, confirmed_name)
}

/// Formats `kubectl --context <ctx> -n <ns> exec -it <pod> [-c <container>] -- <cmd...>`.
pub fn preview_exec_kubectl(spec: &ExecSpec) -> String {
    let cmd = default_shell_command(&spec.command).join(" ");
    match spec.container.as_deref().filter(|c| !c.is_empty()) {
        Some(container) => format!(
            "kubectl --context {} -n {} exec -it {} -c {} -- {}",
            spec.context, spec.namespace, spec.pod, container, cmd
        ),
        None => format!(
            "kubectl --context {} -n {} exec -it {} -- {}",
            spec.context, spec.namespace, spec.pod, cmd
        ),
    }
}

/// Validates that `attach` is permitted on `spec.context` under `settings` (`04-03c`).
pub fn validate_attach_allowed(
    spec: &AttachSpec,
    settings: &ContextSettings,
    confirmed_name: Option<&str>,
) -> Result<()> {
    crate::actions::enforce_write_safety(&spec.context, settings, &spec.pod, confirmed_name)
}

/// Formats `kubectl --context <ctx> -n <ns> attach pod/<pod> [-c <container>] -it`.
pub fn preview_attach_kubectl(spec: &AttachSpec) -> String {
    match spec.container.as_deref().filter(|c| !c.is_empty()) {
        Some(container) => format!(
            "kubectl --context {} -n {} attach pod/{} -c {} -it",
            spec.context, spec.namespace, spec.pod, container
        ),
        None => format!(
            "kubectl --context {} -n {} attach pod/{} -it",
            spec.context, spec.namespace, spec.pod
        ),
    }
}

/// An active interactive container execution session (`R-10`).
#[derive(Debug)]
pub struct ExecSession {
    stdin_tx: mpsc::Sender<Vec<u8>>,
    task: tokio::task::JoinHandle<()>,
}

impl ExecSession {
    /// Creates an [`ExecSession`] from components (useful for tests or custom wiring).
    #[doc(hidden)]
    pub fn new(stdin_tx: mpsc::Sender<Vec<u8>>, task: tokio::task::JoinHandle<()>) -> Self {
        Self { stdin_tx, task }
    }

    /// Returns a clone of the channel sender for stdin bytes.
    pub fn stdin_sender(&self) -> mpsc::Sender<Vec<u8>> {
        self.stdin_tx.clone()
    }

    /// Sends stdin bytes to the remote container process.
    pub async fn send_stdin(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.stdin_tx
            .send(bytes.into())
            .await
            .map_err(|_| Error::Exec {
                message: "exec stdin channel closed".to_owned(),
            })
    }

    /// Convenience method to send a UTF-8 string to stdin.
    pub async fn write_stdin_str(&self, text: &str) -> Result<()> {
        self.send_stdin(text.as_bytes().to_vec()).await
    }

    /// Aborts the remote execution session and background pump tasks.
    pub fn abort(&self) {
        self.task.abort();
    }

    /// Returns `true` if the session's background task has completed.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for ExecSession {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts an interactive exec session in `spec.pod` and streams output chunks to `output_tx`.
///
/// Respects write safety: if `settings.read_only` is true, returns [`Error::ReadOnlyContext`].
/// If `settings.protected` is true, requires typing the pod name to confirm.
pub async fn start_exec_session(
    client: &Client,
    spec: &ExecSpec,
    settings: &ContextSettings,
    confirmed_name: Option<&str>,
    output_tx: mpsc::Sender<String>,
) -> Result<ExecSession> {
    let command = prepare_exec_command(spec, settings, confirmed_name)?;

    // Note: kube-rs and Kubernetes exec protocol reject `tty: true` with `stderr: true`
    // ("AttachParams: tty and stderr cannot both be true"). When TTY is enabled,
    // the container runtime multiplexes all terminal output onto stdout.
    let mut ap = AttachParams::default()
        .stdin(true)
        .stdout(true)
        .stderr(false)
        .tty(true);
    if let Some(container) = &spec.container
        && !container.is_empty()
    {
        ap = ap.container(container);
    }

    let pods: Api<Pod> = Api::namespaced(client.clone(), &spec.namespace);
    let mut attached = pods.exec(&spec.pod, command, &ap).await?;

    let mut stdin = attached.stdin().ok_or_else(|| Error::Exec {
        message: "exec stdin stream unavailable".to_owned(),
    })?;
    let mut stdout = attached.stdout().ok_or_else(|| Error::Exec {
        message: "exec stdout stream unavailable".to_owned(),
    })?;
    let stderr = attached.stderr();

    if spec.cols > 0
        && spec.rows > 0
        && let Some(mut resize) = attached.terminal_size()
    {
        let _ = resize
            .send(kube::api::TerminalSize {
                width: spec.cols,
                height: spec.rows,
            })
            .await;
    }

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(128);
    let output_tx_err = output_tx.clone();

    let task = tokio::spawn(async move {
        let stdin_pump = async {
            while let Some(bytes) = stdin_rx.recv().await {
                if stdin.write_all(&bytes).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        };

        let stdout_pump = async {
            let mut buf = [0u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                        if output_tx.send(text).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        let stderr_pump = async {
            if let Some(mut stderr) = stderr {
                let mut buf = [0u8; 4096];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                            if output_tx_err.send(text).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        };

        let outputs = async { tokio::join!(stdout_pump, stderr_pump) };

        tokio::select! {
            _ = outputs => {},
            _ = async {
                stdin_pump.await;
                futures::future::pending::<()>().await;
            } => {},
        }

        attached.abort();
    });

    Ok(ExecSession { stdin_tx, task })
}

/// Starts an interactive container attach session in `spec.pod` and streams output chunks to `output_tx` (`04-03c`).
pub async fn start_attach_session(
    client: &Client,
    spec: &AttachSpec,
    settings: &ContextSettings,
    confirmed_name: Option<&str>,
    output_tx: mpsc::Sender<String>,
) -> Result<ExecSession> {
    validate_attach_allowed(spec, settings, confirmed_name)?;

    let mut ap = AttachParams::default()
        .stdin(true)
        .stdout(true)
        .stderr(false)
        .tty(true);
    if let Some(container) = &spec.container
        && !container.is_empty()
    {
        ap = ap.container(container);
    }

    let pods: Api<Pod> = Api::namespaced(client.clone(), &spec.namespace);
    let mut attached = pods.attach(&spec.pod, &ap).await?;

    let mut stdin = attached.stdin().ok_or_else(|| Error::Exec {
        message: "attach stdin stream unavailable".to_owned(),
    })?;
    let mut stdout = attached.stdout().ok_or_else(|| Error::Exec {
        message: "attach stdout stream unavailable".to_owned(),
    })?;
    let stderr = attached.stderr();

    if spec.cols > 0
        && spec.rows > 0
        && let Some(mut resize) = attached.terminal_size()
    {
        let _ = resize
            .send(kube::api::TerminalSize {
                width: spec.cols,
                height: spec.rows,
            })
            .await;
    }

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(128);
    let output_tx_err = output_tx.clone();

    let task = tokio::spawn(async move {
        let stdin_pump = async {
            while let Some(bytes) = stdin_rx.recv().await {
                if stdin.write_all(&bytes).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        };

        let stdout_pump = async {
            let mut buf = [0u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                        if output_tx.send(text).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        let stderr_pump = async {
            if let Some(mut stderr) = stderr {
                let mut buf = [0u8; 4096];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                            if output_tx_err.send(text).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        };

        let outputs = async { tokio::join!(stdout_pump, stderr_pump) };

        tokio::select! {
            _ = outputs => {},
            _ = async {
                stdin_pump.await;
                futures::future::pending::<()>().await;
            } => {},
        }

        attached.abort();
    });

    Ok(ExecSession { stdin_tx, task })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec(container: Option<&str>, command: Vec<&str>) -> ExecSpec {
        ExecSpec {
            context: "prod-eu".to_owned(),
            namespace: "payments".to_owned(),
            pod: "api-7f8b9-x12".to_owned(),
            container: container.map(str::to_owned),
            command: command.into_iter().map(str::to_owned).collect(),
            cols: 120,
            rows: 40,
        }
    }

    #[test]
    fn validate_exec_allowed_blocks_read_only_context() {
        let spec = sample_spec(Some("app"), vec![]);
        let settings = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };

        let err = validate_exec_allowed(&spec, &settings, None).unwrap_err();
        assert!(matches!(
            err,
            Error::ReadOnlyContext { ref context } if context == "prod-eu"
        ));
    }

    #[test]
    fn validate_exec_allowed_defaults_to_bin_sh_and_preserves_custom_command() {
        let settings = ContextSettings::default();

        let default_spec = sample_spec(None, vec![]);
        let cmd = validate_exec_allowed(&default_spec, &settings, None).unwrap();
        assert_eq!(cmd, vec!["/bin/sh".to_owned()]);

        let custom_spec = sample_spec(Some("app"), vec!["/bin/bash", "-l"]);
        let custom_cmd = validate_exec_allowed(&custom_spec, &settings, None).unwrap();
        assert_eq!(custom_cmd, vec!["/bin/bash".to_owned(), "-l".to_owned()]);
    }

    #[test]
    fn validate_exec_allowed_unprotected_context_allows_exec_without_confirmed_name() {
        let spec = sample_spec(None, vec![]);
        let settings = ContextSettings {
            read_only: false,
            protected: false,
            ..ContextSettings::default()
        };
        let cmd = validate_exec_allowed(&spec, &settings, None).unwrap();
        assert_eq!(cmd, vec!["/bin/sh".to_owned()]);
    }

    #[test]
    fn validate_exec_allowed_protected_context_requires_matching_confirmed_name() {
        let spec = sample_spec(None, vec![]);
        let settings = ContextSettings {
            protected: true,
            ..ContextSettings::default()
        };

        // None returns ConfirmationRequired
        let err_none = validate_exec_allowed(&spec, &settings, None).unwrap_err();
        assert!(matches!(
            err_none,
            Error::ConfirmationRequired { ref context, ref expected_name }
                if context == "prod-eu" && expected_name == "api-7f8b9-x12"
        ));

        // Wrong name returns ConfirmationRequired
        let err_wrong = validate_exec_allowed(&spec, &settings, Some("wrong-pod")).unwrap_err();
        assert!(matches!(
            err_wrong,
            Error::ConfirmationRequired { ref context, ref expected_name }
                if context == "prod-eu" && expected_name == "api-7f8b9-x12"
        ));

        // Matching name succeeds
        let cmd = validate_exec_allowed(&spec, &settings, Some("api-7f8b9-x12")).unwrap();
        assert_eq!(cmd, vec!["/bin/sh".to_owned()]);
    }

    #[test]
    fn preview_exec_kubectl_formats_with_and_without_container() {
        let without_container = sample_spec(None, vec![]);
        assert_eq!(
            preview_exec_kubectl(&without_container),
            "kubectl --context prod-eu -n payments exec -it api-7f8b9-x12 -- /bin/sh"
        );

        let with_container = sample_spec(Some("sidecar"), vec!["/bin/ash"]);
        assert_eq!(
            preview_exec_kubectl(&with_container),
            "kubectl --context prod-eu -n payments exec -it api-7f8b9-x12 -c sidecar -- /bin/ash"
        );
    }

    #[test]
    fn validate_attach_allowed_enforces_safety_and_preview_formats() {
        let spec = AttachSpec {
            context: "prod-eu".to_owned(),
            namespace: "payments".to_owned(),
            pod: "api-7f8b9-x12".to_owned(),
            container: Some("main".to_owned()),
            cols: 80,
            rows: 24,
        };

        let readonly_settings = ContextSettings {
            read_only: true,
            ..ContextSettings::default()
        };
        assert!(matches!(
            validate_attach_allowed(&spec, &readonly_settings, None),
            Err(Error::ReadOnlyContext { .. })
        ));

        let protected_settings = ContextSettings {
            protected: true,
            ..ContextSettings::default()
        };
        assert!(matches!(
            validate_attach_allowed(&spec, &protected_settings, None),
            Err(Error::ConfirmationRequired { .. })
        ));
        assert!(validate_attach_allowed(&spec, &protected_settings, Some("api-7f8b9-x12")).is_ok());

        assert_eq!(
            preview_attach_kubectl(&spec),
            "kubectl --context prod-eu -n payments attach pod/api-7f8b9-x12 -c main -it"
        );
        let no_container_spec = AttachSpec {
            container: None,
            ..spec
        };
        assert_eq!(
            preview_attach_kubectl(&no_container_spec),
            "kubectl --context prod-eu -n payments attach pod/api-7f8b9-x12 -it"
        );
    }
}
