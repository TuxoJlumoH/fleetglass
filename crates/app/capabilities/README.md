# Capabilities

Least privilege: every permission granted to a window is listed here with a reason.

| File | Window(s) | Permission | Why |
|---|---|---|---|
| `default.json` | `main`, `logs-*` | *(none)* | The UI only invokes app-defined commands (`app_info`, `list_contexts`, …). Detached logs windows (`logs-<n>`, opened by `open_detached_logs`) share this capability and need no permissions either; their log `Channel` is exempt from the ACL. It uses no core plugin APIs (events, window, menu, tray, image, path, resources), so `core:default` is not granted. Saving logs (`save_log_file`) is an app command, not the `fs` plugin: it only creates new, sanitised `*.log` files in the Downloads (or home) directory, never overwrites, and rejects text over 64 MiB. Sharing (`share_text`, Android only; desktop returns "not supported") is an app command too: the Kotlin plugin writes the text (max 8 MiB) to `cacheDir/shared/` (files older than 1 h are pruned) and opens the system share sheet through the app's FileProvider with a one-time read grant. `set_app_foreground` (mobile lifecycle) only pauses/resumes background work. |

Why an empty list works (Tauri 2.12, `tauri/src/webview/mod.rs`, `on_message`): the ACL
check runs only for `plugin:*` commands, for remote origins, or when the app ships an app
ACL manifest (`tauri_build::Attributes::app_manifest`). We have none, so app commands
from the bundled frontend (local origin) are allowed for every window covered by a
capability. The capability file is still needed: it binds the `main` window, and remote
URLs stay blocked because no capability lists `remote`.

When to revisit:
- If we add an app ACL manifest (`build.rs`), every command needs an `allow-<command>`
  permission listed here.
- If the UI starts using `@tauri-apps/api` event/window/... APIs (e.g. `listen()` needs
  `core:event:allow-listen`), add the narrowest `core:<module>:allow-<cmd>` permission,
  not `core:default`. `Channel<T>` for streamed batches needs no permission (its fetch
  command is exempt from the ACL).

Not enabled, by policy (see security design in docs/PLAN.md): `shell`, `fs`, `http`, `process`
plugins. The updater plugin is not included yet: `check_for_update` only honours the opt-in
setting and reports no update (see docs/KNOWN-LIMITATIONS.md).

The webview CSP lives in `../tauri.conf.json` (`app.security.csp`): only `'self'` for
scripts/styles/images, `connect-src ipc: http://ipc.localhost`, no objects, frames or
form targets. `devCsp` additionally allows the Vite dev server's inline/HMR websocket on
`localhost:1420`. Frames are limited to plugins: `fgplugin:` on Linux/macOS; on Windows
`../tauri.windows.conf.json` (merged over the base config) uses
`http(s)://fgplugin.localhost`, which is where WebView2 serves custom schemes. It is not
allowed on other platforms, where `*.localhost` would reach a local loopback server.
