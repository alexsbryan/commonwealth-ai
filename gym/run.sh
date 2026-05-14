#!/usr/bin/env bash
# Gym runner: replay each fixture × N times against the local daemon,
# score against per-fixture pass.yaml predicates, report pass rates.
#
# Daemon must be reachable at $SOVEREIGN_DAEMON (default localhost:9741).
# Daemon's harness for Codex-style fixtures should match production:
# `SOVEREIGN_HARNESS=codex sovereign daemon restart` before running.
#
# Usage:
#   ./run.sh                     # all fixtures, 10 replays each
#   ./run.sh -n 3                # 3 replays each (fast smoke)
#   ./run.sh -f 001_write_stage  # single fixture
#   ./run.sh --json              # machine-readable output

set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
FIXTURES_DIR="$DIR/fixtures"
DAEMON="${SOVEREIGN_DAEMON:-http://localhost:9741}"
N=10
FIXTURE_FILTER=""
JSON_OUT=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        -n) N="$2"; shift 2 ;;
        -f) FIXTURE_FILTER="$2"; shift 2 ;;
        --json) JSON_OUT=1; shift ;;
        -h|--help)
            cat <<EOF
Usage: $0 [-n REPLAYS] [-f FIXTURE_SLUG] [--json]

  -n N         replays per fixture (default 10)
  -f SLUG      run only fixtures whose directory name contains SLUG
  --json       emit one JSON object per fixture instead of table
EOF
            exit 0 ;;
        *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
done

# Quick sanity: is the daemon up?
if ! curl -sf "$DAEMON/v1/models" >/dev/null 2>&1; then
    echo "daemon not reachable at $DAEMON" >&2
    exit 1
fi

