#!/usr/bin/env bash
# cli-journey-nightly.sh — run the full journey harness unattended and leave
# a report behind.
#
# ── why this exists ──────────────────────────────────────────────────────
# The harness this one replaced (cli-contract-live-verify.sh) did not fail.
# It was never RUN. It gated on SOVEREIGN_LIVE_CONTRACT, a variable that
# appears nowhere in this repository except inside the script that reads it,
# so for its entire life it exited 0 having tested nothing — and read as
# coverage the whole time.
#
# The lesson is not "write a better runner". It is that an opt-in guard
# decays into decoration, because the moment it is inconvenient nobody opts
# in, and nothing about the repo looks any different. So the journey harness
# gets two things that do not depend on anyone remembering:
#
#   * the PRE-PUSH hook (scripts/pre-push.sh, gate 4) — static + offline
#     tiers plus the runner's negative controls, seconds, no models.
#   * THIS — the live mutating lane, nightly, where the models and the
#     toolbox already live. ~5 minutes, on hardware we already own.
#
# The split is the same one scripts/pre-push.sh argues for at length: the
# cheap deterministic half runs on every push, and the half that needs real
# weights runs on a schedule rather than never.
#
# ── what it does ─────────────────────────────────────────────────────────
#   1. re-exec inside the dev toolbox (the models and native deps live there)
#   2. build the binaries under test, so a green run is about TODAY's code
#   3. run the runner's own negative controls — a harness that cannot fail
#      is not evidence, so this gates the lane that follows it
#   4. run the READ-ONLY capability lane against the operator's own daemon —
#      the journeys that declare `needs` the sandbox cannot supply, which
#      would otherwise be declared and never run
#   5. run the mutating sandbox lane
#   6. write a dated report + a machine-readable latest.json + the per-step
#      latest-steps.jsonl that `svrn conformance` joins requirements to,
#      and prune
#
# ── usage ────────────────────────────────────────────────────────────────
#   sovereign/scripts/cli-journey-nightly.sh          # run it now, by hand
#   scripts/install-journey-nightly.sh                # install the timer
#   systemctl --user start sovereign-journey-nightly  # fire it once
#   cat ~/.svrnmesh/journey-nightly/latest.log
#
# Env: JOURNEY_NIGHTLY_DIR (report dir), JOURNEY_NIGHTLY_KEEP_DAYS (14),
#      JOURNEY_NIGHTLY_BUILD=0 to test whatever is already in target/,
#      TOOLBOX_CONTAINER (sovereign-vulkan), plus everything
#      cli-journey-sandbox.sh accepts.
#
# ── exit codes ───────────────────────────────────────────────────────────
#   0  the lane passed        2  could not run (build broke, no binaries)
#   1  a journey failed       4  a journey executed nothing (see the report)
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

# ── toolbox re-exec ──────────────────────────────────────────────────────
# The daemon needs the native stack (vulkan, llama) that lives in the dev
# toolbox; on the bare Fedora host it cannot boot. A nightly that runs on the
# host would fail every night for a reason that has nothing to do with the
# code, which is the fastest way to teach everyone to ignore it.
#
# Only re-exec when we are OUTSIDE: `toolbox run` from within a toolbox fails
# (no flatpak-spawn in the container).
TOOLBOX_CONTAINER="${TOOLBOX_CONTAINER:-sovereign-vulkan}"
if [ ! -f /run/.toolboxenv ] && command -v toolbox >/dev/null 2>&1; then
  echo "nightly: re-executing inside toolbox '$TOOLBOX_CONTAINER'"
  exec toolbox run -c "$TOOLBOX_CONTAINER" "${BASH_SOURCE[0]}" "$@"
fi

REPORT_DIR="${JOURNEY_NIGHTLY_DIR:-$HOME/.svrnmesh/journey-nightly}"
KEEP_DAYS="${JOURNEY_NIGHTLY_KEEP_DAYS:-14}"
mkdir -p "$REPORT_DIR"

STAMP="$(date +%Y-%m-%dT%H%M%S)"
REPORT="$REPORT_DIR/$STAMP.log"
# Per-step rows from BOTH lanes, concatenated. `svrn conformance` joins these
# to the `requirements = [...]` claims in the manifest, which is the only way
# a requirement proven by a black-box run gets a per-requirement verdict; the
# summary in latest.json cannot do it, because one lane verdict spread across
# every claim would mark each one proven on the strength of some other step.
STEPS="$REPORT_DIR/$STAMP-steps.jsonl"
: > "$STEPS"
# The symlink is published at the END, next to latest.log — NOT here. Linking
# it up front truncated the instrument before the lanes ran, and the two early
# exits below (build-failed, harness-broken) write their own latest.json and
# never reach the end, so a failed build left `svrn conformance` reading an
# EMPTY lane report: every journey claim silently never-ran, while `svrn
# contract nightly` correctly said "nothing is proven". Two readers, two
# symlinks, no cross-check. Last night's real rows are still on disk as
# <stamp>-steps.jsonl; leaving the link on them is the honest state.

