#!/bin/bash
# Sovereign-mesh worker entrypoint.
#
# Boots an ad-hoc cloud peer that:
#   1. Joins the operator's tailnet (so the home laptop can reach it).
#   2. Pulls Darwin GGUFs from an S3/R2 bucket into /workspace/models.
#   3. Writes ~/.config/sovereign/config.toml from env vars.
#   4. Execs `sovereign-cli daemon run`.
#
# Required env vars:
#   TS_AUTHKEY           Reusable, ephemeral, pre-authorized Tailscale auth key.
#   MESH_SEED_ADDR       host:port of the founder daemon's internal port.
#                        e.g. 100.104.36.28:9742  (laptop's tailnet IP).
#   R2_ENDPOINT          S3-compatible endpoint (Cloudflare R2, AWS S3, etc.)
#                        e.g. https://<account>.r2.cloudflarestorage.com
#   R2_ACCESS_KEY        S3 access key id.
#   R2_SECRET_KEY        S3 secret access key.
#   R2_BUCKET            Bucket name holding the GGUFs. Default: sovereign-models.
#
# Optional env vars:
#   PRIMARY_COPIES       Number of primary slot copies to load (multi-primary).
#                        Default 1. Requires daemon support; falls back to 1
#                        on older binaries.
#   CONTEXT_SIZE         n_ctx for all loaded slots. Default 32768.
#   PRIMARY_GGUF         Filename of the primary GGUF inside the bucket.
#                        Default: FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf
#   FAST_GGUF            Filename of the fast GGUF. Default: Darwin-9B-Opus.Q8_0.gguf
#   EMBED_GGUF           Filename of the embedding GGUF.
#                        Default: Qwen3-Embedding-0.6B-Q8_0.gguf
#   NODE_ROLE            Mesh node role tag. Default: ephemeral-worker.
#   MESH_JOIN_LINK       Optional. `sovereign://join/cwth-...` (or bare key /
#                        https URL) printed by `sovereign mesh create` /
#                        `sovereign mesh rotate` on the founder. When set,
#                        the entrypoint runs `sovereign mesh join` after the
#                        daemon comes up so the pod participates in mesh
#                        gossip and the founder's scheduler can route to it.
#                        When unset, the pod boots into a solo mesh and is
#                        only reachable via per-config `base_url`.
set -euo pipefail

: "${TS_AUTHKEY:?TS_AUTHKEY is required}"
: "${MESH_SEED_ADDR:?MESH_SEED_ADDR is required (host:port of the founder daemon internal port)}"
: "${R2_ENDPOINT:?R2_ENDPOINT is required}"
: "${R2_ACCESS_KEY:?R2_ACCESS_KEY is required}"
: "${R2_SECRET_KEY:?R2_SECRET_KEY is required}"
MESH_JOIN_LINK="${MESH_JOIN_LINK:-}"

R2_BUCKET="${R2_BUCKET:-sovereign-models}"
PRIMARY_COPIES="${PRIMARY_COPIES:-1}"
CONTEXT_SIZE="${CONTEXT_SIZE:-32768}"
PRIMARY_GGUF="${PRIMARY_GGUF:-FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf}"
FAST_GGUF="${FAST_GGUF:-Darwin-9B-Opus.Q8_0.gguf}"
EMBED_GGUF="${EMBED_GGUF:-Qwen3-Embedding-0.6B-Q8_0.gguf}"
NODE_ROLE="${NODE_ROLE:-ephemeral-worker}"

MODELS_DIR="${SOVEREIGN_MODELS_DIR:-/workspace/models}"
DATA_DIR="${SOVEREIGN_DATA_DIR:-/workspace/data}"
mkdir -p "$MODELS_DIR" "$DATA_DIR"

