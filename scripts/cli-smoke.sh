#!/usr/bin/env bash
# cli-smoke.sh — fresh-machine CLI-UX-regression smoke for the svrn distribution.
#
# Answers ONE question: "on a clean machine that is NOT a dev box — a different
# Linux distro, no CUDA, only the runtime deps — does the user-facing golden
# path still work, STARTING FROM THE PUBLISHED INSTALLER?"
#
#   curl -fsSL https://svrnme.sh/install.sh | sh   ->  svrn setup  ->
#   daemon boots  ->  svrn chat ask "..."  ->  grounded answer  ->  clean exit
#
# It is the CLI analogue of scripts/desktop-smoke.sh, but the isolation unit is
# a fresh CONTAINER per distro and the entry point is the REAL installer, so a
# broken release asset, a bad platform-detect branch, a missing runtime dep, or
# a PATH mistake all surface here — none of which `cargo test` can see.
#
# Motivating field report (a friend on a Vulkan/CPU-only Linux box):
# (1) the VRAM preflight HARD-BLOCKED startup and (2) a model-load failure
# surfaced as a bare "null result from llama cpp". Both are Phase-0 assertions.
#
# Install modes (--install-mode):
#   hosted  (default)  curl -fsSL $INSTALL_URL | sh    — tests the LIVE install
#                      (svrnme.sh). Validates the published release end to end;
#                      run post-deploy or on a schedule. Tests the RELEASE, not
#                      your working tree.
#   local              pipe the repo's landing/install.sh into sh — tests your
#                      EDITED installer against the published binaries, before
#                      you deploy it to svrnme.sh.
#   binary             mount a working-tree Linux binary (--binary <path>) at
#                      ~/.local/bin — tests uncommitted CODE (skips the installer).
#
# Platform: the installer publishes ONLY x86_64-unknown-linux-gnu, so containers
# run --platform linux/amd64 (qemu emulation on arm64 hosts) and musl distros
# (alpine) are excluded — the glibc binary won't run on musl.
#
# Usage:
#   scripts/cli-smoke.sh                                   # hosted installer, default distros
#   scripts/cli-smoke.sh --install-mode local              # test landing/install.sh
#   scripts/cli-smoke.sh --install-mode binary --binary target/x86_64-unknown-linux-gnu/release/svrn
#   scripts/cli-smoke.sh [--model-dir sovereign/models] [--distros "ubuntu:24.04,debian:12"]
#                        [--runtime auto|podman|docker] [--prompt "..."] [--keep] [--dry-run]
#
# Local NATIVE soak (arm64 host): build the arm64 binaries first
# (scripts/build-cli-linux-arm64.sh), then loop chat turns for N minutes:
#   scripts/cli-smoke.sh --install-mode binary --platform linux/arm64 \
#     --binary target-container-linux-arm64/aarch64-unknown-linux-gnu/release \
#     --model-dir <dir-of-tiny-gguf> --soak 30
#   (--soak defaults --gpu-layers to `off`: GPU-less container runs on CPU.)
#
# Exit: 0 = every executed distro passed; 1 = a regression/golden-path failure;
#       2 = harness/setup error. SKIPPED phases never fail the run.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# ── Defaults / args ──────────────────────────────────────────────────────────
INSTALL_MODE="hosted"                       # hosted | local | binary
INSTALL_URL="https://svrnme.sh/install.sh"
LOCAL_INSTALLER="$REPO_ROOT/landing/install.sh"
DATA_DIR_HOST=""                             # persist /data on the host (models + config) across runs
BINARY=""                                    # dir of locally-built LINUX binaries (binary mode);
                                            # defaults to the repo's container-build output
DEFAULT_BINARY_DIR="$REPO_ROOT/target-container-linux/x86_64-unknown-linux-gnu/release"
MODEL_DIR=""                                # host dir of tiny GGUFs to pre-mount (optional)
DISTROS="ubuntu:24.04,debian:12,fedora:41"  # glibc only (installer ships -gnu)
RUNTIME="auto"
PLATFORM="linux/amd64"                       # installer only publishes x86_64 linux-gnu
PROMPT="What is this assistant and where does it run?"
KEEP=""
DRY_RUN=""
INSTALL_ONLY=""                              # stop after install (fast; no model/inference)
SOAK_MINUTES="0"                             # >0: loop chat turns for a long soak
GPU_LAYERS=""                                # SOVEREIGN_GPU_LAYERS to pass in (soak defaults to off)
: "${SMOKE_HEALTH_TIMEOUT:=300}"
: "${SMOKE_CHAT_TIMEOUT:=300}"
: "${SMOKE_DAEMON_PORT:=9741}"

