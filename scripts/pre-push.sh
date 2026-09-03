#!/usr/bin/env bash
# pre-push.sh — the primary correctness gate for this repo.
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
# So the WORKSPACE TEST RUN IS NOT HERE. It was the whole cost — ~45-60s
# warm and several minutes cold — against ~15s for everything else combined.
# CI runs it on a clean checkout; that is now the authority for "do the tests
# pass", and `./scripts/sovereign-test.sh --human` is the authority for "do
# they pass HERE", run when you mean to rather than on every push.
#
# What is left is the set of checks that are fast, deterministic, and cannot
# be answered by reading the diff. Measured on this host 2026-08-30:
#
#   cargo check (--all-targets, real features)  14-58s   CONCURRENT
#   ─── everything below runs while that does ─────────
#   rustfmt                                        4s    (Rust pushes only)
#   xtask build freshness check                    1s
#   eight xtask gates, one binary                  5s
#   CLI journey self-test                          3s    (harness changes)
#   release script self-test                       2s    (release changes)
#   desktop svelte-check + vitest                  9s    (desktop changes)
#   ───────────────────────────────────────────────────
#   wall clock = max(cargo check, ~21s) — NOT their sum
#
# The compile check is the one gate that can exceed the budget on its own, and
# what drives that is NOT the size of the diff — it is FREE MEMORY. The script
# derives `--jobs` from it (4GB/job), so the same workspace check measured
# 21.5s at `jobs: 12` (48GB free) and 58.1s at `jobs: 3` (15GB free). Read the
# `jobs:` line on its banner before concluding a run was slow for any other
# reason.
#
# That is exactly why it is started first and collected last instead of run in
# sequence. Whole-hook wall clock, measured 2026-08-30: 22s warm, 27s with
# three of the workspace's most-depended-on crates dirtied. Serial, the
# memory-starved worst case would be 79s and out of budget; overlapped it is
# ~58s and inside it. Nothing after the launch point takes the cargo target
# lock — keep it that way or the concurrency quietly becomes a queue. (The
# xtask build above is the one that would: it ran after the launch in the
# first draft of this and sat on the lock for the whole check.)
#
# The verdict line prints the elapsed total every run, so a gate that creeps
# past the budget shows up as a number rather than as a habit of skipping.
#
# ## Escape hatches
#
# Real ones, because a gate with no escape hatch gets uninstalled:
#
#   git push --no-verify           # skip every hook, one push
#   SOVEREIGN_SKIP_PREPUSH=1 git push
#   SOVEREIGN_PREPUSH_QUICK=1 git push   # skip the desktop node gates
#
# Use them when you mean to (pushing a WIP branch for a colleague to look at,
# racing a hotfix). Do not use them to push red code to main — CI will catch
# it, and now that CI is affordable again it will actually be running.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

# git push exports GIT_DIR (and sometimes GIT_WORK_TREE, GIT_INDEX_FILE,
# GIT_COMMON_DIR, GIT_PREFIX) into the hook environment. Any subprocess
# that spawns git in a temp directory (including the workspace tests
# this hook invokes) will operate on the REAL repository instead of the
# temp one if these leak through — producing fixture commits on the
# branch being pushed ("init", "baseline commit one two three", ...),
# parallel config-lock contention, and spurious test failures. The hook
# itself uses only CWD-based git discovery, so strip them all.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_PREFIX

if [[ -n "${SOVEREIGN_SKIP_PREPUSH:-}" ]]; then
    echo "pre-push: skipped (SOVEREIGN_SKIP_PREPUSH set)" >&2
    exit 0
fi

QUICK="${SOVEREIGN_PREPUSH_QUICK:-}"

# ── Colours (only when attached to a terminal) ─────────────────────────────
if [[ -t 2 ]]; then
    C_RESET=$'\033[0m'; C_DIM=$'\033[2m'; C_RED=$'\033[31m'
    C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_BOLD=$'\033[1m'
else
    C_RESET=; C_DIM=; C_RED=; C_GREEN=; C_YELLOW=; C_BOLD=
fi

say()  { printf '%s\n' "${C_DIM}pre-push:${C_RESET} $*" >&2; }
warn() { printf '%s\n' "${C_YELLOW}pre-push:${C_RESET} $*" >&2; }
fail() { printf '%s\n' "${C_RED}${C_BOLD}pre-push:${C_RESET} $*" >&2; }

