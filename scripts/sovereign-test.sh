#!/usr/bin/env bash
# sovereign-test.sh — repo-wide test runner for the sovereign daemon's
# `test_status` watcher and the agent's pre-merge regression gate.
#
# Two faces, one truth:
#
# - **Daemon mode** (default, no flags): emits Tier 2 JSONL events
#   that `test_results.db` consumes; the daemon turns those into
#   `sovereign tools call test_status` (`fresh_passing` / `fresh_failing`).
# - **Human/agent mode** (`--human`): emits a compact summary, lists
#   every failing test by name, and points at the saved adapter logs
#   for failure-output triage.
#
# Coverage. One `cargo test --workspace` invocation. Pre-monorepo this
# script fanned out across three independent cargo workspaces; the
# 2026-05-10 monorepo collapse means a single root workspace covers
# every crate, and one cargo invocation does the job a fan would.
# Treesitter is enabled explicitly (`-F corpus-engine/treesitter`)
# because sovereign-test ran corpus-engine with --features treesitter
# before the merge and we don't want test coverage to silently shrink.
#
# Definition-of-done. Every feature push expects:
#   `./scripts/sovereign-test.sh --human` → "all green" (or
#   `sovereign tools call test_status` → `fresh_passing`)
# before merge. The daemon's watcher polls this script on debounce;
# the operator/agent invokes it on demand.
#
# Flags:
#   --human                 Compact human-readable summary on stderr.
#                           Tier 2 JSONL still written to logs; stdout
#                           becomes the summary.
#   --package <name>        Run only the named package (e.g.
#                           `--package sovereign-cli`). Repeatable or
#                           comma-separated. Maps to cargo's `-p` flag.
#   --filter <pattern>      Pass <pattern> to cargo test as a name
#                           filter — useful for targeted reruns.
#   --no-default-features   Skip the corpus-engine treesitter feature
#                           (and any others). Default off.
#   --keep-logs             Preserve adapter logs even on success
#                           (failures always preserve).
#   -h, --help              This message.
#
# Outputs Tier 2 JSONL events on stdout (one per line):
#   {"t":"pass","n":"<test_name>"}
#   {"t":"fail","n":"<test_name>","out":"<captured output>"}
#   {"t":"summary","pass":<N>,"fail":<N>,"warn":0,"ms":<elapsed_ms>}
#
# Exit code: 0 iff cargo test exits 0 AND no `fail` events were
# emitted. Non-zero on any failure or build error.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-cargo-test-adapter"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
LOG_DIR="${REPO_ROOT}/target/sovereign-test"

PACKAGES=()
HUMAN=0
KEEP_LOGS=0
FILTER=""
EXTRA_FEATURES="--features corpus-engine/treesitter"

print_help() {
    sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --human) HUMAN=1; shift ;;
        --package)
            shift
            IFS=',' read -ra parts <<< "$1"
            for p in "${parts[@]}"; do PACKAGES+=("$p"); done
            shift
            ;;
        --filter)
            shift
            FILTER="$1"
            shift
            ;;
        --no-default-features)
            EXTRA_FEATURES=""
            shift
            ;;
        --keep-logs) KEEP_LOGS=1; shift ;;
        -h|--help) print_help; exit 0 ;;
        *)
            echo "sovereign-test: unknown arg '$1' (use --help)" >&2
            exit 2
            ;;
    esac
done

# Build cargo argv. `--workspace` covers every member; `-p` filters
# stack on top so `--package foo --package bar` runs only those.
cargo_argv=(test)
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
    cargo_argv+=(--workspace)
else
    for p in "${PACKAGES[@]}"; do cargo_argv+=(-p "$p"); done
fi
# shellcheck disable=SC2206
cargo_argv+=($EXTRA_FEATURES --no-fail-fast)
if [[ -n "$FILTER" ]]; then
    cargo_argv+=(-- "$FILTER")
fi

# ── Adapter-absent fallback ────────────────────────────────────────────────
if [[ ! -x "$ADAPTER" ]]; then
    echo "sovereign-test: adapter not found at $ADAPTER — running raw cargo test" >&2
    (cd "$REPO_ROOT" && cargo "${cargo_argv[@]}" 2>&1)
    exit $?
fi

