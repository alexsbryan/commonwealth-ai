#!/bin/sh
# sovereign validate-plan — PreToolUse hook for ExitPlanMode.
#
# Runs `sovereign plan validate` on the most-recently-modified plan
# file under ~/.claude/plans/. If sections are missing, exits 2 to
# block the ExitPlanMode tool call with stderr feedback. The model
# sees the missing-section list as feedback and can fix the plan
# before retrying.
#
# Drains stdin (Claude Code's PreToolUse payload — we don't need
# any of its fields for v0; the most-recent-plan heuristic is
# sufficient since the plan we're about to exit IS the most recent
# write).
#
# Opt-out: SOVEREIGN_NO_PLAN_VALIDATE=1 — skips validation,
# returns 0. Use sparingly; pair with a memory note explaining why.

# Drain payload — we don't need it but PreToolUse expects the hook
# to consume stdin promptly.
cat >/dev/null 2>&1

[ "${SOVEREIGN_NO_PLAN_VALIDATE:-0}" = "1" ] && exit 0

SOVEREIGN_BIN="${SOVEREIGN_BIN:-$HOME/.local/bin/sovereign}"
[ -x "$SOVEREIGN_BIN" ] || exit 0

# Most-recent plan under ~/.claude/plans/, excluding the template.
PLAN=$(ls -t "$HOME/.claude/plans"/*.md 2>/dev/null \
    | grep -v "_TEMPLATE.md" \
    | head -1)
[ -z "$PLAN" ] && exit 0
[ -f "$PLAN" ] || exit 0

# Run validator. stderr passes through to Claude Code, which
# surfaces it back to the model as feedback when we exit 2.
if ! "$SOVEREIGN_BIN" plan validate "$PLAN" >&2; then
    echo "" >&2
    echo "Plan at $PLAN is missing required alignment sections." >&2
    echo "Add them, then call ExitPlanMode again." >&2
    exit 2
fi

exit 0
