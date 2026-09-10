#!/bin/bash
# map-conversion rung 6: the judged SEP synth lane, both arms, sequential.
# Arm B = current binary, converted stores + written maps (declared rows).
# Arm A = pre-rung-3 binary (9d3a94831 in a worktree, private target) on the
#         SAME converted stores with every sep-*/atlas/ontology.json hidden
#         for its duration (pre-registered rows). Restored on exit, even on failure.
#
# GUARD (measured 2026-09-08, three kills): the kernel OOM-killed the daemon
# (exit 137) each time arm B's first synthesis loaded the 35B. The box is 125 GB;
# with every slot resident the daemon holds ~20 GB anon + ~72 GB GPU-pinned
# system memory (GTT — invisible to process RSS and to MemAvailable until the
# model is loaded), the IDE's two rust-analyzers ~22 GB, swap 8 GB and full,
# leaving ~17 GB; a peer session's `cargo check --workspace` (~15-20 GB) was
# resident at every kill. So: the 35B is WARMED before the guard (its memory
# is then already counted), the guard wants no cargo/rustc/scip and
# MemAvailable >= $NEED_GB, and an arm whose turns errored is FAILED (eval run
# exits 0 over them) and retried up to $ATTEMPTS times once the daemon is back.
set -u
cd /home/alexbryan/dev/commonwealth-ai
D=sovereign/bench/sep_atlas/map-conversion-rung6
I=$HOME/.svrnmesh/indexes
export RUST_LOG=warn,retrieval_audit=debug
BANK=sovereign/bench/sep/questions.toml
ARGS="eval run --bank $BANK --synth --format json --isolate"
NEED_GB=${NEED_GB:-12}
ATTEMPTS=${ATTEMPTS:-3}

restore() { for f in "$I"/sep-*/atlas/ontology.json.armA-hidden; do [ -e "$f" ] && mv "$f" "${f%.armA-hidden}"; done; }
trap restore EXIT

quiet() {
  local avail busy
  avail=$(( $(awk '/MemAvailable/{print $2}' /proc/meminfo) / 1024 / 1024 ))
  busy=$(pgrep -c -f 'bin/cargo |rustc |rust-analyzer scip' || true)
  [ "$busy" -eq 0 ] && [ "$avail" -ge "$NEED_GB" ] && return 0
  echo "guard: waiting — cargo/rustc/scip procs=$busy MemAvailable=${avail}GB need=${NEED_GB}GB $(date +%H:%M:%S)"
  return 1
}
wait_quiet() { until quiet; do sleep 30; done; echo "guard: quiet at $(date +%H:%M:%S)"; }
daemon_up() { sovereign daemon status 2>/dev/null | grep -q 'daemon running'; }
verdict() { # $1=arm $2=rc
  local errs; errs=$(grep -c 'turn: Inference error' "$D/$1.log" || true)
  if [ "$2" -ne 0 ] || [ "$errs" -gt 0 ]; then echo "ARM $1 FAILED: rc=$2 turns_errored=$errs"; return 1; fi
  echo "ARM $1 OK: rc=$2 turns_errored=0"
}

warm() { # load the 35B so its memory is on the books before the guard reads them
  curl -s -m 900 localhost:9741/v1/chat/completions -H 'content-type: application/json' \
    -d '{"model":"primary","messages":[{"role":"user","content":"Say ok."}],"max_tokens":4}' >/dev/null \
    && echo "warm: 35B resident $(date +%H:%M:%S)"
}
wait_daemon() { until daemon_up; do echo "guard: daemon down, waiting $(date +%H:%M:%S)"; sleep 30; done; }

run_arm() { # $1=arm $2=binary ; returns 0 on a clean arm
  local arm=$1 bin=$2 attempt rc S
  for attempt in $(seq 1 "$ATTEMPTS"); do
    wait_daemon; warm; wait_quiet
    echo "arm $arm attempt $attempt starting $(date +%H:%M:%S)"
    S=$(date +%s)
    "$bin" $ARGS --output "$D/$arm.json" > "$D/$arm.stdout" 2> "$D/$arm.log"
    rc=$?
    echo "WALL $arm: $(( $(date +%s) - S ))s rc=$rc attempt=$attempt" | tee -a "$D/$arm.log"
    verdict "$arm" "$rc" && return 0
    cp "$D/$arm.log" "$D/$arm.attempt$attempt.log"
  done
  return 1
}

# Arms by name: armB* = current binary on the written maps; armA* = the
# pre-rung-3 binary with the maps hidden. A suffix (armB2) is a repeat for n=2.
# Default order: armB armA. `run-arms.sh armB2` runs one repeat.
ARMS=${*:-armB armA}
for arm in $ARMS; do
  case $arm in
    armB*) run_arm "$arm" target/debug/sovereign-cli-llm || exit 3 ;;
    armA*)
      for f in "$I"/sep-*/atlas/ontology.json; do mv "$f" "$f.armA-hidden"; done
      echo "$arm: hidden $(ls "$I"/sep-*/atlas/ontology.json.armA-hidden | wc -l) maps"
      run_arm "$arm" /home/alexbryan/dev/cw-armA-target/debug/sovereign-cli-llm; rc=$?
      restore
      echo "$arm: restored $(ls "$I"/sep-*/atlas/ontology.json | wc -l) maps"
      [ $rc -eq 0 ] || exit 3 ;;
    *) echo "unknown arm $arm"; exit 2 ;;
  esac
done
