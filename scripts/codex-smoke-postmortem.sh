#!/usr/bin/env bash
# Post-mortem summary for a Codex /v1/responses smoke run.
#
# Reads ~/.svrnmesh/codex-sessions/sessions.jsonl and emits:
#   - turn count
#   - finish_reason distribution
#   - tool_calls per turn (exec_command vs other)
#   - heredoc-write turns: count, body_bytes histogram, escape-coherence smells
#   - args_parsed_ok=false count (JSON-args failures, should be ~0 post-v15)
#
# Usage:
#   scripts/codex-smoke-postmortem.sh              # full log
#   scripts/codex-smoke-postmortem.sh --since 600  # last 10 min
#
# Requires `jq`. Reads only — no side effects.

set -euo pipefail

LOG="${SOVEREIGN_CODEX_LOG:-$HOME/.svrnmesh/codex-sessions/sessions.jsonl}"
SINCE_SECS=""
if [[ "${1:-}" == "--since" && -n "${2:-}" ]]; then
    SINCE_SECS="$2"
fi

if [[ ! -f "$LOG" ]]; then
    echo "no session log at $LOG" >&2
    exit 1
fi

NOW=$(date +%s)
filter='.'
if [[ -n "$SINCE_SECS" ]]; then
    THRESHOLD=$((NOW - SINCE_SECS))
    filter="select(.ts_unix >= $THRESHOLD)"
fi

echo "── session telemetry summary ──"
echo "log: $LOG"
[[ -n "$SINCE_SECS" ]] && echo "window: last ${SINCE_SECS}s"
echo

# Total turn count (one terminal record per turn).
TURNS=$(jq -c "$filter | select(.kind==\"terminal\")" "$LOG" | wc -l | tr -d ' ')
echo "turns: $TURNS"

echo
echo "finish_reason distribution:"
jq -r "$filter | select(.kind==\"terminal\") | .finish_reason" "$LOG" \
    | sort | uniq -c | sort -rn

echo
echo "function_call names (top 10):"
jq -r "$filter | select(.kind==\"terminal\") | .function_calls[]?.name" "$LOG" \
    | sort | uniq -c | sort -rn | head -10

echo
echo "args_parsed_ok=false count (JSON-args failures, should be ~0):"
jq -r "$filter | select(.kind==\"terminal\") | .function_calls[]? | select(.args_parsed_ok==false) | .name" "$LOG" \
    | wc -l | tr -d ' '

echo
echo "── heredoc-write turns ──"
HEREDOC_TURNS=$(jq -c "$filter | select(.kind==\"terminal\") | .function_calls[]? | select(.heredoc != null)" "$LOG")
HEREDOC_COUNT=$(echo "$HEREDOC_TURNS" | grep -c . || true)
echo "heredoc calls: $HEREDOC_COUNT"

if [[ "$HEREDOC_COUNT" -gt 0 ]]; then
    echo
    echo "body_bytes stats:"
    echo "$HEREDOC_TURNS" | jq -s '
        map(.heredoc.body_bytes) |
        {min: min, max: max, mean: (add / length | floor)}
    '

    echo
    echo "closed vs unterminated:"
    echo "$HEREDOC_TURNS" | jq -r '.heredoc.closed' | sort | uniq -c

    echo
    echo "escape-coherence smell counts (sum across all heredoc bodies):"
    echo "$HEREDOC_TURNS" | jq -s '
        {
            escape_quote_total: (map(.heredoc.escape_quote_count) | add),
            escape_backslash_total: (map(.heredoc.escape_backslash_count) | add),
            heredocs_with_quote_smell: (map(select(.heredoc.escape_quote_count > 0)) | length),
            heredocs_with_backslash_smell: (map(select(.heredoc.escape_backslash_count > 0)) | length)
        }
    '

    echo
    echo "apply_patch operation counts (across heredoc bodies):"
    echo "$HEREDOC_TURNS" | jq -s '
        {
            add_files: (map(.heredoc.add_files) | add),
            update_files: (map(.heredoc.update_files) | add),
            delete_files: (map(.heredoc.delete_files) | add)
        }
    '
fi
