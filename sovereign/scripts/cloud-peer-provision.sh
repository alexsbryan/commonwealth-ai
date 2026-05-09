#!/usr/bin/env bash
# cloud-peer-provision.sh — one-time bootstrap for cloud-peer deploys.
#
# What this script does (idempotent):
#   1. Verifies rclone is installed (inside this toolbox is fine).
#   2. Verifies the three expected GGUFs exist locally.
#   3. Writes ~/.config/rclone/rclone.conf for an R2 (S3-compat) remote.
#   4. Uploads the GGUFs to r2:${R2_BUCKET}/.  (rclone skips identical
#      files so re-runs are cheap.)
#   5. Verifies the bucket size matches what we expect.
#   6. Prints the env-var block ready to paste into RunPod, plus the
#      remaining manual steps (Tailscale install on host, auth-key
#      generation).
#
# What this script does NOT do:
#   - Create the R2 bucket. You make that in the Cloudflare dashboard
#     and paste its endpoint + keys into the env vars below; the API
#     to create buckets needs the same keys we'd be bootstrapping, so
#     this is a cleaner separation.
#   - Install Tailscale. It runs on the host (not inside this
#     toolbox) so the laptop is reachable from cloud peers as the
#     mesh founder. Host-side install instructions are printed at
#     the end.
#
# Usage:
#   1. In Cloudflare → R2 → Create bucket (e.g. "sovereign-models").
#   2. Cloudflare → R2 → "Manage R2 API Tokens" → create an Object
#      Read & Write token. Copy:
#         - Endpoint URL (https://<account>.r2.cloudflarestorage.com)
#         - Access Key ID
#         - Secret Access Key
#   3. Export them and run the script:
#        export R2_ENDPOINT=https://<account>.r2.cloudflarestorage.com
#        export R2_ACCESS_KEY=...
#        export R2_SECRET_KEY=...
#        export R2_BUCKET=sovereign-models   # optional, default
#        ./scripts/cloud-peer-provision.sh
#
# Re-run this script anytime; it skips already-uploaded GGUFs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_ROOT="$(cd "$REPO_ROOT/.." && pwd)"

R2_BUCKET="${R2_BUCKET:-sovereign-models}"

# Defaults match entrypoint.sh's defaults so the cloud peer finds
# what we uploaded without any GGUF-name env-var overrides on the pod.
PRIMARY_GGUF="${PRIMARY_GGUF:-FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf}"
FAST_GGUF="${FAST_GGUF:-Darwin-9B-Opus.Q8_0.gguf}"
EMBED_GGUF="${EMBED_GGUF:-Qwen3-Embedding-0.6B-Q8_0.gguf}"

# Local source paths. Adjust these if your model layout differs.
PRIMARY_LOCAL="$REPO_ROOT/models/$PRIMARY_GGUF"
FAST_LOCAL="$REPO_ROOT/models/$FAST_GGUF"
EMBED_LOCAL="$WORKSPACE_ROOT/models/qwen-embedding-0.6b.gguf/$EMBED_GGUF"

die()  { echo "cloud-peer-provision: $*" >&2; exit 1; }
warn() { echo "cloud-peer-provision: warning: $*" >&2; }
info() { echo "== $*"; }

# ── 1. rclone present? ────────────────────────────────────────────────
if ! command -v rclone >/dev/null 2>&1; then
    cat >&2 <<'EOF'
rclone not found. Install (inside this toolbox is fine):

    sudo dnf install -y rclone        # Fedora / RHEL toolbox
    sudo apt-get install -y rclone    # Ubuntu / Debian toolbox

Or one-liner from rclone.org:
    curl -fsSL https://rclone.org/install.sh | sudo bash

EOF
    die "rclone missing"
fi
info "rclone: $(rclone --version | head -1)"

# ── 2. local GGUFs present? ──────────────────────────────────────────
for entry in \
    "primary:$PRIMARY_LOCAL" \
    "fast:$FAST_LOCAL" \
    "embed:$EMBED_LOCAL"; do
    role="${entry%%:*}"
    path="${entry#*:}"
    if [[ ! -f "$path" ]]; then
        die "expected $role GGUF missing: $path"
    fi
    size_h=$(du -h "$path" | cut -f1)
    info "$role gguf: $path ($size_h)"
done

# ── 3. R2 credentials present? ────────────────────────────────────────
: "${R2_ENDPOINT:?R2_ENDPOINT required (e.g. https://<account>.r2.cloudflarestorage.com)}"
: "${R2_ACCESS_KEY:?R2_ACCESS_KEY required}"
: "${R2_SECRET_KEY:?R2_SECRET_KEY required}"

info "${R2_ENDPOINT} + ${R2_ACCESS_KEY} + ${R2_SECRET_KEY}"
# ── 4. Write rclone config ────────────────────────────────────────────
RCLONE_CONF="$HOME/.config/rclone/rclone.conf"
mkdir -p "$(dirname "$RCLONE_CONF")"

