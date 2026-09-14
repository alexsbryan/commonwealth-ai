#!/bin/bash
# ralph-supervise.sh — a supervisor for a ralph campaign. It runs the campaign
# (ralph-pool.sh or ralph-loop.sh) and, whenever that stops short of DONE —
# a halt, a crash — it dispatches a RESOLUTION session: an agent that reads the
# halt, the state and the log, fixes the blocker, and clears the halt so the
# campaign resumes. Then it restarts the campaign. The operator stops having to
# watch the state machine.
#
# It is not an oracle: if a resolution session cannot fix the blocker it leaves
# NEEDS_HUMAN, the supervisor retries a bounded number of times, then notifies
# the operator and stops. A PASS BAR is never weakened by the supervisor — that
# stays a human decision.
#
# Usage:
#   nohup ralph-supervise.sh --workdir DIR -- <campaign command and args> \
#     >> ralph/log.txt 2>&1 &
# e.g.
#   nohup ralph-supervise.sh --workdir . -- \
#     bash scripts/ralph-pool.sh --workdir . --prompt ralph/PROMPT.md \
#       --lanes 2 --review-model MODEL --notify >> ralph/log.txt 2>&1 &
set -u
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
LIB="$(cd "$(dirname "$0")" && pwd)/ralph-lib.sh"
# shellcheck source=ralph-lib.sh
. "$LIB"

WORKDIR=""
RESOLVE_MAX="${RALPH_RESOLVE_MAX:-4}"
SESSION_TIMEOUT="${RALPH_RESOLVE_TIMEOUT:-1800}"
NOTIFY=1
OPENCODE_BIN="${RALPH_OPENCODE_BIN:-opencode}"
STOP_FILE="ralph/STOP"
DONE_FILE="ralph/DONE"
NEEDS_HUMAN="ralph/NEEDS_HUMAN.md"
MODEL_ARGS=""
INNER=()

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --resolve-model) MODEL_ARGS="--model $2"; shift 2 ;;
    --no-notify) NOTIFY=0; shift ;;
    --) shift; INNER=("$@"); break ;;
    *) echo "ralph-supervise: unexpected flag $1 (put the campaign after --)" >&2; exit 2 ;;
  esac
done
[ -n "$WORKDIR" ] || { echo "ralph-supervise: --workdir is required" >&2; exit 2; }
[ ${#INNER[@]} -gt 0 ] || { echo "ralph-supervise: the campaign command is required after --" >&2; exit 2; }
cd "$WORKDIR" || exit 2
mkdir -p ralph

say "supervisor start: ${INNER[*]}"
attempt=0
while true; do
  [ -f "$DONE_FILE" ] && { say "supervisor: campaign DONE"; notify "DONE" "campaign complete"; exit 0; }

  "${INNER[@]}"
  rc=$?

  [ -f "$DONE_FILE" ] && { say "supervisor: campaign DONE"; notify "DONE" "campaign complete"; exit 0; }

  reason="campaign exited $rc"
  [ -f "$NEEDS_HUMAN" ] && reason=$(head -1 "$NEEDS_HUMAN")
  say "supervisor: campaign stopped — $reason"

  attempt=$((attempt + 1))
  if [ "$attempt" -gt "$RESOLVE_MAX" ]; then
    say "supervisor: $RESOLVE_MAX resolution attempts did not clear it — leaving it to the operator"
    notify "supervisor" "unresolved after $RESOLVE_MAX: $reason"
    exit 2
  fi

  prompt="${TMPDIR:-/tmp}/ralph-supervise-$attempt.md"
  cat > "$prompt" <<EOF
SUPERVISOR RESOLUTION (attempt $attempt of $RESOLVE_MAX).

The campaign stopped short of DONE. Reason:
  $reason

You are the resolution session. Diagnose and fix so the campaign flows again:
1. Read \`ralph/NEEDS_HUMAN.md\`, the tail of the campaign log (the file this is
   redirected to, or \`ralph/log*.txt\`), \`ralph/STATE.md\`, and \`git status\`.
2. The common stops are: a unit or review that returned without marking \`[x]\`
   (finish it — or, if it handed off to a detached run, wait for the marker and
   then finish it); a merge conflict (resolve it in the main tree); a lane or
   session that timed out (resume it); a stale dependency in STATE.md (correct
   the state). Fix the blocker.
3. Do NOT weaken a PASS BAR, and do not mark a unit \`[x]\` that has not earned
   it. If this is a genuine fork or a real blocker you cannot resolve, write a
   clear \`ralph/NEEDS_HUMAN.md\` for the operator and stop.
4. When the blocker is fixed: \`rm -f ralph/STOP ralph/NEEDS_HUMAN.md\` so the
   campaign resumes, and commit your work.
EOF
  say "supervisor: dispatching resolution session $attempt"
  run_session "$prompt" "$PWD" "ralph/log-supervise-$attempt.txt"
  rm -f "$prompt"

  if [ -f "$NEEDS_HUMAN" ] && [ -s "$NEEDS_HUMAN" ]; then
    say "supervisor: resolution $attempt left NEEDS_HUMAN — retrying"
  else
    say "supervisor: resolution $attempt cleared the halt — resuming the campaign"
    attempt=0
  fi
  rm -f "$STOP_FILE"
done
