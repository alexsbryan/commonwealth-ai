#!/bin/bash
# ralph-pool.sh — the PARALLEL campaign loop. Runs a ring's ready units in up
# to --lanes concurrent sessions, each in its own git worktree, merging each
# finished lane serially into the main tree. The session/contract layer lives in
# ralph-lib.sh, shared with ralph-loop.sh — the pool no longer re-implements a
# subset (it kept missing the process-group kill and the wait-for marker).
#
# Wave-based: each wave takes up to --lanes ready units, runs them, waits for
# all, then merges the finished ones serially.
#
# Safety: lanes never share a working tree; merges are serial; a conflict aborts
# and HALTS (never auto-resolved); REVIEW units run serially in the main tree;
# lanes do not edit STATE.md (the pool marks a unit [x] after merging).
#
# Usage:
#   nohup ralph-pool.sh --workdir . --prompt ralph/PROMPT.md --lanes 2 \
#     --review-model MODEL [--state ralph/STATE.md] [--session-timeout 3600] \
#     [--notify] >> ralph/log.txt 2>&1 &
set -u
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
LIB="$(cd "$(dirname "$0")" && pwd)/ralph-lib.sh"
# shellcheck source=ralph-lib.sh
. "$LIB"

WORKDIR=""
PROMPT="ralph/PROMPT.md"
STATE="ralph/STATE.md"
LANES=2
SESSION_TIMEOUT=3600
REVIEW_MODEL=""
NOTIFY=0
STOP_FILE="ralph/STOP"
DONE_FILE="ralph/DONE"
NEEDS_HUMAN="ralph/NEEDS_HUMAN.md"
OPENCODE_BIN="${RALPH_OPENCODE_BIN:-opencode}"
MODEL_ARGS=""

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --prompt) PROMPT="$2"; shift 2 ;;
    --state) STATE="$2"; shift 2 ;;
    --lanes) LANES="$2"; shift 2 ;;
    --session-timeout) SESSION_TIMEOUT="$2"; shift 2 ;;
    --review-model) REVIEW_MODEL="$2"; shift 2 ;;
    --notify) NOTIFY=1; shift ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "ralph-pool: unknown flag $1" >&2; exit 2 ;;
  esac
done
[ -n "$WORKDIR" ] || { echo "ralph-pool: --workdir is required" >&2; exit 2; }
cd "$WORKDIR" || exit 2
BASE_BRANCH=$(git rev-parse --abbrev-ref HEAD)

status_of() {
  local line
  line=$(grep -E "^- \[[x~ ]\] $1([[:space:]]|$)" "$STATE" | head -1)
  case "$line" in
    "- [x] "*) printf 'x' ;;
    "- [~] "*) printf '~' ;;
    "- [ ] "*) printf ' ' ;;
    *) printf '' ;;
  esac
}
deps_of() {
  grep -E "^- \[[x~ ]\] $1([[:space:]]|$)" "$STATE" | head -1 \
    | sed -n 's/.*depends \[\([^]]*\)\].*/\1/p' | tr ',' ' '
}
unit_ids() { grep -oE '^- \[[x~ ]\] [A-Za-z0-9-]+' "$STATE" | awk '{print $NF}'; }
is_review() { case "$1" in REVIEW-*) return 0 ;; *) return 1 ;; esac; }
deps_met() { local d; for d in $(deps_of "$1"); do [ "$(status_of "$d")" = x ] || return 1; done; return 0; }
all_done() { local u; for u in $(unit_ids); do [ "$(status_of "$u")" = x ] || return 1; done; return 0; }

# A lane session: a POOL LANE note + the prompt, run by the shared layer.
run_lane_session() { # unit dir log
  local note="${TMPDIR:-/tmp}/ralph-pool-$1.md"
  {
    printf 'POOL LANE: you are working unit %s in an isolated git worktree.\n' "$1"
    printf 'Commit your work here. When the unit passes its OWN tests, write ralph/done/%s and commit it — the pool merges your branch then.\n' "$1"
    printf 'Do NOT edit ralph/STATE.md; the pool marks the unit done after the merge.\n\n'
    cat "$PROMPT"
  } > "$note"
  run_session "$note" "$2" "$3"
  rm -f "$note"
}

