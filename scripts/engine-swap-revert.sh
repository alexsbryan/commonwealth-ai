#!/bin/sh
# Put the embedded engine back: reverse engine-swap milestone G, the delete,
# in the current tree and index. G is everything the queue landed between
# es-e2-flip-default and the native-build demo; the soak runs after it, so this
# is the soak's rollback (operator, 2026-10-26: "make the revert easy"), and
# es-g-revert-drill rehearses it. ralph/ is left alone: the queue keeps its
# record. Commit the result, set `[engine] kind = "llama"`, rebuild.
#
#   scripts/engine-swap-revert.sh [queue-ref]   (default origin/eutropia/engine-swap)
set -eu
queue=${1:-origin/eutropia/engine-swap}
# eutropia writes one "eutropia: <row> done" commit per landed row.
landed() { git log --format=%H -1 --grep="^eutropia: $1 done\$" "$queue"; }
pre=$(landed es-e2-flip-default)
post=$(landed REVIEW-DEMO-es-native-host-build)
if [ -z "$pre" ] || [ -z "$post" ]; then
  echo "engine-swap-revert: G has not landed on $queue (es-e2: ${pre:-none}, demo: ${post:-none})" >&2
  exit 2
fi
echo "engine-swap-revert: reversing G, ${pre%"${pre#?????????}"}..${post%"${post#?????????}"}, outside ralph/" >&2
git diff --binary "$post" "$pre" -- . ':(exclude)ralph' | git apply -3 --index
git diff --cached --shortstat
echo "engine-swap-revert: applied; commit it, set [engine] kind = \"llama\", rebuild the workspace" >&2