# ── Work out what this push actually contains ──────────────────────────────
#
# git feeds a pre-push hook lines of "<local ref> <local sha> <remote ref>
# <remote sha>" on stdin. An all-zero remote sha means the branch is new on
# the remote, in which case "everything since main" is the honest range; an
# all-zero LOCAL sha means a branch deletion, which has no content to check.
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

# Invoked by hand (no stdin from git), or nothing pushable was found.
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

# FAIL CLOSED. `git diff` failing (an unknown sha, a shallow clone, a ref the
# remote pruned) is NOT the same thing as "this push changes nothing", and
# conflating the two makes the gate silently pass exactly when it is least
# sure of itself. On any error, fall back to gating everything.
CHANGED="$(git diff --name-only "$RANGE" 2>/dev/null)"
diff_status=$?
if (( diff_status != 0 )); then
    warn "could not diff ${RANGE} (git exit ${diff_status}) — gating EVERYTHING rather than assuming it is clean"
    CHANGED="$(git ls-files)"
elif [[ -z "$CHANGED" ]]; then
    # Run by hand with nothing unpushed: you almost certainly meant "check what
    # I am working on right now", not "check the empty set and exit 0". Gate the
    # working tree instead. (Never do this for a real push — git already told us
    # exactly what is going out, and uncommitted work is not part of it.)
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
say "gating ${n_changed} changed file(s) in ${C_BOLD}${RANGE}${C_RESET}"

# These filters MIRROR the `changes` job in .github/workflows/ci.yml. If you
# widen one, widen the other — the whole point of this hook is that a green
# local run predicts a green CI run.
match() { printf '%s\n' "$CHANGED" | grep -Eq "$1"; }

RUST=0
DESKTOP=0
JOURNEY=0
RELEASE=0
# The non-.rs entries are TEST INPUTS. `sovereign/docs/cli-contract.toml` is
# read by cli_contract_{code,docs,journeys} and cli_journey_dispatch, so
# editing the manifest alone can turn the suite red without touching a line of
# Rust — and this filter used to say otherwise, which meant a manifest-only
# push sailed through the local gate and went red in CI. That is the precise
# failure this hook exists to prevent, so the filters must stay in step.
#
# RUST now drives only rustfmt here, since the test run moved to CI — but the
# set stays as-is deliberately. It is the CI `changes` job's filter, and its
# job is to predict which CI gates a diff can break; narrowing it to "files
# rustfmt cares about" would make the two lists diverge for a few seconds of
# saving on a non-Rust push.
match '(\.rs$|(^|/)Cargo\.toml$|^Cargo\.lock$|^rust-toolchain\.toml$|^\.cargo/|^vendor/|^scripts/sovereign-test\.sh$|^scripts/lib/|^sovereign/crates/sovereign-tools/src/code/test_adapters/|^sovereign/docs/cli-contract\.toml$|^sovereign/scripts/cli-journey-.*\.sh$|^sovereign/scripts/tests/)' && RUST=1
match '^sovereign/crates/sovereign-desktop/' && DESKTOP=1
# The journey harness's own negative controls: the runner SCRIPT, which no
# cargo test covers. Every path here is also a RUST path above, so the static
# and offline tiers ride the workspace test run and are not re-run here.
match '(^sovereign/docs/cli-contract\.toml$|^sovereign/scripts/cli-journey-.*\.sh$|^sovereign/scripts/tests/|^sovereign/crates/sovereign-cli/tests/cli_(contract|journey))' && JOURNEY=1
# The release drivers, which decide what bytes reach users and which no cargo
# test can reach. Same shape as JOURNEY above: shell that gates a shipping
# decision is itself ungated unless something like this runs it.
# scripts/lib/release-host.sh and the two build-desktop-* drivers are in this
# set for a reason: they are SOURCED or exec'd by the release drivers, so a
# change there changes what a release does while leaving every path this
# pattern used to match untouched — the gate would sit out the one commit
# most able to break a release.
# .containerignore and the desktop Containerfiles join them on the same
# argument. .containerignore decides what podman streams into every container
# build — a dropped line there costs minutes per build and reports nothing (it
# is not a build failure, just a stall), which is exactly why the guard exists.
# The Containerfiles are the single decider of which paths the build genuinely
# needs, so a new COPY is the one change that can turn a lean context into a
# broken build.
# `scripts/tests/` also holds the CI-bench lane-verdict suite, so the two files
# it guards are named here too. Without them, editing the verdict logic would
# not run the only gate on it — which is the "opt-in guard decays into
# decoration" failure that `scripts/tests/run-all.sh`'s own header warns about.
match '(^scripts/release-.*\.sh$|^scripts/tests/|^scripts/lib/release-host\.sh$|^scripts/lib/ci-bench-verdict\.sh$|^scripts/sovereign-ci-bench\.sh$|^scripts/build-desktop-.*\.sh$|^\.containerignore$|/containerfiles/)' && RELEASE=1

