#!/usr/bin/env bash
# Full-app budget evidence for Fleetglass (V-06): cold-start time and whole-process-tree memory
# of the *built* desktop app (Rust process + WebKitGTK WebProcess/NetworkProcess children),
# under Xvfb with Mesa software rendering. Linux only.
#
#   scripts/measure-app.sh --kubeconfig PATH [--bin PATH] [--runs N] [--idle-secs S]
#                          [--out FILE.json] [--select current|all-clusters]
#                          [--startup-metric overview|rows] [--startup-timeout S] [--display N]
#
#   --kubeconfig PATH   REQUIRED. Never defaults to ~/.kube/config; refused if it resolves to
#                       ~/.kube/config or anything under ~/.kube, or is a ':' list.
#   --bin PATH          app binary (default target/release/fleetglass, built by
#                       `pnpm -C ui tauri build --no-bundle`; it embeds ui/dist)
#   --runs N            app launches for the startup number (default 5)
#   --idle-secs S       idle time before the memory snapshot, taken in run 1 only (default 60)
#   --out FILE          JSON result (default .dev/measure/measure-<timestamp>.json)
#   --select MODE       what the UI queries during the memory snapshot:
#                         current       (default) the kubeconfig's current-context only
#                         all-clusters  click the sidebar "All Clusters" button after startup
#                                       (OCR + XTest), so every context in the kubeconfig is watched
#   --startup-metric M  which app log line is the startup number (default overview):
#                         overview  `first overview returned` = the v0.2 cold-start definition
#                                   (first Overview data with >= 1 connected cluster)
#                         rows      `first rows returned` for Pods. Since v0.2 the app opens on
#                                   Overview, so the script opens Pods itself (keys `g` `p` via
#                                   XTest; needs python3 + libXtst) once the page has loaded; the
#                                   number then INCLUDES that wait and the tab switch. Use it only
#                                   to compare with pre-v0.2 (v0.1) first-rows results.
#   --startup-timeout S max wait for the startup line per run (default 60)
#   --display N         first X display number to try (default 93)
#
# What the app shows by default (v0.2, 2026-11-10): the initial tab is Overview (task 02-08b).
# On start the UI calls list_contexts and selects the kubeconfig's *current-context* only
# (ui/src/stores/contexts.ts loadContexts -> selectedContext = current; clustersForQuery). Since
# 03-03b a saved last selection (WebView localStorage `fleetglass.lastSelection`) wins instead;
# every run here starts with fresh XDG dirs, so there is none and current-context applies. The
# backend connects lazily, so other contexts in the kubeconfig are NOT connected until the user
# picks them or a cluster group ("All Clusters"). Therefore:
#   - the default startup number is "first Overview data with the current context connected";
#   - for the "3 clusters, 50k pods" memory budget use --select all-clusters with a kubeconfig
#     whose contexts are exactly the clusters under test (e.g. .dev/kwok/kubeconfig).
# The contexts found and the current context are recorded in the JSON.
#
# Startup lines (stderr, printed once each; no subscriber/env filter needed, nothing persisted,
# no network), all measured from the first statement of main():
#   `fleetglass startup: first overview returned after N ms (clusters=C, connected=K,
#    attention=A, via=subscribe_overview)` - first Overview push with >= 1 connected cluster
#    (--startup-metric overview; the default and the cold-start budget number);
#   `fleetglass startup: first rows returned after N ms (rows=R, total=T, via=...)` - first
#    query_page/subscribe_query response with >= 1 row; since v0.2 only after a table is opened
#    (--startup-metric rows);
#   `fleetglass startup: main page loaded after N ms` - main webview finished loading.
# They exclude exec + dynamic linking before main; `wallMs` (script-side, launch -> line seen in
# the log, ~50 ms polling granularity) is recorded too as an upper bound.
# JSON: `startup_metric` names the metric; each run has `startupMs` (generic, as before) plus the
# metric-named copy (`firstOverviewMs` or `firstRowsMs`) and its counters (clusters/connected/
# attention or rows/total); `startupMedianMs` etc. keep their names.
# Cold start caveat: every run gets fresh XDG_{CONFIG,DATA,CACHE}_HOME under .dev/xdg-measure/
# (no saved app state, no WebKit cache), but the OS page cache is warm after the first launch
# (binary, WebKitGTK and Mesa libraries), so this is an app-cold, not a machine-cold, start.
#
# Memory metric: the app PID plus all descendants, from /proc/<pid>/smaps_rollup: per-process
# RSS and PSS, totals sum(RSS) and sum(PSS). sum(PSS) is the fair total (RSS double-counts shared
# libraries). Parsing and JSON: scripts/lib/proc_mem.py (`--self-test` covers the parsers).
#
# Required tools: Xvfb, xdpyinfo, setsid (util-linux), python3; kubectl (only to list the
# kubeconfig's contexts; local, no API calls); for --select all-clusters also ImageMagick 7
# `import`, tesseract and python3 with libX11.so.6 + libXtst.so.6 (scripts/lib/xinput.py);
# for --startup-metric rows python3 with libX11.so.6 + libXtst.so.6.
# The clusters must already be up (kind: scripts/dev-cluster.sh up; kwok:
# scripts/kwok-local.sh up ... --hold). This script never creates, changes or deletes cluster
# objects and never reads ~/.kube/config.
# Exit codes: 0 ok, 1 failure, 2 usage, 130 SIGINT, 143 SIGTERM.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROC_MEM="$ROOT/scripts/lib/proc_mem.py"
XINPUT="$ROOT/scripts/lib/xinput.py"

