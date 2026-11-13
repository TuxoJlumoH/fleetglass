# Contributing to Fleetglass

Thanks for helping. Fleetglass is Apache-2.0 and stays that way; see [GOVERNANCE.md](GOVERNANCE.md).

## Before you start
- Read [docs/PLAN.md](docs/PLAN.md) (what we build and why) and the safety rules below (cluster safety, non-negotiables, coding conventions).
- For larger changes, open an issue first. UI redesigns and architectural changes go through an RFC (an issue labelled `rfc`).
- Be kind. We follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Developer Certificate of Origin (DCO), no CLA
We do not use a Contributor Licence Agreement. Instead every commit must be signed off under the
[Developer Certificate of Origin 1.1](https://developercertificate.org/):

```sh
git commit -s -m "feat(core): add windowed pod query"
```

This adds a `Signed-off-by: Your Name <you@example.com>` line, certifying that you wrote the change
or have the right to submit it under the project licence. Use your real name. Forgot it?
`git commit --amend -s` (last commit) or `git rebase --signoff main` (whole branch).
You keep your copyright; contributions are licensed under Apache-2.0 (section 5 of the licence).

## Commit messages: Conventional Commits
Format: `type(scope): summary` ([spec](https://www.conventionalcommits.org/en/v1.0.0/)).

- Types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`
- Scopes: `core`, `app`, `ui`, `bench`, `scripts`, `ci`, `docs`, `deps`
- Breaking changes: `feat(core)!: …` and a `BREAKING CHANGE:` footer.

## Setup
```sh
scripts/bootstrap.sh --check     # every tool and its version; exits non-zero if something is missing
scripts/bootstrap.sh --install   # installs user-space tools into ~/.local/bin, prints root commands
```
The script never runs `sudo`; it prints the system-package commands for you to run.

### Windows and macOS
`scripts/bootstrap.sh` targets Linux and macOS shells. To build and run the app on any platform you
need:
- **Rust** via [rustup](https://rustup.rs) (the version in `rust-toolchain.toml` is picked up
  automatically), **Node.js 24** (20.19 or newer works) and **pnpm 12.16.1** (`npm i -g pnpm@12.16.1`; the
  lockfile needs pnpm 12).
- **macOS:** Xcode Command Line Tools (`xcode-select --install`).
- **Windows:** Visual Studio Build Tools with "Desktop development with C++" and the WebView2
  runtime (preinstalled on Windows 10/11).

Then, from the repository root:
```sh
pnpm -C ui install
pnpm -C ui tauri dev                          # run in development mode
pnpm -C ui tauri build                        # release bundles in target/release/bundle/
```
Fleetglass reads your normal kubeconfig (`KUBECONFIG` or `~/.kube/config`), like `kubectl`.

### Android toolchain (v0.3, optional)
Only needed for the Android work (Tauri mobile). Everything installs in user space; nothing needs
root except KVM access for the emulator. About 7.4 GB (`~/Android` 7.0 GB, JDK 350 MB).

| Component | Version | Path |
|---|---|---|
| Temurin JDK | 21.0.13+9 | `~/.local/opt/jdk-21` |
| Android cmdline-tools | 12.0 | `~/Android/Sdk/cmdline-tools/latest` |
| platform-tools / emulator | 37.0.1 / 37.2.13 | `~/Android/Sdk` |
| platforms, build-tools | `android-35` + `android-36`, `36.0.0` (tauri's Android library needs compileSdk 36) | `~/Android/Sdk` |
| NDK | 27.2.12479018 (r27c LTS, clang 18) | `~/Android/Sdk/ndk/27.2.12479018` |
| System image | `android-35;google_apis;x86_64` | `~/Android/Sdk/system-images` |

```sh
# 1. JDK: Temurin 21 from adoptium.net (SHA-256 is on the release page / in the .sha256.txt asset)
mkdir -p ~/.local/opt && cd ~/.local/opt
curl -LO https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.13%2B9/OpenJDK21U-jdk_x64_linux_hotspot_21.0.13_9.tar.gz
echo "d7f5d5cf865a2902b70ff549b97586a7d3f234e016f041ab447ea740c7d66d1c  OpenJDK21U-jdk_x64_linux_hotspot_21.0.13_9.tar.gz" | sha256sum -c
tar xzf OpenJDK21U-jdk_x64_linux_hotspot_21.0.13_9.tar.gz && mv jdk-21.0.13+9 jdk-21

# 2. Command-line tools from developer.android.com/studio#command-line-tools-only
mkdir -p ~/Android/Sdk/cmdline-tools && cd ~/Android/Sdk/cmdline-tools
curl -LO https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip
echo "2d2d50857e4eb553af5a6dc3ad507a17adf43d115264b1afc116f95c92e5e258  commandlinetools-linux-11076708_latest.zip" | sha256sum -c
unzip -q commandlinetools-linux-11076708_latest.zip && mv cmdline-tools latest

# 3. Environment (or `. .dev/android-env.sh`; .dev/ is gitignored)
export JAVA_HOME="$HOME/.local/opt/jdk-21"
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"
# Only if ~/.gradle, ~/.android or ~/.cargo are not writable (e.g. sandboxed agents):
# export GRADLE_USER_HOME="$PWD/.dev/gradle" ANDROID_USER_HOME="$PWD/.dev/android_home"
# export CARGO_HOME="$PWD/.dev/cargo-home"   # copy of ~/.cargo; must be persistent, see below

# 4. SDK packages (this accepts the Android SDK licence for your local install)
yes | sdkmanager --licenses
sdkmanager "platform-tools" "platforms;android-35" "platforms;android-36" "build-tools;36.0.0" \
  "ndk;27.2.12479018" "emulator" "system-images;android-35;google_apis;x86_64"

# 5. Rust targets (`tauri android init` needs all four) and an emulator
rustup target add aarch64-linux-android x86_64-linux-android armv7-linux-androideabi i686-linux-android
avdmanager create avd -n fg-pixel -d pixel_6 -k "system-images;android-35;google_apis;x86_64"
```

The emulator needs `/dev/kvm` (`emulator -accel-check`). On Arch the udev rule creates it as
`root:kvm 0666` once the module is loaded; if it is missing or not accessible, run as root:
`sudo modprobe kvm_amd` (or `kvm_intel`) and `sudo usermod -aG kvm "$USER"`, then log in again.
Quick headless boot check: `emulator -avd fg-pixel -no-window -no-audio -no-snapshot &`,
`adb wait-for-device shell getprop sys.boot_completed` (prints `1`), then `adb emu kill`.

#### Building and running the Android app
The Android project is committed in `crates/app/gen/android`. Build only through the Tauri CLI
(running `./gradlew` directly fails because the CLI provides the Rust build step):

```sh
pnpm -C ui tauri android build -t x86_64 --apk --debug          # emulator, debug
pnpm -C ui tauri android build -t aarch64 -t x86_64 --apk --split-per-abi   # release, unsigned
adb install -r crates/app/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

The CLI rewrites the gitignored `gen/android/tauri.settings.gradle` to point at tauri's Android
library inside `$CARGO_HOME/registry`, so `CARGO_HOME` must not live in a directory that is wiped.

Until kubeconfig import exists (task 03-02), the app reads `files/.kube/config` in its private
storage. For the kind cluster, the API address must be reachable from the emulator (set
`FG_KIND_API_ADDRESS` to the host LAN IP; `127.0.0.1` is the emulator itself):

```sh
# Streams the file straight into the app sandbox (nothing left in /data/local/tmp).
# run-as only works for debug builds.
adb exec-in run-as dev.fleetglass.Fleetglass sh -c \
  'mkdir -p files/.kube && umask 077 && cat > files/.kube/config' < .dev/kubeconfig
```

Core unit tests on the emulator (fixtures are pushed because the binary runs without the source tree):

```sh
. .dev/android-env.sh   # NDK linker variables for cargo --target *-linux-android
B=$(cargo test -p fleetglass-core --lib --target x86_64-linux-android --no-run 2>&1 \
  | grep -oE 'target/x86_64-linux-android/debug/deps/fleetglass_core-[a-f0-9]+' | tail -1)
adb push "$B" /data/local/tmp/fg-core-tests && adb push crates/core/tests/fixtures/. /data/local/tmp/fixtures/
adb shell 'cd /data/local/tmp && FG_TEST_FIXTURES=/data/local/tmp/fixtures ./fg-core-tests'
```

Plain `cargo build --target <android triple>` needs the NDK compiler variables
(`CC_<triple>`, `AR_<triple>`, `CARGO_TARGET_<TRIPLE>_LINKER` → `$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/<triple>35-clang` / `llvm-ar`);
the Tauri CLI sets them itself.

## Commands
| Purpose | Command |
|---|---|
| Rust check | `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` |
| Rust tests | `cargo test --workspace` |
| Integration tests (needs kind) | `cargo test --workspace --features integration` |
| UI | `pnpm -C ui install && pnpm -C ui lint && pnpm -C ui typecheck && pnpm -C ui test` |
| Run app | `pnpm -C ui tauri dev` |
| Test cluster | `scripts/dev-cluster.sh up` / `down` |
| Perf benchmark | `cargo run -p bench --release -- --pods 50000` |

## Testing against clusters
- Use only the local test cluster from `scripts/dev-cluster.sh` with `KUBECONFIG=$PWD/.dev/kubeconfig`.
  Never point tests at a real cluster or at `~/.kube/config`.
- Performance budgets in [docs/PLAN.md §3](docs/PLAN.md) are acceptance criteria: a change that
  breaks one fails review.

## CI
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs the commands above on every PR and on
pushes to `main`: Rust fmt/clippy/test, UI lint/typecheck/test/build, a Tauri build (Linux on PRs; Linux,
macOS and Windows on pushes to `main`), integration tests against a throwaway kind cluster (skipped
on draft PRs), and a DCO check (every commit needs a matching `Signed-off-by`). It uses no secrets.
Actions are pinned to commit SHAs and Dependabot proposes updates; maintainers must enable
"Require contributors to sign off on web-based commits" in the repo settings so Dependabot commits
pass the DCO check. Lint workflow changes locally with
[actionlint](https://github.com/rhysd/actionlint).

## Pull requests
- Keep them focused; one concern per PR.
- Run the checks for every area you touched and paste the results in the PR description.
- Justify every new dependency (what it does, size, licence; must be Apache/MIT/BSD-compatible).
- No telemetry, no accounts, no network calls besides the user's API servers (and a future opt-in
  update check), and no AI features in the core. PRs that add these will be declined.
- All IPC types are generated (serde + specta/ts-rs); never hand-write TypeScript copies.

## Reporting security issues
Do not open a public issue. See [SECURITY.md](SECURITY.md).
