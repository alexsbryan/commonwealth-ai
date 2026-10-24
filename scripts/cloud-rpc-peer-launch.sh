#!/usr/bin/env bash
# cloud-rpc-peer-launch.sh — run ON the Vast pod (inside the sovereign-cuda
# container) to lend its GPU to a head's llama-server as a ggml-RPC peer.
#
# Usage:  PEER_BIND='<pod tunnel ip>:50052' bash /workspace/cloud-rpc-peer-launch.sh
#
# What it does, in order:
#   1. cuda-preflight            — GPU sanity in ~1s, before any download
#   2. fetch a 0.5 GB stub primary (daemon refuses to boot without one;
#      the peer child itself never touches it)
#   3. daemon run with `[compute] rpc_peer = PEER_BIND`: the daemon runs the
#      shipped `ggml-rpc-server -c` there as a supervised child, its tensor
#      cache under /workspace/svrnmesh/rpc-peer. Raw ggml-RPC authenticates
#      and encrypts nothing, so PEER_BIND is the pod's TUNNEL address
#      (WireGuard, Tailscale), never a public one; the daemon refuses loopback
#      and 0.0.0.0. The head names the same address in its own config.
set -euo pipefail
: "${PEER_BIND:?set PEER_BIND to the pod tunnel ip:port the head will dial}"

mkdir -p /workspace/models /workspace/svrnmesh /root/.svrnmesh

cuda-preflight

STUB=/workspace/models/Qwen3.5-0.8B-Q4_K_M.gguf
[ -f "$STUB" ] || curl -fL --retry 3 -o "$STUB" \
  'https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf'

CFG=/root/.svrnmesh/config.toml
# `embed` is REQUIRED by the config schema (daemon refuses to parse without
# it); the peer never runs embeddings, so the stub doubles for both. The data
# dir is on the volume, so the peer's tensor cache outlives the container.
[ -f "$CFG" ] || cat > "$CFG" <<EOF
[models]
primary = "$STUB"
embed = "$STUB"
context_size = 4096

[data]
dir = "/workspace/svrnmesh"

[compute]
rpc_peer = "$PEER_BIND"
EOF

# Deliberately no RUST_LOG override — it would blind the peer child's logs.
exec sovereign-cli daemon run