if [[ -t 1 ]]; then C_OK=$'\e[32m' C_WARN=$'\e[33m' C_ERR=$'\e[31m' C_DIM=$'\e[2m' C_OFF=$'\e[0m'
else C_OK='' C_WARN='' C_ERR='' C_DIM='' C_OFF=''; fi
log()  { printf '%s==>%s %s\n' "$C_OK" "$C_OFF" "$*"; }
info() { printf '    %s%s%s\n' "$C_DIM" "$*" "$C_OFF"; }
warn() { printf '%s[warn]%s %s\n' "$C_WARN" "$C_OFF" "$*" >&2; }
die()  { printf '%s[error]%s %s\n' "$C_ERR" "$C_OFF" "$*" >&2; exit 1; }
need() {
  local c
  for c in "$@"; do command -v "$c" >/dev/null 2>&1 || die "'$c' not found in PATH"; done
}
usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//'; }

# --- args ---------------------------------------------------------------------------------------
BIN="$ROOT/target/release/fleetglass"
KCFG=""
RUNS=5
IDLE_SECS=60
OUT_JSON=""
SELECT="current"
METRIC="overview"
STARTUP_TIMEOUT=60
DISP="93"
while (($#)); do
  case "$1" in
    --bin) BIN="${2:?--bin needs a value}"; shift 2 ;;
    --kubeconfig) KCFG="${2:?--kubeconfig needs a value}"; shift 2 ;;
    --runs) RUNS="${2:?--runs needs a value}"; shift 2 ;;
    --idle-secs) IDLE_SECS="${2:?--idle-secs needs a value}"; shift 2 ;;
    --out) OUT_JSON="${2:?--out needs a value}"; shift 2 ;;
    --select) SELECT="${2:?--select needs a value}"; shift 2 ;;
    --startup-metric) METRIC="${2:?--startup-metric needs a value}"; shift 2 ;;
    --startup-timeout) STARTUP_TIMEOUT="${2:?--startup-timeout needs a value}"; shift 2 ;;
    --display) DISP="${2:?--display needs a value}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$KCFG" ]] || { usage >&2; echo "--kubeconfig is required" >&2; exit 2; }
for n in "$RUNS" "$IDLE_SECS" "$STARTUP_TIMEOUT" "$DISP"; do
  [[ "$n" =~ ^[0-9]+$ ]] || { echo "not a number: $n" >&2; exit 2; }
done
((RUNS >= 1)) || { echo "--runs must be >= 1" >&2; exit 2; }
[[ "$SELECT" == current || "$SELECT" == all-clusters ]] || { echo "--select: current|all-clusters" >&2; exit 2; }
[[ "$METRIC" == overview || "$METRIC" == rows ]] || { echo "--startup-metric: overview|rows" >&2; exit 2; }

need Xvfb xdpyinfo setsid python3
[[ "$SELECT" == all-clusters ]] && need import tesseract
if [[ "$SELECT" == all-clusters || "$METRIC" == rows ]]; then
  python3 -c 'import ctypes; ctypes.CDLL("libX11.so.6"); ctypes.CDLL("libXtst.so.6")' 2>/dev/null \
    || die "python3 cannot load libX11.so.6/libXtst.so.6 (needed for --select all-clusters and --startup-metric rows)"
