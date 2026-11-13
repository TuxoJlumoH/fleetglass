fn main() {
    // `tauri::generate_context!` embeds `build.frontendDist` (../../ui/dist) and fails
    // to compile if it is missing. Create it empty so `cargo clippy/test` work on a
    // fresh clone before `pnpm -C ui build`; a real build fills it.
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    // create_dir_all is a no-op when the directory already exists.
    if let Err(e) = std::fs::create_dir_all(&dist) {
        println!("cargo::warning=could not create {}: {e}", dist.display());
    }
    tauri_build::build();
}
