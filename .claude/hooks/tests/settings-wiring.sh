#!/usr/bin/env bash
# settings-wiring.sh — .claude/settings.json's hook commands are WELL-FORMED.
#
# WHY THIS EXISTS, and why it is not folded into the other suites here.
#
# One entry has been deleted from settings.json seven times and has come back
# eight: a SECOND registration of inject-notes.py whose command is the
# relative `python3 .claude/hooks/inject-notes.py`. The moment a session's
# shell cwd is not the repo root — which is every session that cd's anywhere —
# the interpreter cannot open the file and the turn dies with
#
#   Python: can't open file '/private/tmp/.claude/hooks/inject-notes.py'
#
# surfaced to the operator as "UserPromptSubmit operation blocked by hook".
# Missed notifications and stalled workflows are the same bug.
#
# IT IS NOT A MISTAKE ANYONE KEEPS MAKING. The entry is the ORIGINAL, present
# since 5ab17057a (2026-05-10). Every fix removes it from main; every branch
# and clone forked before that fix still carries it; a JSON merge takes the
# UNION. So it returns on merges and grab-bags (#52, #55, #59, "stuff", "save
# work", "setting") and never on a deliberate edit. Fixing the file a ninth
# time buys nothing — only a gate that fails the MERGE does.
#
# `hook-selftests` could not catch it: that instrument is `advisory`, runs
# only in `ci:suites`, and sits behind 16 known failures. This is registered
# separately, hard and at prepush, for exactly that reason.
#
#   bash .claude/hooks/tests/settings-wiring.sh              # check the repo
#   bash .claude/hooks/tests/settings-wiring.sh --self-test  # + planted bad
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"

check() {   # check <file> -> prints findings, returns 1 if any
python3 - "$1" <<'PY'
import json, sys, collections
path = sys.argv[1]
try:
    with open(path, encoding="utf-8") as fh:
        cfg = json.load(fh)
except (OSError, ValueError) as e:
    print(f"  UNREADABLE {path}: {e}"); raise SystemExit(2)

bad, seen = [], collections.Counter()
for event, groups in (cfg.get("hooks") or {}).items():
    for group in groups:
        for h in group.get("hooks", []):
            cmd = h.get("command", "")
            if not cmd:
                continue
            script = next((t for t in cmd.split()
                           if t.endswith((".py", ".sh")) or "/hooks/" in t), "")
            key = (event, script.rsplit("/", 1)[-1].strip('"'))
            if key[1]:
                seen[key] += 1
            # A hook command runs with the SESSION's cwd, which no hook
            # controls. Anchored or absolute are the only two safe forms.
            if script and not (script.startswith("/")
                               or "$CLAUDE_PROJECT_DIR" in cmd
                               or "${CLAUDE_PROJECT_DIR" in cmd):
                bad.append(f"  RELATIVE  {event}: {cmd}")
for (event, script), n in sorted(seen.items()):
    if n > 1:
        bad.append(f"  DUPLICATE {event}: {script} registered {n} times")
for b in bad:
    print(b)
raise SystemExit(1 if bad else 0)
PY
}

rc=0
echo "settings-wiring: $REPO/.claude/settings.json"
if check "$REPO/.claude/settings.json"; then
    echo "  ok — every hook command is \$CLAUDE_PROJECT_DIR-anchored or absolute, none registered twice"
else
    rc=1
fi

if [ "${1:-}" = "--self-test" ]; then
    # The negative control. A gate nobody has watched fail is not a gate
    # (ARCH §18.1), and this one's failing input is the exact blob that keeps
    # coming back.
    tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
    cat > "$tmp/bad.json" <<'JSON'
{"hooks": {"UserPromptSubmit": [
  {"hooks": [{"type": "command",
              "command": "python3 \"$CLAUDE_PROJECT_DIR\"/.claude/hooks/inject-notes.py"}]},
  {"hooks": [{"type": "command",
              "command": "python3 .claude/hooks/inject-notes.py"}]}]}}
JSON
    echo "settings-wiring --self-test: the planted historical regression"
    if check "$tmp/bad.json"; then
        echo "  MISBEHAVED: the planted duplicate+relative blob passed"; rc=1
    else
        echo "  ok — planted blob rejected (both findings above are expected)"
    fi
    printf '{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"python3 \\"$CLAUDE_PROJECT_DIR\\"/.claude/hooks/inject-notes.py"}]}]}}\n' > "$tmp/good.json"
    if check "$tmp/good.json"; then
        echo "  ok — a correctly wired blob passes"
    else
        echo "  MISBEHAVED: a clean blob was rejected"; rc=1
    fi
fi
exit $rc
