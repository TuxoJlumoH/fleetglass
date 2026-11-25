#!/usr/bin/env bash
# Fleetglass dev environment bootstrap.
#
#   scripts/bootstrap.sh --check     report every tool + version, exit 1 if anything required is missing
#   scripts/bootstrap.sh --install   install missing user-space tools into ~/.local/bin (no root),
#                                    and PRINT the root commands for system packages (never runs sudo)
#   scripts/bootstrap.sh --help
#
# This script never runs sudo and never creates, changes or deletes Docker resources.
set -euo pipefail

# Pinned versions used by --install (the versions the project is developed against).
KUBECTL_VERSION="${FG_KUBECTL_VERSION:-v1.37.2}"
KIND_VERSION="${FG_KIND_VERSION:-v0.33.0}"
KWOK_VERSION="${FG_KWOK_VERSION:-v0.8.0}"
PNPM_VERSION="${FG_PNPM_VERSION:-12.16.1}"
# Minimums checked by --check.
MIN_RUSTC="1.95.0" # edition 2024 + kube MSRV
MIN_NODE="20.19.0" # Vite 8 / React 19 toolchain
BIN_DIR="${FG_BIN_DIR:-$HOME/.local/bin}"
DOCKER_TIMEOUT="${FG_DOCKER_TIMEOUT:-15}"

if [[ -t 1 ]]; then
  C_OK=$'\e[32m' C_WARN=$'\e[33m' C_ERR=$'\e[31m' C_DIM=$'\e[2m' C_OFF=$'\e[0m'
else
  C_OK='' C_WARN='' C_ERR='' C_DIM='' C_OFF=''
fi

missing=0
warnings=0
root_cmds=()
user_installs=()

ok()   { printf '  %s[ok]%s   %-24s %s\n' "$C_OK" "$C_OFF" "$1" "$2"; }
warn() { printf '  %s[warn]%s %-24s %s\n' "$C_WARN" "$C_OFF" "$1" "$2"; warnings=$((warnings + 1)); }
miss() { printf '  %s[miss]%s %-24s %s\n' "$C_ERR" "$C_OFF" "$1" "$2"; missing=$((missing + 1)); }
section() { printf '\n%s\n' "$1"; }

usage() {
  sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
  cat <<EOF

Environment overrides:
  FG_BIN_DIR          install dir for user-space tools (default: ~/.local/bin)
  FG_KUBECTL_VERSION  default ${KUBECTL_VERSION}
  FG_KIND_VERSION     default ${KIND_VERSION}
  FG_KWOK_VERSION     default ${KWOK_VERSION}  (kwok + kwokctl)
  FG_PNPM_VERSION     default ${PNPM_VERSION}
  FG_DOCKER_TIMEOUT   seconds to wait for the docker server (default ${DOCKER_TIMEOUT})

Platform notes (system packages need root; run them yourself):
  Arch/CachyOS:  sudo pacman -S --needed webkit2gtk-4.1 libayatana-appindicator librsvg \\
                   base-devel curl wget file openssl nodejs npm docker
                 rustup: sudo pacman -S --needed rustup && rustup default stable
                 (AUR alternatives: paru -S kind-bin kwok-bin)
  Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \\
                   build-essential curl wget file libssl-dev libxdo-dev docker-ce-cli
                 Node >= 20 from NodeSource or nvm; Rust via https://rustup.rs
  macOS:         xcode-select --install
                 brew install node pnpm kubectl kind kwok docker   (Rust via https://rustup.rs)
                 Tauri uses the system WebKit; no extra libraries needed.
EOF
}

# version_ge A B: true if A >= B (dotted versions).
version_ge() { [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" == "$2" ]]; }

os_id() {
  if [[ "$(uname -s)" == "Darwin" ]]; then echo macos; return; fi
  if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    case " ${ID:-} ${ID_LIKE:-} " in
      *" arch "*|*" cachyos "*) echo arch; return ;;
      *" debian "*|*" ubuntu "*) echo debian; return ;;
    esac
  fi
  echo unknown
}
OS="$(os_id)"

arch_go() {
  case "$(uname -m)" in
    x86_64|amd64) echo amd64 ;;
    aarch64|arm64) echo arm64 ;;
    *) uname -m ;;
  esac
}

# check_tool NAME VERSION_CMD... : prints first line of version output.
check_tool() {
  local name="$1"; shift
  if ! command -v "$1" >/dev/null 2>&1; then
    return 1
  fi
  local v
  v="$("$@" 2>/dev/null | head -n1 || true)"
  ok "$name" "${v:-<version unknown>} ${C_DIM}($(command -v "$1"))${C_OFF}"
}

