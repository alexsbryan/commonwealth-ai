#!/usr/bin/env bash
# cli-journey-verify.sh — run the SEQUENCED use cases declared as [[journey]]
# in docs/cli-contract.toml against a live daemon, in order, asserting real
# output and real state transitions.
#
# This is tier 3 of the journey harness:
#   1. cli_contract_journeys  (static)   the manifest is coherent
#   2. cli_journey_dispatch   (offline)  every step is reachable
#   3. THIS                   (live)     the sequence actually works
#
# Why it exists: before the journey layer, the repo's entire behavioural CLI
# coverage was four read-only probes asserting `exit == 0` — and nothing ever
# set SOVEREIGN_LIVE_CONTRACT, so they never ran. Exit codes alone are not
# enough: `svrn code search` prints "ships in Phase 2" and exits 0, and
# `svrn project install-hooks` does nothing and exits 0. A journey asserts
# what came back, and that a removal actually removed something.
#
# A SEQUENCE, not a bag: when a step fails, the rest of that journey is
# SKIPPED rather than run. Step 4 of a broken install flow tells you nothing,
# and a wall of consequential failures buries the one that mattered.
#
# ── safety ───────────────────────────────────────────────────────────────
# Mutating steps are REFUSED unless you assert isolation, because
# `corpus remove`, `mesh join` and `setup --reset` against an operator's real
# ~/.sovereign are destructive. Read-only mode is the default and is safe to
# point at any daemon.
#
#   default (read-only)  every step marked `ro`; mutating steps skipped.
#   --mutating           also run `mut` steps. REQUIRES the caller to have
#                        provided a throwaway HOME and a non-default port,
#                        confirmed by SOVEREIGN_JOURNEY_ISOLATED=1.
#
# The established pattern for providing that isolation lives in
# scripts/daemon-soak.sh (SOAK_HOME=$(mktemp -d), SOAK_PORT=19741,
# `unshare -r -n` so the daemon cannot reach the operator's real mesh — the
# daemon has no mDNS-disable knob and `mesh join` hardcodes :9741). This
# script deliberately does NOT reimplement that boot; it verifies journeys
# and lets the caller own the sandbox.
#
# ── usage ────────────────────────────────────────────────────────────────
#   SOVEREIGN_LIVE_JOURNEYS=1 ./scripts/cli-journey-verify.sh
#   SOVEREIGN_LIVE_JOURNEYS=1 ./scripts/cli-journey-verify.sh --tier 1
#   SOVEREIGN_LIVE_JOURNEYS=1 SOVEREIGN_JOURNEY_ISOLATED=1 \
#     HOME=$(mktemp -d) SOVEREIGN_DAEMON_URL=http://127.0.0.1:19741 \
#     ./scripts/cli-journey-verify.sh --mutating
#
# Env: SOVEREIGN_LIVE_JOURNEYS (opt-in), SOVEREIGN_LIVE_STRICT (fail instead
#      of skip when the daemon is down), SOVEREIGN_DAEMON_URL, SOVEREIGN_BIN,
#      SOVEREIGN_JOURNEY_ISOLATED, SOVEREIGN_JOURNEY_TIMEOUT (per step, secs),
#      and the fixture placeholders below.
#
# ── exit codes ───────────────────────────────────────────────────────────
#   0  every journey that ran proved something, and nothing failed
#   1  a step failed
#   2  misuse (unbuilt binary, --mutating without isolation, bad flag)
#   4  VACUOUS — a journey executed zero steps under --mutating. Not a
#      failure of the code under test; a failure of this run to test it.
#      Same idea as scripts/sovereign-test.sh exiting 4 on a zero-test run.
#
# ── verdicts ─────────────────────────────────────────────────────────────
#   ✓ passed    every declared step ran (bar manifest-declared `skip_live`)
#   ~ partial   ran, but a precondition was skipped — sequence not proven
#   ∅ vacuous   NOTHING ran; this journey is evidence of nothing
#   ✗ failed    a step asserted something untrue
#
# Each line carries `ran/declared steps`, and the summary carries the total.
# A journey count alone hides the difference between 57 steps proven and 28.
set -uo pipefail

