//! Fleetglass core: kubeconfig, cluster manager, watch cache, row projection,
//! query engine, logs, exec, port-forward and diagnostics.
//!
//! This crate must not depend on Tauri.

pub mod actions;
pub mod api;
pub mod app_view;
pub mod apply;
pub mod audit;
pub mod builtin_kinds;
pub mod cert;
pub mod cluster;
pub mod columns;
pub mod detail;
pub mod diagnostics;
pub mod ecosystem;
pub mod error;
pub mod exec;
pub mod filecopy;
pub mod filter;
pub mod helm;
pub mod import;
pub mod kubeconfig;
pub mod logs;
pub mod logs_mux;
pub mod mcp;
pub mod metrics;
pub mod ns_fallback;
pub mod overview;
pub mod plugins;
pub mod portforward;
pub mod projection;
pub mod prometheus;
pub mod query;
pub mod settings;
pub mod shell_env;
pub mod team_settings;
pub mod terminal;

pub use error::{Error, Result};

/// Version of the core crate (and of the workspace).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Basic information about this build, exposed to the UI.
pub fn app_info() -> api::AppInfo {
    api::AppInfo {
        version: VERSION.to_owned(),
        platform: std::env::consts::OS.to_owned(),
    }
}

/// Path of a file in `crates/core/tests/fixtures/`. `FG_TEST_FIXTURES` overrides the
/// directory, for test binaries run where the source tree is absent (Android emulator).
#[cfg(test)]
pub(crate) fn test_fixture(name: &str) -> std::path::PathBuf {
    std::env::var_os("FG_TEST_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
        })
        .join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_reports_crate_version() {
        assert_eq!(app_info().version, env!("CARGO_PKG_VERSION"));
        assert_eq!(app_info().platform, std::env::consts::OS);
    }
}
