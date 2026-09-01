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
# THE LOCALITY BAR, added 2026-08-31 after F6. A THIRD daemon — the decoy —
# advertises the same `primary` alias, so the terminal has a CHOICE between the
# node it was bound to and another advertiser. Until this existed, "nearest
# advertiser" and "bound node" were the same node here, and the harness passed
# whether chat honoured the binding or ignored it. F6 shipped through it:
# a terminal bound to MAC served chat from a nearer peer while its embeddings
# went to MAC, and no co-located run could have caught that.
#
# Run with --no-decoy to skip it. Do that only to save the decoy's model load —
# it is ON by default on purpose, because a bar that is opt-in is a bar that
# does not run.
#
# WHAT IT CANNOT PROVE. Both daemons are on this host, so every forward is
# on-box. `ServingLocus::ForwardsOffBox` and the off-box `local_only` refusal
# need a second MACHINE and stay unit-tested until one exists
# (`sovereign/docs/specs/MESH_N4_TOPOLOGY.md` §4.5). Note the PEER TALLY is not
# in that set: `/status.inference.peer_requests` is keyed on the `X-Node-Id`
# header, not on locality, so it is fully checkable on one box — the tn-2 order
# assumed otherwise and steps 13/14 below are the counter-example.
#
#   ./scripts/terminal-e2e.sh              # plaintext mesh
#   ./scripts/terminal-e2e.sh --encrypt    # + the iroh path (the real posture)
#   ./scripts/terminal-e2e.sh --no-decoy   # skip the second advertiser
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
DECOY_PORT="${TERMINAL_E2E_DECOY_PORT:-9781}"
# A SMALL model on purpose. The decoy only has to ADVERTISE `primary` to give
# the terminal a choice; it must never serve, and if it does that is the
# failure this harness exists to catch. Loading the holder's 4B twice would
# double the memory cost to prove nothing extra.
DECOY_MODEL="${TERMINAL_E2E_DECOY_MODEL:-$R/sovereign/models/Qwen3.5-2B.Q6_K.gguf}"
ENCRYPT=""
DECOY=1
for a in "$@"; do
  case "$a" in
    --encrypt)  ENCRYPT="--encrypt" ;;
    --no-decoy) DECOY="" ;;
  esac
done

SB="$(mktemp -d "${TMPDIR:-/tmp}/terminal-e2e.XXXXXX")"
say()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
fail() { printf '\n\033[31mFAIL: %s\033[0m\n' "$*"; exit 1; }
plain() { sed 's/\x1b\[[0-9;]*m//g' "$1"; }   # tracing colours the field NAMES,
                                              # so `key=value` greps miss without this

# Does a log contain a pattern? SIGPIPE-SAFE, and that is the whole reason it
# exists rather than a bare `plain | grep -q`.
#
# `grep -q` exits at the FIRST match, which hands `sed` a SIGPIPE — exit 141 —
# and `set -o pipefail` then makes 141 the pipeline's status, so a MATCH reads
# as a FAILURE. Measured on this script 2026-08-31: the
# `bound_locus_authoritative` assertion reported "not found" against a log that
# demonstrably contained the line, and it reproduced every run once the log grew
# past sed's buffer. The dangerous direction is the other one — a `grep -q ... &&
# fail` guard cannot fire at all under SIGPIPE, so a real regression passes
# silently. `grep -c` consumes all of its input and cannot be signalled.
log_has() { # $1=logfile  $2=extended regex
  local n
  n=$(plain "$1" | grep -cE "$2")
  [ "${n:-0}" -gt 0 ]
}