check_rust() {
  if ! command -v rustc >/dev/null 2>&1; then
    miss rustc "not found (rustup: https://rustup.rs)"
    miss cargo "not found (comes with rustup)"
    root_cmds+=("rustup")
    return
  fi
  local v; v="$(rustc --version | awk '{print $2}')"
  if version_ge "${v%%-*}" "$MIN_RUSTC"; then
    ok rustc "$(rustc --version) ${C_DIM}(>= $MIN_RUSTC)${C_OFF}"
  else
    miss rustc "$v is older than $MIN_RUSTC (run: rustup update stable)"
  fi
  check_tool cargo cargo --version || miss cargo "not found (install via rustup)"
}

check_node() {
  if ! command -v node >/dev/null 2>&1; then
    miss node "not found"; root_cmds+=("nodejs npm"); return
  fi
  local v; v="$(node --version)"; v="${v#v}"
  if version_ge "$v" "$MIN_NODE"; then
    ok node "v$v ${C_DIM}(>= $MIN_NODE)${C_OFF}"
  else
    miss node "v$v is older than $MIN_NODE"
  fi
}

check_cli_tools() {
  check_tool pnpm pnpm --version || { miss pnpm "not found"; user_installs+=(pnpm); }
  check_tool kubectl kubectl version --client=true || { miss kubectl "not found"; user_installs+=(kubectl); }
  check_tool kind kind version || { miss kind "not found"; user_installs+=(kind); }
  check_tool kwok kwok --version || { miss kwok "not found"; user_installs+=(kwok); }
  check_tool kwokctl kwokctl --version || { miss kwokctl "not found"; user_installs+=(kwokctl); }
}

# Tauri system libraries (Linux only). pkg-config name | pacman pkg | apt pkg
TAURI_LIBS=(
  "webkit2gtk-4.1|webkit2gtk-4.1|libwebkit2gtk-4.1-dev"
  "ayatana-appindicator3-0.1|libayatana-appindicator|libayatana-appindicator3-dev"
  "librsvg-2.0|librsvg|librsvg2-dev"
)

check_tauri_libs() {
  if [[ "$OS" == macos ]]; then
    ok "tauri system deps" "macOS uses system WebKit (needs Xcode CLT)"
    return
  fi
  local entry pc pac apt v
  for entry in "${TAURI_LIBS[@]}"; do
    IFS='|' read -r pc pac apt <<<"$entry"
    v=""
    if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists "$pc" 2>/dev/null; then
      v="$(pkg-config --modversion "$pc")"
      ok "$pac" "$v ${C_DIM}(pkg-config $pc)${C_OFF}"
    elif [[ "$OS" == arch ]] && pacman -Q "$pac" >/dev/null 2>&1; then
      ok "$pac" "$(pacman -Q "$pac" | awk '{print $2}') ${C_DIM}(pacman)${C_OFF}"
    else
      miss "$pac" "not found"
      if [[ "$OS" == debian ]]; then root_cmds+=("$apt"); else root_cmds+=("$pac"); fi
    fi
  done
  check_tool pkg-config pkg-config --version || warn pkg-config "not found (needed to build Tauri)"
}

check_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    miss docker "CLI not found"; root_cmds+=("docker"); return
  fi
  ok docker "$(docker --version) ${C_DIM}(client)${C_OFF}"
  local host="${DOCKER_HOST:-<local socket>}"
  # Read-only query; never creates or changes Docker resources.
  local sv
  if sv="$(timeout "$DOCKER_TIMEOUT" docker version --format '{{.Server.Version}}' 2>/dev/null)" && [[ -n "$sv" ]]; then
    ok "docker server" "$sv ${C_DIM}($host)${C_OFF}"
  else
    warn "docker server" "not reachable at $host within ${DOCKER_TIMEOUT}s (kind needs it; not fatal for --check)"
  fi
}

print_root_cmds() {
  [[ ${#root_cmds[@]} -eq 0 ]] && return 0
  section "System packages are missing. Run this yourself (needs root; this script never runs sudo):"
  case "$OS" in
    arch)
      printf '  sudo pacman -S --needed %s\n' "${root_cmds[*]}"
      [[ " ${root_cmds[*]} " == *" rustup "* ]] && printf '  rustup default stable\n'
      ;;
    debian)
      printf '  sudo apt update && sudo apt install %s\n' "${root_cmds[*]}"
      ;;
    macos)
      printf '  brew install %s\n' "${root_cmds[*]}"
      ;;
    *)
      printf '  install with your package manager: %s\n' "${root_cmds[*]}"
      ;;
  esac
}

