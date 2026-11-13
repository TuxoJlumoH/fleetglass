// `pnpm -C ui tauri <args>`: runs the Tauri CLI against the shell in crates/app.
// The CLI only searches the current directory for tauri.conf.json, so point it
// at crates/app (app) and ui/ (frontend) explicitly. Works on all platforms.
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const uiDir = path.dirname(fileURLToPath(import.meta.url));
const cli = createRequire(import.meta.url).resolve("@tauri-apps/cli/tauri.js");

const result = spawnSync(process.execPath, [cli, ...process.argv.slice(2)], {
  stdio: "inherit",
  env: {
    ...process.env,
    TAURI_APP_PATH: path.resolve(uiDir, "../crates/app"),
    TAURI_FRONTEND_PATH: uiDir,
  },
});
process.exit(result.status ?? 1);
