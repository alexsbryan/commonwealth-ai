#!/usr/bin/env bash
# Distributed-inference auto-warm end-to-end harness (#5a / #5b).
#
# Validates the demo-natural topology — THIS node (the host) holds a model the
# Mac worker does NOT, and on distributing it the host auto-seeds the Mac's shard
# (fetch + warm) so the load is all cache hits, no bulk weight send, no wedge.
#
# Run this on the HOST (Strix). It does NOT start/stop daemons — you start the
# host daemon in the toolbox and the Mac agent starts the worker. It:
#   1. Pre-flights both nodes (new build present, worker advertised + reachable),
#   2. Watches the host daemon log while the Mac worker (re)joins,
#   3. Confirms the auto-warm → owned-placement → tokens chain,
#   4. Reports PASS/FAIL and reminds you to read the Mac-side RSS proof.
#
# The decisive proof of *distribution* (vs a local-only fallback) is the host
# log line `explicit tensor placement via -ot overrides` PLUS the Mac worker's
# RSS climbing by only its shard fraction — tokens alone don't distinguish the
# two (a local-only load also returns tokens).
set -uo pipefail

# ── Config (override via flags) ──────────────────────────────────────────────
MAC_HOST="${MAC_HOST:-100.64.0.2}"     # Mac Tailscale IP
CLIENT_PORT="${CLIENT_PORT:-9741}"
INTERNAL_PORT="${INTERNAL_PORT:-9742}"
RPC_PORT="${RPC_PORT:-50052}"
DAEMON_LOG="${DAEMON_LOG:-$HOME/.svrnmesh/logs/daemon.log}"
MODEL="${MODEL:-commonwealth/primary}"
TIMEOUT="${TIMEOUT:-600}"                  # seconds to wait for the chain (raise for a big-GGUF fetch)
SELF="http://127.0.0.1:${CLIENT_PORT}"

while [ $# -gt 0 ]; do
  case "$1" in
    --mac-host) MAC_HOST="$2"; shift 2;;
    --daemon-log) DAEMON_LOG="$2"; shift 2;;
    --timeout) TIMEOUT="$2"; shift 2;;
    --model) MODEL="$2"; shift 2;;
    --rpc-port) RPC_PORT="$2"; shift 2;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0;;
    *) echo "unknown flag: $1" >&2; exit 2;;
  esac
done

