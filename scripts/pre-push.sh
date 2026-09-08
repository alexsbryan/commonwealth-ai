#!/usr/bin/env bash
# pre-push.sh — the primary correctness gate for this repo.
#
# WHAT THIS FILE IS, since 2026-09-07: the git push protocol, and nothing else.
# It resolves the range being pushed, turns it into a changed-file set, and
# hands that to `svrn quality check --trigger prepush`. WHICH gates run, in
# what order, with what budget, with what concurrency, and what a red does —
# all of it is DATA in `quality/instruments.toml` (the `runs_in = ["prepush"]`
# rows and the `[[trigger]] id = "prepush"` block beside them).
#
# It used to carry its own copy of that list: eleven gate invocations, their
# timing, their pass/fail aggregation, an advisory tier, a concurrency
# scaffold and a budget ledger in comments — 221 lines of executable
# duplication of a runner that already existed. Four other harnesses carried
# the same shapes. Adding a ratchet cost an edit here AND in ci.yml AND in the
# registry, and the three drifted, which is how `wizard-verify.sh` — the only
# coverage of the packaged boot chain — ended up referenced by prose and by
# nothing executable for ten days after catching a ship-blocking bug on its
# first run.
#
# ## Why the real gate is local, not in CI
#
# CI is a safety net. It is not the thing that stops bad code, because by the
# time CI speaks the code is already on main, and because CI is a metered
# resource that can — and on 2026-07-24 did — simply stop running when the
# month's Actions allowance ran out (docs/CI_ECONOMY.md has the audit).
#
# A gate you pay per-invocation for is a gate you will eventually ration. A
# gate that runs on hardware you already own is one you can afford to run on
# every push, forever. So the ordering is deliberate:
#
#   * THIS HOOK decides whether code is fit to leave your machine.
#   * CI CONFIRMS it on a clean checkout, and gates contributions from people
#     whose machines we do not control.
#
# ## The budget is ONE MINUTE, and it is a hard constraint
#
# Operator direction 2026-08-30: "prepush has to be less than 1 minute
# otherwise I'm skipping it always." That is not a preference, it is how
# gates die. A gate routinely bypassed with --no-verify protects nothing
# (ARCH §18.1: a gate you have not watched fail is not a gate), so a gate
# that costs more than a minute is strictly worse than a cheaper one that
# actually runs.
#
# The budget now lives where it can be enforced rather than remembered — the
# trigger's `budget_secs = 60` — and the two facts that make it reachable are
# declared beside it instead of being scaffolded here:
#
#   concurrency = 2         the workspace compile overlaps the ratchets. This
#                           file used to launch it at :380 and collect it at
#                           :565 by hand. Serial, the memory-starved worst
#                           case was 79s and out of budget; overlapped, ~58s
#                           and inside it.
#   [[trigger.prepare]]     the xtask binary is built ONCE, before the gates,
#                           and the gates' `cargo xtask <g>` argv is rewritten
#                           to `target/debug/xtask <g>`. Eleven cargo
#                           invocations would otherwise queue on the target
#                           lock and the concurrency would silently become a
#                           queue — this file said that twice, and the first
#                           draft of it made exactly that mistake.
#
# So the WORKSPACE TEST RUN IS NOT HERE. It was the whole cost — ~45-60s warm
# and several minutes cold — against ~15s for everything else combined. CI
# runs it on a clean checkout; that is the authority for "do the tests pass",
# and `./scripts/sovereign-test.sh --human` is the authority for "do they pass
# HERE", run when you mean to rather than on every push.
#
# Two cautions on any timing you read, both learned the hard way 2026-09-03:
#
#   * the compile's range is the DIFF's, not this file's: 16s over 4 crates,
#     67s over the 34 a merge touched. It is the only term that scales with
#     what you are pushing, and on a big merge it IS the budget. Which crates
#     it resolves comes from SOVEREIGN_CHANGED_PATHS, exported below.
#   * every number assumes a quiet machine. A run measured at 261s was not a
#     slow gate: load average was 12.85 with rust-analyzer, a 35GB-RSS daemon
#     and a second agent harness resident, and sovereign-lint.sh had derived
#     `jobs: 2` from 5GB free. Read the `jobs:` line on its banner before
#     concluding anything.
#
# ## Adding or removing a gate
#
# Edit `quality/instruments.toml`. A row with `runs_in = ["prepush"]` runs
# here; `enforcement = "advisory"` reports without blocking; `when_changed`
# scopes it to a diff. Nothing in this file changes, and `cargo xtask
# instrument-gate` will not let the row go missing from the map.
#
#     svrn quality check --trigger prepush --dry-run
#
# prints exactly what this hook would run, in the order it would run it,
# against the budget — without running any of it.
#
# ## Escape hatches
#
# Real ones, because a gate with no escape hatch gets uninstalled:
#
#   git push --no-verify           # skip every hook, one push
#   SOVEREIGN_SKIP_PREPUSH=1 git push
#
# Use them when you mean to (pushing a WIP branch for a colleague to look at,
# racing a hotfix). Do not use them to push red code to main — CI will catch
# it, and now that CI is affordable again it will actually be running.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

# git exports these when it runs a hook, and they point at THIS repo's .git.
# Any child that shells out to git in another checkout would inherit them and
# operate on the wrong tree.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_PREFIX

