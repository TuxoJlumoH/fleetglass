//! The user's login-shell environment, for exec credential plugins.
//!
//! A desktop app started from a launcher (Dock, GNOME, Explorer) does not see the `PATH`,
//! `AWS_PROFILE`, `KUBECONFIG`, … that the user sets in `.zshrc`/`.bash_profile`, so exec
//! plugins such as `aws eks get-token` or `kubelogin` fail with "command not found".
//! We run `$SHELL -l -i -c …` once, capture its environment and use it for exec plugins.
//!
//! The captured output is delimited by sentinel markers (rc files may print banners) and is
//! NUL-separated (`env -0`) when supported. Values are never logged: they may hold secrets.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kube::config::{ExecConfig, Kubeconfig};
use tokio::sync::OnceCell;

/// Default time budget for the login shell to print its environment.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// Output larger than this is treated as a failure (a runaway rc file, not an environment).
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// Variables describing the capturing shell itself, not the user's environment.
const SKIP_VARS: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

static CACHED: OnceCell<ShellEnv> = OnceCell::const_new();

/// Where a [`ShellEnv`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvSource {
    /// Captured from the user's login shell.
    LoginShell { shell: PathBuf },
    /// The app's own process environment; `reason` says why the shell was not used.
    ProcessEnv { reason: String },
}

/// Why capturing the login-shell environment failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShellEnvError {
    #[error("$SHELL is not set")]
    NoShell,
    #[error("could not start {}: {source}", .shell.display())]
    Spawn {
        shell: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading the shell output failed: {0}")]
    Read(#[source] std::io::Error),
    #[error("the shell did not print its environment within {} ms", .0.as_millis())]
    Timeout(Duration),
    #[error("the shell printed more than {MAX_OUTPUT_BYTES} bytes")]
    TooLarge,
    #[error("the shell output did not contain the environment markers")]
    MissingMarkers,
    #[error("login-shell capture is only supported on unix")]
    Unsupported,
}

/// An environment (name → value) to run exec credential plugins with.
#[derive(Clone)]
pub struct ShellEnv {
    vars: HashMap<String, String>,
    source: EnvSource,
}

// Manual impl: never print values.
impl fmt::Debug for ShellEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellEnv")
            .field("vars", &format_args!("<{} variables>", self.vars.len()))
            .field("source", &self.source)
            .finish()
    }
}

