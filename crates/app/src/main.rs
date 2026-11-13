//! Fleetglass desktop app entry point.

// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// R-14b: mimalloc returns freed memory to the OS more eagerly than glibc's
// per-thread arenas, which kept idle RSS high after the initial watch sync.
// Covers both the desktop app and `fleetglass mcp`.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    // V-06: startup marker reference point; must stay the first statement.
    fleetglass_app::mark_process_start();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s == "mcp").unwrap_or(false) {
        let source = fleetglass_core::kubeconfig::KubeconfigSource::from_env()?;
        let manager = std::sync::Arc::new(fleetglass_core::cluster::ClusterManager::new(
            source.clone(),
        ));
        let audit_path =
            fleetglass_core::settings::SettingsStore::default_path().with_file_name("audit.jsonl");
        let audit = std::sync::Arc::new(fleetglass_core::audit::AuditLogStore::new(audit_path));
        return tauri::async_runtime::block_on(fleetglass_core::mcp::run_stdio_mcp_server(
            manager,
            source,
            Some(audit),
        ))
        .map_err(Into::into);
    }
    fleetglass_app::run()
}
