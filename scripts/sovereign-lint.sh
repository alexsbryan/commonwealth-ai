#!/usr/bin/env bash
# sovereign-lint.sh — run cargo check across all three workspaces and emit
# Tier 2 lint events. Used as the default lint command for this repo.
#
# Runs all three workspace checks IN PARALLEL and merges their output.
#
# Outputs Tier 2 JSONL events (one per stdout line):
#   {"t":"pass","n":"<workspace>"}
#   {"t":"fail","n":"<file>","out":"<error>","line":<N>,"col":<N>}
#   {"t":"warn","n":"<file>","out":"<warning>","line":<N>,"col":<N>}
#   {"t":"summary","pass":<N>,"fail":<N>,"warn":<N>,"ms":<N>}

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-cargo-check-adapter"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

if [[ ! -x "$ADAPTER" ]]; then
    echo "sovereign-lint: adapter not found at $ADAPTER — running raw cargo check" >&2
    overall=0
    for workspace in corpus-engine sovereign commonwealth; do
        ws_path="${REPO_ROOT}/${workspace}"
        if [[ -d "$ws_path" ]]; then
            case "$workspace" in
                corpus-engine) extra_flags="--features treesitter" ;;
                *)             extra_flags="" ;;
            esac
            # shellcheck disable=SC2086
            (cd "$ws_path" && cargo check $extra_flags 2>&1) || overall=1
        fi
    done
    exit $overall
fi

# ── Parallel workspace checks ───────────────────────────────────────────────
# Each workspace writes its adapter output to a temp file; we wait for all
# three, then emit events in order and print a merged summary.

TMPDIR_LINT=$(mktemp -d)
trap 'rm -rf "$TMPDIR_LINT"' EXIT

pids=()
for workspace in corpus-engine sovereign commonwealth; do
    ws_path="${REPO_ROOT}/${workspace}"
    if [[ ! -d "$ws_path" ]]; then
        continue
    fi
    case "$workspace" in
        corpus-engine) extra_flags="--features treesitter" ;;
        *)             extra_flags="" ;;
    esac
    out_file="${TMPDIR_LINT}/${workspace}.jsonl"
    exit_file="${TMPDIR_LINT}/${workspace}.exit"
    # shellcheck disable=SC2086
    (
        cd "$ws_path"
        cargo check $extra_flags --message-format json 2>&1 | "$ADAPTER" "$workspace" > "$out_file" 2>/dev/null
        echo $? > "$exit_file"
    ) &
    pids+=($!)
done

# Wait for all background checks.
for pid in "${pids[@]}"; do
    wait "$pid" || true
done

# ── Merge output ─────────────────────────────────────────────────────────────
overall=0
total_pass=0
total_fail=0
total_warn=0

for workspace in corpus-engine sovereign commonwealth; do
    out_file="${TMPDIR_LINT}/${workspace}.jsonl"
    exit_file="${TMPDIR_LINT}/${workspace}.exit"
    [[ -f "$out_file" ]] || continue

    exit_val=0
    [[ -f "$exit_file" ]] && exit_val=$(cat "$exit_file")
    [[ "$exit_val" != "0" ]] && overall=1

    while IFS= read -r line; do
        t=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('t',''))" 2>/dev/null) || continue
        if [[ "$t" == "summary" ]]; then
            p=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('pass',0))" 2>/dev/null) || p=0
            f=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('fail',0))" 2>/dev/null) || f=0
            w=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('warn',0))" 2>/dev/null) || w=0
            total_pass=$((total_pass + p))
            total_fail=$((total_fail + f))
            total_warn=$((total_warn + w))
        else
            echo "$line"
        fi
    done < "$out_file"
done

echo "{\"t\":\"summary\",\"pass\":${total_pass},\"fail\":${total_fail},\"warn\":${total_warn},\"ms\":0}"
exit $overall
