//! External terminal emulator detection, command preparation, and argument escaping (`04-10`).
//!
//! Provides safe command construction and execution for launching external terminal
//! emulators (Ghostty, Kitty, Alacritty, WezTerm, Foot, Konsole, GNOME Terminal,
//! XFCE Terminal, macOS Terminal.app / iTerm.app, Windows Terminal / PowerShell)
//! with argument escaping to prevent shell injection.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::api::ContextSettings;
use crate::error::Result;

/// Known terminal emulator types supported across Linux, macOS, and Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum TerminalEmulatorType {
    // Linux
    Ghostty,
    Kitty,
    Alacritty,
    Wezterm,
    Foot,
    Konsole,
    GnomeTerminal,
    Xfce4Terminal,
    // macOS
    ITerm2,
    MacTerminal,
    // Windows
    WindowsTerminal,
    PowerShell,
    // Fallback
    Custom,
}

impl TerminalEmulatorType {
    /// Friendly display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Ghostty => "Ghostty",
            Self::Kitty => "Kitty",
            Self::Alacritty => "Alacritty",
            Self::Wezterm => "WezTerm",
            Self::Foot => "Foot",
            Self::Konsole => "Konsole",
            Self::GnomeTerminal => "GNOME Terminal",
            Self::Xfce4Terminal => "XFCE Terminal",
            Self::ITerm2 => "iTerm2",
            Self::MacTerminal => "Terminal.app",
            Self::WindowsTerminal => "Windows Terminal",
            Self::PowerShell => "PowerShell",
            Self::Custom => "Custom Terminal",
        }
    }
}

/// Information about a detected or configured terminal emulator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub struct TerminalEmulatorInfo {
    pub emulator_type: TerminalEmulatorType,
    pub name: String,
    pub path: String,
}

/// Targets for launching an external terminal session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExternalTerminalTarget {
    /// Pod exec session (`kubectl exec -it <pod> -c <container> -- <shell>`).
    PodExec {
        cluster: String,
        namespace: String,
        pod: String,
        container: Option<String>,
        shell: Option<String>,
    },
    /// Node host debug shell (`kubectl debug node/<node> -it --image=<image> ...`).
    NodeDebug {
        cluster: String,
        namespace: Option<String>,
        node: String,
        image: Option<String>,
    },
    /// Context shell with `KUBECONFIG` and context configured.
    ContextShell {
        cluster: String,
        namespace: Option<String>,
    },
}

impl ExternalTerminalTarget {
    /// Target context name for write-safety checks.
    pub fn context(&self) -> &str {
        match self {
            Self::PodExec { cluster, .. } => cluster,
            Self::NodeDebug { cluster, .. } => cluster,
            Self::ContextShell { cluster, .. } => cluster,
        }
    }

    /// Target resource name for protected-context confirmation.
    pub fn target_name(&self) -> &str {
        match self {
            Self::PodExec { pod, .. } => pod,
            Self::NodeDebug { node, .. } => node,
            Self::ContextShell { cluster, .. } => cluster,
        }
    }

    /// Target resource kind for audit logging.
    pub fn resource_kind(&self) -> &'static str {
        match self {
            Self::PodExec { .. } => "Pod",
            Self::NodeDebug { .. } => "Node",
            Self::ContextShell { .. } => "Context",
        }
    }

    /// Namespace if applicable.
    pub fn namespace(&self) -> Option<&str> {
        match self {
            Self::PodExec { namespace, .. } => Some(namespace.as_str()),
            Self::NodeDebug { namespace, .. } => namespace.as_deref(),
            Self::ContextShell { namespace, .. } => namespace.as_deref(),
        }
    }
}

/// Quotes a single argument for POSIX shells (`sh`, `bash`, `zsh`) preventing injection.
///
/// Wraps in single quotes (`'...'`), replacing `'` with `'\''`. Safe characters
/// `[a-zA-Z0-9_\-./:=+@,]` without special characters are returned unchanged if non-empty.
pub fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:=+@,".contains(c))
    {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 4);
    quoted.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

