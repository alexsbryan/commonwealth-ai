#!/usr/bin/env bash
# cli-journey-selftest.sh — negative controls for cli-journey-verify.sh.
#
# A test harness nobody has seen FAIL is not evidence of anything. The CLI
# contract's previous live layer (cli-contract-live-verify.sh) was written,
# documented as "safe to call unconditionally in CI", and then never called —
# `SOVEREIGN_LIVE_CONTRACT` appears nowhere but inside the script that reads
# it. Its four probes asserted `exit == 0` and nothing else. So before the
# journey runner is trusted, it has to demonstrate, on staged input, that:
#
#   1. it PASSES a sequence that genuinely works
#   2. it FAILS a wrong exit code
#   3. it FAILS a missing expected substring
#   4. it FAILS a mutation that did NOT reverse (the stdout_absent check —
#      the assertion class the old harness had no way to express)
#   5. it ABORTS the remainder of a journey after a failed step, rather than
#      reporting a wall of consequential noise
#   6. it substitutes {placeholders} as SINGLE argv elements, so a fixture
#      containing spaces does not word-split into "unexpected argument"
#   7. stderr noise cannot satisfy a stdout assertion
#   8. a journey that executed ZERO steps reports ∅, never ✓ — and the
#      identical journey WITH its fixture supplied passes, so the ∅ is
#      discriminating rather than blanket
#   9. the summary reports step COVERAGE (ran/declared), because a journey
#      count cannot distinguish 57 steps proven from 28
#  10. a journey whose every step asserts NOTHING reports ⊘ unproven, never ✓
#      — and one asserting step is enough to earn the tick, which the tick then
#      qualifies with how many of its steps asserted nothing
#
# Runs entirely on a stub binary and a stub daemon: no models, no real
# daemon, no network beyond loopback. Safe in CI (ARCH_PRINCIPLES §12.4).
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="$HERE/../cli-journey-verify.sh"
[ -x "$RUNNER" ] || { echo "selftest: $RUNNER not executable"; exit 2; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"; [ -n "${STUB_PID:-}" ] && kill "$STUB_PID" 2>/dev/null' EXIT

# ── stub daemon: just enough for the runner's reachability probe ─────────
PORT=19787
python3 - "$PORT" <<'PY' &
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Type","application/json")
        self.end_headers(); self.wfile.write(b'{"data":[]}')
    def log_message(self, *a): pass
HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
STUB_PID=$!
for _ in $(seq 1 50); do
  curl -fsS -m 1 "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && break
  sleep 0.1
done

# ── stub CLI: scripted output driven by the argv it receives ─────────────
# `ok <text>`      -> prints text, exit 0
# `boom`           -> prints "kaboom", exit 3
# `echoargs …`     -> prints one line per argv element (proves no splitting)
# `errout <text>`  -> prints text on STDERR only, exit 0 (silent on stdout)
STUB="$WORK/stub-cli"
cat > "$STUB" <<'SH'
#!/usr/bin/env bash
case "${1:-}" in
  ok)       shift; printf '%s\n' "$*"; exit 0 ;;
  boom)     echo "kaboom"; exit 3 ;;
  echoargs) shift; for a in "$@"; do echo "ARG[$a]"; done; exit 0 ;;
  errout)   shift; printf '%s\n' "$*" >&2; exit 0 ;;
  *)        echo "stub: unhandled $*"; exit 9 ;;
esac
SH
chmod +x "$STUB"

run_case() { # plan-file extra-args... ; echoes output, sets RC
  OUT="$(SOVEREIGN_LIVE_JOURNEYS=1 \
      SOVEREIGN_JOURNEY_ISOLATED=1 \
      SOVEREIGN_BIN="$STUB" \
      SOVEREIGN_DAEMON_URL="http://127.0.0.1:$PORT" \
      SOVEREIGN_JOURNEY_PLAN="$1" \
      "$RUNNER" "${@:2}" 2>&1)"
  RC=$?
}

fails=0
check() { # description expected-rc must-contain
  local desc="$1" want_rc="$2" needle="${3:-}"
  local bad=""
  [ "$RC" != "$want_rc" ] && bad="rc=$RC want $want_rc"
  if [ -z "$bad" ] && [ -n "$needle" ] && ! printf '%s' "$OUT" | grep -qF -- "$needle"; then
    bad="output missing '$needle'"
  fi
  if [ -n "$bad" ]; then
    echo "FAIL: $desc — $bad"
    printf '%s\n' "$OUT" | sed 's/^/    /'
    fails=$((fails + 1))
  else
    echo "ok:   $desc"
  fi
}

