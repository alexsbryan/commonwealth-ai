#!/usr/bin/env bash
# THE EVIDENCE ARM — "does showing the writer the evidence recover what the
# port lost?"  drb1-r9, task 69.
#
#   fly-evidence-arm.sh            # 28/5, then 1 pinned control, then 28/5
#   fly-evidence-arm.sh --wide-reps 2
#   fly-evidence-arm.sh --status
#
# THE QUESTION, with a prior number attached. compose_report was ported to Rust
# in a50d2fdf3 from arms/lab/compose2.py, whose 44.40 composite was measured at
# k=28 repeat_cap=5 (39,200 evidence chars per section). The port shipped 8/3 —
# 11,200 chars, a 3.5x cut — with no commit, note or ledger row recording it as
# a decision. The shipped Rust path has NEVER run at the configuration whose
# measured quality justified building it. This runs it there.
#
# WHERE THE REPLICATE GOES, AND WHY NOT ON THE CONTROL. The run-to-run spread
# is ~7 points (86ac6f7c) and every A/B before this measured 2-3 point levers
# with n=2 against it — underpowered by construction. So one cell must be
# REPEATED to measure what SOVEREIGN_DR_PIN_SAMPLING is worth: two identical
# runs, their spread IS the pin's determinism. Within ~1.5 points the pin is
# validated; wider and the arm cannot conclude, and says so rather than
# reading noise.
#
# The first cut put that replicate on the CONTROL, spending two of three cells
# re-measuring 8/3 — a configuration already known to be a PORT REGRESSION
# (a50d2fdf3 shipped 8/3; the prototype whose 44.40 composite justified the
# port used 28/5). That is 90 minutes confirming the nozzle is small. The
# pin's determinism does not depend on WHICH configuration measures it, so the
# replicate belongs on the cell we want to know about. ONE pinned control —
# the existing 45.63 cannot serve, it was measured UNPINNED and on an older
# binary — then TWO at 28/5, with the control flown BETWEEN them so session
# drift shows up as the replicate spread rather than hiding in it.
#
# SINGLE VARIABLE. SOVEREIGN_DR_REPORT_ARCHITECTURE (r8) stays dark: it changes
# the deliverable's SHAPE while this changes how much evidence stands behind
# each part, and an arm that moves both cannot tell them apart.
set -u
cd /home/alexbryan/dev/commonwealth-ai
RUNS=research/deep-research/arms/runs-ceiling
LOGDIR=research/deep-research/arms/runs-aiq-bar
WIDE_REPS=2
case "${1:-}" in
  --status)
    for a in pinned-control wide-28-5; do
      echo "== $a =="
      for r in research/deep-research/drb/overall-derivation/flights-ceiling/$a/*.record.json; do
        [ -f "$r" ] || continue
        printf "   %-14s %s\n" "$(basename "$r" .record.json)" \
          "$(grep -o '"overall_score": [0-9.]*' "$r" | head -1 | awk '{printf "%.2f", $2*100}')"
      done
    done
    exit 0;;
  --wide-reps) WIDE_REPS=$2;;
esac

# A build in flight rewrites target/debug UNDER the flight. Two cells compiled
# from different trees are not two cells of one arm (§18.4).
if pgrep -f "cargo build" >/dev/null 2>&1; then
  echo "REFUSED: a cargo build is running — target/debug is still moving."; exit 2
fi

# Bring the daemon up if it is down, and probe READINESS, not liveness:
# /v1/models answering 200 says the process is alive, not that it can serve
# (§9.5, note 160268d0 — the daemon SHEDS, a 15ms 503 means the slot is busy).
if ! systemctl --user is-active --quiet sovereign.service; then
  echo "daemon down — starting"; systemctl --user start sovereign.service
fi
echo "waiting for the daemon to SERVE (1-token completion, not /v1/models)…"
for i in $(seq 1 120); do
  code=$(curl -s -m 60 -o /dev/null -w '%{http_code}' \
    http://127.0.0.1:9741/v1/chat/completions -H 'Content-Type: application/json' \
    -d '{"model":"Qwen3.5-4B-UD-MTP-Q6_K_XL","max_tokens":1,
         "messages":[{"role":"user","content":"ok"}]}' 2>/dev/null)
  [ "$code" = "200" ] && { echo "  serving after ${i} probe(s)"; break; }
  sleep 10
done
[ "${code:-}" = "200" ] || { echo "REFUSED: daemon never served a token"; exit 2; }

# A daemon START spawns `rust-analyzer scip .` (~11 GiB). On a host whose
# measured wall is ~55 GiB that is the difference between a flight fitting and
# OOM-killing the daemon. NEVER kill it — a half-killed export wipes the
# code-intel graph. Wait.
while pgrep -f "rust-analyzer scip" >/dev/null 2>&1; do
  echo "  waiting out a rust-analyzer scip export (~11 GiB competitor)…"; sleep 60
done

TS=$(date +%Y%m%dT%H%M%S)
run_cell () {   # <arm> <reps> <env...>
  local arm=$1 reps=$2; shift 2
  local log=$LOGDIR/arm-$arm-$TS.log
  echo "=== $arm (reps $reps) -> $log ==="
  local args=(); for e in "$@"; do args+=(--env "$e"); done
  ./research/deep-research/arms/bed/run-ceiling.sh "$arm" --reps "$reps" \
     --task 69 "${args[@]}" 2>&1 | tee "$log" | grep -E "flew|scored|REACH|WITNESS|REFUS|NOT WITNESSED|RESULT|mean"
}

run_cell wide-28-5 1 SOVEREIGN_DR_PIN_SAMPLING=1 SOVEREIGN_DR_REPORT_SECTION_EVIDENCE=1
run_cell pinned-control 1 SOVEREIGN_DR_PIN_SAMPLING=1
run_cell wide-28-5 "$WIDE_REPS" SOVEREIGN_DR_PIN_SAMPLING=1 \
                                SOVEREIGN_DR_REPORT_SECTION_EVIDENCE=1
echo; echo "=== ARM COMPLETE $(date -Is) ==="
./research/deep-research/arms/bed/fly-evidence-arm.sh --status