cleanup() {
  [ -n "${HPID:-}" ] && kill "$HPID" 2>/dev/null
  [ -n "${TPID:-}" ] && kill "$TPID" 2>/dev/null
  [ -n "${DPID:-}" ] && kill "$DPID" 2>/dev/null
  sleep 1; kill -9 "${HPID:-0}" "${TPID:-0}" "${DPID:-0}" 2>/dev/null
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
if [ -n "$DECOY" ]; then
  [ -e "$DECOY_MODEL" ] || fail "missing decoy model: $DECOY_MODEL  (or pass --no-decoy)"
  lsof -nP -iTCP:"$DECOY_PORT" -sTCP:LISTEN >/dev/null 2>&1 && fail ":$DECOY_PORT is busy"
fi

# Cumulative peer-tally for one node id, read off a holder's /status.
#
# The field path is `inference.peer_requests`, NOT a top-level `peer_requests`.
# The 2026-08-31 two-machine baseline used the top-level path, got nothing, and
# read it as "the tally never moved" — a false negative that hid whether the
# terminal was even being counted.
#
# `node_id` in a row is the TRUNCATED Display form (`node-<16 hex>`), so the
# caller passes that shape, not the full 32-hex.
#
# The body is captured FIRST and fed in on stdin, rather than
# `WANT=... curl | python3`. An assignment prefix binds only the FIRST command
# of a pipeline, so the python3 on the right never saw `WANT` and every read
# came back empty — the same trap step 8 documents for HOLDER_ID, made again
# here. Always prints an integer or `ERR`, never nothing, so a caller can use
# it in an arithmetic test.
tally_for() { # $1=port  $2=node display id
  local body
  body=$(curl -s -m 5 "http://127.0.0.1:$1/status" 2>/dev/null) || { echo ERR; return 0; }
  [ -n "$body" ] || { echo ERR; return 0; }
  WANT="$2" python3 -c '
import sys, json, os
want = os.environ["WANT"]
try:
    rows = json.load(sys.stdin).get("inference", {}).get("peer_requests", [])
except Exception:
    print("ERR"); raise SystemExit(0)
print(sum(r.get("served_total", 0) for r in rows if r.get("node_id", "") == want))
' <<<"$body"; }

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
  log_has "$SB/holder.log" "require_encryption=true" \
    || fail "--encrypt asked for, but the holder never logged require_encryption=true"
  log_has "$SB/holder.log" 'required=\[.*"inference"' \
    || fail "inference is not an iroh-REQUIRED class — a plaintext fallback still exists"
  case "$LINK" in
    *iroh=*) ;;
    *) fail "encrypted invite must carry iroh=" ;;
  esac
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
# Polled, not read once. The merge is fed by GOSSIP, so an empty list right
# after the join is "not yet", not "never" (§18.2) — and a single-shot read
# here passed only because earlier drafts happened to have slower steps in
# front of it. The invariant is that it converges, so wait for it and fail on
# the deadline.
for i in $(seq 1 40); do
  IDS=$(curl -s "http://127.0.0.1:$TERM_PORT/v1/models" | python3 -c '
import sys,json;print(",".join(m["id"] for m in json.load(sys.stdin).get("data",[])))' 2>/dev/null)
  case ",$IDS," in *,primary,*) break ;; esac
  sleep 1
done
echo "   $IDS"
case ",$IDS," in
  *,primary,*) ;;
  *) fail "a joined terminal must surface 'primary' to its own clients (waited 40s)" ;;
esac

say "8. /v1/mesh/status names the class and the binding"
curl -s "http://127.0.0.1:$TERM_PORT/v1/mesh/status" | python3 -c '
import sys,json,os
d=json.load(sys.stdin); cls=d.get("node_class"); entry=str(d.get("entry_node"))
print("   node_class:",cls); print("   entry_node:",entry)
raise SystemExit(0 if cls=="terminal" and os.environ["HOLDER_ID"] in entry else 1)' \
  || fail "/v1/mesh/status must report node_class=terminal and name the bound node"

