#!/usr/bin/env bash
# run-if-stale.sh — fire a maintenance lane at LOGIN when, and only when, its
# last run is old. The trigger this fleet can actually keep.
#
# WHY THIS EXISTS. The two scheduled gates on this host both assume a machine
# that is awake at night:
#   * the CLI-contract "nightly" lane has NO scheduler here at all —
#     scripts/install-journey-nightly.sh installs a *systemd user timer* and
#     exits 2 on macOS ("no systemd user session here"), so the lane has only
#     ever run by hand.
#   * com.svrn.co-sweep fires at 03:30 via StartCalendarInterval, and these
#     machines are powered down at 03:30.
# Measured cost of the gap: a FAIL verdict recorded 2026-08-03 sat unread for
# three days, while `svrn posture` printed a cadence claim ("the lane fires
# daily") that nothing on the box was wired to keep.
#
# So: no cadence. A high-water marker per lane, a RunAtLoad LaunchAgent, and a
# lane that runs iff the marker is older than the staleness window. A machine
# that is used every day runs the lane every day; a machine left off for a week
# runs it once when it comes back. Nothing is claimed that the hardware cannot
# keep.
#
#   scripts/run-if-stale.sh contract-nightly   # fire iff >20h since last fire
#   scripts/run-if-stale.sh co-sweep
#   scripts/run-if-stale.sh --status           # every lane: marker age, decision
#   scripts/run-if-stale.sh --self-test        # watch it fire AND skip
#   scripts/run-if-stale.sh --write-plists     # write the LaunchAgents; NEVER loads them
#
# Generic form (what the named lanes are presets for):
#   scripts/run-if-stale.sh --marker PATH --cmd 'shell command' [--label NAME]
#
# Exit codes are the decision, so a caller never has to parse prose:
#   0  fired (lane launched in the background)
#   3  skipped — marker is fresh
#   2  usage / unknown lane
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Everything stateful hangs off this, so the self-test can redirect the whole
# world into a temp dir instead of the script growing a test-only branch.
STATE_HOME="${RUN_IF_STALE_HOME:-$HOME}"
# Root resolved through the SSOT, never hard-coded: `~/.svrnmesh` on a
# migrated machine, a populated legacy `~/.sovereign` on one that is not.
# The Rust reader (`cli_contract_report::nightly_trigger`) resolves the same
# way, and reader/writer MUST agree or the lane silently reports never-run.
# When RUN_IF_STALE_HOME is overridden (the self-test) it wins outright.
# shellcheck source=scripts/lib/svrn-root.sh
. "${REPO}/scripts/lib/svrn-root.sh"
if [ -n "${RUN_IF_STALE_HOME:-}" ]; then
  STATE_ROOT="$STATE_HOME/.svrnmesh"
else
  STATE_ROOT="$(svrn_root)"
fi
STATE_DIR="$STATE_ROOT/run-if-stale"

# The window. 20h, not 24h: a 24h window plus a human who logs in at roughly
# the same time each morning skips every other day (23h58m is "fresh").
STALE_HOURS="${RUN_IF_STALE_HOURS:-20}"

# Boot is the worst moment to start a multi-minute cargo build. The lane waits
# this long after login before doing anything. Set 0 in a foreground run.
DELAY_SECS="${RUN_IF_STALE_DELAY:-300}"

log() { printf '%s run-if-stale[%s] %s\n' "$(date -u +%FT%TZ)" "${LABEL:-?}" "$*"; }

# ── the lane registry ───────────────────────────────────────────────────
# Open set → registry (ARCH_PRINCIPLES §4), keyed by lane id. Each preset is
# just (marker, command); the --marker/--cmd form below is the same thing
# spelled by hand, which is what keeps the self-test honest — it exercises the
# real decision path, not a copy of it.
lane_preset() {
  case "$1" in
    contract-nightly)
      LABEL="contract-nightly"
      MARKER="$STATE_DIR/contract-nightly.last"
      CMD="$REPO/sovereign/scripts/cli-journey-nightly.sh"
      ;;
    co-sweep)
      LABEL="co-sweep"
      MARKER="$STATE_DIR/co-sweep.last"
      CMD="$REPO/scripts/co-sweep.sh"
      ;;
    *) return 1 ;;
  esac
}
LANES=(contract-nightly co-sweep)

# Marker age in whole hours; empty string when there is no marker at all.
# Two states, never collapsed: "never run" is not "very stale" — one means the
# trigger has not been installed, the other means it has and the lane is due.
marker_age_hours() {
  local m="$1"
  [ -f "$m" ] || { echo ""; return; }
  local mt now
  mt="$(stat -f %m "$m" 2>/dev/null || stat -c %Y "$m" 2>/dev/null)"
  [ -n "$mt" ] || { echo ""; return; }
  now="$(date +%s)"
  echo $(( (now - mt) / 3600 ))
}

# ── modes ───────────────────────────────────────────────────────────────
MODE=run
MARKER=""
CMD=""
LABEL=""
LANE=""

