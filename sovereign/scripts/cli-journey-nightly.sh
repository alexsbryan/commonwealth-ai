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
#   4. run the mutating sandbox lane
#   5. write a dated report + a machine-readable latest.json, and prune
#
# ── usage ────────────────────────────────────────────────────────────────
#   sovereign/scripts/cli-journey-nightly.sh          # run it now, by hand
#   scripts/install-journey-nightly.sh                # install the timer
#   systemctl --user start sovereign-journey-nightly  # fire it once
#   cat ~/.sovereign/journey-nightly/latest.log
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

REPORT_DIR="${JOURNEY_NIGHTLY_DIR:-$HOME/.sovereign/journey-nightly}"
KEEP_DAYS="${JOURNEY_NIGHTLY_KEEP_DAYS:-14}"
mkdir -p "$REPORT_DIR"

STAMP="$(date +%Y-%m-%dT%H%M%S)"
REPORT="$REPORT_DIR/$STAMP.log"

# ── one at a time ────────────────────────────────────────────────────────
# A timer that fires while the previous run is still going would put two
# daemons and two cargo builds on the same machine. flock, non-blocking:
# skipping is the right answer, since the run already in flight covers it.
LOCK="$REPORT_DIR/.lock"
exec 9>"$LOCK"
if ! flock -n 9; then
  echo "nightly: another run holds $LOCK — skipping this fire" | tee -a "$REPORT"
  exit 0
fi

# Everything from here is teed into the report, so the file is the whole
# story rather than a verdict you have to trust.
exec > >(tee -a "$REPORT") 2>&1

HEAD_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
DIRTY="clean"
[ -n "$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null)" ] && DIRTY="DIRTY (uncommitted changes present)"

echo "═══ cli-journey nightly ═══"
echo "  when      $(date -Is)"
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
  if ! ( cd "$REPO_ROOT" && cargo build --bins --features sovereign-cli/dev-tools 2>&1 | tail -20 ); then
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

echo "─── mutating sandbox lane ───"
# Capture the lane's output to its OWN file rather than reading it back out of
# $REPORT below. $REPORT is written by the `tee` in the process substitution
# above, which has not necessarily flushed by the time this shell reaches the
# grep — so grepping it is a race that would intermittently report an empty
# summary. This pipeline is closed before it is read.
LANE_OUT="$(mktemp)"
"$HERE/cli-journey-sandbox.sh" "$@" 2>&1 | tee "$LANE_OUT"
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

echo "═══ VERDICT: $VERDICT (exit $RC) ═══"
[ -n "$SUMMARY" ]  && echo "  $SUMMARY"
[ -n "$COVERAGE" ] && echo "  $COVERAGE"
if [ "$RC" = "4" ]; then
  echo "  Nothing is broken — but some journeys tested NOTHING. Coverage, not"
  echo "  correctness, is what needs work; see the ∅ lines above."
fi

printf '{"stamp":"%s","commit":"%s","dirty":%s,"verdict":"%s","exit":%s,"summary":"%s","coverage":"%s"}\n' \
  "$STAMP" "$HEAD_SHA" "$([ "$DIRTY" = clean ] && echo false || echo true)" \
  "$VERDICT" "$RC" "${SUMMARY//\"/\\\"}" "${COVERAGE//\"/\\\"}" \
  > "$REPORT_DIR/latest.json"
ln -sf "$REPORT" "$REPORT_DIR/latest.log"

# Keep the history bounded without keeping a cron entry to do it.
find "$REPORT_DIR" -maxdepth 1 -name '20*.log' -mtime "+$KEEP_DAYS" -delete 2>/dev/null

echo
echo "report: $REPORT   (latest: $REPORT_DIR/latest.log, $REPORT_DIR/latest.json)"
exit "$RC"
