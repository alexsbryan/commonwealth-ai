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

# ─── CUDA preflight (fail-fast on driver/NCCL incompat) ──────────────
# Run a tiny native binary that mirrors llama.cpp's `ggml_cuda_init`
# path. If it fails, the daemon would fail the same way ~5 minutes
# later (after the GGUF rclone sync). Catching the failure here saves
# both the wall-time and the prepaid bandwidth on bad-driver hosts
# (common on Vast.ai marketplaces where host driver versions vary
# widely). ROCm pods don't have this binary in the image; the test
# is silently skipped.
if [[ -x /usr/local/bin/cuda-preflight ]]; then
    echo "[entrypoint] CUDA preflight..."
    if ! /usr/local/bin/cuda-preflight; then
        echo "[entrypoint] FATAL: CUDA preflight failed."
        echo "  This host's NVIDIA driver / CUDA runtime / NCCL combo is incompatible"
        echo "  with the daemon. Continuing would burn the GGUF sync time only to crash"
        echo "  at slot load. Bail out so the orchestrator can pick a different offer."
        exit 1
    fi
fi

# ─── 1. Tailscale ────────────────────────────────────────────────────
# Tailscale provides TWO outbound proxy listeners on its userspace
# netstack:
#   --socks5-server     SOCKS5 (used by curl --socks5-hostname, etc.)
#   --outbound-http-proxy-listen  HTTP CONNECT proxy (used by reqwest
#                                  via HTTP_PROXY/HTTPS_PROXY env).
#
# Both route traffic through tailscale's DERP fallback when direct
# WireGuard NAT-traversal silently drops packets (Vast.ai container
# NAT exhibits this). We expose BOTH because reqwest as built (no
# `socks` cargo feature) treats `socks5h://` URLs as if they were
# HTTP proxies and sends a CONNECT to the SOCKS port — which
# tailscale's SOCKS5 server (correctly) rejects with "incompatible
# SOCKS version". Giving reqwest a real HTTP CONNECT proxy lets it
# tunnel without needing the socks feature.
echo "[entrypoint] starting tailscaled…"
tailscaled --tun=userspace-networking \
    --state=/var/lib/tailscale/tailscaled.state \
    --socket=/var/run/tailscale/tailscaled.sock \
    --socks5-server=localhost:1055 \
    --outbound-http-proxy-listen=localhost:1080 &
TS_PID=$!
sleep 2
echo "[entrypoint] joining tailnet (hostname=sovereign-${HOSTNAME})"
tailscale up \
    --authkey="$TS_AUTHKEY" \
    --hostname="sovereign-${HOSTNAME}" \
    --accept-dns=false
echo "[entrypoint] tailnet IP: $(tailscale ip -4 || true)"

# HTTP_PROXY/HTTPS_PROXY: reqwest auto-detects these and sends valid
# HTTP CONNECT requests to the HTTP proxy port. Works without the
# `socks` feature flag on reqwest.
export HTTP_PROXY="http://localhost:1080"
export HTTPS_PROXY="http://localhost:1080"
# ALL_PROXY: kept on SOCKS5 for curl-style clients (the beacon below
# uses --socks5-hostname explicitly, but ALL_PROXY is a curl-ecosystem
# convention worth preserving for rclone and others).
export ALL_PROXY="socks5h://localhost:1055"
# Don't proxy LOCAL traffic (127.0.0.1) — sovereign-cli talks to its
# own daemon on loopback during mesh-join; that must NOT go via the
# proxy. Add the daemon's bind hosts so internal lookups stay direct.
export NO_PROXY="localhost,127.0.0.1,0.0.0.0"

