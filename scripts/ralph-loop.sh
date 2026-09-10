#!/bin/bash
# ralph-loop.sh — a commit-driven campaign loop over a workdir.
#
# This is the shape that actually moves: one small unit per iteration, and the
# loop advances when HEAD advances. It is svrnmesh-cln's proven ralph loop
# (49 build orders in four days) with the supervision hand-rolled loops kept
# failing to include:
#
#   * progress is a COMMIT, not a claim. A fresh session does one unit, commits,
#     updates the queue; the loop counts commits, and MAX_STALL iterations with
#     no commit halts it and notifies.
#   * the queue is a file in the repo (ralph/STATE.md), so a fresh session has
#     the whole plan as memory; there is no per-order DONE the loop blocks on.
#   * reviews run every REVIEW_EVERY commits and do not consume a work slot.
#   * a session has a wall-clock timeout AND a staleness killer; a crash is
#     never silent (heartbeat + status file; launchd KeepAlive={SuccessfulExit:
#     false} restarts a crash and stops on a clean finish).
#   * permission auto-rejections are detected and reported, not swallowed.
#   * `--self-test` proves the watchdog fires on a hung session.
#
# Usage:
#   ralph-loop.sh --workdir DIR --label NAME --prompt ralph/PROMPT.md \
#     --review-prompt ralph/REVIEW_PROMPT.md [--review-every 3] \
#     [--max-stall 3] [--max-iter 200] [--session-timeout 3600] \
#     [--stale-after 1800] [--done-file ralph/DONE] [--stop-file ralph/STOP] \
#     [--needs-human-file ralph/NEEDS_HUMAN.md] [--last-review ralph/.last_review] \
#     [--notify] [--install-launchd] [--plan] [--self-test]
set -u
ORIG_ARGS=("$@")
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

WORKDIR=""
LABEL="loop"
PROMPT="ralph/PROMPT.md"
REVIEW_PROMPT="ralph/REVIEW_PROMPT.md"
REVIEW_EVERY=3
MAX_STALL=3
MAX_ITER=200
SESSION_TIMEOUT=3600
STALE_AFTER=1800
DONE_FILE="ralph/DONE"
STOP_FILE="ralph/STOP"
NEEDS_HUMAN="ralph/NEEDS_HUMAN.md"
LAST_REVIEW="ralph/.last_review"
NOTIFY=0
INSTALL=0
PLAN=0
SELF_TEST=0
OPENCODE_BIN="${RALPH_OPENCODE_BIN:-opencode}"

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --prompt) PROMPT="$2"; shift 2 ;;
    --review-prompt) REVIEW_PROMPT="$2"; shift 2 ;;
    --review-every) REVIEW_EVERY="$2"; shift 2 ;;
    --max-stall) MAX_STALL="$2"; shift 2 ;;
    --max-iter) MAX_ITER="$2"; shift 2 ;;
    --session-timeout) SESSION_TIMEOUT="$2"; shift 2 ;;
    --stale-after) STALE_AFTER="$2"; shift 2 ;;
    --done-file) DONE_FILE="$2"; shift 2 ;;
    --stop-file) STOP_FILE="$2"; shift 2 ;;
    --needs-human-file) NEEDS_HUMAN="$2"; shift 2 ;;
    --last-review) LAST_REVIEW="$2"; shift 2 ;;
    --notify) NOTIFY=1; shift ;;
    --install-launchd) INSTALL=1; shift ;;
    --plan) PLAN=1; shift ;;
    --self-test) SELF_TEST=1; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "ralph-loop: unknown flag $1" >&2; exit 2 ;;
  esac
done

[ -n "$WORKDIR" ] || { echo "ralph-loop: --workdir is required" >&2; exit 2; }
cd "$WORKDIR" || exit 2
REPO="$PWD"