/// Quotes and joins multiple shell arguments with a single space.
pub fn shell_join<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|s| shell_quote(s.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Checks if a binary exists in `PATH`.
pub fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = candidate.metadata()
                    && meta.permissions().mode() & 0o111 != 0
                {
                    return Some(candidate);
                }
            }
            #[cfg(not(unix))]
            {
                return Some(candidate);
            }
        }
    }
    None
}

/// Detects available terminal emulators in preference order.
pub fn detect_available_terminal_emulators() -> Vec<TerminalEmulatorInfo> {
    let mut emulators = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let iterm_paths = [
            "/Applications/iTerm.app",
            "/Applications/iTerm 2.app",
            "~/Applications/iTerm.app",
        ];
        for p in iterm_paths {
            let path = Path::new(p);
            if path.exists() {
                emulators.push(TerminalEmulatorInfo {
                    emulator_type: TerminalEmulatorType::ITerm2,
                    name: "iTerm2".to_string(),
                    path: p.to_string(),
                });
                break;
            }
        }
        emulators.push(TerminalEmulatorInfo {
            emulator_type: TerminalEmulatorType::MacTerminal,
            name: "Terminal.app".to_string(),
            path: "/System/Applications/Utilities/Terminal.app".to_string(),
        });
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(path) = find_in_path("wt.exe") {
            emulators.push(TerminalEmulatorInfo {
                emulator_type: TerminalEmulatorType::WindowsTerminal,
                name: "Windows Terminal".to_string(),
                path: path.to_string_lossy().to_string(),
            });
        }
        if let Some(path) = find_in_path("powershell.exe") {
            emulators.push(TerminalEmulatorInfo {
                emulator_type: TerminalEmulatorType::PowerShell,
                name: "PowerShell".to_string(),
                path: path.to_string_lossy().to_string(),
            });
        }
    }

    // Linux & Unix preferred list
    let candidates: &[(TerminalEmulatorType, &str, &str)] = &[
        (TerminalEmulatorType::Ghostty, "ghostty", "Ghostty"),
        (TerminalEmulatorType::Kitty, "kitty", "Kitty"),
        (TerminalEmulatorType::Alacritty, "alacritty", "Alacritty"),
        (TerminalEmulatorType::Wezterm, "wezterm", "WezTerm"),
        (TerminalEmulatorType::Foot, "foot", "Foot"),
        (TerminalEmulatorType::Konsole, "konsole", "Konsole"),
        (
            TerminalEmulatorType::GnomeTerminal,
            "gnome-terminal",
            "GNOME Terminal",
        ),
        (
            TerminalEmulatorType::Xfce4Terminal,
            "xfce4-terminal",
            "XFCE Terminal",
        ),
    ];

    for (etype, binary, display) in candidates {
        if let Some(path) = find_in_path(binary) {
            emulators.push(TerminalEmulatorInfo {
                emulator_type: *etype,
                name: (*display).to_string(),
                path: path.to_string_lossy().to_string(),
            });
        }
    }

    emulators
}

/// Returns the primary detected terminal emulator, or `None` if none is installed.
pub fn detect_primary_terminal_emulator() -> Option<TerminalEmulatorInfo> {
    detect_available_terminal_emulators().into_iter().next()
}

