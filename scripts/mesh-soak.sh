#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# mesh-soak.sh — Layer-2 multi-process mesh soak: the "real bytes" layer of the
# mesh QA stack (Layer 1 = in-process DST, `sovereign-mesh --features dst`;
# Layer 3 = the SLO gate, `sovereign mesh soak-gate`). It boots N real
# `sovereign daemon` processes, forms one mesh over real TCP gossip, then drives
# real faults (SIGKILL crash + churn/restart) in repeated cycles, asserting the
# HTTP-observable invariant pack via `sovereign mesh check-invariants` (the
# unit-tested assertion engine — sovereign-cli-llm/src/mesh_soak.rs) at every
# checkpoint. Findings stream to mesh-soak-findings.jsonl for the SLO gate.
#
# ── What it exercises that the in-process DST suite cannot ────────────────────
#   Real process crashes (SIGKILL) + real wall-clock offline-decay + real churn
#   across actual OS-process and TCP boundaries — the fix-A decay path holding
#   under a genuine kill -9, not a simulated `down` flag.
#
# ── Isolation (this is load-bearing) ──────────────────────────────────────────
#   The `local` backend re-execs the whole soak inside a ROOTLESS NETWORK
#   NAMESPACE (`unshare -rn`, loopback-only). Why: the daemon has no mDNS-disable
#   knob and the CLI `mesh join` is hardcoded to :9741 — run on the bare host it
#   would mDNS-advertise to (and try to join) the operator's real production
#   mesh. The netns seals it: test daemons see only `lo`, self-advertise
#   127.0.0.1, and form their mesh entirely on localhost. Zero production
#   cross-talk. (Verified: the host's real mesh member count is unchanged across
#   a full soak.)
#
# ── Why a tiny model ──────────────────────────────────────────────────────────
#   A mesh soak needs daemons that boot + gossip + serve /v1/mesh/status, NOT
#   inference. The eager model load fails the daemon on a bad path, so we point
#   `primary` at a small embedding GGUF (~600MB/node) — N nodes fit in RAM. The
#   embedding model can't chat; that's fine, no chat request is made.
#
# Usage:
#   scripts/mesh-soak.sh [--nodes N] [--minutes M] [--seed S] [--keep] [--gate]
#
# Prereq: a built sovereign-cli (cargo build --bins; debug is fine), and the
# small model at $MESH_SOAK_MODEL (default below). `ip` + `unshare` for the netns.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# ── Args (parsed pre-reexec so they survive into the namespace) ────────────────
NODES="${NODES:-3}"; MINUTES="${MINUTES:-5}"; SEED="${SEED:-1}"; KEEP="${KEEP:-0}"; GATE="${GATE:-0}"
BACKEND="${MESH_SOAK_BACKEND:-local}"
while [ $# -gt 0 ]; do
  case "$1" in
    --nodes)   NODES="$2"; shift 2;;
    --minutes) MINUTES="$2"; shift 2;;
    --seed)    SEED="$2"; shift 2;;
    --keep)    KEEP=1; shift;;
    --gate)    GATE=1; shift;;
    -h|--help) sed -n '3,35p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

# ── Re-exec into a fresh rootless netns (loopback up) for the local backend ────
if [ "$BACKEND" = "local" ] && [ -z "${MESH_SOAK_IN_NETNS:-}" ]; then
  exec unshare -rn env MESH_SOAK_IN_NETNS=1 \
    NODES="$NODES" MINUTES="$MINUTES" SEED="$SEED" KEEP="$KEEP" GATE="$GATE" \
    MESH_SOAK_BACKEND="$BACKEND" bash "$0"
fi
[ "$BACKEND" = "local" ] && ip link set lo up

CLI="${SOVEREIGN_CLI:-$ROOT/target/debug/sovereign-cli}"
[ -x "$CLI" ] || CLI="$ROOT/target/release/sovereign-cli"
[ -x "$CLI" ] || { echo "sovereign-cli not built (cargo build --bins)"; exit 1; }
MODEL="${MESH_SOAK_MODEL:-$ROOT/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf}"
[ -f "$MODEL" ] || { echo "model not found: $MODEL (set MESH_SOAK_MODEL)"; exit 1; }

