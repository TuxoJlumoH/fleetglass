#!/usr/bin/env bash
# Fleetglass scale cluster: kwok fake nodes + fake pods INSIDE the fleetglass-dev kind cluster.
#
#   scripts/scale-cluster.sh up --pods N [--namespaces M] [--nodes K] [--batch B]
#        install the kwok controller (v0.8.0) in kube-system, create K fake nodes and converge to
#        exactly N fake pods (Deployments in namespaces scale-ns-000..), in batches of B pods
#   scripts/scale-cluster.sh down     remove fake pods, scale namespaces, fake nodes and kwok
#   scripts/scale-cluster.sh status   fake nodes / pods summary
#   scripts/scale-cluster.sh --help
#
# Requires scripts/dev-cluster.sh up first. Real pods (kube-system, fg-demo...) are untouched, so
# the app sees one cluster with real and fake pods. N is capped at $FG_MAX_SCALE_PODS (5000).
set -euo pipefail

# shellcheck source=lib/common.sh source-path=SCRIPTDIR
source "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"

SCALE_LABEL="fleetglass.dev/scale"
NS_PREFIX="scale-ns-"
NODE_PREFIX="kwok-node-"
REPLICAS_PER_DEPLOY=50     # pods per Deployment (last one in a namespace takes the remainder)
PODS_PER_FAKE_NODE=1000    # fake node pod capacity

usage() {
  sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
  cat <<EOF

Options for up:
  --pods N          total fake pods (required, 0..$FG_MAX_SCALE_PODS)
  --namespaces M    number of scale namespaces (default: max(1, N/250), at most 100)
  --nodes K         number of fake nodes (default: max(2, N/500)); each holds up to $PODS_PER_FAKE_NODE pods
  --batch B         add at most B pods per step and wait for them to be Running (default 1000)
  --timeout SECS    max wait per batch (default 300)
  --force-large     allow N > FG_MAX_SCALE_PODS (only on a bigger host; agents must never pass it)

Re-running up with another N converges to the new N (scales Deployments up or down,
removes surplus namespaces). Fake pods count towards "kubectl get pods -A".
EOF
}

# ---------- manifest generation ----------

gen_nodes() {
  local k="$1" i name
  for ((i = 0; i < k; i++)); do
    printf -v name '%s%03d' "$NODE_PREFIX" "$i"
    cat <<EOF
---
apiVersion: v1
kind: Node
metadata:
  name: $name
  annotations:
    kwok.x-k8s.io/node: fake
    node.alpha.kubernetes.io/ttl: "0"
  labels:
    type: kwok
    $SCALE_LABEL: "true"
    kubernetes.io/arch: amd64
    kubernetes.io/os: linux
    kubernetes.io/hostname: $name
    kubernetes.io/role: agent
    node-role.kubernetes.io/agent: ""
    topology.kubernetes.io/zone: fake-zone-$((i % 3))
spec:
  taints:
    - key: kwok.x-k8s.io/node
      value: fake
      effect: NoSchedule
status:
  allocatable: {cpu: "64", memory: 256Gi, pods: "$PODS_PER_FAKE_NODE"}
  capacity: {cpu: "64", memory: 256Gi, pods: "$PODS_PER_FAKE_NODE"}
EOF
  done
}

# Prints "ns deploy replicas" lines describing the desired state for N pods over M namespaces.
plan() {
  local n="$1" m="$2" i per rem cnt d left r ns
  per=$((n / m)) rem=$((n % m))
  for ((i = 0; i < m; i++)); do
    cnt=$((per + (i < rem ? 1 : 0)))
    printf -v ns '%s%03d' "$NS_PREFIX" "$i"
    d=0 left=$cnt
    while ((left > 0)); do
      r=$((left < REPLICAS_PER_DEPLOY ? left : REPLICAS_PER_DEPLOY))
      printf '%s app-%03d %d\n' "$ns" "$d" "$r"
      left=$((left - r)) d=$((d + 1))
    done
  done
}

gen_namespaces() {
  local m="$1" i ns
  for ((i = 0; i < m; i++)); do
    printf -v ns '%s%03d' "$NS_PREFIX" "$i"
    printf -- '---\napiVersion: v1\nkind: Namespace\nmetadata:\n  name: %s\n  labels: {%s: "true"}\n' "$ns" "$SCALE_LABEL"
  done
}

