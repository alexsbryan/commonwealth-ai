#!/usr/bin/env bash
# ci-bench-lane-verdict.sh — the four verdicts, inside the instrument that is
# supposed to enforce them.
#
# LEDGER GA-05 (note 25a54d70, 2026-08-26). THE SOFT SYNTH LANE SCORED A
# QUESTION THAT NEVER RAN AS 0.0 AND REPORTED IT AS A REGRESSION. The lane
# posted FAIL(3reg) with not one question executed — ARCH §18.1's four verdicts
# (passed, failed, could-not-judge, never-ran) collapsed to two, in the
# instrument. In the same run two lanes went the other way and posted PASS on
# an all-zero tally: `synth:wikipedia` on five errored questions, `enrichment`
# on a corpus that was not installed. A green built on nothing.
#
# The binary half of that fix is tested in-crate
# (`bench_cmd::all::tests::an_all_errored_run_is_unmeasured_not_regressed`).
# This is the OTHER half: the shell parser at the consumption site, which was
# the one that was actually wrong, because `grep -oE "[0-9]+ regressed"` cannot
# tell 0-of-0 from 0-of-30.
#
# It drives `lane_verdict` directly against fixture lane output. No cargo, no
# daemon, no model, no network. Cost: well under a second.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
LIB="$ROOT/scripts/lib/ci-bench-verdict.sh"
[[ -f "$LIB" ]] || { echo "cannot find $LIB"; exit 2; }
# shellcheck source=../lib/ci-bench-verdict.sh
source "$LIB"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
rc=0
pass() { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; rc=1; }

# verdict <name> <rc> <lane output>  -> echoes the status
verdict() { local f="$T/$1.out"; printf '%s\n' "$3" > "$f"; lane_verdict "$2" "$f"; }

want() { # want <label> <expected> <actual>
    if [[ "$3" == "$2" ]]; then pass "$1"; else fail "$1 — wanted '$2', got '$3'"; fi
}

# ── The failure this file exists for ────────────────────────────────────────
# The exact shape from research/engram/bench-flashnext.log: an all-zero tally
# plus the binary's own "unmeasured" parenthetical. Until 2026-08-26 this was
# stamped PASS.
ALL_ERRORED='synth:wikipedia   0 green · 0 improved · 0 regressed · 0 new · 5 stale
0 regressed (unmeasured — every question errored)'
want "a lane where nothing executed is could-not-judge, not a pass" \
     "SKIP(no-data)" "$(verdict all_errored 0 "$ALL_ERRORED")"

# The same claim from the other direction: an all-zero tally with no explicit
# marker is still nothing adjudicated. A corpus that is not installed prints
# this and no parenthetical.
want "an all-zero tally alone is could-not-judge" \
     "SKIP(no-data)" \
     "$(verdict all_zero 0 'enrichment:governance   0 green · 0 improved · 0 regressed · 0 new · 3 stale')"

# ── The other three verdicts must stay distinct from it ─────────────────────
# NEGATIVE CONTROL (ARCH §18.4): a checker that answers SKIP(no-data) to
# everything would pass both bars above for free. These four prove it reads the
# fixture rather than defaulting.
want "a real regression is a failure, named with its count" \
     "FAIL(3reg)" \
     "$(verdict regressed 0 'synth:literary   12 green · 2 improved · 3 regressed · 0 new · 0 stale')"

want "a lane that adjudicated something and held is a pass" \
     "PASS" \
     "$(verdict green 0 'retrieval-prod   27 green · 1 improved · 0 regressed · 0 new · 0 stale')"

want "a pass over a stale bench says so rather than hiding it" \
     "PASS(warn:setup)" \
     "$(verdict stale 0 'retrieval-prod   9 green · 0 improved · 0 regressed · 0 new · 2 stale')"

want "exit 4 is could-not-judge, the same verdict under the same name" \
     "SKIP(no-data)" "$(verdict rc4 4 'no RAPTOR nodes to judge')"

want "a lane that hit its cap is a timeout, not a regression" \
     "TIMEOUT" "$(verdict timeout 124 'partial output')"

want "a non-zero exit with no tally is a failure carrying its code" \
     "FAIL(2)" "$(verdict rc2 2 'bench: could not open the corpus')"

# A lane type that prints no tally at all must not be turned into
# could-not-judge by the absence — it falls back to the explicit marker.
want "no tally line is not the same claim as nothing adjudicated" \
     "PASS" "$(verdict no_tally 0 'chaos: 14 scenarios, 0 crashes')"

# ── SKIP(daemon-down): an infrastructure event is not a regression ──────────
#
# 2026-09-03: a daemon SIGABRTed mid-suite and the eight lanes after it each
# posted FAIL(1)/FAIL(2) and filed a backlog item claiming a regression. The
# code under test never ran.
want "a lane whose daemon died is could-not-judge, not a regression" \
     "SKIP(daemon-down)" \
     "$(verdict daemon_gone 1 'bootstrap failed: Serialization error: daemon unreachable at http://127.0.0.1:9841. Start it with `svrn daemon run`')"

want "the backlog producer's own probe wording is recognised too" \
     "SKIP(daemon-down)" \
     "$(verdict daemon_probe 1 'error: the daemon is not responding at http://127.0.0.1:9841, so nothing can score this item.')"

# NEGATIVE CONTROLS for the new arm — it must not swallow real failures.
want "a zero-exit lane is never downgraded by the phrase appearing in output" \
     "PASS" \
     "$(verdict daemon_mentioned 0 'note: daemon unreachable at startup, retried ok
retrieval   9 green · 0 improved · 0 regressed · 0 new · 0 stale')"

want "a genuine regression is still a regression, daemon or not" \
     "FAIL(3reg)" \
     "$(verdict daemon_irrelevant 0 'synth   1 green · 0 improved · 3 regressed · 0 new · 0 stale')"

want "a non-zero exit WITHOUT the daemon marker keeps its own failure code" \
     "FAIL(2)" \
     "$(verdict other_failure 2 'bench: corpus index is missing')"

exit "$rc"