tab() { printf '%b' "${1//|/\\t}"; }   # write plans with | for tabs

# Plan row layout, mirroring `svrn __journey-plan` (sovereign-cli/src/main.rs):
#   J | id | tier | persona | visibility | live-or-skip:reason | title |
#       experience | needs (comma-joined `token:why`, or `-`)
#   S | id | idx | ro|mut | exit | stdout_contains | stdout_absent |
#       non_empty | live-or-skip:reason | settle_secs | run
# `run` is LAST because it is the only field that may contain whitespace —
# the runner reads it as the remainder of the line. New columns go before it.
#
# The J row's two experience-axis columns are the exception, appended AFTER
# `title`: `title` is not the runner's last read variable, so nothing shifts,
# and the plans below stay valid without the trailing columns — which is why
# most of these controls omit them entirely. Only the --lacks controls declare
# them.

# ── 1. a working sequence passes ─────────────────────────────────────────
cat > "$WORK/p1" <<EOF
$(tab 'J|good|1|EndUser|Public|live|a sequence that works')
$(tab 'S|good|0|ro|0|hello|-|1|live|0|ok hello')
$(tab 'S|good|1|ro|0|-|-|1|live|0|ok world')
EOF
run_case "$WORK/p1" --mutating
check "passes a sequence that works" 0 "journeys 1 passed"

# ── 2. a wrong exit code fails ───────────────────────────────────────────
cat > "$WORK/p2" <<EOF
$(tab 'J|badexit|1|EndUser|Public|live|wrong exit code')
$(tab 'S|badexit|0|ro|0|-|-|0|live|0|boom')
EOF
run_case "$WORK/p2" --mutating
check "fails a wrong exit code" 1 "exit 3, want 0"

# ── 3. a missing expected substring fails ────────────────────────────────
cat > "$WORK/p3" <<EOF
$(tab 'J|badtext|1|EndUser|Public|live|missing substring')
$(tab 'S|badtext|0|ro|0|expected-marker|-|0|live|0|ok something-else')
EOF
run_case "$WORK/p3" --mutating
check "fails a missing substring" 1 "stdout missing 'expected-marker'"

# ── 4. a mutation that did not reverse fails ─────────────────────────────
# The check the old exit-code-only harness could not express at all: after a
# remove, the thing must be GONE from the listing.
cat > "$WORK/p4" <<EOF
$(tab 'J|noreverse|1|EndUser|Public|live|removal that did not remove')
$(tab 'S|noreverse|0|mut|0|-|-|0|live|0|ok installing my-corpus')
$(tab 'S|noreverse|1|ro|0|-|my-corpus|0|live|0|ok my-corpus still here')
EOF
run_case "$WORK/p4" --mutating
check "fails a mutation that did not reverse" 1 "did not reverse"

# ── 5. a failed step aborts the rest of its journey ──────────────────────
cat > "$WORK/p5" <<EOF
$(tab 'J|abort|1|EndUser|Public|live|abort after failure')
$(tab 'S|abort|0|ro|0|-|-|0|live|0|boom')
$(tab 'S|abort|1|ro|0|-|-|0|live|0|ok never-reached')
$(tab 'S|abort|2|ro|0|-|-|0|live|0|ok also-never-reached')
EOF
run_case "$WORK/p5" --mutating
check "aborts the journey after a failed step" 1 "1 failed"
if printf '%s' "$OUT" | grep -qF "never-reached"; then
  echo "FAIL: steps after a failure were still executed"
  fails=$((fails + 1))
else
  echo "ok:   steps after a failure were skipped"
fi

# ── 6. placeholders substitute as single argv elements ───────────────────
# The STEP's own assertion is the control. The stub echoes one line per argv
# element, and the step demands `ARG[a question with spaces]` — a single
# element. If the fixture word-split (the bug this replaced: `chat ask
# {question}` became "unexpected argument `is`"), the stub would emit
# ARG[a] / ARG[question] / ARG[with] / ARG[spaces] and the step would FAIL.
# So a passing run is exactly the evidence wanted.
cat > "$WORK/p6" <<EOF
$(tab 'J|args|1|EndUser|Public|live|placeholder argv handling')
$(tab 'S|args|0|ro|0|ARG[a question with spaces]|-|0|live|0|echoargs {question}')
EOF
SOVEREIGN_JOURNEY_QUESTION="a question with spaces" run_case "$WORK/p6" --mutating
check "substitutes a spaced fixture as ONE argv element" 0 "1 passed, 0 failed"

