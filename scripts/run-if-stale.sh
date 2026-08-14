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
#   scripts/run-if-stale.sh --write-oneshot contract-nightly
#                                              # transient one-shot plist; NEVER loads it
#
# Generic form (what the named lanes are presets for):
#   scripts/run-if-stale.sh --marker PATH --cmd 'shell command' [--label NAME]
#
# Exit codes are the decision, so a caller never has to parse prose:
#   0  fired (lane launched in the background)
#   3  skipped — marker is fresh
#   4  skipped — the box is busy (another run holds the daemon claim)
#   2  usage / unknown lane
#
# `launchctl submit` is BANNED here and in the comaintainer run channel: it
# carries implicit keepalive and leaves no plist to find, which is how
# `seat.nightly.relaunch2` became a respawner nobody could locate
# (2026-08-13). Every launchd path in this script writes an explicit plist
# with `KeepAlive=false` and prints its bootout command.
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

# launchd hands a job a bare PATH (`/usr/bin:/bin:/usr/sbin:/sbin`), so a lane
# that shells `cargo` or `sovereign` dies on "command not found" minutes after
# it looked like it started. Every plist this script writes carries this.
LANE_PATH="${RUN_IF_STALE_PATH:-$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin}"

# ── the quiet-box precondition (banked item 8f6e6eec) ───────────────────
# A lane that builds and then drives the local daemon is a bad neighbour to
# another daemon-bound run: they share one GPU, one model slot and one
# target/ dir, and the loser reports a latency regression that is really
# contention. Ask the resource commons first and SKIP — never queue, never
# wait. Only `held` blocks: `expired` means the holder died (their work is
# not running), `free` means nobody has it, and `unknown` means the commons
# could not answer, which is not evidence of a busy box.
CLAIM_CMD="${RUN_IF_STALE_CLAIM_CMD:-sovereign claim may-i}"

# The claim scope names the MESH NODE, not the hostname: peers coordinate on
# `sovereign mesh status` names (`BeefyMac`), while `hostname -s` on that same
# box is `Alexs-MacBook-Pro-2` — a second name for one machine that no peer
# would ever match. One accessor, so a lane and a seat cannot compute
# different strings for the same resource.
node_name() {
  local n
  n="$(sovereign mesh status 2>/dev/null | awk '/ \*$/ {print $2; exit}')"
  if [ -n "$n" ]; then printf '%s' "$n"; return; fi
  # Named, never silent (§18.3): a scope keyed on a different name collides
  # with nobody, which is exactly the failure this guard exists to prevent.
  echo "run-if-stale: mesh node name unavailable — claim scope falls back to \`hostname -s\`" >&2
  hostname -s
}

# One of: held | expired | free | unknown.
daemon_claim_verdict() {
  local out v
  # shellcheck disable=SC2086 # CLAIM_CMD is a command line by construction
  out="$($CLAIM_CMD "$1" --format json 2>/dev/null)" || { echo unknown; return; }
  v="$(printf '%s' "$out" | sed -n 's/.*"verdict"[[:space:]]*:[[:space:]]*"\([a-z]*\)".*/\1/p' | head -1)"
  echo "${v:-unknown}"
}

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

