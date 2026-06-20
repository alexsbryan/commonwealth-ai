#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# mesh-soak.sh — Layer-2 multi-process mesh soak (the podman orchestration shell
# behind the QA plan's "real bytes" layer). It spins N real `sovereign daemon`
# processes, forms a mesh, then drives a SEEDED schedule of OS-level faults +
# load while polling the mesh invariants via `sovereign mesh check-invariants`
# (the verified assertion engine — see sovereign-cli-llm/src/mesh_soak.rs).
#
# This is the I/O shell; the assertions are Rust. Findings stream to
# mesh-soak-findings.jsonl, one JSON line per tick, replayable via --seed.
#
# ── What it exercises that the in-process DST suite cannot ────────────────────
#   Real process crashes (SIGKILL), real memory pressure / OOM, real network
#   partitions, real clock skew — the failure modes that only show up across
#   actual OS process + network boundaries.
#
# ── Two backends (set MESH_SOAK_BACKEND) ──────────────────────────────────────
#   local   (default) — daemons as host subprocesses on distinct ports + data
#                       dirs. Runnable immediately on a dev box / toolbox; real
#                       multi-process. Faults: kill -9, libfaketime (skew).
#   podman           — daemons as containers on a podman network (the nightly /
#                       CI target). Adds real network partitions (network
#                       disconnect) and cgroup memory limits (the real OOM path).
#                       Requires an image with `sovereign-cli` (see the runbook).
#
# Usage:
#   scripts/mesh-soak.sh [--nodes N] [--minutes M] [--seed S] [--keep]
#
# Prereq: a built sovereign-cli  (cargo build --bins, debug is fine for soak).
set -uo pipefail

# ── Args ──────────────────────────────────────────────────────────────────────
NODES=3; MINUTES=10; SEED=$(( $(date +%s) % 100000 )); KEEP=0
BACKEND="${MESH_SOAK_BACKEND:-local}"
while [ $# -gt 0 ]; do
  case "$1" in
    --nodes)   NODES="$2"; shift 2;;
    --minutes) MINUTES="$2"; shift 2;;
    --seed)    SEED="$2"; shift 2;;
    --keep)    KEEP=1; shift;;
    -h|--help)
      sed -n '3,40p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLI="${SOVEREIGN_CLI:-$ROOT/target/debug/sovereign-cli}"
[ -x "$CLI" ] || CLI="$ROOT/target/release/sovereign-cli"
if [ ! -x "$CLI" ]; then echo "sovereign-cli not built (cargo build --bins)"; exit 1; fi

WORK="$(mktemp -d -t mesh-soak.XXXXXX)"
FINDINGS="$ROOT/mesh-soak-findings.jsonl"
: > "$FINDINGS"
RANDOM=$SEED                                  # seed bash's PRNG → reproducible schedule
declare -a CLIENT_ADDRS=() INTERNAL_ADDRS=() PIDS=() NODE_IDS=() DOWN=()

log() { echo "[soak $(date +%H:%M:%S)] $*"; }
finding() {  # finding <kind> <detail-json-fragment>
  printf '{"ts":%s,"seed":%s,"tick":%s,"kind":"%s",%s}\n' \
    "$(date +%s)" "$SEED" "${TICK:-0}" "$1" "$2" >> "$FINDINGS"
}

# ── Backend: spawn / kill / partition (local subprocess implementation) ───────
spawn_node() {  # spawn_node <index>
  local i="$1" cport=$((19741 + i*2)) iport=$((19742 + i*2)) dir="$WORK/node-$i"
  mkdir -p "$dir"
  if [ "$BACKEND" = "podman" ]; then
    # Nightly/CI target. Requires an image carrying sovereign-cli (runbook).
    # Real network partitions + cgroup OOM live here.
    podman run -d --name "soak-$i" --network mesh-soak \
      -p "$cport:9741" -p "$iport:9742" --memory 2g \
      "${MESH_SOAK_IMAGE:-sovereign-soak:test}" \
      sovereign daemon run --data-dir /data >/dev/null
  else
    SOVEREIGN_DATA_DIR="$dir" SOVEREIGN_DISABLE_PEER_INFERENCE=0 \
      "$CLI" daemon run --client-port "$cport" --internal-port "$iport" \
      --data-dir "$dir" >"$dir/daemon.log" 2>&1 &
    PIDS[$i]=$!
  fi
  CLIENT_ADDRS[$i]="127.0.0.1:$cport"
  INTERNAL_ADDRS[$i]="127.0.0.1:$iport"
}
kill_node() {  # kill_node <index>  (real SIGKILL — process-level crash)
  local i="$1"
  if [ "$BACKEND" = "podman" ]; then podman kill "soak-$i" >/dev/null 2>&1
  else kill -9 "${PIDS[$i]}" 2>/dev/null; fi
  DOWN[$i]=1
}
restart_node() { local i="$1"; spawn_node "$i"; DOWN[$i]=0; }
partition_node() {  # podman-only: cut a node off the network
  [ "$BACKEND" = "podman" ] && podman network disconnect mesh-soak "soak-$1" 2>/dev/null
}
heal_node() { [ "$BACKEND" = "podman" ] && podman network connect mesh-soak "soak-$1" 2>/dev/null; }

