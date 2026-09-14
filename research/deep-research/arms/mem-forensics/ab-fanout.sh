#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# ab-fanout.sh — does an UNSCOPED knowledge turn (every installed corpus) retain
# memory a scoped one does not, and which allocation sites hold it?
#
# The 2026-09-13 chaos soak put every anon step on a wide corpus fan-out, but a
# whole-run heap readout is dominated by boot-time costs and cannot say what the
# fan-out allocated. Two fresh daemons under heaptrack, identical but for the
# treatment, cancel the boot costs; `heaptrack_print -f B -d A` keeps the rest.
#
#   arm A: the questions below, each `svrn chat ask --corpus $SCOPE`
#   arm B: the same questions, unscoped
#
# Stops the service daemon first and restarts it at the end. Run from the HOST
# (it drives `toolbox run` itself); ~10 min per arm.
#   research/deep-research/arms/mem-forensics/ab-fanout.sh <outdir>
set -uo pipefail
OUT=${1:?usage: ab-fanout.sh <outdir>}
SCOPE=${SCOPE:-alignment}
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(git -C "$HERE" rev-parse --show-toplevel)
SOV=${SOV:-sovereign}
QUESTIONS=(
  "What are the main open problems in AI alignment research?"
  "How do protocols for AI agent interoperability handle tool permissions?"
  "What evidence is there about the effects of minimum wage increases on employment?"
  "Summarize the history of the Byzantine Empire's decline."
)

[ -f /run/.containerenv ] && { echo "refusing: run from the host — this script drives toolbox run" >&2; exit 2; }
mkdir -p "$OUT"
log() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/ab.log"; }

daemon_pid() { pgrep -x sovereign-cli-d | head -1; }
anon_gib() { awk '$1=="RssAnon:"{printf "%.3f", $2/1048576}' "/proc/$1/status" 2>/dev/null; }

wait_healthy() {
  for _ in $(seq 1 300); do
    [ "$(curl -s -m 3 -o /dev/null -w '%{http_code}' localhost:9741/v1/models)" = 200 ] && return 0
    sleep 2
  done
  return 1
}

run_arm() {
  local arm=$1 scope_flag=$2 dir="$OUT/$1"
  mkdir -p "$dir"
  log "arm $arm: stopping service daemon"
  $SOV daemon stop >>"$OUT/ab.log" 2>&1
  for _ in $(seq 1 30); do [ -z "$(daemon_pid)" ] && break; sleep 1; done
  [ -n "$(daemon_pid)" ] && { log "arm $arm: REFUSING — a daemon is still running"; return 1; }

  log "arm $arm: starting daemon under heaptrack"
  toolbox run -c sovereign-vulkan "$HERE/heaptrack-daemon.sh" "$dir" >"$dir/daemon.log" 2>&1 &
  local wrapper=$!
  if ! wait_healthy; then log "arm $arm: daemon never answered /v1/models"; return 1; fi
  local pid; pid=$(daemon_pid)
  log "arm $arm: healthy pid=$pid anon=$(anon_gib "$pid")"

  # Warm-up: one scoped ask in BOTH arms, so model loads and first-use caches are
  # paid before the measured window rather than inside it.
  $SOV chat ask --corpus "$SCOPE" "What is this corpus about?" >"$dir/warmup.txt" 2>&1
  local warm; warm=$(anon_gib "$pid")
  log "arm $arm: warm anon=$warm"
  printf 'q\tbefore_gib\tafter_gib\trc\n' >"$dir/asks.tsv"
  local i=0
  for q in "${QUESTIONS[@]}"; do
    i=$((i + 1))
    local before after rc
    before=$(anon_gib "$pid")
    # shellcheck disable=SC2086
    $SOV chat ask $scope_flag "$q" >"$dir/ask-$i.txt" 2>&1; rc=$?
    sleep 20   # let allocations made after the answer streams land before sampling
    after=$(anon_gib "$pid")
    printf '%s\t%s\t%s\t%s\n' "$i" "$before" "$after" "$rc" >>"$dir/asks.tsv"
    log "arm $arm: ask $i rc=$rc anon $before -> $after"
  done
  log "arm $arm: post-warm growth $(awk -v w="$warm" -v e="$(anon_gib "$pid")" 'BEGIN{printf "%+.3f GiB", e-w}')"
  grep -hcE 'opening index' "$dir/daemon.log" | xargs -I{} echo "{}" >"$dir/index-opens.txt"
  log "arm $arm: index opens logged $(cat "$dir/index-opens.txt")"

  log "arm $arm: SIGTERM daemon, waiting for heaptrack to close the trace"
  kill -TERM "$pid"
  wait "$wrapper"
  log "arm $arm: trace closed ($(stat -c %s "$dir/heaptrack.daemon.zst" 2>/dev/null) bytes)"
}

run_arm A "--corpus $SCOPE" && run_arm B ""
status=$?
log "restarting service daemon"
$SOV daemon start >>"$OUT/ab.log" 2>&1
log "done status=$status"
exit $status
