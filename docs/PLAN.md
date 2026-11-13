# Fleetglass: Execution Plan

> Name: Fleetglass. Status: planning. Date: 2026-10-31.

## 1. Positioning

**A fast, keyboard-first, multi-cluster Kubernetes desktop app. Open source, no login, no AI by default, no node limits.**

What we are NOT building:
- An in-cluster web dashboard (Headlamp owns this)
- A topology/AI troubleshooting platform (Radar, PodSteer, Kunobi own this)
- A terminal UI (k9s owns this)

Target users:
1. k9s users who want visuals and multi-cluster views without losing keyboard speed
2. Ex-Lens/OpenLens/FreeLens users tired of Electron, logins and licence changes
3. Users of commercial desktop clients who want open source or have no node limits

## 2. Complaints → what we do differently

| # | What people complained about (source) | Our answer |
|---|---|---|
| 1 | Lens forced login, licence flip, closed source (923 reactions, lens#5444; "never let unauditable code access my clusters", 89 upvotes) | Apache-2.0, **no CLA** (DCO only), no account, **no telemetry**. Commit to this in `GOVERNANCE.md`. Signed releases + SBOM. |
| 2 | Slow/freezing on big clusters (Headlamp #4816; Lens "5GB RAM"; other desktop clients' issue trackers) | **Backend-owned data.** Rust holds the cache; UI only receives the visible rows. Performance budgets are enforced in CI against 50k simulated pods (kwok). |
| 3 | Memory depends on what you cache, not on Electron vs Tauri (independent test, Oct 2026) | Watch only what is being viewed; stop idle watches after N minutes; store compact rows, not full objects; memory budget in CI. |
| 4 | Auth breaks: EKS SSO, exec plugins, 1Password, custom CAs (Headlamp #358/#1716, k9s #3730) | Load the user's **login shell environment** for exec plugins; show exec plugin output and prompts; a **connection diagnostics panel** shows exactly what failed and why. |
| 5 | Headlamp health-checks every cluster at startup | **Connect lazily.** Only connect to clusters the user opens or pins. Status lights 🟢🟡🔴 per cluster, updated when it's used. |
| 6 | Prod mistakes ("thought I was in dev") | Context **colours + coloured window frame**, per-context **read-only mode**, **type-the-name** confirmation on protected clusters, preview of every action as a `kubectl` command. |
| 7 | k9s logs incomplete or slow; Lens: no log wrap, no deployment logs (k9s #1228, Lens #1077, FreeLens #687) | Logs handled in Rust: ring buffer with disk overflow, regex search runs in the backend, virtualised view, wrap, JSON pretty-print, logs for a whole workload that also follow new pods, previous-container logs, export. |
| 8 | No metrics without Prometheus (FreeLens #466/#627, Lens #298) | metrics-server works out of the box; Prometheus optional. |
| 9 | Edit secrets without base64 (k9s #1017, open since 2021) | Secret editor decodes values for editing and encodes them on save. |
| 10 | Copy files to/from pods (Lens #1369, 98 reactions) | GUI `kubectl cp`: drag-and-drop file browser for containers. |
| 11 | k9s: one view at a time; tabs request rejected (k9s #1430) | Tabs, **split panes**, and detachable windows (e.g. logs). |
| 12 | Headlamp navigation feels off; Lens "too flat, no icons" redesign | k9s-compatible keys + ⌘K palette, dense but readable tables, sensible default columns. No surprise redesigns: UI changes go through RFCs. |
| 13 | Custom/resizable columns (k9s #755, 214 reactions; Lens #601, 165) | Resizable, reorderable, custom columns using JSONPath; saved per resource type. |
| 14 | AI turned on by default (Lens Prism backlash), "not littered with AI" | **No AI in the core.** Later: optional read-only MCP server, off by default. |
| 15 | One screen for an app's Service, Ingress, Endpoints, ConfigMap and Secret (Reddit) | **App view** grouping by `app.kubernetes.io/instance` + owner references. |
| 16 | Wrong status ("CrashLoop shown as Running"); hidden RBAC gaps | Compute status the way kubectl does. When RBAC blocks something, say "No permission to list X" instead of showing an empty list. |
| 17 | Solo projects die (Seabird, OpenLens) | Governance + several maintainers from the start, good first issues, contributor docs, and a public roadmap. |
| 18 | Desktop clients treat Linux as second-class: terminal/zsh/AppImage breakage, no ARM64 Linux | Linux is a first-class platform: deb, rpm, AppImage, Flatpak, AUR, Homebrew, winget; x64 and ARM64. |

## 3. Architecture

### Key decision: a "headless core"
```
fleetglass/
├── crates/
│   ├── core/        # Kubernetes engine, no Tauri dependency
│   │                #  cluster manager, watch cache, row projection,
│   │                #  query engine, logs, exec, port-forward, auth diagnostics
│   ├── app/         # Tauri 2 shell: IPC commands, windows, tray, updater
│   └── bench/       # Performance harness against kind + kwok
├── ui/              # React 19 + Vite + TypeScript
└── docs/
```
Keeping `core` independent means it can be tested without a GUI and later reused for a CLI, TUI or MCP server.

### Data flow (the main thing we do better)
Most clients stream every object to the frontend and filter in JavaScript. That is why they freeze at scale.

```
API server ──watch──▶ core: per-cluster store (kube-rs reflector)
                        │  project to compact Row {uid, cells[], status}
                        ▼
                     Query engine (filter/sort/search in Rust)
                        │  UI asks: "rows 200–260 of pods, sorted by CPU, filter=api"
                        ▼
                     IPC: visible window + batched deltas (max ~10/s)
                        ▼
                     UI: virtualised table (TanStack Virtual)
```
- Full objects are fetched only when a detail/YAML view opens.
- For large lists, use metadata-only watches where possible (`PartialObjectMeta`).
- Merged multi-cluster lists are just one query over several stores, each row carrying a `cluster` column.
- Watches are reference-counted: when no view uses a kind for 5 minutes, the watch stops.

### Stack
| Layer | Choice |
|---|---|
| Shell | Tauri 2 (2.12.x) |
| K8s client | `kube` 4.x + `k8s-openapi` 0.28 (`kube::runtime` reflector, watcher, ws) |
| Async | tokio |
| UI | React 19, Vite, TypeScript |
| Tables | TanStack Table + TanStack Virtual |
| State | Zustand |
| Editor | Monaco (YAML, with schema validation from cluster OpenAPI) |
| Terminal | xterm.js |
| Palette | cmdk |
| Settings storage | SQLite or TOML in the app config dir; secrets in the OS keychain |
| Tests | Rust unit/integration tests against kind; perf tests with kwok; UI tests with Vitest + Playwright |

### Performance budgets (checked in CI)
| Scenario | Budget |
|---|---|
| Cold start to first cluster list | < 1 s |
| Open pod list, 50k pods across 3 clusters | first rows < 1.5 s (first full page, may be a labelled partial list while the initial LIST streams in; user decision 2026-11-02) |
| Scrolling / sorting / filtering 50k rows | < 100 ms per interaction |
| Idle memory, 1 cluster | < 200 MB |
| Memory, 3 clusters with 50k pods total | < 500 MB |
| Logs: 1M lines buffered, search | < 300 ms |

## 4. Roadmap

Time estimates assume 1–2 developers working part-time. Halve them for full-time.

### Phase 0: Foundations (2 weeks)
- Repo, Apache-2.0 licence, DCO, `GOVERNANCE.md`, `CONTRIBUTING.md`, code of conduct
- Cargo workspace (`core`, `app`, `bench`) + Vite UI
- CI: build on Linux/macOS/Windows, clippy, rustfmt, tests, kind cluster for integration tests
- kwok setup to simulate 10k/50k pods; first benchmark running
- Load kubeconfig (multiple files, `KUBECONFIG` merge), login shell environment capture

**Exit:** app opens and lists contexts; CI green on 3 OSes.

### Phase 1: MVP, "daily driver for one person" (8–10 weeks)
1. Cluster manager: lazy connect, status lights, diagnostics panel, colours, tags, read-only flag
2. Watch cache + row projection + query engine (the architecture above)
3. Resource lists: built-in kinds + any CRD, multi-namespace filter, label filter, virtualised table
4. Detail view: readable summary, YAML (Monaco), events, related resources
5. Actions: edit, delete, scale, restart, cordon/drain; type-the-name confirmation; "copy as kubectl"
6. Logs v1: single pod/container, follow, search, wrap, previous container, export
7. Exec shell (xterm.js) and port forwarding (manage all forwards in one place)
8. Keyboard: k9s keys, ⌘K palette, back/forward
9. Tabs
10. Metrics via metrics-server (CPU/memory vs requests/limits)

**Exit:** passes perf budgets; 10 beta testers use it daily for a week. **Public release v0.1.**

### Phase 2: Differentiation (8–10 weeks)
- **Merged multi-cluster lists** (one table across clusters) + cluster groups
- **Cross-cluster / cross-namespace diff**
- Logs v2: logs for a whole workload or label selector, following new pods; JSON view; open in a detached window
- Split panes
- App view (resources grouped by app)
- Secret editor with decoded values; file copy to/from containers
- Helm: releases, values, history, upgrade, rollback
- Custom/resizable/reorderable columns
- Prometheus metrics (optional)
- Packaging: Flatpak, AUR, Homebrew, winget; ARM64 Linux; auto-update (signed)

**Exit:** v0.5; launch post on r/kubernetes + Show HN.

### Phase 0.3: Android app (after v0.2)
- Standalone Tauri 2 Android app on the same Rust core; talks to API servers directly. No remote-control server, no relay, no account.
- View + safe actions (restart, scale, delete) with the same preview, read-only, protected and audit rules as desktop.
- Limit: exec credential plugins (EKS/GKE/AKS login tools) can't run on Android; tokens, client certs and OIDC refresh tokens work.
- Tasks: `03-01`..`03-06`, `03-GATE` in TASKS.md.

### Phase 0.4: Power-user & multi-cluster operations (active 2026-11-11)
- **Simultaneous multi-context exploration:** (04-01) Select any subset of contexts (`Ctrl/⌘+click` or saved cluster groups) across all resource tables, Overview, Events, and Logs, with a pinned colour-coded `Context` column and independent per-cluster access banners.
- **Cloud auth & expired credential recovery:** (04-02) Detect expired `aws sso` / `gcloud` / `az` / `kubelogin` / OIDC sessions, parse device-code prompts (URL + code), and offer one-click terminal sign-in with automatic client invalidation and reconnect.
- **Ephemeral debug containers, container attach, and node debug shell:** (04-03) `pods/ephemeralcontainers` (`busybox`, `netshoot`, `alpine`, custom image, target process namespace), container `attach`, and `kubectl debug node/<name>` host shell — all gated by `enforce_write_safety`, `kubectl` preview, and audit logging.
- Tasks: `04-01`..`04-10`, `04-GATE` in TASKS.md.

### Phase 3: Ecosystem (ongoing)
- Argo CD / Flux / Gateway API / Karpenter views
- Plugin API (sandboxed, declared permissions, e.g. WASM or iframe-isolated JS)
- Optional **read-only MCP server**, off by default, with a local audit log
- Shared team settings via a file in Git (contexts, colours, protection rules); no server needed
- Local action audit log ("who changed what from this machine")

## 5. Launch & community
- Build in public: weekly changelog, public roadmap (GitHub Projects), Discord or Matrix
- Pitch: a fast, open-source, multi-cluster desktop client, not "another K8s UI" (people are tired of new K8s UIs)
- Avoid astroturfing (Radar was called out for it on HN). Let real users post.
- Respond to issues fast. A responsive developer is one of the most praised things in this space.
- Publish benchmarks with a reproducible script (kwok), not marketing numbers.

## 6. Success metrics
| By | Target |
|---|---|
| v0.1 (≈3 months) | 10 daily beta users, perf budgets met |
| v0.5 (≈6 months) | 1k GitHub stars, 3+ outside contributors, packaged in 5+ channels |
| 12 months | 5k stars, 2+ maintainers with merge rights, plugin API v1 |

## 7. Risks
| Risk | Mitigation |
|---|---|
| Users are tired of new K8s UIs | Clear, specific pitch + published benchmarks |
| Kubeli (MIT, Tauri, 397★) is close to this idea | Kubeli connects to one cluster at a time and streams objects to the UI. Our architecture (backend query engine, merged multi-cluster) is different enough to justify a fresh codebase. Stay friendly; borrow ideas, credit them. |
| Webview differences (WebKitGTK on Linux is slower) | Keep the UI light; virtualise everything; test on Linux early |
| Burnout / bus factor | Governance and co-maintainers from Phase 1; narrow scope |
| Scope creep (topology, AI, cost…) | Anything outside section 1 waits for the plugin API |

## 8. Immediate next steps
1. Pick a name and check domain + GitHub org availability
2. Install prerequisites: Tauri system libs, pnpm, kind, kwok, kubectl
3. Scaffold the Phase 0 repo
4. Spike (1 week): reflector + row projection + windowed query over 50k kwok pods, to prove the architecture before building the UI on top of it
