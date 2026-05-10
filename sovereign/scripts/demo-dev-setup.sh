#!/usr/bin/env bash
# demo-dev-setup.sh — Join an existing Commonwealth cluster as a dev node.
#
# Run this on each developer's machine after the bootstrap node is up.
#
# Usage:
#   JOIN_KEY=<key> BOOTSTRAP_PEER=<ip>:9741 ./scripts/demo-dev-setup.sh
#
# Prerequisites:
#   - sovereign-server binary in PATH (or: cargo build -p sovereign-server --release)
#   - commonwealth-server binary in PATH
#   - .gguf model at $HOME/.sovereign/models/default.gguf (or set SOVEREIGN_MODEL)

set -euo pipefail

SOVEREIGN_CONFIG="${SOVEREIGN_CONFIG:-$HOME/.sovereign/server.toml}"
JOIN_KEY="${JOIN_KEY:?'Set JOIN_KEY to the bootstrap node join key'}"
BOOTSTRAP_PEER="${BOOTSTRAP_PEER:?'Set BOOTSTRAP_PEER to <bootstrap-ip>:9741'}"
SOVEREIGN_MODEL="${SOVEREIGN_MODEL:-$HOME/.sovereign/models/default.gguf}"

echo "=== Sovereign dev node setup ==="
echo "Bootstrap peer: $BOOTSTRAP_PEER"
echo "Join key:       ${JOIN_KEY:0:8}..."
echo

# ── Create Commonwealth config for this node ──────────────────────
COMMONWEALTH_CONFIG="${COMMONWEALTH_CONFIG:-$HOME/.commonwealth/server.toml}"
mkdir -p "$(dirname "$COMMONWEALTH_CONFIG")"
if [[ ! -f "$COMMONWEALTH_CONFIG" ]]; then
  cat > "$COMMONWEALTH_CONFIG" <<EOF
[server]
bind = "0.0.0.0:9741"

[mesh]
join_key = "$JOIN_KEY"
bootstrap_peers = ["$BOOTSTRAP_PEER"]

[store]
path = "$HOME/.commonwealth/store"
EOF
  echo "Created: $COMMONWEALTH_CONFIG"
fi

# ── Create sovereign config ────────────────────────────────────────
mkdir -p "$(dirname "$SOVEREIGN_CONFIG")"
if [[ ! -f "$SOVEREIGN_CONFIG" ]]; then
  cat > "$SOVEREIGN_CONFIG" <<EOF
[server]
bind = "127.0.0.1:8080"

[commonwealth]
url = "http://localhost:9741"

[inference]
model = "$SOVEREIGN_MODEL"

[store]
path = "$HOME/.sovereign/store"
EOF
  echo "Created: $SOVEREIGN_CONFIG"
fi

echo
echo "=== Setup complete ==="
echo
echo "Start services (two terminals):"
echo "  commonwealth-server --config $COMMONWEALTH_CONFIG"
echo "  sovereign-server --config $SOVEREIGN_CONFIG"
echo
echo "Verify mesh connection:"
echo "  curl -s http://localhost:9741/status | jq .mesh"
echo
echo "Seed example notes:"
echo "  ./scripts/demo-seed-notes.sh"