MAX_TIER=5
MUTATING=0
JOURNEY_FILTER=""
OUT_JSONL="${SOVEREIGN_JOURNEY_OUT:-}"
declare -a EXCLUDED=()

while [ $# -gt 0 ]; do
  case "$1" in
    --tier) MAX_TIER="$2"; shift 2 ;;
    --journey) JOURNEY_FILTER="$2"; shift 2 ;;
    # Caller-side exclusion, distinct from the manifest's `skip_live`.
    # `skip_live` is a property of the JOURNEY ("this needs a second
    # machine"); `--exclude` is a property of the LANE this run provides
    # ("my sandbox HOME has no Claude transcripts, so the journeys that read
    # them cannot be judged here"). Excluded journeys are announced, never
    # silently dropped.
    --exclude) EXCLUDED+=("$2"); shift 2 ;;
    --mutating) MUTATING=1; shift ;;
    --out) OUT_JSONL="$2"; shift 2 ;;
    -h|--help) sed -n '2,60p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

# ── locate the dispatcher ────────────────────────────────────────────────
BIN="${SOVEREIGN_BIN:-}"
if [ -z "$BIN" ]; then
  for c in target/debug/sovereign-cli target/release/sovereign-cli \
           ../target/debug/sovereign-cli ../target/release/sovereign-cli; do
    [ -x "$c" ] && BIN="$c" && break
  done
fi
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
  echo "cli-journey: sovereign-cli not built (set SOVEREIGN_BIN, or cargo build --bins)"
  exit 2
fi

# ── opt-in gate ──────────────────────────────────────────────────────────
if [ "${SOVEREIGN_LIVE_JOURNEYS:-0}" != "1" ]; then
  echo "cli-journey: skipped (set SOVEREIGN_LIVE_JOURNEYS=1 to run)"
  exit 0
fi

# ── daemon reachability ──────────────────────────────────────────────────
DAEMON="${SOVEREIGN_DAEMON_URL:-http://127.0.0.1:9741}"
if ! curl -fsS -m 5 "$DAEMON/v1/models" >/dev/null 2>&1; then
  if [ "${SOVEREIGN_LIVE_STRICT:-0}" = "1" ]; then
    echo "cli-journey: daemon not reachable at $DAEMON (STRICT)" >&2
    exit 1
  fi
  echo "cli-journey: skipped — daemon not reachable at $DAEMON"
  exit 0
fi

# ── isolation gate for mutating runs ─────────────────────────────────────
if [ "$MUTATING" = "1" ] && [ "${SOVEREIGN_JOURNEY_ISOLATED:-0}" != "1" ]; then
  cat >&2 <<'EOF'
cli-journey: --mutating REFUSED.

Mutating journey steps install and remove corpora, join and leave meshes,
and register services. Run against a real ~/.sovereign that is destructive.

Set SOVEREIGN_JOURNEY_ISOLATED=1 to assert you have provided a throwaway
HOME and a non-default daemon port. See scripts/daemon-soak.sh for the
established sandbox pattern (mktemp HOME + SOAK_PORT + `unshare -r -n`).
EOF
  exit 2
fi
if [ "$MUTATING" = "1" ] && [ "$DAEMON" = "http://127.0.0.1:9741" ]; then
  echo "cli-journey: --mutating against the DEFAULT port :9741 — that is the" >&2
  echo "             operator's daemon. Point SOVEREIGN_DAEMON_URL at a sandbox." >&2
  exit 2
fi

export SOVEREIGN_NO_STALE_WARN=1 SOVEREIGN_QUIET_DEPRECATIONS=1
STEP_TIMEOUT="${SOVEREIGN_JOURNEY_TIMEOUT:-120}"
EXCLUDE_REASON="${SOVEREIGN_JOURNEY_EXCLUDE_REASON:-caller passed --exclude}"