gen_deployment() {
  local ns="$1" name="$2" replicas="$3"
  cat <<EOF
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: $name
  namespace: $ns
  labels: {app: $name, $SCALE_LABEL: "true"}
spec:
  replicas: $replicas
  revisionHistoryLimit: 1
  selector:
    matchLabels: {app: $name}
  template:
    metadata:
      labels: {app: $name, tier: fake, $SCALE_LABEL: "true"}
    spec:
      nodeSelector: {type: kwok}
      tolerations:
        - {key: kwok.x-k8s.io/node, operator: Exists, effect: NoSchedule}
      automountServiceAccountToken: false
      enableServiceLinks: false
      containers:
        - name: app
          image: registry.k8s.io/pause:3.10
          resources:
            requests: {cpu: 10m, memory: 16Mi}
            limits: {cpu: 50m, memory: 32Mi}
EOF
}

# ---------- helpers ----------

ensure_kwok() {
  if k -n kube-system get deploy kwok-controller >/dev/null 2>&1 \
    && k get crd stages.kwok.x-k8s.io >/dev/null 2>&1 \
    && [[ "$(k get stages.kwok.x-k8s.io --no-headers 2>/dev/null | grep -c .)" -gt 0 ]]; then
    info "kwok controller already installed"
  else
    local ctrl stages
    ctrl="$(fetch_pinned "$FG_KWOK_URL" "$FG_KWOK_SHA256")"
    stages="$(fetch_pinned "$FG_KWOK_STAGES_URL" "$FG_KWOK_STAGES_SHA256")"
    log "installing kwok controller $FG_KWOK_VERSION (kube-system/kwok-controller)"
    preload_image "registry.k8s.io/kwok/kwok:$FG_KWOK_VERSION"
    preload_image "registry.k8s.io/pause:3.10"
    k apply --server-side --force-conflicts -f "$ctrl" >/dev/null
    k wait --for condition=established --timeout=60s crd/stages.kwok.x-k8s.io >/dev/null
    k apply --server-side --force-conflicts -f "$stages" >/dev/null
  fi
  k -n kube-system rollout status deploy/kwok-controller --timeout=300s >/dev/null \
    || {
      k -n kube-system describe pods -l app=kwok-controller || true
      k -n kube-system get events --field-selector involvedObject.kind=Pod --sort-by=.lastTimestamp | tail -n 20 || true
      die "kwok-controller rollout failed"
    }
}

# Images present in the kind node's containerd (all nodes of our cluster).
node_has_image() {
  local img="$1" node
  for node in $(kind get nodes --name "$FG_CLUSTER_NAME"); do
    docker exec "$node" crictl inspecti "$img" >/dev/null 2>&1 || return 1
  done
}

# Make an image available inside the kind nodes so the kubelet never has to pull it.
# With Docker's containerd image store, `kind load docker-image` fails on multi-platform images
# ("content digest ... not found": only one platform's layers are local), so export a
# single-platform archive and load that. Fallback: pull inside each node with crictl.
# Retries cover transient registry errors; fails loudly instead of an ImagePullBackOff later.
preload_image() {
  local img="$1" platform="linux/amd64" attempt node tar
  case "$(docker info --format '{{.Architecture}}' 2>/dev/null)" in aarch64 | arm64) platform="linux/arm64" ;; esac
  node_has_image "$img" && { info "image present in node: $img"; return 0; }
  mkdir -p "$FG_CACHE_DIR"
  tar="$FG_CACHE_DIR/preload-image.tar"
  for attempt in 1 2 3; do
    if docker pull --quiet --platform "$platform" "$img" >/dev/null \
      && docker save --platform "$platform" -o "$tar" "$img" \
      && kind load image-archive "$tar" --name "$FG_CLUSTER_NAME" >/dev/null \
      && node_has_image "$img"; then
      rm -f "$tar"
      info "image loaded via kind: $img"
      return 0
    fi
    rm -f "$tar"
    warn "kind load of $img failed (attempt $attempt/3); retrying"
    sleep $((attempt * 5))
  done
  for attempt in 1 2 3; do
    for node in $(kind get nodes --name "$FG_CLUSTER_NAME"); do
      docker exec "$node" crictl pull "$img" >/dev/null || true
    done
    node_has_image "$img" && { info "image pulled inside node: $img"; return 0; }
    warn "in-node pull of $img failed (attempt $attempt/3); retrying"
    sleep $((attempt * 10))
  done
  die "could not make $img available in the kind node(s) (check registry access from the runner)"
}

# Sum of ready replicas over the scale Deployments.
ready_pods() {
  k get deploy -A -l "$SCALE_LABEL=true" -o jsonpath='{range .items[*]}{.status.readyReplicas}{"\n"}{end}' \
    | awk '{s += $1} END {print s + 0}'
}

