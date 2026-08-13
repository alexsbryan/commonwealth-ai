#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# RED BASELINE PROBE — order `mesh-scale-t1-red`, bars `t1-expansion-scoped`
# and `t1-prefilter-per-turn`. Spec: MESH_SCALE_100_USERS_1000_CORPORA.md §8.3.
#
# TWO QUESTIONS, one knowledge turn at the Probe B 1000-stub rig:
#   1. How many FULL fan-outs does one knowledge turn issue, and how many
#      corpora does each one search? (bar t1-expansion-scoped)
#   2. How many times does the corpus relevance prefilter run per TURN — once,
#      or once per fan-out? And what does each pass cost?
#      (bar t1-prefilter-per-turn; `corpus_search.rs:266-275` puts the call
#      inside `search_corpus_indexes_with_overrides`, i.e. per fan-out call.)
#
# INSTRUMENT: the shipped `retrieval_audit` glassbox target. One
# `fanout_complete` line per fan-out (`corpus_search.rs:409-418`, carrying
# `label`, `corpora`, `fanout_ms`); one `corpus_prefilter` line per prefilter
# pass (`corpus_search.rs:565-580`). Nothing is added to production code — the
# probe only turns the existing target up and counts its lines.
#
# ISOLATION: same sealed rootless netns as `probe-a-shed-under-load.sh`, and
# the same reason — a daemon that loses a bind only warns, so on the bare host
# a probe can silently drive the operator's live daemon. On top of that this
# probe runs under a THROWAWAY $HOME, because the CLI's Runtime resolves
# installed corpora from `~/.svrnmesh/indexes`: the operator's real corpora
# must not be in the eligible set, and the 1,000 stubs must be.
#
# Usage:
#   scripts/probe-t1-expansion-fanout.sh --rig <dir-with-1000-index-clones> \
#      [--prefilter K] [--question "…"] [--keep]
#
# The rig is built exactly as Probe B builds it (clone one tiny real index N
# times, stamp a unique corpus_id per clone). Pass `--prefilter K` to set
# `SOVEREIGN_CORPUS_PREFILTER_TOPK=K` for the run.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RIG="${RIG:-}"
PREFILTER="${PREFILTER:-}"
QUESTION="${QUESTION:-What does the drive fix change about folder sync, and who signed off on it?}"
# When set, the probe runs an eval BANK through the production retrieval
# pipeline (`eval run --prod-pipeline`) instead of a single chat turn — the
# quality anchor arm of bar `t1-prefilter-per-turn` (distraction at the rig).
EVAL_BANK="${EVAL_BANK:-}"
REPEAT="${REPEAT:-1}"
KEEP="${KEEP:-0}"
# `--set K=V`, repeatable: extra env for the TURN process only (the expansion
# flags are all env-gated and all default off — see
# `retrieval_pipeline.rs:222-256`). Serialized through PROBE_T1_SET so the
# netns re-exec carries it.
declare -a EXTRA_ENV=()
if [[ -n "${PROBE_T1_SET:-}" ]]; then
  IFS=$'\n' read -r -d '' -a EXTRA_ENV < <(printf '%s\0' "$PROBE_T1_SET") || true
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rig)       RIG="$2"; shift 2 ;;
    --prefilter) PREFILTER="$2"; shift 2 ;;
    --question)  QUESTION="$2"; shift 2 ;;
    --set)       EXTRA_ENV+=("$2"); shift 2 ;;
    --turns)     REPEAT="$2"; shift 2 ;;
    --eval-bank) EVAL_BANK="$2"; shift 2 ;;
    --keep)      KEEP=1; shift ;;
    -h|--help)   sed -n '3,38p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$RIG" ]] || { echo "probe-t1: --rig is required (dir containing the index clones)" >&2; exit 2; }
[[ -d "$RIG" ]] || { echo "probe-t1: --rig not a directory: $RIG" >&2; exit 2; }
case "$RIG" in
  */.svrnmesh/indexes|*/.svrnmesh/indexes/) echo "probe-t1: refusing to run against the operator's indexes dir" >&2; exit 2 ;;
esac