# 6b. the paired negative: the same assertion against a genuinely split argv
# must FAIL, proving the check above discriminates rather than always passing.
cat > "$WORK/p6b" <<EOF
$(tab 'J|argsneg|1|EndUser|Public|live|split argv must not satisfy the assertion')
$(tab 'S|argsneg|0|ro|0|ARG[a question with spaces]|-|0|live|0|echoargs a question with spaces')
EOF
run_case "$WORK/p6b" --mutating
check "a split argv does NOT satisfy the same assertion" 1 "stdout missing"

# ── 7. read-only mode degrades rather than false-failing ─────────────────
cat > "$WORK/p7" <<EOF
$(tab 'J|degrade|1|EndUser|Public|live|read-only cannot prove a mutation')
$(tab 'S|degrade|0|mut|0|-|-|0|live|0|ok installing thing')
$(tab 'S|degrade|1|ro|0|thing|-|0|live|0|ok nothing-here')
EOF
run_case "$WORK/p7"
check "read-only reports partial, not pass and not fail" 0 "1 partial"

# ── 8. stderr noise must NOT satisfy stdout assertions ───────────────────
# Found live on 2026-07-28, the first time the runner was pointed at a real
# daemon: every `svrn` invocation emits `svrnmesh: bridged N legacy SOVEREIGN_*
# env var(s)` on stderr — triggered, ironically, by the harness's own
# SOVEREIGN_*-prefixed env vars. The runner captured `2>&1`, so that one line
# satisfied `stdout_non_empty` for ANY command. Every such assertion in the
# manifest was vacuous: a command that printed nothing at all still passed.
#
# These two controls pin the fix. 8a is the load-bearing one — a command that
# is SILENT on stdout must fail `stdout_non_empty` no matter how much it wrote
# to stderr.
cat > "$WORK/p8a" <<EOF
$(tab 'J|stderrempty|1|EndUser|Public|live|stderr noise is not stdout output')
$(tab 'S|stderrempty|0|ro|0|-|-|1|live|0|errout some warning on stderr')
EOF
run_case "$WORK/p8a" --mutating
check "stderr-only output FAILS stdout_non_empty" 1 "stdout was empty"

# 8b. and the same for a substring: text on stderr must not satisfy a
# `stdout_contains` — otherwise a command could "prove" a result by way of its
# own error message.
cat > "$WORK/p8b" <<EOF
$(tab 'J|stderrmatch|1|EndUser|Public|live|stderr text does not satisfy stdout_contains')
$(tab 'S|stderrmatch|0|ro|0|expected-marker|-|0|live|0|errout expected-marker')
EOF
run_case "$WORK/p8b" --mutating
check "stderr text does NOT satisfy stdout_contains" 1 "stdout missing 'expected-marker'"

# 8c. the paired positive: stderr is still CAPTURED and shown on failure, so
# splitting the streams must not cost the operator their triage context.
cat > "$WORK/p8c" <<EOF
$(tab 'J|stderrshown|1|EndUser|Public|live|stderr is still reported on failure')
$(tab 'S|stderrshown|0|ro|7|-|-|0|live|0|errout the-real-diagnostic')
EOF
run_case "$WORK/p8c" --mutating
check "stderr is still shown when a step fails" 1 "the-real-diagnostic"

# ── 10. a journey that executed NOTHING must not report a pass ───────────
# Found on the first full sandbox run (2026-07-28): four journeys reported ✓
# having run zero steps, because every step was skipped for a missing fixture
# and only FAILURES could demote a verdict. The summary said "29 ok, 0 failed"
# off 28 of 57 declared steps. A tick that survives having proven nothing is
# the same vacuous-green class as controls 8a/8b above, one level up.
#
# `{corpus}` has no default fixture, so both steps skip and nothing runs.
cat > "$WORK/p10" <<EOF
$(tab 'J|vacuous|1|EndUser|Public|live|every step blocked on a missing fixture')
$(tab 'S|vacuous|0|ro|0|-|-|0|live|0|ok install {corpus}')
$(tab 'S|vacuous|1|ro|0|-|-|0|live|0|ok status {corpus}')
EOF
run_case "$WORK/p10" --mutating
check "a journey that ran NOTHING reports vacuous, not passed" 4 "NOTHING RAN"
if printf '%s' "$OUT" | grep -qE '^\s+✓ vacuous'; then
  echo "FAIL: a zero-step journey still printed a green tick"
  fails=$((fails + 1))