# ONE plist writer for every launchd path in this script, so the login-time
# agent and the seat's one-shot can never drift apart on the properties that
# actually matter — KeepAlive, PATH, and the stop command (§10.6: one
# implementation per decision).
write_plist() { # path label lane logfile delay_secs
  local path="$1" label="$2" lane="$3" logfile="$4" delay="$5"
  cat > "$path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  $label — written by scripts/run-if-stale.sh, which never loads it.

  load:   launchctl bootstrap gui/$(id -u) $path
  fire:   launchctl kickstart -k gui/$(id -u)/$label
  status: launchctl print gui/$(id -u)/$label | grep -E 'state|runs|last exit'
  STOP:   launchctl bootout gui/$(id -u)/$label

  KeepAlive=false is the point: launchd runs this ONCE per load and never
  respawns it. \`launchctl submit\` implies the opposite and leaves no file
  to read, which is how seat.nightly.relaunch2 became an unkillable
  respawner nobody could locate (2026-08-13). It is banned; this is the
  replacement.
-->
<plist version="1.0"><dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$REPO/scripts/run-if-stale.sh</string>
    <string>$lane</string>
  </array>
  <!-- RunAtLoad, no cadence: the guard decides from the marker, so a login on
       a machine that already ran today is a no-op that costs one stat(2). -->
  <key>RunAtLoad</key><true/>
  <!-- Never respawn. Explicit rather than defaulted: the reader must be able
       to answer "does this come back?" from the file. -->
  <key>KeepAlive</key><false/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$LANE_PATH</string>
    <key>HOME</key><string>$HOME</string>
    <key>RUN_IF_STALE_DELAY</key><string>$delay</string>
  </dict>
  <key>StandardOutPath</key><string>$logfile</string>
  <key>StandardErrorPath</key><string>$logfile</string>
  <!-- Nice: whatever else login is doing matters more than a maintenance lane. -->
  <key>ProcessType</key><string>Background</string>
</dict></plist>
EOF
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
  --write-oneshot) MODE=oneshot ;;
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

# ── --write-oneshot <lane> ──────────────────────────────────────────────
# The seat's launch tier for work that outlives a harness task (SKILL.md,
# "The run channel"). Deliberately NOT a LaunchAgent: the plist lands under
# the state dir, so bootstrapping it arms ONE run and nothing survives the
# logout. The login-time agents in ~/Library/LaunchAgents are a different
# artifact and stay REJECTED by operator decision (DEFAULTS_LEDGER.md,
# "Run-if-stale launchd triggers"); this mode does not re-raise them.
if [ "$MODE" = oneshot ]; then
  lane="${2:-}"
  [ -n "$lane" ] || { echo "run-if-stale --write-oneshot: name a lane (${LANES[*]})" >&2; exit 2; }
  lane_preset "$lane" || { echo "run-if-stale: unknown lane \`$lane\` (have: ${LANES[*]})" >&2; exit 2; }
  mkdir -p "$STATE_DIR"
  label="com.svrn.$lane-oneshot"
  plist="$STATE_DIR/$label.plist"
  write_plist "$plist" "$label" "$lane" "$STATE_DIR/$lane.oneshot.log" 0
  echo "wrote $plist  ->  run-if-stale.sh $lane   (repo $REPO)"
  [ -x "$CMD" ] || echo "WARNING: lane command $CMD is missing or not executable — this would fire into nothing."
  echo
  echo "NOT LOADED — arming launchd work is the operator's act, never a script's."
  echo "  load+fire: launchctl bootstrap gui/$(id -u) $plist"
  echo "  watch:     tail -f $STATE_DIR/$lane.oneshot.log"
  echo "  proof:     launchctl print gui/$(id -u)/$label | grep -E 'state|runs|last exit'"
  echo "  STOP:      launchctl bootout gui/$(id -u)/$label"
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
    write_plist "$plist" "$label" "$l" "$STATE_DIR/$l.guard.log" "$DELAY_SECS"
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

  # Hermetic by construction: the commons is faked at the command boundary
  # for every case below, so the self-test cannot flake on whether some real
  # run happens to hold the daemon claim right now — and the HELD case below
  # is a real refusal on the real decision path, not a mocked branch.
  export RUN_IF_STALE_CLAIM_CMD='/bin/echo {"verdict":"free"}'

  echo "run-if-stale self-test (window ${STALE_HOURS}h) in $tmp"
  check "no marker at all -> fires"                    0 1
  check "marker just written by that fire -> skips"    3 1
  touch -t "$(date -v-30H +%Y%m%d%H%M 2>/dev/null || date -d '30 hours ago' +%Y%m%d%H%M)" "$marker"
  check "marker aged 30h -> fires again"               0 2
  RUN_IF_STALE_HOURS=99999 RUN_IF_STALE_DELAY=0 "$0" --marker "$marker" --cmd "$cmd" --label selftest >/dev/null 2>&1
  if [ $? = 3 ]; then echo "ok:   the window is what decides (99999h -> skip)"; else echo "FAIL: window override ignored"; fails=$((fails+1)); fi

  # ── the quiet-box gate, watched in both directions ────────────────────
  # A stale marker AND a held claim must skip, and the same stale marker must
  # still fire once the claim is free — which is what proves the refusal came
  # from the commons and not from the window.
  age30() { touch -t "$(date -v-30H +%Y%m%d%H%M 2>/dev/null || date -d '30 hours ago' +%Y%m%d%H%M)" "$marker"; }
  age30
  RUN_IF_STALE_DELAY=0 RUN_IF_STALE_CLAIM_CMD='/bin/echo {"verdict":"held"}' \
    "$0" --marker "$marker" --cmd "$cmd" --label selftest >"$tmp/held" 2>&1
  got=$?
  if [ "$got" = 4 ] && grep -q 'HELD' "$tmp/held" && [ "$(fired_count)" = 2 ]; then
    echo "ok:   held daemon claim -> skip (exit 4, lane not fired, marker not moved)"
  else
    echo "FAIL: held claim did not skip — exit $got want 4, fires $(fired_count) want 2"
    sed 's/^/        /' "$tmp/held"; fails=$((fails+1))
  fi
  check "same stale marker, claim free -> fires"       0 3
  # `expired` is NOT `held`: the holder died, so the lane is clear to run.
  age30
  RUN_IF_STALE_DELAY=0 RUN_IF_STALE_CLAIM_CMD='/bin/echo {"verdict":"expired"}' \
    "$0" --marker "$marker" --cmd "$cmd" --label selftest >"$tmp/exp" 2>&1
  got=$?
  for _ in $(seq 1 40); do [ "$(fired_count)" -ge 4 ] && break; sleep 0.25; done
  if [ "$got" = 0 ] && [ "$(fired_count)" = 4 ]; then
    echo "ok:   expired claim -> fires (a dead holder does not block the box)"
  else
    echo "FAIL: expired claim blocked the lane — exit $got, fires $(fired_count) want 4"
    sed 's/^/        /' "$tmp/exp"; fails=$((fails+1))
  fi

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

# The box has to be quiet, not just the marker stale. Checked AFTER the
# window (a fresh marker is cheaper to read than the commons) and BEFORE the
# marker is moved: a lane skipped for contention has not run, so it must
# still be due the next time this guard fires.
SCOPE="${RUN_IF_STALE_CLAIM_SCOPE:-daemon:$(node_name):nightly-lanes}"
VERDICT="$(daemon_claim_verdict "$SCOPE")"
case "$VERDICT" in
  held)
    log "skip: $SCOPE is HELD by another run — the box is busy (marker not moved)"
    exit 4 ;;
  free|expired)
    log "quiet-box: $SCOPE is $VERDICT — proceeding" ;;
  *)
    # Not evidence of a busy box. Named rather than defaulted (§18.3).
    log "quiet-box: could not read $SCOPE (verdict $VERDICT) — proceeding anyway" ;;
esac

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
