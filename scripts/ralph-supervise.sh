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
RESOLVE_MODEL=""
RESOLVE_VARIANT=""
STATE="ralph/STATE.md"
INNER=()

while [ $# -gt 0 ]; do
  case "$1" in
    --workdir) WORKDIR="$2"; shift 2 ;;
    --resolve-model) RESOLVE_MODEL="$2"; shift 2 ;;
    --resolve-variant) RESOLVE_VARIANT="$2"; shift 2 ;;
    --no-notify) NOTIFY=0; shift ;;
    --) shift; INNER=("$@"); break ;;
    *) echo "ralph-supervise: unexpected flag $1 (put the campaign after --)" >&2; exit 2 ;;
  esac
done
[ -n "$WORKDIR" ] || { echo "ralph-supervise: --workdir is required" >&2; exit 2; }
[ ${#INNER[@]} -gt 0 ] || { echo "ralph-supervise: the campaign command is required after --" >&2; exit 2; }
cd "$WORKDIR" || exit 2
mkdir -p ralph target/ralph

# Per-host model configuration; the inner campaign's flags still win.
MODEL=""; REVIEW_MODEL=""; VARIANT=""
load_models "${RALPH_MODELS_FILE:-ralph/models.env}"

# Inherit the campaign's review settings; model swaps have one configuration.
for ((i=0; i<${#INNER[@]}-1; i++)); do
  case "${INNER[$i]}" in
    --review-model) [ -n "$RESOLVE_MODEL" ] || RESOLVE_MODEL="${INNER[$((i+1))]}" ;;
    --variant) [ -n "$RESOLVE_VARIANT" ] || RESOLVE_VARIANT="${INNER[$((i+1))]}" ;;
    --state) STATE="${INNER[$((i+1))]}" ;;
    --stop-file) STOP_FILE="${INNER[$((i+1))]}" ;;
    --done-file) DONE_FILE="${INNER[$((i+1))]}" ;;
    --needs-human-file) NEEDS_HUMAN="${INNER[$((i+1))]}" ;;
  esac
done
[ -n "$RESOLVE_MODEL" ] || RESOLVE_MODEL="$REVIEW_MODEL"
[ -n "$RESOLVE_VARIANT" ] || RESOLVE_VARIANT="$VARIANT"
[ -n "$RESOLVE_MODEL" ] && MODEL_ARGS="--model $RESOLVE_MODEL"
[ -n "$RESOLVE_VARIANT" ] && MODEL_ARGS="$MODEL_ARGS --variant $RESOLVE_VARIANT"

terminal_stop() {
  if [ -f "$DONE_FILE" ]; then
    say "supervisor: campaign DONE"; notify "DONE" "campaign complete"; exit 0
  fi
  if [ -f "$STOP_FILE" ] && [ ! -s "$NEEDS_HUMAN" ]; then
    say "supervisor: operator STOP — leaving it stopped"
    notify "STOP" "operator stop preserved"; exit 0
  fi
  local unit
  unit=$(current_unit)
  case "$unit" in
    HUMAN-*)
      say "supervisor: operator approval required — $unit (no resolution session)"
      notify "NEEDS_HUMAN" "approval required: $unit"; exit 2 ;;
  esac
}

say "supervisor start: ${INNER[*]}"
say "supervisor resolution: model ${RESOLVE_MODEL:-configured default}, variant ${RESOLVE_VARIANT:-configured default}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
attempt=0
resolution=0
while true; do
  terminal_stop

  before=$(git rev-parse HEAD 2>/dev/null || true)
  "${INNER[@]}"
  rc=$?
  after=$(git rev-parse HEAD 2>/dev/null || true)
  [ "$before" = "$after" ] || attempt=0

  terminal_stop

  reason="campaign exited $rc"
  [ -f "$NEEDS_HUMAN" ] && reason=$(head -1 "$NEEDS_HUMAN")
  say "supervisor: campaign stopped — $reason"

  attempt=$((attempt + 1))
  if [ "$attempt" -gt "$RESOLVE_MAX" ]; then
    say "supervisor: $RESOLVE_MAX resolution attempts did not clear it — leaving it to the operator"
    notify "supervisor" "unresolved after $RESOLVE_MAX: $reason"
    exit 2
  fi

  resolution=$((resolution + 1))
  prompt="${TMPDIR:-/tmp}/ralph-supervise-$RUN_ID-$resolution.md"
  cat > "$prompt" <<EOF
SUPERVISOR RESOLUTION (attempt $attempt of $RESOLVE_MAX).

The campaign stopped short of DONE. Reason:
  $reason

You are the resolution session. Diagnose and fix so the campaign flows again:
1. Read \`$NEEDS_HUMAN\`, \`$STATE\`, and \`git status\`. The campaign logs are
   under \`~/.svrnmesh/ralph/\` (launchd.log and logs/*.out); check and resolution
   logs are under \`target/ralph/\`.
2. The common stops are: a unit or review that returned without marking \`[x]\`
   (finish it — or, if it handed off to a detached run, wait for the marker and
   then finish it); a merge conflict (resolve it in the main tree); a lane or
   session that timed out (resume it); a stale dependency in STATE.md (correct
   the state). Fix the blocker.
3. Do NOT weaken a PASS BAR, and do not mark a unit \`[x]\` that has not earned
   it. Never approve or mark a HUMAN- row. A false premise may be corrected
   only from verified code/consumer evidence, with the row and its source order
   corrected together. If this is a genuine design fork, leave a clear
   \`$NEEDS_HUMAN\` for the operator and stop.
4. When the blocker is fixed: remove \`$NEEDS_HUMAN\` so the
   campaign resumes, and commit your work.
   The supervisor cleared the old blocker STOP before starting you; a NEW
   \`$STOP_FILE\` is an operator request and you must not remove it.
EOF
  # A blocker STOP would otherwise kill its own resolver in run_session.
  [ -s "$NEEDS_HUMAN" ] && rm -f "$STOP_FILE"
  say "supervisor: dispatching resolution session $attempt — $reason"
  notify "resolving" "attempt $attempt: $reason"
  before=$(git rev-parse HEAD 2>/dev/null || true)
  run_session "$prompt" "$PWD" "target/ralph/supervise-$RUN_ID-$resolution.out"
  rm -f "$prompt"
  if [ -f "$STOP_FILE" ]; then
    say "supervisor: operator STOP during resolution — leaving it stopped"
    notify "STOP" "resolution interrupted; operator stop preserved"; exit 0
  fi

  if [ -f "$NEEDS_HUMAN" ] && [ -s "$NEEDS_HUMAN" ]; then
    say "supervisor: resolution $attempt left NEEDS_HUMAN — retrying"
  else
    say "supervisor: resolution $attempt cleared the halt — resuming the campaign"
    after=$(git rev-parse HEAD 2>/dev/null || true)
    [ "$before" = "$after" ] || attempt=0
  fi
done
