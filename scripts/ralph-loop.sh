#!/bin/bash
# ralph-loop.sh — a supervised agent loop over an ordered queue of work units.
#
# Hardened against the failure modes that made earlier hand-rolled loops
# silently useless:
#
#   * A crash is never silent. A status/heartbeat file is written throughout;
#     a DONE marker is written only on clean completion and a HALT marker only
#     on a deliberate stop. The launchd template uses
#     KeepAlive={SuccessfulExit:false}, so a crash auto-restarts and a clean
#     finish stops. Remove the halt marker to resume.
#   * A hung session cannot eat hours. Every session has a wall-clock timeout
#     and a staleness killer (no output growth for --stale-after seconds).
#   * Work survives a SIGKILL. The prompt carries an incremental-commit
#     contract: commit every distinct step, never hold >10 min uncommitted.
#   * The failure path is testable. `--self-test` runs a fake hung session and
#     asserts the watchdog killed it.
#
# Generic by design: it knows nothing about any one repo. The repo supplies a
# queue (file or command), a unit path template, and an accept command.
#
# Usage:
#   ralph-loop.sh --workdir DIR --label NAME (--queue FILE | --queue-cmd CMD)
#     [--unit-path 'orders/{id}'] [--prompt-file order.md] [--done-file DONE.md]
#     [--accept-cmd CMD] [--max-attempts N] [--session-timeout S]
#     [--stale-after S] [--review-every N] [--principles PATH]
#     [--review-path 'orders/reviews/review-{n}'] [--notify]
#     [--plan] [--self-test]
set -u
ORIG_ARGS=("$@")
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

WORKDIR=""
LABEL="loop"
QUEUE=""
QUEUE_CMD=""
UNIT_PATH='orders/{id}'
PROMPT_FILE="order.md"
DONE_FILE="DONE.md"
WITHDRAWN_FILE="WITHDRAWN.md"
ACCEPT_CMD=""
MAX_ATTEMPTS=12
SESSION_TIMEOUT=3600
STALE_AFTER=900
REVIEW_EVERY=0
PRINCIPLES=""
REVIEW_PATH='orders/reviews/review-{n}'
NOTIFY=0
PLAN=0
SELF_TEST=0
INSTALL=0
OPENCODE_BIN="${RALPH_OPENCODE_BIN:-opencode}"

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --queue) QUEUE="$2"; shift 2 ;;
    --queue-cmd) QUEUE_CMD="$2"; shift 2 ;;
    --unit-path) UNIT_PATH="$2"; shift 2 ;;
    --prompt-file) PROMPT_FILE="$2"; shift 2 ;;
    --done-file) DONE_FILE="$2"; shift 2 ;;
    --accept-cmd) ACCEPT_CMD="$2"; shift 2 ;;
    --max-attempts) MAX_ATTEMPTS="$2"; shift 2 ;;
    --session-timeout) SESSION_TIMEOUT="$2"; shift 2 ;;
    --stale-after) STALE_AFTER="$2"; shift 2 ;;
    --review-every) REVIEW_EVERY="$2"; shift 2 ;;
    --principles) PRINCIPLES="$2"; shift 2 ;;
    --review-path) REVIEW_PATH="$2"; shift 2 ;;
    --notify) NOTIFY=1; shift ;;
    --plan) PLAN=1; shift ;;
    --self-test) SELF_TEST=1; shift ;;
    --install-launchd) INSTALL=1; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "ralph-loop: unknown flag $1" >&2; exit 2 ;;
  esac
done

[ -n "$WORKDIR" ] || { echo "ralph-loop: --workdir is required" >&2; exit 2; }
cd "$WORKDIR" || exit 2
REPO="$PWD"

STATE_DIR="${HOME}/.svrnmesh/ralph/$(basename "$REPO")-${LABEL}"
mkdir -p "$STATE_DIR/logs" "$STATE_DIR/accepted"
DONE_MARKER="$STATE_DIR/done"
HALT_MARKER="$STATE_DIR/halt"
STATUS="$STATE_DIR/status.json"
LOG="$STATE_DIR/loop.log"