# Per-fixture scorer. Reads the response body from stdin (one chat
# completion result) and pass.yaml from $1. Echoes "pass" or "fail<reason>".
score_response() {
    local pass_yaml="$1"
    # The response is on stdin.
    local resp
    resp="$(cat)"

    # Extract fields once.
    local args
    args="$(echo "$resp" | jq -r '(.choices[0].message.tool_calls // [{}])[0].function.arguments // ""')"
    local tool
    tool="$(echo "$resp" | jq -r '(.choices[0].message.tool_calls // [{}])[0].function.name // ""')"
    local args_parseable
    if echo "$args" | jq empty >/dev/null 2>&1; then
        args_parseable=true
    else
        args_parseable=false
    fi
    local cmd
    cmd="$(echo "$args" | jq -r '.cmd // ""' 2>/dev/null || echo "")"

    # Parse pass.yaml. Keep this simple — pass.yaml is a tiny subset of YAML
    # we hand-author. yq isn't a hard dep; we use line-grep.
    # Format:
    #   expected_tool: <name>
    #   args_parseable: true
    #   must_contain:
    #     - "<substr>"
    #   must_not_contain:
    #     - "<substr>"
    #   content_must_contain_regex:
    #     - "<regex>"

    local expected_tool must_parse
    expected_tool="$(awk -F': ' '/^expected_tool:/ {print $2; exit}' "$pass_yaml" | tr -d '"')"
    must_parse="$(awk -F': ' '/^args_parseable:/ {print $2; exit}' "$pass_yaml" | tr -d ' ')"

    if [[ -n "$expected_tool" && "$expected_tool" != "any" && "$tool" != "$expected_tool" ]]; then
        echo "fail|wrong_tool:$tool"
        return
    fi
    if [[ "$must_parse" == "true" && "$args_parseable" != "true" ]]; then
        echo "fail|args_unparseable"
        return
    fi

    # Walk must_contain
    local in_section=""
    while IFS= read -r line; do
        if [[ "$line" =~ ^must_contain: ]]; then in_section="must"; continue; fi
        if [[ "$line" =~ ^must_not_contain: ]]; then in_section="not"; continue; fi
        if [[ "$line" =~ ^content_must_contain_regex: ]]; then in_section="re"; continue; fi
        if [[ "$line" =~ ^[a-z_]+: ]]; then in_section=""; continue; fi
        if [[ -z "$line" || "$line" =~ ^[[:space:]]*# ]]; then continue; fi
        # Item line: `  - "<value>"`
        if [[ "$line" =~ ^[[:space:]]*-[[:space:]]+\"(.*)\"[[:space:]]*$ ]]; then
            local val="${BASH_REMATCH[1]}"
            # Unescape \" \\ inside YAML string
            val="${val//\\\"/\"}"
            val="${val//\\\\/\\}"
            case "$in_section" in
                must)
                    if [[ "$cmd" != *"$val"* ]]; then
                        echo "fail|missing:$val"
                        return
                    fi
                    ;;
                not)
                    if [[ "$cmd" == *"$val"* ]]; then
                        echo "fail|forbidden:$val"
                        return
                    fi
                    ;;
                re)
                    if ! echo "$cmd" | grep -qE "$val"; then
                        echo "fail|regex_miss:$val"
                        return
                    fi
                    ;;
            esac
        fi
    done < "$pass_yaml"

    echo "pass|"
}

# Iterate fixtures.
results=()
total_pass=0
total_run=0

for fdir in "$FIXTURES_DIR"/*/; do
    slug="$(basename "$fdir")"
    if [[ -n "$FIXTURE_FILTER" && "$slug" != *"$FIXTURE_FILTER"* ]]; then continue; fi
    input="$fdir/input.json"
    pass="$fdir/pass.yaml"
    if [[ ! -f "$input" || ! -f "$pass" ]]; then
        echo "skip $slug — missing input.json or pass.yaml" >&2
        continue
    fi

    pass_count=0
    fail_reasons=()
    payload="$(jq '.stream = false' "$input")"
    for i in $(seq 1 "$N"); do
        resp="$(curl -sS -X POST "$DAEMON/v1/chat/completions" \
            -H "content-type: application/json" \
            --max-time 180 \
            -d "$payload" 2>/dev/null || echo '{}')"
        verdict="$(echo "$resp" | score_response "$pass")"
        status="${verdict%%|*}"
        reason="${verdict#*|}"
        if [[ "$status" == "pass" ]]; then
            pass_count=$((pass_count + 1))
        else
            fail_reasons+=("$reason")
        fi
    done

    rate=$((pass_count * 100 / N))
    total_pass=$((total_pass + pass_count))
    total_run=$((total_run + N))
    results+=("$slug|$pass_count/$N|${rate}%|${fail_reasons[*]:-}")
done

# Output.
if [[ $JSON_OUT -eq 1 ]]; then
    echo "{"
    echo "  \"fixtures\": ["
    first=1
    for r in "${results[@]}"; do
        IFS='|' read -r slug pf rate reasons <<< "$r"
        [[ $first -eq 0 ]] && echo ","
        first=0
        printf '    {"fixture":"%s","pass":"%s","rate":"%s","reasons":"%s"}' "$slug" "$pf" "$rate" "${reasons//\"/\\\"}"
    done
    echo
    echo "  ],"
    echo "  \"total_pass\": $total_pass,"
    echo "  \"total_run\": $total_run,"
    echo "  \"total_rate\": \"$((total_pass * 100 / (total_run > 0 ? total_run : 1)))%\""
    echo "}"
else
    echo "── codex-harness gym ─────────────────────────────────────"
    echo "daemon: $DAEMON"
    echo "replays per fixture: $N"
    echo
    printf "%-30s  %-8s  %-6s  %s\n" "fixture" "pass" "rate" "fail reasons (first 3)"
    printf "%-30s  %-8s  %-6s  %s\n" "──────────" "────" "────" "──────"
    for r in "${results[@]}"; do
        IFS='|' read -r slug pf rate reasons <<< "$r"
        # Reasons can be many — trim to first 3 distinct.
        trimmed_reasons="$(echo "$reasons" | tr ' ' '\n' | sort -u | head -3 | tr '\n' ' ')"
        printf "%-30s  %-8s  %-6s  %s\n" "$slug" "$pf" "$rate" "$trimmed_reasons"
    done
    echo
    if [[ $total_run -gt 0 ]]; then
        echo "total: $total_pass/$total_run ($((total_pass * 100 / total_run))%)"
    fi
fi