FAILED=()        # gates that ran and said no — these block
UNVERIFIED=()    # gates that could not run here — these warn
ADVISORY=()      # gates that ran and could not judge — reported, not blocking

# Is a build break attributable to code in THIS repo?
#
# The distinction is the difference between a gate that protects you and a gate
# that just stands in your way. A rustc diagnostic pointing at a file in this
# workspace is yours: block it. A third-party build script that fell over —
# missing native header, no cmake, a libclang whose resource directory has no
# include/ — is a property of the shell you happen to be pushing FROM, and no
# edit to this push can fix it. This repo's native deps (cmake, clang, vulkan)
# live in the dev toolbox, and a push from the Fedora host dies in
# llama-cpp-sys-4's build script on `stdbool.h` every time. Blocking there
# teaches people that the gate is noise, and a gate people route around
# protects nothing.
#
# FAILS CLOSED: no log, or anything it cannot classify, counts as first-party.
break_is_first_party() {
    local raw="${1:-}"
    [[ -f "$raw" ]] || return 0

    # Any diagnostic pointing outside the registry/toolchain is our source.
    #
    # Collect into a variable rather than testing `grep -qv`'s exit status: on
    # EMPTY input (no diagnostics at all — the exact case we are classifying)
    # GNU grep -v exits 1 but some drop-in replacements exit 0, which silently
    # inverts the verdict. Emptiness of the result is unambiguous everywhere.
    local ours
    ours="$(grep -E '^[[:space:]]*--> ' "$raw" 2>/dev/null \
        | grep -v -e '\.cargo/registry' -e '/rustc/' -e '\.cargo/git' || true)"
    [[ -n "$ours" ]] && return 0

    # Nothing of ours in the log, and a dependency's build script or the
    # native linker is what died.
    if grep -qE 'failed to run custom build command for|error: linking with|cannot find -l' \
        "$raw" 2>/dev/null; then
        return 1
    fi

    return 0
}

run_gate() {
    local label="$1"; shift
    local started=$SECONDS
    printf '%s\n' "${C_DIM}────${C_RESET} ${C_BOLD}${label}${C_RESET}" >&2
    if "$@"; then
        say "${C_GREEN}ok${C_RESET} ${label} ($((SECONDS - started))s)"
    else
        fail "${label} FAILED ($((SECONDS - started))s)"
        FAILED+=("$label")
    fi
}

# Same shape as run_gate, but findings are REPORTED and the push continues.
# For gates that are real and currently in arrears: turning a multi-failure
# wall on at the push is exactly how a gate teaches people to reach for
# --no-verify (see the rustfmt note below). Promote a gate to run_gate the
# day its backlog reaches zero.
warn_gate() {
    local label="$1"; shift
    local started=$SECONDS
    printf '%s\n' "${C_DIM}────${C_RESET} ${C_BOLD}${label}${C_RESET}" >&2
    if "$@"; then
        say "${C_GREEN}ok${C_RESET} ${label} ($((SECONDS - started))s)"
    else
        warn "${label} reported findings ($((SECONDS - started))s) — advisory, push continues"
    fi
}

# One build for all eight, hoisted out of the gate function and run BEFORE
# the lint check starts, because `cargo build` and `cargo check` contend for
# the same target-directory lock: launched after, this would idle for the
# whole lint run waiting on a lock it has no dependency on.
#
# A build failure is its own verdict — it is not "the gates passed" and it is
# not "your code is bad" (ARCH §18.3).
XTASK_BIN="${REPO_ROOT}/target/debug/xtask"
XTASK_BUILT=0
if cargo build --quiet -p xtask 2>/dev/null; then
    XTASK_BUILT=1