case "${1:-}" in
  -h|--help) sed -n '2,32p' "$0"; exit 2 ;;
  --status)      MODE=status ;;
  --self-test)   MODE=selftest ;;
  --write-plists) MODE=plists ;;
  --list)        printf '%s\n' "${LANES[@]}"; exit 0 ;;
  --marker)
    MODE=run
    while [ $# -gt 0 ]; do
      case "$1" in
        --marker) MARKER="$2"; shift 2 ;;
        --cmd)    CMD="$2"; shift 2 ;;
        --label)  LABEL="$2"; shift 2 ;;
        *) echo "run-if-stale: unexpected argument \`$1\`" >&2; exit 2 ;;
      esac
    done
    [ -n "$MARKER" ] && [ -n "$CMD" ] || { echo "run-if-stale: --marker and --cmd are both required" >&2; exit 2; }
    LABEL="${LABEL:-adhoc}"
    ;;
  "") echo "run-if-stale: name a lane (${LANES[*]}) or pass --status/--self-test/--write-plists" >&2; exit 2 ;;
  -*) echo "run-if-stale: unknown option \`$1\`" >&2; exit 2 ;;
  *)
    LANE="$1"
    lane_preset "$LANE" || { echo "run-if-stale: unknown lane \`$LANE\` (have: ${LANES[*]})" >&2; exit 2; }
    ;;
esac

# ── --status ────────────────────────────────────────────────────────────
if [ "$MODE" = status ]; then
  echo "run-if-stale: window ${STALE_HOURS}h · state $STATE_DIR"
  for l in "${LANES[@]}"; do
    lane_preset "$l"
    age="$(marker_age_hours "$MARKER")"
    if [ -z "$age" ]; then
      printf '  %-18s never fired by this guard — next login WILL run it\n' "$l"
    elif [ "$age" -ge "$STALE_HOURS" ]; then
      printf '  %-18s %sh ago — STALE, next login will run it\n' "$l" "$age"
    else
      printf '  %-18s %sh ago — fresh, next login will skip it\n' "$l" "$age"
    fi
    printf '  %-18s   marker %s\n' "" "$MARKER"
    printf '  %-18s   lane   %s\n' "" "$CMD"
  done
  exit 0
fi

# ── --write-plists ──────────────────────────────────────────────────────
# Writes the files and prints the load command. It does NOT run launchctl:
# loading an agent into the operator's GUI session is the operator's act, and
# a script that silently arms a login-time job on someone else's machine is
# exactly the kind of invisible mechanism this whole change is fixing.
if [ "$MODE" = plists ]; then
  shift
  while [ $# -gt 0 ]; do
    case "$1" in
      --repo) REPO="$(cd "$2" && pwd)" || exit 2; shift 2 ;;
      *) echo "run-if-stale --write-plists: unexpected argument \`$1\`" >&2; exit 2 ;;
    esac
  done
  # A LaunchAgent outlives the checkout that wrote it, and a linked git
  # worktree is by definition temporary — a plist pointing into one is a
  # trigger that silently stops working the day the worktree is removed.
  # Detect it (a worktree's .git is a FILE) and refuse to be quiet about it.
  if [ -f "$REPO/.git" ]; then
    echo "run-if-stale: $REPO is a linked git WORKTREE, not the main checkout." >&2
    echo "              A LaunchAgent pointing here breaks when the worktree is removed." >&2
    echo "              Re-run with --repo /path/to/main/checkout." >&2
    exit 2
  fi
  AGENTS="$HOME/Library/LaunchAgents"
  mkdir -p "$AGENTS" "$STATE_DIR"
  written=()
  missing=()
  for l in "${LANES[@]}"; do
    lane_preset "$l"
    label="com.svrn.$l-onboot"
    plist="$AGENTS/$label.plist"
    cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$REPO/scripts/run-if-stale.sh</string>
    <string>$l</string>
  </array>
  <!-- RunAtLoad, no cadence: the guard decides from the marker, so a login on
       a machine that already ran today is a no-op that costs one stat(2). -->
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>$STATE_DIR/$l.guard.log</string>
  <key>StandardErrorPath</key><string>$STATE_DIR/$l.guard.log</string>
  <!-- Nice: whatever else login is doing matters more than a maintenance lane. -->
  <key>ProcessType</key><string>Background</string>
