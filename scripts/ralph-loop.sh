#!/bin/bash
# ralph-loop.sh — the SERIAL campaign loop. One unit at a time, one commit
# advancing it. The session/contract layer (launch in a process group, timeout,
# STOP, dirty-tree note, permission-reject detection, the wait-for marker) lives
# in ralph-lib.sh, shared with ralph-pool.sh.
#
# Run detached and watch it live:
#   nohup ralph-loop.sh --workdir . --label ring1 --prompt ralph/PROMPT.md \
#     >> ralph/log.txt 2>&1 &
#   tail -f ralph/log.txt
set -u
ORIG_ARGS=("$@")
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
LIB="$(cd "$(dirname "$0")" && pwd)/ralph-lib.sh"
# shellcheck source=ralph-lib.sh
. "$LIB"

WORKDIR=""
LABEL="campaign"
PROMPT="ralph/PROMPT.md"
STATE="ralph/STATE.md"
REVIEW_PROMPT=""
REVIEW_EVERY=0
REVIEW_MODEL=""
MAX_STALL=3
MAX_ITER=200
SESSION_TIMEOUT=3600
DONE_FILE="ralph/DONE"
STOP_FILE="ralph/STOP"
NEEDS_HUMAN="ralph/NEEDS_HUMAN.md"
LAST_REVIEW="ralph/.last_review"
NOTIFY=0
PLAN=0
SELF_TEST=0
INSTALL=0
OPENCODE_BIN="${RALPH_OPENCODE_BIN:-opencode}"
MODEL_ARGS=""

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    --prompt) PROMPT="$2"; shift 2 ;;
    --state) STATE="$2"; shift 2 ;;
    --review-prompt) REVIEW_PROMPT="$2"; shift 2 ;;
    --review-every) REVIEW_EVERY="$2"; shift 2 ;;
    --review-model) REVIEW_MODEL="$2"; shift 2 ;;
    --max-stall) MAX_STALL="$2"; shift 2 ;;
    --max-iter) MAX_ITER="$2"; shift 2 ;;
    --session-timeout) SESSION_TIMEOUT="$2"; shift 2 ;;
    --done-file) DONE_FILE="$2"; shift 2 ;;
    --stop-file) STOP_FILE="$2"; shift 2 ;;
    --needs-human-file) NEEDS_HUMAN="$2"; shift 2 ;;
    --last-review) LAST_REVIEW="$2"; shift 2 ;;
    --notify) NOTIFY=1; shift ;;
    --plan) PLAN=1; shift ;;
    --self-test) SELF_TEST=1; shift ;;
    --install-launchd) INSTALL=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "ralph-loop: unknown flag $1" >&2; exit 2 ;;
  esac
done
[ -n "$WORKDIR" ] || { echo "ralph-loop: --workdir is required" >&2; exit 2; }
cd "$WORKDIR" || exit 2
STATE_DIR="${HOME}/.svrnmesh/ralph/$(basename "$PWD")-${LABEL}"
mkdir -p "$STATE_DIR/logs"

# Runtime markers must not dirty the tree the loop commits into.
mkdir -p .git/info 2>/dev/null
for f in "$DONE_FILE" "$STOP_FILE" "$NEEDS_HUMAN" "$LAST_REVIEW" ralph/log.txt; do
  printf '%s\n' "$f" >> .git/info/exclude
done
sort -u .git/info/exclude -o .git/info/exclude 2>/dev/null || true

if [ "$INSTALL" -eq 1 ]; then
  PLIST_LABEL="dev.ralph.$(basename "$PWD")-${LABEL}"
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
    echo "  <key>WorkingDirectory</key><string>${PWD}</string>"
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
  exit 0
fi

current_unit() {
  [ -f "$STATE" ] || return 0
  local u
  u=$(grep -oE '^- \[~\] [A-Za-z0-9-]+' "$STATE" | head -1 | awk '{print $NF}')
  [ -n "$u" ] || u=$(grep -oE '^- \[ \] [A-Za-z0-9-]+' "$STATE" | head -1 | awk '{print $NF}')
  printf '%s' "$u"
}

