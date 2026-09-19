#!/usr/bin/env bash
# The ralph queue's check macros, one verb each — the command a worker types
# for CLEAN / LINT / TEST(c) / LAYER / TOML / DOCS / NODE(d) / DEMO / TESTALL /
# PREPUSH in ralph/next/ring-doc/PROMPT.md's check table.
#
#   scripts/ralph-check.sh clean
#   scripts/ralph-check.sh test sovereign-mesh
#   scripts/ralph-check.sh node sovereign/apps/ring-doc
#
# Each runs the gate into target/ralph/<check>.log, prints `exit=N`, tails the
# log, and exits N. Why a script and not the inline string it replaces: the
# worker now runs as `claude -p` under an allowlist (ralph/claude-settings.json),
# and a command carrying `$?` or a redirect never matches a rule, prefix or
# exact — every macro asked the operator once per session (2026-09-17,
# log-permissions.txt 22:58-23:00). One verb, one rule: Bash(scripts/ralph-check.sh *).
#
# The macros' own rules are kept here, not re-decided:
#   clean  — no lock wrapper (at the 256G ceiling it is a du and never runs
#            cargo; the build-latency campaign holds the lock ~19 min a build)
#   lint / test / layer / docs / testall — through scripts/with-cargo-lock.sh
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
mkdir -p target/ralph

check="${1:-}"; shift || true
run() {   # run <log-name> <tail-lines> <command...>
    local log="target/ralph/$1.log" n="$2"; shift 2
    "$@" > "$log" 2>&1; local rc=$?
    echo "exit=$rc"; tail -n "$n" "$log"
    exit "$rc"
}
case "$check" in
    clean)   RALPH_CLEAN_MB="${RALPH_CLEAN_MB:-262144}" run build 5 ./scripts/dev-build.sh --clean --gate-only ;;
    lint)    run lint 5 ./scripts/with-cargo-lock.sh ./scripts/sovereign-lint.sh --human ;;
    test)    [ -n "${1:-}" ] || { echo "usage: ralph-check.sh test <crate>" >&2; exit 2; }
             run test 8 ./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human --package "$1" ;;
    layer)   run layer 5 bash -c 'cd corpus-engine && ../scripts/with-cargo-lock.sh cargo xtask layer-gate' ;;
    docs)    run docs 5 bash -c 'cd corpus-engine && ../scripts/with-cargo-lock.sh cargo xtask docs-gate' ;;
    toml)    python3 -c "import tomllib; [tomllib.load(open(p,'rb')) for p in ('quality/campaigns/ring-doc.toml','quality/campaigns/ring-apps.toml')]" \
             && python3 scripts/co-lineage.py list >/dev/null; rc=$?; echo "exit=$rc"; exit "$rc" ;;
    node)    [ -n "${1:-}" ] || { echo "usage: ralph-check.sh node <dir>" >&2; exit 2; }
             run node 8 node --test "$1" ;;
    demo)    run demo 12 scripts/ring-doc-demo.sh verdict all ;;
    # Campaign-neutral verbs (ei7-stage0, 2026-09-18). `toml` and `demo` above
    # name ring-doc's files; a queue that is not ring-doc uses these instead.
    testfn)  [ -n "${2:-}" ] || { echo "usage: ralph-check.sh testfn <crate> <whole-test-fn-name>" >&2; exit 2; }
             run testfn 8 ./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human --package "$1" --filter "$2" ;;
    env)     run env 5 bash -c 'cd corpus-engine && ../scripts/with-cargo-lock.sh cargo xtask env-gate' ;;
    py)      [ -n "${1:-}" ] || { echo "usage: ralph-check.sh py <script.py> (runs its --self-test)" >&2; exit 2; }
             run py 12 python3 "$1" --self-test ;;
    campaign) [ -n "${1:-}" ] || { echo "usage: ralph-check.sh campaign <id>" >&2; exit 2; }
             run campaign 5 python3 -c "import sys,tomllib; tomllib.load(open(f'quality/campaigns/{sys.argv[1]}.toml','rb'))" "$1" ;;
    desktop) run desktop 12 bash -c 'cd sovereign/crates/sovereign-desktop && npm run check && npm run test' ;;
    pilot)   run pilot 20 research/ontology-retrieval/pilot/run-pilot.sh ;;
    testall) run testall 12 ./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human ;;
    prepush) run prepush 20 ./scripts/pre-push.sh ;;
    *) echo "usage: scripts/ralph-check.sh clean|lint|test <crate>|testfn <crate> <fn>|layer|env|toml|campaign <id>|docs|py <script>|node <dir>|desktop|demo|pilot|testall|prepush" >&2; exit 2 ;;
esac