WORK="$(mktemp -d -t mesh-soak.XXXXXX)"
FINDINGS="$ROOT/mesh-soak-findings.jsonl"; : > "$FINDINGS"
DECAY_WAIT="${DECAY_WAIT:-72}"     # offline_threshold is 60s — wait past it
RANDOM=$SEED                        # seed bash PRNG → reproducible victim picks
declare -a PIDS NODE_IDS
FAILS=0

cport() { echo $((19741 + 2 * $1)); }
iport() { echo $((19742 + 2 * $1)); }
log()   { printf '\n\033[1m# [soak] %s\033[0m\n' "$*"; }
finding() { printf '%s\n' "$1" >> "$FINDINGS"; }
jget() { curl -s -m 4 "$1" 2>/dev/null | python3 -c "import sys,json
try:
    d=json.load(sys.stdin); print(eval(sys.argv[1]))
except Exception: pass" "$2"; }

status_url() { echo "http://127.0.0.1:$(cport $1)/v1/mesh/status"; }

boot_node() {  # boot_node <i>
  local i="$1" d="$WORK/node$i"; mkdir -p "$d"
  cat > "$d/config.toml" <<EOF
[models]
primary = "$MODEL"
embed = "$MODEL"
context_size = 4096
[daemon]
client_port = $(cport $i)
internal_port = $(iport $i)
autostart = false
primary_idle_secs = 1800
extras_idle_secs = 0
freshness_watchers_enabled = false
client_bind = "127.0.0.1"
[data]
dir = "$d"
EOF
  "$CLI" daemon run --config "$d/config.toml" > "$d/daemon.$RANDOM.log" 2>&1 &
  PIDS[$i]=$!
}
wait_port() { local i="$1" _; for _ in $(seq 1 40); do
  curl -s -m 2 -o /dev/null "$(status_url $i)" 2>/dev/null && return 0
  kill -0 "${PIDS[$i]}" 2>/dev/null || return 1; sleep 0.5; done; return 1; }

join_to_founder() {  # join_to_founder <i> <founder_key>
  local i="$1" key="$2"
  local body; body=$(printf '{"key_or_url":"sovereign://join/%s?relay=127.0.0.1:%s","node_name":"node%s"}' \
    "$key" "$(iport 0)" "$i")
  curl -s -m 25 -X POST "http://127.0.0.1:$(cport $i)/v1/mesh/join" \
    -H 'content-type: application/json' -d "$body" >/dev/null 2>&1
}

self_id() { jget "$(status_url $1)" '[m["node_id"] for m in d["members"] if m["is_self"]][0]'; }
online_count() { jget "$(status_url $1)" 'd["members_online"]'; }
sees_status() { jget "$(status_url $1)" "[m['status'] for m in d['members'] if m['node_id']=='$2'][0]"; }

check() {  # check <label> <nodes-csv> <expect-live-csv> ; appends a finding, bumps FAILS
  local label="$1" nodes="$2" live="$3"
  if "$CLI" mesh check-invariants --nodes "$nodes" --expect-live "$live"; then
    finding "{\"phase\":\"$label\",\"ok\":true,\"violations\":[]}"
  else
    finding "{\"phase\":\"$label\",\"ok\":false,\"violations\":[\"see-stderr\"]}"; FAILS=$((FAILS+1))
  fi
}

teardown() { log "teardown"; for i in $(seq 0 $((NODES-1))); do kill -9 "${PIDS[$i]:-0}" 2>/dev/null; done
  [ "$KEEP" = 0 ] && rm -rf "$WORK"; }
trap teardown EXIT

# ── bring up the mesh ─────────────────────────────────────────────────────────
log "backend=$BACKEND nodes=$NODES minutes=$MINUTES seed=$SEED model=$(basename "$MODEL")"
for i in $(seq 0 $((NODES-1))); do boot_node "$i"; done
for i in $(seq 0 $((NODES-1))); do wait_port "$i" && echo "  node$i up" || echo "  node$i FAILED to bind"; done

FKEY=$(jget "$(status_url 0)" 'd["join_key"]')
log "founder key=$FKEY — joining $((NODES-1)) peers over localhost relay"
for i in $(seq 1 $((NODES-1))); do join_to_founder "$i" "$FKEY"; done

log "waiting for convergence to $NODES members"
for _ in $(seq 1 45); do conv=1
  for i in $(seq 0 $((NODES-1))); do [ "$(jget "$(status_url $i)" 'd["members_total"]')" = "$NODES" ] || conv=0; done
  [ "$conv" = 1 ] && break; sleep 1; done
for i in $(seq 0 $((NODES-1))); do NODE_IDS[$i]=$(self_id "$i"); done
ALL_NODES=$(for i in $(seq 0 $((NODES-1))); do printf '127.0.0.1:%s,' "$(cport $i)"; done | sed 's/,$//')
ALL_IDS=$(IFS=,; echo "${NODE_IDS[*]}")
echo "  converged: node0 online=$(online_count 0)/$NODES"
check "healthy" "$ALL_NODES" "$ALL_IDS"

# ── repeated crash/churn cycles until the deadline ────────────────────────────
DEADLINE=$(( $(date +%s) + MINUTES*60 )); CYCLE=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  CYCLE=$((CYCLE+1))
  victim=$(( RANDOM % (NODES-1) + 1 ))         # never the founder (node0)
  log "cycle $CYCLE — crash node$victim (kill -9)"
  kill -9 "${PIDS[$victim]}" 2>/dev/null
  finding "{\"kind\":\"fault\",\"action\":\"kill-9\",\"node\":$victim,\"cycle\":$CYCLE}"

  echo "  waiting ${DECAY_WAIT}s for offline-decay…"; sleep "$DECAY_WAIT"
  # survivors must now show the victim OFFLINE (decayed, not a live ghost)
  surv_nodes=""; surv_ids=""
  for i in $(seq 0 $((NODES-1))); do [ "$i" = "$victim" ] && continue
    surv_nodes+="127.0.0.1:$(cport $i),"; surv_ids+="${NODE_IDS[$i]},"; done
  surv_nodes="${surv_nodes%,}"; surv_ids="${surv_ids%,}"
  echo "  node0 sees node$victim as: $(sees_status 0 "${NODE_IDS[$victim]}")"
  check "post-crash-decay" "$surv_nodes" "$surv_ids"

  # churn: restart + rejoin + reconverge
  log "cycle $CYCLE — restart node$victim + rejoin"
  boot_node "$victim"; wait_port "$victim"
  join_to_founder "$victim" "$FKEY"
  for _ in $(seq 1 45); do [ "$(online_count 0)" = "$NODES" ] && break; sleep 1; done
  NODE_IDS[$victim]=$(self_id "$victim"); ALL_IDS=$(IFS=,; echo "${NODE_IDS[*]}")
  echo "  reconverged: node0 online=$(online_count 0)/$NODES"
  check "healed" "$ALL_NODES" "$ALL_IDS"

  # load: timed /v1/mesh/status queries (latency SLIs for the gate)
  for _ in $(seq 1 20); do
    ms=$(curl -s -m 4 -o /dev/null -w '%{time_total}' "$(status_url 0)" 2>/dev/null)
    finding "{\"kind\":\"load\",\"latency_ms\":$(python3 -c "print(round(float('${ms:-0}')*1000,2))" 2>/dev/null || echo 0),\"ok\":$([ -n "$ms" ] && echo true || echo false)}"
  done
done

# ── verdict ───────────────────────────────────────────────────────────────────
log "VERDICT — $CYCLE cycle(s), $FAILS checkpoint failure(s); findings=$FINDINGS"
if [ "$GATE" = 1 ]; then
  log "SLO gate"
  "$CLI" mesh soak-gate "$FINDINGS" --baseline "$ROOT/mesh-soak-baseline.json" || true
fi
[ "$FAILS" = 0 ] && { echo "  PASS ✓ — invariants held across all checkpoints"; exit 0; } \
                 || { echo "  FAIL ✗ — $FAILS checkpoint(s) violated"; exit 1; }