fi

# ── Gate 1: rustfmt. Instant, deterministic, cannot flake. ─────────────────
#
# A rustfmt failure is mechanical, zero-risk, and always fixed by exactly one
# command. `cargo fmt --check` answers it by printing every hunk it would
# change — fifteen files of that, followed by "PUSH BLOCKED", makes a
# three-second problem read like a crisis and teaches people to reach for
# --no-verify. So report WHICH files and THE fix, and keep the diff for anyone
# who actually wants to look at it.
fmt_gate() {
    local out files
    out="$(cargo fmt --all --check 2>&1)" && return 0

    files="$(printf '%s\n' "$out" \
        | sed -n 's/^Diff in //p' | sed 's/:[0-9][0-9]*:*$//' \
        | sed "s#^${REPO_ROOT}/##" \
        | sort -u)"

    if [[ -z "$files" ]]; then
        # Not a formatting diff — rustfmt itself failed (parse error, missing
        # component). Show it verbatim; the file list would be a lie.
        printf '%s\n' "$out" >&2
        return 1
    fi

    printf '%s\n' "  ${C_BOLD}$(printf '%s\n' "$files" | wc -l | tr -d ' ') file(s) need formatting:${C_RESET}" >&2
    printf '%s\n' "$files" | sed 's/^/    /' >&2
    printf '\n%s\n\n' "  ${C_BOLD}fix:${C_RESET} cargo fmt --all   ${C_DIM}(then re-push; nothing else to do)${C_RESET}" >&2
    return 1
}

if (( RUST )); then
    run_gate "rustfmt" fmt_gate
fi

# ── Gate 1b: does it COMPILE. Started here, collected last. ────────────────
#
# `scripts/sovereign-lint.sh` is `cargo check --all-targets` under the repo's
# real feature contract (corpus-engine/treesitter + sovereign-cli/dev-tools +
# sovereign-mesh/mesh-sim), which is the coverage the workspace test run used
# to provide incidentally and nothing replaced when it left. Its own header
# names a pre-push hook as where the full sweep belongs.
#
# IT RUNS CONCURRENTLY, and that is what makes it affordable. Measured on this
# host 2026-08-30: 11.3s when nothing needs recompiling, 58.1s right after a
# `cargo fmt` sweep touched oicp-client, corpus-engine and sovereign-inference
# — three crates most of the workspace depends on. Serial, that worst case put
# the hook at 79s and straight through the budget. Overlapped with the ~21s of
# gates that need no cargo lock, the hook costs max(lint, the rest) instead of
# their sum, and the observed worst case lands at 58s — inside the minute.
#
# Ordering is load-bearing, not incidental:
#   * the xtask build above is already done, so it never waits on this lock;
#   * `cargo fmt` does not build, so it does not contend;
#   * the journey and desktop gates are shell and node.
# Nothing below this line takes the cargo target lock. Keep it that way, or
# the concurrency silently becomes a queue.
#
# Scope comes from the push range rather than the working tree: what is going
# out is the honest subject, and `SOVEREIGN_CHANGED_PATHS` is the script's
# highest-priority discovery path. Workspace-level files escalate it to a full
# check on their own, and a range that resolves to no crate falls back to the
# full workspace rather than checking nothing — it fails toward more coverage.
LINT_PID=""
LINT_LOG=""
if (( RUST )); then
    LINT_LOG="$(mktemp -t prepush-lint.XXXXXX)"
    printf '%s\n' "${C_DIM}────${C_RESET} ${C_BOLD}cargo check (sovereign-lint.sh)${C_RESET} ${C_DIM}— running alongside the gates below${C_RESET}" >&2
    SOVEREIGN_CHANGED_PATHS="$(printf '%s\n' "$CHANGED" | tr '\n' ':')" \
        ./scripts/sovereign-lint.sh --human >"$LINT_LOG" 2>&1 &
    LINT_PID=$!
    # A hook that is interrupted must not leave a cargo check holding the
    # target lock against the next thing the developer types.
    trap '[[ -n "${LINT_PID:-}" ]] && kill "$LINT_PID" 2>/dev/null; [[ -n "${LINT_LOG:-}" ]] && rm -f "$LINT_LOG"' EXIT INT TERM