wait_ready() {
  local want="$1" timeout="$2" start=$SECONDS have=-1 last=-1
  while :; do
    have="$(ready_pods)"
    if [[ "$have" != "$last" ]]; then info "ready $have/$want ($(fmt_secs $((SECONDS - start))))"; last="$have"; fi
    ((have >= want)) && return 0
    ((SECONDS - start > timeout)) && die "timed out after ${timeout}s: $have/$want scale pods ready"
    sleep 3
  done
}

# Delete fake nodes cleanly. Pods still bound to a deleted node are no longer finalized by kwok and
# hang in Terminating until PodGC (minutes). Pods are force-deleted AFTER the node is gone, so
# DaemonSets (kindnet, kube-proxy also land on fake nodes) do not recreate them. Fake pods have
# no containers, so force deletion loses nothing.
remove_fake_nodes() {
  local n
  log "removing $# fake node(s)"
  for n in "$@"; do
    [[ "$(k get node "$n" -o jsonpath='{.metadata.annotations.kwok\.x-k8s\.io/node}')" == fake ]] \
      || die "internal: refusing to remove non-kwok node $n"
  done
  k cordon "$@" >/dev/null
  k delete node "$@" --wait=true --timeout=120s >/dev/null
  for n in "$@"; do
    k delete pods -A --field-selector "spec.nodeName=$n" --grace-period=0 --force --wait=false >/dev/null 2>&1 || true
  done
}

# Fast removal of scale namespaces. Graceful deletion of thousands of fake pods is throttled by
# kwok (~10-15 pods/s), so: orphan-delete the Deployments/ReplicaSets (nothing recreates pods),
# force-delete the fake pods, then delete the (now empty) namespaces. Only namespaces carrying
# the scale label are accepted.
drop_scale_namespaces() {
  local ns
  for ns in "$@"; do
    [[ "$(k get ns "$ns" -o jsonpath="{.metadata.labels.fleetglass\.dev/scale}" 2>/dev/null)" == true ]] \
      || die "internal: refusing to drop namespace $ns without $SCALE_LABEL=true"
  done
  for ns in "$@"; do
    k -n "$ns" delete deploy,rs --all --cascade=orphan --wait=false >/dev/null 2>&1 || true
    k -n "$ns" delete pods --all --grace-period=0 --force --wait=false >/dev/null 2>&1 || true
  done
  k delete ns "$@" --wait=false >/dev/null 2>&1 || true
}

# ---------- commands ----------

