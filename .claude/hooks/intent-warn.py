#!/usr/bin/env python3
"""PreToolUse advisory: the argument that produced a fn, shown as it is about to change.

THE MOMENT. An Edit or Write whose old/new text names a fn. This is the one
moment the intent model exists for (order: .sovereign/features/intent-model):
the person, or the session, is about to change code, and the reasoning that
produced it — claim, objection, concession, and whether the evidence it cited
actually saw the change — should be in front of them without being asked for.
Not a rule in a markdown file they may not have read.

WHAT IT DOES. Collects the fn names in the edit, asks the notes store for
decisions tagged with those symbols (the records scripts/intent.py writes),
and prints their first lines. Nothing else: no ranking, no model, no gate.

It also prints the canon rules that name the file or identifiers being edited,
ratified or proposed; scripts/canon-ratify.py --lookup decides which, and logs
each proposed rule it shows so the operator ratifies the ones work actually hits.

NEVER BLOCKS. Always exit 0; daemon down or no envelope means silence.
Harness-neutral: JSON envelope on stdin, like every script in this directory.
"""
import json
import os
import re
import subprocess
import sys
import urllib.request

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
FN = re.compile(r"\bfn\s+([a-z_][a-z0-9_]*)\s*[(<]")
MAX_NOTES = 3
MAX_LINES = 8
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def canon_rules(payload, ti, text):
    script = os.path.join(ROOT, "scripts", "canon-ratify.py")
    if not os.path.exists(script):
        return
    try:
        out = subprocess.run(
            [sys.executable, script, "--lookup", "--root", ROOT,
             "--file", str(ti.get("file_path") or ""), "--session", str(payload.get("session_id") or "")],
            input=text, capture_output=True, text=True, timeout=5).stdout
    except Exception:
        return
    if out.strip():
        print(out.rstrip())


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    if payload.get("tool_name") not in ("Edit", "Write"):
        return
    ti = payload.get("tool_input") or {}
    text = "\n".join(str(ti.get(k) or "") for k in ("old_string", "new_string", "content"))
    canon_rules(payload, ti, text)
    names = sorted(set(FN.findall(text)))
    if not names:
        return
    req = urllib.request.Request(
        f"http://localhost:{PORT}/mcp",
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": "notes",
                                    "arguments": {"symbols": names, "kinds": ["decision"],
                                                  "limit": MAX_NOTES}}}).encode(),
        headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=2) as resp:
            inner = json.loads(json.load(resp)["result"]["content"][0]["text"])
    except Exception:
        return
    notes = [n for n in (inner.get("notes") or []) if (n.get("content") or "").startswith("INTENT ·")]
    if not notes:
        return
    print(f"## Intent on record for {', '.join(names)} (hook: intent-warn) — read before editing")
    for n in notes[:MAX_NOTES]:
        lines = [l for l in n["content"].splitlines() if l.strip()][:MAX_LINES]
        print("\n".join(lines))
        print(f"  (note {n.get('id', '')[:8]})")


if __name__ == "__main__":
    main()
    sys.exit(0)
