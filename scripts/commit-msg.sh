#!/usr/bin/env bash
# commit-msg.sh — refuse a commit whose message is its parent's, verbatim.
#
# Workers commit with `git commit -F <file>`, so one skipped rewrite of the
# file labels a change with the previous commit's description, and nothing
# downstream can tell. b415f1c1d is the case: fe5c0c00b's "rustfmt the four
# daemon files" over a 23-line scripts/cw-work-lift.sh change. Blocking,
# unlike pre-commit.sh: the check is exact equality, not a soft signal.
#
# Merges are exempt (git writes their message). `--amend` keeping its message
# is refused too, since a hook cannot tell it apart; pass --no-verify there.
# Runnable by hand: ./scripts/commit-msg.sh <message-file>
set -uo pipefail

[ -e "$(git rev-parse --git-path MERGE_HEAD)" ] && exit 0
norm() { git stripspace --strip-comments; }
prev=$(git log -1 --format=%B HEAD 2>/dev/null | norm) || exit 0
[ -n "$prev" ] || exit 0
if [ "$(norm < "$1")" = "$prev" ]; then
    echo "commit-msg: this message is HEAD's ($(git rev-parse --short HEAD)) verbatim." >&2
    echo "commit-msg: a stale message file? Write the message for THIS change." >&2
    exit 1
fi
exit 0