</dict></plist>
EOF
    written+=("$plist")
    echo "wrote $plist  ->  run-if-stale.sh $l   (repo $REPO)"
    [ -x "$CMD" ] || missing+=("$l: $CMD")
  done
  [ -x "$REPO/scripts/run-if-stale.sh" ] || missing+=("guard: $REPO/scripts/run-if-stale.sh")
  if [ ${#missing[@]} -gt 0 ]; then
    echo
    echo "WARNING — the plists were written, but these paths do not exist (or are not"
    echo "          executable) in $REPO yet. Loading now would give you a trigger that"
    echo "          fires into nothing. Land the branch first, then load:"
    printf '            %s\n' "${missing[@]}"
  fi
  echo
  echo "NOT LOADED. Load them yourself (this script deliberately never calls launchctl):"
  for p in "${written[@]}"; do
    echo "    launchctl bootstrap gui/\$(id -u) $p"
  done
  echo
  echo "  verify:   launchctl list | grep com.svrn"
  echo "  unload:   launchctl bootout gui/\$(id -u)/com.svrn.<lane>-onboot"
  exit 0
fi

# ── --self-test ─────────────────────────────────────────────────────────
# A gate you have not watched fail is not a gate (§18.1). This watches BOTH
# directions on the real decision path — same script, same argv shape, only
# the marker and the command are fixtures.
if [ "$MODE" = selftest ]; then
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/run-if-stale-selftest.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  marker="$tmp/lane.last"
  sentinel="$tmp/fired"
  cmd="/bin/sh -c 'echo fired >> $sentinel'"
  fails=0
  fired_count() { [ -f "$sentinel" ] && wc -l < "$sentinel" | tr -d ' ' || echo 0; }
  check() { # name expected_exit expected_fires
    local name="$1" want_exit="$2" want_fires="$3"
    RUN_IF_STALE_DELAY=0 "$0" --marker "$marker" --cmd "$cmd" --label selftest >"$tmp/out" 2>&1
    local got=$?
    # The lane is launched detached; wait for it rather than racing it.
    for _ in $(seq 1 40); do [ "$(fired_count)" -ge "$want_fires" ] && break; sleep 0.25; done
    local fires; fires="$(fired_count)"
    if [ "$got" = "$want_exit" ] && [ "$fires" = "$want_fires" ]; then
      echo "ok:   $name (exit $got, $fires fire(s))"
    else
      echo "FAIL: $name — exit $got want $want_exit; fires $fires want $want_fires"
      sed 's/^/        /' "$tmp/out"
      fails=$((fails + 1))
    fi
  }

  echo "run-if-stale self-test (window ${STALE_HOURS}h) in $tmp"
  check "no marker at all -> fires"                    0 1
  check "marker just written by that fire -> skips"    3 1
  touch -t "$(date -v-30H +%Y%m%d%H%M 2>/dev/null || date -d '30 hours ago' +%Y%m%d%H%M)" "$marker"
  check "marker aged 30h -> fires again"               0 2
  RUN_IF_STALE_HOURS=99999 RUN_IF_STALE_DELAY=0 "$0" --marker "$marker" --cmd "$cmd" --label selftest >/dev/null 2>&1
  if [ $? = 3 ]; then echo "ok:   the window is what decides (99999h -> skip)"; else echo "FAIL: window override ignored"; fails=$((fails+1)); fi

  echo
  if [ "$fails" = 0 ]; then echo "self-test: PASS (fires when stale, skips when fresh)"; exit 0; fi
  echo "self-test: FAIL ($fails)"; exit 1
fi

# ── the decision ────────────────────────────────────────────────────────
# The lane log lives NEXT TO the marker, not under a second root. v0 put it at
# $STATE_DIR/<label>.lane.log while the marker followed --marker; the self-test
# then redirected the marker into a temp dir, the log redirect pointed at a
# $HOME directory that did not exist, and the background lane died on the
# redirect without ever running — a fire that logged "launching" and launched
# nothing. One state location per lane, so that cannot recur.
LANE_LOG="${RUN_IF_STALE_LOG:-${MARKER%.last}.lane.log}"
mkdir -p "$(dirname "$MARKER")" "$(dirname "$LANE_LOG")" || {
  echo "run-if-stale[$LABEL]: cannot create state dir for $MARKER" >&2; exit 2; }
AGE="$(marker_age_hours "$MARKER")"

if [ -n "$AGE" ] && [ "$AGE" -lt "$STALE_HOURS" ]; then
  log "skip: last fire ${AGE}h ago, window ${STALE_HOURS}h"
  exit 3
fi

if [ -z "$AGE" ]; then
  log "fire: no marker at $MARKER (this guard has never run this lane)"
else
  log "fire: last fire ${AGE}h ago, window ${STALE_HOURS}h"
fi

# Mark BEFORE launching, not after. The marker answers "when did we last fire
# this lane", which is the question the trigger needs; whether the lane passed
# is a different question with a different artifact (the lane's own report,
# which is what `svrn posture` reads). Marking after would let a lane that
# takes 40 minutes be fired twice by two logins, and a lane that crashes be
# fired on every login forever.
: > "$MARKER"

log "launching in background (delay ${DELAY_SECS}s): $CMD"
(
  sleep "$DELAY_SECS"
  # shellcheck disable=SC2086 # CMD is a command line by construction
  eval $CMD
  printf '%s run-if-stale[%s] lane exited %s\n' "$(date -u +%FT%TZ)" "$LABEL" "$?"
) </dev/null >>"$LANE_LOG" 2>&1 &
disown 2>/dev/null || true
exit 0
