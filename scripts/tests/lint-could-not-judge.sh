#!/usr/bin/env bash
# The third verdict `scripts/sovereign-lint.sh` can give, and the negative
# controls that make it worth anything.
#
# WHY THIS EXISTS. A cargo failure with no first-party diagnostic and a
# third-party build-script or link error in it means the check could not RUN on
# this host — the native toolchain (cmake, clang, vulkan) lives in the dev
# toolbox — and nothing in the diff can fix that. Reporting it as a FAILURE
# blocks a push over a machine's setup; reporting it as a pass is the
# substitution ARCH §18.3 forbids. So it is exit 3, and the registry declares
# `could_not_judge_exits = [3]` on both lint rows.
#
# ARCH §18.2, amended: the two verdicts that make no claim are OWED, not free.
# An abstention nobody has watched be necessary is not rigor, it is a hole. The
# logic here lived in `scripts/pre-push.sh::break_is_first_party` from
# 2026-07-28 to 2026-09-07 and was never exercised by anything: this host has
# the toolchain, so the happy path is the only one its green runs ever took.
# Four cases below, and the two that must NOT abstain are the point — a
# predicate that answers "could not judge" to everything would pass a
# one-sided test while turning the compile gate off.
#
# It drives the predicate the way the script does, against raw logs that are
# the real shapes: a llama-cpp-sys-4 build-script failure with no `-->` from
# our tree, and a plain rustc error from ours.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
LINT="$ROOT/scripts/sovereign-lint.sh"
[[ -f "$LINT" ]] || { echo "cannot find $LINT"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

# The predicate, lifted from the script by name so this test cannot drift from
# it silently: if the function is renamed or deleted, extraction fails loudly
# rather than testing a copy that no longer exists.
sed -n '/^could_not_judge() {$/,/^}$/p' "$LINT" > "$T/pred.sh"
if [[ ! -s "$T/pred.sh" ]]; then
    echo "  FAIL  could_not_judge() is not in $LINT any more — this suite is testing nothing"
    exit 1
fi
# shellcheck disable=SC1090
. "$T/pred.sh"

rc=0
check() {  # check <name> <expect: abstain|judge> <log body>
    local name="$1" expect="$2" body="$3"
    raw_log="$T/cargo.raw.log"
    printf '%s\n' "$body" > "$raw_log"
    if could_not_judge; then got=abstain; else got=judge; fi
    if [[ "$got" == "$expect" ]]; then
        echo "  ok    $name ($got)"
    else
        echo "  FAIL  $name — wanted $expect, got $got"
        rc=1
    fi
}

echo "lint-could-not-judge:"

# 1. THE POINT. A third-party build script died and our tree was never
#    compiled. Nothing in any diff can fix this.
check "a third-party build-script failure with no first-party diagnostic abstains" abstain \
"error: failed to run custom build command for \`llama-cpp-sys-4 v0.1.0\`
Caused by:
  process didn't exit successfully: \`/tmp/build-script-build\` (exit status: 101)
  --- stderr
  fatal error: 'stdbool.h' file not found"

# 2. A link failure with no first-party diagnostic is the same fact.
check "a link failure with no first-party diagnostic abstains" abstain \
"error: linking with \`cc\` failed: exit status: 1
  = note: ld: library not found for -lvulkan"

# 3. THE NEGATIVE CONTROL THAT MATTERS. The same third-party failure, but our
#    tree DID produce a diagnostic — so the check ran and judged, and abstaining
#    here would turn the compile gate into a rubber stamp on every push that
#    happens to touch a crate with a native dep.
check "a build-script failure alongside OUR error is a judgement, not an abstention" judge \
"error[E0308]: mismatched types
  --> sovereign/crates/sovereign-core/src/lib.rs:41:9
error: failed to run custom build command for \`llama-cpp-sys-4 v0.1.0\`"

# 4. An ordinary compile error from our tree, alone.
check "an ordinary first-party error is a judgement" judge \
"error[E0425]: cannot find value \`x\` in this scope
  --> corpus-engine/src/facts.rs:12:5"

# 5. A diagnostic from a REGISTRY crate is not ours — a dependency's own
#    warning pointing into ~/.cargo must not make us claim we judged this tree.
check "a diagnostic pointing only into .cargo/registry is not first-party" abstain \
"error: failed to run custom build command for \`llama-cpp-sys-4 v0.1.0\`
warning: unused import
  --> /Users/x/.cargo/registry/src/index.crates.io-abc/foo-1.0/src/lib.rs:3:5"

exit "$rc"