# ── fixture placeholders ─────────────────────────────────────────────────
# Every {token} a journey step can carry. Overridable so a sandbox can point
# at its own tiny fixtures; the defaults are the smallest real things in the
# repo. A step whose placeholder has no value is SKIPPED, not guessed.
declare -A FIX=(
  [corpus]="${SOVEREIGN_JOURNEY_CORPUS:-}"
  [question]="${SOVEREIGN_JOURNEY_QUESTION:-what is this corpus about?}"
  [tool]="${SOVEREIGN_JOURNEY_TOOL:-symbols}"
  # Paired with {tool}: `tools call symbols` needs a name to look up. Default
  # to a symbol this repo actually defines, so the step proves a real lookup
  # rather than argument validation.
  [symbol]="${SOVEREIGN_JOURNEY_SYMBOL:-ToolRegistry}"
  [mcp_name]="${SOVEREIGN_JOURNEY_MCP_NAME:-}"
  [url]="${SOVEREIGN_JOURNEY_MCP_URL:-}"
  [workflow]="${SOVEREIGN_JOURNEY_WORKFLOW:-}"
  [folder]="${SOVEREIGN_JOURNEY_FOLDER:-}"
  [recipe]="${SOVEREIGN_JOURNEY_RECIPE:-}"
  [repo]="${SOVEREIGN_JOURNEY_REPO:-}"
  [session_id]="${SOVEREIGN_JOURNEY_SESSION:-}"
  [scope]="${SOVEREIGN_JOURNEY_SCOPE:-ToolRegistry}"
  # Distinctive and single-token so the read-back assertion survives any
  # column truncation in `notes` output.
  [content]="${SOVEREIGN_JOURNEY_CONTENT:-journey-roundtrip-probe}"
  [intent]="${SOVEREIGN_JOURNEY_INTENT:-cli-journey-verify smoke}"
  [claim_id]="${SOVEREIGN_JOURNEY_CLAIM_ID:-}"
)

# Substitute {tokens} in a plain string (used for assertion text).
# Returns 1 if any token has no configured fixture value.
subst_str() {
  local s="$1" tok val
  while [[ "$s" =~ \{([a-z_]+)\} ]]; do
    tok="${BASH_REMATCH[1]}"
    val="${FIX[$tok]:-}"
    [ -z "$val" ] && return 1
    s="${s//\{$tok\}/$val}"
  done
  printf '%s' "$s"
}

# Build the argv ARRAY for a step, one element per token.
#
# An array, not a string: a fixture value may legitimately contain spaces
# (`{question}` is a sentence), and word-splitting an interpolated string
# turns `chat ask {question}` into six arguments and an "unexpected argument"
# error. Each `{token}` becomes exactly ONE argv element.
# Result lands in the global ARGV; returns 1 if a token has no fixture.
build_argv() {
  local raw="$1" word tok val
  ARGV=()
  for word in $raw; do
    if [[ "$word" =~ ^\{([a-z_]+)\}$ ]]; then
      tok="${BASH_REMATCH[1]}"
      val="${FIX[$tok]:-}"
      [ -z "$val" ] && return 1
      ARGV+=("$val")
    elif [[ "$word" == *"{"*"}"* ]]; then
      # Embedded token, e.g. --corpus={corpus}: substitute in place.
      word="$(subst_str "$word")" || return 1
      ARGV+=("$word")
    else
      ARGV+=("$word")
    fi
  done
  return 0
}

# Plan source. Defaults to the binary's own `__journey-plan` arm; override
# with a file to run a hand-written subset — and to give this runner its own
# negative controls (scripts/tests/cli-journey-selftest.sh feeds it synthetic
# journeys against a stub binary to prove the runner can actually FAIL).
PLAN_FILE="${SOVEREIGN_JOURNEY_PLAN:-}"
plan_source() {
  if [ -n "$PLAN_FILE" ]; then cat "$PLAN_FILE"; else "$BIN" __journey-plan 2>/dev/null; fi
}

