#!/usr/bin/env bash
# The freeze on the slice that moved to corpus-engine-core: scripts/commit-msg.sh
# refuses a commit that changes a path on quality/frozen-paths.txt unless its
# message carries a `Frozen-path:` trailer (scripts/frozen-paths.py).
#
# Cases 1-4 drive the REAL commit-msg.sh and frozen-paths.py, copied into a
# temp repository with a two-line list, so the refusal is watched, not assumed.
# Case 5 reads the repository's own list: every entry is a tracked file, and
# none is shared infrastructure the port only copied (the grounding gate the
# chat pipeline uses, the daemon's server.rs, sovereign-contracts), so a
# refresh that widened MOVED by mistake fails here before it freezes the
# platform.
#
# No cargo, no daemon, no network; nothing written outside the temp dir.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
rc=0
pass() { echo "  ok    $1"; }
flunk() { echo "  FAIL  $1 — $2"; rc=1; }

R="$T/repo"
mkdir -p "$R/scripts" "$R/quality" "$R/frozen" "$R/open"
git -C "$R" init -q -b main
git -C "$R" config user.email t@example.invalid
git -C "$R" config user.name t
cp "$ROOT/scripts/commit-msg.sh" "$ROOT/scripts/frozen-paths.py" "$R/scripts/"
printf '# test list\nfrozen/a.rs\nfrozen/b.rs\n' > "$R/quality/frozen-paths.txt"
echo 1 > "$R/frozen/a.rs"; echo 1 > "$R/frozen/b.rs"; echo 1 > "$R/open/c.rs"
git -C "$R" add -A && git -C "$R" commit -q -m init

# hook <message> <file to change>: stage a change to the file and run the hook
# on the message; prints the hook's stderr, returns its exit code.
hook() {
    echo "$RANDOM" >> "$R/$2"
    git -C "$R" add "$2"
    printf '%s\n' "$1" > "$T/msg"
    (cd "$R" && scripts/commit-msg.sh "$T/msg" 2> "$T/err")
    local code=$?
    git -C "$R" reset -q HEAD -- "$2"; git -C "$R" checkout -q -- "$2"
    return "$code"
}

if hook "fix: tune the frozen file" frozen/a.rs; then
    flunk "a frozen file without the trailer is refused" "the hook exited 0"
elif grep -q "frozen/a.rs" "$T/err" && grep -q "corpus-engine-core" "$T/err"; then
    pass "a frozen file without the trailer is refused, naming the file and where the work goes"
else
    flunk "the refusal names the file and corpus-engine-core" "$(cat "$T/err")"
fi

if hook "$(printf 'fix: backport\n\nFrozen-path: backport of corpus-engine-core abc1234')" frozen/b.rs; then
    pass "a Frozen-path: trailer with a reason lets it through"
else
    flunk "a Frozen-path: trailer with a reason lets it through" "$(cat "$T/err")"
fi

if hook "$(printf 'fix: x\n\nFrozen-path:')" frozen/a.rs; then
    flunk "an empty Frozen-path: trailer is refused" "the hook exited 0"
else
    pass "an empty Frozen-path: trailer is refused"
fi

if hook "feat: open work" open/c.rs; then
    pass "a file off the list commits as before"
else
    flunk "a file off the list commits as before" "$(cat "$T/err")"
fi

list="$ROOT/quality/frozen-paths.txt"
untracked=$(grep -v '^#' "$list" | while read -r p; do
    [ -n "$p" ] && ! git -C "$ROOT" ls-files --error-unmatch -- "$p" >/dev/null 2>&1 && echo "$p"; done)
shared=$(grep -E 'runtime/grounding|sovereign-daemon/src/server\.rs|sovereign-contracts|kernel-types|host-kit|oicp-client' "$list")
if [ -n "$untracked" ]; then
    flunk "every frozen path is a tracked file" "$(echo "$untracked" | head -3)"
elif [ -n "$shared" ]; then
    flunk "no shared infrastructure is frozen" "$(echo "$shared" | head -3)"
else
    pass "the list holds $(grep -vc '^#' "$list") tracked files and no shared infrastructure"
fi

exit "$rc"
