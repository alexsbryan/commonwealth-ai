#!/usr/bin/env bash
# fim-smoke.sh — weight-gated FIM vertical-slice smoke
# (sovereign/docs/INLINE_COMPLETION.md, plan F0 exit criterion).
#
# Boots a REAL daemon on an isolated $HOME (scratch ports, scratch
# data dir) against a real coder GGUF, then exercises
# POST /v1/completions in both serving modes:
#
#   alias      [models.edit].path == fast path → served from the
#              resident fast slot (lean mode, decision D8); asserts
#              sovereign_debug.slot == "fast".
#   dedicated  [models.edit].path != fast path → a dedicated pinned
#              extras slot ("edit"); asserts slot == "edit".
#
# Readiness is the FIM LANE, not the slot: since the two-lane split the
# editing slot may serve next-edit only, and /v1/completions 503s by
# design in that arrangement. So this waits for
# /status.inference.edit.fim_style, and says which model failed the
# marker probe when it never appears.
#
# Per mode: non-stream curl, streaming curl (finish_reason + [DONE]),
# and a debug curl — collecting the adapter-side timings_ms{ttft,total}
# over N samples for the p50/p95 record.
#
# NOT in the default CI gate (needs multi-GB weights).
#
# Env overrides:
#   SOVEREIGN_FIM_GGUF     FIM model (default: Mellum2-12B-A2.5B-Instruct-Q6_K)
#   SOVEREIGN_SMOKE_PRIMARY  primary/fast model for dedicated mode
#   SOVEREIGN_SMOKE_EMBED    embed model
#   SOVEREIGN_DAEMON_BIN   daemon binary (default: target/debug/sovereign-cli-daemon)
#   SOVEREIGN_FIM_SAMPLES  timing samples per mode (default 3)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIM_GGUF="${SOVEREIGN_FIM_GGUF:-$REPO_ROOT/sovereign/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf}"
PRIMARY_GGUF="${SOVEREIGN_SMOKE_PRIMARY:-$REPO_ROOT/models/bonsai-8b.gguf/Bonsai-8B-Q1_0.gguf}"
EMBED_GGUF="${SOVEREIGN_SMOKE_EMBED:-$REPO_ROOT/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf}"
DAEMON_BIN="${SOVEREIGN_DAEMON_BIN:-$REPO_ROOT/target/debug/sovereign-cli-daemon}"
SAMPLES="${SOVEREIGN_FIM_SAMPLES:-3}"

for f in "$FIM_GGUF" "$PRIMARY_GGUF" "$EMBED_GGUF" "$DAEMON_BIN"; do
  if [ ! -e "$f" ]; then
    echo "fim-smoke: missing $f" >&2
    echo "  (build the daemon: cargo build -p sovereign-cli-daemon; or point the env vars at real files)" >&2
    exit 2
  fi
done

FIM_STEM="$(basename "$FIM_GGUF" .gguf)"

# ── helpers ────────────────────────────────────────────────────────

json_get() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