pass=0; fail=0; skipped=0; degraded=0; jrun=0; jpass=0; jfail=0; jpartial=0
jvacuous=0
# Coverage bookkeeping — the answer to "how much of what this manifest CLAIMS
# to cover did this run actually execute?"
#
# Without it the summary line reports only the journeys, and a journey whose
# every step was skipped counted as a pass. On the first full sandbox run
# (2026-07-28) that read `29 ok, 0 failed` while 28 of 57 declared steps had
# actually executed and FOUR journeys had run nothing at all. Same vacuous-green
# class this runner already fixes twice elsewhere (the folded 2>&1 stream, the
# `--journey` typo that reported "0 ok, 0 failed"), sitting in its own summary.
steps_declared=0; steps_ran=0
skip_nofixture=0; skip_mut=0; skip_declared=0; skip_afterfail=0
# Steps in journeys this lane never ENTERS — dropped whole by `skip_live` or
# by a caller `--exclude`. They used to vanish from both numerator and
# denominator, so the coverage ratio was computed against an already-filtered
# manifest and read far better than the truth: 28/57 (49%) where the manifest
# declares 121 steps and 28 ran (23%). Reporting a percentage of what you were
# willing to attempt is the same vacuous-green move one level up.
#
# Deliberate SCOPING (--tier, --journey) is not counted here: asking for a
# subset is not a coverage gap, it is a question.
unattempted_journeys=0; unattempted_steps=0
cur_uncounted=0
declare -a FAILURES=()
declare -a VACUOUS=()
# Reused for every step's stderr; see the capture site for why the streams
# are kept apart.
ERR_FILE="$(mktemp)"
trap 'rm -f "$ERR_FILE"' EXIT
[ -n "$OUT_JSONL" ] && : > "$OUT_JSONL"

jsonl() { # id idx status run detail
  [ -z "$OUT_JSONL" ] && return 0
  printf '{"journey":"%s","step":%s,"status":"%s","run":"%s","detail":"%s"}\n' \
    "$1" "$2" "$3" "${4//\"/\\\"}" "${5//\"/\\\"}" >> "$OUT_JSONL"
}

# One row per JOURNEY, carrying the verdict and its coverage. Step rows already
# imply the coverage, but only if every consumer re-derives which statuses count
# as "executed" — and a consumer that gets that wrong reproduces exactly the bug
# this verdict exists to kill. `kind` distinguishes the two row shapes so a
# reader can filter without guessing from which keys are present.
jsonl_journey() { # id verdict ran declared
  [ -z "$OUT_JSONL" ] && return 0
  printf '{"kind":"journey","journey":"%s","verdict":"%s","steps_ran":%s,"steps_declared":%s}\n' \
    "$1" "$2" "$3" "$4" >> "$OUT_JSONL"
}

# State for the journey currently being walked.
#
# `cur_degraded` is the honest bookkeeping for read-only mode: once a
# mutating step is skipped, every later assertion that depended on that
# mutation is unverifiable. `corpus status` cannot contain a corpus we
# declined to install. Those steps still RUN (they prove the command works)
# but a mismatch is reported as unverifiable, not as a failure — and the
# journey reports `partial`, never a green tick it did not earn.
#
# `cur_ran` is the load-bearing addition: how many steps actually INVOKED the
# binary. A skip of any kind is not evidence, and a journey made entirely of
# skips proved nothing whatever its tick said.
cur_id=""; cur_broken=0; cur_degraded=0; cur_title=""
cur_declared=0; cur_ran=0; cur_skipped_declared=0; cur_degraded_why=""