/// Builds the kubectl or shell command string and arguments for the given target.
pub fn preview_external_terminal_command(
    target: &ExternalTerminalTarget,
    kubeconfig_paths: Option<&str>,
) -> (String, Vec<String>, BTreeMap<String, String>) {
    let mut env = BTreeMap::new();
    if let Some(paths) = kubeconfig_paths
        && !paths.is_empty()
    {
        env.insert("KUBECONFIG".to_string(), paths.to_string());
    }

    match target {
        ExternalTerminalTarget::PodExec {
            cluster,
            namespace,
            pod,
            container,
            shell,
        } => {
            let sh = shell.as_deref().unwrap_or("/bin/sh");
            let mut args = vec![
                "kubectl".to_string(),
                "--context".to_string(),
                cluster.clone(),
                "-n".to_string(),
                namespace.clone(),
                "exec".to_string(),
                "-it".to_string(),
                pod.clone(),
            ];
            if let Some(c) = container
                && !c.is_empty()
            {
                args.push("-c".to_string());
                args.push(c.clone());
            }
            args.push("--".to_string());
            args.push(sh.to_string());

            let preview = shell_join(&args);
            (preview, args, env)
        }
        ExternalTerminalTarget::NodeDebug {
            cluster,
            namespace,
            node,
            image,
        } => {
            let mut args = vec![
                "kubectl".to_string(),
                "--context".to_string(),
                cluster.clone(),
            ];
            if let Some(ns) = namespace
                && !ns.is_empty()
            {
                args.push("-n".to_string());
                args.push(ns.clone());
            }
            args.push("debug".to_string());
            args.push(format!("node/{node}"));
            args.push("-it".to_string());
            if let Some(img) = image
                && !img.is_empty()
            {
                args.push(format!("--image={img}"));
            } else {
                args.push("--image=busybox".to_string());
            }

            let preview = shell_join(&args);
            (preview, args, env)
        }
        ExternalTerminalTarget::ContextShell { cluster, namespace } => {
            env.insert("KUBECONTEXT".to_string(), cluster.clone());
            if let Some(ns) = namespace
                && !ns.is_empty()
            {
                env.insert("KUBENAMESPACE".to_string(), ns.clone());
            }
            let shell_bin = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            let args = vec![shell_bin];
            let preview = format!(
                "KUBECONTEXT={} {} (KUBECONFIG={})",
                shell_quote(cluster),
                shell_quote(&args[0]),
                kubeconfig_paths.unwrap_or("~/.kube/config")
            );
            (preview, args, env)
        }
    }
}