# ── one at a time ────────────────────────────────────────────────────────
# A timer that fires while the previous run is still going would put two
# daemons and two cargo builds on the same machine. Non-blocking: skipping
# is the right answer, since the run already in flight covers it.
#
# `mkdir` and not `flock`, because flock is util-linux and does not exist
# on macOS — and its absence failed in the worst possible direction. A
# missing binary makes `if ! flock -n 9` TRUE, so every run on that host
# printed "another run holds the lock" and exited 0 having tested nothing:
# a green tick, a plausible reason, and no coverage. That is the precise
# defect this lane was built to catch, reproduced inside the lane itself
# (proven 2026-08-03 on the M2 Max, where the lane had never once run).
# mkdir is POSIX and atomic, so the lock primitive cannot go missing.
#
# Three outcomes, never two — a skip has to earn itself:
#   held by a LIVE pid   → a real concurrent run covers this fire  (exit 0)
#   held by a DEAD pid   → a crashed run left it behind; say so, take it
#   cannot create it     → a filesystem problem, not a scheduling one; we
#                          refuse to run unlocked rather than skip quietly
LOCK="$REPORT_DIR/.lock.d"
release_lock() { rm -rf "$LOCK"; }

if ! mkdir "$LOCK" 2>/dev/null; then
  holder="$(cat "$LOCK/pid" 2>/dev/null || true)"
  if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
    echo "nightly: pid $holder still running — skipping this fire" | tee -a "$REPORT"
    exit 0
  fi
  echo "nightly: STALE LOCK (pid '${holder:-unknown}' is not running) — a previous" \
       "fire died without releasing it. Taking the lock." | tee -a "$REPORT"
  rm -rf "$LOCK"
  if ! mkdir "$LOCK" 2>/dev/null; then
    echo "VERDICT: CANNOT CREATE $LOCK — refusing to run unlocked. Nothing is proven." \
      | tee -a "$REPORT"
    exit 2
  fi
fi
echo $$ > "$LOCK/pid"
trap release_lock EXIT

# Everything from here is teed into the report, so the file is the whole
# story rather than a verdict you have to trust.
exec > >(tee -a "$REPORT") 2>&1

HEAD_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
DIRTY="clean"
[ -n "$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null)" ] && DIRTY="DIRTY (uncommitted changes present)"

echo "═══ cli-journey nightly ═══"
echo "  when      $(date +%Y-%m-%dT%H:%M:%S%z)"  # not `date -Is`: BSD date rejects -I
echo "  host      $(uname -n)  (toolbox: $([ -f /run/.toolboxenv ] && echo yes || echo no))"
echo "  commit    $HEAD_SHA"
echo "  worktree  $DIRTY"
echo "  report    $REPORT"
echo

# A nightly that tests a stale binary reports a green tick about code nobody
# is running. Build first — the whole point of a scheduled lane is that it
# has the time.
if [ "${JOURNEY_NIGHTLY_BUILD:-1}" = "1" ]; then
  echo "─── build ───"
  # `code-intel` is NOT optional for this lane, however developer-only it
  # sounds. `cli-contract.toml` declares `project init` with
  # `feature = "code-intel"`, and the dispatcher only intercepts that verb when
  # the feature is on; without it the call falls through to the dev sibling,
  # which answers "Unknown project subcommand: init" and exits 1. Building
  # `dev-tools` alone therefore produced a binary THAT COULD NOT SERVE THIS
  # LANE'S OWN MANIFEST, and `code-intel-lifecycle` failed at step [0] on every
  # run from 2026-08-03 to 2026-08-12 for that reason and no other. If a future
  # journey needs another feature-gated verb, it is added here — the rule is
  # that the lane's build must cover every feature its manifest names.
  if ! ( cd "$REPO_ROOT" && cargo build --bins --features sovereign-cli/dev-tools,sovereign-cli/code-intel 2>&1 | tail -20 ); then
    echo
    echo "VERDICT: BUILD FAILED — the lane did not run. Nothing is proven."
    printf '{"stamp":"%s","commit":"%s","verdict":"build-failed","exit":2}\n' \
      "$STAMP" "$HEAD_SHA" > "$REPORT_DIR/latest.json"
    ln -sf "$REPORT" "$REPORT_DIR/latest.log"
    exit 2
  fi
  echo "build ok"
  echo
fi