else
  echo "ok:   no green tick on a zero-step journey"
fi

# 10b. the paired POSITIVE: the identical journey, with the fixture supplied,
# runs both steps and passes. Without this, control 10 would also be satisfied
# by a runner that called everything vacuous.
SOVEREIGN_JOURNEY_CORPUS=fixture-corpus run_case "$WORK/p10" --mutating
check "the same journey WITH its fixture passes" 0 "2/2 steps"

# 10c. vacuous is a property of the RUN, not a permanent verdict: read-only
# mode runs no mutating steps by construction, so it reports ∅ but must not
# fail the lane — otherwise the read-only lane is red forever and stops being
# read. The information is still on the line; only the exit code differs.
cat > "$WORK/p10c" <<EOF
$(tab 'J|allmut|1|EndUser|Public|live|nothing but mutating steps')
$(tab 'S|allmut|0|mut|0|-|-|0|live|0|ok installing')
EOF
run_case "$WORK/p10c"
check "read-only reports vacuous WITHOUT failing the lane" 0 "NOTHING RAN"

# ── 11. the summary reports step coverage, not just journey counts ───────
# "29 ok, 0 failed" is a claim about journeys. The number that says how much
# was actually PROVEN is ran/declared, and its absence is what let the gap sit
# unnoticed. One journey here runs 2 of 3 steps (the third has no fixture).
cat > "$WORK/p11" <<EOF
$(tab 'J|partialcov|1|EndUser|Public|live|two of three steps runnable')
$(tab 'S|partialcov|0|ro|0|-|-|0|live|0|ok one')
$(tab 'S|partialcov|1|ro|0|-|-|0|live|0|ok two')
$(tab 'S|partialcov|2|ro|0|-|-|0|live|0|ok three {corpus}')
EOF
run_case "$WORK/p11" --mutating
check "summary reports step coverage" 0 "coverage 2/3 declared steps executed (66%)"
check "and names WHY steps were skipped" 0 "1 no-fixture"
# A missing fixture leaves the sequence unproven, so the journey is partial —
# not the ✓ it used to earn off two thirds of its steps.
check "a fixture-skipped step demotes the journey to partial" 0 "1 partial"

# ── 12. the denominator is the WHOLE manifest, not the runnable part ─────
# The coverage line fixed in control 11 was still a percentage of what the
# lane was WILLING to attempt: journeys dropped whole by `skip_live` left both
# sides of the ratio, so 28/57 (49%) was reported where the manifest declared
# 121 steps and 28 had run (23%). Reporting a ratio against a pre-filtered
# denominator is the vacuous-green move one level up from control 10.
#
# Here: one runnable journey of 1 step, one skip_live journey of 3 steps.
# The lane ratio is 1/1 (100%) and the manifest ratio is 1/4 (25%).
cat > "$WORK/p12" <<EOF
$(tab 'J|runnable|1|EndUser|Public|live|the one journey this lane can run')
$(tab 'S|runnable|0|ro|0|-|-|0|live|0|ok yes')
$(tab 'J|notlive|1|EndUser|Public|skip:needs a second machine|three steps nobody attempts')
$(tab 'S|notlive|0|ro|0|-|-|0|live|0|ok a')
$(tab 'S|notlive|1|ro|0|-|-|0|live|0|ok b')
$(tab 'S|notlive|2|ro|0|-|-|0|live|0|ok c')
EOF
run_case "$WORK/p12" --mutating
check "the lane ratio counts only journeys it entered" 0 "coverage 1/1 declared steps executed (100%)"
check "and the manifest ratio counts the skip_live journey too" 0 "manifest 1/4 steps in the WHOLE manifest (25%)"
check "naming how much was never attempted" 0 "3 steps in 1 journeys not attempted here"

# ── 13. settle_secs waits for an ASYNC effect, and only for that ─────────
# `corpus install` POSTs and returns; the ingest lands a moment later. Before
# settle_secs the next step asserted instantly and failed for a reason that had
# nothing to do with correctness. The risk in any such mechanism is that it
# becomes a blanket flake-allowance, so both directions are pinned.
#
# 13a. a step whose assertion can NEVER hold must still fail — the settle
# window is bounded and expires, it does not retry forever.
cat > "$WORK/p13a" <<EOF
$(tab 'J|settlefail|1|EndUser|Public|live|settle expires on a genuine failure')
$(tab 'S|settlefail|0|ro|0|never-appears|-|0|live|2|ok something-else')
EOF
run_case "$WORK/p13a" --mutating
check "settle EXPIRES rather than retrying forever" 1 "stdout missing 'never-appears'"
check "and the failure names the settle window" 1 "of settle"