if [[ -z "${PROBE_T1_IN_NETNS:-}" ]]; then
  SET_JOINED=""
  ((${#EXTRA_ENV[@]})) && SET_JOINED="$(printf '%s\n' "${EXTRA_ENV[@]}")"
  exec unshare -rn env PROBE_T1_IN_NETNS=1 RIG="$RIG" PREFILTER="$PREFILTER" \
    QUESTION="$QUESTION" KEEP="$KEEP" REPEAT="$REPEAT" EVAL_BANK="$EVAL_BANK" \
    PROBE_T1_SET="$SET_JOINED" bash "$0"
fi
ip link set lo up

CLI="$ROOT/target/debug/sovereign-cli"
LLM="$ROOT/target/debug/sovereign-cli-llm"
for b in "$CLI" "$LLM"; do
  [[ -x "$b" ]] || { echo "probe-t1: $b not built (cargo build --bins)" >&2; exit 1; }
done
PRIMARY="$ROOT/sovereign/models/gemma-4-E4B-it-Q4_K_M.gguf"
EMBED="$ROOT/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf"
for m in "$PRIMARY" "$EMBED"; do
  [[ -f "$m" ]] || { echo "probe-t1: model not found: $m" >&2; exit 1; }
done

CPORT=19741
IPORT=19742
WORK="${TMPDIR:-/tmp}/probe-t1-$$"
FAKE_HOME="$WORK/home"
mkdir -p "$FAKE_HOME/.svrnmesh"
# The rig IS the throwaway home's index set. Symlink rather than copy: the
# search path only reads, and a 94 MB copy per run buys nothing.
ln -s "$RIG" "$FAKE_HOME/.svrnmesh/indexes"
# Count dirs AND symlinks-to-dirs: the sweep driver builds each point as a
# symlink farm, and `-type d` alone would silently report 0 corpora.
N_CORPORA="$(find "$RIG" -maxdepth 1 -mindepth 1 \( -type d -o -type l \) | wc -l)"
cleanup() {
  [[ -n "${DPID:-}" ]] && kill "$DPID" 2>/dev/null || true
  [[ "$KEEP" == 1 ]] || rm -rf "$WORK"
}
trap cleanup EXIT

cat > "$WORK/config.toml" <<EOF
[models]
primary = "$PRIMARY"
embed = "$EMBED"
# 32768, not Probe A's 4096: a knowledge turn carries ~20 evidence chunks, and
# at 4096 synthesis dies with an empty `Inference error:` AFTER retrieval has
# already run — a turn that looks measured and never answered. Caught by
# checking the turn's exit code, which is why it is reported per turn.
context_size = 32768
[daemon]
client_port = $CPORT
internal_port = $IPORT
autostart = true
primary_idle_secs = 1800
extras_idle_secs = 0
freshness_watchers_enabled = false
client_bind = "127.0.0.1"
[data]
dir = "$FAKE_HOME/.svrnmesh"
[iroh]
enabled = false
EOF

echo "probe-t1: netns sealed (loopback only). rig=$RIG corpora=$N_CORPORA"
echo "probe-t1: throwaway HOME=$FAKE_HOME (operator corpora are NOT in the eligible set)"
echo "probe-t1: prefilter=${PREFILTER:-OFF (production default)}"
env HOME="$FAKE_HOME" SOVEREIGN_ALLOW_MULTIPLE_DAEMONS=1 \
  "$CLI" daemon run --config "$WORK/config.toml" > "$WORK/daemon.log" 2>&1 &
DPID=$!
for _ in $(seq 1 300); do
  curl -s -m 2 -o /dev/null "http://127.0.0.1:$CPORT/v1/mesh/status" 2>/dev/null && break
  kill -0 "$DPID" 2>/dev/null || { echo "probe-t1: daemon exited during boot" >&2; tail -20 "$WORK/daemon.log"; exit 1; }
  sleep 1
done

# ── BIND ASSERTION (same shape as Probe A; a probe whose bind check never
# ran is a gate that never ran) ───────────────────────────────────────────────
BIND_OWNER="$(python3 - "$CPORT" <<'PY'
import glob, os, sys
port = int(sys.argv[1]); inode = None
for line in open("/proc/net/tcp").read().splitlines()[1:]:
    f = line.split()
    if f[3] != "0A":
        continue
    if int(f[1].split(":")[1], 16) == port:
        inode = f[9]; break
if inode is None:
    print("NO_LISTENER"); raise SystemExit
for fd in glob.glob("/proc/[0-9]*/fd/*"):
    try:
        if os.readlink(fd) == f"socket:[{inode}]":
            print(fd.split("/")[2]); raise SystemExit
    except OSError:
        continue
print("UNRESOLVED")
PY
)"
echo "probe-t1: BIND CHECK — listener on :$CPORT is pid $BIND_OWNER; daemon pid is $DPID"
[[ "$BIND_OWNER" == "NO_LISTENER" || "$BIND_OWNER" == "UNRESOLVED" ]] && {
  echo "probe-t1: BIND CHECK COULD-NOT-JUDGE — refusing to run" >&2; exit 1; }
python3 - "$BIND_OWNER" "$DPID" <<'PY' || { echo "probe-t1: BIND CHECK FAILED" >&2; exit 1; }
import sys
pid, want = int(sys.argv[1]), int(sys.argv[2]); seen = set()
while pid > 1 and pid not in seen:
    if pid == want:
        raise SystemExit(0)
    seen.add(pid)
    try:
        st = open(f"/proc/{pid}/stat").read()
        pid = int(st[st.rindex(")") + 2:].split()[1])
    except OSError:
        break
raise SystemExit(1)
PY
echo "probe-t1: BIND CHECK PASSED — this turn reaches this probe's daemon and nothing else."

PREFILTER_ENV=()
[[ -n "$PREFILTER" ]] && PREFILTER_ENV=(SOVEREIGN_CORPUS_PREFILTER_TOPK="$PREFILTER")

if [[ -n "$EVAL_BANK" ]]; then
  echo "probe-t1: eval bank $EVAL_BANK through the PRODUCTION retrieval pipeline"
  echo "probe-t1: corpora in the eligible set: $N_CORPORA"
  ET0=$(date +%s.%N)
  set +e
  env HOME="$FAKE_HOME" RUST_LOG="warn,retrieval_audit=info" \
    "${PREFILTER_ENV[@]}" "${EXTRA_ENV[@]}" \
    "$LLM" eval run --bank "$EVAL_BANK" --prod-pipeline \
    --daemon "http://127.0.0.1:$CPORT" > "$WORK/eval.txt" 2> "$WORK/eval.trace"
  EVAL_RC=$?
  set -e
  ET1=$(date +%s.%N)
  echo "PROBE_T1_EVAL bank=$(basename "$(dirname "$EVAL_BANK")") corpora=$N_CORPORA \
prefilter=${PREFILTER:-off} exit=$EVAL_RC wall_s=$(python3 -c "print(f'{$ET1-$ET0:.1f}')")"
  # `--prod-pipeline` prints an "overall" block (sources/facts); the threaded
  # runner prints fact_recall/source_recall lines. Accept either, and say
  # COULD-NOT-JUDGE when neither appears rather than reporting a tidy nothing.
  if ! grep -E "^ *sources +[0-9]+/[0-9]+|^ *facts +[0-9]+/[0-9]+|^fact_recall:|^source_recall:|^wall total:" \
       "$WORK/eval.txt" | sed 's/^ */PROBE_T1_EVAL /'; then
    echo "PROBE_T1_EVAL COULD-NOT-JUDGE — no summary lines in the eval output"
  fi
  echo "PROBE_T1_EVAL fanout_lines=$(grep -c 'retrieval_audit: fanout_complete' "$WORK/eval.trace" || true) \
prefilter_lines=$(grep -c 'retrieval_audit: corpus_prefilter' "$WORK/eval.trace" || true)"
  if [[ "$KEEP" == 1 ]]; then echo "probe-t1: kept $WORK"; fi
  exit 0
fi

echo "probe-t1: warm-up turn (model load is NOT part of the measurement)…"
set +e
env HOME="$FAKE_HOME" "$LLM" chat ask "$QUESTION" \
  --daemon "http://127.0.0.1:$CPORT" > "$WORK/warmup.txt" 2> "$WORK/warmup.trace"
WARM_RC=$?
set -e
echo "probe-t1: warm-up exit=$WARM_RC answer_chars=$(wc -c < "$WORK/warmup.txt")"

# Turns run against ONE daemon boot, and each turn gets its own trace + its
# own report line. They are reported in order rather than averaged: the first
# turn cold-opens every index and the later ones do not, and collapsing that
# into a mean would hide the one number a reader needs (§18.5 — a single run
# is not a measurement, and an average over unlike runs is not one either).
for ((turn = 1; turn <= REPEAT; turn++)); do
  TRACE="$WORK/turn-$turn.trace"
  echo
  echo "probe-t1: turn $turn/$REPEAT — \"$QUESTION\""
  T0=$(date +%s.%N)
  set +e
  env HOME="$FAKE_HOME" \
    RUST_LOG="warn,retrieval_audit=info,retrieval.pipeline=info,sovereign_core=debug" \
    "${PREFILTER_ENV[@]}" "${EXTRA_ENV[@]}" \
    "$LLM" chat ask "$QUESTION" --daemon "http://127.0.0.1:$CPORT" \
    > "$WORK/answer-$turn.txt" 2> "$TRACE"
  ASK_RC=$?
  set -e
  T1=$(date +%s.%N)
  echo "probe-t1: turn $turn exit=$ASK_RC wall=$(python3 -c "print(f'{$T1-$T0:.1f}')")s answer_chars=$(wc -c < "$WORK/answer-$turn.txt")"
  python3 "$ROOT/scripts/probe_t1_fanout_report.py" --trace "$TRACE" \
    --corpora "$N_CORPORA" --turn-wall "$(python3 -c "print(f'{$T1-$T0:.3f}')")" \
    --prefilter "${PREFILTER:-off}" --ask-rc "$ASK_RC" --turn "$turn"
done

if [[ "$KEEP" == 1 ]]; then
  echo "probe-t1: kept $WORK (trace: $TRACE, daemon log: $WORK/daemon.log)"
fi