# ── gate the harness before trusting the harness ─────────────────────────
# If the runner's negative controls do not hold, a green sandbox lane below
# means nothing — it would be the vacuous-green failure one level up, which
# is the exact class this whole layer exists to catch.
echo "─── harness negative controls ───"
if ! "$HERE/tests/cli-journey-selftest.sh"; then
  echo
  echo "VERDICT: THE HARNESS ITSELF FAILED its controls — sandbox lane not run,"
  echo "         because its result would not be evidence of anything."
  printf '{"stamp":"%s","commit":"%s","verdict":"harness-broken","exit":1}\n' \
    "$STAMP" "$HEAD_SHA" > "$REPORT_DIR/latest.json"
  ln -sf "$REPORT" "$REPORT_DIR/latest.log"
  exit 1
fi
echo

# ── read-only capability lane ────────────────────────────────────────────
# The complement of the sandbox lane, and the reason the two exist at all.
#
# Some journeys declare `needs` in the manifest — the operator's real HOME, or
# a live code index — and the sandbox lane drops them with `--lacks`, because a
# throwaway HOME can only produce a false failure for them. If that were the
# end of it, those journeys would be *declared and never run*: precisely the
# failure that killed cli-contract-live-verify.sh, restated one level up.
#
# So this lane runs exactly what the sandbox lane skipped, READ-ONLY, against
# the operator's own daemon — where the transcripts, notes, drift report and
# SCIP graph actually are. Read-only mode is safe to point at a production
# daemon by the runner's own contract; nothing here mutates.
#
# The journey list is DERIVED from the plan (`needs` non-empty, live-eligible),
# not written out here, so a future journey that declares a need is picked up
# by both lanes without either script being edited.
echo "─── read-only capability lane (operator HOME + real index) ───"
CLI_BIN="${SOVEREIGN_BIN:-$REPO_ROOT/target/debug/sovereign-cli}"
CAP_DAEMON="${SOVEREIGN_DAEMON_URL:-http://127.0.0.1:9741}"
CAP_VERDICT="pass"
# Read loop rather than `mapfile`: mapfile is bash 4+, and macOS ships
# 3.2.57 as /bin/bash. Same portability rule as the lock above.
CAP_IDS=()
while IFS= read -r cap_id; do
  [ -n "$cap_id" ] && CAP_IDS+=("$cap_id")
done < <(
  "$CLI_BIN" __journey-plan 2>/dev/null |
    awk -F'\t' '$1=="J" && $6=="live" && $9!="-" && $9!="" {print $2}'
)
if [ "${#CAP_IDS[@]}" = "0" ]; then
  echo "  (no journey declares a lane need — nothing for this lane to prove)"
elif ! curl -fsS -m 5 "$CAP_DAEMON/v1/models" >/dev/null 2>&1; then
  # NOT a failure of the code, and NOT a pass either. The operator's daemon
  # being down at 3am says nothing about this commit — but reporting it as
  # green would be the vacuous-green move, so it gets its own verdict and is
  # named in latest.json.
  echo "  ⚠ no daemon at $CAP_DAEMON — ${#CAP_IDS[@]} capability journeys UNPROVEN"
  echo "    (${CAP_IDS[*]})"
  CAP_VERDICT="no-daemon"
else
  CAP_FAILED=()
  for jid in "${CAP_IDS[@]}"; do
    # Gate on PIPESTATUS, never on the pipeline's exit code: the filter is the
    # last command, so `if ! runner | grep` would be asking grep whether the
    # journey passed. That mistake is how a red lane reads green.
    # SOVEREIGN_LIVE_STRICT=1 matters here, and it is not belt-and-braces. The
    # runner's DEFAULT posture on an unreachable daemon is to print "skipped"
    # and exit 0 — right for a hand-run read-only check, fatal for this lane:
    # the probe above already established the daemon was up, so if it has gone
    # away mid-lane every remaining journey would be counted as PASSED for
    # having run nothing. Observed on this very host, twice: the daemon dies
    # under a heavy lane and comes back only when restarted by hand.
    CAP_OUT="$(mktemp)"
    CAP_JSONL="$(mktemp)"
    SOVEREIGN_LIVE_JOURNEYS=1 SOVEREIGN_LIVE_STRICT=1 SOVEREIGN_BIN="$CLI_BIN" \
      SOVEREIGN_DAEMON_URL="$CAP_DAEMON" \
      SOVEREIGN_JOURNEY_OUT="$CAP_JSONL" \
      "$HERE/cli-journey-verify.sh" --journey "$jid" > "$CAP_OUT" 2>&1
    CAP_RC=$?
    cat "$CAP_JSONL" >> "$STEPS" 2>/dev/null || true
    rm -f "$CAP_JSONL"
    grep -E '^ +[✓✗~?·]|^  [✓✗~∅⊘—]|not reachable' "$CAP_OUT"
    # ⊘ UNPROVEN IS A FAILURE OF *THIS* LANE, unlike in the runner, which exits 0
    # for it in read-only mode. That leniency is right there and wrong here: the
    # runner cannot know whether a journey's asserting steps were all mutating,
    # but this lane exists for exactly one reason — to produce the evidence the
    # sandbox lane cannot — and these journeys are read-only by construction. A
    # ⊘ here means the lane ran and produced nothing, which is the whole failure
    # it was built to prevent.
    if [ "$CAP_RC" != "0" ] || grep -q '⊘' "$CAP_OUT"; then
      CAP_FAILED+=("$jid")
    fi
    rm -f "$CAP_OUT"
  done
  if [ "${#CAP_FAILED[@]}" -gt 0 ]; then
    CAP_VERDICT="fail"
    printf '  ✗ %s\n' "${CAP_FAILED[@]}"
  fi
