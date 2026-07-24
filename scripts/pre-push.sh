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
if [[ -z "$RANGE" ]]; then
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
    say "no file changes in ${RANGE} — nothing to gate"
    exit 0
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

FAILED=()
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
if (( RUST )); then
    run_gate "rustfmt" cargo fmt --all --check
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
    cat >&2 <<EOF

  Test failures are listed above; adapter logs for triage are under
  target/sovereign-test/latest/ (cargo.jsonl, cargo.raw.log).

  To push anyway:  git push --no-verify
EOF
    exit 1
fi

say "${C_GREEN}all gates passed${C_RESET} — pushing"
exit 0
