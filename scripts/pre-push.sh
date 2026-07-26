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
# ## Why it is affordable to run every time
#
# It scopes to what the push actually changes, and it reuses your warm
# `target/`. `scripts/sovereign-test.sh` runs on cargo-nextest — 59s for the
# full workspace against 126s for serial `cargo test`. A push touching only
# docs skips the Rust work entirely and costs about a second.
#
# ## Escape hatches
#
# Real ones, because a gate with no escape hatch gets uninstalled:
#
#   git push --no-verify           # skip every hook, one push
#   SOVEREIGN_SKIP_PREPUSH=1 git push
#   SOVEREIGN_PREPUSH_QUICK=1 git push   # fmt + docs only; no test run
#
# Use them when you mean to (pushing a WIP branch for a colleague to look at,
# racing a hotfix). Do not use them to push red code to main — CI will catch
# it, and now that CI is affordable again it will actually be running.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

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
match '(\.rs$|(^|/)Cargo\.toml$|^Cargo\.lock$|^rust-toolchain\.toml$|^\.cargo/|^vendor/|^scripts/sovereign-test\.sh$|^scripts/lib/)' && RUST=1
match '^sovereign/crates/sovereign-desktop/' && DESKTOP=1

FAILED=()        # gates that ran and said no — these block
UNVERIFIED=()    # gates that could not run here — these warn
BUILD_BROKE=0

# Is a build break attributable to code in THIS repo?
#
# The distinction is the difference between a gate that protects you and a gate
# that just stands in your way. A rustc diagnostic pointing at a file in this
# workspace is yours: block it. A third-party build script that fell over —
# missing native header, no cmake, a libclang whose resource directory has no
# include/ — is a property of the shell you happen to be pushing FROM, and no
# edit to this push can fix it. Blocking there teaches people that the gate is
# noise, and a gate people route around protects nothing.
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

# ── Gate 2: docs-gate. Every repo path cited by the narrative docs must ────
# resolve on disk. Runs on ANY change, not just doc edits: the usual way this
# breaks is a CODE file being renamed out from under a citation.
run_gate "docs-gate (cited paths resolve)" cargo run --quiet -p xtask -- docs-gate

# ── Gate 3: the workspace test suite. The expensive one. ───────────────────
if (( RUST )); then
    if [[ -n "$QUICK" ]]; then
        warn "SKIPPING workspace tests (SOVEREIGN_PREPUSH_QUICK set) — CI will run them"
    else
        run_gate "workspace tests (sovereign-test.sh)" ./scripts/sovereign-test.sh --human

        # "The tests ran and some failed" and "nothing ran because the
        # workspace did not compile" are different problems with different
        # fixes, and the second one is very often not about your code at all —
        # a toolbox that lost a dnf package, a stale bindgen, a linker that
        # moved. Reporting it as a TEST failure sends you hunting through test
        # code for a missing header. sovereign-test.sh already knows the
        # difference (cargo exited non-zero, zero tests parsed); read it.
        counts="target/sovereign-test/latest/counts.env"
        cargo_exit="target/sovereign-test/latest/cargo.exit"
        if [[ -f "$counts" && -f "$cargo_exit" ]]; then
            tp="$(sed -n 's/^total_pass=//p' "$counts")"
            tf="$(sed -n 's/^total_fail=//p' "$counts")"
            ce="$(tr -d '[:space:]' < "$cargo_exit")"
            if [[ "${tp:-0}" == "0" && "${tf:-0}" == "0" && "${ce:-0}" != "0" ]]; then
                if break_is_first_party "target/sovereign-test/latest/cargo.raw.log"; then
                    BUILD_BROKE=1
                else
                    # Not this push's fault, and not fixable by editing this
                    # push. Downgrade to a warning and let it through — CI
                    # compiles on a clean checkout and is the right authority
                    # for "does this build somewhere that isn't your laptop."
                    unset 'FAILED[-1]'
                    warn "${C_BOLD}workspace tests could not RUN in this shell${C_RESET} — a third-party build"
                    warn "script failed and no first-party diagnostic was emitted. Not blocking:"
                    warn "nothing in this push can fix a native toolchain that isn't installed here."
                    warn "  what broke:  $(sed -n 's/^error: failed to run custom build command for `\(.*\)`.*/\1/p' \
                        target/sovereign-test/latest/cargo.raw.log 2>/dev/null | head -1 || true)"
                    warn "  full log:    target/sovereign-test/latest/cargo.raw.log"
                    warn "  this repo's native deps (cmake, clang, vulkan) live in the dev toolbox —"
                    warn "  push from there, or run ./scripts/sovereign-test.sh inside it, to gate for real."
                    UNVERIFIED+=("workspace tests")
                fi
            fi
        fi
    fi
else
    say "no Rust changes — skipping workspace tests"
fi

# ── Gate 4: the desktop webview surface, which cargo is blind to. ──────────
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

# ── Verdict ───────────────────────────────────────────────────────────────
echo >&2
if (( ${#FAILED[@]} )); then
    fail "PUSH BLOCKED — ${#FAILED[@]} gate(s) failed:"
    for g in "${FAILED[@]}"; do printf '           - %s\n' "$g" >&2; done
    if (( BUILD_BROKE )); then
        cat >&2 <<EOF

  ${C_BOLD}The workspace did not COMPILE — zero tests ran.${C_RESET} That is a build
  problem, not a test failure, and the diagnostic points at source in this
  repo, so it is this push's to fix. The compiler error is the last thing in:

      target/sovereign-test/latest/cargo.raw.log

  (A build break coming from a DEPENDENCY's build script is treated as
  environmental and only warns — see break_is_first_party in this script.)
EOF
    else
        cat >&2 <<EOF

  Failures are listed above; adapter logs for triage are under
  target/sovereign-test/latest/ (cargo.jsonl, cargo.raw.log).

  To push anyway:  git push --no-verify
EOF
    fi
    exit 1
fi

if (( ${#UNVERIFIED[@]} )); then
    # Honest bookkeeping: this is NOT the same claim as "all gates passed", and
    # saying so would be the sort of green light that makes a gate worthless.
    warn "${C_YELLOW}pushing with ${#UNVERIFIED[@]} gate(s) UNVERIFIED here${C_RESET} — CI is the authority for:"
    for g in "${UNVERIFIED[@]}"; do printf '           - %s\n' "$g" >&2; done
    exit 0
fi

say "${C_GREEN}all gates passed${C_RESET} — pushing"
exit 0
