# ADR 0001: IPC type generation (Rust → TypeScript)

- Status: accepted
- Date: 2026-10-31
- Task: P0-05

## Context

Project architecture rules require that every IPC type lives in `crates/core/src/api/`, derives
`serde`, and has its TypeScript type **generated**, never hand-written. The UI
(`ui/`) calls Tauri 2 commands defined in `crates/app`. We want:

1. Types *and* command signatures generated, so a renamed field or command breaks
   `pnpm typecheck` instead of failing at runtime.
2. `crates/core` stays free of Tauri (it will also back a CLI/TUI/MCP later).
3. Reproducible output that is committed and checked in CI.
4. Support for what is coming: Tauri channels (`tauri::ipc::Channel`) for batched
   row/log deltas, events, serde attributes (`rename_all`, tagged enums).
5. Permissive licences (Apache-2.0/MIT/BSD).

## Options

Versions checked on crates.io on 2026-10-31.

### A. `specta` + `tauri-specta` (+ `specta-typescript`)
- `specta 2.0.0-rc.26/27`, `tauri-specta 2.0.0-rc.26` (released 2026-10-21, depends on
  `tauri ^2`), `specta-typescript 0.0.13/0.0.14`. All MIT.
- The 2.x line has been a release candidate since 2024 (rc.20 Sep 2024 … rc.26 Oct 2026);
  the stable 1.x line does not support Tauri 2. Upstream says to pin with `=`.
- Generates types **and** a typed `commands` object (`commands.appInfo()`), event
  helpers and `Channel<T>` typing. Reads serde attributes.
- Core only needs `specta` with `derive` (no Tauri). `tauri-specta` lives in `crates/app`.
- Pin note: `tauri-specta 2.0.0-rc.26` requires `specta =2.0.0-rc.26` and
  `specta-typescript ^0.0.13`; `specta-typescript 0.0.14` already requires `specta rc.27`,
  so we pin `specta =2.0.0-rc.26` and `specta-typescript =0.0.13` until tauri-specta moves.

### B. `ts-rs`
- `ts-rs 12.0.1` (stable, Jan 2026), MIT. No Tauri dependency at all.
- Generates **types only**, one file per type, via `#[ts(export)]` + `cargo test`.
- Command names and argument/return types would be hand-written `invoke("app_info")`
  wrappers in TS — exactly the drift we want to rule out — or we would write our own
  command-signature generator.
- No awareness of Tauri `Channel<T>`.

### C. Hand-written types / JSON Schema (`schemars`) + codegen
- Rejected: violates project rules (no hand-written types), or adds a second toolchain step for
  less than A gives.

## Decision

**Option A: `specta` + `tauri-specta` + `specta-typescript`, exact-pinned.**

- `crates/core`: optional feature `specta` adds `specta = "=2.0.0-rc.26"` (derive).
  IPC types use `#[cfg_attr(feature = "specta", derive(specta::Type))]`.
- `crates/app`: enables `fleetglass-core/specta`, depends on `tauri-specta`
  (`derive`, `typescript`) and `specta-typescript`. Commands are annotated with
  `#[tauri::command] #[specta::specta]` and registered once in
  `specta_builder()`, which feeds both the runtime invoke handler and export.
- Output: one file, `ui/src/generated/bindings.ts`, with a "DO NOT EDIT" header.
  Regenerate with `cargo run -p fleetglass-app --bin gen-types`.
- Drift check: the unit test `generated_bindings_are_up_to_date` in `crates/app`
  regenerates into a temp dir and fails if the committed file differs, so
  `cargo test --workspace` (CI) catches stale bindings. We do not regenerate on app
  start (the upstream example does that in debug builds) because a running app should
  not write into the source tree.

The choice is mainly about command signatures: with A the UI calls
`commands.appInfo()` and both the name and the return type come from Rust.

## Consequences

- RC risk: specta 2 is still not stable after ~2 years. Mitigations: `=` pins in
  both crates (they must match; Cargo errors loudly if not), upgrade deliberately in
  one PR with the regenerated diff, and the drift test. Usage is confined to derive
  attributes and one builder function, so a fallback to ts-rs (types) plus a small
  hand-rolled command list would touch few lines.
- `crates/core` gains an optional, Tauri-free dependency (`specta` + `specta-macros`
  only). Without the feature, core builds as before.
- Every new IPC type must add the `cfg_attr` derive; every new command must be added to
  `collect_commands!` in `crates/app/src/lib.rs` and the bindings regenerated.
- specta-typescript refuses to export BigInt-sized integers (`u64`, `i64`, `u128`,
  `usize`…) and fails the export with an error. IPC fields should use `u32`/`i32`/`f64`, or a string
  when lossless 64-bit values are needed (e.g. resourceVersion). Decide per type.
- Licences: specta, specta-typescript, specta-serde, specta-util, tauri-specta: MIT.
  ts-rs (not used): MIT. Tauri: Apache-2.0 OR MIT. All compatible with Apache-2.0.