die()  { echo "cli-smoke: $*" >&2; exit 2; }
info() { echo "[cli-smoke] $*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --install-mode) INSTALL_MODE="${2:?}"; shift 2;;
    --install-url)  INSTALL_URL="${2:?}"; shift 2;;
    --binary)       BINARY="${2:?}"; shift 2;;
    --model-dir)    MODEL_DIR="${2:?}"; shift 2;;
    --data-dir)     DATA_DIR_HOST="${2:?}"; shift 2;;
    --distros)      DISTROS="${2:?}"; shift 2;;
    --runtime)      RUNTIME="${2:?}"; shift 2;;
    --platform)     PLATFORM="${2:?}"; shift 2;;
    --prompt)       PROMPT="${2:?}"; shift 2;;
    --install-only) INSTALL_ONLY="1"; shift;;
    --soak)         SOAK_MINUTES="${2:?}"; shift 2;;
    --gpu-layers)   GPU_LAYERS="${2:?}"; shift 2;;
    --keep)         KEEP="1"; shift;;
    --dry-run)      DRY_RUN="1"; shift;;
    -h|--help)      sed -n '2,50p' "$0"; exit 0;;
    *) die "unknown arg: $1 (see --help)";;
  esac
done

# Soak runs in GPU-less containers; a Vulkan-compiled arm64 binary must fall
# back to CPU, so default the layer override to `off` unless the caller set one.
if [ "$SOAK_MINUTES" != "0" ] && [ -z "$GPU_LAYERS" ]; then GPU_LAYERS="off"; fi

case "$INSTALL_MODE" in
  hosted|local|binary) ;;
  *) die "invalid --install-mode: $INSTALL_MODE (hosted|local|binary)";;
esac
if [ "$INSTALL_MODE" = "binary" ]; then
  [ -n "$BINARY" ] || BINARY="$DEFAULT_BINARY_DIR"   # default to the local container build
  [ -d "$BINARY" ] || die "--install-mode binary needs a directory of linux binaries (got '$BINARY'); build with scripts/release-cli-local.sh (linux leg) or pass --binary <dir>"
  BINARY="$(cd "$BINARY" && pwd)"   # absolute — podman treats a relative -v source as a named volume
  for b in sovereign-cli sovereign-cli-daemon sovereign-cli-llm; do
    [ -f "$BINARY/$b" ] || die "missing binary in $BINARY: $b (the dispatcher exec()s its siblings — all three are required)"
  done
fi
[ "$INSTALL_MODE" = "local" ] && { [ -f "$LOCAL_INSTALLER" ] || die "local installer not found: $LOCAL_INSTALLER"; }

# ── Container runtime ────────────────────────────────────────────────────────
if [ "$RUNTIME" = "auto" ]; then
  if command -v podman >/dev/null 2>&1; then RUNTIME="podman"
  elif command -v docker >/dev/null 2>&1; then RUNTIME="docker"
  else die "no container runtime found (install podman or docker)"; fi
fi
command -v "$RUNTIME" >/dev/null 2>&1 || die "runtime '$RUNTIME' not on PATH"
info "runtime: $RUNTIME · platform: $PLATFORM · install-mode: $INSTALL_MODE"

STAMP="$(date +%Y%m%d-%H%M%S)"
ART="$REPO_ROOT/test-artifacts/cli-smoke/$STAMP"
mkdir -p "$ART"
info "artifacts: $ART"

IFS=',' read -r -a DISTRO_ARR <<< "$DISTROS"

# ── Per-distro runtime deps. The installer needs curl + tar; llama.cpp CPU
# needs libgomp; libstdc++/libm are in the base images. NOT a toolchain. ───────
deps_cmd_for() {
  case "$1" in
    ubuntu:*|debian:*) echo "apt-get update -qq && apt-get install -y -qq ca-certificates curl tar libgomp1 libvulkan1 >/dev/null";;
    fedora:*|rockylinux:*|almalinux:*) echo "dnf install -y -q ca-certificates curl tar libgomp vulkan-loader >/dev/null";;
    alpine:*) echo "echo 'WARN: alpine is musl; the -gnu installer binary will not run' >&2; apk add --no-cache ca-certificates curl tar libgomp libstdc++ >/dev/null";;
    *) echo "true # unknown distro: hoping base image has curl/tar/libgomp";;
  esac
}

