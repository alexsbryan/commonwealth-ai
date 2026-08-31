#!/usr/bin/env bash
# Terminal-node end-to-end, two daemons on one box.
#
# WHAT THIS PROVES that unit tests cannot. A `terminal` holds no weights and
# binds its entry node by MESH IDENTITY, resolved per turn through
# `PeerEndpointSource`. Nothing about that is observable from a compile: the
# binding is exercised only when a real second daemon is a real mesh peer.
# Run with --encrypt and it also proves the case the feature exists for — an
# encrypted mesh closes its plaintext client API entirely, so the terminal can
# only reach its entry node over the iroh bridge.
#
# WHAT IT CANNOT PROVE. Both daemons are on this host, so every forward is
# on-box. `ServingLocus::ForwardsOffBox` and the off-box `local_only` refusal
# need a second MACHINE and stay unit-tested until one exists
# (`sovereign/docs/specs/MESH_N4_TOPOLOGY.md` §4.5).
#
#   ./scripts/terminal-e2e.sh              # plaintext mesh
#   ./scripts/terminal-e2e.sh --encrypt    # + the iroh path (the real posture)
#
# Needs debug binaries: cargo build --bins --features corpus-engine/treesitter
set -uo pipefail

R="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLI="${CLI:-$R/target/debug/sovereign-cli}"
DMN="${DMN:-$R/target/debug/sovereign-cli-daemon}"
MODEL="${TERMINAL_E2E_MODEL:-$R/sovereign/models/Qwopus3.5-4B-v3-MTP-Q8_0.gguf}"
EMBED="${TERMINAL_E2E_EMBED:-$R/sovereign/models/qwen-embedding-0.6b.gguf}"
HOLDER_PORT="${TERMINAL_E2E_HOLDER_PORT:-9751}"
TERM_PORT="${TERMINAL_E2E_TERM_PORT:-9761}"
ENCRYPT=""
[ "${1:-}" = "--encrypt" ] && ENCRYPT="--encrypt"

SB="$(mktemp -d "${TMPDIR:-/tmp}/terminal-e2e.XXXXXX")"
say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
fail() { printf '\n\033[31mFAIL: %s\033[0m\n' "$*"; exit 1; }
plain() { sed 's/\x1b\[[0-9;]*m//g' "$1"; }   # tracing colours the field NAMES,
                                              # so `key=value` greps miss without this

cleanup() {
  [ -n "${HPID:-}" ] && kill "$HPID" 2>/dev/null
  [ -n "${TPID:-}" ] && kill "$TPID" 2>/dev/null
  sleep 1; kill -9 "${HPID:-0}" "${TPID:-0}" 2>/dev/null
  [ -n "${KEEP_SANDBOX:-}" ] || rm -rf "$SB"
  return 0
}
trap cleanup EXIT

wait_http() { local u=$1 n=$2 i=0
  while [ $i -lt "$n" ]; do curl -sf -m 2 "$u" >/dev/null 2>&1 && return 0; i=$((i+1)); sleep 1; done
  return 1; }

for f in "$CLI" "$DMN" "$MODEL" "$EMBED"; do
  [ -e "$f" ] || fail "missing: $f  (cargo build --bins --features corpus-engine/treesitter)"
done
lsof -nP -iTCP:"$HOLDER_PORT" -sTCP:LISTEN >/dev/null 2>&1 && fail ":$HOLDER_PORT is busy"
lsof -nP -iTCP:"$TERM_PORT"  -sTCP:LISTEN >/dev/null 2>&1 && fail ":$TERM_PORT is busy"

mkdir -p "$SB/holder/.svrnmesh" "$SB/terminal/.svrnmesh"
cat > "$SB/holder/.svrnmesh/config.toml" <<EOF
[models]
primary = "$MODEL"
embed = "$EMBED"
context_size = 8192
[daemon]
client_port = $HOLDER_PORT
internal_port = $((HOLDER_PORT+1))
autostart = false
[data]
dir = "$SB/holder/.svrnmesh"
[iroh]
enabled = true
[discovery]
mdns = true
seed_addrs = []
EOF