# ─── Tailnet reachability beacon ─────────────────────────────────────
# Confirm we can reach the mesh seed (founder's :9742) over the
# tailnet before sinking 5+ min into the GGUF sync. Catches: TS_AUTHKEY
# revoked/exhausted, Tailscale ACL blocking tag:cloud-peer → laptop,
# founder's daemon down or bound to a different port, and stale
# MESH_SEED_ADDR (pointing at an IP no longer in the tailnet).
#
# Retries for ~60s with 5s spacing — `tailscale up` returns as soon as
# auth completes, but the DERP relay handshake and the per-peer
# WireGuard path establishment take an additional ~10–20s. Without the
# retry, a beacon firing in the first second of `tailscale up` failure
# is a false negative: tailscale's just still warming up.
seed_host="${MESH_SEED_ADDR%:*}"
seed_port="${MESH_SEED_ADDR#*:}"
echo "[entrypoint] tailnet reach beacon: ${seed_host}:${seed_port} (via SOCKS5)..."
beacon_ok=""
for attempt in $(seq 1 12); do
    # curl with --socks5-hostname routes through tailscale's userspace
    # netstack, which falls back to DERP if direct WG is broken.
    # `-o /dev/null` discards body, `-w` captures the http code only.
    code=$(curl --socks5-hostname localhost:1055 -s -m 5 -o /dev/null \
        -w "%{http_code}" "http://${seed_host}:${seed_port}/" 2>/dev/null || true)
    # ANY response (even 404 or 405) proves the path works — the
    # founder's daemon may not have a handler at GET / but reaching it
    # is what we're verifying. "000" = no TCP-level connection.
    if [ -n "$code" ] && [ "$code" != "000" ]; then
        beacon_ok="$attempt"
        break
    fi
    sleep 5
done
if [ -z "$beacon_ok" ]; then
    echo "[entrypoint] FATAL: cannot reach mesh seed ${MESH_SEED_ADDR} over the tailnet (12 attempts × 5s + 5s timeout each)."
    echo "  Likely causes:"
    echo "    - TS_AUTHKEY expired / revoked / single-use already consumed"
    echo "    - Tailscale ACL doesn't allow tag:cloud-peer → ${seed_host}:${seed_port}"
    echo "    - Founder's daemon isn't running, or is bound to a different port"
    echo "    - MESH_SEED_ADDR is stale (founder's tailnet IP changed)"
    echo "    - WireGuard NAT-traversal failed (rare; would need DERP relay)"
    echo "  Bail out before the 38 GB GGUF sync."
    exit 1
fi
echo "[entrypoint] mesh seed reachable (attempt ${beacon_ok})"

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

# Single self-test: list the target bucket directly. This is the
# minimum permission a scoped R2 token (Object Read on a specific
# bucket) is granted, which is the security-correct config — no
# need for account-level ListBuckets. The previous two-step probe
# called `rclone lsd r2:` first, which DID require ListBuckets and
# rejected scoped tokens with 403, masking otherwise-fine bucket
# access. A single bucket-scoped probe diagnoses both auth and
# bucket-access failures with one call.
echo "[entrypoint] R2 self-test: list target bucket"
if ! rclone lsf "r2:${R2_BUCKET}" 2>&1; then
    echo "[entrypoint] FATAL: 'rclone lsf r2:${R2_BUCKET}' failed."
    echo "  Possible causes:"
    echo "    - R2_ENDPOINT is the wrong host (need https://<account>.r2.cloudflarestorage.com)"
    echo "    - R2_ACCESS_KEY / R2_SECRET_KEY are wrong, swapped, or include whitespace"
    echo "    - Bucket name typo (R2_BUCKET=${R2_BUCKET} doesn't exist)"
    echo "    - Token isn't scoped to this bucket OR is missing 'Object Read' permission"
    echo "    - region = auto wasn't applied (should be in this image now; rebuild if not)"
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
        # Synthesize a full sovereign://join URL when MESH_JOIN_LINK is
        # the bare key (cwth-XXXX-XXXX-XXXX). The bare-key form drops
        # `relay_hint` and the daemon falls through to mDNS discovery,
        # which doesn't traverse Tailscale (pod and founder are on
        # different L2 segments connected only by overlay). Using
        # MESH_SEED_ADDR as the relay tells the join handshake to
        # speak directly to the founder's :9742 instead of broadcasting.
        # Already-formed URLs (sovereign://join/... or https://...) are
        # passed through unchanged.
        join_arg="$MESH_JOIN_LINK"
        if [[ "$join_arg" =~ ^cwth- ]]; then
            join_arg="sovereign://join/${join_arg}?relay=${MESH_SEED_ADDR}"
            echo "[entrypoint] synthesizing join URL with relay=${MESH_SEED_ADDR}"
        fi
        echo "[entrypoint] daemon up — joining mesh"
        if sovereign-cli mesh join "$join_arg"; then
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