download() { # url dest [sha256_url]
  local tmp sha256_url="${3:-}" expected actual
  tmp="$(mktemp)"
  curl -fsSL --retry 3 -o "$tmp" "$1"
  if [[ -n "$sha256_url" ]] && { command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1; }; then
    expected="$(curl -fsSL --retry 3 "$sha256_url" | grep -Eo '[0-9a-fA-F]{64}' | head -n1)"
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "$tmp" | awk '{print $1}')"
    else
      actual="$(shasum -a 256 "$tmp" | awk '{print $1}')"
    fi
    if [[ -z "$expected" || "${actual,,}" != "${expected,,}" ]]; then
      rm -f "$tmp"
      printf '  %sSHA-256 checksum mismatch for %s (expected %s, got %s)%s\n' "$C_ERR" "$1" "${expected:-<empty>}" "$actual" "$C_OFF" >&2
      exit 1
    fi
  fi
  install -m 0755 "$tmp" "$2"
  rm -f "$tmp"
}

install_user_tools() {
  local os_go a t
  os_go="$( [[ "$OS" == macos ]] && echo darwin || echo linux )"
  a="$(arch_go)"
  mkdir -p "$BIN_DIR"
  for t in "${user_installs[@]}"; do
    printf '  installing %s into %s\n' "$t" "$BIN_DIR"
    case "$t" in
      kubectl)
        download \
          "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/${os_go}/${a}/kubectl" \
          "$BIN_DIR/kubectl" \
          "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/${os_go}/${a}/kubectl.sha256" ;;
      kind)
        download \
          "https://kind.sigs.k8s.io/dl/${KIND_VERSION}/kind-${os_go}-${a}" \
          "$BIN_DIR/kind" \
          "https://kind.sigs.k8s.io/dl/${KIND_VERSION}/kind-${os_go}-${a}.sha256sum" ;;
      kwok|kwokctl)
        download \
          "https://github.com/kubernetes-sigs/kwok/releases/download/${KWOK_VERSION}/${t}-${os_go}-${a}" \
          "$BIN_DIR/$t" \
          "https://github.com/kubernetes-sigs/kwok/releases/download/${KWOK_VERSION}/${t}-${os_go}-${a}.sha256sum" ;;
      pnpm)
        if command -v npm >/dev/null 2>&1; then
          npm install -g --prefix "$HOME/.local" "pnpm@${PNPM_VERSION}"
        else
          printf '  %scannot install pnpm: npm not found (install node first)%s\n' "$C_ERR" "$C_OFF"
        fi ;;
    esac
  done
  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) printf '\n  %snote:%s add %s to PATH\n' "$C_WARN" "$C_OFF" "$BIN_DIR" ;;
  esac
}

run_checks() {
  printf 'Fleetglass bootstrap check (os: %s, arch: %s)\n' "$OS" "$(uname -m)"
  section "Rust"
  check_rust
  section "Node / UI"
  check_node
  section "Kubernetes tooling"
  check_cli_tools
  section "Tauri system dependencies"
  check_tauri_libs
  section "Docker (remote, read-only check)"
  check_docker
}

summary() {
  printf '\n'
  if [[ $missing -eq 0 ]]; then
    printf '%sAll required tools present%s (%d warning(s)).\n' "$C_OK" "$C_OFF" "$warnings"
  else
    printf '%s%d required tool(s) missing%s, %d warning(s). Run: scripts/bootstrap.sh --install\n' \
      "$C_ERR" "$missing" "$C_OFF" "$warnings"
  fi
}

main() {
  local mode="${1:---check}"
  case "$mode" in
    -h|--help) usage; exit 0 ;;
    --check)
      run_checks
      print_root_cmds
      summary
      [[ $missing -eq 0 ]]
      ;;
    --install)
      run_checks
      if [[ ${#user_installs[@]} -gt 0 ]]; then
        section "Installing user-space tools (no root)"
        install_user_tools
      fi
      print_root_cmds
      if [[ ${#user_installs[@]} -gt 0 || ${#root_cmds[@]} -gt 0 ]]; then
        printf '\nRe-run scripts/bootstrap.sh --check after installing.\n'
      else
        summary
      fi
      ;;
    *) printf 'unknown option: %s\n\n' "$mode" >&2; usage >&2; exit 2 ;;
  esac
}

main "$@"
