#!/usr/bin/env bash
# sovereign-lint.sh — repo-wide cargo check for the sovereign daemon's
# `lint_status` watcher.
#
# Pre-monorepo this script fanned `cargo check` across three
# independent workspaces in parallel. Post-monorepo (2026-05-10) one
# `cargo check --workspace` covers everything in a single invocation —
# the resolver unifies dep features and a single compile pass is
# faster than three. Treesitter is enabled explicitly to match what
# the test runner enables, so lint and test coverage stay aligned.
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

# ── Adapter-absent fallback ────────────────────────────────────────────────
if [[ ! -x "$ADAPTER" ]]; then
    echo "sovereign-lint: adapter not found at $ADAPTER — running raw cargo check" >&2
    (cd "$REPO_ROOT" && cargo check --workspace --features corpus-engine/treesitter 2>&1)
    exit $?
fi

# ── Single workspace check ────────────────────────────────────────────────
(cd "$REPO_ROOT" && cargo check --workspace --features corpus-engine/treesitter --message-format json 2>&1) | "$ADAPTER" "monorepo"
exit "${PIPESTATUS[0]}"
