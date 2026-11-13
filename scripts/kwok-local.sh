#!/usr/bin/env bash
# Fleetglass LOCAL scale clusters: kwokctl "binary" runtime on THIS machine (no Docker, no sudo).
#
#   scripts/kwok-local.sh up [--clusters C] [--pods-per-cluster N] [--nodes K] [--namespaces M]
#        create kwok clusters fleetglass-scale-1..C (kube-apiserver on 127.0.0.1:17443+i), each
#        with K fake nodes and N realistic fake pods (Deployments in namespaces scale-ns-000..),
#        and write the merged kubeconfig .dev/kwok/kubeconfig (contexts kwok-fleetglass-scale-i)
#   scripts/kwok-local.sh down     delete every fleetglass-scale-* kwok cluster (nothing else)
#   scripts/kwok-local.sh status   clusters, API ports, nodes and pods per cluster
#   scripts/kwok-local.sh --help
#
# Everything lives under .dev/kwok (KWOK_WORKDIR): binaries cache, cluster dirs, kubeconfigs.
# ~/.kube/config and .dev/kubeconfig (the VM kind cluster) are never read or written.
# Total pods (C * N) are capped at $FG_LOCAL_MAX_SCALE_PODS (default 60000).
# Note: kwokctl's binary runtime starts plain child processes; they stop when their parent
# process group / PID namespace goes away (e.g. a sandboxed shell), so keep the shell alive.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export KWOK_WORKDIR="$ROOT/.dev/kwok"
KWOK_KUBECONFIG="$KWOK_WORKDIR/kubeconfig"
export KUBECONFIG="$KWOK_KUBECONFIG"

FG_LOCAL_MAX_SCALE_PODS="${FG_LOCAL_MAX_SCALE_PODS:-60000}"
NAME_PREFIX="fleetglass-scale-"
BASE_PORT=17443           # cluster i listens on BASE_PORT + i (17444, 17445, ...)
MAX_CLUSTERS=20
SCALE_LABEL="fleetglass.dev/scale"
NS_PREFIX="scale-ns-"
NODE_PREFIX="kwok-node-"
REPLICAS_PER_DEPLOY=50
PODS_PER_FAKE_NODE=1000

if [[ -t 1 ]]; then C_OK=$'\e[32m' C_WARN=$'\e[33m' C_ERR=$'\e[31m' C_DIM=$'\e[2m' C_OFF=$'\e[0m'
else C_OK='' C_WARN='' C_ERR='' C_DIM='' C_OFF=''; fi
log()  { printf '%s==>%s %s\n' "$C_OK" "$C_OFF" "$*"; }
info() { printf '    %s%s%s\n' "$C_DIM" "$*" "$C_OFF"; }
warn() { printf '%s[warn]%s %s\n' "$C_WARN" "$C_OFF" "$*" >&2; }
die()  { printf '%s[error]%s %s\n' "$C_ERR" "$C_OFF" "$*" >&2; exit 1; }

usage() {
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
  cat <<EOF

Options for up:
  --clusters C          number of kwok clusters (default 3, at most $MAX_CLUSTERS)
  --pods-per-cluster N  fake pods per cluster (default 16667)
  --nodes K             fake nodes per cluster (default max(2, ceil(N/$PODS_PER_FAKE_NODE)))
  --namespaces M        scale namespaces per cluster (default max(1, N/250), at most 100)
  --timeout SECS        max wait for pods to be Running per cluster (default 600)
  --hold                stay in the foreground after up (for sandboxes that kill orphaned
                        processes when the calling shell exits)
EOF
}

need() {
  local c
  for c in "$@"; do command -v "$c" >/dev/null 2>&1 || die "'$c' not found in PATH"; done
}