# The terminal's OWN id, for the peer tally on the holders. Written by the join
# in step 4, so it exists by now.
export TERM_ID
TERM_ID=$(xxd -p "$SB/terminal/.svrnmesh/node_id" | tr -d '\n')
[ ${#TERM_ID} -eq 32 ] || fail "terminal node_id is ${#TERM_ID} chars, expected 32"
TERM_DISPLAY="node-${TERM_ID:0:16}"
echo "   terminal $TERM_ID  (tally key $TERM_DISPLAY)"

if [ -n "$DECOY" ]; then
  say "8b. the DECOY — a second advertiser of \`primary\`, so the binding has a rival"
  mkdir -p "$SB/decoy/.svrnmesh"
  cat > "$SB/decoy/.svrnmesh/config.toml" <<EOF
[models]
primary = "$DECOY_MODEL"
embed = "$EMBED"
context_size = 4096
[daemon]
client_port = $DECOY_PORT
internal_port = $((DECOY_PORT+1))
autostart = false
[data]
dir = "$SB/decoy/.svrnmesh"
[iroh]
enabled = true
[discovery]
mdns = true
seed_addrs = []
EOF
  HOME="$SB/decoy" "$CLI" mesh join "$LINK" >/dev/null 2>&1 \
    || fail "decoy could not join the mesh"
  HOME="$SB/decoy" "$DMN" daemon run > "$SB/decoy.log" 2>&1 &
  DPID=$!
  wait_http "http://127.0.0.1:$DECOY_PORT/status" 240 \
    || { tail -30 "$SB/decoy.log"; fail "decoy never answered /status"; }
  export DECOY_ID
  DECOY_ID=$(xxd -p "$SB/decoy/.svrnmesh/node_id" | tr -d '\n')
  DECOY_STEM=$(basename "$DECOY_MODEL" .gguf)
  BOUND_STEM=$(basename "$MODEL" .gguf)
  echo "   decoy $DECOY_ID on :$DECOY_PORT  (serves $DECOY_STEM, bound node serves $BOUND_STEM)"

  # VALIDATE THE INSTRUMENT BEFORE THE RESULT (§18.4). If the decoy does not
  # actually advertise `primary`, the terminal never had a choice and step 11
  # passes for the wrong reason — the exact shape of the co-located blind spot
  # this whole harness was added to close.
  for i in $(seq 1 60); do
    DADV=$(curl -s -m 5 "http://127.0.0.1:$DECOY_PORT/oicp/v1/capabilities" | python3 -c '
import sys,json
try: print(",".join(m.get("id","") for m in json.load(sys.stdin).get("models",[])))
except Exception: print("")' 2>/dev/null)
    case ",$DADV," in *,primary,*) break ;; esac
    sleep 1
  done
  case ",$DADV," in
    *,primary,*) echo "   decoy advertises: $DADV  <- the terminal now has a rival" ;;
    *) fail "the decoy does not advertise 'primary' (saw '$DADV') — the locality bar would be vacuous" ;;
  esac

  BASE_DECOY=$(tally_for "$DECOY_PORT" "$TERM_DISPLAY")
  echo "   baseline rival tally for the terminal: $BASE_DECOY"
fi

# Baseline for the BOUND holder, outside the decoy block because step 12 asserts
# on it either way. A delta rather than an absolute: these daemons are fresh so
# both readings agree today, but a harness that only ever checks "> 0" silently
# stops being a measurement the first time something warms the tally up front.
BASE_BOUND=$(tally_for "$HOLDER_PORT" "$TERM_DISPLAY")
echo "   baseline bound-holder tally for the terminal: $BASE_BOUND"

say "9. a real turn"
curl -s -m 300 -X POST "http://127.0.0.1:$TERM_PORT/v1/chat/completions" \
  -H 'content-type: application/json' \
  -d '{"model":"primary","messages":[{"role":"user","content":"Reply with the single word: ready"}],"max_tokens":16,"stream":false}' \
  | SERVED_OUT="$SB/served_model" python3 -c '
import sys,json,os
d=json.load(sys.stdin)
if "error" in d: print("   ERROR:",json.dumps(d["error"])[:300]); raise SystemExit(1)
c=d.get("choices",[{}])[0].get("message",{}).get("content","")
print("   served by:",d.get("model")); print("   content:  ",repr(c[:100]))
open(os.environ["SERVED_OUT"],"w").write(str(d.get("model") or ""))
raise SystemExit(0 if c.strip() else 1)' || fail "no turn came back served"
SERVED_MODEL=$(cat "$SB/served_model" 2>/dev/null)

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

