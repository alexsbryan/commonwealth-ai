#!/bin/bash
# Every end-to-end suite for the Claude Code hooks in this directory.
#
# These exercise the REAL scripts and the REAL `sovereign` binary against an
# isolated store (SOVEREIGN_SESSIONS_DIR / SOVEREIGN_LINEAGE_DIR), so they
# prove behaviour rather than restating it. They are shell, not `cargo test`,
# because the thing under test IS a shell hook plus a subprocess boundary —
# unit tests on either side cannot see the seam where continuity actually
# broke.
#
#   .claude/hooks/tests/run-all.sh              # all suites
#   bash .claude/hooks/tests/lineage-cli.sh     # one suite
#
# Requires target/debug/sovereign-cli (cargo build -p sovereign-cli --features dev-tools).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

if [ ! -x target/debug/sovereign-cli ]; then
    echo "run-all: target/debug/sovereign-cli missing — build it first:"
    echo "  cargo build -p sovereign-cli --features dev-tools"
    exit 2
fi

rc=0
for suite in "$(dirname "$0")"/*.sh; do
    case "$suite" in *run-all.sh) continue ;; esac
    echo "════════ $(basename "$suite") ════════"
    bash "$suite" || rc=1
    echo
done
[ "$rc" -eq 0 ] && echo "ALL HOOK SUITES GREEN" || echo "SOME HOOK SUITES FAILED"
exit "$rc"