# Idempotent: only rewrite the [r2] block if it's missing or stale.
write_r2_block=0
if [[ ! -f "$RCLONE_CONF" ]]; then
    write_r2_block=1
elif ! grep -q '^\[r2\]' "$RCLONE_CONF"; then
    write_r2_block=1
elif ! grep -F "endpoint = $R2_ENDPOINT" "$RCLONE_CONF" >/dev/null \
   || ! grep -F "provider = Cloudflare"  "$RCLONE_CONF" >/dev/null \
   || ! grep -F "region = auto"          "$RCLONE_CONF" >/dev/null; then
    warn "rclone.conf has an [r2] block with stale settings — overwriting"
    # Strip the old block (between [r2] and the next [section] or EOF)
    awk '
        /^\[r2\]/      { skip=1; next }
        /^\[/ && skip  { skip=0 }
        !skip          { print }
    ' "$RCLONE_CONF" > "${RCLONE_CONF}.tmp" && mv "${RCLONE_CONF}.tmp" "$RCLONE_CONF"
    write_r2_block=1
fi

if (( write_r2_block )); then
    info "writing [r2] remote to $RCLONE_CONF"
    # provider = Cloudflare (NOT "Other") — gives rclone the SigV4
    # quirks R2 actually wants. With "Other", rclone defaults to
    # us-east-1 for the signing region and R2 401s the request.
    # `region = auto` is also required: R2 only accepts that literal
    # value, not a real AWS region.
    cat >> "$RCLONE_CONF" <<EOF

[r2]
type = s3
provider = Cloudflare
region = auto
endpoint = $R2_ENDPOINT
access_key_id = $R2_ACCESS_KEY
secret_access_key = $R2_SECRET_KEY
acl = private
EOF
fi

# ── 5. Upload (idempotent) ────────────────────────────────────────────
info "uploading GGUFs to r2:$R2_BUCKET (rclone skips already-present files)"

# --transfers=2 keeps the upload polite without throttling — adjust if
# your uplink is fatter. --checkers=4 hashes existing remote files
# so re-runs short-circuit when content is identical.
rclone copy --progress --transfers=2 --checkers=4 \
    "$PRIMARY_LOCAL" "r2:$R2_BUCKET/"
rclone copy --progress --transfers=2 --checkers=4 \
    "$FAST_LOCAL" "r2:$R2_BUCKET/"
rclone copy --progress --transfers=2 --checkers=4 \
    "$EMBED_LOCAL" "r2:$R2_BUCKET/"

# ── 6. Verify ─────────────────────────────────────────────────────────
info "bucket contents:"
rclone ls "r2:$R2_BUCKET" | sed 's/^/    /'
info "bucket size:"
rclone size "r2:$R2_BUCKET" | sed 's/^/    /'

# ── 7. Print next steps ───────────────────────────────────────────────
LAPTOP_TS_IP=""
if command -v tailscale >/dev/null 2>&1; then
    LAPTOP_TS_IP="$(tailscale ip -4 2>/dev/null | head -1 || true)"
fi

cat <<EOF

=================================================================
  R2 staging done. Next: Tailscale (host-side) and pod deploy.
=================================================================

1. Tailscale on the laptop HOST (not inside this toolbox):

      sudo dnf install -y tailscale       # Fedora 43 host
      sudo systemctl enable --now tailscaled
      sudo tailscale up

   Toolboxes share the host's network namespace, so once the host
   joins the tailnet, this toolbox sees the same tailnet IP.

2. Capture the laptop's tailnet IP (after step 1):

      tailscale ip -4
EOF

if [[ -n "$LAPTOP_TS_IP" ]]; then
    cat <<EOF

   (already detected: $LAPTOP_TS_IP)
EOF
fi

cat <<EOF

3. Generate a Tailscale auth key for the cloud peer:

      https://login.tailscale.com/admin/settings/keys
      → "Generate auth key"
      → Reusable: yes; Ephemeral: yes; Tags: optional

   Save the tskey-... string.

4. RunPod env block to paste (fill TS_AUTHKEY + MESH_SEED_ADDR):

      TS_AUTHKEY        tskey-...
      MESH_SEED_ADDR    ${LAPTOP_TS_IP:-<laptop tailnet IP>}:9742
      R2_ENDPOINT       $R2_ENDPOINT
      R2_ACCESS_KEY     $R2_ACCESS_KEY
      R2_SECRET_KEY     <secret>
      R2_BUCKET         $R2_BUCKET
      PRIMARY_COPIES    1
      CONTEXT_SIZE      32768

5. Smoke deploy (cheapest CUDA option — \$0.79/hr):
   See sovereign/docs/CLOUD_PEER_DEPLOY.md → "Deploy on RunPod".
   Recommended first pod: L40S 48GB with PRIMARY_COPIES=1.

EOF