/// Constructs a `std::process::Command` to spawn the emulator running the command.
pub fn build_terminal_spawn_command(
    emulator: &TerminalEmulatorInfo,
    command_args: &[String],
    env: &BTreeMap<String, String>,
) -> Command {
    let mut cmd = match emulator.emulator_type {
        TerminalEmulatorType::Ghostty => {
            let mut c = Command::new(&emulator.path);
            c.arg("-e");
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::Kitty => {
            let mut c = Command::new(&emulator.path);
            c.arg("--");
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::Alacritty => {
            let mut c = Command::new(&emulator.path);
            c.arg("-e");
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::Wezterm => {
            let mut c = Command::new(&emulator.path);
            c.arg("start");
            c.arg("--");
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::Foot => {
            let mut c = Command::new(&emulator.path);
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::Konsole => {
            let mut c = Command::new(&emulator.path);
            c.arg("-e");
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::GnomeTerminal => {
            let mut c = Command::new(&emulator.path);
            c.arg("--");
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::Xfce4Terminal => {
            let mut c = Command::new(&emulator.path);
            c.arg("-e");
            let joined = shell_join(command_args);
            c.arg(joined);
            c
        }
        TerminalEmulatorType::ITerm2 | TerminalEmulatorType::MacTerminal => {
            let mut c = Command::new("open");
            c.arg("-a");
            c.arg(&emulator.name);
            c.arg("--args");
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::WindowsTerminal => {
            let mut c = Command::new(&emulator.path);
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
        TerminalEmulatorType::PowerShell => {
            let mut c = Command::new(&emulator.path);
            c.arg("-NoExit");
            c.arg("-Command");
            let joined = shell_join(command_args);
            c.arg(joined);
            c
        }
        TerminalEmulatorType::Custom => {
            let mut c = Command::new(&emulator.path);
            for arg in command_args {
                c.arg(arg);
            }
            c
        }
    };

    for (k, v) in env {
        cmd.env(k, v);
    }

    cmd
}

/// Enforces write safety and validates that opening an external terminal is allowed.
pub fn validate_external_terminal_allowed(
    target: &ExternalTerminalTarget,
    settings: &ContextSettings,
    confirmed_name: Option<&str>,
) -> Result<()> {
    crate::actions::enforce_write_safety(
        target.context(),
        settings,
        target.target_name(),
        confirmed_name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_quote_safe_inputs_untouched() {
        assert_eq!(shell_quote("normal"), "normal");
        assert_eq!(shell_quote("pod-123_abc.xyz"), "pod-123_abc.xyz");
        assert_eq!(shell_quote("/bin/sh"), "/bin/sh");
        assert_eq!(shell_quote("key=value"), "key=value");
    }

    #[test]
    fn test_shell_quote_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn test_shell_quote_spaces_and_special_characters() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
        assert_eq!(shell_quote("arg with $VAR"), "'arg with $VAR'");
        assert_eq!(shell_quote("semi;colon"), "'semi;colon'");
        assert_eq!(shell_quote("pipe|cmd"), "'pipe|cmd'");
        assert_eq!(shell_quote("back`tick`"), "'back`tick`'");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    #[test]
    fn test_shell_quote_escapes_single_quotes_securely() {
        // 'foo'\''bar'
        assert_eq!(shell_quote("it's cool"), "'it'\\''s cool'");
        assert_eq!(shell_quote("a'b'c"), "'a'\\''b'\\''c'");
    }

    #[test]
    fn test_shell_join_quotes_multiple_arguments() {
        let args = vec![
            "kubectl",
            "exec",
            "-it",
            "pod;name",
            "--",
            "sh",
            "-c",
            "echo 'hi'",
        ];
        let joined = shell_join(&args);
        assert_eq!(
            joined,
            "kubectl exec -it 'pod;name' -- sh -c 'echo '\\''hi'\\'''"
        );
    }

    #[test]
    fn test_preview_external_terminal_pod_exec() {
        let target = ExternalTerminalTarget::PodExec {
            cluster: "prod-cluster".to_string(),
            namespace: "prod-ns".to_string(),
            pod: "web-pod".to_string(),
            container: Some("nginx".to_string()),
            shell: Some("/bin/bash".to_string()),
        };
        let (preview, args, env) =
            preview_external_terminal_command(&target, Some("/path/to/kubeconfig"));
        assert_eq!(
            preview,
            "kubectl --context prod-cluster -n prod-ns exec -it web-pod -c nginx -- /bin/bash"
        );
        assert_eq!(args.len(), 12);
        assert_eq!(
            env.get("KUBECONFIG"),
            Some(&"/path/to/kubeconfig".to_string())
        );
    }

    #[test]
    fn test_preview_external_terminal_node_debug() {
        let target = ExternalTerminalTarget::NodeDebug {
            cluster: "stage-cluster".to_string(),
            namespace: Some("kube-system".to_string()),
            node: "worker-01".to_string(),
            image: Some("alpine:latest".to_string()),
        };
        let (preview, args, _) = preview_external_terminal_command(&target, None);
        assert_eq!(
            preview,
            "kubectl --context stage-cluster -n kube-system debug node/worker-01 -it --image=alpine:latest"
        );
        assert_eq!(args[5], "debug");
        assert_eq!(args[6], "node/worker-01");
    }

    #[test]
    fn test_validate_external_terminal_safety_guards() {
        let target = ExternalTerminalTarget::PodExec {
            cluster: "prod".to_string(),
            namespace: "default".to_string(),
            pod: "important-pod".to_string(),
            container: None,
            shell: None,
        };

        // 1. Read-only context rejects
        let ro_settings = ContextSettings {
            read_only: true,
            ..Default::default()
        };
        assert!(validate_external_terminal_allowed(&target, &ro_settings, None).is_err());

        // 2. Protected context requires confirmation
        let protected_settings = ContextSettings {
            protected: true,
            ..Default::default()
        };
        assert!(validate_external_terminal_allowed(&target, &protected_settings, None).is_err());
        assert!(
            validate_external_terminal_allowed(&target, &protected_settings, Some("wrong-pod"))
                .is_err()
        );
        assert!(
            validate_external_terminal_allowed(&target, &protected_settings, Some("important-pod"))
                .is_ok()
        );

        // 3. Unlocked context permits write
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32;
        let unlocked_settings = ContextSettings {
            read_only: true,
            unlocked_until: Some(now + 900),
            ..Default::default()
        };
        assert!(validate_external_terminal_allowed(&target, &unlocked_settings, None).is_ok());
    }
}