if [ "$INSTALL" -eq 1 ]; then
  PLIST_LABEL="dev.ralph.$(basename "$REPO")-${LABEL}"
  PLIST="${HOME}/Library/LaunchAgents/${PLIST_LABEL}.plist"
  {
    echo '<?xml version="1.0" encoding="UTF-8"?>'
    echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
    echo '<plist version="1.0"><dict>'
    echo "  <key>Label</key><string>${PLIST_LABEL}</string>"
    echo '  <key>ProgramArguments</key><array>'
    echo '    <string>/bin/bash</string>'
    echo "    <string>${SELF}</string>"
    for a in ${ORIG_ARGS[@]+"${ORIG_ARGS[@]}"}; do
      [ "$a" = "--install-launchd" ] && continue
      echo "    <string>${a}</string>"
    done
    echo '  </array>'
    echo "  <key>WorkingDirectory</key><string>${REPO}</string>"
    echo '  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>'
    echo '  <key>ThrottleInterval</key><integer>30</integer>'
    echo '  <key>RunAtLoad</key><true/>'
    echo '  <key>EnvironmentVariables</key><dict>'
    echo "    <key>PATH</key><string>${PATH}</string>"
    echo "    <key>HOME</key><string>${HOME}</string>"
    [ -n "${OPENCODE_CONFIG:-}" ] && echo "    <key>OPENCODE_CONFIG</key><string>${OPENCODE_CONFIG}</string>"
    echo '  </dict>'
    echo "  <key>StandardOutPath</key><string>${STATE_DIR}/launchd.log</string>"
    echo "  <key>StandardErrorPath</key><string>${STATE_DIR}/launchd.log</string>"
    echo '</dict></plist>'
  } > "$PLIST"
  plutil -lint "$PLIST" >/dev/null || { echo "ralph-loop: generated an invalid plist" >&2; exit 1; }
  echo "wrote $PLIST"
  echo "load: launchctl bootstrap gui/\$(id -u) $PLIST"
  echo "stop: launchctl bootout gui/\$(id -u)/$PLIST_LABEL"
  echo "watch: tail -f $STATE_DIR/launchd.log"
  exit 0
fi

now() { date -u +%FT%TZ; }
say() { echo "$(now) $*" | tee -a "$LOG"; }
status() { # unit attempt phase detail
  printf '{"ts":"%s","unit":"%s","attempt":%s,"phase":"%s","detail":"%s"}\n' \
    "$(now)" "$1" "$2" "$3" "$4" > "$STATUS"
}
notify() { # title body
  [ "$NOTIFY" -eq 1 ] || return 0
  command -v osascript >/dev/null 2>&1 && \
    osascript -e "display notification \"$2\" with title \"ralph-loop: $1\"" >/dev/null 2>&1
}
clean_tree() { [ -z "$(git status --porcelain)" ]; }
die_halt() { # reason
  status "-" 0 halted "$1"
  : > "$HALT_MARKER"
  say "HALT: $1"
  notify "HALT" "$1"
  exit 0
}
trap 'rc=$?; if [ $rc -ne 0 ] && [ ! -f "$HALT_MARKER" ] && [ ! -f "$DONE_MARKER" ]; then status "-" 0 crashed "exit $rc"; say "CRASH: exit $rc (launchd will restart)"; notify "CRASH" "exit $rc"; fi' EXIT

# --- the session runner: timeout + staleness, in one place ---
SESSION_KILLED=""
SESSION_RC=0
run_session() { # prompt_file log unit attempt
  local prompt="$1" log="$2" unit="$3" attempt="$4"
  SESSION_KILLED=""; SESSION_RC=0
  "$OPENCODE_BIN" run "$(cat "$prompt")" > "$log" 2>&1 &
  local pid=$! start lmt last_change
  start=$(date +%s); last_change=$start
  while kill -0 "$pid" 2>/dev/null; do
    sleep 15
    local t; t=$(date +%s)
    if [ -f "$log" ]; then lmt=$(stat -f %m "$log" 2>/dev/null || echo "$last_change"); [ "$lmt" -gt "$last_change" ] && last_change=$lmt; fi
    status "$unit" "$attempt" running "pid $pid"
    if [ $((t - start)) -gt "$SESSION_TIMEOUT" ]; then
      SESSION_KILLED="timeout after $((t-start))s"; say "$unit/$attempt: $SESSION_KILLED — killing"
      kill "$pid" 2>/dev/null; sleep 3; kill -9 "$pid" 2>/dev/null; break
    fi
    if [ $((t - last_change)) -gt "$STALE_AFTER" ]; then
      SESSION_KILLED="stale $((t-last_change))s"; say "$unit/$attempt: $SESSION_KILLED — killing"
      kill "$pid" 2>/dev/null; sleep 3; kill -9 "$pid" 2>/dev/null; break
    fi
  done
  wait "$pid" 2>/dev/null
  SESSION_RC=$?
}