commits_since_review() {
  local last base
  last=$(cat "$LAST_REVIEW" 2>/dev/null || true)
  if [ -n "$last" ] && git rev-parse --verify -q "$last" >/dev/null; then base=$last
  else base=$(git rev-list --max-parents=0 HEAD 2>/dev/null || echo HEAD); fi
  git rev-list --count "$base..HEAD" 2>/dev/null || echo 0
}

if [ "$SELF_TEST" -eq 1 ]; then
  STUB="$STATE_DIR/fake-opencode"
  printf '#!/bin/bash\nsleep 3600\n' > "$STUB"; chmod +x "$STUB"
  OPENCODE_BIN="$STUB"; SESSION_TIMEOUT=8
  printf 'x\n' > "$STATE_DIR/st.prompt"; t0=$(date +%s)
  run_session "$STATE_DIR/st.prompt" "$PWD" "$STATE_DIR/logs/self-test.out"
  t1=$(date +%s); rm -f "$STUB"
  if [ $((t1 - t0)) -lt 90 ]; then echo "self-test: PASS — watchdog killed the session after $((t1-t0))s"; exit 0; fi
  echo "self-test: FAIL — hung session not killed" >&2; exit 1
fi

if [ "$PLAN" -eq 1 ]; then
  echo "prompt: $PROMPT  review: ${REVIEW_PROMPT:-none} every $REVIEW_EVERY commits"
  echo "commits since review: $(commits_since_review)  head: $(git rev-parse --short HEAD 2>/dev/null)"
  exit 0
fi

[ -f "$DONE_FILE" ] && { say "already done"; exit 0; }
[ -f "$STOP_FILE" ] && { say "halted; remove $STOP_FILE to resume"; exit 0; }

say "start $LABEL at $(git rev-parse --short HEAD 2>/dev/null) review_every=$REVIEW_EVERY max_stall=$MAX_STALL"
stall=0; iter=0
while [ "$iter" -lt "$MAX_ITER" ]; do
  iter=$((iter + 1))
  [ -f "$STOP_FILE" ] && { say "STOP after $iter iterations"; exit 0; }
  [ -f "$DONE_FILE" ] && { say "DONE after $iter iterations ($(git rev-parse --short HEAD))"; notify "DONE" "$LABEL complete"; exit 0; }
  if [ -f "$NEEDS_HUMAN" ]; then
    say "operator needed after $iter iterations:"; cat "$NEEDS_HUMAN"
    notify "NEEDS_HUMAN" "decision package in $NEEDS_HUMAN"; exit 2
  fi

  if ! wait_for_marker; then sleep 120; iter=$((iter - 1)); continue; fi

  if [ "$REVIEW_EVERY" -gt 0 ] && [ -n "$REVIEW_PROMPT" ] && [ "$(commits_since_review)" -ge "$REVIEW_EVERY" ]; then
    say "review iteration $iter ($(commits_since_review) commits since last review)"
    MODEL_ARGS=""; [ -n "$REVIEW_MODEL" ] && MODEL_ARGS="--model $REVIEW_MODEL"
    run_session "$REVIEW_PROMPT" "$PWD" "$STATE_DIR/logs/review-$iter.out"
    iter=$((iter - 1)); continue
  fi

  before=$(git rev-parse HEAD 2>/dev/null || echo none)
  echo "[ralph] === iteration $iter $(date -u +%FT%TZ) from $(git rev-parse --short HEAD 2>/dev/null || echo none) ==="
  unit=$(current_unit)
  MODEL_ARGS=""
  if [ -n "$REVIEW_MODEL" ] && printf '%s' "$unit" | grep -qi 'review'; then
    MODEL_ARGS="--model $REVIEW_MODEL"
    say "unit $unit is a review — model $REVIEW_MODEL"
  fi
  run_session "$PROMPT" "$PWD" "$STATE_DIR/logs/iter-$iter.out"
  after=$(git rev-parse HEAD 2>/dev/null || echo none)
  if [ "$before" = "$after" ]; then
    stall=$((stall + 1)); say "no commit this iteration (stall $stall/$MAX_STALL)"
    notify "stall" "no commit ($stall/$MAX_STALL)"
    [ "$stall" -ge "$MAX_STALL" ] && halt "$MAX_STALL iterations without a commit"
  else
    stall=0; say "progress -> $(git rev-parse --short HEAD)"
  fi
done
halt "MAX_ITER=$MAX_ITER reached"
