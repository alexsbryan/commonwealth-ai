#!/usr/bin/env bash
# cloud-rpc-peer-launch.sh — run ON the Vast pod (inside the sovereign-cuda
# container) to turn it into a mesh-member ggml-RPC tensor worker.
#
# Usage:  DIAL_LINK='<full join link with dial=>' bash /workspace/cloud-rpc-peer-launch.sh
#
# What it does, in order:
#   1. cuda-preflight            — GPU sanity in ~1s, before any download
#   2. fetch a 0.5 GB stub primary (daemon refuses to boot without one;
#      the RPC worker itself never touches it)
#   3. sovereign-cli mesh join   — joins the operator's mesh over iroh
#   4. daemon run with SOVEREIGN_RPC_SERVE on LOOPBACK — raw ggml-RPC is
#      unencrypted, so it must never bind a public interface; the iroh
#      acceptor bridges ALPN cwth/rpc/0 to this port and all tensor
#      traffic crosses the WAN encrypted.
set -euo pipefail
: "${DIAL_LINK:?set DIAL_LINK to the full mesh join link (svrn mesh rotate output on the host)}"

mkdir -p /workspace/models /workspace/rpc-cache /root/.svrnmesh /root/.local/share

# CLI mesh verbs persist mesh.json via mesh_data_dir() (XDG,
# ~/.local/share/svrnmesh — shared with the desktop app) while the daemon
# persists via svrnmesh_root() (~/.svrnmesh). Unify them or the join the CLI
# writes is invisible to the daemon (split-brain observed on RuggedFox
# 2026-07-27: `mesh rotate` minted keys the daemon never loaded).
[ -e /root/.local/share/svrnmesh ] || ln -s /root/.svrnmesh /root/.local/share/svrnmesh

cuda-preflight

STUB=/workspace/models/Qwen3.5-0.8B-Q4_K_M.gguf
[ -f "$STUB" ] || curl -fL --retry 3 -o "$STUB" \
  'https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf'

CFG=/root/.svrnmesh/config.toml
# `embed` is REQUIRED by the config schema (daemon refuses to parse without
# it); the RPC worker never runs embeddings, so the stub doubles for both.
[ -f "$CFG" ] || cat > "$CFG" <<EOF
[models]
primary = "$STUB"
embed = "$STUB"
context_size = 4096
EOF

sovereign-cli mesh join "$DIAL_LINK"

export SOVEREIGN_RPC_SERVE=127.0.0.1:50052
export SOVEREIGN_RPC_CACHE_DIR=/workspace/rpc-cache
# Deliberately no RUST_LOG override — it would blind placement/warm logs.

exec sovereign-cli daemon run
