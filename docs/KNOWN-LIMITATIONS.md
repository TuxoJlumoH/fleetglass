# Known limitations (0.4)

These are known gaps in Fleetglass 0.4; none of them sends data anywhere. Please report any others as GitHub issues.

## Platforms
- Linux is tested live against real and synthetic clusters (requires WebKitGTK ≥ 2.40 for CSS color-mix token support).
- Tested live on Windows 11 (x64) and macOS 13 (Intel). Plugins on Windows are unverified, and Apple Silicon and Windows on ARM are untested.
- Android client is available as a standalone build (requires Android 10+ / API 29+ with encrypted Keystore credentials).
- Release builds are not code-signed or notarised yet: macOS Gatekeeper and Windows SmartScreen will warn on first launch.

## Updates
- There is no in-app updater yet. The "check for updates" setting is stored but the check always reports no update and makes no network call. Download new versions from the GitHub releases page.

## Editor and terminal
- YAML editing uses CodeMirror 6 with line numbers, code folding, and syntax highlighting (schema validation and autocomplete are not implemented yet).
- The in-app exec terminal is a simple line-based view. For full-screen interactive programs (`vim`, `htop`, `tmux`), use the **External Terminal** handoff button, which detects installed terminal emulators (`ghostty`, `kitty`, `alacritty`, `wezterm`, `konsole`, `gnome-terminal`, `iTerm.app`, `Terminal.app`, `wt.exe`) and launches a native interactive TTY with audit logging.
- Ephemeral debug containers require Kubernetes 1.23+ with the `EphemeralContainers` feature gate enabled.

## Write safety & timed unlock
- By default, contexts flagged as `read_only` reject all mutating actions. A temporary 15-minute write-unlock can be granted via the status bar or command palette (requiring typed context name confirmation if also `protected`). The unlock expires in-memory after 15 minutes, re-engaging read-only protection and logging an auto-relock audit entry.

## Plugins
- On Windows, all plugins share one web origin (`http(s)://fgplugin.localhost/<id>/`) because WebView2 cannot give each plugin its own host; on Linux and macOS each plugin has its own origin. Each plugin's content security policy only allows scripts, styles and images from its own folder.
- On Windows the app cannot see navigations inside a plugin frame, so a plugin frame could navigate to another plugin's page. Fleetglass then stops answering that frame and shows "Reload plugin", but requests sent before the new page finished loading may still be answered with the original plugin's read-only permissions.
- Plugin permissions are declared by the plugin itself; Fleetglass does not ask you to approve them.
- The content security policy does not cover WebRTC or DNS prefetch.
- Plugin scripts must be external files; inline scripts are blocked.
- The `read:logs` permission is accepted but not implemented yet.

## Port-forward
- Port-forwards are bound to 127.0.0.1 only.
- Saved port-forwards are stored in the local settings store; port-forward lifecycles are tracked with live TCP connection counters and audited.

## Logs and exec
- The built-in web exec terminal does not resize the remote terminal when the window size changes (use the External Terminal handoff for automatic PTY window resize).
- Log text kept in memory for a stream is not capped yet, so very long-running log streams use more memory over time.

## Tables and navigation
- If the row you selected leaves the loaded window of rows, the selection moves to the row now at the same position.
- While a cluster reconnects, the table keeps the last known rows and shows "Reconnecting to <cluster>… (showing last known rows)".
- If a cluster reports "No permission" or an error after it had loaded, the last known rows stay visible without an "outdated" label.

## Overview & Large clusters
- The Overview page displays up to 64 clusters, caps the "Needs attention" list at 50 items, caps "Recent warning events" at 50 events, and bounds right-sizing efficiency analyses to top 100 items.
- Fleetglass watches each resource type cluster-wide. If you may only list it in some namespaces, there is no automatic fallback to per-namespace watches yet, and you will see "No permission to list …" (Overview cards display `—` with problem notices).
- While the first list of a large cluster streams in, rows appear in batches under a "Loading… N rows so far" banner.
- Memory use grows with the number of objects watched. Measured on the backend process alone (not counting the web view): about 140 MB for one cluster with 16,700 pods and about 320 MB for three clusters with 50,000 pods in total.
