#!/usr/bin/env bash
# E2E app-level smoke test for Fleetglass (R-15).
#
# Drives the *built* desktop app (target/debug/fleetglass with ui/dist embedded, built by
# `pnpm -C ui tauri build --debug --no-bundle`; no Vite dev server) under Xvfb with Mesa
# software rendering, against the kind cluster `fleetglass-dev`:
#   1. start the app -> the Overview tab (initial tab since v0.2) shows "Overview", the kind
#      context name and live data for it: app.log has the "first overview returned" line (needs a
#      Connected cluster) and the 02-00 sample `crashloop` is listed under Needs attention;
#      then `g p` (fallback: the sidebar "Pods" item) -> "<N> pods" equal to kubectl's count,
#      and filtering the table to "web" shows the web-* rows
#   2. double-click a fg-demo/web pod -> the resource drawer opens
#   3. Logs tab -> a real nginx access-log line (`GET /` / `kube-probe`) is rendered
#   4. Ctrl+K "Deployments" -> select fg-demo/web -> `s` -> ActionConfirmDialog -> set the
#      replica count -> Enter. kubectl must see spec.replicas change; the UI must report the
#      request; audit.jsonl must get a new scale entry. Then scale back the same way.
#      kubectl is only used to assert, and in the EXIT trap as a safety net restore.
# Input is synthesised with XTest (scripts/lib/xinput.py), the screen is read with OCR.
#
# Required tools (checked by `need` below):
#   Xvfb, xdpyinfo (xorg-xdpyinfo), import + magick (ImageMagick 7), tesseract (+ eng data),
#   python3 with libX11.so.6 + libXtst.so.6 (libxtst), setsid (util-linux), kubectl, kind,
#   pnpm, cargo, Mesa EGL (software rendering) and the Tauri/WebKitGTK runtime deps.
#   Arch:   pacman -S xorg-server-xvfb xorg-xdpyinfo imagemagick tesseract tesseract-data-eng libxtst
#   Debian: apt install xvfb x11-utils imagemagick tesseract-ocr libxtst6 (IM6 has no `magick`)
#   macOS:  not supported (needs X11/Xvfb + WebKitGTK).
#
# Isolation: KUBECONFIG is a pinned, minified copy of the kind-fleetglass-dev context from
# .dev/kubeconfig (other contexts in that file are never shown to the app or used here), and
# XDG_* point at .dev/xdg*, so the app's settings and audit log live in .dev/xdg/config/fleetglass.
# Exit codes: 0 pass, 1 failure, 130 SIGINT, 143 SIGTERM.

set -euo pipefail

FG_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/common.sh
source "$FG_ROOT/scripts/lib/common.sh"

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//'
  echo "Usage: scripts/e2e-smoke.sh [--help]"
  echo "Env: FG_E2E_TIMEOUT (seconds per wait, default 40), DISPLAY_NUM (first display to try, default 77)"
}
case "${1:-}" in
  -h | --help) usage; exit 0 ;;
  "") ;;
  *) usage >&2; exit 2 ;;
esac

TIMEOUT="${FG_E2E_TIMEOUT:-40}"
XINPUT="$FG_ROOT/scripts/lib/xinput.py"
APP_BIN="$FG_ROOT/target/debug/fleetglass"
NS="fg-demo"
DEPLOY="web"

need Xvfb xdpyinfo import magick tesseract python3 setsid kubectl kind pnpm cargo
python3 -c 'import ctypes; ctypes.CDLL("libX11.so.6"); ctypes.CDLL("libXtst.so.6")' 2>/dev/null \
  || die "python3 cannot load libX11.so.6/libXtst.so.6 (install libxtst / libxtst6)"