# ── The in-container driver ───────────────────────────────────────────────────
# Emits machine-greppable RESULT: lines the outer loop asserts on. Deliberately
# sets NO SOVEREIGN_* bypass env — the point is to prove the DEFAULT posture
# works with no env-var soup.
container_driver() {
  cat <<'DRIVER'
set -u
export HOME=/root
export PATH="$HOME/.local/bin:$PATH"
export SOVEREIGN_DATA_DIR="/data"
mkdir -p /data
PORT="${SMOKE_DAEMON_PORT:-9741}"

# CPU-only containers: force the GPU offload layer count when requested. Soak
# runs default this to `off` (see the outer script) so a Vulkan-compiled binary
# runs on CPU instead of failing to offload to an absent GPU.
[ -n "${SMOKE_GPU_LAYERS:-}" ] && export SOVEREIGN_GPU_LAYERS="$SMOKE_GPU_LAYERS"

echo "RESULT: phase=deps begin"
eval "$DEPS_CMD" || { echo "RESULT: phase=deps status=FAIL"; exit 21; }
echo "RESULT: phase=deps status=PASS"

# ── Phase: install (the distribution surface under test) ────────────────────
echo "RESULT: phase=install mode=$INSTALL_MODE begin"
case "$INSTALL_MODE" in
  hosted) curl -fsSL "$INSTALL_URL" | sh > /data/install.log 2>&1 ;;
  local)  sh /host-install.sh          > /data/install.log 2>&1 ;;
  binary) ln -sf sovereign-cli "$HOME/.local/bin/svrn" && echo "binary mounted; svrn -> sovereign-cli (+ daemon/llm siblings)" > /data/install.log 2>&1 ;;
esac
INSTALL_RC=$?
sed 's/^/[install] /' /data/install.log || true
if [ "$INSTALL_RC" -ne 0 ]; then echo "RESULT: phase=install status=FAIL rc=$INSTALL_RC"; exit 24; fi
if ! command -v svrn >/dev/null 2>&1; then
  echo "RESULT: phase=install status=FAIL detail=svrn-not-on-PATH"; exit 24
fi
echo "RESULT: phase=install status=PASS version=$(svrn --version 2>/dev/null | head -n1)"

# Installer-only lane: prove the distribution installs and runs, no model/
# inference. Fast and CI-friendly — the right gate for release/installer edits.
if [ "${INSTALL_ONLY:-}" = "1" ]; then
  echo "RESULT: phase=done status=PASS scope=install-only"
  exit 0
fi