# A missing fixture is the SAME epistemic class as a skipped mutation: the
# step's precondition never happened, so nothing downstream of it is proven
# either. It used to be counted but never held against the verdict, which is
# how `corpus-lifecycle` earned a green tick off one of its six steps. Mark
# the journey degraded and let the verdict fall out of that.
#
# Reads $idx / $run / $cur_id from the step loop by dynamic scope, like the
# rest of the per-step reporting.
skip_no_fixture() { # what-was-missing
  echo "    · [$idx] skip ($run) — no fixture for $1"
  skipped=$((skipped + 1)); skip_nofixture=$((skip_nofixture + 1)); cur_degraded=1
  [ -z "$cur_degraded_why" ] && cur_degraded_why="a step was skipped for a missing fixture"
  jsonl "$cur_id" "$idx" "skipped-no-fixture" "$run" "$1"
}

finish_journey() {
  [ -z "$cur_id" ] && return 0
  steps_declared=$((steps_declared + cur_declared))
  steps_ran=$((steps_ran + cur_ran))
  local cov="$cur_ran/$cur_declared steps"

  if [ "$cur_broken" = "1" ]; then
    jfail=$((jfail + 1)); echo "  ✗ $cur_id ($cov)"
    jsonl_journey "$cur_id" "fail" "$cur_ran" "$cur_declared"

  elif [ "$cur_ran" = "0" ]; then
    # NOTHING RAN. Reporting this as a pass is the failure mode the whole
    # journey layer exists to prevent, one level up: an assertion nobody
    # executed is indistinguishable from an assertion that holds, and a
    # manifest of them reads as coverage. Precedent is deliberate —
    # scripts/sovereign-test.sh exits 4 on a zero-test run for the same
    # reason ("a zero-test run is never green", note 8def98d7).
    jvacuous=$((jvacuous + 1))
    local why="every step was skipped"
    [ -n "$cur_degraded_why" ] && why="$cur_degraded_why"
    [ "$cur_declared" = "0" ] && why="the manifest declares no steps"
    echo "  ∅ $cur_id (NOTHING RAN — $cov; $why)"
    VACUOUS+=("$cur_id — $why")
    jsonl_journey "$cur_id" "vacuous" "$cur_ran" "$cur_declared"

  elif [ "$cur_degraded" = "1" ]; then
    jpartial=$((jpartial + 1))
    echo "  ~ $cur_id (partial, $cov — sequence not proven; $cur_degraded_why)"
    jsonl_journey "$cur_id" "partial" "$cur_ran" "$cur_declared"

  else
    # A green tick now means every declared step ran, or the only ones that
    # did not were skips the MANIFEST declares (`skip_live` — "needs a second
    # machine"). Those are the author's stated scope, not this lane failing to
    # supply something, so they do not demote the verdict — but they are named
    # on the line, because a silent 6/7 is how coverage quietly leaks away.
    jpass=$((jpass + 1))
    if [ "$cur_skipped_declared" -gt 0 ]; then
      echo "  ✓ $cur_id ($cov; $cur_skipped_declared declared skip_live)"
    else
      echo "  ✓ $cur_id ($cov)"
    fi
    jsonl_journey "$cur_id" "pass" "$cur_ran" "$cur_declared"
  fi
}

echo "cli-journey: $BIN against $DAEMON (tier<=$MAX_TIER, mutating=$MUTATING)"
echo