# ─── 0. GPU + driver diagnostics ─────────────────────────────────────
# Print enough to identify driver/runtime mismatches before the daemon
# crashes (NCCL "CUDA driver version is insufficient" errors are loud
# but don't tell you the actual driver number — this does).
echo "[entrypoint] GPU + driver diagnostics:"
if command -v nvidia-smi >/dev/null 2>&1; then
    nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>&1 | sed 's/^/  nvidia-smi: /' || true
elif command -v rocm-smi >/dev/null 2>&1; then
    rocm-smi --showdriverversion 2>&1 | sed 's/^/  rocm-smi: /' || true
fi
if [[ -f /usr/local/cuda/version.json ]]; then
    awk -F'"' '/version/ {print "  cuda toolkit (image): " $4; exit}' /usr/local/cuda/version.json
fi

# ─── 1. Tailscale ────────────────────────────────────────────────────
echo "[entrypoint] starting tailscaled…"
tailscaled --tun=userspace-networking --state=/var/lib/tailscale/tailscaled.state \
    --socket=/var/run/tailscale/tailscaled.sock &
TS_PID=$!
sleep 2
echo "[entrypoint] joining tailnet (hostname=sovereign-${HOSTNAME})"
tailscale up \
    --authkey="$TS_AUTHKEY" \
    --hostname="sovereign-${HOSTNAME}" \
    --accept-dns=false
echo "[entrypoint] tailnet IP: $(tailscale ip -4 || true)"

# ─── 2. rclone config + model sync ───────────────────────────────────
# provider = Cloudflare + region = auto are required for R2.
# `Other` falls back to us-east-1 for SigV4 signing and R2 401s.
cat > /root/.config/rclone/rclone.conf <<EOF
[r2]
type = s3
provider = Cloudflare
region = auto
endpoint = ${R2_ENDPOINT}
access_key_id = ${R2_ACCESS_KEY}
secret_access_key = ${R2_SECRET_KEY}
acl = private
EOF

# Self-test the R2 wiring before the long-running sync, so any
# misconfiguration fails loudly with a precise diagnosis (the
# previous behavior was rclone retrying silently for minutes before
# we finally noticed something was wrong).
echo "[entrypoint] R2 config:"
echo "  endpoint  = ${R2_ENDPOINT}"
echo "  bucket    = ${R2_BUCKET}"
echo "  akey len  = ${#R2_ACCESS_KEY}    (expected 32)"
echo "  skey len  = ${#R2_SECRET_KEY}    (expected ~64)"

echo "[entrypoint] R2 self-test 1/2: list root remote"
if ! rclone lsd r2: 2>&1; then
    echo "[entrypoint] FATAL: 'rclone lsd r2:' failed."
    echo "  This means auth itself is broken — endpoint/keys are wrong."
    echo "  Common causes:"
    echo "    - R2_ENDPOINT is the wrong host (need https://<account>.r2.cloudflarestorage.com)"
    echo "    - You passed the Cloudflare API Token instead of the S3 Access Key + Secret"
    echo "    - region = auto wasn't applied (should be in this image now; rebuild if not)"
    exit 1
fi

echo "[entrypoint] R2 self-test 2/2: list target bucket"
if ! rclone lsf "r2:${R2_BUCKET}" 2>&1; then
    echo "[entrypoint] FATAL: 'rclone lsf r2:${R2_BUCKET}' failed."
    echo "  Auth works but bucket access does not. Likely causes:"
    echo "    - Bucket name typo (R2_BUCKET=${R2_BUCKET} doesn't exist)"
    echo "    - Token wasn't scoped to this bucket"
    exit 1
fi

echo "[entrypoint] syncing models from r2:${R2_BUCKET} → ${MODELS_DIR}"
# --transfers=4 saturates RunPod's network without overwhelming local IO.
# --checkers=8 hashes existing files so re-runs skip already-present GGUFs.
# --retries 5: pod's network can blip during a 50 GB pull.
rclone sync "r2:${R2_BUCKET}" "$MODELS_DIR" --progress --transfers=4 --checkers=8 --retries=5

