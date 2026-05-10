#!/usr/bin/env bash
# demo-cluster-bootstrap.sh — Bootstrap a demo Commonwealth + sovereign cluster.
#
# Run this once on the machine that will host the Commonwealth daemon.
# Each team member then runs demo-dev-setup.sh on their own machine.
#
# Usage:
#   ./scripts/demo-cluster-bootstrap.sh [--join-key KEY]
#
# Prerequisites:
#   - commonwealth-server binary in PATH (or built from the commonwealth repo)
#   - sovereign-server binary in PATH (or: cargo build -p sovereign-server --release)
#   - Port 9741 (Commonwealth API) and 9742 (inference) open on LAN

set -euo pipefail

COMMONWEALTH_CONFIG="${COMMONWEALTH_CONFIG:-$HOME/.commonwealth/server.toml}"
SOVEREIGN_CONFIG="${SOVEREIGN_CONFIG:-$HOME/.sovereign/server.toml}"

echo "=== Commonwealth cluster bootstrap ==="
echo

# ── Generate join key ─────────────────────────────────────────────
if [[ -z "${JOIN_KEY:-}" ]]; then
  JOIN_KEY=$(openssl rand -hex 16)
  echo "Generated join key: $JOIN_KEY"
  echo "(share this with teammates so they can run demo-dev-setup.sh)"
  echo
fi

# ── Create Commonwealth config ────────────────────────────────────
mkdir -p "$(dirname "$COMMONWEALTH_CONFIG")"
if [[ ! -f "$COMMONWEALTH_CONFIG" ]]; then
  cat > "$COMMONWEALTH_CONFIG" <<EOF
[server]
bind = "0.0.0.0:9741"

[mesh]
join_key = "$JOIN_KEY"
bootstrap_peers = []

[store]
path = "$HOME/.commonwealth/store"
EOF
  echo "Created: $COMMONWEALTH_CONFIG"
fi

# ── Create sovereign config ────────────────────────────────────────
mkdir -p "$(dirname "$SOVEREIGN_CONFIG")"
if [[ ! -f "$SOVEREIGN_CONFIG" ]]; then
  MACHINE_IP=$(ipconfig getifaddr en0 2>/dev/null || hostname -I | awk '{print $1}')
  cat > "$SOVEREIGN_CONFIG" <<EOF
[server]
bind = "127.0.0.1:8080"

[commonwealth]
url = "http://localhost:9741"

[inference]
model = "$HOME/.sovereign/models/default.gguf"

[store]
path = "$HOME/.sovereign/store"
EOF
  echo "Created: $SOVEREIGN_CONFIG"
  echo "Note: update [inference].model to point to your .gguf file"
fi

echo
echo "=== Bootstrap complete ==="
echo
echo "Start services:"
echo "  commonwealth-server --config $COMMONWEALTH_CONFIG"
echo "  sovereign-server --config $SOVEREIGN_CONFIG"
echo
echo "Share with teammates:"
echo "  JOIN_KEY=$JOIN_KEY BOOTSTRAP_PEER=<your-ip>:9741 ./scripts/demo-dev-setup.sh"