while IFS=$'\t' read -r kind f2 f3 f4 f5 f6 f7 f8 f9 f10 f11; do
  case "$kind" in
  J)
    finish_journey
    cur_id="$f2"; tier="$f3"; live="$f6"; cur_title="$f7"
    cur_broken=0; cur_degraded=0; cur_skip=0
    cur_declared=0; cur_ran=0; cur_skipped_declared=0; cur_degraded_why=""
    cur_uncounted=0
    if [ -n "$JOURNEY_FILTER" ] && [ "$cur_id" != "$JOURNEY_FILTER" ]; then
      cur_skip=1; cur_id=""; continue
    fi
    if [ "$tier" -gt "$MAX_TIER" ]; then cur_skip=1; cur_id=""; continue; fi
    for x in "${EXCLUDED[@]:-}"; do
      if [ -n "$x" ] && [ "$cur_id" = "$x" ]; then
        echo "  — $cur_id (excluded by this run: $EXCLUDE_REASON)"
        # A lane exclusion is a real gap in THIS lane's evidence, so its steps
        # stay in the manifest denominator rather than disappearing from it.
        cur_skip=1; cur_id=""; cur_uncounted=1
        unattempted_journeys=$((unattempted_journeys + 1)); break
      fi
    done
    [ -z "$cur_id" ] && continue
    if [ "${live%%:*}" = "skip" ]; then
      echo "  — $f2 (not live: ${live#skip:})"
      cur_skip=1; cur_id=""; cur_uncounted=1
      unattempted_journeys=$((unattempted_journeys + 1)); continue
    fi
    jrun=$((jrun + 1))
    echo "▸ $f2 — $cur_title"
    ;;
  S)
    if [ -z "$cur_id" ]; then
      # Belongs to a journey this lane never entered. Tally it so the manifest
      # denominator below is the whole manifest, not the runnable remainder.
      [ "$cur_uncounted" = "1" ] && unattempted_steps=$((unattempted_steps + 1))
      continue
    fi
    idx="$f3"; mut="$f4"; want_exit="$f5"; want_has="$f6"
    want_absent="$f7"; want_nonempty="$f8"; step_live="$f9"
    settle="$f10"; run="$f11"
    cur_declared=$((cur_declared + 1))

    if [ "$cur_broken" = "1" ]; then
      # A sequence is dead once a step fails — do not run the remainder.
      skipped=$((skipped + 1)); skip_afterfail=$((skip_afterfail + 1))
      jsonl "$cur_id" "$idx" "skipped-after-failure" "$run" ""
      continue
    fi
    if [ "${step_live%%:*}" = "skip" ]; then
      # Declared in the MANIFEST as un-runnable in an automated lane (needs a
      # second machine, needs sudo). The author's stated scope, so it does not
      # demote the journey — it is counted and named instead.
      echo "    · [$idx] skip ($run) — ${step_live#skip:}"
      skipped=$((skipped + 1)); skip_declared=$((skip_declared + 1))
      cur_skipped_declared=$((cur_skipped_declared + 1))
      jsonl "$cur_id" "$idx" "skipped" "$run" "${step_live#skip:}"
      continue
    fi
    if [ "$mut" = "mut" ] && [ "$MUTATING" != "1" ]; then
      echo "    · [$idx] skip ($run) — mutating, read-only mode"
      skipped=$((skipped + 1)); skip_mut=$((skip_mut + 1)); cur_degraded=1
      [ -z "$cur_degraded_why" ] && cur_degraded_why="a mutating step was skipped in read-only mode"
      jsonl "$cur_id" "$idx" "skipped-mutating" "$run" ""
      continue
    fi
    if ! build_argv "$run"; then
      skip_no_fixture "a placeholder"
      continue
    fi
    # Assertions carry placeholders too — `stdout_contains = "{corpus}"` must
    # become the fixture's real corpus id before it is grepped for, or the
    # step asserts the literal string "{corpus}" and can never pass.
    if [ "$want_has" != "-" ] && ! want_has="$(subst_str "$want_has")"; then
      skip_no_fixture "the expected substring"
      continue
    fi
    if [ "$want_absent" != "-" ] && ! want_absent="$(subst_str "$want_absent")"; then
      skip_no_fixture "the absent substring"
      continue
    fi

    # Past every skip gate: this step is about to invoke the binary, and that
    # is the only thing that counts as coverage.
    cur_ran=$((cur_ran + 1))
    argv="${ARGV[*]}"
    # STDOUT and STDERR are captured SEPARATELY, and every `stdout_*`
    # assertion reads stdout ALONE.
    #
    # This used to be `2>&1`, and that folded stream quietly made a whole
    # assertion class vacuous. Every `svrn` invocation prints
    # `svrnmesh: bridged N legacy SOVEREIGN_* env var(s)` to stderr —
    # triggered by this very script's SOVEREIGN_*-prefixed env vars — so
    # `stdout_non_empty` was satisfied by the warning banner for ANY command,
    # including one that printed nothing at all. Worse for `stdout_contains`:
    # a command could "prove" a result with its own error message.
    #
    # stderr is still captured and still printed on failure — the operator
    # keeps the diagnostic, they just cannot pass a test with it.
    # Run the command and judge it. Factored out so a `settle_secs` step can
    # re-run the WHOLE thing — command included — rather than re-judging one
    # stale capture: the point of settling is that the system's state is still
    # changing, and `corpus status` has to be asked again to see it.
    attempt_step() {
      out="$(timeout "$STEP_TIMEOUT" "$BIN" "${ARGV[@]}" 2>"$ERR_FILE")"; code=$?
      err="$(cat "$ERR_FILE")"
      why=""
      [ "$want_exit" != "-" ] && [ "$code" != "$want_exit" ] && why="exit $code, want $want_exit"
      if [ -z "$why" ] && [ "$want_has" != "-" ] && ! printf '%s' "$out" | grep -qF -- "$want_has"; then
        why="stdout missing '$want_has'"
      fi
      if [ -z "$why" ] && [ "$want_absent" != "-" ] && printf '%s' "$out" | grep -qF -- "$want_absent"; then
        why="stdout still contains '$want_absent' — the mutation did not reverse"
      fi
      if [ -z "$why" ] && [ "$want_nonempty" = "1" ] && [ -z "$(printf '%s' "$out" | tr -d '[:space:]')" ]; then
        why="stdout was empty"
      fi
    }

    attempt_step
    settled_after=""
    # Asynchronous effects only. The assertion is unchanged and must still
    # hold — the step is given the system's own latency to produce it, and
    # how long it actually took is REPORTED, so a step that quietly drifts
    # from 1s to 25s is visible rather than merely still-green.
    if [ -n "$why" ] && [ "${settle:-0}" -gt 0 ] 2>/dev/null; then
      settle_start=$(date +%s)
      while [ -n "$why" ] && (( $(date +%s) - settle_start < settle )); do
        sleep 1
        attempt_step
      done
      settled_after=$(( $(date +%s) - settle_start ))
    fi

    if [ -z "$why" ]; then
      pass=$((pass + 1))
      if [ -n "$settled_after" ]; then
        echo "    ✓ [$idx] $argv (settled after ${settled_after}s)"
      else
        echo "    ✓ [$idx] $argv"
      fi
      jsonl "$cur_id" "$idx" "pass" "$argv" ""
    elif [ "$cur_degraded" = "1" ]; then
      # The assertion could not hold — its precondition was skipped. Say so
      # rather than pretending either outcome.
      degraded=$((degraded + 1))
      echo "    ? [$idx] $argv — unverifiable, $cur_degraded_why ($why)"
      jsonl "$cur_id" "$idx" "unverifiable" "$argv" "$why"
    else
      fail=$((fail + 1)); cur_broken=1
      # Name the settle window on failure. "stdout missing X" after a silent
      # 30-second wait reads like an instant failure and sends you looking in
      # the wrong place.
      [ -n "$settled_after" ] && why="$why (still true after ${settled_after}s of settle)"
      echo "    ✗ [$idx] $argv — $why"
      # Both streams, labelled. A step that fails on `stdout was empty` is
      # only diagnosable if the stderr that WAS produced is visible.
      [ -n "$out" ] && printf '%s\n' "$out" | head -5 | sed 's/^/        /'
      [ -n "$err" ] && printf '%s\n' "$err" | head -5 | sed 's/^/        stderr: /'
      FAILURES+=("$cur_id[$idx] $argv — $why")
      jsonl "$cur_id" "$idx" "fail" "$argv" "$why"
    fi
    ;;
  esac