# ── Run cargo test --workspace ─────────────────────────────────────────────
# Per-invocation scratch dir so concurrent runs (e.g. daemon watcher
# + manual run) don't collide on the log files. Promoted to
# LOG_DIR/latest at the end.
mkdir -p "$LOG_DIR"
RUN_DIR="${LOG_DIR}/.runs/$$-$(date +%s)"
mkdir -p "$RUN_DIR"

raw_log="${RUN_DIR}/cargo.raw.log"
out_jsonl="${RUN_DIR}/cargo.jsonl"
exit_file="${RUN_DIR}/cargo.exit"

start_ms=$(($(date +%s%N) / 1000000))

(
    cd "$REPO_ROOT"
    cargo "${cargo_argv[@]}" 2>&1 | tee "$raw_log" | "$ADAPTER" "monorepo" > "$out_jsonl"
    echo "${PIPESTATUS[0]}" > "$exit_file"
)

elapsed_ms=$(( $(date +%s%N) / 1000000 - start_ms ))
exit_val=$(cat "$exit_file" 2>/dev/null || echo 1)

# ── Aggregate ───────────────────────────────────────────────────────────────
total_pass=0
total_fail=0
failed_names=""

while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    kind=$(echo "$line" | python3 -c "
import sys, json
try:
    d = json.loads(sys.stdin.read())
    print(d.get('t', ''))
except Exception:
    pass
" 2>/dev/null) || continue

    case "$kind" in
        summary)
            total_pass=$(echo "$line" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('pass',0))" 2>/dev/null || echo 0)
            total_fail=$(echo "$line" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('fail',0))" 2>/dev/null || echo 0)
            ;;
        fail)
            n=$(echo "$line" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('n',''))" 2>/dev/null || echo "")
            [[ -n "$n" ]] && failed_names+="${n}"$'\n'
            [[ $HUMAN -eq 0 ]] && echo "$line"
            ;;
        pass|*)
            [[ $HUMAN -eq 0 ]] && echo "$line"
            ;;
    esac
done < "$out_jsonl"

final_summary="{\"t\":\"summary\",\"pass\":${total_pass},\"fail\":${total_fail},\"warn\":0,\"ms\":${elapsed_ms}}"

if [[ $HUMAN -eq 1 ]]; then
    {
        echo
        echo "═══════════════════════════════════════════════════════════════"
        echo " sovereign-test — repo-wide regression gate"
        echo "═══════════════════════════════════════════════════════════════"
        printf " %-12s  %s\n" "pass:" "$total_pass"
        printf " %-12s  %s\n" "fail:" "$total_fail"
        printf " %-12s  %s\n" "elapsed:" "${elapsed_ms}ms"
        printf " %-12s  %s\n" "cargo exit:" "$exit_val"
        echo

        if [[ "$total_fail" -gt 0 ]] || [[ "$exit_val" != "0" ]]; then
            if [[ -n "$failed_names" ]]; then
                echo " ✘ Failures:"
                while IFS= read -r failed; do
                    [[ -z "$failed" ]] && continue
                    echo "    $failed"
                done <<< "$failed_names"
            fi
            if [[ "$exit_val" != "0" ]] && [[ "$total_fail" == "0" ]]; then
                echo " ✘ Cargo exited $exit_val with no test failures parsed —"
                echo "    likely a build error. See raw log:"
                echo "      ${LOG_DIR}/latest/cargo.raw.log"
            fi
            echo
            echo " Triage:"
            echo "   - Raw cargo output:  ${LOG_DIR}/latest/cargo.raw.log"
            echo "   - Adapter JSONL:     ${LOG_DIR}/latest/cargo.jsonl"
            echo "   - Rerun a name filter: $0 --human --filter <pattern>"
            echo "   - Rerun one package:   $0 --human --package <crate>"
            echo
        else
            echo " ✓ All green."
            echo
        fi
    } >&2
fi

echo "$final_summary"

# ── Promote scratch run → latest ───────────────────────────────────────────
if [[ -d "$RUN_DIR" ]]; then
    rm -rf "${LOG_DIR}/latest" 2>/dev/null || true
    mv "$RUN_DIR" "${LOG_DIR}/latest" 2>/dev/null || true
fi
if [[ -d "${LOG_DIR}/.runs" ]]; then
    # shellcheck disable=SC2012
    ls -1t "${LOG_DIR}/.runs" 2>/dev/null | tail -n +6 | while IFS= read -r old; do
        rm -rf "${LOG_DIR}/.runs/${old}" 2>/dev/null || true
    done
fi

exit "$exit_val"