fi

# ── Gate 2: the xtask gates. BLOCKING. ────────────────────────────────────
#
# docs-gate is the first of them: every repo path cited by the narrative docs
# must resolve on disk, checked on ANY change rather than only on doc edits,
# because the usual way it breaks is a CODE file renamed out from under a
# citation. It used to run as its own `cargo run --quiet -p xtask -- docs-gate`
# above this block, which re-resolved freshness over 56 workspace crates
# before invoking a gate that finishes in 0.4s. It is the same binary as the
# other seven; running it through the same loop costs one process, not one
# cargo.
#
# docs-gate was ONE of ten. The rest ran only when somebody typed
# `cargo xtask quality` — not in CI (ci.yml's `gates:` job has been commented
# out since the day it was written, deliberately: docs/CI_ECONOMY.md argues
# the real gate is local), not here, and not in AGENTS.md's definition of
# done. Measured 2026-08-27: six of eight enforcing gates were failing and
# nothing surfaced it. boundary-gate is documented "NOT advisory" and had no
# caller at all.
#
# Wiring them here makes them structural instead of remembered (ARCH §7; the
# ten's #10). Re-measured 2026-08-28: FOUR of ten were failing, not six of
# eight — arch-gate (87 size violations), docs-gate (one stale citation), plus
# api-gate and lint-gate, which are tooling/usage errors rather than code
# failures and are not push gates at all. docs-gate is fixed; arch-gate is
# baselined at 170 oversized files and now blocks regressions.
#
# BLOCKING since 2026-08-28, and the two changes are one decision. Advisory
# WITH SUPPRESSED OUTPUT is indistinguishable from passing to anyone reading
# quickly, which is exactly how six failures sat unseen for a month. A gate
# nobody consults is not a gate (ARCH §18.1) — so it either blocks and shows
# its findings, or it should be deleted.
#
# The budget that makes blocking tolerable: ONE xtask build, then the BINARY
# per gate. `cargo run` re-resolves freshness over 56 workspace crates and
# costs ~5.4s per invocation against a gate that runs in 0.04s — so ~38s of
# the old ~70s was cargo, not gates. Measured 2026-08-28 on this host:
#   concept 15.1s · arch 6.3s · env 4.2s · layout 3.9s · boundary/layer/lock <0.1s
#   = 29.6s for all seven, plus one build.
# If that ever creeps, concept-gate is 51% of it and REFACTOR_LEDGER.md
# already specifies its content-hash cache.
#
# api-gate and lint-gate are deliberately NOT here. api-gate needs a pinned
# nightly plus `cargo install cargo-public-api` (a CI concern, and it burns
# 15.7s failing to find the binary); lint-gate consumes a clippy JSON stream
# and belongs to the lint script that produces one. Neither is a push gate.
XTASK_GATES=(docs-gate arch-gate boundary-gate layer-gate lock-gate layout-gate env-gate concept-gate)

