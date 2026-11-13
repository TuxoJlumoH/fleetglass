# ADR 0004: Standalone Android app via Tauri 2 mobile

- Status: accepted
- Date: 2026-11-10 (updated 2026-11-11 for 03-06)
- Task: 03-01, 03-06

## Context

Phase 0.3 adds an Android app. The core non-negotiables still apply: no account, no
telemetry, no network calls except to the user's Kubernetes API servers, the backend owns the data,
and writes are previewed and guarded. The spike checks whether the existing Rust core and React UI
run on Android unchanged, and what it costs.

## Decisions

1. **Standalone app, no relay.** The phone talks directly to the API servers. `crates/app` builds as
   `staticlib`/`cdylib`/`rlib`; Tauri's Android shell loads `libfleetglass_app.so` and the same
   React UI runs in the system WebView. `mobile_main` (`#[tauri::mobile_entry_point]`) calls the
   same `run()` as desktop and logs its error.
2. **TLS:** kube-rs with rustls (no OpenSSL), as on desktop. With a kubeconfig CA
   (`certificate-authority[-data]`, the kind case) the server certificate is verified against that
   CA, and this works on the emulator. **Known gap:** without a kubeconfig CA, kube-client 4.2 uses
   `rustls-platform-verifier`, which on Android must first be initialised from the JVM side (plus
   its Kotlin component). Nothing does that today, so such a connection would panic
   (`Expect rustls-platform-verifier to be initialized`). 03-02 must either initialise it or
   refuse CA-less contexts on Android with a clear message.
3. **No shell environment, no exec plugins.** `shell_env` returns `Unsupported` on Android (there is
   no login shell in the app sandbox). Exec credential plugins cannot run; 03-02 rejects such
   contexts with a clear message. Supported auth: client certificate, bearer token, basic auth,
   OIDC refresh token.
4. **Kubeconfig location (spike only).** Android does not set `HOME` or `KUBECONFIG` for app
   processes. `MainActivity` sets `HOME` to the app data directory and `KUBECONFIG` to
   `<data>/files/.kube/config`. This is a stop-gap: 03-02 replaces it with import into
   Keystore-encrypted storage.
5. **Android project is committed** in `crates/app/gen/android` (from `tauri android init`, then
   edited: compileSdk 36 / build-tools 36.0.0 required by tauri 2.12's Android library, targetSdk
   35, minSdk 29, `MainActivity` env above). Builds go through `pnpm -C ui tauri android build`;
   the CLI generates the gitignored `tauri.settings.gradle` that points at tauri's Android library
   in `$CARGO_HOME`.
6. **Toolchain** (user space, see CONTRIBUTING.md): Temurin JDK 21, cmdline-tools, NDK 27.2.12479018
   (r27c), platforms android-35/36, build-tools 36.0.0, AVD `fg-pixel` (Android 15, x86_64).
7. **Minimum Android version Android 10+ (API 29+):**
   - **Modern WebView requirements:** Fleetglass frontend runs on React 19 and requires modern
     CSS features (CSS Grid, scrollbar-gutter, container queries), ES2020+ (BigInt, optional chaining,
     nullish coalescing), and WebCrypto subtle AES-GCM. Android 10+ uses decoupled Trichrome / modern
     system WebView updates and ensures modern Chromium engine standards without legacy Blink/V8 polyfills.
   - **Hardware-backed Keystore AES-GCM guarantees:** Android 10 (API 29) provides reliable
     hardware-backed Keystore primitives (`KeyGenParameterSpec` with AES-256-GCM authenticated
     encryption and TEE/StrongBox backing) for secure kubeconfig storage (task 03-02), eliminating
     vendor-specific Keystore corruption bugs prevalent on Android 7–9 (API 24–28).
   - **TLS 1.3 & network security:** TLS 1.3 is enabled by default in Conscrypt on API 29+, matching
     the desktop rustls client security profile. Cleartext traffic is disabled by default.
   - **Release signing & reproducible fallback:** `build.gradle.kts` supports release signing via
     standard environment variables (`KEYSTORE_PATH` or `KEYSTORE_BASE64`, `KEYSTORE_PASSWORD`,
     `KEY_ALIAS`, `KEY_PASSWORD`) and Gradle properties, cleanly falling back to unsigned builds for
     local developer builds and offline packaging.

## Measurements (2026-11-10, emulator `fg-pixel`, Android 15 x86_64, kind `fleetglass-dev`)

| What | Value |
|---|---|
| Release APK, unsigned, per ABI | arm64-v8a 42.9 MB (40.9 MiB), x86_64 42.7 MB (40.7 MiB) |
| Debug APK, x86_64 (unstripped) | ~796 MB; not representative |
| First Overview data after page load | 156 ms (1 cluster connected, 7 attention items) |
| Pods view | 23 rows of 23 total from `kind-fleetglass-dev` |
| Memory, debug build, Pods open, ~30 s idle (`dumpsys meminfo`) | PSS 164 MB, RSS 274 MB, native heap 21 MB, Java heap 14 MB |
| Core unit tests on the emulator (`x86_64-linux-android`) | 283 passed, 0 failed, 1 ignored |

Memory is from the debug build; a release-build measurement on a real arm64 phone is part of
03-GATE. The core compiles for `aarch64-linux-android` too; the arm64 APK has not been run yet
(no arm64 device or emulator in the spike).

## Consequences

- The desktop core and UI run on Android without forks; mobile-specific work is kubeconfig
  import/storage (03-02) and a mobile layout (03-03).
- Setting `minSdk = 29` (Android 10) sets an unambiguous baseline: any device capable of running
  Fleetglass has modern WebView CSS/JS engine support, hardware-backed Keystore AES-GCM encryption,
  and TLS 1.3 Conscrypt default support. Devices below Android 10 are not supported.
- CI and release packaging produce signed APKs (`arm64-v8a` and `x86_64`) with CycloneDX SBOM and
  SHA-256 checksums, enabling user verification via `apksigner` and `sha256sum`.
- A test binary run on the device cannot see the source tree, so core tests that read fixtures use
  `test_fixture()`, overridable with `FG_TEST_FIXTURES`.
- The committed `gen/android` project must be updated by hand when Tauri's template changes.