# Sanity: required files present?
for f in "$PRIMARY_GGUF" "$FAST_GGUF" "$EMBED_GGUF"; do
  if [ ! -f "$MODELS_DIR/$f" ]; then
    echo "[entrypoint] FATAL: $MODELS_DIR/$f missing after sync (check R2_BUCKET / GGUF env vars)"
    exit 1
  fi
done

# ─── 3. config.toml ──────────────────────────────────────────────────
CONFIG=/root/.config/sovereign/config.toml
mkdir -p "$(dirname "$CONFIG")"

# When PRIMARY_COPIES>1, we emit a `primary_pool` table the daemon
# interprets as "load N copies of `path` and register each as a
# distinct primary slot". Older binaries that don't recognise
# primary_pool will load only `primary` (single slot) — this preserves
# back-compat at the cost of losing parallelism.
if [ "$PRIMARY_COPIES" -gt 1 ]; then
  PRIMARY_BLOCK="
[models.primary_pool]
copies = ${PRIMARY_COPIES}
path = \"${MODELS_DIR}/${PRIMARY_GGUF}\""
else
  PRIMARY_BLOCK=""
fi

cat > "$CONFIG" <<EOF
[models]
primary = "${MODELS_DIR}/${PRIMARY_GGUF}"
fast = "${MODELS_DIR}/${FAST_GGUF}"
embed = "${MODELS_DIR}/${EMBED_GGUF}"
context_size = ${CONTEXT_SIZE}
${PRIMARY_BLOCK}

[daemon]
client_port = 9741
internal_port = 9742
autostart = false
primary_idle_secs = 0
extras_idle_secs = 0
yield_to_foreground_secs = 0

[data]
dir = "${DATA_DIR}"

[mesh]
seed_addrs = ["${MESH_SEED_ADDR}"]
node_role = "${NODE_ROLE}"
EOF

echo "[entrypoint] config written:"
sed -E 's/(access_key|secret_key|authkey)([^=]*)=.*/\1\2 = <redacted>/' "$CONFIG"

# ─── 4. Launch daemon ────────────────────────────────────────────────
# When MESH_JOIN_LINK is set, we need the daemon up *before* we can run
# `sovereign mesh join` (the CLI talks to the running daemon's data dir).
# So: background the daemon, poll for readiness, fire join, then wait.
# If unset, just exec — preserves the legacy "solo mesh + base_url" path.
if [ -n "$MESH_JOIN_LINK" ]; then
    echo "[entrypoint] launching sovereign-cli daemon (background, will join mesh)"
    sovereign-cli daemon run &
    DAEMON_PID=$!

    # Poll the client port until the daemon answers /v1/models. 2 min cap
    # — slot loads can be slow on cold-start but the HTTP server comes up
    # well before the slots finish loading.
    deadline=$(($(date +%s) + 120))
    until curl -s -m 3 -o /dev/null -w "%{http_code}" http://127.0.0.1:9741/v1/models | grep -q "^200$"; do
        if [ $(date +%s) -ge $deadline ]; then
            echo "[entrypoint] daemon failed to come up within 2 min — proceeding without mesh join"
            break
        fi
        sleep 2
    done

    if curl -s -m 3 -o /dev/null -w "%{http_code}" http://127.0.0.1:9741/v1/models | grep -q "^200$"; then
        echo "[entrypoint] daemon up — joining mesh via invite link"
        if sovereign-cli mesh join "$MESH_JOIN_LINK"; then
            echo "[entrypoint] mesh join succeeded"
        else
            echo "[entrypoint] mesh join FAILED — pod will run in solo mesh; use per-config base_url to reach it"
        fi
    fi

    wait "$DAEMON_PID"
else
    echo "[entrypoint] launching sovereign-cli daemon (no MESH_JOIN_LINK; solo mesh)"
    exec sovereign-cli daemon run
fi
