#!/usr/bin/env bash
# sovereign-test.sh — repo-wide test runner for the sovereign daemon's
# `test_status` watcher and the agent's pre-merge regression gate.
#
# Two faces, one truth:
#
# - **Daemon mode** (default, no flags): emits Tier 2 JSONL events that
#   `test_results.db` consumes; the daemon turns those into
#   `sovereign tools call test_status` (`fresh_passing` / `fresh_failing`).
# - **Human/agent mode** (`--human`): emits a compact per-workspace
#   summary, lists every failing test by name, and points at the saved
#   adapter logs for failure-output triage. Works equally well for an
#   operator at the terminal and an agent reading via Bash.
#
# Coverage. Mirrors `scripts/sovereign-lint.sh`: runs all three workspaces
# (corpus-engine + sovereign + commonwealth) in parallel. Pre-2026-05-10
# this script silently skipped commonwealth, so the auto_recover SCIP-
# canonical regression and any other commonwealth-only regression landed
# unobserved. Always run all three.
#
# Definition-of-done. Every feature push expects:
#   `./scripts/sovereign-test.sh --human` → "all green" (or
#   `sovereign tools call test_status` → `fresh_passing`)
# before merge. The daemon's watcher polls this script on debounce; the
# operator/agent invokes it on demand.
#
# Flags:
#   --human                 Compact human-readable summary on stderr.
#                           Tier 2 JSONL still goes to the regular log
#                           file; stdout becomes the summary.
#   --workspace <name>      Run only the named workspace (corpus-engine,
#                           sovereign, or commonwealth). Default: all.
#                           Can repeat or comma-separate.
#   --filter <pattern>      Pass <pattern> to cargo test as a name filter.
#                           Useful for targeted reruns: `--filter notes`.
#   --no-parallel           Run workspaces sequentially. Useful when
#                           debugging adapter or runner issues.
#   --keep-logs             Preserve per-workspace adapter logs at
#                           `target/sovereign-test/<workspace>.jsonl`
#                           even on success. (Failures always preserve.)
#   -h, --help              This message.
#
# Outputs Tier 2 JSONL events on stdout (one per line):
#   {"t":"pass","n":"<test_name>"}
#   {"t":"fail","n":"<test_name>","out":"<captured output>"}
#   {"t":"summary","pass":<N>,"fail":<N>,"warn":0,"ms":<elapsed_ms>}
#
# Exit code: 0 iff every workspace's cargo test exits 0 AND no `fail`
# events were emitted. Non-zero on any test failure or build error.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER="${SCRIPT_DIR}/../sovereign/crates/sovereign-tools/src/code/test_adapters/sovereign-cargo-test-adapter"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
LOG_DIR="${REPO_ROOT}/target/sovereign-test"

ALL_WORKSPACES=(corpus-engine sovereign commonwealth)
SELECTED_WORKSPACES=()
HUMAN=0
PARALLEL=1
KEEP_LOGS=0
FILTER=""

print_help() {
    sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//'
}

# ── Arg parsing ────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --human) HUMAN=1; shift ;;
        --workspace)
            shift
            IFS=',' read -ra parts <<< "$1"
            for p in "${parts[@]}"; do
                SELECTED_WORKSPACES+=("$p")
            done
            shift
            ;;
        --filter)
            shift
            FILTER="$1"
            shift
            ;;
        --no-parallel) PARALLEL=0; shift ;;
        --keep-logs) KEEP_LOGS=1; shift ;;
        -h|--help) print_help; exit 0 ;;
        *)
            echo "sovereign-test: unknown arg '$1' (use --help)" >&2
            exit 2
            ;;
    esac
done

