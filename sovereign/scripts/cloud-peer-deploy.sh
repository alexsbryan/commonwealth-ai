#!/usr/bin/env bash
# cloud-peer-deploy.sh — RunPod pod lifecycle for cloud-peer flavors.
#
# Subcommands:
#   up                  Create a new pod with the right env vars + GPU.
#                       Prints the pod ID. Defaults to the cheapest
#                       CUDA option (L40S 48GB, ~$0.79/hr).
#   ls                  List all your pods + their states.
#   get <pod-id>        Show one pod's details (GPU type, IP, status).
#   down <pod-id>       Stop billing AND remove the pod entry.
#
# Note: there's no `logs` subcommand. runpodctl has no log-streaming
# CLI (open issue runpod/runpodctl#29 since 2023). To watch a pod's
# container output, use the RunPod web UI's "Logs" tab on the pod
# detail page, or shell in via the UI's "Connect" → "Web Terminal".
#
# Required env (sourced from your shell — re-export from cloud-peer-provision.sh
# output, or paste directly):
#
#   TS_AUTHKEY          tskey-... (reusable, ephemeral)
#   MESH_SEED_ADDR      <laptop-tailnet-ip>:9742
#   R2_ENDPOINT         https://<account>.r2.cloudflarestorage.com
#   R2_ACCESS_KEY       <r2 access key id>
#   R2_SECRET_KEY       <r2 secret access key>
#
# Optional env (defaults match the typical first-smoke deploy):
#   IMAGE               ghcr.io/<gh-user>/sovereign-cuda:latest
#                       Default reads GHCR_USER, then ${USER}.
#   FLAVOR              cuda | rocm  (default: cuda — wider GPU availability)
#   GPU_TYPE            "NVIDIA L40S"   for cuda
#                       "AMD Instinct MI300X"  for rocm
#                       (override if you want a specific class)
#   POD_NAME            sovereign-<flavor>-<random>
#   PRIMARY_COPIES      1   (bump for fan-out on bigger boxes)
#   CONTEXT_SIZE        32768
#   R2_BUCKET           sovereign-models
#   CONTAINER_DISK_GB   25
#   COST_CEILING        Pass-through to --cost (max $/hr). Default 0
#                       = "lowest available". Set e.g. 1.00 to cap.
#   CLOUD               secure | community | any  (default secure)
#
# Examples:
#   ./scripts/cloud-peer-deploy.sh up
#   ./scripts/cloud-peer-deploy.sh up FLAVOR=rocm GPU_TYPE='AMD Instinct MI300X' PRIMARY_COPIES=4
#   ./scripts/cloud-peer-deploy.sh logs <pod-id>
#   ./scripts/cloud-peer-deploy.sh down <pod-id>

set -euo pipefail

die()  { echo "cloud-peer-deploy: $*" >&2; exit 1; }
info() { echo "== $*"; }

require_tool() {
    command -v "$1" >/dev/null 2>&1 \
        || die "$1 not found on PATH. Install: $2"
}

cmd_up() {
    require_tool runpodctl "mkdir -p ~/.local/bin && curl -fL --progress-bar https://github.com/runpod/runpodctl/releases/latest/download/runpodctl-linux-amd64 -o ~/.local/bin/runpodctl && chmod +x ~/.local/bin/runpodctl"

    : "${TS_AUTHKEY:?TS_AUTHKEY required (Tailscale auth key)}"
    : "${MESH_SEED_ADDR:?MESH_SEED_ADDR required (e.g. 100.x.y.z:9742)}"
    : "${R2_ENDPOINT:?R2_ENDPOINT required}"
    : "${R2_ACCESS_KEY:?R2_ACCESS_KEY required}"
    : "${R2_SECRET_KEY:?R2_SECRET_KEY required}"

    # Sanity check: MESH_SEED_ADDR needs host:port; the daemon's mesh
    # gossip parses this strictly. A bare IP fails silently downstream.
    if [[ "$MESH_SEED_ADDR" != *:* ]]; then
        die "MESH_SEED_ADDR must be host:port (you have '${MESH_SEED_ADDR}'); append ':9742'"
    fi

    local flavor="${FLAVOR:-cuda}"
    local gpu_type
    case "$flavor" in
        cuda) gpu_type="${GPU_TYPE:-NVIDIA L40S}" ;;
        rocm) gpu_type="${GPU_TYPE:-AMD Instinct MI300X}" ;;
        *) die "FLAVOR must be 'cuda' or 'rocm', got '$flavor'" ;;
    esac

    # GHCR username: env override > `gh` CLI's logged-in user > local
    # $USER (last-resort default; often wrong since GitHub usernames
    # don't always match Linux usernames).
    local gh_user="${GHCR_USER:-}"
    if [[ -z "$gh_user" ]] && command -v gh >/dev/null 2>&1; then
        gh_user="$(gh api user --jq .login 2>/dev/null || true)"
    fi
    gh_user="${gh_user:-${USER}}"
    local image="${IMAGE:-ghcr.io/${gh_user}/sovereign-${flavor}:latest}"

    local pod_name="${POD_NAME:-sovereign-${flavor}-$(date +%s | tail -c 6)}"
    local primary_copies="${PRIMARY_COPIES:-1}"
    local context_size="${CONTEXT_SIZE:-32768}"
    local r2_bucket="${R2_BUCKET:-sovereign-models}"
    # 60 GB default = ~50 GB for GGUFs (28 primary + 9 fast + 0.6 embed)
    # + ~5 GB image footprint + headroom. Container disk pricing is
    # negligible (~$0.10/GB-month while running, so 60 GB for a 1 hr
    # session is fractions of a cent). Smaller defaults ENOSPC mid-sync.
    local container_disk="${CONTAINER_DISK_GB:-60}"
    local cloud_tier="${CLOUD:-secure}"   # secure | community
    local cost_ceiling="${COST_CEILING:-0}"

    cat <<EOF