STATE_DIR="${HOME}/.svrnmesh/ralph/$(basename "$REPO")-${LABEL}"
mkdir -p "$STATE_DIR/logs"
STATUS="$STATE_DIR/status.json"
LOG="$STATE_DIR/loop.log"

# Runtime markers must not dirty the tree the loop commits into.
mkdir -p .git/info 2>/dev/null
for f in "$LOG" "$STATUS" "$DONE_FILE" "$STOP_FILE" "$NEEDS_HUMAN" "$LAST_REVIEW"; do
  printf '%s\n' "$f" >> .git/info/exclude
done
sort -u .git/info/exclude -o .git/info/exclude 2>/dev/null || true

now() { date -u +%FT%TZ; }
say() { echo "$(now) $*" | tee -a "$LOG"; }
status() { printf '{"ts":"%s","unit":"%s","iter":%s,"phase":"%s","detail":"%s"}\n' \
  "$(now)" "$1" "$2" "$3" "$4" > "$STATUS"; }
notify() { [ "$NOTIFY" -eq 1 ] || return 0
  command -v osascript >/dev/null 2>&1 && \
    osascript -e "display notification \"$2\" with title \"ralph: $1\"" >/dev/null 2>&1; }
die_halt() { status "-" 0 halted "$1"; : > "$STOP_FILE"; say "HALT: $1"; notify "HALT" "$1"; exit 0; }
trap 'rc=$?; if [ $rc -ne 0 ] && [ ! -f "$STOP_FILE" ] && [ ! -f "$DONE_FILE" ]; then status "-" 0 crashed "exit $rc"; say "CRASH exit $rc (launchd restarts)"; notify "CRASH" "exit $rc"; fi' EXIT

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
  plutil -lint "$PLIST" >/dev/null || { echo "ralph-loop: invalid plist" >&2; exit 1; }
  echo "wrote $PLIST"
  echo "load: launchctl bootstrap gui/\$(id -u) $PLIST"
  echo "stop: launchctl bootoff gui/\$(id -u)/$PLIST_LABEL"
  exit 0
fi

# --- one session, with a wall-clock timeout, a staleness killer, and
#     permission-rejection detection ---
SESSION_KILLED=""; SESSION_RC=0
run_session() { # prompt_file tag
  local prompt="$1" tag="$2"
  local log="$STATE_DIR/logs/${tag}.out"
  SESSION_KILLED=""; SESSION_RC=0
  local before after
  before=$(grep -c 'auto-rejecting' "$log" 2>/dev/null || true); before=${before:-0}
  "$OPENCODE_BIN" run "$(cat "$prompt")" > "$log" 2>&1 &
  local pid=$! start last_change t lmt
  start=$(date +%s); last_change=$start
  while kill -0 "$pid" 2>/dev/null; do
    sleep 30
    t=$(date +%s)
    [ -f "$log" ] && { lmt=$(stat -f %m "$log" 2>/dev/null || echo "$last_change"); [ "$lmt" -gt "$last_change" ] && last_change=$lmt; }
    status "$tag" 0 running "pid $pid"
    if [ $((t - start)) -gt "$SESSION_TIMEOUT" ]; then
      SESSION_KILLED="timeout ${SESSION_TIMEOUT}s"; say "$tag: $SESSION_KILLED — killing (tree resumes it)"
      kill "$pid" 2>/dev/null; sleep 5; kill -9 "$pid" 2>/dev/null; break
    fi
    if [ $((t - last_change)) -gt "$STALE_AFTER" ]; then
      SESSION_KILLED="stale $((t-last_change))s"; say "$tag: $SESSION_KILLED — killing"
      kill "$pid" 2>/dev/null; sleep 5; kill -9 "$pid" 2>/dev/null; break
    fi
  done
  wait "$pid" 2>/dev/null; SESSION_RC=$?
  after=$(grep -c 'auto-rejecting' "$log" 2>/dev/null || true); after=${after:-0}
  if [ "$after" -gt "$before" ]; then
    say "$tag: $((after-before)) permission auto-rejections — extend opencode.json"
    notify "permissions" "$((after-before)) auto-rejections this session"
  fi
}