xtask_gates() {
    local g rc=0 code out xtask
    local -a failed=() advisory=()

    if (( ! XTASK_BUILT )); then
        printf '%s\n' "  ${C_BOLD}xtask failed to build${C_RESET} — the gates did not run" >&2
        printf '\n%s\n\n' "  ${C_BOLD}see why:${C_RESET} cargo build -p xtask" >&2
        return 1
    fi
    xtask="$XTASK_BIN"

    for g in "${XTASK_GATES[@]}"; do
        # Per-gate capture: on failure show THAT gate's findings only. Eight
        # gates' full output at every push is the crisis-wall shape the
        # rustfmt note above warns about — but zero output was worse.
        out="$("$xtask" "$g" 2>&1)"; code=$?
        (( code == 0 )) && continue

        # FOUR VERDICTS, NOT TWO (ARCH §18.2). concept-gate does not count
        # anything itself — it relays `svrn code converge status`, which
        # answers 0 pass, 1 a duplicate was ADDED, 3 the graph cannot speak
        # for this commit, 4 never ran. Only 1 is about this push. 3 and 4 are
        # properties of the INDEXER (it lags HEAD by design; here it was 59
        # source files behind), and concept_gate.rs's own module doc calls
        # itself advisory in a habit-run for exactly this reason: "failing a
        # pre-push run for an indexer that is eight minutes behind is the
        # false-positive machine that gets a gate switched off inside a week."
        # Report it, name the re-index, do not block. CI and the landing
        # verdict call converge directly, where the graph IS authoritative.
        if [[ "$g" == "concept-gate" ]] && (( code == 3 || code == 4 )); then
            advisory+=("$g")
            printf '\n%s\n' "  ${C_BOLD}${g}${C_RESET} ${C_DIM}(advisory — the graph cannot judge this commit)${C_RESET}" >&2
            printf '%s\n' "$out" | grep -E 'COULD-NOT-JUDGE|NEVER-RAN|re-index:|indexed ' | head -3 | sed 's/^/    /' >&2
            continue
        fi

        failed+=("$g"); rc=1
        printf '\n%s\n' "  ${C_BOLD}${g}${C_RESET}" >&2
        printf '%s\n' "$out" | grep -E '✗|FAIL|^error' | head -6 | sed 's/^/    /' >&2
    done
    if (( ${#advisory[@]} )); then
        ADVISORY+=("${advisory[@]}")
    fi
    if (( rc )); then
        printf '\n%s\n' "  ${C_BOLD}${#failed[@]} of ${#XTASK_GATES[@]} failing:${C_RESET} ${failed[*]}" >&2
        printf '\n%s\n\n' "  ${C_BOLD}full output:${C_RESET} cargo xtask quality   ${C_DIM}(each gate's output ends with its own fix command)${C_RESET}" >&2
    fi
    return $rc
}

run_gate "xtask gates (docs/arch/boundary/layer/lock/layout/env/concept)" xtask_gates

# ── The size term. ADVISORY on purpose, for now. ──────────────────────────
#
# Every gate above answers "is this correct?", and correctness is monotone:
# no amount of added code can make a passing test fail. So "done" has never
# had a size term, and the workspace runs about +622k / -179k over 90 days
# (quality/DELETION.md) — 29 lines deleted per 100 added. These two print the
# missing number at every push.
#
# They are warn_gate, not run_gate, and the reason is this file's own rule:
# a gate in arrears, or one whose false-positive rate nobody has measured
# yet, is how people learn to reach for --no-verify. size-gate's arrears are
# zero today but it has never run across a real week of pushes; the deletion
# ratchet is genuinely in arrears (two lanes grew since the freeze). Promote
# each to run_gate on its own evidence:
#   size-gate      — after a week of pushes with no false positive.
#   deletion       — the day `--verify` reports no lane growing.
# Raising ONE crate's ceiling is `xtask size-gate --accept <crate>`; never
# --update-baseline on a working tree, which absorbs everyone else's growth.
size_gate() {
    (( XTASK_BUILT )) || return 0
    "$XTASK_BIN" size-gate
}
warn_gate "size ratchet (code lines per crate)" size_gate
warn_gate "deletion manifest ratchet" python3 "${REPO_ROOT}/scripts/deletion-manifest.py" --verify

# The awareness layer had no gate at all, which is why its own suite rotted:
# `scripts/tests/run-all.sh` is gated below, `.claude/hooks/tests/run-all.sh`
# was gated by nothing, and it sits at 16 failures at HEAD (inject-boot-block
# 11, inject-budget 3, inject-notes 2 — seat-sidecar cases whose subject was
# removed from the hook while its tests stayed). Nothing surfaced that, so
# nothing surfaced the injector defects underneath it either: there was no
# green to break.
#
# DIFF-SCOPED because the suite is ~50s and this file holds a one-minute
# budget — free on the pushes that do not touch a hook, present on the ones
# that do. warn_gate for the reason above it: a 16-failure wall at the push is
# how a gate teaches people to reach for --no-verify.
# Promote to run_gate the day the backlog reaches zero.
if match '^\.claude/hooks/'; then
    warn_gate "hook suites (.claude/hooks/tests — 16 known failures)" \
        bash "${REPO_ROOT}/.claude/hooks/tests/run-all.sh"
fi

# ── Gate 3: NOT HERE. The workspace test suite runs in CI, not at the push. ─
#
# Removed 2026-08-30 on operator direction, and the reasoning is the header's:
# a one-minute budget with a 45-60s warm test run in it is a budget that is
# always over, and a gate that is always over is a gate you always skip. The
# tests did not get weaker — they moved to the authority that can afford them.
#
#   here          the checks a diff cannot answer, every push
#   ./scripts/sovereign-test.sh --human   when you mean to, before a push
#   CI            every push, clean checkout, no warm target to flatter it
#
# "Does it COMPILE" did not go with it. That coverage was incidental to the
# test run rather than the point of it, and losing it would mean a push could
# leave this machine without building — so Gate 1b above runs
# `scripts/sovereign-lint.sh` (cargo check --all-targets under the repo's real
# feature contract) concurrently for a fraction of the cost. What is genuinely
# gone is behaviour: nothing here now runs a single test.

# ── Gate 4: the journey harness's own negative controls. ───────────────────
#
# Tiers 1 and 2 of the journey harness are Rust tests and ride Gate 3. Tier 3
# is a shell RUNNER, and no cargo test can reach it — so without this gate the
# thing that decides whether every journey passed or failed is itself ungated.
#
# That is not hypothetical. The runner this replaced, cli-contract-live-verify.sh,
# was written, documented as "safe to call unconditionally in CI", and then
# never called once: SOVEREIGN_LIVE_CONTRACT appears nowhere in the repo except
# inside the script that reads it. An opt-in guard decays into decoration, and
# a harness nobody has seen FAIL is not evidence of anything.
#
# Cost is a few seconds: a stub binary and a loopback stub daemon, no models,
# no real daemon, nothing written outside a mktemp dir (ARCH_PRINCIPLES §12.4).
if (( JOURNEY )); then
    if command -v python3 >/dev/null 2>&1; then
        run_gate "CLI journey self-test (harness negative controls)" \
            sovereign/scripts/tests/cli-journey-selftest.sh
    else
        # The stub daemon is a python3 one-liner. No python3 is a property of
        # this shell, not of the push — same reasoning as the third-party build
        # break above: block only on what an edit here could actually fix.
        warn "python3 not found — cannot run the journey self-test (its stub daemon needs it)"
        UNVERIFIED+=("CLI journey self-test")
    fi
else
    say "no journey-harness changes — skipping journey self-test"
fi

# ── Gate 4b: the release drivers' own negative controls. ───────────────────
#
# scripts/release-*.sh decide which bytes reach users, and no cargo test can
# reach them. Both defects these suites cover shipped silently for weeks and
# were found only by a release that happened to trip them:
#
#   • the upload list is a GLOB over an uncleaned dist/, so a leftover tarball
#     ships under today's tag with a filename and checksum that both verify.
#     desktop-v0.3.5 actually did this (0.3.3 payload under a 0.3.5 name);
#     cli-v0.5.0 came one build leg from repeating it with Jul-29 binaries.
#   • a stopped podman VM was reported as "container image missing", sending
#     you to rebuild a 3.3GB image that was already present.
#   • every driver hard-gated on `uname -sm == "Darwin arm64"`, so the x86_64
#     Linux workstation could not cut the three legs it builds NATIVELY while
#     the Mac emulates them. The capability suite asserts the split both ways:
#     a Linux host announces the Apple skip and never calls xcrun, and a Mac
#     host does NOT get auto-skipped into shipping a release with no .dmg.
#
# Seconds to run: `gh` and `podman` are stubbed, nothing is uploaded or
# started, everything happens in a mktemp dir.
if (( RELEASE )); then
    run_gate "release script self-test (provenance + podman diagnosis + host capability)" \
        scripts/tests/run-all.sh
else
    say "no release-driver changes — skipping release script self-test"
fi

# ── Gate 5: the desktop webview surface, which cargo is blind to. ──────────
if (( DESKTOP )); then
    if [[ -n "$QUICK" ]]; then
        warn "SKIPPING desktop gates (SOVEREIGN_PREPUSH_QUICK set) — CI will run them"
    else
        desktop_dir="sovereign/crates/sovereign-desktop"
        if [[ -d "$desktop_dir/node_modules" ]]; then
            run_gate "desktop svelte-check" npm --prefix "$desktop_dir" run check
            run_gate "desktop vitest"       npm --prefix "$desktop_dir" run test
        else
            warn "$desktop_dir/node_modules missing — run 'npm ci' there; skipping desktop gates"
        fi
    fi
else
    say "no desktop changes — skipping svelte-check / vitest"
fi

# ── Collect Gate 1b, started before everything above. ──────────────────────
if [[ -n "$LINT_PID" ]]; then
    wait "$LINT_PID"; lint_rc=$?
    LINT_PID=""
    printf '%s\n' "${C_DIM}────${C_RESET} ${C_BOLD}cargo check (sovereign-lint.sh)${C_RESET}" >&2
    if (( lint_rc == 0 )); then
        # The banner names the scope it actually checked, so a scoped clean run
        # cannot be read as a workspace-wide guarantee. Show it either way.
        grep -E '^ (scope|features|jobs|errors|warnings|elapsed):' "$LINT_LOG" >&2 || true
        say "${C_GREEN}ok${C_RESET} cargo check"
    elif break_is_first_party "${REPO_ROOT}/target/sovereign-lint/latest/cargo.raw.log"; then
        cat "$LINT_LOG" >&2
        fail "cargo check FAILED"
        FAILED+=("cargo check (sovereign-lint.sh)")
    else
        # Not this push's fault, and not fixable by editing this push: a
        # dependency's build script died with no first-party diagnostic. CI
        # compiles on a clean checkout and is the right authority for "does
        # this build somewhere that isn't your laptop." (ARCH §18.3 — the
        # absence of a verdict is REPORTED, never defaulted to a pass.)
        warn "${C_BOLD}cargo check could not RUN in this shell${C_RESET} — a third-party build"
        warn "script failed and no first-party diagnostic was emitted. Not blocking:"
        warn "nothing in this push can fix a native toolchain that isn't installed here."
        warn "  what broke:  $(sed -n 's/^error: failed to run custom build command for `\(.*\)`.*/\1/p' \
            "${REPO_ROOT}/target/sovereign-lint/latest/cargo.raw.log" 2>/dev/null | head -1 || true)"
        warn "  full log:    target/sovereign-lint/latest/cargo.raw.log"
        warn "  this repo's native deps (cmake, clang, vulkan) live in the dev toolbox —"
        warn "  push from there, or run ./scripts/sovereign-lint.sh inside it, to gate for real."
        UNVERIFIED+=("cargo check")
    fi
    rm -f "$LINT_LOG"; LINT_LOG=""
fi

# ── Verdict ───────────────────────────────────────────────────────────────
echo >&2
if (( ${#FAILED[@]} )); then
    fail "PUSH BLOCKED — ${#FAILED[@]} gate(s) failed in ${SECONDS}s:"
    for g in "${FAILED[@]}"; do printf '           - %s\n' "$g" >&2; done
    cat >&2 <<EOF

  Each gate's findings are above, and each ends with its own fix command.

  To push anyway:  git push --no-verify
EOF
    exit 1
fi

# Honest bookkeeping: neither of these is the claim "all gates passed", and
# saying so would be the sort of green light that makes a gate worthless
# (ARCH §18.2 — four verdicts, not two).
if (( ${#ADVISORY[@]} )); then
    warn "${C_YELLOW}${#ADVISORY[@]} gate(s) COULD NOT JUDGE this commit${C_RESET} — reported, not blocking:"
    for g in "${ADVISORY[@]}"; do printf '           - %s\n' "$g" >&2; done
fi

# "All gates passed" is a stronger claim than "nothing failed", and printing
# the first one directly under a COULD-NOT-JUDGE line would unsay the warning.
VERDICT="all gates passed"
(( ${#ADVISORY[@]} + ${#UNVERIFIED[@]} )) && VERDICT="no gate failed"

if (( ${#UNVERIFIED[@]} )); then
    warn "${C_YELLOW}pushing with ${#UNVERIFIED[@]} gate(s) UNVERIFIED here${C_RESET} — CI is the authority for:"
    for g in "${UNVERIFIED[@]}"; do printf '           - %s\n' "$g" >&2; done
    say "${C_GREEN}${VERDICT}${C_RESET} — pushing (${SECONDS}s)"
    exit 0
fi

# The budget is the header's, and it is checkable here rather than remembered.
if (( SECONDS > 60 )); then
    warn "${C_YELLOW}this run took ${SECONDS}s — over the 60s budget.${C_RESET} A gate that costs"
    warn "more than a minute gets skipped, which is worse than a cheaper one. Trim it."
fi

say "${C_GREEN}${VERDICT}${C_RESET} — pushing (${SECONDS}s)"
exit 0
