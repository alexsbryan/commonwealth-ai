#!/bin/bash
# Tailscale-served alternative to entrypoint.sh.
#
# Same shape as the R2 entrypoint, but pulls GGUFs from a small HTTP
# server running on the laptop (mesh founder) over the tailnet
# instead of from S3. Useful when you want to skip object-store
# infrastructure and serve directly from the laptop you already have.
#
# Trade-off vs entrypoint.sh: cold-start GGUF download is bound by
# your laptop's upstream bandwidth (residential ~30-100 Mbps means
# 1-2 hrs to pull 50 GB). R2 hits ~500 MB/s. For ad-hoc one-off pods
# where the laptop is the only mesh founder anyway, this is fine and
# saves the R2 credential dance.
#
# Required env:
#   TS_AUTHKEY        Tailscale auth key (reusable, ephemeral).
#   MESH_SEED_ADDR    host:port of the founder daemon's internal port.
#                     e.g. 100.64.0.2:9742.  Used unchanged for mesh.
#                     The host portion is also used as the default
#                     GGUF source — see MODEL_SERVE_HOST below.
#
# Optional env:
#   MODEL_SERVE_HOST  Host serving GGUFs. Defaults to the host part
#                     of MESH_SEED_ADDR (laptop). Override if your
#                     laptop's GGUFs live on a different machine in
#                     the tailnet.
#   MODEL_SERVE_PORT  Port the laptop's HTTP server is bound to.
#                     Default 9743 (matches scripts/cloud-peer-serve-models.sh).
#   PRIMARY_GGUF      Filename of the primary GGUF on the source.
#                     Default: FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf
#   FAST_GGUF         Filename of the fast GGUF.
#                     Default: Darwin-9B-Opus.Q8_0.gguf
#   EMBED_GGUF        Filename of the embedding GGUF.
#                     Default: Qwen3-Embedding-0.6B-Q8_0.gguf
#   PRIMARY_COPIES    Number of primary slot copies to load.
#                     Default 1.
#   CONTEXT_SIZE      n_ctx for all loaded slots. Default 32768.
#   NODE_ROLE         Mesh node role tag. Default: ephemeral-worker.
set -euo pipefail

: "${TS_AUTHKEY:?TS_AUTHKEY is required}"
: "${MESH_SEED_ADDR:?MESH_SEED_ADDR is required (host:port of the founder daemon internal port)}"

PRIMARY_COPIES="${PRIMARY_COPIES:-1}"
CONTEXT_SIZE="${CONTEXT_SIZE:-32768}"
PRIMARY_GGUF="${PRIMARY_GGUF:-FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf}"
FAST_GGUF="${FAST_GGUF:-Darwin-9B-Opus.Q8_0.gguf}"
EMBED_GGUF="${EMBED_GGUF:-Qwen3-Embedding-0.6B-Q8_0.gguf}"
NODE_ROLE="${NODE_ROLE:-ephemeral-worker}"

# Default model source = host portion of MESH_SEED_ADDR. Strip :port.
MODEL_SERVE_HOST="${MODEL_SERVE_HOST:-${MESH_SEED_ADDR%:*}}"
MODEL_SERVE_PORT="${MODEL_SERVE_PORT:-9743}"
MODEL_SERVE_URL="http://${MODEL_SERVE_HOST}:${MODEL_SERVE_PORT}"

MODELS_DIR="${SOVEREIGN_MODELS_DIR:-/workspace/models}"
DATA_DIR="${SOVEREIGN_DATA_DIR:-/workspace/data}"
mkdir -p "$MODELS_DIR" "$DATA_DIR"

# ─── 1. Tailscale ────────────────────────────────────────────────────
echo "[entrypoint-tailscale] starting tailscaled…"
tailscaled --tun=userspace-networking --state=/var/lib/tailscale/tailscaled.state \
    --socket=/var/run/tailscale/tailscaled.sock &
TS_PID=$!
sleep 2
echo "[entrypoint-tailscale] joining tailnet (hostname=sovereign-${HOSTNAME})"
tailscale up \
    --authkey="$TS_AUTHKEY" \
    --hostname="sovereign-${HOSTNAME}" \
    --accept-dns=false
echo "[entrypoint-tailscale] tailnet IP: $(tailscale ip -4 || true)"

# ─── 2. Fetch GGUFs from the laptop over Tailscale ───────────────────
echo "[entrypoint-tailscale] source: $MODEL_SERVE_URL"

# Skip-if-present: HEAD the remote file, compare Content-Length to
# the local file size. This catches the common "previous pod cold-start
# was killed mid-download" case (partial file → re-fetch) while making
# the warm-restart case free.
fetch_gguf() {
    local fname="$1"
    local local_path="$MODELS_DIR/$fname"
    local remote_url="$MODEL_SERVE_URL/$fname"

    local remote_size
    remote_size=$(curl -fsSI "$remote_url" \
        | awk -v IGNORECASE=1 '/^content-length:/ {gsub("\r",""); print $2}')
    if [[ -z "$remote_size" ]]; then
        echo "[entrypoint-tailscale] FATAL: HEAD $remote_url returned no Content-Length"
        echo "[entrypoint-tailscale]   is the laptop's serve script running? check:"
        echo "[entrypoint-tailscale]     scripts/cloud-peer-serve-models.sh"
        exit 1
    fi

    if [[ -f "$local_path" ]]; then
        local local_size
        local_size=$(stat -c '%s' "$local_path")
        if [[ "$local_size" == "$remote_size" ]]; then
            echo "[entrypoint-tailscale] $fname: already present ($local_size bytes), skipping"
            return 0
        else
            echo "[entrypoint-tailscale] $fname: size mismatch (local=$local_size remote=$remote_size); re-fetching"
            rm -f "$local_path"
        fi
    fi

    echo "[entrypoint-tailscale] $fname: downloading ($remote_size bytes)"
    # --retry 3 + --retry-connrefused: laptop may briefly drop during
    # tailnet handshake races. Resume on partial via --continue-at.
    curl --fail --location --retry 3 --retry-connrefused \
         --output "$local_path" \
         "$remote_url"
}

fetch_gguf "$PRIMARY_GGUF"
fetch_gguf "$FAST_GGUF"
fetch_gguf "$EMBED_GGUF"

# Sanity: required files present?
for f in "$PRIMARY_GGUF" "$FAST_GGUF" "$EMBED_GGUF"; do
  if [ ! -f "$MODELS_DIR/$f" ]; then
    echo "[entrypoint-tailscale] FATAL: $MODELS_DIR/$f missing after fetch"
    exit 1
  fi
done

# ─── 3. config.toml ──────────────────────────────────────────────────
CONFIG=/root/.config/sovereign/config.toml
mkdir -p "$(dirname "$CONFIG")"

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

echo "[entrypoint-tailscale] config written:"
sed -E 's/(authkey)([^=]*)=.*/\1\2 = <redacted>/' "$CONFIG"

# ─── 4. Launch daemon ────────────────────────────────────────────────
echo "[entrypoint-tailscale] launching sovereign-cli daemon"
exec sovereign-cli daemon run