say "11. THE LOCALITY BAR — the turn went to the BOUND node, not the rival"
# F6, as a runnable assertion. Two things are checked because they fail
# differently:
#
#   (a) the DECISION — the terminal's own log must show the gate that pins a
#       weightless node to its binding. Deterministic: it either fired or it
#       did not, and pre-fix code cannot emit it at all.
#   (b) the OUTCOME — the rival must have served this terminal NOTHING. This is
#       the half a co-located harness could never express before, because
#       without a rival there was nothing for the turn to go wrong towards.
#
# (a) alone would pass if the gate fired and the turn still leaked; (b) alone
# is only probabilistically falsifying on pre-fix code, since with two idle
# advertisers the old min-in-flight pick could land on the bound node by luck.
# Together they are a gate.
if [ -n "$DECOY" ]; then
  # (a0) THE STRONGEST SIGNAL, and it needs no log at all: the two holders run
  # DIFFERENT model files, so the served model name says which machine ran the
  # turn. Deployment-level and unambiguous — this is the assertion the
  # co-located harness could never make before there was a second holder.
  # `@ peer <name>` is what a PEER-routed turn reports, and it is the marker
  # that actually discriminates. The watch-fail probe of 2026-08-31 proved the
  # model-stem test alone is not enough: with the gate disabled the response
  # read `primary @ peer Alexs-MacBook-Pro-2`, which names no model file at all,
  # so a stem comparison passed a turn that had demonstrably left the binding.
  # Co-located, all three daemons share one hostname, so the peer NAME cannot
  # distinguish them either — the routing FORM can.
  case "$SERVED_MODEL" in
    *"@ peer"*)      fail "the turn was PEER-ROUTED ($SERVED_MODEL) rather than served through the binding. This is F6." ;;
    *"$DECOY_STEM"*) fail "the turn was served by the RIVAL's model ($SERVED_MODEL) — the binding was ignored. This is F6." ;;
    "")              fail "step 9 recorded no served model, so which node ran the turn cannot be established" ;;
    *)               echo "   served model: $SERVED_MODEL  (no '@ peer' attribution; rival runs $DECOY_STEM)" ;;
  esac

  # (a1) the DECISION, from the terminal's own log. Quote-tolerant: tracing
  # renders a string field as gate="value", and grepping for the bare word
  # matched nothing while the gate had in fact fired — the first run of this
  # harness failed here for that reason and not because of the routing.
  if log_has "$SB/terminal.log" 'gate="?bound_locus_authoritative"?'; then
    echo "   decision: gate=bound_locus_authoritative fired"
  else
    plain "$SB/terminal.log" | grep -oE 'gate="?[a-z_]*"?|verdict="?[a-z_:]*"?' | sort | uniq -c
    fail "no bound_locus_authoritative gate in the terminal log — chat did not route through the binding"
  fi

  AFTER_DECOY=$(tally_for "$DECOY_PORT" "$TERM_DISPLAY")
  # An unreadable tally on BOTH sides would compare equal and pass for the
  # wrong reason — absence reported, never defaulted (§18.3).
  case "$AFTER_DECOY$BASE_DECOY" in
    *ERR*|'') fail "could not read the rival's peer tally (base='$BASE_DECOY' after='$AFTER_DECOY') — cannot say whether the turn leaked to it" ;;
  esac
  if [ "$AFTER_DECOY" != "$BASE_DECOY" ]; then
    fail "the RIVAL served this terminal (tally $BASE_DECOY -> $AFTER_DECOY). \
The binding was ignored — this is F6."
  fi
  echo "   outcome:  rival tally unchanged at $AFTER_DECOY — nothing leaked to it"

  # A peer route would also name the serving peer on the response `model`
  # field ("primary @ peer <name>"), which is how F6 was originally spotted.
  log_has "$SB/terminal.log" 'verdict="?named_peer:|served_by="?peer:' \
    && fail "the terminal logged a PEER route for a turn that should have gone to its binding"
fi

say "12. the PEER TALLY moved on the bound node  (the tn-2 stamp bar)"
# `SplitInferenceProvider::resolved` did not stamp `X-Node-Id` until
# 2026-08-31, so a terminal's traffic was admitted as the entry node's OWN
# local traffic: unrationable (ceiling, foreground yield and contribution
# pause are all keyed on the header) and unaccounted.
#
# The tn-2 order recorded this bar as needing two machines. It does not — the
# tally keys on a header, not on locality — and that mistake is why the check
# went unrun rather than being one curl.
BOUND_TALLY=$(tally_for "$HOLDER_PORT" "$TERM_DISPLAY")
case "$BOUND_TALLY" in
  ''|*[!0-9]*) fail "could not read inference.peer_requests off the bound holder (got '$BOUND_TALLY')" ;;
esac
case "${BASE_BOUND:-0}" in ''|*[!0-9]*) BASE_BOUND=0 ;; esac
if [ "$BOUND_TALLY" -le "$BASE_BOUND" ]; then
  curl -s -m 5 "http://127.0.0.1:$HOLDER_PORT/status" | python3 -c '
import sys,json
rows=json.load(sys.stdin).get("inference",{}).get("peer_requests",[])
print("   rows the holder DOES have:", json.dumps(rows)[:500] or "<none>")'
  fail "the bound holder counted no new requests from $TERM_DISPLAY \
(tally ${BASE_BOUND:-0} -> $BOUND_TALLY). The terminal is UNSTAMPED, so its \
turns are admitted as the holder's OWN local traffic: unrationable and unaccounted."
fi
echo "   bound-holder tally for the terminal: ${BASE_BOUND:-0} -> $BOUND_TALLY"

say "13. glassbox: which transport resolved it?"
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

if [ -n "$DECOY" ]; then
  say "PASSED  (locality bar INCLUDED; ForwardsOffBox still needs a second machine)"
else
  say "PASSED  (--no-decoy: the locality bar did NOT run; ForwardsOffBox needs a second machine)"
fi
