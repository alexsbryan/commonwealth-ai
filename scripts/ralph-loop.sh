#!/bin/bash
# ralph-loop.sh — a commit-driven campaign loop.
#
# The shape that works (svrnmesh-cln's ralph loop: 49 build orders in four
# days). Run it detached and watch it live:
#
#   nohup scripts/ralph-loop.sh --workdir . --label ring1 \
#     --prompt ralph/PROMPT.md >> ralph/log.txt 2>&1 &
#   tail -f ralph/log.txt
#
# There is no daemon and no supervisor. The loop is a process; the log is one
# append-only file the agent's output streams into, so progress is visible as
# it happens, not summarised after. The safety is in the loop, not around it:
#
#   * progress is a COMMIT. The loop advances when HEAD advances; --max-stall
#     iterations with no commit halts it and notifies.
#   * a session past --session-timeout is killed (its uncommitted work stays in
#     the tree; the next iteration resumes it).
#   * a stall, a halt, a completion, or a permission auto-rejection notifies.
#   * the queue is ralph/STATE.md; the unit is one order (or one REVIEW).
#
# Usage:
#   ralph-loop.sh --workdir DIR --prompt ralph/PROMPT.md
#     [--label NAME] [--review-prompt FILE] [--review-every N]
#     [--max-stall N] [--max-iter N] [--session-timeout S]
#     [--done-file ralph/DONE] [--stop-file ralph/STOP]
#     [--needs-human-file ralph/NEEDS_HUMAN.md] [--last-review ralph/.last_review]
#     [--notify] [--plan]
set -u

WORKDIR=""
LABEL="campaign"
PROMPT="ralph/PROMPT.md"
REVIEW_PROMPT=""
REVIEW_EVERY=0
MAX_STALL=3
MAX_ITER=200
SESSION_TIMEOUT=3600
DONE_FILE="ralph/DONE"
STOP_FILE="ralph/STOP"
NEEDS_HUMAN="ralph/NEEDS_HUMAN.md"
LAST_REVIEW="ralph/.last_review"
STATE="ralph/STATE.md"
REVIEW_MODEL=""
NOTIFY=0
PLAN=0

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
    --done-file) DONE_FILE="$2"; shift 2 ;;
    --stop-file) STOP_FILE="$2"; shift 2 ;;
    --needs-human-file) NEEDS_HUMAN="$2"; shift 2 ;;
    --last-review) LAST_REVIEW="$2"; shift 2 ;;
    --state) STATE="$2"; shift 2 ;;
    --review-model) REVIEW_MODEL="$2"; shift 2 ;;
    --notify) NOTIFY=1; shift ;;
    --plan) PLAN=1; shift ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) echo "ralph-loop: unknown flag $1" >&2; exit 2 ;;
  esac
done

[ -n "$WORKDIR" ] || { echo "ralph-loop: --workdir is required" >&2; exit 2; }
cd "$WORKDIR" || exit 2

# Runtime markers must not dirty the tree the loop commits into.
mkdir -p .git/info 2>/dev/null
for f in "$DONE_FILE" "$STOP_FILE" "$NEEDS_HUMAN" "$LAST_REVIEW" ralph/log.txt; do
  printf '%s\n' "$f" >> .git/info/exclude
done
sort -u .git/info/exclude -o .git/info/exclude 2>/dev/null || true

notify() {
  [ "$NOTIFY" -eq 1 ] || return 0
  /usr/bin/osascript -e "display notification \"${2}\" with title \"ralph: ${1}\"" >/dev/null 2>&1 || true
}
commits_since_review() {
  local last base
  last=$(cat "$LAST_REVIEW" 2>/dev/null || true)
  if [ -n "$last" ] && git rev-parse --verify -q "$last" >/dev/null; then base=$last
  else base=$(git rev-list --max-parents=0 HEAD 2>/dev/null || echo HEAD); fi
  git rev-list --count "$base..HEAD" 2>/dev/null || echo 0
}

# The unit a session is about to run: the one in progress (`[~]`), else the
# first pending (`[ ]`). Used only to route reviews to --review-model.
current_unit() {
  [ -f "$STATE" ] || return 0
  local u
  u=$(grep -oE '^- \[~\] [A-Za-z0-9-]+' "$STATE" | head -1 | awk '{print $NF}')
  [ -n "$u" ] || u=$(grep -oE '^- \[ \] [A-Za-z0-9-]+' "$STATE" | head -1 | awk '{print $NF}')
  printf '%s' "$u"
}
MODEL_ARGS=""