impl ShellEnv {
    /// The login-shell environment, captured once per process and cached.
    /// Never fails: falls back to the process environment.
    pub async fn get() -> &'static ShellEnv {
        CACHED.get_or_init(Self::capture).await
    }

    /// Capture now (uncached) from `$SHELL` with [`DEFAULT_TIMEOUT`], falling back to the
    /// process environment on any failure.
    pub async fn capture() -> ShellEnv {
        if !cfg!(unix) || cfg!(target_os = "android") {
            return Self::from_process(ShellEnvError::Unsupported.to_string());
        }
        let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) else {
            return Self::fallback(&ShellEnvError::NoShell);
        };
        Self::capture_from(Path::new(&shell), DEFAULT_TIMEOUT).await
    }

    /// Capture from a specific shell binary (falls back to the process environment).
    pub async fn capture_from(shell: &Path, timeout: Duration) -> ShellEnv {
        match capture_vars(shell, timeout).await {
            Ok(vars) => {
                tracing::info!(
                    shell = %shell.display(),
                    count = vars.len(),
                    "captured login shell environment"
                );
                ShellEnv {
                    vars,
                    source: EnvSource::LoginShell {
                        shell: shell.to_path_buf(),
                    },
                }
            }
            Err(err) => Self::fallback(&err),
        }
    }

    fn fallback(err: &ShellEnvError) -> ShellEnv {
        tracing::warn!(
            reason = %err,
            "login shell environment unavailable; exec plugins use the process environment"
        );
        Self::from_process(err.to_string())
    }

    /// The current process environment (non-UTF-8 entries are dropped).
    pub fn from_process(reason: impl Into<String>) -> ShellEnv {
        let vars = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        ShellEnv {
            vars,
            source: EnvSource::ProcessEnv {
                reason: reason.into(),
            },
        }
    }

    /// Build from explicit variables (tests, or a caller with its own environment).
    pub fn from_vars(vars: HashMap<String, String>, source: EnvSource) -> ShellEnv {
        ShellEnv { vars, source }
    }

    pub fn vars(&self) -> &HashMap<String, String> {
        &self.vars
    }

    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    pub fn source(&self) -> &EnvSource {
        &self.source
    }

    /// Add this environment to an exec plugin config. Variables the kubeconfig sets
    /// explicitly in `exec.env` keep their kubeconfig value. Because `PATH` is set on the
    /// child, the plugin command is also looked up in the shell's `PATH`.
    pub fn apply_to_exec(&self, exec: &mut ExecConfig) {
        let explicit: Vec<HashMap<String, String>> = exec.env.take().unwrap_or_default();
        let is_explicit = |name: &str| {
            explicit
                .iter()
                .any(|e| e.get("name").is_some_and(|n| n == name))
        };
        let mut names: Vec<&String> = self
            .vars
            .keys()
            .filter(|k| !SKIP_VARS.contains(&k.as_str()) && !is_explicit(k))
            .collect();
        names.sort();
        let mut env: Vec<HashMap<String, String>> = names
            .into_iter()
            .map(|name| {
                HashMap::from([
                    ("name".to_owned(), name.clone()),
                    ("value".to_owned(), self.vars[name].clone()),
                ])
            })
            .collect();
        env.extend(explicit);
        exec.env = Some(env);
    }

    /// [`apply_to_exec`](Self::apply_to_exec) for every user with an exec plugin.
    pub fn apply_to_kubeconfig(&self, config: &mut Kubeconfig) {
        for named in &mut config.auth_infos {
            if let Some(exec) = named.auth_info.as_mut().and_then(|a| a.exec.as_mut()) {
                self.apply_to_exec(exec);
            }
        }
    }
}

/// Run the shell and parse its environment. No fallback.
pub async fn capture_vars(
    shell: &Path,
    timeout: Duration,
) -> Result<HashMap<String, String>, ShellEnvError> {
    if !cfg!(unix) || cfg!(target_os = "android") {
        return Err(ShellEnvError::Unsupported);
    }
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;

    let nonce = nonce();
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &shell_command(&nonce)])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| ShellEnvError::Spawn {
            shell: shell.to_path_buf(),
            source,
        })?;

    let end = marker(&nonce, "END");
    let mut stdout = child.stdout.take().ok_or(ShellEnvError::MissingMarkers)?;
    // Stop at the end marker rather than EOF: background jobs started by rc files can keep
    // the pipe open long after the environment has been printed.
    let read = async {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = stdout.read(&mut chunk).await.map_err(ShellEnvError::Read)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > MAX_OUTPUT_BYTES {
                return Err(ShellEnvError::TooLarge);
            }
            if find(&buf, end.as_bytes()).is_some() {
                break;
            }
        }
        Ok(buf)
    };
    let result = tokio::time::timeout(timeout, read).await;

    // Give a finished shell a moment to exit on its own (logout scripts), then kill it.
    if tokio::time::timeout(Duration::from_millis(200), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
    }

    let buf = result.map_err(|_| ShellEnvError::Timeout(timeout))??;
    parse_marked_output(&buf, &nonce)
}

/// Command passed to `$SHELL -l -i -c`. It works in POSIX shells and fish alike: it only
/// `exec`s `/bin/sh` with a single-quoted script (no backslashes, so fish quoting matches).
/// The markers are printed as two halves so an echoed command line never matches them.
fn shell_command(nonce: &str) -> String {
    format!(
        "exec /bin/sh -c 'printf \"%s%s\" __FG_ENV_ {nonce}_BEGIN__; \
         env -0 2>/dev/null || env; \
         printf \"%s%s\" __FG_ENV_ {nonce}_END__'"
    )
}