say()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*"; }
note() { printf '  • %s\n' "$*"; }
code() { curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$@" 2>/dev/null; }

FAIL=0

# ── 1. Pre-flight ────────────────────────────────────────────────────────────
say "Pre-flight"

# Host daemon up?
if [ "$(code "$SELF/status")" = "200" ]; then ok "host daemon up ($SELF)"; else
  bad "host daemon not responding at $SELF/status — start it (debug, in the dev-toolbox toolbox)"; FAIL=1; fi

# Host running the NEW build? (old build 404s the route; new build 4xx/5xx it).
HC="$(code -X POST -H 'content-type: application/json' -d '{}' "http://127.0.0.1:${INTERNAL_PORT}/internal/rpc-warm")"
if [ "$HC" = "404" ] || [ -z "$HC" ]; then
  bad "host /internal/rpc-warm → ${HC:-no-response}: this daemon predates #5. Rebuild + restart it (debug, toolbox)."; FAIL=1
else ok "host has the #5 auto-warm route (/internal/rpc-warm → $HC)"; fi

# Host env hints (best-effort — these are set at daemon launch, not here).
note "host MUST have been launched with: SOVEREIGN_RPC_DISCOVER=1, and WITHOUT SOVEREIGN_RPC_ASSUME_WARMED"
note "for the #5b byte-range variant, also launch with SOVEREIGN_RPC_SHARD_FETCH=ranges"

# Mac worker reachable + advertising?
MAC_STATUS="$(curl -s --max-time 5 "http://${MAC_HOST}:${CLIENT_PORT}/status" 2>/dev/null)"
if [ -n "$MAC_STATUS" ]; then
  ok "Mac daemon reachable (http://${MAC_HOST}:${CLIENT_PORT})"
  RPCW="$(printf '%s' "$MAC_STATUS" | grep -o '"rpc_worker"[^}]*}' || true)"
  if [ -n "$RPCW" ]; then ok "Mac advertises an RPC worker: $RPCW"; else
    bad "Mac /status has no live rpc_worker — start it with SOVEREIGN_RPC_SERVE=0.0.0.0:${RPC_PORT} (and confirm the port is listening)"; FAIL=1; fi
else
  note "Mac daemon not reachable yet at http://${MAC_HOST}:${CLIENT_PORT} — that's fine if you're about to bring it up"
fi

# Mac new build? (the worker side of the warm route).
MC="$(code -X POST -H 'content-type: application/json' -d '{}' "http://${MAC_HOST}:${INTERNAL_PORT}/internal/rpc-warm")"
if [ "$MC" = "404" ]; then
  bad "Mac /internal/rpc-warm → 404: the Mac daemon predates #5. It MUST run the same new build (the warm route + RpcShardWarmer are worker-side)."; FAIL=1
elif [ -n "$MC" ]; then ok "Mac has the #5 worker route (/internal/rpc-warm → $MC)"
else note "Mac internal port not reachable yet (bring the worker up; firewall 9742 + ${RPC_PORT} on the tailnet)"; fi

# Mac must serve the worker port for the actual RPC weight path.
if [ "$(code "http://${MAC_HOST}:${INTERNAL_PORT}/internal/v1/models/list")" = "200" ] 2>/dev/null; then
  ok "Mac internal model-list reachable (the host fetches/range-fetches from here in reverse)"; fi

[ "$FAIL" = "1" ] && { say "Pre-flight FAILED — fix the ✗ items above, then re-run."; exit 1; }

# Daemon log readable?
if [ ! -r "$DAEMON_LOG" ]; then
  bad "cannot read host daemon log: $DAEMON_LOG"
  note "point --daemon-log at the host daemon's stdout/err (e.g. start it with '> ~/.svrnmesh/logs/daemon.log 2>&1')"
  exit 1
fi
ok "watching host daemon log: $DAEMON_LOG"

# ── 2. Trigger + watch ───────────────────────────────────────────────────────
say "Trigger the distributed load"
cat <<EOF
  The host redistributes the primary when the WORKER SET CHANGES. The cleanest
  trigger is to (re)start the Mac worker now — the host will discover it (~15s),
  debounce (~20s), then reload_primary → auto-warm → distributed load.

  → On the Mac, (re)start the worker:  SOVEREIGN_RPC_SERVE=0.0.0.0:${RPC_PORT} <daemon>
  (If the Mac worker is already up and the primary is already distributed,
   restart it to force a fresh, observable event.)

  Watching for up to ${TIMEOUT}s. A large whole-GGUF fetch can take minutes —
  raise --timeout for a 35B+.
EOF

TAIL_TMP="$(mktemp)"
# Capture only NEW log lines from this point.
tail -Fn0 "$DAEMON_LOG" > "$TAIL_TMP" 2>/dev/null &
TAIL_PID=$!
trap 'kill "$TAIL_PID" 2>/dev/null; rm -f "$TAIL_TMP"' EXIT

# Markers, in causal order.
M_DISCOVER='mesh RPC workers changed|RPC worker set changed'
M_WARMING='auto-warming worker shards'
M_WARMED='auto-warm complete'
M_OVERRIDE='explicit tensor placement via -ot overrides'
M_LOCAL='loading local-only|auto-warm failed|local-only \(never wedge\)'

deadline=$(( SECONDS + TIMEOUT ))
saw_discover=0 saw_warming=0 saw_warmed=0 saw_override=0 saw_local=0
while [ $SECONDS -lt $deadline ]; do
  grep -Eq "$M_DISCOVER"  "$TAIL_TMP" && [ $saw_discover = 0 ] && { saw_discover=1; ok "host saw the worker-set change"; }
  grep -Eq "$M_WARMING"   "$TAIL_TMP" && [ $saw_warming  = 0 ] && { saw_warming=1;  ok "host is auto-warming worker shards"; }
  grep -Eq "$M_WARMED"    "$TAIL_TMP" && [ $saw_warmed   = 0 ] && { saw_warmed=1;   ok "all worker shards reported warm"; }
  grep -Eq "$M_OVERRIDE"  "$TAIL_TMP" && [ $saw_override = 0 ] && { saw_override=1; ok "loading with -ot overrides (DISTRIBUTED, warm shards)"; }
  grep -Eq "$M_LOCAL"     "$TAIL_TMP" && [ $saw_local    = 0 ] && { saw_local=1;    bad "host fell back to LOCAL-ONLY — auto-warm did not complete"; }
  { [ $saw_override = 1 ] || [ $saw_local = 1 ]; } && break
  sleep 3
done

# ── 3. Verify tokens (the load is healthy, not wedged) ───────────────────────
say "Verify the distributed primary generates"
REQ='{"model":"'"$MODEL"'","messages":[{"role":"user","content":"Reply with one word: hello."}],"max_tokens":8,"stream":false}'
RESP="$(curl -s --max-time 180 -H 'content-type: application/json' -d "$REQ" "$SELF/v1/chat/completions" 2>/dev/null)"
TOKENS="$(printf '%s' "$RESP" | grep -o '"content":"[^"]*"' | head -1 || true)"
if [ -n "$TOKENS" ]; then ok "primary generated: $TOKENS"; else
  bad "no tokens from the primary (response: $(printf '%s' "$RESP" | head -c 200))"; fi

# ── 4. Verdict ───────────────────────────────────────────────────────────────
say "Verdict"
if [ $saw_override = 1 ] && [ -n "$TOKENS" ] && [ $saw_local = 0 ]; then
  ok "PASS — the host auto-warmed the worker's shard and loaded DISTRIBUTED with no manual cache step, no ASSUME_WARMED, no wedge."
  echo
  note "Confirm on the Mac (ask the Mac agent): worker-process RSS climbed by ONLY the shard fraction"
  note "(~30-40% of the model, NOT the whole model). Whole-GGUF mode: the full file lands on Mac disk;"
  note "byte-range mode (SOVEREIGN_RPC_SHARD_FETCH=ranges): only the shard's tensors do."
  RESULT=0
else
  bad "FAIL — distribution+auto-warm not confirmed."
  RESULT=1
fi

echo
say "Host log excerpt (new lines this run)"
grep -nE "$M_DISCOVER|$M_WARMING|$M_WARMED|$M_OVERRIDE|$M_LOCAL|rpc-warm|redistribut" "$TAIL_TMP" | tail -40 || note "(no matching lines — is the daemon logging to $DAEMON_LOG at info level? set RUST_LOG=sovereign_inference=info,sovereign_mesh=info)"

exit $RESULT