wait_http() { # url, timeout_secs
  local url="$1" deadline=$(( $(date +%s) + $2 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if curl -sf -o /dev/null --max-time 2 "$url" 2>/dev/null; then return 0; fi
    sleep 2
  done
  return 1
}

# The editing slot from /status, preferring the current `inference.edit`
# key and falling back to the deprecated `inference.fim` mirror. The
# fallback is NAMED on stderr when it fires, so a stale daemon binary
# can never quietly pass this smoke under the old contract.
EDIT_SLOT_PY='
import sys, json
d = json.load(sys.stdin)
inf = d.get("inference") or {}
slot = inf.get("edit")
if slot is None and inf.get("fim") is not None:
    slot = inf["fim"]
    print("fim-smoke: daemon predates the two-lane split — reading the "
          "deprecated inference.fim mirror", file=sys.stderr)
'

# wait until /status reports a slot that can serve FIM (or fail with log
# tail). A slot with no fim_style serves next-edit only — real, but not
# what this script exercises.
wait_fim() { # port, timeout_secs, logfile
  local port="$1" deadline=$(( $(date +%s) + $2 )) log="$3"
  while [ "$(date +%s)" -lt "$deadline" ]; do
    local out
    out="$(curl -sf --max-time 3 "http://127.0.0.1:$port/status" 2>/dev/null)" || { sleep 3; continue; }
    if echo "$out" | python3 -c "$EDIT_SLOT_PY
sys.exit(0 if slot and slot.get('fim_style') else 1)" 2>/dev/null; then
      echo "$out"
      return 0
    fi
    sleep 3
  done
  echo "fim-smoke: no FIM lane appeared in /status." >&2
  # Distinguish "no editing model" from "editing model without markers":
  # the second is a model choice, not a broken daemon, and the fix differs.
  local last
  last="$(curl -sf --max-time 3 "http://127.0.0.1:$port/status" 2>/dev/null)" || last=""
  if [ -n "$last" ]; then
    echo "$last" | python3 -c "$EDIT_SLOT_PY
if not slot:
    print('  /status reports NO editing model at all (inference.edit absent).', file=sys.stderr)
else:
    print('  editing slot is live on %r (next_edit_format=%s) but carries no FIM'
          % (slot.get('model_id'), slot.get('next_edit_format')), file=sys.stderr)
    print('  markers, so POST /v1/completions 503s by design. Point', file=sys.stderr)
    print('  [models.edit].path at a coder GGUF.', file=sys.stderr)
    if slot.get('advice'):
        print('  daemon advice: %s' % slot['advice'], file=sys.stderr)
" 2>/dev/null || true
  fi
  echo "fim-smoke: daemon log tail:" >&2
  tail -30 "$log" >&2
  return 1
}

fim_curl() { # port, body  → prints response body
  curl -sf --max-time 300 -H 'content-type: application/json' \
    -d "$2" "http://127.0.0.1:$1/v1/completions"
}

run_mode() { # mode ∈ {alias, dedicated}
  local mode="$1"
  local scratch; scratch="$(mktemp -d /tmp/fim-smoke-XXXXXX)"
  local home="$scratch/home" port log
  mkdir -p "$home/.svrnmesh/data"
  port=$(( (RANDOM % 20000) + 20000 ))
  log="$scratch/daemon.log"

  case "$mode" in
    alias)
      # Lean mode: one model IS the fast slot AND the editing slot.
      cat > "$home/.svrnmesh/config.toml" <<EOF
[models]
primary = "$FIM_GGUF"
embed = "$EMBED_GGUF"
context_size = 4096

[models.edit]
path = "$FIM_GGUF"

[daemon]
client_port = $port
internal_port = $((port + 1))

[data]
dir = "$home/.svrnmesh/data"
EOF
      ;;
    dedicated)
      cat > "$home/.svrnmesh/config.toml" <<EOF
[models]
primary = "$PRIMARY_GGUF"
embed = "$EMBED_GGUF"
context_size = 4096

[models.edit]
path = "$FIM_GGUF"
context_size = 4096

[daemon]
client_port = $port
internal_port = $((port + 1))

[data]
dir = "$home/.svrnmesh/data"
EOF
      ;;
  esac

  echo "═══ mode: $mode (port $port, scratch $scratch) ═══"
  HOME="$home" "$DAEMON_BIN" daemon run > "$log" 2>&1 &
  local pid=$!
  # Explicit cleanup on every exit path (a RETURN trap would mask the
  # function's failure status and let a red run exit 0).
  mode_fail() { echo "fim-smoke: $1" >&2; kill $pid 2>/dev/null || true; wait $pid 2>/dev/null || true; return 1; }

  if ! wait_http "http://127.0.0.1:$port/status" 900; then
    tail -40 "$log" >&2
    mode_fail "daemon never came up" || return 1
  fi
  local status_json
  status_json="$(wait_fim "$port" 900 "$log")" || { mode_fail "FIM never appeared in /status" || return 1; }
  local got_slot got_style got_nes got_degraded got_advice
  got_slot="$(echo "$status_json"     | python3 -c "$EDIT_SLOT_PY"$'\nprint(slot["slot"])')"
  got_style="$(echo "$status_json"    | python3 -c "$EDIT_SLOT_PY"$'\nprint(slot["fim_style"])')"
  got_nes="$(echo "$status_json"      | python3 -c "$EDIT_SLOT_PY"$'\nprint(slot.get("next_edit_format", "<none>"))')"
  got_degraded="$(echo "$status_json" | python3 -c "$EDIT_SLOT_PY"$'\nprint(slot.get("degraded", False))')"
  got_advice="$(echo "$status_json"   | python3 -c "$EDIT_SLOT_PY"$'\nprint(slot.get("advice", ""))')"
  echo "/status inference.edit: slot=$got_slot fim_style=$got_style next_edit=$got_nes degraded=$got_degraded"
  # A configured smoke run should have nothing to advise. Print it when
  # it does rather than swallowing the daemon's own diagnosis.
  [ -z "$got_advice" ] || echo "/status advice: $got_advice"

  case "$mode" in
    alias)     [ "$got_slot" = "fast" ] || { mode_fail "alias mode must serve from slot 'fast', got '$got_slot'" || return 1; } ;;
    dedicated) [ "$got_slot" = "edit" ] || { mode_fail "dedicated mode must serve from slot 'edit', got '$got_slot'" || return 1; } ;;
  esac
  # [models.edit] was configured explicitly in both modes, so the
  # fallback-provenance flag must be false — a True here means the
  # daemon ignored the config and borrowed the chat model.
  [ "$got_degraded" = "False" ] || { mode_fail "[models.edit] was configured but /status reports degraded=$got_degraded" || return 1; }

  # ── non-stream + debug round-trip ────────────────────────────────
  local resp ttfts=() totals=()
  local i
  for i in $(seq 1 "$SAMPLES"); do
    resp="$(fim_curl "$port" '{
      "prefix": "fn fibonacci(n: u32) -> u32 {\n    match n {\n        0 => 0,\n        1 => 1,\n        _ => ",
      "suffix": "\n    }\n}\n",
      "path": "fib.rs",
      "debug": true
    }')" || { tail -20 "$log" >&2; mode_fail "non-stream curl" || return 1; }
    local text stop_rule ttft total
    text="$(echo "$resp" | json_get "d['choices'][0]['text']")"
    stop_rule="$(echo "$resp" | json_get "d.get('sovereign_debug',{}).get('stop_rule','<absent>')")"
    ttft="$(echo "$resp" | json_get "d.get('sovereign_debug',{}).get('timings_ms',{}).get('ttft',-1)")"
    total="$(echo "$resp" | json_get "d.get('sovereign_debug',{}).get('timings_ms',{}).get('total',-1)")"
    ttfts+=("$ttft"); totals+=("$total")
    if [ -z "$text" ]; then mode_fail "empty completion text: $resp" || return 1; fi
    case "$text" in
      *"<fim_"*|*"<|endoftext|>"*|*"<|im_end|>"*)
        mode_fail "marker leak in completion text: $text" || return 1 ;;
    esac
    printf 'sample %d: ttft=%sms total=%sms stop_rule=%s text=%q\n' "$i" "$ttft" "$total" "$stop_rule" "$text"
  done

  # ── streaming: finish_reason + [DONE] ────────────────────────────
  local sse
  sse="$(curl -sfN --max-time 300 -H 'content-type: application/json' \
    -d '{"prefix": "def quicksort(items):\n    ", "suffix": "\n", "stream": true, "debug": true}' \
    "http://127.0.0.1:$port/v1/completions")" || { mode_fail "streaming curl" || return 1; }
  echo "$sse" | grep -q '"finish_reason"' || { mode_fail "stream missing finish_reason: $sse" || return 1; }
  echo "$sse" | grep -q '"sovereign_debug"' || { mode_fail "stream missing sovereign_debug: $sse" || return 1; }
  echo "$sse" | grep -q '\[DONE\]' || { mode_fail "stream missing [DONE]: $sse" || return 1; }
  echo "stream: finish_reason + sovereign_debug + [DONE] ✓"

  # ── latency percentiles (adapter-side timings) ───────────────────
  python3 - "$mode" "${ttfts[@]}" -- "${totals[@]}" <<'PY'
import sys
mode = sys.argv[1]
sep = sys.argv.index("--")
ttfts = sorted(int(x) for x in sys.argv[2:sep] if int(x) >= 0)
totals = sorted(int(x) for x in sys.argv[sep+1:] if int(x) >= 0)
def pct(vals, p):
    if not vals: return -1
    k = max(0, min(len(vals)-1, round(p*(len(vals)-1))))
    return vals[k]
print(f"[{mode}] ttft  p50={pct(ttfts,0.5)}ms p95={pct(ttfts,0.95)}ms  "
      f"total p50={pct(totals,0.5)}ms p95={pct(totals,0.95)}ms  (n={len(ttfts)})")
PY

  kill $pid 2>/dev/null || true
  wait $pid 2>/dev/null || true
  rm -rf "$scratch"
  echo "mode $mode: PASS"
  echo
}

run_mode alias
run_mode dedicated
echo "fim-smoke: ALL PASS (model: $FIM_STEM)"
