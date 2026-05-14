#!/usr/bin/env bash
# Replay a captured ChatCompletionRequest against the local daemon.
#
# Usage:
#   harness-replay.sh <response_id>            # one shot, pretty-printed
#   harness-replay.sh <response_id> -n 10      # run 10 times, summarise
#   harness-replay.sh <response_id> --diff     # compare against captured output
#   harness-replay.sh <response_id> --raw      # print raw response body
#
# Reads `~/.sovereign/codex-sessions/raw/<response_id>.input.json`
# (the post-frontdoor ChatCompletionRequest captured by routes_responses)
# and POSTs it to the local daemon's /v1/chat/completions. Captured
# output for diff comparison: `<response_id>.txt`.
#
# Lets us iterate on inference-side fixes (grammar, stop tokens,
# compression) against a frozen prompt — no codex required.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <response_id> [-n N] [--diff] [--raw]" >&2
    echo "examples in: ~/.sovereign/codex-sessions/raw/*.input.json" >&2
    exit 1
fi

RESP_ID="$1"
shift
N=1
DO_DIFF=0
DO_RAW=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        -n) N="$2"; shift 2 ;;
        --diff) DO_DIFF=1; shift ;;
        --raw) DO_RAW=1; shift ;;
        *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
done

RAW_DIR="$HOME/.sovereign/codex-sessions/raw"
INPUT="$RAW_DIR/${RESP_ID}.input.json"
EXPECTED="$RAW_DIR/${RESP_ID}.txt"
DAEMON="${SOVEREIGN_DAEMON:-http://localhost:9741}"

if [[ ! -f "$INPUT" ]]; then
    echo "input fixture not found: $INPUT" >&2
    exit 1
fi

# Force stream=false for deterministic single-shot reads; the
# fixture's `stream` field doesn't matter for replay.
PAYLOAD="$(jq '.stream = false' "$INPUT")"

run_once() {
    curl -sS \
        -X POST "$DAEMON/v1/chat/completions" \
        -H "content-type: application/json" \
        --max-time 600 \
        -d "$PAYLOAD"
}

if [[ $N -eq 1 ]]; then
    RESP="$(run_once)"
    if [[ $DO_RAW -eq 1 ]]; then
        echo "$RESP"
        exit 0
    fi
    # Pretty: choice 0 content + tool_calls
    echo "── replay $RESP_ID ──"
    echo "input bytes:  $(wc -c < "$INPUT")"
    echo "model:        $(echo "$PAYLOAD" | jq -r '.model // "?"')"
    echo "tool_choice:  $(echo "$PAYLOAD" | jq -r '.tool_choice // "?"')"
    echo "tools[]:      $(echo "$PAYLOAD" | jq -r '.tools | length // 0')"
    echo
    echo "── response ──"
    echo "$RESP" | jq '{
        finish_reason: .choices[0].finish_reason,
        content_bytes: (.choices[0].message.content | length),
        content_preview: (.choices[0].message.content[:300] // ""),
        tool_calls: (.choices[0].message.tool_calls // [] | map({
            name: .function.name,
            args_bytes: (.function.arguments | length),
            args_sample: (.function.arguments[:200])
        })),
        usage: .usage
    }'
    if [[ $DO_DIFF -eq 1 && -f "$EXPECTED" ]]; then
        echo
        echo "── expected (raw_emission) ──"
        wc -c "$EXPECTED"
        head -c 600 "$EXPECTED"
    fi
    exit 0
fi

# Multi-run: summarise variance.
echo "── replay $RESP_ID × $N ──"
TMPDIR_R="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_R"' EXIT
for i in $(seq 1 "$N"); do
    run_once > "$TMPDIR_R/$i.json"
done
jq -s '
    map({
        finish_reason: (.choices[0].finish_reason // "none"),
        content_bytes: ((.choices[0].message.content // "") | length),
        tool_call_count: ((.choices[0].message.tool_calls // []) | length),
        first_tool: ((.choices[0].message.tool_calls // [{}])[0].function.name // "none"),
        first_args_bytes: (((.choices[0].message.tool_calls // [{}])[0].function.arguments // "") | length),
        first_args_parseable: (
            ((.choices[0].message.tool_calls // [{}])[0].function.arguments // "{}")
            | (try (fromjson | true) catch false)
        ),
        tokens: (.usage.total_tokens // 0)
    })
    | {
        runs: length,
        finish_reasons: (map(.finish_reason) | group_by(.) | map({k:.[0], n:length}) | map({key:.k, value:.n}) | from_entries),
        first_tool_names: (map(.first_tool) | group_by(.) | map({k:.[0], n:length}) | map({key:.k, value:.n}) | from_entries),
        tool_call_count_distribution: (map(.tool_call_count) | group_by(.) | map({k:(.[0]|tostring), n:length}) | map({key:.k, value:.n}) | from_entries),
        args_parse_ok_rate: (map(select(.first_args_parseable)) | length),
        tokens_stats: {
            min: (map(.tokens) | min),
            max: (map(.tokens) | max),
            mean: ((map(.tokens) | add) / length | floor)
        },
        content_bytes_stats: {
            min: (map(.content_bytes) | min),
            max: (map(.content_bytes) | max)
        },
        first_args_bytes_stats: {
            min: (map(.first_args_bytes) | min),
            max: (map(.first_args_bytes) | max)
        }
    }
' "$TMPDIR_R"/*.json
