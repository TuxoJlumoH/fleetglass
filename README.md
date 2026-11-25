<p align="center">
  <img src="docs/assets/icon.png" width="112" alt="Fleetglass icon">
</p>

<h1 align="center">Fleetglass</h1>

<p align="center">
  <b>A fast, keyboard-first Kubernetes client for desktop and mobile.</b><br>
  Open source. Zero telemetry. No account. Offline-first architecture with hardware-backed encryption.
</p>

<p align="center">
  <a href="LICENSE"><img alt="Licence: Apache-2.0" src="https://img.shields.io/badge/licence-Apache--2.0-blue"></a>
  <a href="https://github.com/TuxoJlumoH/fleetglass/releases"><img alt="Version: 0.4.0" src="https://img.shields.io/badge/release-v0.4.0-green"></a>
  <img alt="Platforms: Linux | macOS | Windows | Android" src="https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows%20%7C%20Android-lightgrey">
  <img alt="Built with Tauri 2 + Rust" src="https://img.shields.io/badge/built%20with-Tauri%202%20%2B%20Rust-dea584">
</p>

<p align="center">
  <img src="docs/assets/demo.gif" width="900" alt="Fleetglass demo: Overview, filtering pods, command palette, deployments and the detail drawer">
</p>

---

## Highlights

- **Blazing fast at scale.** An in-memory Rust query engine projects and window-slices resources. Sorting, filtering, and scrolling across 50,000 pods stays under 50 ms.
- **Simultaneous multi-context exploration.** `Ctrl`/`⌘`+click across any combination of clusters in the sidebar. Merged tables display a pinned, color-coded Context column with independent per-cluster access indicators.
- **Strict write safety & audit trail.** Mutating actions preview their exact `kubectl` command before executing. Protected contexts enforce typed-name confirmation, read-only contexts can be temporarily unlocked for 15 minutes, and all actions are recorded in a local JSONL audit log.
- **Interactive debugging suite.** Launch ephemeral debug containers (`pods/ephemeralcontainers`), attach to PID 1, or start host-namespace Node debug shells (`node/<name>`). Handoff to your favourite native terminal emulator (`ghostty`, `kitty`, `alacritty`, `wezterm`, `iTerm2`, `wt.exe`) with shell-safe escaping.
- **Cloud auth & device code recovery.** Automatically detects expired sessions (`aws sso`, `gcloud`, `az`, `kubelogin`, OIDC), extracts verification URLs and user codes, and triggers seamless cluster reconnection.
- **Resource right-sizing.** Overview analyzes live container metrics against requests and limits to flag Near Limit (≥80%), Over Request (>100%), Unused Requests (<10%, <20%, <50%), and Missing Limits.
- **Standalone Android client.** Direct connection to your API servers with hardware-backed Keystore AES-256-GCM encryption, virtualized pull-to-refresh lists, touch ergonomics, and battery-friendly watcher management.

---

## Desktop Experience

| Multi-Cluster Overview Dashboard | Deep Resource Drawer & Live Inspection |
|:---:|:---:|
| <img src="docs/assets/desktop-overview.png" width="450" alt="Fleetglass Overview Dashboard"> | <img src="docs/assets/desktop-drawer.png" width="450" alt="Fleetglass Pod Summary Drawer"> |
| *Cluster capacity, warning events & right-sizing* | *Container metrics, condition chips & safe actions* |

| Crisp Light Theme | Keyboard-First Command Palette |
|:---:|:---:|
| <img src="docs/assets/desktop-light.png" width="450" alt="Fleetglass Light Theme"> | <img src="docs/assets/screenshot.png" width="450" alt="Fleetglass Command Palette"> |
| *Semantic tokens following system appearance* | *k9s-style keys (`j`/`k`, `/`, `s`, `r`) & `⌘K` palette* |

---

## Standalone Mobile Client (Android)

Fleetglass runs as a standalone native app on Android (API 29+ / Android 10+). It talks directly to your Kubernetes API servers — no intermediate relay, no cloud backend, and no third-party accounts. Kubeconfigs and credentials are encrypted on-device via the Android Keystore.

<p align="center">
  <img src="docs/assets/mobile-overview.png" width="215" alt="Android Overview">
  &nbsp;&nbsp;
  <img src="docs/assets/mobile-pods.png" width="215" alt="Android Pods List">
  &nbsp;&nbsp;
  <img src="docs/assets/mobile-detail.png" width="215" alt="Android Pod Detail Drawer">
  &nbsp;&nbsp;
  <img src="docs/assets/mobile-logs.png" width="215" alt="Android Live Streaming Logs">
</p>

<p align="center">
  <em>Cluster Health Overview · Virtualized Pods with Pull-to-Refresh · Container Details & Safe Actions · Live Log Streaming</em>
</p>

---

## Everyday Tools & Operations

- **Logs & Diagnostics:** Workload and single-pod log streaming, regex search, previous container log recovery, level filters (Error/Warn/Info/Debug), and bounded local export.
- **Arbitrary Compare / Diff:** Side-by-side YAML diffing comparing any two resources across clusters or namespaces with normalization toggles (mask generated hashes/status) and Secret data masking.
- **X.509 Certificate Inspector:** Automatic chain parsing, SAN listing, and validity tracking for TLS Secrets and cert-manager Certificates with expiring alert banners.
- **Port-Forward Manager:** Persistent saved configurations across restarts, live TCP connection counters, byte transfer telemetry, and audit logging.
- **Ecosystem CRD Views:** Built-in projections for Gateway API (`Gateway`, `HTTPRoute`, `GRPCRoute`), Argo CD (`Application`), Flux (`Kustomization`, `HelmRelease`), cert-manager (`Certificate`), and External Secrets (`ExternalSecret`).

