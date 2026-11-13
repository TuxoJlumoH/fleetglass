//! Regenerates `ui/src/generated/bindings.ts` from the Rust IPC types and commands.
//!
//! Run: `cargo run -p fleetglass-app --bin gen-types`

fn main() -> anyhow::Result<()> {
    let path = fleetglass_app::bindings_path();
    fleetglass_app::export_bindings(&path)?;
    println!("wrote {}", path.display());
    Ok(())
}