mkdir -p ralph/done .ralph/wt
say "start pool: lanes=$LANES base=$BASE_BRANCH"
while true; do
  [ -f "$STOP_FILE" ] && { say "STOP"; exit 0; }
  if all_done; then say "DONE: all units [x]"; notify "DONE" "pool complete"; exit 0; fi

  # A detached milestone run in flight: wait on its marker, no session, no failure.
  if ! wait_for_marker; then sleep 120; continue; fi

  # A ready REVIEW runs serially in the main tree, retrying if a session times out.
  review=""
  for u in $(unit_ids); do
    if [ "$(status_of "$u")" = " " ] && is_review "$u" && deps_met "$u"; then review="$u"; break; fi
  done
  if [ -n "$review" ]; then
    review_attempt=1; review_waiting=0
    while [ "$review_attempt" -le 3 ]; do
      say "serial review $review (main tree) attempt $review_attempt"
      MODEL_ARGS=""; [ -n "$REVIEW_MODEL" ] && MODEL_ARGS="--model $REVIEW_MODEL"
      run_session "$PROMPT" "$PWD" "ralph/log-$review.txt"
      [ "$(status_of "$review")" = x ] && break
      # A review that handed off to a detached run returns quickly and writes
      # ralph/waiting; wait on its marker instead of counting a failed attempt.
      if ! wait_for_marker; then review_waiting=1; break; fi
      say "review $review did not mark [x] (attempt $review_attempt) — resuming"
      review_attempt=$((review_attempt + 1))
    done
    [ "$review_waiting" -eq 1 ] && continue
    [ "$(status_of "$review")" = x ] || halt "review $review did not finish after 3 attempts"
    continue
  fi

  # Wave: up to LANES ready, non-review, non-conflicting units, concurrently.
  wave=""
  for u in $(unit_ids); do
    [ "$(echo "$wave" | wc -w)" -ge "$LANES" ] && break
    [ "$(status_of "$u")" = " " ] || continue
    is_review "$u" && continue
    deps_met "$u" || continue
    conflict=0
    for v in $wave; do
      [ -f ralph/conflicts.txt ] || break
      grep -qE "^($u $v|$v $u)$" ralph/conflicts.txt && conflict=1
    done
    [ "$conflict" -eq 1 ] && continue
    wave="$wave $u"
  done
  wave=$(echo "$wave")
  if [ -z "$wave" ]; then
    say "no ready unit and no ready review — waiting (dependencies unmet?)"
    sleep 60; continue
  fi

  pids=""
  for u in $wave; do
    wt=".ralph/wt/$u"; branch="ralph/$u"
    if [ -d "$wt" ]; then
      say "lane $u resuming in its existing worktree"
    else
      git worktree add -q -b "$branch" "$wt" "$BASE_BRANCH" 2>/dev/null || { say "worktree add failed for $u"; continue; }
      say "lane start $u (worktree $wt)"
    fi
    run_lane_session "$u" "$wt" "ralph/log-$u.txt" &
    pids="$pids $!"
  done
  for p in $pids; do wait "$p" 2>/dev/null || true; done

  for u in $wave; do
    wt=".ralph/wt/$u"; branch="ralph/$u"
    [ -d "$wt" ] || continue
    if [ -f "$wt/ralph/done/$u" ]; then
      say "lane $u finished — merging $branch"
      if ! git merge --no-ff -m "merge $u" "$branch" >/dev/null 2>&1; then
        git merge --abort 2>/dev/null || true
        halt "merge conflict merging $branch — resolve in the main tree, then resume"
      fi
      sed -i '' "s/^- \[[~ ]\] $u\$/- [x] $u/" "$STATE"
      sed -i '' "s/^- \[[~ ]\] $u\([[:space:]]\)/- [x] $u\1/" "$STATE"
      git add "$STATE" && git commit -q -m "$u: merged (pool)" >/dev/null 2>&1 || true
      git worktree remove --force "$wt" 2>/dev/null || true
      git branch -D "$branch" >/dev/null 2>&1 || true
      say "lane $u merged and marked [x]"
    else
      say "lane $u ended without ralph/done/$u — its branch $branch is kept for a retry"
    fi
  done
done
