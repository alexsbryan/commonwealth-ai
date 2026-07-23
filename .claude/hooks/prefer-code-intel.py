#!/usr/bin/env python3
"""PreToolUse advisory: nudge agents toward the code-intelligence / RAG path.

WHY. Audited sessions on this repo acquire codebase understanding almost
entirely through raw file reads (Read / `cat` / `grep`) and make ~zero
`symbols`/`callers`/`code_search`/`notes` calls. Every raw read then rides the
cache-read tail for the rest of the session (see `sovereign cache-audit`). This
hook makes the wrong path a little more resistant by surfacing the distilled
path at the moment the raw-acquisition *pattern* emerges.

BEHAVIOUR. Non-blocking (never denies a tool). Counts raw-source-acquisition
events per session and injects a single advisory once the pattern is clear (the
Nth such event), then stays quiet so it never nags. Registered via a PreToolUse
matcher on Read / Grep / Bash in .claude/settings.json — inert until then.

Contract: reads the hook payload as JSON on stdin, emits (on exit 0) a JSON
object with hookSpecificOutput.additionalContext to add advice without blocking.
"""

import json
import os
import sys
from pathlib import Path

# Nudge once the raw-acquisition pattern is established, not on the first
# incidental read. Fires exactly once per session (at this Nth event).
NUDGE_AT = 3

SOURCE_EXTS = (
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java",
    ".c", ".cc", ".cpp", ".h", ".hpp", ".svelte", ".rb",
)
BASH_READ_TOKENS = ("cat ", "head ", "tail ", "sed -n", "grep ", "rg ", "less ")

ADVICE = (
    "You've been acquiring source context via raw reads. On this repo the "
    "code-intelligence / RAG path is cheaper and exact: `symbols(\"Name\")` for "
    "a definition, `callers(\"fn\")` / `callees(\"fn\")` for the call graph "
    "(compiler-resolved via SCIP — catches dispatch grep misses), "
    "`code_search(\"...\")` for concepts, `notes(query:\"...\")` for prior "
    "decisions/invariants. Every whole-file Read rides the cache-read tail for "
    "the rest of the session — prefer a distilled query, or Read a tight "
    "15-25 line slice around a `symbols` hit. (`sovereign cache-audit` shows "
    "your own spend.)"
)


def is_raw_source_acquisition(tool_name: str, tool_input: dict) -> bool:
    if tool_name == "Grep":
        return True
    if tool_name == "Read":
        fp = str(tool_input.get("file_path", ""))
        return fp.endswith(SOURCE_EXTS)
    if tool_name == "Bash":
        cmd = str(tool_input.get("command", ""))
        return any(tok in cmd for tok in BASH_READ_TOKENS)
    return False


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return 0  # never block on a parse problem

    tool_name = data.get("tool_name", "")
    tool_input = data.get("tool_input", {}) or {}
    session_id = str(data.get("session_id", "unknown"))

    if not is_raw_source_acquisition(tool_name, tool_input):
        return 0

    # Per-session counter of raw-acquisition events. session_id is stable for
    # the life of a session; a stray file just accumulates and is swept below.
    state_dir = Path(os.path.expanduser("~/.cache/sovereign/code-intel-nudge"))
    try:
        state_dir.mkdir(parents=True, exist_ok=True)
    except OSError:
        return 0
    counter = state_dir / f"{session_id}.count"
    fired = state_dir / f"{session_id}.fired"

    if fired.exists():
        return 0  # already advised this session

    n = 0
    try:
        n = int(counter.read_text().strip() or "0")
    except (OSError, ValueError):
        n = 0
    n += 1
    try:
        counter.write_text(str(n))
    except OSError:
        pass

    if n < NUDGE_AT:
        return 0

    try:
        fired.touch()
    except OSError:
        pass

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "additionalContext": ADVICE,
        }
    }))
    return 0


if __name__ == "__main__":
    sys.exit(main())
