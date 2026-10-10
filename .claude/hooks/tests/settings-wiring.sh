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
# The same merge resurrects a hook DELETED on main (intent-warn.py, removed
# 2026-10-10), and then `python3 <missing file>` exits 2, which blocks every
# Edit. So an anchored command naming a script that does not exist fails too.
#
#   bash .claude/hooks/tests/settings-wiring.sh              # check the repo
#   bash .claude/hooks/tests/settings-wiring.sh --self-test  # + planted bad
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"

check() {   # check <file> -> prints findings, returns 1 if any
python3 - "$1" "$REPO" <<'PY'
import json, os, re, sys, collections
path, repo = sys.argv[1], sys.argv[2]
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
            # A deleted hook comes back the same way: a branch forked before
            # the deletion merges its settings entry back in. `python3 <gone>`
            # exits 2, and exit 2 from PreToolUse BLOCKS the tool, so a missing
            # script is a broken session, not a stale line.
            local = re.sub(r'"?\$\{?CLAUDE_PROJECT_DIR\}?"?', repo, script).strip('"')
            if script and local.startswith(repo) and not os.path.exists(local):
                bad.append(f"  MISSING   {event}: {local[len(repo) + 1:]} does not exist")
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
    echo "  ok — every hook command is \$CLAUDE_PROJECT_DIR-anchored or absolute, names a script that exists, none registered twice"
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
        echo "  ok — planted blob rejected (the findings above are expected)"
    fi
    printf '{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"python3 \\"$CLAUDE_PROJECT_DIR\\"/.claude/hooks/no-such-hook.py"}]}]}}\n' > "$tmp/missing.json"
    echo "settings-wiring --self-test: an anchored entry whose script was deleted"
    if check "$tmp/missing.json"; then
        echo "  MISBEHAVED: an entry naming a deleted script passed"; rc=1
    else
        echo "  ok — missing script rejected (the finding above is expected)"
    fi
    printf '{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"sh \\"$CLAUDE_PROJECT_DIR\\"/.claude/hooks/session-boot.sh"}]}]}}\n' > "$tmp/good.json"
    if check "$tmp/good.json"; then
        echo "  ok — a correctly wired blob passes"
    else
        echo "  MISBEHAVED: a clean blob was rejected"; rc=1
    fi
fi
exit $rc