incremental_contract() { # id
  cat <<EOF
THE INCREMENTAL-COMMIT CONTRACT (the reason this session exists):
You may be SIGKILLed at any moment, without warning. Treat every successful
tool result as possibly your last. Commit after EVERY distinct step that leaves
the tree in a working state — never hold more than ~10 minutes of work
uncommitted. Commit message: "$1: <one line>". A doc change lands in the same
commit as the code it describes. Sessions before you died uncommitted and lost
hours; do not repeat that.
EOF
}

done_contract() { # id dir done_file
  cat <<EOF
THE DONE CONTRACT:
When the unit's tests pass and the accept command is green AND
\`git status --porcelain\` is empty:
1. Commit the work (git add -A && git commit) with message "$1: <one line>".
2. Write $2/$3 with exactly these sections: What was built (path:line);
   Red first: each row watched red and the bug it caught; Gate output (tail);
   What I did NOT do.
3. Commit it ("$1: done claim").
Work autonomously; do not ask questions; the mechanical gate is the reviewer.
If the unit's "not worth continuing" clause fires: write $2/WITHDRAWN.md
naming what fired, commit, stop.
EOF
}

prompt_for() { # id dir attempt out_log
  local id="$1" dir="$2" attempt="$3" prevlog="$4" src="$dir/$PROMPT_FILE"
  if [ "$attempt" -eq 1 ]; then
    printf 'You are executing %s in %s.\nRead %s first; it is bound below.\n\n' "$id" "$(basename "$REPO")" "$src"
    if ! clean_tree; then
      printf 'NOTE: the tree already holds uncommitted work from a prior attempt:\n'
      git status --short
      printf 'Inspect it and continue from it; do not discard work already done.\n\n'
    fi
  else
    printf 'Attempt %s of %s on %s. Prior attempt(s) were interrupted or returned without a done claim.\n' "$attempt" "$MAX_ATTEMPTS" "$id"
    printf 'What the last attempt did (output tail):\n'; tail -80 "$prevlog" 2>/dev/null
    printf 'Commits landed so far:\n'; git log --oneline -10
    printf 'Current tree state:\n'; git status --short
    printf '\n'
  fi
  [ -n "$STANDING_RULES" ] && printf '%s\n\n' "$STANDING_RULES"
  incremental_contract "$id"
  done_contract "$id" "$dir" "$DONE_FILE"
  printf '\n=== %s ===\n\n' "$src"
  cat "$src"
  [ "$attempt" -gt 1 ] && printf '\nContinue from the current tree state. Fix what the gate names; do not rewrite what already passes.\n'
}

accepts() { # dir
  [ -f "$1/$DONE_FILE" ] || return 1
  clean_tree || return 1
  [ -n "$ACCEPT_CMD" ] && ! bash -c "$ACCEPT_CMD" >/dev/null 2>&1 && return 1
  return 0
}

run_unit() { # id dir
  local id="$1" dir="$2" attempt=1 rc
  if [ -f "$STATE_DIR/accepted/$id" ]; then say "$id already accepted"; return 0; fi
  if accepts "$dir"; then
    : > "$STATE_DIR/accepted/$id"
    say "$id already meets its accept criteria"
    return 0
  fi
  while [ "$attempt" -le "$MAX_ATTEMPTS" ]; do
    local prompt="$STATE_DIR/logs/$id-$attempt.prompt.md" log="$STATE_DIR/logs/$id-$attempt.out"
    prompt_for "$id" "$dir" "$attempt" "$STATE_DIR/logs/$id-$((attempt-1)).out" > "$prompt"
    say "$id attempt $attempt/$MAX_ATTEMPTS"
    run_session "$prompt" "$log" "$id" "$attempt"; rc=$SESSION_RC
    [ -n "$SESSION_KILLED" ] && say "$id/$attempt killed: $SESSION_KILLED"
    if [ -f "$dir/WITHDRAWN.md" ]; then die_halt "$id withdrew by its not-worth-continuing clause"; fi
    if accepts "$dir"; then
      : > "$STATE_DIR/accepted/$id"
      say "$id ACCEPTED (exit $rc)"
      return 0
    fi
    [ -f "$dir/$DONE_FILE" ] && { mv "$dir/$DONE_FILE" "$STATE_DIR/logs/$id-$attempt.rejected.md"; say "$id done claim rejected (tree dirty or accept red)"; }
    attempt=$((attempt+1))
  done
  die_halt "$id: $MAX_ATTEMPTS attempts exhausted"
}

run_review() { # n ids
  local n="$1" ids="$2" dir
  dir=$(printf '%s' "$REVIEW_PATH" | sed "s/{n}/$n/")
  mkdir -p "$dir"
  {
    printf '# Review %s\n\n' "$n"
    printf 'Review the code the last batch landed: %s\n\n' "$ids"
    printf 'Standard: %s — hold "The eleven" (they are held, not looked up); the numbered sections carry the evidence.\n\n' "${PRINCIPLES:-the architectural principles}"
    printf 'Find and fix, behaviour-preserving: violations of the principles; duplicated deciders, helpers and accessors (one implementation per threshold, scorer, schema, key); stringly-typed closed sets; oversized files (split or flag); missing tracing on non-obvious decisions; layer violations; dead code; missed reuse of what already exists. Consolidate rather than accrete. A review that changes behaviour is a bug.\n\n'
    printf 'Done when: a report at %s/report.md lists each finding (principle + path:line) and each fix, or says honestly none was found; every fix is committed; the accept command is green.\n' "$dir"
  } > "$dir/$PROMPT_FILE"
  run_unit "review-$n" "$dir" || return $?
  [ -f "$dir/report.md" ] || say "review-$n accepted without report.md"
  return 0
}

# --- self-test: prove the staleness watchdog fires on a hung session ---
if [ "$SELF_TEST" -eq 1 ]; then
  STUB="$STATE_DIR/fake-opencode"
  printf '#!/bin/bash\nsleep 3600\n' > "$STUB"; chmod +x "$STUB"
  OPENCODE_BIN="$STUB"; STALE_AFTER=8; SESSION_TIMEOUT=3600
  printf 'stand-in prompt\n' > "$STATE_DIR/self-test.prompt"
  t0=$(date +%s)
  run_session "$STATE_DIR/self-test.prompt" "$STATE_DIR/logs/self-test.out" selftest 1
  t1=$(date +%s)
  rm -f "$STUB"
  if [ -n "$SESSION_KILLED" ] && [ $((t1-t0)) -lt 60 ]; then
    echo "self-test: PASS — watchdog fired ($SESSION_KILLED) after $((t1-t0))s"
    exit 0
  fi
  echo "self-test: FAIL — hung session was not killed" >&2
  exit 1
fi

# --- resolve the queue ---
if [ -n "$QUEUE_CMD" ]; then
  QUEUE_LIST=$(bash -c "$QUEUE_CMD") || { echo "ralph-loop: queue command failed" >&2; exit 2; }
elif [ -n "$QUEUE" ]; then
  QUEUE_LIST=$(cat "$QUEUE")
else
  echo "ralph-loop: need --queue or --queue-cmd" >&2; exit 2
fi
QUEUE_LIST=$(echo "$QUEUE_LIST" | tr ' ' '\n' | sed '/^$/d')

if [ "$PLAN" -eq 1 ]; then
  echo "queue: $(echo $QUEUE_LIST)"
  exit 0
fi

# --- the run ---
[ -f "$DONE_MARKER" ] && { say "already done ($DONE_MARKER)"; exit 0; }
[ -f "$HALT_MARKER" ] && { say "halted; remove $HALT_MARKER to resume"; exit 0; }

# Standing rules, if the repo has a PLAN.md with them (ersilia does); generic
# repos simply have none.
STANDING_RULES=$(awk '/^## Standing rules for every order/,/^---$/' PLAN.md 2>/dev/null)

say "start: $(echo $QUEUE_LIST | tr '\n' ' ')"
COUNT=0; REVIEWED=""
for unit in $QUEUE_LIST; do
  dir=$(printf '%s' "$UNIT_PATH" | sed "s/{id}/$unit/")
  run_unit "$unit" "$dir" || exit $?
  COUNT=$((COUNT+1)); REVIEWED="$REVIEWED $unit"
  if [ "$REVIEW_EVERY" -gt 0 ] && [ $((COUNT % REVIEW_EVERY)) -eq 0 ]; then
    run_review $((COUNT / REVIEW_EVERY)) "$REVIEWED" || exit $?
    REVIEWED=""
  fi
done
if [ "$REVIEW_EVERY" -gt 0 ] && [ -n "$(echo $REVIEWED)" ]; then
  run_review final "$REVIEWED" || exit $?
fi
status "-" 0 done "all units accepted"
: > "$DONE_MARKER"
say "DONE: $(echo $QUEUE_LIST | tr '\n' ' ') complete"
notify "DONE" "$LABEL complete"
exit 0