# 13b. a step with NO settle_secs is checked exactly once — the mechanism must
# not silently apply to every step, or a real regression would be papered over
# by however long the window is.
cat > "$WORK/p13b" <<EOF
$(tab 'J|nosettle|1|EndUser|Public|live|no settle declared')
$(tab 'S|nosettle|0|ro|0|never-appears|-|0|live|0|ok something-else')
EOF
run_case "$WORK/p13b" --mutating
check "a step without settle_secs is checked once" 1 "stdout missing 'never-appears'"
if printf '%s' "$OUT" | grep -qF "of settle"; then
  echo "FAIL: a step with settle_secs=0 still entered the settle loop"
  fails=$((fails + 1))
else
  echo "ok:   settle_secs=0 does not enter the settle loop"
fi

# ── 14. `--lacks` drops a journey by its DECLARED need, and only then ────
# A lane says what it cannot supply (`--lacks indexed-repo`) and the journeys
# declaring that need are dropped whole, with the manifest's own reason. The
# mechanism replaced a hardcoded array of journey ids in
# cli-journey-sandbox.sh, and it needs both directions pinned for the same
# reason `settle_secs` did: a drop mechanism that over-fires silently deletes
# coverage, and one that under-fires produces false failures.
#
# 14a. the need matches → not attempted, and it stays in the MANIFEST
# denominator. Dropping it from both sides of the ratio is what made the old
# coverage line flatter the lane (control 12).
cat > "$WORK/p14" <<EOF
$(tab 'J|plain|1|EndUser|Public|live|no declared needs|exp-a|-')
$(tab 'S|plain|0|ro|0|-|-|0|live|0|ok yes')
$(tab 'J|needy|1|Agent|Internal|live|needs a real index|exp-b|indexed-repo:needs an index, a graph, and a repo')
$(tab 'S|needy|0|ro|0|-|-|0|live|0|ok yes')
$(tab 'S|needy|1|ro|0|-|-|0|live|0|ok yes')
EOF
run_case "$WORK/p14" --mutating --lacks indexed-repo
check "a lane that lacks a declared need does not attempt the journey" 0 "this lane lacks indexed-repo"
# The reason is PROSE and contains commas. The first version of the plan format
# joined `token:why` pairs with a comma, so every reason printed up to its first
# one and stopped — `operator-home` announced itself as "(Claude transcripts"
# in the live lane. Asserting the TAIL of a comma-bearing reason is what pins
# the separator; asserting the head would have passed against the bug.
check "and the manifest's own reason is what it prints" 0 "needs an index, a graph, and a repo"
check "the dropped steps stay in the manifest denominator" 0 "manifest 1/3 steps in the WHOLE manifest (33%)"
if printf '%s' "$OUT" | grep -qE '^  [✓✗~∅⊘] needy'; then
  echo "FAIL: a lacked journey still produced a verdict"
  fails=$((fails + 1))
else
  echo "ok:   a lacked journey produces no verdict at all"
fi

# 14b. the SAME plan with the need supplied runs it — the drop is about the
# lane, not about the journey. Without this control, `--lacks` matching
# everything (or the needs column being misparsed) would look like a pass.
run_case "$WORK/p14" --mutating
check "the same journey runs when the lane supplies its need" 0 "coverage 3/3 declared steps executed (100%)"
check "and the experience is named on the journey header" 0 "needy [exp-b]"
# 14c. a DIFFERENT need is not a match — token equality, not substring.
run_case "$WORK/p14" --mutating --lacks operator-home
check "an unrelated --lacks token drops nothing" 0 "coverage 3/3 declared steps executed (100%)"

