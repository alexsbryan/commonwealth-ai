#!/usr/bin/env bash
# Mark a ralph row done and commit the queue: PROMPT step 7 as one verb.
#
#   scripts/ralph-mark.sh <unit-id> <short-sha> [state-file]
#
# Rewrites `- [~] <unit-id> — depends` (or `- [ ]`) to `- [x] <unit-id> <sha> — depends`
# in the state file, stages that file alone, and commits `ralph: <unit-id> done`.
# The state file is the third argument, else $RALPH_STATE (scripts/ralph.py
# exports it into every worker session), else a refusal: the default used to be
# ring-doc's queue, so a two-argument call from any other queue could not find
# its row (ring-room's PROMPT.md:67 still makes that call). Why a script: under the claude
# harness a python heredoc never matches an allow rule, so every row mark
# asked the operator (log-permissions.txt 23:49:58Z) — seated, seconds;
# unattended, a 600 s timeout, a deny, and a stall on the last step of a unit
# that already landed.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

unit="${1:-}"; sha="${2:-}"; state="${3:-${RALPH_STATE:-}}"
[ -n "$unit" ] && [ -n "$sha" ] || { echo "usage: scripts/ralph-mark.sh <unit-id> <short-sha> [state-file]" >&2; exit 2; }
[ -n "$state" ] || { echo "ralph-mark: no state file — pass it as the third argument, or run under scripts/ralph.py, which sets RALPH_STATE" >&2; exit 2; }
git cat-file -e "${sha}^{commit}" 2>/dev/null || { echo "ralph-mark: $sha is not a commit in this repo" >&2; exit 2; }

n=$(grep -c -E "^- \[[~ ]\] ${unit} — depends" "$state" || true)
[ "$n" = "1" ] || { echo "ralph-mark: expected one open row for $unit in $state, found $n" >&2; exit 3; }
# python, not `sed -i -E`: BSD sed reads `-E` as the BACKUP SUFFIX, so on macOS
# the edit landed and left a `STATE.md-E` beside it on every mark (2026-09-19).
python3 - "$state" "$unit" "$sha" <<'PYEOF'
import re, sys
path, unit, sha = sys.argv[1:4]
text = open(path, encoding="utf-8").read()
new, n = re.subn(rf"(?m)^- \[[~ ]\] {re.escape(unit)} — depends", f"- [x] {unit} {sha} — depends", text)
if n != 1:
    sys.exit(f"ralph-mark: rewrite matched {n} rows")
open(path, "w", encoding="utf-8").write(new)
PYEOF
grep -q -E "^- \[x\] ${unit} ${sha} — depends" "$state" || { echo "ralph-mark: rewrite did not land" >&2; exit 3; }
git add -- "$state"
git commit -q -m "ralph: ${unit} done" -- "$state"
echo "marked ${unit} ${sha}: $(git log --oneline -1)"
