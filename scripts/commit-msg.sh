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
msg=$(norm < "$1")
[ -n "$msg" ] || exit 0
# The last RECENT commits, not only HEAD: a commit from another session can
# land between the one whose file went stale and the one that reuses it.
# 2f946c1fd reused 881446702's message with the seat's 355b19374 between them.
RECENT=10
for sha in $(git rev-list -n "$RECENT" HEAD 2>/dev/null); do
    if [ "$(git log -1 --format=%B "$sha" | norm)" = "$msg" ]; then
        echo "commit-msg: this message is $(git rev-parse --short "$sha")'s verbatim (one of the last $RECENT commits)." >&2
        echo "commit-msg: a stale message file? Write the message for THIS change." >&2
        exit 1
    fi
done
exit 0
