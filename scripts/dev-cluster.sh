#!/usr/bin/env bash
# Fleetglass dev cluster (kind) on the remote docker VM.
#
#   scripts/dev-cluster.sh up       create kind cluster fleetglass-dev (idempotent), install
#                                   metrics-server and the sample workloads
#   scripts/dev-cluster.sh down     delete kind cluster fleetglass-dev and its kubeconfig context
#   scripts/dev-cluster.sh status   cluster existence, nodes, pod counts, metrics-server readiness
#   scripts/dev-cluster.sh --help
#
# Kubeconfig: always $REPO/.dev/kubeconfig (context kind-fleetglass-dev), never ~/.kube/config.
# Docker: only touches fleetglass-* resources (node container fleetglass-dev-control-plane,
# network fleetglass-kind). The API server listens on $FG_KIND_API_ADDRESS:$FG_KIND_API_PORT.
set -euo pipefail

# shellcheck source=lib/common.sh source-path=SCRIPTDIR
source "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"

KIND_NODE_IMAGE="${FG_KIND_NODE_IMAGE:-}" # empty = kind's default for its version

usage() {
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
  cat <<EOF

Environment:
  FG_KIND_API_ADDRESS  API server address on the docker host (default 127.0.0.1)
  FG_KIND_API_PORT     API server port (default 7443; 6443 is refused)
  FG_KIND_NODE_IMAGE   override the kindest/node image (default: kind's pinned default)
  DOCKER_HOST          docker daemon endpoint (default: local docker socket)

Use the cluster:  KUBECONFIG=$FG_DEV_DIR/kubeconfig kubectl get pods -A
EOF
}

render_kind_config() {
  sed -e "s|@CLUSTER_NAME@|$FG_CLUSTER_NAME|" \
      -e "s|@API_ADDRESS@|$FG_KIND_API_ADDRESS|" \
      -e "s|@API_PORT@|$FG_KIND_API_PORT|" \
      "$FG_MANIFESTS/kind-config.yaml.tmpl"
}

install_metrics_server() {
  local src
  src="$(fetch_pinned "$FG_METRICS_SERVER_URL" "$FG_METRICS_SERVER_SHA256")"
  log "metrics-server $FG_METRICS_SERVER_VERSION"
  # kind kubelets use self-signed serving certs -> --kubelet-insecure-tls.
  # kwok fake nodes (label type=kwok) have no kubelet -> excluded with --node-selector.
  # The patch is applied by inserting args after the pinned --metric-resolution line.
  grep -q -- '- --metric-resolution=15s' "$src" || die "unexpected metrics-server manifest layout"
  sed -e 's|^\( *\)- --metric-resolution=15s$|&\n\1- --kubelet-insecure-tls\n\1- --node-selector=type!=kwok|' "$src" \
    | k apply --server-side --force-conflicts -f - >/dev/null
}

wait_metrics_server() {
  log "waiting for metrics-server"
  k -n kube-system rollout status deploy/metrics-server --timeout=180s >/dev/null \
    || { warn "metrics-server deployment not ready yet"; return 0; }
  local i
  for ((i = 0; i < 30; i++)); do
    if k get --raw /apis/metrics.k8s.io/v1beta1/nodes >/dev/null 2>&1; then
      info "metrics API available"
      return 0
    fi
    sleep 4
  done
  warn "metrics API not serving yet (first scrape can take ~60s); check with: $0 status"
}

cmd_up() {
  need kind kubectl docker curl sha256sum
  fg_kubeconfig_guard
  fg_docker_guard

  if docker network inspect kind >/dev/null 2>&1; then
    info "note: a docker network named 'kind' exists on the host; it is not used or touched"
  fi

  if fg_cluster_exists; then
    log "kind cluster $FG_CLUSTER_NAME already exists; refreshing kubeconfig"
    kind export kubeconfig --name "$FG_CLUSTER_NAME" --kubeconfig "$KUBECONFIG" >/dev/null
  else
    log "creating kind cluster $FG_CLUSTER_NAME (API https://$FG_KIND_API_ADDRESS:$FG_KIND_API_PORT, network $FG_KIND_NETWORK)"
    local cfg="$FG_DEV_DIR/kind-config.yaml" start=$SECONDS
    render_kind_config >"$cfg"
    local args=(create cluster --name "$FG_CLUSTER_NAME" --config "$cfg" --kubeconfig "$KUBECONFIG" --wait 180s)
    [[ -n "$KIND_NODE_IMAGE" ]] && args+=(--image "$KIND_NODE_IMAGE")
    kind "${args[@]}"
    info "created in $(fmt_secs $((SECONDS - start)))"
  fi
  chmod 600 "$KUBECONFIG"
  kubectl --kubeconfig "$KUBECONFIG" config use-context "$FG_CONTEXT" >/dev/null
  fg_context_guard

  k wait --for=condition=Ready node --all --timeout=180s >/dev/null
  install_metrics_server
  log "sample workloads (namespaces fg-demo, fg-demo-2)"
  # Jobs are immutable once created; apply would fail on re-run only if the spec changed.
  k apply --server-side --force-conflicts -f "$FG_MANIFESTS/sample-workloads.yaml" >/dev/null
  wait_metrics_server
  k -n fg-demo rollout status deploy/web --timeout=180s >/dev/null || warn "fg-demo/web not ready yet"

  echo
  cmd_status
  echo
  log "ready: export KUBECONFIG=$KUBECONFIG"
}

cmd_down() {
  need kind kubectl docker
  fg_kubeconfig_guard
  fg_docker_guard
  if fg_cluster_exists; then
    log "deleting kind cluster $FG_CLUSTER_NAME"
    # kind delete removes only this cluster's node containers and its kubeconfig entries.
    kind delete cluster --name "$FG_CLUSTER_NAME" --kubeconfig "$KUBECONFIG"
  else
    log "kind cluster $FG_CLUSTER_NAME does not exist"
  fi
  # Remove any leftover context/cluster/user entries in our kubeconfig (never ~/.kube/config).
  if [[ -f "$KUBECONFIG" ]]; then
    kubectl --kubeconfig "$KUBECONFIG" config delete-context "$FG_CONTEXT" >/dev/null 2>&1 || true
    kubectl --kubeconfig "$KUBECONFIG" config delete-cluster "$FG_CONTEXT" >/dev/null 2>&1 || true
    kubectl --kubeconfig "$KUBECONFIG" config delete-user "$FG_CONTEXT" >/dev/null 2>&1 || true
  fi
  # kind leaves the docker network behind; remove it only if it is ours and unused.
  if docker network inspect "$FG_KIND_NETWORK" >/dev/null 2>&1; then
    local attached
    attached="$(docker network inspect "$FG_KIND_NETWORK" --format '{{len .Containers}}')"
    if [[ "$attached" == "0" ]]; then
      log "removing unused docker network $FG_KIND_NETWORK"
      docker network rm "$FG_KIND_NETWORK" >/dev/null
    else
      warn "docker network $FG_KIND_NETWORK still has $attached container(s); left in place"
    fi
  fi
  log "down"
}

cmd_status() {
  need kind kubectl docker
  fg_kubeconfig_guard
  fg_docker_guard
  if ! fg_cluster_exists; then
    echo "cluster:        $FG_CLUSTER_NAME does not exist (run: $0 up)"
    return 0
  fi
  echo "cluster:        $FG_CLUSTER_NAME exists (kind, network $FG_KIND_NETWORK)"
  echo "kubeconfig:     $KUBECONFIG (context $FG_CONTEXT)"
  if [[ ! -f "$KUBECONFIG" ]] || ! kubectl --kubeconfig "$KUBECONFIG" config get-contexts "$FG_CONTEXT" >/dev/null 2>&1; then
    echo "                context missing; run: $0 up"
    return 0
  fi
  echo "api server:     $(kubectl --kubeconfig "$KUBECONFIG" config view -o jsonpath="{.clusters[?(@.name==\"$FG_CONTEXT\")].cluster.server}")"
  if ! k get --raw /readyz >/dev/null 2>&1; then
    echo "                API server not reachable"
    return 0
  fi
  local real fake
  real="$(k get nodes -l 'type!=kwok' --no-headers 2>/dev/null | grep -c . || true)"
  fake="$(k get nodes -l 'type=kwok' --no-headers 2>/dev/null | grep -c . || true)"
  echo "nodes:          $real real, $fake kwok fake"
  k get nodes -l 'type!=kwok' --no-headers | sed 's/^/                /'
  local pods
  pods="$(k get pods -A --no-headers 2>/dev/null || true)"
  echo "pods:           $(grep -c . <<<"$pods" || true) total"
  awk '{print $4}' <<<"$pods" | sort | uniq -c | sort -rn | awk '{printf "                %6d %s\n", $1, $2}'
  local scale_ns
  scale_ns="$(k get ns -l fleetglass.dev/scale=true --no-headers 2>/dev/null | grep -c . || true)"
  echo "scale ns:       $scale_ns (scripts/scale-cluster.sh)"
  local ms
  ms="$(k -n kube-system get deploy metrics-server -o jsonpath='{.status.readyReplicas}/{.spec.replicas}' 2>/dev/null || echo 'not installed')"
  if k get --raw /apis/metrics.k8s.io/v1beta1/nodes >/dev/null 2>&1; then
    echo "metrics-server: ready ($ms replicas, metrics API serving)"
  else
    echo "metrics-server: NOT ready ($ms replicas, metrics API not serving)"
  fi
}

case "${1:-}" in
  up) cmd_up ;;
  down) cmd_down ;;
  status) cmd_status ;;
  -h | --help | help) usage ;;
  *) usage >&2; exit 2 ;;
esac
