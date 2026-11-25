# Changelog

All notable changes to Fleetglass are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/) (0.x releases may change behaviour between minors).

## [Unreleased]

## [0.5.0] - 2026-11-25

### Added
- **Per-namespace watcher fallback:** when a cluster-wide list/watch returns 403, Fleetglass watches the namespaces you can access instead of showing an empty view.
  - The namespaces come from the new per-context `watchNamespaces` setting. Without it, Fleetglass uses the namespaces you may list, or the context's namespace.
  - Each one is checked with a `SelfSubjectRulesReview`.
  - A "Restricted: N namespaces" pill, the Overview and the MCP summary say exactly what's covered.
- **In-app terminal:** exec, attach and node-debug shells run in an embedded xterm.js terminal.
  - The remote PTY follows the window and pane size, so `vim`, `top` and `htop` render correctly.
  - Output is bounded by the scrollback, and colours follow the theme.
  - Links in the terminal open through the validated, audited browser launcher.
- **Split view:** split the main view horizontally (`Ctrl+\`) or vertically (`Ctrl+Shift+\`).
  - Each pane keeps its own query, live updates, scroll position and columns.
  - Drag a tab onto the second pane; `Ctrl+W` switches panes.
- **Schema validation in the YAML editor:**
  - Diagnostics, autocomplete and hover docs come from the cluster's own OpenAPI v3 schemas, so they cover built-in kinds and CRDs at the server's version.
  - If a cluster can't serve its schema, the editor shows a "Schema unavailable" note instead of guessing.
- **Open in browser:** one click opens an active port-forward or an Ingress / Gateway / HTTPRoute endpoint.
  - URLs are validated in the backend and opened without a shell.
  - Every launch is written to the audit log.
- **Release builds:** a macOS Intel (`x64`) dmg, Android APKs signed in CI with the release key (the certificate is verified), and draft GitHub Releases created from version tags.

### Changed
- Updated to kube 5.2 and k8s-openapi 0.29 (Kubernetes 1.37 API). The minimum supported Rust version is now 1.95.

### Fixed
- Previous logs of a crash-looping container whose old log was already garbage-collected now show a clear message instead of a raw kubelet error line.

## [0.4.0] - 2026-11-13

### Added
- **Simultaneous Multi-Context Exploration:** Multi-select any combination of Kubernetes contexts using `Ctrl/⌘+Click` in the sidebar or contexts switcher. View merged resources across clusters with a pinned color-coded `Context` column, independent per-cluster access indicators, and parallel query execution.
- **Cloud Auth & Device Code Recovery:** Automatic detection of expired cloud authentication sessions (`aws sso`, `gcloud`, `az`, `kubelogin`, OIDC tokens). Parses device login prompts (verification URLs and user codes) with one-click URL copy, automatic client invalidation, and seamless cluster reconnection in the Diagnostics modal.
- **Ephemeral Debug Containers, Attach & Node Debug Shell:** Interactive debugging suite supporting Kubernetes ephemeral debug containers (`pods/ephemeralcontainers`) with customizable image presets (`busybox`, `netshoot`, `alpine`, `curl`), target container process namespace sharing, container PID 1 attach, and node host debugging shell (`node/<name>`). All mutating actions require `enforce_write_safety` and write structured audit records.
- **Arbitrary Two-Resource Compare & Diff:** Side-by-side resource diffing comparing any two arbitrary resources across clusters, namespaces, or kinds. Features normalization toggles (mask generated hashes, metadata, runtime status) and automatic Secret `.data` masking with diff-only plaintext revelation.
- **Expanded Kubernetes Kinds & Ecosystem Projections:** Built-in support for Core and Ecosystem kinds: Ingress, NetworkPolicy, PersistentVolume, StorageClass, ServiceAccount, Role/RoleBinding, ClusterRole/ClusterRoleBinding, Gateway API (`Gateway`, `HTTPRoute`, `GRPCRoute`), Argo CD (`Application`, `AppProject`), Flux (`Kustomization`, `HelmRelease`), cert-manager (`Certificate`, `Issuer`, `ClusterIssuer`), and External Secrets (`ExternalSecret`, `SecretStore`). Includes RBAC subject cross-links and write-safe reconcile/sync/suspend/resume actions.
- **Structured Search Query Grammar:** Advanced query parsing engine supporting boolean operations (`AND`, `OR`, `NOT`), parentheses grouping, field filters (`ns:`, `name:`, `status:`, `app:`, `node:`, `phase:`, `is:crashloop`, `is:terminating`, `is:oomkilled`, `is:unhealthy`), and regex matching.
- **X.509 TLS Certificate Inspector:** Live inspection of TLS Secrets and cert-manager Certificates. Parses certificate chains, Subject Alternative Names (SANs), issuers, validities, key algorithms, and expiring certificate warnings (< 30 days, < 7 days, expired) surfaced in the Overview attention list.
- **Saved Port-Forwards & Live Telemetry:** Persistent saved port-forward configurations across app restarts, with live active TCP connection counters, cumulative byte transfer metrics, and audit logging.
- **Overview Right-Sizing & Resource Efficiency Mode:** Automated resource efficiency analysis categorizing workloads into Near Limit (≥ 80%), Over Request (> 100%), Unused Requests (< 10%, < 20%, < 50%), and Missing Requests/Limits. Quick-filter pills in the Pod table for instant drill-down.
- **Timed Write-Unlock & External Terminal Handoff:** Temporary 15-minute write-unlock for protected or read-only contexts with live countdown timer and automatic relock. Safe external terminal handoff detecting installed terminal emulators (`ghostty`, `kitty`, `alacritty`, `wezterm`, `konsole`, `gnome-terminal`, `iTerm.app`, `Terminal.app`, `wt.exe`) for Pod exec, Node host debug shells, and context shells with POSIX shell argument escaping.

## [0.2.0] - 2026-11-10

### Added
- **Overview Landing Page:** Default landing view summarizing connected clusters with live node readiness, pod totals, warning counts, and CPU/memory capacity usage. Includes bounded "Needs attention" workload list and "Recent warning events" across clusters with 1h/6h/24h filters and drawer navigation.
- **Light Theme & Design System:** Full light theme alongside dark theme with OS preference tracking (`prefers-color-scheme`) and Command Palette switcher. Semantic tokenized palette, compact 28px row density setting, and refreshed status bar with live connection and access indicators.
- **CodeMirror 6 YAML Editor:** Full syntax-highlighted editor with line numbers, code folding, and in-drawer search matching active dark/light theme.
- **Enhanced Pod Summary Drawer:** Conditions chips, Uses chips linking directly to related ConfigMaps/Secrets/PVCs, and container cards with live CPU/memory bars, termination reason banners, and direct action triggers.
- **Logs Viewer Upgrades:** Log level filtering (Error/Warn/Info/Debug), time-range windowing, bounded local file download, responsive single-source column hiding, and light-theme terminal contrast.
- **Collapsible Sidebar & Navigation Chords:** Grouped sidebar with Lucide icons, live failing resource counts and indicator dots per watched kind, and k9s-style go-to chords (`g p`, `g d`, `g s`, `g n`, `g e`, `g h`).
- **Resource Table Metrics:** In-table CPU and memory usage cells with requests/limits progress bars and sorting; Unschedulable reason badges and termination exit codes.

### Fixed
- Terminal pods (Succeeded/Failed) no longer incorrectly inflate requested node allocations.
- Fixed dark-on-dark unreadable log and exec text under light theme mode via dedicated `--fg-terminal` token.
- Overview subscription throttled to avoid startup latency spikes; cold-start time reduced to <700 ms.
- Cluster cards honestly display `—` and explicit problem banners when RBAC restricts listing specific kinds.

## [0.1.0] - Preview

### Fixed (found during macOS testing)
- Hidden tab panels no longer take up space on WebKit (only one pod row was visible).
- Sidebar and ⌘K resource items reuse the open tab for that kind instead of opening duplicates.
- Switching kinds no longer flashes an empty "Connecting…" notice.
- The Age column shows a relative age ("2d") with the exact timestamp on hover.
- Singular counts ("1 pod") and spacing in the log status line.

First public preview. Tested live on Linux, macOS 13 (Intel) and Windows 11 (x64). See
[docs/KNOWN-LIMITATIONS.md](docs/KNOWN-LIMITATIONS.md).

### Added
- Multi-cluster kubeconfig loading (`KUBECONFIG` lists, per-context colours, read-only and
  protected contexts, team settings file).
- Live, windowed resource tables for pods and the other built-in kinds, with sort, filter,
  merged multi-cluster views and honest "no permission" / reconnecting states.
- Resource drawer with details, events and YAML; edit with server-side dry-run, diff and apply.
- Actions: scale, restart, delete and more, each previewable as a `kubectl` command, with
  type-the-name confirmation on protected contexts and a local audit log.
- Pod and workload logs, interactive exec, and port-forwarding (started from the pod drawer).
- Helm release list, cluster connection diagnostics and a ⌘K command palette with k9s-style keys.
- Sandboxed plugins served over `fgplugin://` with a strict content security policy.
- Optional MCP server (`fleetglass mcp`, stdio, strictly read-only) for external AI tools; nothing runs unless you start it.
- Performance harness (`crates/bench`) with budgets checked in CI.

[Unreleased]: https://github.com/TuxoJlumoH/fleetglass/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/TuxoJlumoH/fleetglass/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/TuxoJlumoH/fleetglass/compare/v0.2.0...v0.4.0
[0.2.0]: https://github.com/TuxoJlumoH/fleetglass/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/TuxoJlumoH/fleetglass/releases/tag/v0.1.0