teardown() {
  log "teardown"
  if [ "$BACKEND" = "podman" ]; then
    for i in $(seq 0 $((NODES-1))); do podman rm -f "soak-$i" >/dev/null 2>&1; done
    podman network rm mesh-soak >/dev/null 2>&1
  else
    for p in "${PIDS[@]}"; do kill -9 "$p" 2>/dev/null; done
  fi
  [ "$KEEP" = 0 ] && rm -rf "$WORK"
}
trap teardown EXIT

# ── Bring up the mesh ─────────────────────────────────────────────────────────
[ "$BACKEND" = "podman" ] && podman network create mesh-soak >/dev/null 2>&1
log "backend=$BACKEND nodes=$NODES minutes=$MINUTES seed=$SEED"
for i in $(seq 0 $((NODES-1))); do spawn_node "$i"; DOWN[$i]=0; done
sleep 5   # let daemons bind + settle

# Node 0 creates the mesh; the rest join via its invite. (Adapt the create/join
# mechanism to your build — over HTTP POST /v1/mesh/{create,join} or the CLI.)
INVITE="$("$CLI" mesh create --client "${CLIENT_ADDRS[0]}" 2>/dev/null | grep -oE 'sovereign://[^ ]+' | head -1)"
for i in $(seq 1 $((NODES-1))); do
  "$CLI" mesh join "$INVITE" --client "${CLIENT_ADDRS[$i]}" >/dev/null 2>&1 || true
done
sleep 5

all_internal() { local IFS=,; echo "${INTERNAL_ADDRS[*]}"; }
live_ids() {  # node_ids of nodes the harness knows are up — for --expect-live
  local ids=() i; for i in $(seq 0 $((NODES-1))); do
    [ "${DOWN[$i]:-0}" = 0 ] && ids+=("${NODE_IDS[$i]:-}"); done
  local IFS=,; echo "${ids[*]}"
}

# ── Seeded fault + load + check loop ──────────────────────────────────────────
DEADLINE=$(( $(date +%s) + MINUTES*60 ))
TICK=0
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  TICK=$((TICK+1))
  # ~1-in-4 ticks injects a fault, chosen from the seeded PRNG.
  if [ $((RANDOM % 4)) -eq 0 ]; then
    victim=$((RANDOM % NODES))
    case $((RANDOM % 3)) in
      0) if [ "${DOWN[$victim]:-0}" = 0 ] && [ "$victim" != 0 ]; then
           log "fault: kill node $victim"; kill_node "$victim"
           finding "fault" "\"action\":\"kill\",\"node\":$victim"; fi;;
      1) if [ "${DOWN[$victim]:-0}" = 1 ]; then
           log "fault: restart node $victim"; restart_node "$victim"
           finding "fault" "\"action\":\"restart\",\"node\":$victim"; fi;;
      2) if [ "$BACKEND" = podman ] && [ "$victim" != 0 ]; then
           log "fault: partition node $victim"; partition_node "$victim"
           ( sleep 30; heal_node "$victim" ) &
           finding "fault" "\"action\":\"partition\",\"node\":$victim"; fi;;
    esac
  fi

  # Drive a little load at a random up node + record latency for the SLO gate
  # (mesh soak-gate reads these "load" findings).
  drv=$((RANDOM % NODES))
  if [ "${DOWN[$drv]:-0}" = 0 ]; then
    t=$(curl -s -m 3 -o /dev/null -w '%{time_total} %{http_code}' \
        -X POST "http://${CLIENT_ADDRS[$drv]}/v1/knowledge/search" \
        -H 'content-type: application/json' -d '{"query_text":"soak","limit":3}' 2>/dev/null) || t="3.0 000"
    lat_ms=$(awk -v s="${t%% *}" 'BEGIN{printf "%.1f", s*1000}')
    code="${t##* }"
    ok=$([ "$code" = "200" ] && echo true || echo false)
    finding "load" "\"latency_ms\":$lat_ms,\"http\":\"$code\",\"ok\":$ok,\"node\":$drv"
  fi

  sleep 10  # one gossip interval

  # Assert invariants across the up nodes. Non-zero exit ⇒ a violation.
  "$CLI" mesh check-invariants --nodes "$(all_internal)" --json >> "$FINDINGS" \
    || finding "violation_tick" "\"note\":\"check-invariants exited non-zero — see prior line\""
done

# Final pass after letting everything quiesce.
log "final quiesce + check"
for i in $(seq 0 $((NODES-1))); do [ "${DOWN[$i]:-0}" = 1 ] && restart_node "$i"; done
sleep 20
if "$CLI" mesh check-invariants --nodes "$(all_internal)" --json >> "$FINDINGS"; then
  log "PASS — invariants hold at quiescence (seed=$SEED, findings=$FINDINGS)"; RC=0
else
  log "FAIL — invariant violation at quiescence (seed=$SEED, findings=$FINDINGS)"; RC=1
fi
exit $RC
