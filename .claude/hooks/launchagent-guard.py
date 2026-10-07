#!/usr/bin/env python3
"""PreToolUse (Bash, Write) hook: an agent never creates a plist in
~/Library/LaunchAgents.

WHY. launchd loads every plist in ~/Library/LaunchAgents at each login, so a
one-shot written there (RunAtLoad, KeepAlive=false) runs when it is
bootstrapped and again at every login after. At the 2026-10-05 06:49 reboot,
six oplog one-shots from 2026-09-13 fired before the daemon was up, and each
overwrote its committed record in quality/report-audit/ with a could-not-judge
run. 30 more finished one-shots sat in the same directory.

WHAT. Refused: a Write to a path under Library/LaunchAgents/ that does not
exist yet, and a Bash command whose write redirect, `tee`, or
cp/mv/ln/install/ditto destination would create one. Allowed: rewriting a
plist already there (the daemon's agent is tuned in place), reading, rm,
moving a plist out, and launchctl itself (bootstrapping the daemon's agent is
how it restarts).

REMEDY, named in the refusal: the same plist in any other directory, loaded by
path. launchd scans LaunchAgents only at login, so a plist bootstrapped from
elsewhere runs once and is gone at logout. A login-time agent is installed by
its own setup script, or by the operator.

NOT A SECURITY BOUNDARY. A script, eval, or a python one-liner hides the
write. This stops the hand-written plist.

Exit 2 refuses (the harness withholds the call and shows stderr). A command
that will not lex exits 0 and names the skip in additionalContext.
"""
from __future__ import annotations

import json
import os
import re
import sys
from pathlib import PurePath

from bash_lex import ASSIGN_RX, REDIRECTS, SHELLS, WRAPPERS, WRITE_REDIRECTS, simple_commands

AGENTS_DIR = "Library/LaunchAgents"
COPIERS = {"cp", "mv", "ln", "install", "ditto"}
VAR_RX = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)")


def envelope() -> dict:
    raw = os.environ.get("SOVEREIGN_HOOK_INPUT")
    try:
        return json.loads(raw) if raw else json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return {}


def expand(word: str, shell_vars: dict) -> str:
    """$NAME and ${NAME} from an earlier assignment in the command, else the
    environment, then ~. An unknown variable stays literal, so its path does
    not exist and a destination built on it counts as new."""
    def sub(m):
        name = m.group(1) or m.group(2)
        return shell_vars.get(name, os.environ.get(name, m.group(0)))
    return os.path.expanduser(VAR_RX.sub(sub, word))


def new_agent(dest: str, sources: list[str]) -> str | None:
    """The plist under LaunchAgents that writing to `dest` would create, or
    None when `dest` is elsewhere or every target already exists."""
    if AGENTS_DIR not in dest:
        return None
    into_dir = dest.endswith("/") or os.path.isdir(dest)
    targets = ([os.path.join(dest, os.path.basename(s.rstrip("/"))) for s in sources]
               if into_dir else [dest])
    return next((t for t in targets if not os.path.exists(t)), None)


def judge_bash(cmd: str, depth: int = 0) -> str | None:
    shell_vars: dict = {}
    for seg in simple_commands(cmd):
        if all(ASSIGN_RX.match(t) for t in seg):
            for t in seg:                                  # P=...; on its own
                name, value = ASSIGN_RX.match(t).groups()
                shell_vars[name] = expand(value, shell_vars)
            continue
        words, j = [], 0
        while j < len(seg):
            tok = seg[j]
            if tok in REDIRECTS:
                if tok in WRITE_REDIRECTS and j + 1 < len(seg):
                    hit = new_agent(expand(seg[j + 1], shell_vars), [])
                    if hit:
                        return hit
                j += 2
                continue
            if tok.isdigit() and j + 1 < len(seg) and seg[j + 1] in REDIRECTS:
                j += 1                                     # the 2 of 2>
                continue
            words.append(tok)
            j += 1
        i = 0
        while i < len(words) and (words[i] in WRAPPERS or words[i] == "env"
                                  or ASSIGN_RX.match(words[i])):
            i += 1
        if i >= len(words):
            continue
        prog, args = PurePath(words[i]).name, words[i + 1:]
        if prog in SHELLS and "-c" in args and depth < 3:
            k = args.index("-c") + 1
            hit = judge_bash(args[k], depth + 1) if k < len(args) else None
            if hit:
                return hit
            continue
        paths = [expand(a, shell_vars) for a in args if not a.startswith("-")]
        if prog == "tee":
            hit = next((h for h in (new_agent(p, []) for p in paths) if h), None)
        elif prog in COPIERS and len(paths) >= 2:
            hit = new_agent(paths[-1], paths[:-1])
        else:
            hit = None
        if hit:
            return hit
    return None


def refusal(path: str) -> str:
    home = os.path.expanduser("~")
    shown = "~" + path[len(home):] if path.startswith(home + "/") else path
    name = os.path.basename(path)
    return (f"this creates {shown}. launchd loads every plist in ~/Library/LaunchAgents "
            f"at each login, so a one-shot written there runs again at every login "
            f"after (2026-10-05: six oplog one-shots re-fired at boot and overwrote "
            f"their committed records). Write the same plist outside it, beside the "
            f"run's script under runs/ or in your scratchpad, and load it by path: "
            f"`launchctl bootstrap gui/$(id -u) <dir>/{name}`. It runs once and is "
            f"gone at logout. A login-time agent is installed by its own setup "
            f"script or by the operator: hand them the command.")


def main() -> int:
    env = envelope()
    tool = env.get("tool_name", "")
    tin = env.get("tool_input") or {}
    if tool == "Bash":
        try:
            hit = judge_bash(str(tin.get("command", "")))
        except ValueError as e:
            print(json.dumps({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": f"launchagent-guard skipped: command did not lex ({e})"}}))
            return 0
    elif tool == "Write":
        hit = new_agent(os.path.expanduser(str(tin.get("file_path", ""))), [])
    else:
        return 0
    if hit:
        print(f"launchagent-guard: {refusal(hit)}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
