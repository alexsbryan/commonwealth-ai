#!/bin/bash
# Sovereign ephemeral-worker pod entrypoint.
#
# Replaces the pre-2026-05-15 Tailscale + R2 + mesh-join boot path.
# This pod is owned by exactly one persistent peer (its owner) for the
# duration of one job; it never joins the mesh.
#
# Spec: sovereign/docs/EPHEMERAL_WORKER_PODS.md.
#
# Boot sequence:
#   1. Validate SOVEREIGN_BOOTSTRAP env (base64 blob the owner minted).
#   2. Sync the clock against an HTTPS Date header — vast hosts hand
#      us clocks with skew measured in hours, and that breaks both
#      the bootstrap's TLS handshake and any time-bounded token
#      validation. (Same logic as the legacy entrypoint; kept here.)
#   3. exec `sovereign-cli daemon run --worker-mode`. The daemon
#      decodes the blob, derives the seed-derived TLS cert, and
#      serves the four owner-only routes on :9742.
#
# Required env (set by the owner via the bootstrap blob mechanism;
# see EPHEMERAL_WORKER_PODS.md "Bootstrap flow"):
#   SOVEREIGN_BOOTSTRAP   base64-url-no-pad encoding of the bootstrap
#                         blob (job id, seed, owner pubkey, signed
#                         worker token, upload SHA manifest, expiry).
#
# Compared to the legacy entrypoint, the following env vars are GONE:
#   TS_AUTHKEY / TAILSCALE_AUTHKEY   — no Tailscale
#   MESH_SEED_ADDR / MESH_JOIN_LINK  — no mesh join
#   R2_ENDPOINT / R2_ACCESS_KEY / R2_SECRET_KEY / R2_BUCKET — no rclone
#   PRIMARY_GGUF / FAST_GGUF / EMBED_GGUF / SINGLE_MODEL — owner uploads
#   PRIMARY_COPIES / CONTEXT_SIZE / NODE_ROLE — no model loading in
#                                                worker mode
#
# Logs go to stderr (Vast captures + makes them available via
# `vastai logs <id>`).
set -euo pipefail

: "${SOVEREIGN_BOOTSTRAP:?SOVEREIGN_BOOTSTRAP is required (base64 bootstrap blob from owner; see EPHEMERAL_WORKER_PODS.md)}"

DATA_DIR="${SOVEREIGN_DATA_DIR:-/workspace/data}"
MODELS_DIR="${SOVEREIGN_MODELS_DIR:-/workspace/models}"
mkdir -p "$DATA_DIR" "$MODELS_DIR"

# ─── GPU + driver diagnostics ────────────────────────────────────────
# Print enough to identify driver/runtime mismatches before a slot
# load crashes. The new worker-mode daemon doesn't *load* models
# itself (the owner uploads them; the runner consumes them), but
# downstream pipeline runners may; either way this is cheap and the
# operator wants the info in `vastai logs` from second one.
echo "[entrypoint] worker-mode boot — pod is owned, not meshed."
if command -v nvidia-smi >/dev/null 2>&1; then
    nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>&1 \
        | sed 's/^/  nvidia-smi: /' || true
elif command -v rocm-smi >/dev/null 2>&1; then
    rocm-smi --showdriverversion 2>&1 | sed 's/^/  rocm-smi: /' || true
fi
if [[ -f /usr/local/cuda/version.json ]]; then
    awk -F'"' '/version/ {print "  cuda toolkit (image): " $4; exit}' /usr/local/cuda/version.json
fi

# ─── CUDA preflight (kept from legacy entrypoint) ────────────────────
# Fails fast on broken CUDA hosts so the orchestrator can pick a
# different Vast offer. The owner pays for the failed offer's first
# few minutes but skips the 80-GB upload that would otherwise follow.
if [[ -x /usr/local/bin/cuda-preflight ]]; then
    echo "[entrypoint] CUDA preflight..."
    if ! /usr/local/bin/cuda-preflight; then
        echo "[entrypoint] FATAL: CUDA preflight failed — bail out before the upload."
        exit 1
    fi
fi

# ─── Clock sync via HTTPS Date header ────────────────────────────────
# Tokens carry an `expires_unix` claim; the worker daemon rejects
# tokens older than that. If the pod's clock is ahead by hours
# (observed on Finnish Vast hosts on 2026-05-15) every owner request
# will 401 immediately. Pull a trustworthy Date from Cloudflare and
# set the system clock. Best-effort — failure is non-fatal because
# small skew (<5 min) is within token validity windows.
echo "[entrypoint] clock sync (before: $(date -u +%Y-%m-%dT%H:%M:%SZ))"
HTTP_DATE=$(curl -sIm 10 https://www.cloudflare.com/ 2>/dev/null \
    | awk 'BEGIN{IGNORECASE=1} /^date:/ {sub(/^[Dd]ate:[ \t]*/,""); sub(/\r$/,""); print; exit}')
if [ -z "$HTTP_DATE" ]; then
    echo "[entrypoint] WARNING: HTTP Date probe returned nothing; clock unchanged"
elif date -s "$HTTP_DATE" >/dev/null 2>&1; then
    echo "[entrypoint] clock set to $(date -u +%Y-%m-%dT%H:%M:%SZ) (cloudflare.com Date)"
else
    echo "[entrypoint] WARNING: date -s rejected (CAP_SYS_TIME absent or kernel blocked)"
    echo "[entrypoint]   target was: $HTTP_DATE"
    echo "[entrypoint]   current:    $(date -u +%Y-%m-%dT%H:%M:%SZ)"
fi

# ─── Sanity: blob length is plausible ────────────────────────────────
# A well-formed bootstrap blob is base64-url-no-pad of compact JSON
# carrying a 32-byte seed + a 32-byte owner key + a signed token +
# the upload SHA manifest. Tens to a few-hundred bytes encoded; if
# the env got truncated (Vast onstart-cmd length cap, shell quoting)
# we want a clear error here rather than a cryptic JSON-decode
# failure from the daemon.
blob_len=${#SOVEREIGN_BOOTSTRAP}
if [ "$blob_len" -lt 64 ]; then
    echo "[entrypoint] FATAL: SOVEREIGN_BOOTSTRAP looks truncated (length=$blob_len)."
    echo "  Expected at least ~200 bytes of base64-url. Check Vast's onstart-cmd"
    echo "  passing — the env var must be quoted to preserve '=' and '_' chars."
    exit 1
fi
echo "[entrypoint] bootstrap blob loaded (length=$blob_len bytes)"

# ─── Launch worker daemon ────────────────────────────────────────────
# The daemon reads SOVEREIGN_BOOTSTRAP from the inherited env. We
# don't decode here — the daemon's own decoder gives better errors
# (and decoding twice would be redundant + drift-prone).
echo "[entrypoint] launching sovereign-cli daemon run --worker-mode"
exec sovereign-cli daemon run --worker-mode