if [[ -n "${SOVEREIGN_SKIP_PREPUSH:-}" ]]; then
    echo "pre-push: skipped (SOVEREIGN_SKIP_PREPUSH set)" >&2
    exit 0
fi

say()  { printf '%s\n' "pre-push: $*" >&2; }
fail() { printf '%s\n' "pre-push: $*" >&2; }

# ── The push range, from git's own hook protocol on stdin ────────────
#
# `<local ref> <local sha> <remote ref> <remote sha>` per ref being pushed.
ZERO="0000000000000000000000000000000000000000"
RANGE=""
while read -r _local_ref local_sha _remote_ref remote_sha; do
    [[ -z "${local_sha:-}" ]] && continue
    [[ "$local_sha" == "$ZERO" ]] && continue   # deleting a remote branch
    if [[ "${remote_sha:-$ZERO}" == "$ZERO" ]]; then
        base="$(git merge-base "$local_sha" origin/main 2>/dev/null || true)"
        RANGE="${base:-HEAD~1}..$local_sha"
    else
        RANGE="$remote_sha..$local_sha"
    fi
done

HAND_RUN=0
if [[ -z "$RANGE" ]]; then
    HAND_RUN=1
    if [[ -n "$(git rev-parse --verify -q origin/main 2>/dev/null || true)" ]]; then
        RANGE="origin/main..HEAD"
    else
        RANGE="HEAD~1..HEAD"
    fi
    say "no push range on stdin — falling back to ${RANGE}"
fi

# ── Fail closed ─────────────────────────────────────────────────────
#
# `git diff` FAILING is not the same fact as "this push changes nothing". A
# shallow clone, a rebased branch, a pruned ref or an unknown sha all exit
# non-zero with no output, and conflating the two would pass the gate green,
# in silence, exactly when it is least sure of itself. Watched in
# scripts/tests/pre-push-fail-closed.sh, all four directions.
CHANGED="$(git diff --name-only "$RANGE" 2>/dev/null)"
diff_status=$?
if (( diff_status != 0 )); then
    say "could not diff ${RANGE} (git exit ${diff_status}) — gating EVERYTHING rather than assuming it is clean"
    CHANGED="$(git ls-files)"
elif [[ -z "$CHANGED" ]]; then
    if (( HAND_RUN )); then
        CHANGED="$( { git diff --name-only HEAD; git ls-files --others --exclude-standard; } 2>/dev/null | sort -u)"
        [[ -n "$CHANGED" ]] && say "nothing unpushed — gating your uncommitted working tree instead"
    fi
    if [[ -z "$CHANGED" ]]; then
        say "no file changes in ${RANGE} — nothing to gate"
        exit 0
    fi
fi

n_changed=$(printf '%s\n' "$CHANGED" | wc -l | tr -d ' ')
say "gating ${n_changed} changed file(s) in ${RANGE}"

# ── The dispatcher ──────────────────────────────────────────────────
#
# NEVER built here. `cargo build -p sovereign-cli` after a change to
# sovereign-core is minutes, which is the budget several times over — and it
# is unnecessary, because the registry is DATA read at runtime: a dispatcher
# from last week runs today's gate list. A MISSING one is a different matter
# and is a loud refusal, never a silent skip (ARCH §18.3).
SVRN=""
for cand in "$REPO_ROOT/target/debug/sovereign-cli" "$(command -v svrn || true)" \
            "$(command -v sovereign || true)"; do
    [[ -n "$cand" && -x "$cand" ]] && { SVRN="$cand"; break; }
done
if [[ -z "$SVRN" ]]; then
    fail "no sovereign-cli to run the gates with — NOT gating, and NOT passing."
    fail "  build it:  cargo build -p sovereign-cli --features dev-tools"
    fail "  push anyway (you are on your own):  git push --no-verify"
    exit 1
fi

# The compile gate resolves its crate scope from this. Colon-separated is the
# spelling scripts/sovereign-lint.sh already reads.
export SOVEREIGN_CHANGED_PATHS="$(printf '%s\n' "$CHANGED" | tr '\n' ':')"

"$SVRN" quality check --trigger prepush
rc=$?

# Exit 2 is the dispatcher refusing its own arguments, and the overwhelmingly
# likely cause is a sovereign-cli built WITHOUT `dev-tools` — which has no
# `quality` verb at all. Say the repair rather than letting a setup problem
# read as a code failure.
if (( rc == 2 )); then
    fail "the gate runner refused to start (exit 2)."
    fail "  most likely: sovereign-cli was built without \`--features dev-tools\`,"
    fail "  which drops the \`quality\` verb entirely. Repair:"
    fail "    cargo build -p sovereign-cli --features dev-tools"
    exit 1
fi

if (( rc == 0 )); then
    say "all gates passed — pushing (${SECONDS}s)"
    if (( SECONDS > 60 )); then
        say "this run took ${SECONDS}s — over the 60s budget. A gate that costs"
        say "more than a minute gets skipped, which is worse than a cheaper one."
        say "Trim it: svrn quality check --trigger prepush --dry-run"
    fi
    exit 0
fi

fail "PUSH BLOCKED in ${SECONDS}s. Each gate's findings are above, and each"
fail "ends with its own fix command."
fail "  To push anyway:  git push --no-verify"
exit 1
