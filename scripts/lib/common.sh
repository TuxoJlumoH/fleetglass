# shellcheck shell=bash disable=SC2034 # variables are consumed by the sourcing scripts
# Shared helpers for scripts/dev-cluster.sh and scripts/scale-cluster.sh.
# Source it; it does not run anything by itself except variable setup.
#
# Safety (Cluster safety policy):
#   - KUBECONFIG is always forced to $ROOT/.dev/kubeconfig; we refuse to run if that resolves to ~/.kube/config.
#   - Every kubectl call goes through `k`, which pins --context kind-fleetglass-dev.
#   - Docker resources touched are only fleetglass-* (kind node containers, network fleetglass-kind).

FG_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FG_DEV_DIR="$FG_ROOT/.dev"
FG_CACHE_DIR="$FG_DEV_DIR/cache"
FG_MANIFESTS="$FG_ROOT/scripts/manifests"

FG_CLUSTER_NAME="fleetglass-dev"
FG_CONTEXT="kind-$FG_CLUSTER_NAME"
FG_KIND_NETWORK="fleetglass-kind"
FG_KIND_API_ADDRESS="${FG_KIND_API_ADDRESS:-127.0.0.1}"
FG_KIND_API_PORT="${FG_KIND_API_PORT:-7443}"
FG_MAX_SCALE_PODS="${FG_MAX_SCALE_PODS:-5000}"
if [[ -z "${DOCKER_HOST:-}" && -n "${FG_DOCKER_HOST:-}" ]]; then
  export DOCKER_HOST="$FG_DOCKER_HOST"
fi

# Pinned upstream manifests (fetched once into .dev/cache, verified by sha256).
FG_KWOK_VERSION="v0.8.0"
FG_METRICS_SERVER_VERSION="v0.8.1"
FG_KWOK_URL="https://github.com/kubernetes-sigs/kwok/releases/download/$FG_KWOK_VERSION/kwok.yaml"
FG_KWOK_SHA256="a4c16e6431e382dcb5c1903139344b7a68652f16a6460337fe17a678a426f405"
FG_KWOK_STAGES_URL="https://github.com/kubernetes-sigs/kwok/releases/download/$FG_KWOK_VERSION/stage-fast.yaml"
FG_KWOK_STAGES_SHA256="2f28d95564ec43056c0873f7a25ac7d2a5bba4c8496c72f8b3ee73fd4f54ee24"
FG_METRICS_SERVER_URL="https://github.com/kubernetes-sigs/metrics-server/releases/download/$FG_METRICS_SERVER_VERSION/components.yaml"
FG_METRICS_SERVER_SHA256="4a672c4891902573a3ff753cece5de1bf1f55dd053403dfec39df9d1636b7ff1"

if [[ -t 1 ]]; then
  C_OK=$'\e[32m' C_WARN=$'\e[33m' C_ERR=$'\e[31m' C_DIM=$'\e[2m' C_OFF=$'\e[0m'
else
  C_OK='' C_WARN='' C_ERR='' C_DIM='' C_OFF=''
fi

log()  { printf '%s==>%s %s\n' "$C_OK" "$C_OFF" "$*"; }
info() { printf '    %s%s%s\n' "$C_DIM" "$*" "$C_OFF"; }
warn() { printf '%s[warn]%s %s\n' "$C_WARN" "$C_OFF" "$*" >&2; }
die()  { printf '%s[error]%s %s\n' "$C_ERR" "$C_OFF" "$*" >&2; exit 1; }

need() {
  local c
  for c in "$@"; do
    command -v "$c" >/dev/null 2>&1 || die "'$c' not found in PATH (run scripts/bootstrap.sh --check)"
  done
}

# Force the project kubeconfig and refuse anything that would resolve to ~/.kube/config.
fg_kubeconfig_guard() {
  mkdir -p "$FG_DEV_DIR"
  export KUBECONFIG="$FG_DEV_DIR/kubeconfig"
  local home_cfg="$HOME/.kube/config" resolved home_resolved
  resolved="$(realpath -m -- "$KUBECONFIG")"
  home_resolved="$(realpath -m -- "$home_cfg")"
  if [[ -z "$KUBECONFIG" || "$KUBECONFIG" == *:* || "$resolved" == "$home_resolved" || "$resolved" == "$(realpath -m -- "$HOME/.kube")"/* ]]; then
    die "refusing to run: KUBECONFIG would resolve to '$resolved' (must be $FG_DEV_DIR/kubeconfig, never ~/.kube/config)"
  fi
  if [[ -e "$KUBECONFIG" && -e "$home_cfg" && "$KUBECONFIG" -ef "$home_cfg" ]]; then
    die "refusing to run: $KUBECONFIG is the same file as ~/.kube/config"
  fi
}

fg_docker_guard() {
  [[ "$FG_KIND_API_PORT" != "6443" ]] || die "FG_KIND_API_PORT=6443 is the VM's k3s port; refusing (use 7443)"
  [[ "$FG_KIND_NETWORK" == fleetglass-* && "$FG_CLUSTER_NAME" == fleetglass-* ]] \
    || die "internal: docker resource names must start with fleetglass-"
  export KIND_EXPERIMENTAL_DOCKER_NETWORK="$FG_KIND_NETWORK"
  timeout 20 docker version --format '{{.Server.Version}}' >/dev/null 2>&1 \
    || die "docker not reachable at DOCKER_HOST=${DOCKER_HOST:-local}"
}

# kubectl pinned to our context and kubeconfig.
k() { kubectl --kubeconfig "$KUBECONFIG" --context "$FG_CONTEXT" "$@"; }

fg_cluster_exists() { kind get clusters 2>/dev/null | grep -qx "$FG_CLUSTER_NAME"; }

# Must be called before applying anything: the kubeconfig must contain our context and it must be current.
fg_context_guard() {
  [[ -f "$KUBECONFIG" ]] || die "$KUBECONFIG missing; run scripts/dev-cluster.sh up"
  local cur
  cur="$(kubectl --kubeconfig "$KUBECONFIG" config current-context 2>/dev/null || true)"
  [[ "$cur" == "$FG_CONTEXT" ]] || die "current context in $KUBECONFIG is '$cur', expected '$FG_CONTEXT'; refusing to apply"
  local server
  server="$(kubectl --kubeconfig "$KUBECONFIG" config view -o jsonpath="{.clusters[?(@.name==\"$FG_CONTEXT\")].cluster.server}")"
  [[ "$server" == "https://$FG_KIND_API_ADDRESS:$FG_KIND_API_PORT" ]] \
    || die "context $FG_CONTEXT points at '$server', expected https://$FG_KIND_API_ADDRESS:$FG_KIND_API_PORT; refusing"
}

# fetch_pinned <url> <sha256> -> prints the cached path
fetch_pinned() {
  local url="$1" sum="$2" out
  mkdir -p "$FG_CACHE_DIR"
  out="$FG_CACHE_DIR/$(basename "$(dirname "$url")")-$(basename "$url")"
  if [[ ! -f "$out" ]] || ! echo "$sum  $out" | sha256sum -c --status; then
    curl -fsSL --retry 3 -o "$out.tmp" "$url" || die "download failed: $url"
    echo "$sum  $out.tmp" | sha256sum -c --status || { rm -f "$out.tmp"; die "sha256 mismatch for $url"; }
    mv "$out.tmp" "$out"
  fi
  printf '%s\n' "$out"
}

# Format seconds as e.g. 1m23s.
fmt_secs() { local s="$1"; if ((s >= 60)); then printf '%dm%02ds' $((s / 60)) $((s % 60)); else printf '%ds' "$s"; fi; }