# Default to all workspaces if none selected.
if [[ ${#SELECTED_WORKSPACES[@]} -eq 0 ]]; then
    SELECTED_WORKSPACES=("${ALL_WORKSPACES[@]}")
fi

# Validate every selected workspace is one of the known three —
# typo-protection so `--workspace common` fails loudly instead of
# silently running zero tests.
for ws in "${SELECTED_WORKSPACES[@]}"; do
    found=0
    for known in "${ALL_WORKSPACES[@]}"; do
        if [[ "$ws" == "$known" ]]; then found=1; break; fi
    done
    if [[ $found -eq 0 ]]; then
        echo "sovereign-test: unknown workspace '$ws' — must be one of: ${ALL_WORKSPACES[*]}" >&2
        exit 2
    fi
done

flags_for() {
    case "$1" in
        corpus-engine) echo "--features treesitter" ;;
        *)             echo "" ;;
    esac
}

# ── Adapter-absent fallback ────────────────────────────────────────────────
# Keeps a fresh checkout usable for human-driven cargo test even when
# the adapter binary hasn't been built yet. Sequential, no JSONL.
if [[ ! -x "$ADAPTER" ]]; then
    echo "sovereign-test: adapter not found at $ADAPTER — running raw cargo test" >&2
    overall=0
    for workspace in "${SELECTED_WORKSPACES[@]}"; do
        ws_path="${REPO_ROOT}/${workspace}"
        if [[ ! -d "$ws_path" ]]; then continue; fi
        extra_flags="$(flags_for "$workspace")"
        # shellcheck disable=SC2086
        if [[ -n "$FILTER" ]]; then
            (cd "$ws_path" && cargo test $extra_flags -- "$FILTER" 2>&1) || overall=1
        else
            (cd "$ws_path" && cargo test $extra_flags 2>&1) || overall=1
        fi
    done
    exit $overall
fi

# ── Per-workspace runner ───────────────────────────────────────────────────
# Each invocation writes adapter output to its OWN scratch dir under
# LOG_DIR/.runs/<pid>-<ts>/<workspace>.{jsonl,raw.log,exit}. On
# completion, the scratch dir is renamed to LOG_DIR/latest/ so a
# follow-up triage doesn't have to know the pid. Per-invocation
# isolation is load-bearing because the daemon's test_status watcher
# spawns this same script on file-change debounce, and a manual run
# triggered while the watcher is mid-flight would otherwise collide
# on the same JSONL files (observed: two concurrent writers produced
# torn JSON lines and a duplicated `summary` event mid-stream).
mkdir -p "$LOG_DIR"
RUN_DIR="${LOG_DIR}/.runs/$$-$(date +%s)"
mkdir -p "$RUN_DIR"
# Don't trap-cleanup on success: we promote RUN_DIR → LOG_DIR/latest
# at the end. Failure paths preserve RUN_DIR for triage.

run_workspace() {
    local workspace="$1"
    local ws_path="${REPO_ROOT}/${workspace}"
    if [[ ! -d "$ws_path" ]]; then
        echo "0" > "${RUN_DIR}/${workspace}.exit"
        : > "${RUN_DIR}/${workspace}.jsonl"
        return 0
    fi
    local extra_flags
    extra_flags="$(flags_for "$workspace")"
    local out_jsonl="${RUN_DIR}/${workspace}.jsonl"
    local raw_log="${RUN_DIR}/${workspace}.raw.log"
    local exit_file="${RUN_DIR}/${workspace}.exit"

    # tee the raw cargo output into raw_log (for human triage) and into
    # the adapter (for Tier 2 JSONL). PIPESTATUS captures the cargo exit.
    # shellcheck disable=SC2086
    (
        cd "$ws_path"
        if [[ -n "$FILTER" ]]; then
            cargo test $extra_flags -- "$FILTER" 2>&1 | tee "$raw_log" | "$ADAPTER" "$workspace" > "$out_jsonl"
        else
            cargo test $extra_flags 2>&1 | tee "$raw_log" | "$ADAPTER" "$workspace" > "$out_jsonl"
        fi
        echo "${PIPESTATUS[0]}" > "$exit_file"
    )
}

start_ms=$(($(date +%s%N) / 1000000))

if [[ $PARALLEL -eq 1 ]]; then
    pids=()
    for workspace in "${SELECTED_WORKSPACES[@]}"; do
        run_workspace "$workspace" &
        pids+=($!)
    done
    for pid in "${pids[@]}"; do
        wait "$pid" || true
    done
else
    for workspace in "${SELECTED_WORKSPACES[@]}"; do
        run_workspace "$workspace"
    done
fi

elapsed_ms=$(( $(date +%s%N) / 1000000 - start_ms ))

# ── Aggregate ───────────────────────────────────────────────────────────────
overall=0
total_pass=0
total_fail=0

# Per-workspace counts for the human summary, populated in stable order.
declare -a ws_names ws_pass ws_fail ws_exit ws_failed_names

for workspace in "${SELECTED_WORKSPACES[@]}"; do
    out_file="${RUN_DIR}/${workspace}.jsonl"
    exit_file="${RUN_DIR}/${workspace}.exit"
    [[ -f "$out_file" ]] || continue

    exit_val=0
    [[ -f "$exit_file" ]] && exit_val=$(cat "$exit_file")
    [[ "$exit_val" != "0" ]] && overall=1

    ws_pass_n=0
    ws_fail_n=0
    ws_failed_list=""

    # Read the adapter's events. The adapter emits per-test pass/fail
    # plus exactly one summary line at EOF whose counts are
    # authoritative (post-2026-05-10 fix; the pre-fix adapter
    # silently truncated to the last binary's totals).
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        # Parse as JSON, dispatch by t.
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
                p=$(echo "$line" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('pass',0))" 2>/dev/null) || p=0
                f=$(echo "$line" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('fail',0))" 2>/dev/null) || f=0
                ws_pass_n="$p"
                ws_fail_n="$f"
                # Deliberately do NOT echo per-workspace summary;
                # one repo-wide summary is emitted at the end.
                ;;
            fail)
                # Capture the failing test name for the human summary.
                n=$(echo "$line" | python3 -c "import sys,json; print(json.loads(sys.stdin.read()).get('n',''))" 2>/dev/null) || n=""
                if [[ -n "$n" ]]; then
                    ws_failed_list+="${workspace}::${n}"$'\n'
                fi
                # Pass through the JSONL event for the daemon.
                if [[ $HUMAN -eq 0 ]]; then
                    echo "$line"
                fi
                ;;
            pass)
                if [[ $HUMAN -eq 0 ]]; then
                    echo "$line"
                fi
                ;;
            *)
                if [[ $HUMAN -eq 0 ]]; then
                    echo "$line"
                fi
                ;;
        esac
    done < "$out_file"

    total_pass=$((total_pass + ws_pass_n))
    total_fail=$((total_fail + ws_fail_n))
    ws_names+=("$workspace")
    ws_pass+=("$ws_pass_n")
    ws_fail+=("$ws_fail_n")
    ws_exit+=("$exit_val")
    ws_failed_names+=("$ws_failed_list")