== Cloud peer deploy ==
  flavor:           $flavor
  image:            $image
  gpu type:         $gpu_type
  cloud tier:       $cloud_tier
  cost ceiling:     ${cost_ceiling} \$/hr  $([ "$cost_ceiling" = "0" ] && echo '(0 = lowest available)')
  pod name:         $pod_name
  container disk:   ${container_disk} GB
  primary_copies:   $primary_copies
  context_size:     $context_size
  r2 bucket:        $r2_bucket
  mesh seed:        $MESH_SEED_ADDR
EOF

    # Build env-var args. Each --env arg is a single KEY=VALUE; we
    # repeat the flag per var so values can contain '=' without
    # ambiguity (R2 secrets sometimes do).
    local env_args=(
        --env "TS_AUTHKEY=${TS_AUTHKEY}"
        --env "MESH_SEED_ADDR=${MESH_SEED_ADDR}"
        --env "R2_ENDPOINT=${R2_ENDPOINT}"
        --env "R2_ACCESS_KEY=${R2_ACCESS_KEY}"
        --env "R2_SECRET_KEY=${R2_SECRET_KEY}"
        --env "R2_BUCKET=${r2_bucket}"
        --env "PRIMARY_COPIES=${primary_copies}"
        --env "CONTEXT_SIZE=${context_size}"
    )

    # CUDA-only: llama.cpp's bundled ggml-cuda.cu calls ncclCommInitAll
    # unconditionally even on single-GPU builds, and NCCL fails in
    # some container environments due to /dev/shm size or P2P checks.
    # These three together force NCCL into its safest single-device
    # path; NCCL_DEBUG=INFO surfaces the specific cause when it does
    # still fail. Benign no-ops on ROCm pods.
    if [[ "$flavor" == "cuda" ]]; then
        env_args+=(
            --env "NCCL_P2P_DISABLE=1"
            --env "NCCL_SHM_DISABLE=1"
            --env "NCCL_DEBUG=INFO"
        )
    fi

    # Cloud-tier flag: --secureCloud or --communityCloud.
    local tier_args=()
    case "$cloud_tier" in
        secure)    tier_args=(--secureCloud) ;;
        community) tier_args=(--communityCloud) ;;
        any)       tier_args=() ;;
        *) die "CLOUD must be 'secure', 'community', or 'any' (got '$cloud_tier')" ;;
    esac

    info "running: runpodctl create pods …"
    # Flag names match `runpodctl create pods --help` (current as of
    # the runpodctl release linked in docs/CLOUD_PEER_DEPLOY.md):
    #   --containerDiskSize  not --containerDiskInGb
    #   --volumeSize         min 1 (no way to skip the volume entirely;
    #                        1 GB ≈ $0.10/mo, negligible)
    # No --ports: traffic flows over Tailscale, not RunPod's HTTP
    # proxy, so we don't need a public port mapping.
    runpodctl create pods \
        --name "$pod_name" \
        --imageName "$image" \
        --gpuType "$gpu_type" \
        --gpuCount 1 \
        --containerDiskSize "$container_disk" \
        --volumeSize 1 \
        --cost "$cost_ceiling" \
        "${tier_args[@]}" \
        "${env_args[@]}"

    cat <<EOF

== Pod created ==

Cold-start timeline:
  0-30s    pod scheduling, image pull
  30s-2m   tailscale up, rclone sync ~50 GB from R2
  2m-4m    slot loads (~28 GB primary + 9 GB fast + 0.6 GB embed)
  4m-5m    daemon advertising via mesh

Watch progress:
  - RunPod web UI → My Pods → click your pod → "Logs" tab
  - or shell in: web UI → "Connect" → "Web Terminal", then 'tail -f' the
    daemon stdout (no 'runpodctl logs' subcommand exists)
  - state-only check from CLI:
      $0 ls
      $0 get <pod-id>

Once the pod's daemon is up, smoke test from the laptop:
  curl -s http://localhost:9741/v1/models | jq '.data[].id'
  # expect to see: primary, fast, embed alongside local slots

Tear down when done (this stops billing AND deletes the pod):
  $0 down <pod-id>
EOF
}

cmd_ls() {
    require_tool runpodctl "see 'up' subcommand error"
    # Note: subcommand is 'get pod' (singular) — 'get pods' is not
    # registered in runpodctl's legacy alias map.
    runpodctl get pod
}

cmd_get() {
    require_tool runpodctl "see 'up' subcommand error"
    local pod_id="${1:?pod id required: $0 get <pod-id>}"
    # Modern style: 'pod get <id>'. The legacy 'get pod <id>' form
    # exists too but reuses the list handler — modern is more reliable.
    runpodctl pod get "$pod_id"
}

cmd_down() {
    require_tool runpodctl "see 'up' subcommand error"
    local pod_id="${1:?pod id required: $0 down <pod-id>}"
    info "stopping pod $pod_id"
    runpodctl stop pod "$pod_id" || true
    info "removing pod $pod_id"
    runpodctl remove pod "$pod_id" || true
    info "done — Tailscale auto-cleans the ephemeral peer"
}

case "${1:-}" in
    up)    shift; cmd_up   "$@" ;;
    ls)    shift; cmd_ls   "$@" ;;
    get)   shift; cmd_get  "$@" ;;
    down)  shift; cmd_down "$@" ;;
    ""|-h|--help)
        sed -n '2,/^set -/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;/^set -/d'
        exit 0 ;;
    *)
        die "unknown subcommand: $1 (try: up | ls | get | down | --help)" ;;
esac