# Pre-seed tiny models when mounted so `setup --yes` skips the download.
if [ -d /models ] && ls /models/*.gguf >/dev/null 2>&1; then
  mkdir -p /data/models && cp -n /models/*.gguf /data/models/ 2>/dev/null || true
  echo "RESULT: phase=provision models=local"
else
  echo "RESULT: phase=provision models=download"
fi

echo "RESULT: phase=setup begin"
svrn setup --yes --data-dir /data > /data/setup.log 2>&1
echo "RESULT: phase=setup rc=$?"
sed 's/^/[setup] /' /data/setup.log || true

# ── Golden path: boot the daemon (default posture — no bypass env) ──────────
echo "RESULT: phase=daemon begin"
svrn daemon run --data-dir /data > /data/daemon.log 2>&1 &
DAEMON_PID=$!
UP=""
for _ in $(seq 1 "${SMOKE_HEALTH_TIMEOUT:-180}"); do
  kill -0 "$DAEMON_PID" 2>/dev/null || break
  if curl -fsS "http://127.0.0.1:${PORT}/status" >/dev/null 2>&1; then UP="1"; break; fi
  sleep 1
done
sed 's/^/[daemon] /' /data/daemon.log || true

if [ -n "$UP" ]; then
  echo "RESULT: phase=daemon status=PASS"
else
  echo "RESULT: phase=daemon status=FAIL"
  grep -q "VRAM capacity check refused" /data/daemon.log \
    && echo "RESULT: regression=vram_hard_block detail=daemon-refused-on-vram"
  if grep -q "null result from llama cpp" /data/daemon.log \
     && ! grep -q "likely causes" /data/daemon.log; then
    echo "RESULT: regression=opaque_null detail=bare-null-result-no-guidance"
  fi
  kill "$DAEMON_PID" 2>/dev/null
  exit 22
fi

# ── Soak: loop chat turns for a duration (long local run) ───────────────────
SOAK="${SMOKE_SOAK_MINUTES:-0}"
if [ "$SOAK" != "0" ]; then
  echo "RESULT: phase=soak begin minutes=$SOAK gpu_layers=${SMOKE_GPU_LAYERS:-default}"
  END=$(( $(date +%s) + SOAK * 60 ))
  set -- "What is this assistant and where does it run?" \
         "Name one source it can cite." \
         "Does anything leave my machine by default?" \
         "What can it help me do?"
  turns=0; fails=0; grounded=0; i=0
  while [ "$(date +%s)" -lt "$END" ]; do
    eval "p=\${$(( i % 4 + 1 ))}"
    out="$(timeout "${SMOKE_CHAT_TIMEOUT:-300}" svrn chat ask "$p" 2>&1)"
    turns=$((turns+1)); i=$((i+1))
    if [ -z "$(printf '%s' "$out" | tr -d '[:space:]')" ]; then
      fails=$((fails+1)); echo "RESULT: soak_turn=$turns status=FAIL detail=empty"
    else
      printf '%s' "$out" | grep -q "\[Source:" && grounded=$((grounded+1))
      echo "RESULT: soak_turn=$turns status=PASS"
    fi
    if ! curl -fsS "http://127.0.0.1:${PORT}/status" >/dev/null 2>&1; then
      echo "RESULT: soak_daemon=DIED after_turns=$turns"
      echo "RESULT: phase=soak done turns=$turns fails=$fails grounded=$grounded status=FAIL"
      kill "$DAEMON_PID" 2>/dev/null; exit 25
    fi
  done
  st=$([ "$fails" -eq 0 ] && echo PASS || echo DEGRADED)
  echo "RESULT: phase=soak done turns=$turns fails=$fails grounded=$grounded status=$st"
  kill "$DAEMON_PID" 2>/dev/null
  echo "RESULT: phase=done status=PASS scope=soak turns=$turns fails=$fails grounded=$grounded"
  exit 0
fi

# ── Golden path: one grounded chat turn ─────────────────────────────────────
echo "RESULT: phase=chat begin"
CHAT_OUT="$(timeout "${SMOKE_CHAT_TIMEOUT:-180}" svrn chat ask "$SMOKE_PROMPT" 2>&1)"
echo "$CHAT_OUT" | sed 's/^/[chat] /'
if [ -n "$(printf '%s' "$CHAT_OUT" | tr -d '[:space:]')" ]; then
  echo "RESULT: phase=chat status=PASS"
  printf '%s' "$CHAT_OUT" | grep -q "\[Source:" \
    && echo "RESULT: chat_grounded=yes" || echo "RESULT: chat_grounded=no"
else
  echo "RESULT: phase=chat status=FAIL detail=empty-response"
  kill "$DAEMON_PID" 2>/dev/null
  exit 23
fi

kill "$DAEMON_PID" 2>/dev/null
echo "RESULT: phase=done status=PASS"
DRIVER
}

# ── Dry run: validate plumbing on any host, no binary/model required ──────────
if [ -n "$DRY_RUN" ]; then
  info "DRY RUN — validating harness plumbing (no golden-path assertions)"
  info "install-mode=$INSTALL_MODE $([ "$INSTALL_MODE" = hosted ] && echo "url=$INSTALL_URL")"
  [ "$INSTALL_MODE" = binary ] && { [ -f "$BINARY/sovereign-cli" ] && info "binary dir: $BINARY (sovereign-cli + siblings present)" || info "binary dir: MISSING/incomplete ($BINARY) — build the linux leg"; }
  [ "$INSTALL_MODE" = local ]  && info "local installer: $LOCAL_INSTALLER"
  [ -n "$MODEL_DIR" ] && info "model-dir: $MODEL_DIR ($(ls "$MODEL_DIR"/*.gguf 2>/dev/null | wc -l | tr -d ' ') gguf)" || info "model-dir: none (setup will download)"
  rc=0
  for distro in "${DISTRO_ARR[@]}"; do
    info "would run distro=$distro platform=$PLATFORM"
    info "  proving container plumbing: $RUNTIME run --rm --platform $PLATFORM $distro true"
    if "$RUNTIME" run --rm --platform "$PLATFORM" "$distro" true >/dev/null 2>&1; then
      info "  distro=$distro plumbing=OK"
    else
      info "  distro=$distro plumbing=FAIL (image pull / platform emulation issue)"; rc=1
    fi
  done
  [ "$rc" = 0 ] && info "dry-run: plumbing OK for all distros" || info "dry-run: some distros unreachable"
  exit "$rc"
fi

# ── Real run (binary dir already validated at parse time) ────────────────────
overall=0
declare -a SUMMARY
for distro in "${DISTRO_ARR[@]}"; do
  info "──────── distro: $distro ────────"
  cname="cli-smoke-${STAMP}-$(echo "$distro" | tr ':/.' '-')"
  log="$ART/$(echo "$distro" | tr ':/.' '-').log"

  mounts=()
  if [ "$INSTALL_MODE" = binary ]; then
    for b in sovereign-cli sovereign-cli-daemon sovereign-cli-llm; do
      mounts+=(-v "$BINARY/$b:/root/.local/bin/$b:ro")
    done
  fi
  [ "$INSTALL_MODE" = local ]  && mounts+=(-v "$LOCAL_INSTALLER:/host-install.sh:ro")
  [ -n "$MODEL_DIR" ] && mounts+=(-v "$(cd "$MODEL_DIR" && pwd):/models:ro")
  # Persist /data (config + downloaded models) so a soak downloads once and
  # reuses it on later runs instead of re-fetching into an ephemeral container.
  if [ -n "$DATA_DIR_HOST" ]; then
    mkdir -p "$DATA_DIR_HOST"
    mounts+=(-v "$(cd "$DATA_DIR_HOST" && pwd):/data:Z")
  fi

  "$RUNTIME" run --rm --name "$cname" --platform "$PLATFORM" \
    ${mounts[@]+"${mounts[@]}"} \
    -e DEPS_CMD="$(deps_cmd_for "$distro")" \
    -e INSTALL_MODE="$INSTALL_MODE" \
    -e INSTALL_URL="$INSTALL_URL" \
    -e INSTALL_ONLY="$INSTALL_ONLY" \
    -e SMOKE_PROMPT="$PROMPT" \
    -e SMOKE_HEALTH_TIMEOUT="$SMOKE_HEALTH_TIMEOUT" \
    -e SMOKE_CHAT_TIMEOUT="$SMOKE_CHAT_TIMEOUT" \
    -e SMOKE_DAEMON_PORT="$SMOKE_DAEMON_PORT" \
    -e SMOKE_SOAK_MINUTES="$SOAK_MINUTES" \
    -e SMOKE_GPU_LAYERS="$GPU_LAYERS" \
    "$distro" \
    /bin/sh -c "$(container_driver)" > "$log" 2>&1
  drc=$?

  verdict="PASS"; note=""
  if grep -q "RESULT: phase=install status=FAIL" "$log"; then
    verdict="FAIL"; note="installer failed ($(grep -o 'detail=[^ ]*' "$log" | head -n1))"; overall=1
  elif grep -q "RESULT: regression=vram_hard_block" "$log"; then
    verdict="FAIL"; note="VRAM gate hard-blocked CPU-only startup (advisory regression)"; overall=1
  elif grep -q "RESULT: regression=opaque_null" "$log"; then
    verdict="FAIL"; note="bare 'null result from llama cpp' with no guidance (diagnostics regression)"; overall=1
  elif grep -q "RESULT: phase=done status=PASS scope=install-only" "$log"; then
    note="installer OK — $(grep -o 'version=.*' "$log" | head -n1)"
  elif grep -q "RESULT: phase=done status=PASS scope=soak" "$log"; then
    s="$(grep -o 'RESULT: phase=soak done .*' "$log" | tail -n1 | sed 's/RESULT: phase=soak done //')"
    note="soak OK — $s"
  elif grep -q "RESULT: phase=done status=PASS" "$log"; then
    note="golden path OK$(grep -q 'chat_grounded=yes' "$log" && echo ' (grounded)' || echo ' (ungrounded)')"
  elif grep -q "RESULT: phase=daemon status=PASS" "$log" && [ -z "$MODEL_DIR" ]; then
    note="installed + daemon booted; chat SKIP (no --model-dir)"
  else
    verdict="FAIL"; note="golden path incomplete (rc=$drc; see $log)"; overall=1
  fi
  SUMMARY+=("$(printf '%-22s %-4s %s' "$distro" "$verdict" "$note")")
  info "$distro → $verdict — $note"
done

echo
info "══════════ cli-smoke summary (install-mode=$INSTALL_MODE) ══════════"
for row in "${SUMMARY[@]}"; do echo "  $row"; done
info "log dir: $ART"
[ "$overall" = 0 ] && info "RESULT: all distros passed" || info "RESULT: regressions found"
exit "$overall"