fi
[[ -x "$BIN" ]] || die "app binary '$BIN' missing; build it with: pnpm -C ui tauri build --no-bundle"
BIN="$(realpath -- "$BIN")"

# --- kubeconfig safety --------------------------------------------------------------------------
[[ "$KCFG" != *:* ]] || die "refusing: --kubeconfig must be one file, not a ':' list"
KCFG="$(realpath -m -- "$KCFG")"
KUBE_DIR="$(realpath -m -- "$HOME/.kube")"
[[ "$KCFG" != "$KUBE_DIR" && "$KCFG" != "$KUBE_DIR"/* ]] \
  || die "refusing: --kubeconfig resolves under ~/.kube ($KCFG)"
[[ -f "$KCFG" ]] || die "kubeconfig '$KCFG' not found"
if [[ -e "$HOME/.kube/config" && "$KCFG" -ef "$HOME/.kube/config" ]]; then
  die "refusing: --kubeconfig is the same file as ~/.kube/config"
fi
export KUBECONFIG="$KCFG"

CONTEXTS=""
CURRENT_CONTEXT=""
if command -v kubectl >/dev/null 2>&1; then
  # Local kubeconfig reads only (no API server calls).
  CONTEXTS="$(kubectl --kubeconfig "$KCFG" config get-contexts -o name 2>/dev/null | paste -sd, - || true)"
  CURRENT_CONTEXT="$(kubectl --kubeconfig "$KCFG" config current-context 2>/dev/null || true)"
else
  warn "kubectl not found; contexts are not recorded in the JSON"
fi
[[ -n "$CURRENT_CONTEXT" || "$SELECT" == all-clusters ]] \
  || warn "kubeconfig has no current-context: the UI selects nothing and no startup line will appear"

# --- output locations ---------------------------------------------------------------------------
STAMP="$(date +%Y%m%d-%H%M%S)"
WORK="$ROOT/.dev/measure/$STAMP"
XDG_BASE="$ROOT/.dev/xdg-measure"
mkdir -p "$WORK"
[[ -n "$OUT_JSON" ]] || OUT_JSON="$ROOT/.dev/measure/measure-$STAMP.json"
mkdir -p "$(dirname -- "$OUT_JSON")"
RUNS_TSV="$WORK/runs.tsv"
MEM_JSON="$WORK/memory.json"
: >"$RUNS_TSV"

# Isolated runtime dir (WebKitGTK needs one) and headless software rendering.
export XDG_RUNTIME_DIR="$ROOT/.dev/xdg-runtime"
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
export WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1
if [[ -f /usr/share/glvnd/egl_vendor.d/50_mesa.json ]]; then
  export __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json
fi

# --- cleanup ------------------------------------------------------------------------------------
XVFB_PID=""
APP_PID=""
CLEANED=0
EXIT_CODE=""
# Stops a process group we started with setsid (pgid == pid): TERM, then KILL after 5 s.
stop_group() {
  local pid="$1" i
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || return 0
  for i in $(seq 1 50); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  kill -KILL -- "-$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
cleanup() {
  local code="${EXIT_CODE:-$?}"
  ((CLEANED)) && return
  CLEANED=1
  trap '' INT TERM
  set +e
  [[ -n "$APP_PID" ]] && stop_group "$APP_PID"
  [[ -n "$XVFB_PID" ]] && stop_group "$XVFB_PID"
  ((code == 0)) || warn "measure-app failed (exit $code); logs in $WORK"
  exit "$code"
}
on_signal() { EXIT_CODE="$1"; warn "interrupted"; exit "$1"; }
trap cleanup EXIT
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

now_ms() { date +%s%3N; }

# --- Xvfb ---------------------------------------------------------------------------------------
while [[ -e "/tmp/.X${DISP}-lock" || -e "/tmp/.X11-unix/X${DISP}" ]]; do DISP=$((DISP + 1)); done
export DISPLAY=":$DISP"
log "Starting Xvfb on $DISPLAY"
setsid Xvfb "$DISPLAY" -screen 0 1600x1000x24 -nolisten tcp >"$WORK/xvfb.log" 2>&1 &
XVFB_PID=$!
for i in $(seq 1 100); do
  xdpyinfo >/dev/null 2>&1 && break
  kill -0 "$XVFB_PID" 2>/dev/null || die "Xvfb exited (see $WORK/xvfb.log)"
  ((i == 100)) && die "Xvfb did not become ready on $DISPLAY within 10 s"
  sleep 0.1
done

# --- helpers ------------------------------------------------------------------------------------
# launch_app RUN: fresh XDG dirs, start the app in its own process group, set APP_PID/LAUNCH_MS.
launch_app() {
  local run="$1" xdg="$XDG_BASE/run-$1"
  [[ "$xdg" == "$ROOT/.dev/xdg-measure/run-"* ]] || die "internal: bad XDG path $xdg"
  rm -rf -- "$xdg"
  mkdir -p "$xdg/config" "$xdg/data" "$xdg/cache"
  APP_LOG="$WORK/app-$run.log"
  LAUNCH_MS="$(now_ms)"
  XDG_CONFIG_HOME="$xdg/config" XDG_DATA_HOME="$xdg/data" XDG_CACHE_HOME="$xdg/cache" \
    setsid "$BIN" >"$APP_LOG" 2>&1 &
  APP_PID=$!
}

# send_go_pods: focus the app window and send the `g` `p` chord (open / switch to Pods). Xvfb runs
# without a window manager, so keyboard focus follows the pointer: park it in the main pane
# (window 1280x800 at the origin; x=700 is right of the sidebar, y=420 below the tab strip).
send_go_pods() {
  python3 "$XINPUT" move 700 420
  python3 "$XINPUT" key g p
}

# wait_startup: poll the log for the startup line of $METRIC; sets STARTUP (overview:
# "ms clusters connected attention"; rows: "ms rows total"), WALL_MS and, for rows, KEYS_MS
# (script clock, launch -> `g p` first sent).
wait_startup() {
  local deadline=$((SECONDS + STARTUP_TIMEOUT)) sub=startup-overview next_keys=0 now
  [[ "$METRIC" == rows ]] && sub=startup
  STARTUP=""
  KEYS_MS="-"
  while ((SECONDS < deadline)); do
    if STARTUP="$(python3 "$PROC_MEM" "$sub" "$APP_LOG")"; then
      WALL_MS=$(($(now_ms) - LAUNCH_MS))
      return 0
    fi
    kill -0 "$APP_PID" 2>/dev/null || die "the app exited before the startup line (see $APP_LOG)"
    # rows: once the page has loaded, open Pods; resend every 3 s in case the first chord arrived
    # before React registered its key handler (`g p` on an open Pods tab only re-selects it).
    if [[ "$METRIC" == rows ]] && python3 "$PROC_MEM" page-loaded "$APP_LOG" >/dev/null; then
      now="$(now_ms)"
      if ((now >= next_keys)); then
        if ((next_keys == 0)); then
          sleep 0.3 # let React mount and register its key handlers after the load event
          now="$(now_ms)"
        else
          info "no rows yet; sending \`g p\` again"
        fi
        send_go_pods
        [[ "$KEYS_MS" == - ]] && KEYS_MS=$((now - LAUNCH_MS))
        next_keys=$((now + 3000))
      fi
    fi
    sleep 0.05
  done
  if [[ "$METRIC" == rows ]]; then
    die "no 'fleetglass startup: first rows' line within ${STARTUP_TIMEOUT}s after opening Pods with \`g p\` (keys sent: ${KEYS_MS} ms after launch; see $APP_LOG; was the page loaded, is the current context reachable and does it have pods?)"
  fi
  die "no 'fleetglass startup: first overview' line within ${STARTUP_TIMEOUT}s (see $APP_LOG; it is printed only when the Overview first gets data with >= 1 connected cluster: is the current context reachable, and does this build print the line? For older builds use --startup-metric rows)"
}

# click_all_clusters: OCR the screen for the sidebar "All Clusters" button and click it.
click_all_clusters() {
  local png="$WORK/select.png" xy="" i
  for i in $(seq 1 20); do
    import -window root "$png"
    xy="$(tesseract "$png" - tsv 2>/dev/null | awk -F'\t' '
      NF >= 12 && $1 == 5 { if (prev_ok && $12 ~ /^Clusters/) { print int(pl + (($7 + $9) - pl) / 2), int($8 + $10 / 2); exit }
                            prev_ok = ($12 == "All"); pl = $7 }')"
    [[ -n "$xy" ]] && break
    sleep 1
  done
  [[ -n "$xy" ]] || die "could not find the 'All Clusters' button on screen (see $png)"
  info "clicking 'All Clusters' at $xy"
  # shellcheck disable=SC2086 # "x y" must split into two args
  python3 "$XINPUT" click $xy
  sleep 2
  import -window root "$WORK/after-select.png"
  tesseract "$WORK/after-select.png" - 2>/dev/null >"$WORK/after-select.txt" || true
  info "pod counters on screen after the click: $(grep -Eo '[0-9][0-9,]* pods' "$WORK/after-select.txt" | paste -sd' ' - || true)"
}

# --- runs ---------------------------------------------------------------------------------------
log "Binary:     $BIN"
log "Kubeconfig: $KCFG (contexts: ${CONTEXTS:-?}; current: ${CURRENT_CONTEXT:-none}; select: $SELECT)"
if [[ "$METRIC" == overview ]]; then
  log "Startup metric: overview (process start -> first Overview data with a connected cluster)"
else
  log "Startup metric: rows (process start -> first Pods rows; includes waiting for the page and opening Pods with \`g p\`)"
fi
for run in $(seq 1 "$RUNS"); do
  launch_app "$run"
  wait_startup
  loaded="$(python3 "$PROC_MEM" page-loaded "$APP_LOG" || echo -)"
  # runs.tsv uses key=value tokens (scripts/lib/proc_mem.py read_runs).
  if [[ "$METRIC" == overview ]]; then
    read -r ms clusters connected attention <<<"$STARTUP"
    printf 'startupMs=%s clusters=%s connected=%s attention=%s pageLoadedMs=%s wallMs=%s\n' \
      "$ms" "$clusters" "$connected" "$attention" "$loaded" "$WALL_MS" >>"$RUNS_TSV"
    info "run $run/$RUNS: first overview after ${ms} ms (clusters=$clusters, connected=$connected, attention=$attention; page loaded ${loaded} ms; wall ${WALL_MS} ms)"
  else
    read -r ms rows total <<<"$STARTUP"
    # The overview line is printed before Pods is opened; record it for comparison when present.
    ov="$(python3 "$PROC_MEM" startup-overview "$APP_LOG" || true)"
    ov_ms="${ov%% *}"
    printf 'startupMs=%s rows=%s total=%s pageLoadedMs=%s podsKeysAtMs=%s firstOverviewMs=%s wallMs=%s\n' \
      "$ms" "$rows" "$total" "$loaded" "$KEYS_MS" "${ov_ms:--}" "$WALL_MS" >>"$RUNS_TSV"
    info "run $run/$RUNS: first Pods rows after ${ms} ms, including opening Pods (rows=$rows, total=$total; page loaded ${loaded} ms; \`g p\` sent ~${KEYS_MS} ms after launch; first overview ${ov_ms:--} ms; wall ${WALL_MS} ms)"
  fi
  if ((run == 1)); then
    [[ "$SELECT" == all-clusters ]] && click_all_clusters
    log "Idling ${IDLE_SECS}s before the memory snapshot"
    sleep "$IDLE_SECS"
    kill -0 "$APP_PID" 2>/dev/null || die "the app exited while idling (see $APP_LOG)"
    python3 "$PROC_MEM" snapshot "$APP_PID" >"$MEM_JSON"
  fi
  stop_group "$APP_PID"
  APP_PID=""
done

# --- report -------------------------------------------------------------------------------------
COMMIT="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
if ! git -C "$ROOT" diff --quiet HEAD -- 2>/dev/null; then COMMIT="$COMMIT+dirty"; fi
python3 "$PROC_MEM" report --runs "$RUNS_TSV" --memory "$MEM_JSON" --out "$OUT_JSON" \
  --idle-secs "$IDLE_SECS" --startup-metric "$METRIC" --bin "$BIN" --kubeconfig "$KCFG" --contexts "$CONTEXTS" \
  --current-context "$CURRENT_CONTEXT" --selection "$SELECT" --commit "$COMMIT"
log "Wrote $OUT_JSON (raw logs in $WORK)"