# ── 15. a step that asserts NOTHING never reports a tick ─────────────────
# 63 of the manifest's 133 steps declare no `expect` block at all: no exit code,
# no substring. The runner invoked them and printed ✓, because `why` is empty by
# construction when there is nothing to be wrong about.
#
# It is not a cosmetic problem. `enrich-atlas` declared its first two steps that
# way; on 2026-07-29 `enrich init --from-corpus` wrote no enrichment directory
# whatsoever, `enrich build --full` ran after it, both showed ✓, and the journey
# failed on step [2] — the first step that asserted anything — so the report
# pointed two steps past the breakage and read as "enrich status is broken".
#
# Here: a step with every assertion field empty, against a command that FAILS
# loudly (exit 3). It must still be reported as unasserted rather than passed,
# and it must not be counted as a pass in the summary.
cat > "$WORK/p15" <<EOF
$(tab 'J|noassert|1|EndUser|Public|live|a step that declares nothing')
$(tab 'S|noassert|0|ro|-|-|-|0|live|0|boom')
EOF
run_case "$WORK/p15" --mutating
check "a step with no expect block is not a pass" 4 "ran, asserted nothing"
check "and the summary counts it apart from passes" 4 "0 passed, 0 failed, 1 unasserted"
if printf '%s' "$OUT" | grep -qE '✓ \[0\]'; then
  echo "FAIL: an unasserted step still printed a ✓"
  fails=$((fails + 1))
else
  echo "ok:   no tick is printed for an unasserted step"
fi
# The step still EXECUTED, so it counts as coverage — the distinction is between
# "ran" and "proved", not between "ran" and "skipped".
check "an unasserted step still counts as executed" 4 "coverage 1/1 declared steps executed (100%)"

# ── 15b. and the JOURNEY verdict is demoted with it ───────────────────────
# The half of #15 that was missing until 2026-07-30, and the more dangerous half.
# The step lines were honest — `· ran, asserted nothing` — while the journey they
# belong to still printed `✓ noassert (1/1 steps)`, because the verdict was
# derived from "did every declared step run" and an unasserted step runs fine.
# `code-intel-lifecycle` shipped exactly that shape: a green ✓ 6/6 over four
# steps that could not fail. A reader who trusts the summary line (everyone, on
# a 30-journey lane) sees a proof that does not exist.
#
# Note the stub COMMAND here exits 3. The journey is not merely unproven, it is
# unproven over a command that is loudly broken — and it must still not be
# reported as a failure, because nothing in the manifest claimed otherwise.
check "a journey whose steps all assert nothing is UNPROVEN, not passed" 4 "⊘ noassert (UNPROVEN"
check "and the unproven journey is counted in its own bucket" 4 "1 unproven"
check "and it names how many steps ran without asserting" 4 "ran 1 step(s), none asserted anything"
if printf '%s' "$OUT" | grep -qE '^  ✓ noassert'; then
  echo "FAIL: an all-unasserted journey still printed a ✓"
  fails=$((fails + 1))
else
  echo "ok:   no journey tick is printed when nothing asserted"
fi
# Under --mutating this is exit 4, the same as ∅: the caller said it supplied a
# sandbox, so an absence of evidence is a hole somebody can close. Read-only mode
# must NOT be painted red for it — a journey whose asserting steps are all
# mutating legitimately runs only its read-only prefix there.
run_case "$WORK/p15"
check "read-only mode reports the same verdict without failing the run" 0 "⊘ noassert (UNPROVEN"

# ── 15c. one real assertion is enough to earn the tick — and it says so ───
# The discriminating control. Without it, "⊘ on everything" would pass #15b just
# as well as a working demotion. A journey with one asserting step and one
# unasserted step is a PASS (something was proven) that must NAME the weak step
# on its own verdict line, so a ✓ can never again read as "all six proven".
cat > "$WORK/p15c" <<EOF
$(tab 'J|mixed|1|EndUser|Public|live|one real assertion, one none')
$(tab 'S|mixed|0|ro|0|yes|-|0|live|0|ok yes')
$(tab 'S|mixed|1|ro|-|-|-|0|live|0|boom')
EOF
run_case "$WORK/p15c" --mutating
check "one asserting step earns the journey its tick" 0 "✓ mixed"
check "and the tick names the steps that asserted nothing" 0 "1 asserted, 1 asserted NOTHING"

# ── 9. the safety refusal actually refuses ───────────────────────────────
OUT="$(SOVEREIGN_LIVE_JOURNEYS=1 SOVEREIGN_BIN="$STUB" \
       SOVEREIGN_DAEMON_URL="http://127.0.0.1:$PORT" \
       SOVEREIGN_JOURNEY_PLAN="$WORK/p1" "$RUNNER" --mutating 2>&1)"; RC=$?
check "refuses --mutating without an isolation assertion" 2 "REFUSED"

echo
if [ "$fails" -gt 0 ]; then
  echo "cli-journey-selftest: $fails control(s) FAILED — the runner cannot be trusted"
  exit 1
fi
echo "cli-journey-selftest: all controls passed — the runner passes what should pass and fails what should fail"
exit 0
