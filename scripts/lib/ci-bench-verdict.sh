#!/usr/bin/env bash
# ci-bench-verdict.sh — one decider for "what verdict did this lane earn?",
# extracted from `sovereign-ci-bench.sh::run_lane` so it can be driven by a
# test.
#
# WHY THIS EXISTS
#
# The logic below is the four-verdict discipline (ARCH §18.1) living inside the
# instrument: passed, failed, could-not-judge, never-ran. It was written after
# a run where the SOFT synth lane scored questions that NEVER EXECUTED as 0.0
# and reported FAIL(3reg) with not one question run — the four verdicts
# collapsed to two inside the thing that was supposed to enforce them (ledger
# GA-05, note 25a54d70, 2026-08-26).
#
# It sat inline in a 735-line straight-line script that cannot be sourced, so
# nothing could reach it. A rule this hard-won, guarding a failure this quiet,
# with no test on it, is the exact shape of the thing it was written to catch.
# The extraction is the seam; `scripts/tests/ci-bench-lane-verdict.sh` is the
# gate. The body is VERBATIM from run_lane — this moved code, it did not
# rewrite it.
#
# Contract:
#   lane_verdict <rc> <lane-output-file>   ->  echoes one status string
#
# Status strings, and what each claims:
#   PASS              a real baseline comparison happened and nothing regressed
#   PASS(warn:setup)  same, but some bench was stale or first-run
#   FAIL(Nreg)        N items regressed against the baseline
#   FAIL(rc)          the lane exited non-zero for a reason that is not N-regressed
#   TIMEOUT           the lane hit its cap
#   SKIP(no-data)     COULD-NOT-JUDGE — the lane ran and adjudicated nothing
#
# `SKIP(no-data)` is the load-bearing one and it is deliberately NOT a pass:
# a HARD lane still fails on it, via the `PASS*` test at the call site, because
# for a gated corpus "suddenly nothing to judge" is a regression signal.

lane_verdict() {
  local rc="$1" out="$2"
  local status
local regressed; regressed=$(grep -oE "[0-9]+ regressed" "$out" 2>/dev/null | grep -oE "^[0-9]+" | tail -1)
if (( rc == 124 )); then
  status="TIMEOUT"
elif (( rc == 4 )); then
  # COULD-NOT-JUDGE, not FAILED — the lane ran and had nothing to verify.
  # Exit 4 means exactly this and nothing else in the bench family (both
  # sites are bench_cmd/faithfulness.rs: "no RAPTOR nodes" and "zero claims
  # judged — nothing verified is not a pass"). Distinguishing it matters
  # now that the faithfulness lane discovers every enriched corpus: a corpus
  # whose nodes are all sentinel-filtered would otherwise post a red FAIL(4)
  # every run and train everyone to ignore the lane.
  #
  # A HARD lane still fails on it, via the PASS* test below — for a gated
  # corpus, "suddenly nothing to judge" is a regression signal, not a pass.
  status="SKIP(no-data)"
elif [[ -n "$regressed" ]]; then
  if (( regressed == 0 )); then
    # "0 regressed" is NOT the same claim as "nothing regressed". A lane that
    # adjudicated NOTHING also prints zero, and until 2026-08-26 that was
    # stamped PASS. Two real lanes did it in one run (Flash-Next bench,
    # research/engram/bench-flashnext.log):
    #   synth:wikipedia   0 green · 0 improved · 0 regressed · … · 5 stale
    #                     with "0 regressed (unmeasured — every question errored)"
    #   enrichment:...    same all-zero tally, corpus not installed locally
    # Both posted PASS(warn:setup) — a green built on five errored questions
    # and an absent corpus. The BINARY is right (bench_cmd/all.rs:1153 prints
    # the "unmeasured" parenthetical, and `an_all_errored_run_is_unmeasured_
    # not_regressed` tests it); only this parser was wrong, because
    # `grep -oE "[0-9]+ regressed"` cannot tell 0-of-0 from 0-of-30.
    #
    # So: prove the lane adjudicated something before calling it a pass.
    # green/improved/regressed are the three outcomes that mean a real
    # baseline comparison happened; first-run/no-baseline/stale all mean
    # "could not compare" and must not, alone, carry a PASS. Verified against
    # this run's seven tallies — the five genuine passes each have
    # green+improved ≥ 3, both false passes have all three at zero.
    local tally adjudicated n_green n_improved n_regressed
    tally=$(grep -oE "[0-9]+ green · [0-9]+ improved · [0-9]+ regressed" "$out" 2>/dev/null | tail -1)
    if [[ -n "$tally" ]]; then
      # Parse by LABEL, not by column. The separator is " · " and awk splits
      # the middle dot into its own field, so $1/$3/$5 read "3 + · + improved"
      # — an arithmetic error that would have made every lane could-not-judge
      # and failed every HARD gate. Keyed on the word, column drift is moot.
      n_green=$(grep -oE "[0-9]+ green" <<<"$tally" | grep -oE "^[0-9]+")
      n_improved=$(grep -oE "[0-9]+ improved" <<<"$tally" | grep -oE "^[0-9]+")
      n_regressed=$(grep -oE "[0-9]+ regressed" <<<"$tally" | grep -oE "^[0-9]+")
      adjudicated=$(( ${n_green:-0} + ${n_improved:-0} + ${n_regressed:-0} ))
    else
      # No tally line at all (lane types that don't print one) — fall back to
      # the explicit all-errored marker rather than inventing a verdict.
      adjudicated=1
    fi
    if grep -qF "unmeasured — every question errored" "$out" 2>/dev/null; then
      adjudicated=0
    fi
    if (( adjudicated == 0 )); then
      # Same verdict, same name as the rc==4 arm above: could-not-judge.
      # One concept, one status string — and a HARD lane still fails on it
      # via the PASS* test below, which is the point.
      status="SKIP(no-data)"
    elif grep -qE "[1-9][0-9]* (stale|first-run)" "$out" 2>/dev/null; then
      status="PASS(warn:setup)"  # 0 regressed, but a bench was stale/first-run
    else
      status="PASS"
    fi
  else
    status="FAIL(${regressed}reg)"
  fi
elif (( rc == 0 )); then
  status="PASS"
else
  status="FAIL($rc)"
fi
  printf '%s' "$status"
}
