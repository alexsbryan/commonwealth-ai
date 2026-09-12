#!/bin/bash
# ralph-pool.sh — run a campaign's ready units in parallel lanes, each in its
# own git worktree, merging each finished lane back into the main tree.
#
# ralph-loop.sh is serial (one unit at a time). This keeps up to --lanes
# sessions in flight over the units whose dependencies are already done, so a
# ring's wide frontier is worked in parallel: ring 1 opened with five
# independent units; ring 2 has transform-rung beside the registry chain.
#
# It is WAVE-based, not a dynamic pool: each wave takes up to --lanes ready
# units, runs them concurrently, waits for all of them, then merges the ones
# that finished (serially, in the main tree). Simple and predictable.
#
# Safety, because parallel agents on one repo can corrupt each other:
#   * every lane is its own `git worktree` on its own branch — no two sessions
#     share a working tree;
#   * merges are serial and happen only in the main tree;
#   * a merge conflict ABORTS and HALTS (NEEDS_HUMAN) — never auto-resolved;
#   * a lane session runs in its own process group and is killed (group) past
#     --session-timeout, so a cut leaves no orphans;
#   * REVIEW units run serially in the main tree, never in a lane — a review
#     must see its units merged;
#   * lanes do not edit ralph/STATE.md; the pool marks a unit [x] after merging,
#     so the merge never fights over the queue file.
#
# A lane session works the unit and commits; when the unit passes its own tests
# it writes `ralph/done/<unit>` and commits it. The pool merges a lane whose
# done marker is present, marks the unit [x] in the main tree, and removes the
# worktree.
#
# Usage:
#   nohup ralph-pool.sh --workdir . --prompt ralph/PROMPT.md --lanes 2 \
#     [--state ralph/STATE.md] [--session-timeout 3600] [--review-model MODEL] \
#     [--notify] >> ralph/log.txt 2>&1 &
set -u

WORKDIR=""
PROMPT="ralph/PROMPT.md"
STATE="ralph/STATE.md"
LANES=2
SESSION_TIMEOUT=3600
REVIEW_MODEL=""
NOTIFY=0
STOP_FILE="ralph/STOP"
NEEDS_HUMAN="ralph/NEEDS_HUMAN.md"

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --prompt) PROMPT="$2"; shift 2 ;;
    --state) STATE="$2"; shift 2 ;;
    --lanes) LANES="$2"; shift 2 ;;
    --session-timeout) SESSION_TIMEOUT="$2"; shift 2 ;;
    --review-model) REVIEW_MODEL="$2"; shift 2 ;;
    --notify) NOTIFY=1; shift ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) echo "ralph-pool: unknown flag $1" >&2; exit 2 ;;
  esac
done
[ -n "$WORKDIR" ] || { echo "ralph-pool: --workdir is required" >&2; exit 2; }
cd "$WORKDIR" || exit 2
BASE_BRANCH=$(git rev-parse --abbrev-ref HEAD)
OPENCODE_BIN="${RALPH_OPENCODE_BIN:-opencode}"

notify() { [ "$NOTIFY" -eq 1 ] || return 0
  /usr/bin/osascript -e "display notification \"${2}\" with title \"ralph-pool: ${1}\"" >/dev/null 2>&1 || true; }
say() { echo "$(date -u +%FT%TZ) $*"; }
halt() { say "HALT: $1"; printf '%s\n\nresolve by hand, then remove %s\n' "$1" "$STOP_FILE" > "$NEEDS_HUMAN"; : > "$STOP_FILE"; notify "HALT" "$1"; exit 3; }

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

# One lane session, in its worktree, own process group, wall-clock timeout.
run_lane_session() { # unit dir logfile
  local unit="$1" dir="$2" log="$3" input
  input="${TMPDIR:-/tmp}/ralph-pool-$unit.md"
  {
    printf 'POOL LANE: you are working unit %s in an isolated git worktree.\n' "$unit"
    printf 'Commit your work here. When the unit passes its OWN tests, write ralph/done/%s and commit it — the pool merges your branch then.\n' "$unit"
    printf 'Do NOT edit ralph/STATE.md; the pool marks the unit done after the merge.\n\n'
    cat "$PROMPT"
  } > "$input"
  ( cd "$dir" && exec "$OPENCODE_BIN" run "$(cat "$input")" ) > "$log" 2>&1 &
  local pid=$! waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$SESSION_TIMEOUT" ]; do
    sleep 30; waited=$((waited + 30))
    [ $((waited % 60)) -eq 0 ] && say "  lane $unit running ${waited}s"
    [ -f "$STOP_FILE" ] && { kill -TERM -- -"$pid" 2>/dev/null; sleep 3; kill -KILL -- -"$pid" 2>/dev/null; break; }
  done
  if kill -0 "$pid" 2>/dev/null; then
    say "lane $unit exceeded ${SESSION_TIMEOUT}s — killing its group"
    kill -TERM -- -"$pid" 2>/dev/null; sleep 5; kill -KILL -- -"$pid" 2>/dev/null
  fi
  wait "$pid" 2>/dev/null || true
  rm -f "$input"
}

mkdir -p ralph/done .ralph/wt
say "start pool: lanes=$LANES base=$BASE_BRANCH"
while true; do
  [ -f "$STOP_FILE" ] && { say "STOP"; exit 0; }
  if all_done; then say "DONE: all units [x]"; notify "DONE" "pool complete"; exit 0; fi

  # A ready REVIEW runs serially in the main tree (it must see merged units).
  review=""
  for u in $(unit_ids); do
    if [ "$(status_of "$u")" = " " ] && is_review "$u" && deps_met "$u"; then review="$u"; break; fi
  done
  if [ -n "$review" ]; then
    say "serial review $review (main tree)"
    MODEL_ARGS=""; [ -n "$REVIEW_MODEL" ] && MODEL_ARGS="--model $REVIEW_MODEL"
    run_lane_session "$review" "$PWD" "ralph/log-$review.txt"
    [ "$(status_of "$review")" = x ] || halt "review $review did not mark [x]"
    continue
  fi

  # Wave: up to LANES ready, non-review units, concurrently.
  wave=""
  for u in $(unit_ids); do
    [ "$(echo "$wave" | wc -w)" -ge "$LANES" ] && break
    [ "$(status_of "$u")" = " " ] || continue
    is_review "$u" && continue
    deps_met "$u" || continue
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
    git worktree add -q -b "$branch" "$wt" "$BASE_BRANCH" 2>/dev/null || { say "worktree add failed for $u"; continue; }
    say "lane start $u (worktree $wt)"
    run_lane_session "$u" "$wt" "ralph/log-$u.txt" &
    pids="$pids $!"
  done
  for p in $pids; do wait "$p" 2>/dev/null || true; done

  # Merge the finished lanes serially; mark and clean up.
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