say "1. found the mesh BEFORE any daemon boots  (${ENCRYPT:-plaintext})"
# Order is load-bearing: a booting daemon silently auto-founds a PLAINTEXT solo
# mesh, after which `mesh create` refuses ("a mesh already exists") and any
# --encrypt is never reached. Creating first means the daemon resumes into the
# mesh we asked for.
HOME="$SB/holder" "$CLI" mesh create $ENCRYPT >/dev/null 2>&1 \
  || fail "mesh create $ENCRYPT failed"

say "2. holder daemon on :$HOLDER_PORT"
HOME="$SB/holder" "$DMN" daemon run > "$SB/holder.log" 2>&1 &
HPID=$!
wait_http "http://127.0.0.1:$HOLDER_PORT/status" 240 \
  || { tail -30 "$SB/holder.log"; fail "holder never answered /status"; }
# The FULL 32-hex id, from the persisted file. NOT `/status.node_id`, which is
# the truncated Display form (`node-a65f8cfeb45ac139`) that the binding
# correctly refuses. The product path never types this — `setup --terminal`
# reads `peer.node_id.to_hex()` off the mesh — but a script has to.
# Exported: step 8 reads it inside a python3 that sits on the RIGHT of a pipe,
# and an assignment prefix binds only the FIRST command of a pipeline.
export HOLDER_ID
HOLDER_ID=$(xxd -p "$SB/holder/.svrnmesh/node_id" | tr -d '\n')
[ ${#HOLDER_ID} -eq 32 ] || fail "holder node_id is ${#HOLDER_ID} chars, expected 32"
echo "   holder $HOLDER_ID"

LINK=$(HOME="$SB/holder" "$CLI" mesh status 2>&1 | grep -oE 'sovereign://join/[^ ]+' | head -1)
[ -n "$LINK" ] || fail "no join link printed by 'mesh status'"
if [ -n "$ENCRYPT" ]; then
  plain "$SB/holder.log" | grep -q "require_encryption=true" \
    || fail "--encrypt asked for, but the holder never logged require_encryption=true"
  plain "$SB/holder.log" | grep -q 'required=\[.*"inference"' \
    || fail "inference is not an iroh-REQUIRED class — a plaintext fallback still exists"
  echo "$LINK" | grep -q "iroh=" || fail "encrypted invite must carry iroh="
  echo "   posture: require_encryption=true, inference iroh-REQUIRED"
fi

say "3. terminal config — IDENTITY binding, no address"
cat > "$SB/terminal/.svrnmesh/config.toml" <<EOF
[node]
entry_node = "$HOLDER_ID"
entry_embed_model = "$(basename "$EMBED" .gguf)"
[daemon]
client_port = $TERM_PORT
internal_port = $((TERM_PORT+1))
autostart = false
[data]
dir = "$SB/terminal/.svrnmesh"
[iroh]
enabled = true
[discovery]
mdns = true
seed_addrs = []
EOF
grep -q '^entry =' "$SB/terminal/.svrnmesh/config.toml" && fail "config carries an address binding"

say "4. terminal joins"
HOME="$SB/terminal" "$CLI" mesh join "$LINK" 2>&1 | tail -4

say "5. terminal daemon on :$TERM_PORT (holds NOTHING)"
HOME="$SB/terminal" RUST_LOG=info,transport=debug \
  "$DMN" daemon run > "$SB/terminal.log" 2>&1 &
TPID=$!
wait_http "http://127.0.0.1:$TERM_PORT/status" 180 \
  || { tail -40 "$SB/terminal.log"; fail "terminal never answered /status"; }

say "6. it advertises NOTHING to peers"
ADV=$(curl -s "http://127.0.0.1:$TERM_PORT/oicp/v1/capabilities" | python3 -c '
import sys,json;print(json.dumps([m.get("id") for m in json.load(sys.stdin).get("models",[])]))')
echo "   /oicp/v1/capabilities models = $ADV"
[ "$ADV" = "[]" ] || fail "a node holding nothing advertised $ADV — peers would route real work to it"

say "7. …but local clients still see the mesh alias"
curl -s "http://127.0.0.1:$TERM_PORT/v1/models" | python3 -c '
import sys,json
ids=[m["id"] for m in json.load(sys.stdin).get("data",[])]
print("   ",ids); raise SystemExit(0 if "primary" in ids else 1)' \
  || fail "a joined terminal must surface 'primary' to its own clients"

say "8. /v1/mesh/status names the class and the binding"
curl -s "http://127.0.0.1:$TERM_PORT/v1/mesh/status" | python3 -c '
import sys,json,os
d=json.load(sys.stdin); cls=d.get("node_class"); entry=str(d.get("entry_node"))
print("   node_class:",cls); print("   entry_node:",entry)
raise SystemExit(0 if cls=="terminal" and os.environ["HOLDER_ID"] in entry else 1)' \
  || fail "/v1/mesh/status must report node_class=terminal and name the bound node"

say "9. a real turn"
curl -s -m 300 -X POST "http://127.0.0.1:$TERM_PORT/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d '{"model":"primary","messages":[{"role":"user","content":"Reply with the single word: ready"}],"max_tokens":16,"stream":false}' \
  | python3 -c '
import sys,json
d=json.load(sys.stdin)
if "error" in d: print("   ERROR:",json.dumps(d["error"])[:300]); raise SystemExit(1)
c=d.get("choices",[{}])[0].get("message",{}).get("content","")
print("   served by:",d.get("model")); print("   content:  ",repr(c[:100]))
raise SystemExit(0 if c.strip() else 1)' || fail "no turn came back served"

say "10. an embedding — the path that MUST use the binding"
# Chat on a JOINED terminal is resolved by the mesh scheduler from the holder's
# advertised manifest (`provider_for_peer`) and never touches
# `EntryNodeEndpoint`. Embeddings have no such path: the daemon's embed fn IS
# the provider, and on a terminal that provider is the resolved binding. This
# step is what exercises the code the binding added.
curl -s -m 120 -X POST "http://127.0.0.1:$TERM_PORT/v1/embeddings" \
  -H 'content-type: application/json' \
  -d '{"model":"embed","input":"terminal embedding through the resolved entry node"}' \
  | python3 -c '
import sys,json
d=json.load(sys.stdin)
if "error" in d: print("   ERROR:",json.dumps(d["error"])[:300]); raise SystemExit(1)
v=d.get("data",[{}])[0].get("embedding",[])
print("   dims:",len(v)); raise SystemExit(0 if v else 1)' \
  || fail "no embedding came back through the entry binding"

say "11. glassbox: which transport resolved it?"
RESOLVED=$(plain "$SB/terminal.log" | grep -oE 'endpoint=http://[^ ]+' | tail -1)
plain "$SB/terminal.log" | grep -E "terminal: resolved entry node" | tail -1
echo "   resolved: ${RESOLVED:-<none logged>}"
if [ -n "$ENCRYPT" ]; then
  case "$RESOLVED" in
    *":$HOLDER_PORT"*) fail "resolved to the holder's plaintext :$HOLDER_PORT on an ENCRYPTED mesh — iroh was not used" ;;
    *127.0.0.1*)       echo "   -> iroh bridge on loopback: the encrypted path was used" ;;
    "")                fail "nothing resolved was logged — cannot say which transport served it" ;;
    *)                 echo "   -> NOTE: unexpected endpoint shape, inspect" ;;
  esac
fi

say "PASSED  (co-located; ForwardsOffBox still needs a second machine)"
