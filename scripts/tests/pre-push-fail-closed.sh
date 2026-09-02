#!/usr/bin/env bash
# Negative controls for the fail-closed range derivation in scripts/pre-push.sh.
#
# WHY THIS EXISTS. The hook is the PRIMARY correctness gate for this repo — CI
# is the safety net, not the authority (see the pre-push header). What the hook
# gates is derived from one command:
#
#     CHANGED="$(git diff --name-only "$RANGE" 2>/dev/null)"
#
# and `git diff` failing is NOT the same fact as "this push changes nothing".
# A shallow clone, a rebased branch, a ref the remote pruned, or a sha the
# local object store has never seen all make it exit non-zero with no output.
# Conflate the two and the gate passes green, in silence, on a push that could
# contain anything — and it does so exactly when it is least sure of itself.
#
# The fallback at pre-push.sh:143-151 gates `git ls-files` instead. It has
# never been exercised by anything: no cargo test can reach a shell script's
# subprocess boundary, and the hook's own green runs only ever take the happy
# path (ARCH §18.1 — a gate you have not watched fail is not a gate).
#
# FOUR CASES, not one. A fallback that gated everything unconditionally would
# also "pass" the fail-closed case while destroying the hook's scoping, so the
# suite pins both directions: a broken range gates EVERY tracked file, a valid
# empty range gates NOTHING, and a valid non-empty range gates exactly its own
# diff — smaller than the tracked set.
#
# Runs the REAL hook in a mktemp repo, driving its stdin the way `git push`
# does. No cargo, no network, nothing written outside the temp dir (ARCH §12.4).
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
REAL_HOOK="$ROOT/scripts/pre-push.sh"
[[ -f "$REAL_HOOK" ]] || { echo "cannot find $REAL_HOOK"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/root/scripts"
cp "$REAL_HOOK" "$T/root/scripts/pre-push.sh"

cd "$T/root" || exit 2
git init -q .
git config user.email t@t
git config user.name t

# Two TRACKED files, and neither matches any of the hook's gate filters — so
# whatever it decides to gate, no cargo/npm/journey gate is triggered by the
# file names themselves and the run stays cheap. The hook itself is left
# untracked so `git ls-files` has a known, exact answer: 2.
echo alpha > alpha.txt
git add alpha.txt && git commit -q -m "one"
C1="$(git rev-parse HEAD)"
echo beta > beta.txt
git add beta.txt && git commit -q -m "two"
C2="$(git rev-parse HEAD)"

# A sha no object store here has ever seen — the shallow-clone / pruned-ref
# shape, reproduced without needing either.
UNKNOWN="1111111111111111111111111111111111111111"

# The hook's later gates cannot run in a bare temp repo (no Cargo.toml, so the
# xtask build fails immediately) and that is fine — every assertion below is
# about the RANGE derivation, which happens before any of them. `timeout`
# distinguishes a hang from a slow pass; rc=124 is a failure, not patience.
if command -v timeout >/dev/null 2>&1; then bounded() { timeout 90 "$@"; }
else bounded() { "$@"; }; fi

rc=0

# run_case <name> <remote_sha> <local_sha> <expect: gated|clean> <needle>
run_case() {
    local name="$1" remote="$2" local_sha="$3" expect="$4" needle="$5" out code
    out="$(printf 'refs/heads/main %s refs/heads/main %s\n' "$local_sha" "$remote" \
        | bounded bash scripts/pre-push.sh 2>&1)"
    code=$?
    if (( code == 124 )); then
        echo "  FAIL  $name — the hook hung (90s)"
        rc=1
        return
    fi
    if grep -q -- "$needle" <<<"$out"; then
        echo "  ok    $name"
    else
        echo "  FAIL  $name — wanted stderr matching '$needle'"
        sed 's/^/          /' <<<"$out" | grep -E 'gating|nothing to gate|could not diff' | head -4
        rc=1
    fi
    # A "clean" case must also have said so and stopped, not gated something.
    if [[ "$expect" == clean ]] && grep -qE 'gating [0-9]+ changed file' <<<"$out"; then
        echo "  FAIL  $name — gated files on a range that legitimately has none"
        rc=1
    fi
}

echo "pre-push-fail-closed:"

# 1. THE POINT. An undiffable range gates every tracked file, and says why.
run_case "an undiffable range gates EVERYTHING" \
    "$UNKNOWN" "$C2" gated "could not diff"
run_case "an undiffable range gates all 2 tracked files, not 0" \
    "$UNKNOWN" "$C2" gated "gating 2 changed file(s)"

# 2. The fallback is a FALLBACK: a valid range that really is empty still
#    exits without gating anything. Without this, a hook hard-wired to gate
#    `git ls-files` would pass case 1 while throwing the scoping away.
run_case "a valid empty range gates nothing" \
    "$C2" "$C2" clean "nothing to gate"

# 3. And a valid non-empty range gates its OWN diff — one file, not the two
#    the tracked set holds.
run_case "a valid range gates exactly its diff" \
    "$C1" "$C2" gated "gating 1 changed file(s)"

exit "$rc"