---

## Installation

Download pre-built binaries from [Releases](https://github.com/TuxoJlumoH/fleetglass/releases):

| Platform | Package | Notes |
|---|---|---|
| **Android** (10+, API 29+) | `fleetglass-android-arm64-v8a.apk`<br>`fleetglass-android-x86_64.apk` | Standalone client. See [Android Verification Guide](docs/VERIFYING_APK.md). |
| **Linux** (x64 / ARM64) | `.AppImage`, `.deb`, `.rpm` (`amd64`/`x86_64` or `arm64`/`aarch64`) | Flatpak and AUR are planned for v0.5. |
| **macOS** (Apple Silicon / Intel) | `Fleetglass_<version>_aarch64.dmg` / `Fleetglass_<version>_x64.dmg` | Not notarized yet: run `xattr -dr com.apple.quarantine /Applications/Fleetglass.app`, or **System Settings → Privacy & Security → Open Anyway**. Intel builds start with v0.4.1. |
| **Windows** (10/11 x64) | `Fleetglass_<version>_x64-setup.exe` or `.msi` | SmartScreen warning on first launch: **More info → Run anyway**. |

> [!NOTE]
> Fleetglass loads your kubeconfig read-only (`KUBECONFIG` or `~/.kube/config`). It **never modifies** your original kubeconfig files.

---

## Build from Source

Requirements: Rust (stable), Node.js (v22+), and `pnpm` (v10+).

```sh
# Clone repository
git clone https://github.com/TuxoJlumoH/fleetglass.git
cd fleetglass

# Install UI dependencies & launch desktop dev mode
pnpm -C ui install
pnpm -C ui tauri dev

# Build release desktop bundles
pnpm -C ui tauri build
```

---

## Architecture

Fleetglass uses a split-process architecture designed for zero UI lag:

```
crates/core   Pure Rust Kubernetes engine: kubeconfig parser, reflector caches,
              query engine, streaming logs, exec, port-forward, and audit log.
crates/app    Tauri 2 desktop/mobile application shell with Specta-generated bindings.
crates/bench  Reproducible performance benchmark suite against synthetic & Kind clusters.
ui/           React 19 + TypeScript: virtualized windowed tables, CodeMirror 6 editor.
```

The frontend never receives unbounded JSON payloads; the Rust backend executes windowed queries (`offset`, `limit`, `sort`, `filter`) and pushes minimal diffs.

---

## Roadmap (Phase 0.5 & Beyond)

Next up for **v0.5.0** ([details](docs/PLAN.md#phase-05-distribution-packaging--daily-driver-polish-active-2026-11-13)):

- **Signed In-App Auto-Updater:** `tauri-plugin-updater` with ECDSA signature verification.
- **Expanded Packaging:** Flatpak, AUR (`PKGBUILD`), Homebrew Cask and winget.
- **Per-Namespace Watcher Fallback:** Graceful fallback to watching accessible namespaces on RBAC 403 Forbidden.
- **In-App PTY Terminal:** Embedded `xterm.js` with dynamic PTY window resize signals alongside external terminal handoff.
- **Split View Layout:** Resizable horizontal and vertical split panes for simultaneous side-by-side inspection.
- **Schema Validation & Autocomplete:** Kubernetes OpenAPI JSONSchema diagnostics inside the CodeMirror 6 YAML editor.

---

## How This Was Built

Fleetglass is a portfolio project and an experiment in directing AI coding agents. I wrote no
feature code by hand. My job was product owner, architect and reviewer:

- **Plan first.** I started from research into what people dislike about existing Kubernetes
  clients: freezes on big clusters, confusing empty lists and broken cloud auth. From that I wrote
  [`docs/PLAN.md`](docs/PLAN.md) with non-negotiables (no account, no telemetry, the backend owns
  the data, honest error states) and hard performance budgets. Key decisions are recorded as
  [ADRs](docs/adr/).
- **Small tasks with acceptance criteria.** The work was split into tasks with a clear "done when",
  scoped to specific crates or folders and grouped into phases (0.1 → 0.4) with a review gate at the
  end of each phase.
- **An orchestrator and workers.** An orchestrator agent picked tasks, briefed worker agents and
  checked their reports. Strong hosted models handled architecture-heavy tasks. Local open-weight
  models (Qwen, DeepSeek) handled smaller, well-specified ones.
- **Machines check, humans decide.** Every change had to pass `cargo fmt`, `clippy -D warnings`, unit
  tests, UI lint/typecheck/tests, `cargo deny`, and an integration job that boots a real kind cluster,
  scales it with kwok and fails the build if a performance budget is missed. An end-to-end smoke test
  drives the built app under Xvfb with synthetic input and OCR. I reviewed the results and the code,
  and sent work back when it didn't hold up.
- **Agents never touch real clusters.** They only get a throwaway kind cluster and its kubeconfig.

The public history starts at a squashed snapshot of v0.4. Commits since then are regular.

---

## Contributing

Contributions are welcome! Please sign off your commits with `git commit -s` (Developer Certificate of Origin; no CLA required).

- Read [CONTRIBUTING.md](CONTRIBUTING.md) for local dev cluster setup and workflow rules.
- Read [GOVERNANCE.md](GOVERNANCE.md) and our [Code of Conduct](CODE_OF_CONDUCT.md).
- Report security concerns responsibly via [SECURITY.md](SECURITY.md).

---

## Licence

[Apache-2.0](LICENSE)