done < <(plan_source)
finish_journey

pct=0
[ "$steps_declared" -gt 0 ] && pct=$(( steps_ran * 100 / steps_declared ))

echo
echo "cli-journey: journeys $jpass passed, $jpartial partial, $jvacuous vacuous, $jfail failed (of $jrun run)"
echo "             steps   $pass passed, $fail failed, $degraded unverifiable, $skipped skipped"
# The line the summary was missing. "29 ok, 0 failed" is a statement about
# journeys; this is the statement about EVIDENCE, and the two can differ by
# half. Always printed, in every mode — a lane that proves little should say
# so even when it is not failing.
printf '             coverage %s/%s declared steps executed (%s%%)' "$steps_ran" "$steps_declared" "$pct"
if [ "$skipped" -gt 0 ]; then
  printf ' — %s skipped:' "$skipped"
  [ "$skip_nofixture" -gt 0 ] && printf ' %s no-fixture' "$skip_nofixture"
  [ "$skip_mut" -gt 0 ]       && printf ' %s mutating-in-read-only' "$skip_mut"
  [ "$skip_declared" -gt 0 ]  && printf ' %s declared-skip_live' "$skip_declared"
  [ "$skip_afterfail" -gt 0 ] && printf ' %s after-a-failure' "$skip_afterfail"