fn marker(nonce: &str, which: &str) -> String {
    format!("__FG_ENV_{nonce}_{which}__")
}

fn nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("{:x}{:x}", std::process::id(), nanos)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Extract and parse the environment between the BEGIN and END markers.
fn parse_marked_output(
    output: &[u8],
    nonce: &str,
) -> Result<HashMap<String, String>, ShellEnvError> {
    let begin = marker(nonce, "BEGIN");
    let end = marker(nonce, "END");
    let start = find(output, begin.as_bytes()).ok_or(ShellEnvError::MissingMarkers)? + begin.len();
    let len = find(&output[start..], end.as_bytes()).ok_or(ShellEnvError::MissingMarkers)?;
    Ok(parse_env(&output[start..start + len]))
}

/// Parse `env -0` (NUL-separated) or plain `env` (newline-separated) output.
///
/// For newline output, a line that does not start with `NAME=` continues the previous
/// value (multi-line values). Entries with invalid names or non-UTF-8 bytes are skipped.
pub fn parse_env(raw: &[u8]) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    if raw.contains(&0) {
        for entry in raw.split(|b| *b == 0) {
            if let Some((k, v)) = std::str::from_utf8(entry).ok().and_then(split_entry) {
                vars.insert(k.to_owned(), v.to_owned());
            }
        }
        return vars;
    }

    let text = String::from_utf8_lossy(raw);
    let mut last: Option<String> = None;
    for line in text.lines() {
        if let Some((k, v)) = split_entry(line) {
            vars.insert(k.to_owned(), v.to_owned());
            last = Some(k.to_owned());
        } else if let Some(value) = last.as_ref().and_then(|k| vars.get_mut(k)) {
            value.push('\n');
            value.push_str(line);
        }
    }
    vars.retain(|_, v| !v.contains('\u{FFFD}'));
    vars
}