# One session: the agent's output streams to this process's stdout (the log);
# a watchdog kills it past the wall-clock timeout; permission auto-rejections
# are counted from the log.
SESSION_PGID=""
trap 'if [ -n "${SESSION_PGID:-}" ]; then kill -TERM -- -"$SESSION_PGID" 2>/dev/null; sleep 2; kill -KILL -- -"$SESSION_PGID" 2>/dev/null; fi' EXIT
run_session() { # prompt_file
  local prompt="$1" before after child waited input
  input="${TMPDIR:-/tmp}/ralph-input-$$.md"
  {
    if [ -n "$(git status --porcelain)" ]; then
      printf 'NOTE: the tree holds uncommitted work from a prior session:\n'
      git status --short
      printf 'Inspect it and continue from it; do not discard work already done. Commit it as you go.\n\n'
    fi
    cat "$prompt"
  } > "$input"
  before=$(grep -c 'auto-rejecting' ralph/log.txt 2>/dev/null || true); before=${before:-0}
  # Launch the session in its own process group, so a timeout kills the whole
  # tree — cargo, rustc, the embedded Postgres postmasters — and leaves no
  # orphans for the next session to fight.
  set -m
  # shellcheck disable=SC2086
  opencode run $MODEL_ARGS "$(cat "$input")" &
  child=$!
  set +m
  SESSION_PGID="$child"
  waited=0
  while kill -0 "$child" 2>/dev/null && [ "$waited" -lt "$SESSION_TIMEOUT" ]; do
    sleep 30; waited=$((waited + 30))
    [ $((waited % 60)) -eq 0 ] && echo "[ralph]   ... session running ${waited}s (pid $child)"
    if [ -f "$STOP_FILE" ]; then
      echo "[ralph] STOP requested — killing the session process group"
      kill -TERM -- -"$SESSION_PGID" 2>/dev/null; sleep 5; kill -KILL -- -"$SESSION_PGID" 2>/dev/null
      break
    fi
  done
  if kill -0 "$child" 2>/dev/null; then
    echo "[ralph] session exceeded ${SESSION_TIMEOUT}s — killing the process group (the tree resumes it)"
    notify "iteration timeout" "killed at ${SESSION_TIMEOUT}s"
    kill -TERM -- -"$SESSION_PGID" 2>/dev/null; sleep 5; kill -KILL -- -"$SESSION_PGID" 2>/dev/null
  fi
  wait "$child" 2>/dev/null || true
  SESSION_PGID=""
  rm -f "$input"
  after=$(grep -c 'auto-rejecting' ralph/log.txt 2>/dev/null || true); after=${after:-0}
  if [ "$after" -gt "$before" ]; then
    echo "[ralph] WARNING: $((after - before)) permission auto-rejections this session — extend opencode.json"
    notify "permissions" "$((after - before)) auto-rejections"
  fi
}

if [ "$PLAN" -eq 1 ]; then
  echo "prompt: $PROMPT  review: ${REVIEW_PROMPT:-none} every $REVIEW_EVERY commits"
  echo "head: $(git rev-parse --short HEAD 2>/dev/null)  commits since review: $(commits_since_review)"
  exit 0
fi

[ -f "$DONE_FILE" ] && { echo "[ralph] already done"; exit 0; }
[ -f "$STOP_FILE" ] && { echo "[ralph] halted; remove $STOP_FILE to resume"; exit 0; }

echo "[ralph] start $LABEL at $(git rev-parse --short HEAD 2>/dev/null) review_every=$REVIEW_EVERY max_stall=$MAX_STALL"
stall=0; iter=0
while [ "$iter" -lt "$MAX_ITER" ]; do
  iter=$((iter + 1))
  [ -f "$STOP_FILE" ] && { echo "[ralph] STOP after $iter iterations"; exit 0; }
  [ -f "$DONE_FILE" ] && { echo "[ralph] DONE after $iter iterations ($(git rev-parse --short HEAD))"; notify "DONE" "$LABEL complete"; exit 0; }
  if [ -f "$NEEDS_HUMAN" ]; then
    echo "[ralph] operator needed after $iter iterations:"; cat "$NEEDS_HUMAN"
    notify "NEEDS_HUMAN" "decision package in $NEEDS_HUMAN"; exit 2
  fi

  if [ "$REVIEW_EVERY" -gt 0 ] && [ -n "$REVIEW_PROMPT" ] && [ "$(commits_since_review)" -ge "$REVIEW_EVERY" ]; then
    echo "[ralph] review iteration $iter ($(commits_since_review) commits since last review)"
    MODEL_ARGS=""
    [ -n "$REVIEW_MODEL" ] && MODEL_ARGS="--model $REVIEW_MODEL"
    run_session "$REVIEW_PROMPT"
    iter=$((iter - 1))
    continue
  fi

  # A detached milestone run in flight: wait on its marker without spending a
  # session or counting a stall. The agent writes ralph/waiting with the marker
  # path when a unit's only remaining work is a long run.
  if [ -f ralph/waiting ]; then
    wait_marker=$(tr -d '[:space:]' < ralph/waiting)
    if [ -n "$wait_marker" ] && [ ! -f "$wait_marker" ]; then
      echo "[ralph] waiting on $wait_marker (no session this tick)"
      sleep 120; iter=$((iter - 1)); continue
    fi
    echo "[ralph] $wait_marker present — resuming"
    rm -f ralph/waiting
  fi

  before=$(git rev-parse HEAD 2>/dev/null || echo none)
  echo "[ralph] === iteration $iter $(date -u +%FT%TZ) from $(git rev-parse --short HEAD 2>/dev/null || echo none) ==="
  unit=$(current_unit)
  MODEL_ARGS=""
  if [ -n "$REVIEW_MODEL" ] && printf '%s' "$unit" | grep -qi 'review'; then
    MODEL_ARGS="--model $REVIEW_MODEL"
    echo "[ralph] unit $unit is a review — model $REVIEW_MODEL"
  fi
  run_session "$PROMPT"
  after=$(git rev-parse HEAD 2>/dev/null || echo none)
  if [ "$before" = "$after" ]; then
    stall=$((stall + 1)); echo "[ralph] no commit this iteration (stall $stall/$MAX_STALL)"
    notify "stall" "no commit ($stall/$MAX_STALL)"
    if [ "$stall" -ge "$MAX_STALL" ]; then
      echo "[ralph] HALT: $MAX_STALL iterations without a commit — inspect ralph/log.txt"
      notify "HALT" "$MAX_STALL iterations without a commit"
      : > "$STOP_FILE"; exit 3
    fi
  else
    stall=0; echo "[ralph] progress -> $(git rev-parse --short HEAD)"
  fi
done
echo "[ralph] HALT: MAX_ITER=$MAX_ITER reached"; notify "HALT" "MAX_ITER reached"; exit 4