done

# ── Final summary ──────────────────────────────────────────────────────────
final_summary="{\"t\":\"summary\",\"pass\":${total_pass},\"fail\":${total_fail},\"warn\":0,\"ms\":${elapsed_ms}}"

if [[ $HUMAN -eq 1 ]]; then
    {
        echo
        echo "═══════════════════════════════════════════════════════════════"
        echo " sovereign-test — repo-wide regression gate"
        echo "═══════════════════════════════════════════════════════════════"
        printf " %-18s  %8s  %8s  %s\n" "workspace" "pass" "fail" "cargo"
        printf " %-18s  %8s  %8s  %s\n" "─────────" "────" "────" "─────"
        for i in "${!ws_names[@]}"; do
            cargo_marker="ok"
            if [[ "${ws_exit[$i]}" != "0" ]]; then
                cargo_marker="exit=${ws_exit[$i]}"
            fi
            printf " %-18s  %8d  %8d  %s\n" \
                "${ws_names[$i]}" \
                "${ws_pass[$i]}" \
                "${ws_fail[$i]}" \
                "$cargo_marker"
        done
        printf " %-18s  %8s  %8s\n" "─────────" "────" "────"
        printf " %-18s  %8d  %8d  (%dms)\n" "TOTAL" "$total_pass" "$total_fail" "$elapsed_ms"
        echo

        # Failure list — the most useful thing in the report.
        if [[ $total_fail -gt 0 ]] || [[ $overall -ne 0 ]]; then
            echo " ✘ Failures:"
            for i in "${!ws_names[@]}"; do
                if [[ -n "${ws_failed_names[$i]}" ]]; then
                    while IFS= read -r failed; do
                        [[ -z "$failed" ]] && continue
                        echo "    $failed"
                    done <<< "${ws_failed_names[$i]}"
                fi
                if [[ "${ws_exit[$i]}" != "0" ]] && [[ "${ws_fail[$i]}" == "0" ]]; then
                    # cargo exited non-zero but the adapter saw no
                    # failed tests — usually a build error before
                    # any test ran. Surface the raw log path.
                    echo "    ${ws_names[$i]}: build/cargo error (exit ${ws_exit[$i]})"
                fi
            done
            echo
            echo " Triage:"
            echo "   - Raw cargo output:   ${LOG_DIR}/latest/<workspace>.raw.log"
            echo "   - Adapter JSONL:      ${LOG_DIR}/latest/<workspace>.jsonl"
            echo "   - Rerun one workspace: $0 --human --workspace <name>"
            echo "   - Rerun a name filter: $0 --human --filter <pattern>"
            echo
        else
            echo " ✓ All green."
            echo
        fi
    } >&2
fi

# Emit the final daemon-facing summary on stdout regardless of mode —
# the watcher's parser keys off the final `summary` line.
echo "$final_summary"

# ── Promote scratch run → latest ───────────────────────────────────────────
# Atomic rename so concurrent invocations don't see a half-written
# `latest/`. Best-effort cleanup of older `.runs/` scratch dirs (keep
# the 5 most recent for debugging).
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

exit $overall