fi
printf '\n'

# The manifest-level number. The line above is a percentage of what this lane
# was WILLING to attempt; this one is a percentage of what the manifest
# actually claims to cover, and the two differ by more than half. Printed
# whenever they differ, because the first number alone flatters the lane in
# exactly the way the ∅ verdict exists to prevent.
manifest_total=$((steps_declared + unattempted_steps))
if [ "$unattempted_steps" -gt 0 ]; then
  mpct=0
  [ "$manifest_total" -gt 0 ] && mpct=$(( steps_ran * 100 / manifest_total ))
  printf '             manifest %s/%s steps in the WHOLE manifest (%s%%) — %s steps in %s journeys not attempted here\n' \
    "$steps_ran" "$manifest_total" "$mpct" "$unattempted_steps" "$unattempted_journeys"
fi

if [ "$jpartial" -gt 0 ] && [ "$MUTATING" != "1" ]; then
  echo
  echo "NOTE: read-only mode cannot prove a sequence that mutates state. The"
  echo "      partial journeys above verified only their read-only prefix."
  echo "      Run with --mutating against a sandbox to prove them end to end."
fi
if [ "$fail" -gt 0 ]; then
  echo
  echo "failed steps:"
  printf '  %s\n' "${FAILURES[@]}"
  exit 1
fi
if [ "$jvacuous" -gt 0 ]; then
  echo
  echo "journeys that executed NOTHING:"
  printf '  ∅ %s\n' "${VACUOUS[@]}"
  # Exit 4 only in the lane that CLAIMS to prove sequences end to end.
  #
  # In read-only mode a journey of nothing-but-mutating-steps runs nothing by
  # construction — that is the mode working as designed, it is already stated
  # on the ∅ line and in the coverage breakdown, and making it non-zero would
  # paint the read-only lane permanently red until nobody read it. Under
  # --mutating there is no such excuse: the caller asserted it supplied a
  # sandbox, so a journey with no evidence is a gap in the fixtures, and a
  # gap you can close is worth a non-zero exit. Distinct from 1 so a caller
  # can tell "something is broken" from "nothing was proven".
  if [ "$MUTATING" = "1" ]; then
    echo
    echo "      These declare steps but ran none — supply the missing fixtures"
    echo "      (SOVEREIGN_JOURNEY_* env, see the fixture table in this script)."
    exit 4
  fi
  echo "      (read-only mode: expected for mutating journeys; not failing the run)"
fi
exit 0
