# Generated IPC types

`bindings.ts` is generated from the Rust IPC types in `crates/core/src/api/` and
the commands in `crates/app/src/lib.rs` (specta + tauri-specta, see
`docs/adr/0001-ipc-type-generation.md`). Do not hand-write or edit files here.

Regenerate: `cargo run -p fleetglass-app --bin gen-types`

`cargo test -p fleetglass-app` fails if the committed file is out of date.