if [ "$SELF_TEST" -eq 1 ]; then
  STUB="$STATE_DIR/fake-opencode"
  printf '#!/bin/bash\nsleep 3600\n' > "$STUB"; chmod +x "$STUB"
  OPENCODE_BIN="$STUB"; STALE_AFTER=8; SESSION_TIMEOUT=3600
  printf 'x\n' > "$STATE_DIR/st.prompt"; t0=$(date +%s)
  run_session "$STATE_DIR/st.prompt" selftest; t1=$(date +%s)
  rm -f "$STUB"
  if [ -n "$SESSION_KILLED" ] && [ $((t1-t0)) -lt 90 ]; then
    echo "self-test: PASS — watchdog fired ($SESSION_KILLED) after $((t1-t0))s"; exit 0
  fi
  echo "self-test: FAIL — hung session not killed" >&2; exit 1
fi

commits_since_review() {
  local last base
  last=$(cat "$LAST_REVIEW" 2>/dev/null || true)
  if [ -n "$last" ] && git rev-parse --verify -q "$last" >/dev/null; then base=$last
  else base=$(git rev-list --max-parents=0 HEAD 2>/dev/null || echo HEAD); fi
  git rev-list --count "$base..HEAD" 2>/dev/null || echo 0
}

if [ "$PLAN" -eq 1 ]; then
  echo "prompt: $PROMPT"; echo "review: $REVIEW_PROMPT every $REVIEW_EVERY commits"
  echo "commits since review: $(commits_since_review)"
  echo "head: $(git rev-parse --short HEAD 2>/dev/null)"
  exit 0
fi

[ -f "$DONE_FILE" ] && { say "already done"; exit 0; }
[ -f "$STOP_FILE" ] && { say "halted; remove $STOP_FILE to resume"; exit 0; }

say "start $LABEL at $(git rev-parse --short HEAD 2>/dev/null) review_every=$REVIEW_EVERY max_stall=$MAX_STALL"
stall=0; iter=0
while [ "$iter" -lt "$MAX_ITER" ]; do
  iter=$((iter+1))
  [ -f "$STOP_FILE" ] && { say "STOP after $iter iterations"; exit 0; }
  [ -f "$DONE_FILE" ] && { say "DONE after $iter iterations ($(git rev-parse --short HEAD))"; notify "DONE" "$LABEL complete"; exit 0; }
  if [ -f "$NEEDS_HUMAN" ]; then
    say "operator needed after $iter iterations:"; cat "$NEEDS_HUMAN" | tee -a "$LOG"
    notify "NEEDS_HUMAN" "decision package in $NEEDS_HUMAN"; exit 2
  fi

  if [ "$REVIEW_EVERY" -gt 0 ] && [ "$(commits_since_review)" -ge "$REVIEW_EVERY" ]; then
    say "review iteration $iter ($(commits_since_review) commits since last review)"
    run_session "$REVIEW_PROMPT" "review-$iter"
    iter=$((iter-1))
    continue
  fi

  before=$(git rev-parse HEAD 2>/dev/null || echo none)
  say "iteration $iter from $(git rev-parse --short HEAD 2>/dev/null || echo none)"
  run_session "$PROMPT" "iter-$iter"
  after=$(git rev-parse HEAD 2>/dev/null || echo none)
  if [ "$before" = "$after" ]; then
    stall=$((stall+1)); say "no commit this iteration (stall $stall/$MAX_STALL)"
    notify "stall" "no commit (stall $stall/$MAX_STALL)"
    [ "$stall" -ge "$MAX_STALL" ] && die_halt "$MAX_STALL iterations without a commit — inspect $LOG"
  else
    stall=0; say "progress -> $(git rev-parse --short HEAD)"
  fi
done
die_halt "MAX_ITER=$MAX_ITER reached"