OUT="$FG_DEV_DIR/smoke"
mkdir -p "$OUT"
rm -f "$OUT"/*.png "$OUT"/*.txt "$OUT"/*.tsv

# --- cluster safety ---------------------------------------------------------------------------
fg_kubeconfig_guard # forces KUBECONFIG=.dev/kubeconfig, refuses ~/.kube/config
[[ -f "$KUBECONFIG" ]] || die "$KUBECONFIG missing; run scripts/dev-cluster.sh up"
PINNED_KUBECONFIG="$OUT/kubeconfig"
(umask 077 && kubectl --kubeconfig "$KUBECONFIG" config view --minify --flatten \
  --context "$FG_CONTEXT" >"$PINNED_KUBECONFIG") \
  || die "context $FG_CONTEXT not found in $KUBECONFIG; run scripts/dev-cluster.sh up"
export KUBECONFIG="$PINNED_KUBECONFIG"
fg_context_guard # current context must be ours and point at $FG_KIND_API_ADDRESS:$FG_KIND_API_PORT

fg_cluster_exists || die "kind cluster '$FG_CLUSTER_NAME' not found. Run scripts/dev-cluster.sh up first."
k cluster-info >/dev/null 2>&1 || die "kind cluster is not reachable via $KUBECONFIG. Run scripts/dev-cluster.sh up."

if ! k -n "$NS" get deployment "$DEPLOY" >/dev/null 2>&1; then
  info "Applying sample workloads..."
  k apply -f "$FG_MANIFESTS/sample-workloads.yaml" >/dev/null
fi
k -n "$NS" rollout status "deployment/$DEPLOY" --timeout=120s >/dev/null \
  || die "$NS/$DEPLOY is not available"

# --- isolated app environment -----------------------------------------------------------------
export XDG_RUNTIME_DIR="$FG_DEV_DIR/xdg-runtime"
export XDG_CONFIG_HOME="$FG_DEV_DIR/xdg/config"
export XDG_DATA_HOME="$FG_DEV_DIR/xdg/data"
export XDG_CACHE_HOME="$FG_DEV_DIR/xdg/cache"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
AUDIT_LOG="$XDG_CONFIG_HOME/fleetglass/audit.jsonl"

# Software rendering for headless WebKitGTK.
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export LIBGL_ALWAYS_SOFTWARE=1
if [[ -f /usr/share/glvnd/egl_vendor.d/50_mesa.json ]]; then
  export __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json
fi

# --- cleanup ----------------------------------------------------------------------------------
XVFB_PID=""
BUILD_PID=""
APP_PID=""
RESTORE_REPLICAS=""
CLEANED=0
EXIT_CODE=""

# Stops a process group we started with setsid (pgid == pid), TERM then KILL.
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
  info "Cleaning up..."
  [[ -n "$APP_PID" ]] && stop_group "$APP_PID"
  [[ -n "$BUILD_PID" ]] && stop_group "$BUILD_PID"
  [[ -n "$XVFB_PID" ]] && stop_group "$XVFB_PID"
  if [[ -n "$RESTORE_REPLICAS" ]]; then
    local cur
    cur="$(k -n "$NS" get deployment "$DEPLOY" -o jsonpath='{.spec.replicas}' 2>/dev/null)"
    if [[ "$cur" != "$RESTORE_REPLICAS" ]]; then
      warn "Safety net: restoring $NS/$DEPLOY from '${cur:-?}' to $RESTORE_REPLICAS replicas with kubectl"
      k -n "$NS" scale deployment "$DEPLOY" --replicas="$RESTORE_REPLICAS" >/dev/null 2>&1 \
        || warn "RESTORE FAILED: run: KUBECONFIG=$FG_DEV_DIR/kubeconfig kubectl --context $FG_CONTEXT -n $NS scale deployment $DEPLOY --replicas=$RESTORE_REPLICAS"
    fi
  fi
  if [[ "$code" -eq 0 ]]; then
    log "E2E smoke test PASSED"
  else
    warn "E2E smoke test FAILED (exit $code); screenshots and logs in $OUT"
  fi
  exit "$code"
}
on_signal() {
  EXIT_CODE="$1"
  warn "Interrupted (signal), exiting $1"
  exit "$1"
}
trap cleanup EXIT
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

# Runs a long command in its own process group and waits for it, so signals interrupt the
# script right away and cleanup can stop the whole command.
run_logged() {
  local logf="$1"
  shift
  setsid "$@" >"$logf" 2>&1 &
  BUILD_PID=$!
  local rc=0
  wait "$BUILD_PID" || rc=$?
  BUILD_PID=""
  return "$rc"
}

# --- screen helpers ---------------------------------------------------------------------------
# shot NAME: screenshot + an inverted/thresholded copy (OCR works far better on the dark UI)
# + a 2x upscaled inverted copy (the table's small font is only read reliably when enlarged).
shot() {
  import -window root "$OUT/$1.png"
  magick "$OUT/$1.png" -negate -threshold 60% "$OUT/$1-n.png"
  magick "$OUT/$1.png" -resize 200% -negate -threshold 55% "$OUT/$1-x2.png"
  # The 1280x800 app window at 2x without thresholding: reads small toolbar text ("23 pods").
  magick "$OUT/$1.png" -crop 1280x800+0+0 +repage -resize 200% "$OUT/$1-w2.png"
}
# ocr NAME: OCR text of all variants into NAME.txt. Results are always read back from files
# (no `producer | grep -q` pipes, which die of SIGPIPE under pipefail).
ocr() {
  local v
  for v in "$1-n" "$1" "$1-x2" "$1-w2"; do tesseract "$OUT/$v.png" - 2>/dev/null; done >"$OUT/$1.txt"
}
# tsv NAME: word boxes "left top width height text" (screen coordinates) into NAME.tsv.
tsv() {
  local v
  {
    for v in "$1-n" "$1"; do
      tesseract "$OUT/$v.png" - tsv 2>/dev/null \
        | awk -F'\t' 'NF >= 12 && $1 == 5 && $12 != "" {print $7, $8, $9, $10, $12}'
    done
    for v in "$1-x2" "$1-w2"; do
      tesseract "$OUT/$v.png" - tsv 2>/dev/null \
        | awk -F'\t' 'NF >= 12 && $1 == 5 && $12 != "" {print int($7/2), int($8/2), int($9/2), int($10/2), $12}'
    done
  } >"$OUT/$1.tsv"
}
# center_of NAME AWK_COND: centre "x y" of the first word box matching the awk condition on
# $5 (text), $1 left, $2 top.
center_of() {
  tsv "$1"
  awk "!found && ($2) {print int(\$1 + \$3 / 2), int(\$2 + \$4 / 2); found = 1}" "$OUT/$1.tsv"
}
# ocr_strips PNG: OCR of the main pane (right of the sidebar) in overlapping 180 px strips,
# upscaled, greyscale, two contrast levels. Tesseract drops small coloured text (status line, table
# cells) on a full dark screenshot; strips read it reliably.
ocr_strips() {
  local y
  for y in $(seq 0 90 900); do
    magick "$1" -crop 1320x180+280+"$y" +repage -resize 300% -colorspace Gray -negate "$OUT/.strip.png"
    tesseract "$OUT/.strip.png" - 2>/dev/null
    magick "$OUT/.strip.png" -level 0%,70% "$OUT/.strip2.png"
    tesseract "$OUT/.strip2.png" - 2>/dev/null
  done
}
xi() { python3 "$XINPUT" "$@"; }

# wait_for NAME ERE [TIMEOUT]: screenshot until the OCR text matches the extended regex.
wait_for() {
  local name="$1" re="$2" t="${3:-$TIMEOUT}" end
  end=$((SECONDS + t))
  while ((SECONDS < end)); do
    kill -0 "$APP_PID" 2>/dev/null || die "the app exited (see $OUT/app.log)"
    shot "$name"
    ocr "$name"
    if grep -Eq -- "$re" "$OUT/$name.txt"; then return 0; fi
    sleep 1
  done
  return 1
}

replicas_now() { k -n "$NS" get deployment "$DEPLOY" -o jsonpath='{.spec.replicas}'; }
audit_scale_count() {
  [[ -f "$AUDIT_LOG" ]] || { echo 0; return; }
  grep -c "\"action\":\"scale\".*\"namespace\":\"$NS\",\"name\":\"$DEPLOY\".*\"success\":true" "$AUDIT_LOG" || true
}

# --- Xvfb -------------------------------------------------------------------------------------
DISP="${DISPLAY_NUM:-77}"
while [[ -e "/tmp/.X${DISP}-lock" || -e "/tmp/.X11-unix/X${DISP}" ]]; do
  DISP=$((DISP + 1))
done
export DISPLAY=":$DISP"
log "Starting Xvfb on $DISPLAY..."
setsid Xvfb "$DISPLAY" -screen 0 1600x1000x24 -nolisten tcp >"$OUT/xvfb.log" 2>&1 &
XVFB_PID=$!
for i in $(seq 1 100); do
  xdpyinfo >/dev/null 2>&1 && break
  kill -0 "$XVFB_PID" 2>/dev/null || die "Xvfb exited (see $OUT/xvfb.log)"
  ((i == 100)) && die "Xvfb did not become ready on $DISPLAY within 10 s"
  sleep 0.1
done

# --- build ------------------------------------------------------------------------------------
log "Building the app (pnpm -C ui tauri build --debug --no-bundle, embeds ui/dist)..."
run_logged "$OUT/build.log" pnpm -C "$FG_ROOT/ui" tauri build --debug --no-bundle \
  || die "app build failed (see $OUT/build.log)"
[[ -x "$APP_BIN" ]] || die "$APP_BIN missing after build"

log "Starting Fleetglass under Xvfb ($DISPLAY)..."
AUDIT_BEFORE="$(audit_scale_count)"
setsid "$APP_BIN" >"$OUT/app.log" 2>&1 &
APP_PID=$!

# --- step 1a: Overview on launch --------------------------------------------------------------
# Since v0.2 (02-08b) the initial tab is Overview, not Pods.
log "Step 1a: the app opens on the Overview with $FG_CONTEXT"
end=$((SECONDS + TIMEOUT * 2))
ok=0
while ((SECONDS < end)); do
  kill -0 "$APP_PID" 2>/dev/null || die "the app exited (see $OUT/app.log)"
  sleep 2
  shot 1-overview
  ocr 1-overview
  text="$(<"$OUT/1-overview.txt")"
  # Card titles are uppercase in CSS, so match the context name case-insensitively.
  if grep -q 'Overview' <<<"$text" && grep -qiF "$FG_CONTEXT" <<<"$text"; then
    ok=1
    # The Needs attention card renders from the first push (cluster still Connecting), so it
    # proves nothing. Live data = the backend's first-overview line (a Connected cluster) plus
    # the 02-00 sample `crashloop` listed under Needs attention.
    if grep -q 'fleetglass startup: first overview returned' "$OUT/app.log" \
      && grep -Eiq 'needs ?attention' <<<"$text" && grep -q 'crashloop' <<<"$text"; then
      ok=2
      break
    fi
  fi
done
((ok)) || die "the Overview with '$FG_CONTEXT' was not shown on launch (OCR in $OUT/1-overview.txt)"
if ((ok == 2)); then
  info "Overview shows $FG_CONTEXT with live data ('crashloop' under Needs attention)"
  info "app.log: $(grep -m1 -o 'first overview returned.*' "$OUT/app.log")"
elif grep -q 'No cluster selected' "$OUT/1-overview.txt"; then
  die "Overview says 'No cluster selected': the current context was not selected on launch (see $OUT/1-overview.png)"
elif ! grep -q 'fleetglass startup: first overview returned' "$OUT/app.log"; then
  die "Overview shows $FG_CONTEXT but app.log has no 'first overview returned' line: the cluster never connected (see $OUT/1-overview.png, $OUT/app.log)"
else
  die "Overview is connected but 'crashloop' was not read under Needs attention (OCR in $OUT/1-overview.txt, $OUT/1-overview.png)"
fi

# --- step 1b: open Pods -----------------------------------------------------------------------
log "Step 1b: open Pods (keys g p) -> pods visible in the table"
# No window manager: keyboard focus follows the pointer, so park it in the main pane (right of
# the sidebar, below the tabs) without clicking anything, then send the go-to chord.
xi move 700 420
xi key g p
end=$((SECONDS + TIMEOUT * 2))
fallback=$((SECONDS + TIMEOUT / 2))
ok=0
while ((SECONDS < end)); do
  kill -0 "$APP_PID" 2>/dev/null || die "the app exited (see $OUT/app.log)"
  sleep 2
  shot 1-pods
  ocr 1-pods
  text="$(<"$OUT/1-pods.txt")"
  expected="$(k get pods -A --no-headers 2>/dev/null | wc -l)"
  if grep -Eq "\\b${expected} pods\\b" <<<"$text"; then
    ok=1
    break
  fi
  if ((fallback && SECONDS >= fallback)); then
    fallback=0
    # The chord did not open Pods (focus?): click the sidebar "Pods" item instead (x < 280 is
    # the sidebar; the Overview's "PODS" stat label is uppercase and in the main pane).
    pods_xy="$(center_of 1-pods '$5 == "Pods" && $1 < 280')"
    if [[ -n "$pods_xy" ]]; then
      warn "\`g p\` did not open Pods; clicking the sidebar 'Pods' item at $pods_xy"
      # shellcheck disable=SC2086 # "x y"
      xi click $pods_xy
    else
      warn "\`g p\` did not open Pods and the sidebar 'Pods' item was not found (see $OUT/1-pods.tsv)"
    fi
  fi
done
((ok)) || die "pods table did not show '${expected} pods' after opening Pods (OCR in $OUT/1-pods.txt)"
info "Pods tab shows '${expected} pods', matching kubectl"

# The 02-00 sample pods sort above fg-demo/web, pushing its rows below the 1280x800 window:
# filter the table to "web" (click the "Filter pods" box) so the web-* rows are on screen for step 2.
filter_xy="$(center_of 1-pods '$5 == "Filter" && $1 > 240 && $2 < 120')"
[[ -n "$filter_xy" ]] || die "could not locate the 'Filter pods' box (see $OUT/1-pods.tsv)"
# shellcheck disable=SC2086 # "x y"
xi click $filter_xy
xi type web
end=$((SECONDS + TIMEOUT))
ok=0
while ((SECONDS < end)); do
  sleep 1
  shot 1-pods
  ocr 1-pods
  grep -Eq '\bweb-[a-z0-9]' "$OUT/1-pods.txt" && { ok=1; break; }
done
((ok)) || die "filtering the pods table to 'web' did not show web-* rows (OCR in $OUT/1-pods.txt)"
info "filter 'web': web-* rows visible"

# --- step 2: drawer ---------------------------------------------------------------------------
log "Step 2: open the resource drawer on a web pod"
web_xy="$(center_of 1-pods '$5 ~ /^web-[a-z0-9]/')"
[[ -n "$web_xy" ]] || die "could not locate a web-* row (see $OUT/1-pods.tsv)"
info "double-clicking web pod at $web_xy"
# shellcheck disable=SC2086 # "x y"
xi click $web_xy double
end=$((SECONDS + TIMEOUT))
ok=0
while ((SECONDS < end)); do
  sleep 1
  shot 2-drawer
  ocr 2-drawer
  text="$(<"$OUT/2-drawer.txt")"
  if grep -Eiq '\bsummary\b.*\byaml\b.*\blogs\b' <<<"$text"; then
    ok=1
    break
  fi
done
((ok)) || die "resource drawer did not open (see $OUT/2-drawer.txt)"
info "drawer open (Summary / YAML / Logs tabs)"

# --- step 3: logs -----------------------------------------------------------------------------
log "Step 3: live logs in the drawer"
logs_xy="$(center_of 2-drawer '$5 == "Logs"')"
[[ -n "$logs_xy" ]] || die "could not locate the Logs tab (see $OUT/2-drawer.tsv)"
# shellcheck disable=SC2086
xi click $logs_xy
# nginx access-log lines of the readiness probe; "nginx" alone is the container name.
wait_for 3-logs 'GET / HTTP|kube-probe' || die "no nginx log line rendered in the drawer (see $OUT/3-logs.txt)"
info "nginx access-log lines rendered"
xi key Escape
sleep 1

# --- step 4: scale through the app ------------------------------------------------------------
INITIAL_REPLICAS="$(replicas_now)"
[[ "$INITIAL_REPLICAS" =~ ^[0-9]+$ ]] || die "cannot read spec.replicas of $NS/$DEPLOY"
TARGET_REPLICAS=$((INITIAL_REPLICAS == 2 ? 3 : 2))
RESTORE_REPLICAS="$INITIAL_REPLICAS"

log "Step 4: open the Deployments view (Ctrl+K → Deployments)"
xi key Control_L+k
sleep 1
xi type Deployments
sleep 1
xi key Return
sleep 2
xi key slash
xi type "$DEPLOY"
sleep 1

# Selects the fg-demo/web Deployment row (exact name "web"; its pods are web-<hash>).
select_deploy_row() {
  local name="$1" end xy
  end=$((SECONDS + TIMEOUT))
  while ((SECONDS < end)); do
    shot "$name"
    ocr "$name"
    tsv "$name"
    # A cell exactly "web" on the same text line as an "fg-demo" namespace cell (this skips the
    # filter box, which also shows "web").
    xy="$(awk -v ns="$NS" -v dep="$DEPLOY" '
      { top[NR] = $2; txt[NR] = $5; cx[NR] = int($1 + $3 / 2); cy[NR] = int($2 + $4 / 2) }
      END {
        for (i = 1; i <= NR; i++) if (txt[i] == dep)
          for (j = 1; j <= NR; j++) if (txt[j] == ns && top[j] - top[i] < 8 && top[i] - top[j] < 8) {
            print cx[i], cy[i]; exit
          }
      }' "$OUT/$name.tsv")"
    if [[ -n "$xy" ]]; then
      # shellcheck disable=SC2086
      xi click $xy
      sleep 0.5
      return 0
    fi
    sleep 1
  done
  # The pod table always shows "<N> pods"; pod names may be misread by OCR.
  if grep -Eq "\\b${DEPLOY}-[a-z0-9]+-[a-z0-9]+|\\b[0-9]+ pods\\b" "$OUT/$name.txt"; then
    die "UI gap: the Deployments view lists pods (web-<hash>), not Deployments, so the scale action cannot be reached through the app (see $OUT/$name.png)"
  fi
  die "Deployment row '$DEPLOY' not found in the Deployments view (see $OUT/$name.tsv)"
}

# scale_via_app FROM TO STEP: `s` on the selected row, set the count in ActionConfirmDialog, Enter.
scale_via_app() {
  local from="$1" to="$2" step="$3" xy end
  select_deploy_row "$step-row"
  xi key s
  wait_for "$step-dialog" 'Scale deployment' || die "scale dialog did not open (see $OUT/$step-dialog.txt)"
  grep -Eq -- "--replicas=${from}\b" "$OUT/$step-dialog.txt" \
    || warn "dialog did not show the prefilled count $from (OCR); continuing"
  # Clicking the "Replicas" label focuses its input.
  xy="$(center_of "$step-dialog" '$5 == "Replicas"')"
  [[ -n "$xy" ]] || die "Replicas field not found in the dialog (see $OUT/$step-dialog.tsv)"
  # shellcheck disable=SC2086
  xi click $xy
  xi key Control_L+a BackSpace
  xi type "$to"
  if grep -q 'Protected context' "$OUT/$step-dialog.txt"; then
    info "context is protected: typing the name"
    xy="$(center_of "$step-dialog" '$5 == "confirm"')"
    # shellcheck disable=SC2086
    [[ -n "$xy" ]] && xi click $xy && xi type "$DEPLOY"
    xy="$(center_of "$step-dialog" '$5 == "Replicas"')"
    # shellcheck disable=SC2086
    xi click $xy
  fi
  # The kubectl preview reflects the typed count (OCR often drops spaces in "Scale to N replicas").
  wait_for "$step-filled" "--replicas=${to}\\b" 15 || die "dialog did not accept $to replicas (see $OUT/$step-filled.txt)"
  # The preview shows the new count, so it has loaded and Enter in the Replicas field submits.
  # The success status is only shown for 6 s: grab raw screenshots right away, OCR them later.
  xi key Return
  local i ok=0
  for i in $(seq -w 1 12); do
    import -window root "$OUT/$step-done-$i.png"
    [[ "$(replicas_now)" == "$to" ]] && ok=1
    ((ok)) && ((10#$i >= 4)) && break
    sleep 0.3
  done
  end=$((SECONDS + TIMEOUT))
  while ((SECONDS < end)) && [[ "$(replicas_now)" != "$to" ]]; do sleep 1; done
  [[ "$(replicas_now)" == "$to" ]] \
    || die "spec.replicas of $NS/$DEPLOY did not change to $to within ${TIMEOUT}s (still $(replicas_now); see $OUT/$step-done-*.png)"
  info "kubectl: spec.replicas = $to"
  ok=0
  for f in "$OUT/$step-done-"[0-9][0-9].png; do
    ocr_strips "$f" >"${f%.png}.txt"
    if grep -Eiq "${to} ?replicas? ?requested for deployment ${NS}/${DEPLOY}" "${f%.png}.txt"; then ok=1; break; fi
  done
  ((ok)) || die "the app did not show 'Scale to $to replicas requested for deployment $NS/$DEPLOY' (see $OUT/$step-done-*.txt)"
  info "app: 'Scale to $to replicas requested for deployment $NS/$DEPLOY'"
  # The live watch must update the row's READY cell to the new desired count (*/to).
  end=$((SECONDS + TIMEOUT))
  ok=0
  while ((SECONDS < end)); do
    import -window root "$OUT/$step-ready.png"
    ocr_strips "$OUT/$step-ready.png" >"$OUT/$step-ready.txt"
    if grep -Eq "\\b${NS} ${DEPLOY} [0-9]+/${to}\\b" "$OUT/$step-ready.txt"; then ok=1; break; fi
    sleep 1
  done
  ((ok)) || die "the $NS/$DEPLOY row did not show READY */$to (see $OUT/$step-ready.txt)"
  info "app: row shows '$(grep -Eo "${NS} ${DEPLOY} [0-9]+/${to}" "$OUT/$step-ready.txt" | head -1)'"
}

log "Step 4a: scale $NS/$DEPLOY $INITIAL_REPLICAS → $TARGET_REPLICAS through the app"
scale_via_app "$INITIAL_REPLICAS" "$TARGET_REPLICAS" 4a
log "Step 4b: scale $NS/$DEPLOY back $TARGET_REPLICAS → $INITIAL_REPLICAS through the app"
scale_via_app "$TARGET_REPLICAS" "$INITIAL_REPLICAS" 4b
RESTORE_REPLICAS=""

AUDIT_AFTER="$(audit_scale_count)"
((AUDIT_AFTER >= AUDIT_BEFORE + 2)) \
  || die "audit log $AUDIT_LOG has $AUDIT_AFTER successful scale entries for $NS/$DEPLOY, expected >= $((AUDIT_BEFORE + 2))"
info "audit.jsonl: $((AUDIT_AFTER - AUDIT_BEFORE)) new scale entries"

log "All E2E smoke steps passed"