fi
echo "  capability lane: $CAP_VERDICT (${#CAP_IDS[@]} journeys)"
echo

echo "─── mutating sandbox lane ───"
# Capture the lane's output to its OWN file rather than reading it back out of
# $REPORT below. $REPORT is written by the `tee` in the process substitution
# above, which has not necessarily flushed by the time this shell reaches the
# grep — so grepping it is a race that would intermittently report an empty
# summary. This pipeline is closed before it is read.
LANE_OUT="$(mktemp)"
JOURNEY_LANE_JSONL="$STEPS" "$HERE/cli-journey-sandbox.sh" "$@" 2>&1 | tee "$LANE_OUT"
RC="${PIPESTATUS[0]}"
echo

# Pull the numbers back out of the lane's own summary rather than recounting:
# one definition of coverage, computed where it is decided.
SUMMARY="$(grep -E '^cli-journey-sandbox: [0-9]+ ok' "$LANE_OUT" | tail -1)"
COVERAGE="$(grep -E '^ +coverage [0-9]+/' "$LANE_OUT" | tail -1 | sed 's/^ *//')"
rm -f "$LANE_OUT"

case "$RC" in
  0) VERDICT="pass" ;;
  4) VERDICT="vacuous" ;;
  2) VERDICT="could-not-run" ;;
  *) VERDICT="fail" ;;
esac

# The night's verdict is BOTH lanes. A capability journey failing is a product
# failure like any other, and letting a green sandbox lane speak for the whole
# run would hide it — so a failure there outranks a sandbox pass. `no-daemon`
# does not fail the run (the operator's daemon being down says nothing about
# this commit) but it is carried into the verdict so the report never claims
# evidence it does not have.
if [ "$CAP_VERDICT" = "fail" ] && [ "$RC" = "0" ]; then
  RC=1; VERDICT="fail"
elif [ "$CAP_VERDICT" = "no-daemon" ] && [ "$VERDICT" = "pass" ]; then
  VERDICT="pass-capability-unproven"
fi

echo "═══ VERDICT: $VERDICT (exit $RC) ═══"
[ -n "$SUMMARY" ]  && echo "  $SUMMARY"
[ -n "$COVERAGE" ] && echo "  $COVERAGE"
if [ "$RC" = "4" ]; then
  echo "  Nothing is broken — but some journeys tested NOTHING. Coverage, not"
  echo "  correctness, is what needs work; see the ∅ lines above."
fi

printf '{"stamp":"%s","commit":"%s","dirty":%s,"verdict":"%s","exit":%s,"summary":"%s","coverage":"%s","capability_lane":"%s","capability_journeys":%s}\n' \
  "$STAMP" "$HEAD_SHA" "$([ "$DIRTY" = clean ] && echo false || echo true)" \
  "$VERDICT" "$RC" "${SUMMARY//\"/\\\"}" "${COVERAGE//\"/\\\"}" \
  "$CAP_VERDICT" "${#CAP_IDS[@]}" \
  > "$REPORT_DIR/latest.json"
ln -sf "$REPORT" "$REPORT_DIR/latest.log"
# Only now, and only on a path where a lane actually ran.
ln -sf "$STEPS" "$REPORT_DIR/latest-steps.jsonl"

# Keep the history bounded without keeping a cron entry to do it.
find "$REPORT_DIR" -maxdepth 1 -name '20*.log' -mtime "+$KEEP_DAYS" -delete 2>/dev/null
find "$REPORT_DIR" -maxdepth 1 -name '20*-steps.jsonl' -mtime "+$KEEP_DAYS" -delete 2>/dev/null

echo
echo "report: $REPORT   (latest: $REPORT_DIR/latest.log, $REPORT_DIR/latest.json)"
echo "steps:  $STEPS   (read by \`svrn conformance\`)"
exit "$RC"
