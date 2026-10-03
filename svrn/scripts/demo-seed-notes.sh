#!/usr/bin/env bash
# demo-seed-notes.sh — Seed the working notes database with example entries.
#
# Run this after sovereign-server is up to pre-populate the notes store
# with architecture decisions and invariants that demonstrate the notes
# attribution feature.
#
# Usage:
#   ./scripts/demo-seed-notes.sh [--server http://localhost:8080]

set -euo pipefail

SERVER="${1:-http://localhost:8080}"
MCP_URL="$SERVER/mcp/message"

rpc() {
  local method="$1"
  local params="$2"
  curl -s -X POST "$MCP_URL" \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}"
}

write_note() {
  local kind="$1" title="$2" body="$3"
  rpc "tools/call" "{
    \"name\": \"write_note\",
    \"arguments\": {
      \"kind\": \"$kind\",
      \"title\": \"$title\",
      \"body\": \"$body\"
    }
  }"
}

echo "=== Seeding sovereign notes ==="
echo "Server: $SERVER"
echo

write_note "decision" \
  "ActivityCallback trait lives in corpus-engine" \
  "The ActivityCallback trait is defined in corpus-engine (not sovereign-server) to keep the dependency direction correct. corpus-engine must not depend on sovereign-server. sovereign-server's ActivityReporter implements the trait and is passed as Arc<dyn ActivityCallback> to WatcherCoordinator."

echo "  ✓ decision: ActivityCallback trait placement"

write_note "invariant" \
  "inference_availability defaults to 1.0 for old peers" \
  "NodeCapabilities.inference_availability uses #[serde(default = \"default_inference_availability\")] returning 1.0. Old peers that don't include this field in gossip JSON deserialize to 1.0 (fully available). Never remove this default — it's a backwards-compat invariant."

echo "  ✓ invariant: inference_availability serde default"

write_note "decision" \
  "Hot availability floor is 0.20 not 0.0" \
  "The availability clamp floor is 0.20 (not 0.0) so even the busiest node remains reachable for requests that have no better option. Without the floor, a fully-hot node would score 0 and never receive any requests, breaking fallback scenarios where all peers are busy."

echo "  ✓ decision: availability floor rationale"

write_note "invariant" \
  "Session IDs encode username for cross-session attribution" \
  "Session IDs are formatted as {slug}-{YYYY-MM-DDTHH:MM}-{uuid6}. The slug is derived from clientInfo.name or meta.userName in the MCP initialize params. This encoding allows read_notes to display 'added by alice, 2h ago' without a separate user registry."

echo "  ✓ invariant: session ID format"

write_note "todo" \
  "Wire ActivityReporter to WatcherCoordinator in sovereign-server" \
  "The activity.rs TODO comment says: pass reporter to WatcherCoordinator when watcher support is added. main.rs creates the reporter but doesn't yet pass it to the corpus_engine watcher. Add: coordinator.with_activity(Arc::clone(&reporter)) once the server initializes a WatcherCoordinator."

echo "  ✓ todo: WatcherCoordinator wiring"

echo
echo "=== Notes seeded. Verify with: ==="
echo "  curl -s -X POST $MCP_URL \\"
echo "    -H 'Content-Type: application/json' \\"
echo "    -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"read_notes\",\"arguments\":{\"query\":\"activity\"}}}'"