cmd_up() {
  local pods="" nss="" nodes="" batch=1000 timeout=300 force=0
  while (($#)); do
    case "$1" in
      --pods) pods="${2:-}"; shift 2 ;;
      --namespaces) nss="${2:-}"; shift 2 ;;
      --nodes) nodes="${2:-}"; shift 2 ;;
      --batch) batch="${2:-}"; shift 2 ;;
      --timeout) timeout="${2:-}"; shift 2 ;;
      --force-large) force=1; shift ;;
      -h | --help) usage; exit 0 ;;
      *) die "unknown option '$1' (see --help)" ;;
    esac
  done
  local v
  for v in pods batch timeout; do
    [[ "${!v}" =~ ^[0-9]+$ ]] || die "--$v must be a non-negative integer (got '${!v}')"
  done
  if [[ -n "$nss" ]] && ! { [[ "$nss" =~ ^[0-9]+$ ]] && ((nss >= 1 && nss <= 1000)); }; then
    die "--namespaces must be 1..1000"
  fi
  if [[ -n "$nodes" ]] && ! { [[ "$nodes" =~ ^[0-9]+$ ]] && ((nodes >= 1 && nodes <= 1000)); }; then
    die "--nodes must be 1..1000"
  fi
  ((batch >= 1)) || die "--batch must be >= 1"
  if ((pods > FG_MAX_SCALE_PODS)); then
    if ((force)); then
      warn "--force-large: running $pods pods above FG_MAX_SCALE_PODS=$FG_MAX_SCALE_PODS"
    else
      die "refusing --pods $pods: the cap is FG_MAX_SCALE_PODS=$FG_MAX_SCALE_PODS on this docker VM (4 CPU / 7 GB).
        Larger runs need a bigger host (user decision); there, raise FG_MAX_SCALE_PODS or pass --force-large."
    fi
  fi
  [[ -n "$nss" ]] || { nss=$((pods / 250)); ((nss < 1)) && nss=1; ((nss > 100)) && nss=100; }
  [[ -n "$nodes" ]] || { nodes=$((pods / 500)); ((nodes < 2)) && nodes=2; }
  ((nodes * PODS_PER_FAKE_NODE >= pods)) \
    || die "--nodes $nodes holds at most $((nodes * PODS_PER_FAKE_NODE)) pods; use --nodes >= $(((pods + PODS_PER_FAKE_NODE - 1) / PODS_PER_FAKE_NODE))"

  need kind kubectl docker curl sha256sum awk
  fg_kubeconfig_guard
  fg_docker_guard
  fg_cluster_exists || die "kind cluster $FG_CLUSTER_NAME does not exist; run scripts/dev-cluster.sh up first"
  fg_context_guard

  local t0=$SECONDS
  log "scale up: $pods fake pods, $nss namespaces, $nodes fake nodes (batch $batch)"
  ensure_kwok

  # Fake nodes: create the desired set now; surplus ones are cordoned now and removed at the end
  # (after their pods are gone), see remove_fake_nodes.
  gen_nodes "$nodes" | k apply --server-side --force-conflicts -f - >/dev/null
  local n idx surplus_nodes=()
  while read -r n; do
    idx="${n#"$NODE_PREFIX"}"
    [[ "$idx" =~ ^[0-9]+$ ]] && ((10#$idx >= nodes)) && surplus_nodes+=("$n")
  done < <(k get nodes -l type=kwok -o name | sed 's|^node/||')
  ((${#surplus_nodes[@]})) && k cordon "${surplus_nodes[@]}" >/dev/null
  k wait --for=condition=Ready node -l type=kwok --timeout=120s >/dev/null
  info "$nodes fake nodes Ready"

  # Namespaces: create desired, delete surplus (their pods go with them).
  gen_namespaces "$nss" | k apply --server-side -f - >/dev/null
  local surplus=()
  while read -r n; do
    idx="${n#"$NS_PREFIX"}"
    [[ "$idx" =~ ^[0-9]+$ ]] && ((10#$idx >= nss)) && surplus+=("$n")
  done < <(k get ns -l "$SCALE_LABEL=true" -o name | sed 's|^namespace/||')
  if ((${#surplus[@]})); then
    info "deleting ${#surplus[@]} surplus namespace(s)"
    drop_scale_namespaces "${surplus[@]}"
  fi

  # Deployments: desired set from plan(); delete any not in it, then grow in batches.
  local desired current
  desired="$(plan "$pods" "$nss")"
  current="$(k get deploy -A -l "$SCALE_LABEL=true" --no-headers -o custom-columns=NS:.metadata.namespace,N:.metadata.name,R:.spec.replicas 2>/dev/null || true)"
  local ns name r
  while read -r ns name r; do
    [[ -n "$ns" ]] || continue
    printf '%s\n' "${surplus[@]:-}" | grep -qx "$ns" && continue
    grep -q "^$ns $name " <<<"$desired" || k -n "$ns" delete deploy "$name" --wait=false >/dev/null
  done <<<"$current"

  # Converge replicas. Shrinks go in the first step; growth is applied in steps of <= batch new
  # pods, each followed by a wait until that many more pods are ready (keeps etcd/apiserver load
  # on the small VM bounded). target = pods expected ready once the step has settled.
  local -A cur_r=()
  while read -r ns name r; do [[ -n "$ns" ]] && cur_r["$ns/$name"]="$r"; done <<<"$current"
  local step_file="$FG_DEV_DIR/scale-step.yaml" added=0 target=0 step=0 c
  : >"$step_file"
  while read -r ns name r; do
    c="${cur_r["$ns/$name"]:-0}"
    target=$((target + (r < c ? r : c)))
    ((r < c)) && gen_deployment "$ns" "$name" "$r" >>"$step_file"
  done <<<"$desired"
  while read -r ns name r; do
    c="${cur_r["$ns/$name"]:-0}"
    ((r > c)) || continue
    gen_deployment "$ns" "$name" "$r" >>"$step_file"
    added=$((added + r - c)) target=$((target + r - c))
    if ((added >= batch)); then
      step=$((step + 1)); info "step $step: +$added pods (target $target)"
      k apply --server-side --force-conflicts -f "$step_file" >/dev/null
      : >"$step_file"; added=0
      wait_ready "$target" "$timeout"
    fi
  done <<<"$desired"
  if [[ -s "$step_file" ]]; then
    step=$((step + 1)); info "step $step: +$added pods (target $target)"
    k apply --server-side --force-conflicts -f "$step_file" >/dev/null
  fi
  rm -f "$step_file"
  wait_ready "$pods" "$timeout"

  # Wait for surplus namespaces to finish terminating so the pod count is exact.
  # (kwok finalizes their pods; this must happen while the fake nodes still exist.)
  if ((${#surplus[@]})); then
    info "waiting for ${#surplus[@]} surplus namespace(s) to terminate"
    k wait --for=delete ns "${surplus[@]}" --timeout="${timeout}s" >/dev/null 2>&1 || warn "surplus namespaces still terminating"
  fi
  if ((${#surplus_nodes[@]})); then
    remove_fake_nodes "${surplus_nodes[@]}"
    wait_ready "$pods" "$timeout" # pods evicted from removed nodes are recreated elsewhere
  fi
  # Deleted/scaled-down pods may still be terminating; wait until the pod list is exact.
  local have_pods start=$SECONDS
  while :; do
    have_pods="$(k get pods -A -l "$SCALE_LABEL=true" --no-headers 2>/dev/null | grep -c . || true)"
    ((have_pods == pods)) && break
    ((SECONDS - start > timeout)) && { warn "scale pods listed: $have_pods, expected $pods"; break; }
    sleep 3
  done
  local total
  total="$(k get pods -A --no-headers | grep -c . || true)"
  log "done in $(fmt_secs $((SECONDS - t0))): $have_pods fake pods ($nss ns, $nodes fake nodes); kubectl get pods -A: $total"
}

cmd_down() {
  need kind kubectl docker
  fg_kubeconfig_guard
  fg_docker_guard
  fg_cluster_exists || { log "kind cluster $FG_CLUSTER_NAME does not exist; nothing to do"; return 0; }
  fg_context_guard
  local t0=$SECONDS nss
  mapfile -t nss < <(k get ns -l "$SCALE_LABEL=true" -o name | sed 's|^namespace/||')
  if ((${#nss[@]})); then
    log "deleting ${#nss[@]} scale namespace(s) and their fake pods"
    drop_scale_namespaces "${nss[@]}"
    k wait --for=delete ns "${nss[@]}" --timeout=600s >/dev/null 2>&1 || warn "scale namespaces still terminating"
  fi
  local fake_nodes
  mapfile -t fake_nodes < <(k get nodes -l type=kwok -o name | sed 's|^node/||')
  ((${#fake_nodes[@]})) && remove_fake_nodes "${fake_nodes[@]}"
  if k -n kube-system get deploy kwok-controller >/dev/null 2>&1; then
    log "removing kwok controller"
    local ctrl stages
    ctrl="$(fetch_pinned "$FG_KWOK_URL" "$FG_KWOK_SHA256")"
    stages="$(fetch_pinned "$FG_KWOK_STAGES_URL" "$FG_KWOK_STAGES_SHA256")"
    k delete -f "$stages" --ignore-not-found --wait=true >/dev/null 2>&1 || true
    k delete -f "$ctrl" --ignore-not-found --wait=true >/dev/null
  fi
  log "scale resources removed in $(fmt_secs $((SECONDS - t0))); pods left: $(k get pods -A --no-headers | grep -c . || true)"
}

cmd_status() {
  need kind kubectl docker
  fg_kubeconfig_guard
  fg_docker_guard
  fg_cluster_exists || { echo "cluster $FG_CLUSTER_NAME does not exist"; return 0; }
  fg_context_guard
  local kwok
  kwok="$(k -n kube-system get deploy kwok-controller -o jsonpath='{.status.readyReplicas}/{.spec.replicas}' 2>/dev/null || echo 'not installed')"
  echo "kwok controller: $kwok"
  echo "fake nodes:      $(k get nodes -l type=kwok --no-headers 2>/dev/null | grep -c . || true)"
  echo "scale ns:        $(k get ns -l "$SCALE_LABEL=true" --no-headers 2>/dev/null | grep -c . || true)"
  echo "fake pods:       $(k get pods -A -l "$SCALE_LABEL=true" --no-headers 2>/dev/null | grep -c . || true) (ready via deployments: $(ready_pods))"
  echo "all pods:        $(k get pods -A --no-headers 2>/dev/null | grep -c . || true)"
}

sub="${1:-}"; shift || true
case "$sub" in
  up) cmd_up "$@" ;;
  down) cmd_down ;;
  status) cmd_status ;;
  -h | --help | help) usage ;;
  *) usage >&2; exit 2 ;;
esac