# KUBECONFIG must be our single file inside .dev/kwok; never ~/.kube/*, never a ':' list.
kubeconfig_guard() {
  local resolved kube_dir
  [[ "$KUBECONFIG" != *:* ]] || die "refusing: KUBECONFIG is a ':' list"
  resolved="$(realpath -m -- "$KUBECONFIG")"
  kube_dir="$(realpath -m -- "$HOME/.kube")"
  [[ "$resolved" == "$(realpath -m -- "$KWOK_WORKDIR")"/* ]] || die "refusing: KUBECONFIG '$resolved' is outside $KWOK_WORKDIR"
  [[ "$resolved" != "$kube_dir" && "$resolved" != "$kube_dir"/* ]] || die "refusing: KUBECONFIG resolves under ~/.kube"
  if [[ -e "$KUBECONFIG" && -e "$HOME/.kube/config" && "$KUBECONFIG" -ef "$HOME/.kube/config" ]]; then
    die "refusing: $KUBECONFIG is the same file as ~/.kube/config"
  fi
}

cluster_name() { printf '%s%d' "$NAME_PREFIX" "$1"; }
cluster_port() { printf '%d' $((BASE_PORT + $1)); }
cluster_kubeconfig() { printf '%s/clusters/%s/kubeconfig.yaml' "$KWOK_WORKDIR" "$1"; }

# kubectl pinned to one cluster's own kubeconfig file and context.
kc() {
  local name="$1"; shift
  kubectl --kubeconfig "$(cluster_kubeconfig "$name")" --context "kwok-$name" "$@"
}

# All kwokctl calls: explicit workdir and explicit kubeconfig (kwokctl defaults to $KUBECONFIG
# and would otherwise edit whatever file that points to).
# (--kubeconfig is a flag of create/delete cluster only, so it is passed there as well.)
kwokctl_() { KWOK_WORKDIR="$KWOK_WORKDIR" KUBECONFIG="$KWOK_KUBECONFIG" kwokctl "$@"; }

our_clusters() { kwokctl_ get clusters 2>/dev/null | grep -E "^${NAME_PREFIX}[0-9]+$" || true; }

# Regenerates .dev/kwok/kubeconfig from the per-cluster files (flattened, no ':' list exported).
write_merged_kubeconfig() {
  local files=() n f
  while read -r n; do
    [[ -n "$n" ]] || continue
    f="$(cluster_kubeconfig "$n")"
    [[ -f "$f" ]] && files+=("$f")
  done < <(our_clusters | sort -t- -k3,3n)
  if ((${#files[@]} == 0)); then
    rm -f "$KWOK_KUBECONFIG"
    return 0
  fi
  local joined
  joined="$(IFS=:; printf '%s' "${files[*]}")"
  KUBECONFIG="$joined" kubectl config view --flatten >"$KWOK_KUBECONFIG.tmp"
  mv "$KWOK_KUBECONFIG.tmp" "$KWOK_KUBECONFIG"
  chmod 600 "$KWOK_KUBECONFIG"
}

# ---------- manifests ----------

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
  allocatable: {cpu: "256", memory: 1Ti, pods: "$PODS_PER_FAKE_NODE"}
  capacity: {cpu: "256", memory: 1Ti, pods: "$PODS_PER_FAKE_NODE"}
EOF
  done
}

gen_namespaces() {
  local m="$1" i ns
  for ((i = 0; i < m; i++)); do
    printf -v ns '%s%03d' "$NS_PREFIX" "$i"
    printf -- '---\napiVersion: v1\nkind: Namespace\nmetadata:\n  name: %s\n  labels: {%s: "true", team: team-%d}\n' \
      "$ns" "$SCALE_LABEL" $((i % 7))
  done
}

# Prints "ns deploy replicas" lines for N pods over M namespaces (same plan as scale-cluster.sh).
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

# A realistic workload pod: 3 containers + 1 init container, env, ports, probes, resources,
# volumes, several labels and annotations. kwok fills in status (conditions, IPs, container
# statuses), and the API server adds managedFields and the service-account token volume.
gen_deployment() {
  local ns="$1" name="$2" replicas="$3" tier
  case $(( ${name#app-} % 3 )) in 0) tier=web ;; 1) tier=api ;; *) tier=worker ;; esac
  cat <<EOF
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: $name
  namespace: $ns
  labels: {app: $name, tier: $tier, $SCALE_LABEL: "true"}
spec:
  replicas: $replicas
  revisionHistoryLimit: 1
  selector:
    matchLabels: {app: $name}
  template:
    metadata:
      labels:
        app: $name
        tier: $tier
        version: v1.4.2
        team: platform
        app.kubernetes.io/name: $name
        app.kubernetes.io/part-of: fleetglass-scale
        app.kubernetes.io/managed-by: kwok-local
        $SCALE_LABEL: "true"
      annotations:
        prometheus.io/scrape: "true"
        prometheus.io/port: "9090"
        fleetglass.dev/owner: "scale-test@example.invalid"
    spec:
      nodeSelector: {type: kwok}
      tolerations:
        - {key: kwok.x-k8s.io/node, operator: Exists, effect: NoSchedule}
      enableServiceLinks: false
      terminationGracePeriodSeconds: 30
      initContainers:
        - name: init-config
          image: registry.k8s.io/busybox:1.36
          command: ["sh", "-c", "cp /defaults/* /config/"]
          volumeMounts: [{name: config, mountPath: /config}]
      containers:
        - name: app
          image: ghcr.io/fleetglass/scale-$tier:1.4.2
          args: ["--port=8080", "--metrics-port=9090", "--log-level=info"]
          ports:
            - {name: http, containerPort: 8080, protocol: TCP}
            - {name: metrics, containerPort: 9090, protocol: TCP}
          env:
            - {name: APP_NAME, value: $name}
            - {name: APP_ENV, value: production}
            - {name: LOG_FORMAT, value: json}
            - {name: DB_HOST, value: postgres.$ns.svc.cluster.local}
            - {name: CACHE_URL, value: "redis://redis.$ns.svc.cluster.local:6379/0"}
            - name: POD_NAME
              valueFrom: {fieldRef: {fieldPath: metadata.name}}
            - name: POD_IP
              valueFrom: {fieldRef: {fieldPath: status.podIP}}
          resources:
            requests: {cpu: 100m, memory: 128Mi}
            limits: {cpu: 500m, memory: 512Mi}
          readinessProbe:
            httpGet: {path: /healthz/ready, port: http}
            periodSeconds: 10
          livenessProbe:
            httpGet: {path: /healthz/live, port: http}
            initialDelaySeconds: 15
            periodSeconds: 20
          volumeMounts:
            - {name: config, mountPath: /etc/app, readOnly: true}
            - {name: tmp, mountPath: /tmp}
        - name: proxy
          image: ghcr.io/fleetglass/envoy-proxy:1.31.0
          ports: [{name: proxy, containerPort: 15001}]
          env:
            - {name: PROXY_UPSTREAM, value: "127.0.0.1:8080"}
            - {name: PROXY_LOG_LEVEL, value: warn}
          resources:
            requests: {cpu: 50m, memory: 64Mi}
            limits: {cpu: 200m, memory: 128Mi}
        - name: log-shipper
          image: ghcr.io/fleetglass/log-shipper:2.2.1
          env:
            - {name: SHIP_TO, value: "loki.monitoring.svc.cluster.local:3100"}
            - {name: SHIP_LABELS, value: "app=$name,ns=$ns"}
          resources:
            requests: {cpu: 10m, memory: 32Mi}
            limits: {cpu: 100m, memory: 64Mi}
          volumeMounts: [{name: tmp, mountPath: /var/log/app, readOnly: true}]
      volumes:
        - {name: config, emptyDir: {}}
        - {name: tmp, emptyDir: {sizeLimit: 256Mi}}
EOF
}

# ---------- commands ----------

running_pods() {
  kc "$1" get pods -A -l "$SCALE_LABEL=true" --field-selector status.phase=Running --no-headers 2>/dev/null | grep -c . || true
}

wait_running() {
  local name="$1" want="$2" timeout="$3" start=$SECONDS have=-1 last=-1
  while :; do
    have="$(running_pods "$name")"
    if [[ "$have" != "$last" ]]; then info "$name: running $have/$want ($((SECONDS - start))s)"; last="$have"; fi
    ((have >= want)) && return 0
    ((SECONDS - start > timeout)) && die "$name: timed out after ${timeout}s: $have/$want pods Running"
    sleep 3
  done
}

populate_cluster() {
  local name="$1" pods="$2" nodes="$3" nss="$4" timeout="$5"
  kc "$name" wait --for=create sa/default -n default --timeout=60s >/dev/null 2>&1 || true
  gen_nodes "$nodes" | kc "$name" apply --server-side -f - >/dev/null
  kc "$name" wait --for=condition=Ready node -l type=kwok --timeout=120s >/dev/null
  gen_namespaces "$nss" | kc "$name" apply --server-side -f - >/dev/null
  local manifest="$KWOK_WORKDIR/clusters/$name/scale-workloads.yaml" ns dep r
  : >"$manifest"
  while read -r ns dep r; do gen_deployment "$ns" "$dep" "$r" >>"$manifest"; done < <(plan "$pods" "$nss")
  kc "$name" apply --server-side --force-conflicts -f "$manifest" >/dev/null
}

cmd_up() {
  local clusters=3 pods=16667 nodes="" nss="" timeout=600 hold=0
  while (($#)); do
    case "$1" in
      --clusters) clusters="${2:-}"; shift 2 ;;
      --pods-per-cluster) pods="${2:-}"; shift 2 ;;
      --nodes) nodes="${2:-}"; shift 2 ;;
      --namespaces) nss="${2:-}"; shift 2 ;;
      --timeout) timeout="${2:-}"; shift 2 ;;
      --hold) hold=1; shift ;;
      -h | --help) usage; exit 0 ;;
      *) die "unknown option '$1' (see --help)" ;;
    esac
  done
  local v
  for v in clusters pods timeout FG_LOCAL_MAX_SCALE_PODS; do
    [[ "${!v}" =~ ^[0-9]+$ ]] || die "$v must be a non-negative integer (got '${!v}')"
  done
  ((clusters >= 1 && clusters <= MAX_CLUSTERS)) || die "--clusters must be 1..$MAX_CLUSTERS"
  ((pods >= 1)) || die "--pods-per-cluster must be >= 1"
  local total=$((clusters * pods))
  ((total <= FG_LOCAL_MAX_SCALE_PODS)) \
    || die "refusing: $clusters x $pods = $total pods exceeds FG_LOCAL_MAX_SCALE_PODS=$FG_LOCAL_MAX_SCALE_PODS"
  [[ "$nss" =~ ^[0-9]*$ && "$nodes" =~ ^[0-9]*$ ]] || die "--nodes/--namespaces must be integers"
  [[ -n "$nss" ]] || { nss=$((pods / 250)); ((nss < 1)) && nss=1; ((nss > 100)) && nss=100; }
  [[ -n "$nodes" ]] || { nodes=$(((pods + PODS_PER_FAKE_NODE - 1) / PODS_PER_FAKE_NODE)); ((nodes < 2)) && nodes=2; }
  ((nss >= 1 && nodes >= 1)) || die "--nodes/--namespaces must be >= 1"
  ((nodes * PODS_PER_FAKE_NODE >= pods)) || die "--nodes $nodes holds at most $((nodes * PODS_PER_FAKE_NODE)) pods"

  need kwokctl kubectl awk realpath
  kubeconfig_guard
  mkdir -p "$KWOK_WORKDIR"
  local t0=$SECONDS i name port existing
  existing="$(our_clusters)"
  log "kwok local scale: $clusters cluster(s) x $pods pods = $total ($nodes fake nodes, $nss namespaces each)"
  for ((i = 1; i <= clusters; i++)); do
    name="$(cluster_name "$i")" port="$(cluster_port "$i")"
    ((port != 6443 && port != 7443)) || die "internal: refusing port $port"
    if grep -qx "$name" <<<"$existing"; then
      info "$name exists; starting it (re-run 'down' first for a clean cluster)"
      kwokctl_ start cluster --name "$name" >/dev/null 2>&1 || true
    else
      info "creating $name on 127.0.0.1:$port"
      kwokctl_ create cluster --name "$name" --runtime binary --kube-apiserver-port "$port" \
        --disable-qps-limits --wait 3m --kubeconfig "$KWOK_KUBECONFIG" >"$KWOK_WORKDIR/create-$name.log" 2>&1 \
        || { tail -20 "$KWOK_WORKDIR/create-$name.log" >&2; die "kwokctl create cluster $name failed"; }
    fi
  done
  write_merged_kubeconfig
  for ((i = 1; i <= clusters; i++)); do
    name="$(cluster_name "$i")"
    log "$name: populating $pods pods"
    populate_cluster "$name" "$pods" "$nodes" "$nss" "$timeout" &
  done
  local pid
  for pid in $(jobs -p); do wait "$pid" || die "populating a cluster failed"; done
  for ((i = 1; i <= clusters; i++)); do
    wait_running "$(cluster_name "$i")" "$pods" "$timeout"
  done
  log "done in $((SECONDS - t0))s; kubeconfig: $KWOK_KUBECONFIG"
  cmd_status
  if ((hold)); then
    log "--hold: keeping this process alive (clusters stay up); stop it, then run '$0 down'"
    while :; do sleep 3600; done
  fi
}

cmd_down() {
  need kwokctl
  kubeconfig_guard
  local name
  while read -r name; do
    [[ -n "$name" ]] || continue
    [[ "$name" =~ ^${NAME_PREFIX}[0-9]+$ ]] || die "internal: refusing to delete '$name'"
    log "deleting kwok cluster $name"
    kwokctl_ delete cluster --name "$name" --kubeconfig "$KWOK_KUBECONFIG" >/dev/null 2>&1 || warn "kwokctl delete $name failed"
  done < <(our_clusters)
  write_merged_kubeconfig
  rm -f "$KWOK_WORKDIR"/create-"$NAME_PREFIX"*.log
  log "remaining kwok clusters in $KWOK_WORKDIR: $(kwokctl_ get clusters 2>/dev/null | tr '\n' ' ')"
}

cmd_status() {
  need kwokctl kubectl
  kubeconfig_guard
  local name port nodes pods
  echo "KWOK_WORKDIR: $KWOK_WORKDIR"
  echo "kubeconfig:   $KWOK_KUBECONFIG"
  while read -r name; do
    [[ -n "$name" ]] || continue
    port="$(cluster_port "${name#"$NAME_PREFIX"}")"
    if kc "$name" get --raw /readyz >/dev/null 2>&1; then
      nodes="$(kc "$name" get nodes -l type=kwok --no-headers 2>/dev/null | grep -c . || true)"
      pods="$(kc "$name" get pods -A --no-headers 2>/dev/null | grep -c . || true)"
      echo "$name  127.0.0.1:$port  up    fake nodes: $nodes  pods: $pods (Running scale: $(running_pods "$name"))"
    else
      echo "$name  127.0.0.1:$port  down"
    fi
  done < <(our_clusters)
}

sub="${1:-}"; shift || true
case "$sub" in
  up) cmd_up "$@" ;;
  down) cmd_down ;;
  status) cmd_status ;;
  -h | --help | help) usage ;;
  *) usage >&2; exit 2 ;;
esac
