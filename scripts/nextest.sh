#!/usr/bin/env bash
# nextest.sh — fast-path test runner using cargo-nextest.
#
# Sibling to sovereign-test.sh. The sovereign-test.sh path emits Tier 2
# JSONL the daemon's test_results watcher consumes; the watcher's
# adapter parses libtest output and is not nextest-aware yet. So this
# script is for interactive/agent runs where speed matters and the
# daemon watcher isn't watching — typical dev iteration.
#
# When to use which:
# - Iterating on a feature, want a fast green:  ./scripts/nextest.sh --human
# - Pre-merge regression gate (writes to       :  ./scripts/sovereign-test.sh --human
#   test_results.db, drives `test_status`)
#
# Flags mirror sovereign-test.sh where they make sense:
#   --human                 Compact human summary on stderr.
#   --package <name>        Run one crate (repeatable or comma-separated).
#   --filter <expr>         Nextest filter expression (more expressive
#                           than cargo test — uses nextest's E-DSL).
#                           e.g. 'test(/router/)', 'package(sovereign-cli)'.
#   --no-default-features   Skip corpus-engine/treesitter feature.
#   --profile <name>        nextest profile: default | ci | quick (see
#                           .config/nextest.toml). Default: default.
#   --no-doc                Skip the cargo test --doc pass at the end.
#                           Nextest does not run doctests itself — this
#                           script appends a separate cargo invocation
#                           unless --no-doc is passed.
#   -h, --help              This message.
#
# Exit code: non-zero on any test failure or build error (nextest's
# native exit code is preserved; doctest failures are folded in too).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"

# resolve_features / nextest_install_hint — shared with sovereign-test.sh so the
# two runners cover identically. See scripts/lib/cargo-scope.sh.
# shellcheck source=lib/cargo-scope.sh
source "${SCRIPT_DIR}/lib/cargo-scope.sh"

PACKAGES=()
HUMAN=0
FILTER=""
PROFILE="default"
# WHICH `<pkg>/<feature>` flags apply is decided after arg parsing, from the
# resolved package selection — see "Feature selection" below.
WANT_FEATURES=1
EXTRA_FEATURES=""
RUN_DOCTESTS=1

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
            WANT_FEATURES=0
            shift
            ;;
        --profile)
            shift
            PROFILE="$1"
            shift
            ;;
        --no-doc)
            RUN_DOCTESTS=0
            shift
            ;;
        -h|--help) print_help; exit 0 ;;
        *)
            echo "nextest: unknown arg '$1' (use --help)" >&2
            exit 2
            ;;
    esac
done

if ! command -v cargo-nextest >/dev/null 2>&1; then
    echo "nextest: cargo-nextest not installed." >&2
    # Platform-correct hint — the precompiled tarballs are keyed by OS, so a
    # hardcoded /mac URL handed Linux developers a macOS binary.
    nextest_install_hint >&2
    echo "  (scripts/sovereign-test.sh works without nextest — it's the gate anyway.)" >&2
    exit 2
fi

# ── Feature selection — scope-aware ────────────────────────────────────────
# Identical rule to sovereign-test.sh: a `<pkg>/<feature>` flag whose package is
# outside the `-p` selection is a hard cargo ERROR, not a no-op. This script
# previously hardcoded `--features corpus-engine/treesitter` (so `--package
# oicp-types` could not run at all) and never passed sovereign-cli/dev-tools at
# all (so it silently UNDER-COVERED the dev-verb suites that the gate runs).
# Both runners now ask the same helper, so their coverage cannot drift apart.
if [[ $WANT_FEATURES -eq 1 ]]; then
    feature_list="$(resolve_features ${PACKAGES[@]+"${PACKAGES[@]}"})"
    if [[ -n "$feature_list" ]]; then
        EXTRA_FEATURES="--features $feature_list"
        [[ ${#PACKAGES[@]} -gt 0 ]] && \
            echo "nextest: features in scope: ${feature_list}" >&2
    fi
fi

# Build nextest argv.
nextest_argv=(nextest run --profile "$PROFILE")
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
    nextest_argv+=(--workspace)
else
    for p in "${PACKAGES[@]}"; do nextest_argv+=(-p "$p"); done
fi
# shellcheck disable=SC2206
nextest_argv+=($EXTRA_FEATURES)
if [[ -n "$FILTER" ]]; then
    nextest_argv+=(-E "$FILTER")
fi

start_ms=$(($(date +%s%N) / 1000000))

cd "$REPO_ROOT"
cargo "${nextest_argv[@]}"
nextest_exit=$?

doctest_exit=0
if [[ $RUN_DOCTESTS -eq 1 ]]; then
    if [[ $HUMAN -eq 1 ]]; then
        echo "── doctests ────────────────────────────────────────────────" >&2
    fi
    doc_argv=(test --doc)
    if [[ ${#PACKAGES[@]} -eq 0 ]]; then
        doc_argv+=(--workspace)
    else
        for p in "${PACKAGES[@]}"; do doc_argv+=(-p "$p"); done
    fi
    # shellcheck disable=SC2206
    doc_argv+=($EXTRA_FEATURES)
    cargo "${doc_argv[@]}"
    doctest_exit=$?
fi

elapsed_ms=$(( $(date +%s%N) / 1000000 - start_ms ))

# Final exit = first non-zero of (nextest, doctests).
final_exit=$nextest_exit
[[ $final_exit -eq 0 ]] && final_exit=$doctest_exit

if [[ $HUMAN -eq 1 ]]; then
    {
        echo
        echo "═══════════════════════════════════════════════════════════════"
        echo " nextest — fast-path test runner"
        echo "═══════════════════════════════════════════════════════════════"
        printf " %-14s  %s\n" "profile:" "$PROFILE"
        printf " %-14s  %s\n" "nextest exit:" "$nextest_exit"
        if [[ $RUN_DOCTESTS -eq 1 ]]; then
            printf " %-14s  %s\n" "doctests exit:" "$doctest_exit"
        fi
        printf " %-14s  %s\n" "elapsed:" "${elapsed_ms}ms"
        echo
        if [[ $final_exit -eq 0 ]]; then
            echo " ✓ All green."
        else
            echo " ✘ Failures above. Rerun a filter:"
            echo "     $0 --human --filter 'test(/<pattern>/)'"
            echo "   Or one crate:"
            echo "     $0 --human --package <crate>"
        fi
        echo
    } >&2
fi

exit $final_exit