fn split_entry(entry: &str) -> Option<(&str, &str)> {
    let (name, value) = entry.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    let valid = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_separated_env_with_multiline_values() {
        let raw = b"A=1\0MULTI=line1\nline2\0EMPTY=\0EQ=a=b\0BASH_FUNC_f%%=() { :; }\0junk\0";
        let vars = parse_env(raw);
        assert_eq!(vars["A"], "1");
        assert_eq!(vars["MULTI"], "line1\nline2");
        assert_eq!(vars["EMPTY"], "");
        assert_eq!(vars["EQ"], "a=b");
        assert!(!vars.contains_key("BASH_FUNC_f%%"));
        assert_eq!(vars.len(), 4);
    }

    #[test]
    fn parses_newline_env_with_continuations() {
        let raw = b"PATH=/usr/bin:/bin\nMULTI=one\ntwo\nHOME=/home/u\n";
        let vars = parse_env(raw);
        assert_eq!(vars["PATH"], "/usr/bin:/bin");
        assert_eq!(vars["MULTI"], "one\ntwo");
        assert_eq!(vars["HOME"], "/home/u");
    }

    #[test]
    fn ignores_rc_file_garbage_outside_markers() {
        let nonce = "abc";
        let mut out = b"Welcome! FOO=bar\nfortune says hi\n".to_vec();
        out.extend_from_slice(b"__FG_ENV_abc_BEGIN__X=1\0Y=2\0__FG_ENV_abc_END__");
        out.extend_from_slice(b"bye\nZ=3\n");
        let vars = parse_marked_output(&out, nonce).unwrap();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars["X"], "1");
        assert_eq!(vars["Y"], "2");
    }

    #[test]
    fn missing_markers_is_an_error() {
        let err = parse_marked_output(b"A=1\0", "abc").unwrap_err();
        assert!(matches!(err, ShellEnvError::MissingMarkers));
    }

    #[test]
    fn debug_does_not_print_values() {
        let env = ShellEnv::from_vars(
            HashMap::from([("TOKEN".to_owned(), "s3cr3t".to_owned())]),
            EnvSource::ProcessEnv {
                reason: "test".into(),
            },
        );
        let dbg = format!("{env:?}");
        assert!(!dbg.contains("s3cr3t"));
        assert!(dbg.contains("<1 variables>"));
    }

    #[test]
    fn apply_to_exec_keeps_kubeconfig_values_and_skips_shell_vars() {
        let env = ShellEnv::from_vars(
            HashMap::from([
                ("PATH".to_owned(), "/opt/aws/bin:/usr/bin".to_owned()),
                ("AWS_PROFILE".to_owned(), "shell".to_owned()),
                ("PWD".to_owned(), "/tmp".to_owned()),
            ]),
            EnvSource::LoginShell {
                shell: "/bin/zsh".into(),
            },
        );
        let mut exec = ExecConfig {
            command: Some("aws".into()),
            env: Some(vec![HashMap::from([
                ("name".to_owned(), "AWS_PROFILE".to_owned()),
                ("value".to_owned(), "kubeconfig".to_owned()),
            ])]),
            ..Default::default()
        };
        env.apply_to_exec(&mut exec);
        let entries: Vec<(String, String)> = exec
            .env
            .unwrap()
            .into_iter()
            .map(|e| (e["name"].clone(), e["value"].clone()))
            .collect();
        assert_eq!(
            entries,
            [
                ("PATH".to_owned(), "/opt/aws/bin:/usr/bin".to_owned()),
                ("AWS_PROFILE".to_owned(), "kubeconfig".to_owned()),
            ]
        );
    }

    // Shell capture is disabled on Android (no login shells in the app sandbox).
    #[cfg(all(unix, not(target_os = "android")))]
    mod fake_shell {
        use super::super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;

        fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        // One test so the scripts are written (and closed) before anything is spawned,
        // avoiding ETXTBSY races with concurrently forking tests.
        #[tokio::test]
        async fn fake_shells_capture_garbage_timeout_and_fallback() {
            let dir = std::env::temp_dir().join(format!("fg-shell-env-{}", nonce()));
            std::fs::create_dir_all(&dir).unwrap();
            // Args are: -l -i -c <command>. Prints rc-file noise, then runs the command.
            let noisy = write_script(
                &dir,
                "noisy.sh",
                "#!/bin/sh\necho 'motd: FAKE=1'\nprintf 'no newline'\nFG_TEST_VAR='a b\nc' \
                 exec /bin/sh -c \"$4\"\n",
            );
            let slow = write_script(&dir, "slow.sh", "#!/bin/sh\nexec sleep 5\n");
            let failing = write_script(&dir, "fail.sh", "#!/bin/sh\necho oops\nexit 1\n");

            let vars = capture_vars(&noisy, DEFAULT_TIMEOUT).await.unwrap();
            assert_eq!(vars.get("FG_TEST_VAR").map(String::as_str), Some("a b\nc"));
            assert!(!vars.contains_key("FAKE"));
            assert!(vars.contains_key("PATH"));

            let started = Instant::now();
            let err = capture_vars(&slow, Duration::from_millis(300))
                .await
                .unwrap_err();
            assert!(matches!(err, ShellEnvError::Timeout(_)), "{err}");
            assert!(started.elapsed() < Duration::from_secs(2));

            let env = ShellEnv::capture_from(&failing, DEFAULT_TIMEOUT).await;
            assert!(matches!(env.source(), EnvSource::ProcessEnv { .. }));

            let env = ShellEnv::capture_from(&dir.join("missing"), DEFAULT_TIMEOUT).await;
            match env.source() {
                EnvSource::ProcessEnv { reason } => assert!(reason.contains("could not start")),
                other => panic!("unexpected source {other:?}"),
            }

            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
