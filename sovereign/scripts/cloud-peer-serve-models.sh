#!/usr/bin/env bash
# cloud-peer-serve-models.sh — laptop-side companion to
# container/entrypoint-tailscale.sh.
#
# Starts a small HTTP server on the laptop's tailnet IP that the
# cloud pod fetches GGUFs from on cold-start. Runs in the foreground;
# Ctrl-C to stop. Bind is restricted to the tailnet interface so the
# server isn't reachable from the public internet — Tailscale ACLs
# are the access boundary.
#
# Usage (on the laptop, before spinning up a pod):
#     ./scripts/cloud-peer-serve-models.sh           # binds tailnet IP, port 9743
#     ./scripts/cloud-peer-serve-models.sh --port 9999
#     ./scripts/cloud-peer-serve-models.sh --bind 0.0.0.0     # all interfaces (NOT recommended)
#
# The pod's entrypoint-tailscale.sh defaults to:
#     MODEL_SERVE_HOST = host part of MESH_SEED_ADDR  (= laptop's tailnet IP)
#     MODEL_SERVE_PORT = 9743
# so a `cloud-peer-serve-models.sh` with no arguments matches.
#
# Security model: relies on Tailscale ACLs. Set up a tag like
# `tag:cloud-peer` for pods and limit which tags can reach
# `tag:laptop:9743`. The HTTP server has no auth — Tailscale is the auth.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_ROOT="$(cd "$REPO_ROOT/.." && pwd)"

PORT=9743
BIND=""
SERVE_DIR=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --port)  PORT="$2"; shift 2 ;;
        --bind)  BIND="$2"; shift 2 ;;
        --dir)   SERVE_DIR="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,/^set -/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;/^set -/d'
            exit 0 ;;
        *) echo "unknown flag: $1 (see --help)" >&2; exit 1 ;;
    esac
done

# Resolve bind address.
if [[ -z "$BIND" ]]; then
    if ! command -v tailscale >/dev/null 2>&1; then
        echo "tailscale not found on PATH. Install on the host (not in toolbox):" >&2
        echo "    sudo dnf install -y tailscale" >&2
        echo "    sudo systemctl enable --now tailscaled" >&2
        echo "    sudo tailscale up" >&2
        exit 1
    fi
    BIND="$(tailscale ip -4 2>/dev/null | head -1 || true)"
    if [[ -z "$BIND" ]]; then
        echo "could not detect tailnet IP. Is tailscaled running?" >&2
        exit 1
    fi
fi

# Resolve serve directory: ${WORKSPACE_ROOT} contains both
# sovereign/models/ and models/qwen-embedding-0.6b.gguf/. Building a
# unified view requires either a chosen dir + symlinks, or two
# servers. We pick the simple path: a temp staging dir with symlinks
# to whichever GGUFs we want exposed, then serve that.
if [[ -z "$SERVE_DIR" ]]; then
    SERVE_DIR="$(mktemp -d -t sovereign-models-serve.XXXX)"
    trap 'rm -rf "$SERVE_DIR"' EXIT

    PRIMARY_GGUF="${PRIMARY_GGUF:-FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf}"
    FAST_GGUF="${FAST_GGUF:-Darwin-9B-Opus.Q8_0.gguf}"
    EMBED_GGUF="${EMBED_GGUF:-Qwen3-Embedding-0.6B-Q8_0.gguf}"

    PRIMARY_LOCAL="$REPO_ROOT/models/$PRIMARY_GGUF"
    FAST_LOCAL="$REPO_ROOT/models/$FAST_GGUF"
    EMBED_LOCAL="$WORKSPACE_ROOT/models/qwen-embedding-0.6b.gguf/$EMBED_GGUF"

    for entry in \
        "$PRIMARY_LOCAL" \
        "$FAST_LOCAL" \
        "$EMBED_LOCAL"; do
        if [[ ! -f "$entry" ]]; then
            echo "missing local GGUF: $entry" >&2
            exit 1
        fi
        ln -s "$entry" "$SERVE_DIR/$(basename "$entry")"
    done
fi

cat <<EOF
== sovereign cloud-peer model server ==
  bind:        $BIND:$PORT
  serve dir:   $SERVE_DIR
  contents:
$(ls -lh "$SERVE_DIR" | sed 's/^/    /')

  pod env hint:
    MODEL_SERVE_HOST=$BIND
    MODEL_SERVE_PORT=$PORT

  Ctrl-C to stop.
EOF

# python3's stdlib server is fine for Tailscale-internal traffic; it
# saturates a typical residential uplink and supports HTTP HEAD which
# the pod uses for skip-if-present.
cd "$SERVE_DIR"
exec python3 -m http.server "$PORT" --bind "$BIND"
