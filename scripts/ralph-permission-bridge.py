#!/usr/bin/env python3
"""The ralph worker's permission prompt, bubbled to the operator.

A stdio MCP server with one tool, `approve`, which `claude -p` calls (via
`--permission-prompt-tool mcp__ralph__approve`) for every tool use that the
settings file neither allows nor denies. Hard rules in
ralph/claude-settings.json `deny` never reach here; the allowlist never
reaches here; only the gray zone does.

On a call it writes ralph/PERMISSION_REQUEST.md, sends a desktop
notification, and waits for ralph/PERMISSION_ANSWER whose first word is:

    allow           run it this once
    always          run it, and append an exact-match rule to the settings
                    file so it never asks again (generalise the rule by hand)
    deny [reason]   refuse; the reason reaches the worker

No answer within RALPH_PERMISSION_WAIT_SECS (default 600) is a deny that says
so — a worker is never wedged past that bound, and a timeout costs wall
clock, not tokens. Every decision is one line in ralph/log-permissions.txt.

Wire format: JSON-RPC over stdin/stdout, one message per line (MCP stdio).
The tool returns a text block holding the permission result JSON:
{"behavior":"allow","updatedInput":<input>} or {"behavior":"deny","message":..}.
"""
import json
import os
import pathlib
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent.parent
RALPH = HERE / "ralph"
REQUEST = RALPH / "PERMISSION_REQUEST.md"
ANSWER = RALPH / "PERMISSION_ANSWER"
LOG = RALPH / "log-permissions.txt"
SETTINGS = pathlib.Path(os.environ.get("RALPH_CLAUDE_SETTINGS", HERE / "ralph" / "claude-settings.json"))
WAIT_SECS = int(os.environ.get("RALPH_PERMISSION_WAIT_SECS", "600"))
POLL_SECS = 2


def log(line):
    with open(LOG, "a") as fh:
        fh.write(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {line}\n")


def notify(title, body):
    for cmd in (["notify-send", "-u", "critical", f"ralph: {title}", body],
                ["/usr/bin/osascript", "-e", f'display notification "{body}" with title "ralph: {title}"']):
        try:
            subprocess.run(cmd, capture_output=True, timeout=10)
            return
        except (OSError, subprocess.SubprocessError):
            continue


def describe(tool_name, tool_input):
    if tool_name == "Bash" and isinstance(tool_input, dict):
        return str(tool_input.get("command", ""))
    return json.dumps(tool_input, indent=1)[:2000]


def add_always_rule(tool_name, tool_input):
    """Exact-match rule for this one call; the operator generalises by hand."""
    if tool_name == "Bash" and isinstance(tool_input, dict):
        rule = f"Bash({tool_input.get('command', '')})"
    else:
        rule = tool_name
    try:
        data = json.loads(SETTINGS.read_text())
        allow = data.setdefault("permissions", {}).setdefault("allow", [])
        if rule not in allow:
            allow.append(rule)
            SETTINGS.write_text(json.dumps(data, indent=2) + "\n")
        return rule
    except (OSError, ValueError) as e:
        log(f"always: could not edit {SETTINGS}: {e}")
        return None


def ask_operator(tool_name, tool_input):
    what = describe(tool_name, tool_input)
    try:
        ANSWER.unlink()
    except FileNotFoundError:
        pass
    REQUEST.write_text(
        f"# ralph worker asks permission\n\n"
        f"tool: {tool_name}\nat: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n"
        f"waits until: {time.strftime('%H:%M:%SZ', time.gmtime(time.time() + WAIT_SECS))} (then denied)\n\n"
        f"```\n{what}\n```\n\n"
        f"Answer with one of (first word is read):\n"
        f"  echo allow  > {ANSWER.relative_to(HERE)}\n"
        f"  echo always > {ANSWER.relative_to(HERE)}   # also appends an exact rule to {SETTINGS.relative_to(HERE)}\n"
        f"  echo 'deny <reason>' > {ANSWER.relative_to(HERE)}\n"
    )
    notify("permission?", (what[:120] + ("..." if len(what) > 120 else "")))
    log(f"ask {tool_name}: {what[:300]!r}")
    deadline = time.time() + WAIT_SECS
    while time.time() < deadline:
        if ANSWER.exists():
            text = ANSWER.read_text().strip()
            try:
                ANSWER.unlink()
                REQUEST.unlink()
            except FileNotFoundError:
                pass
            word, _, rest = text.partition(" ")
            word = word.lower()
            if word in ("allow", "y", "yes"):
                log(f"allow {tool_name}")
                return {"behavior": "allow", "updatedInput": tool_input}
            if word == "always":
                rule = add_always_rule(tool_name, tool_input)
                log(f"always {tool_name} -> rule {rule}")
                return {"behavior": "allow", "updatedInput": tool_input}
            reason = rest.strip() or "denied by the operator"
            log(f"deny {tool_name}: {reason}")
            return {"behavior": "deny", "message": f"Permission denied by the operator: {reason}"}
        time.sleep(POLL_SECS)
    try:
        REQUEST.unlink()
    except FileNotFoundError:
        pass
    log(f"timeout {tool_name}: no answer in {WAIT_SECS}s")
    return {"behavior": "deny",
            "message": f"Permission request not answered by the operator within {WAIT_SECS}s; "
                       f"treat as denied and say so in your report."}


TOOL = {
    "name": "approve",
    "description": "Ask the operator whether the worker may use a tool; blocks until answered or timed out.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "tool_name": {"type": "string"},
            "input": {"type": "object"},
            "tool_use_id": {"type": "string"},
        },
        "required": ["tool_name", "input"],
    },
}


def reply(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is not None:
        out["error"] = error
    else:
        out["result"] = result
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        method = msg.get("method")
        msg_id = msg.get("id")
        params = msg.get("params") or {}
        if method == "initialize":
            reply(msg_id, {
                "protocolVersion": params.get("protocolVersion", "2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "ralph", "version": "1"},
            })
        elif method == "tools/list":
            reply(msg_id, {"tools": [TOOL]})
        elif method == "tools/call":
            args = params.get("arguments") or {}
            result = ask_operator(args.get("tool_name", "?"), args.get("input", {}))
            reply(msg_id, {"content": [{"type": "text", "text": json.dumps(result)}]})
        elif method == "ping":
            reply(msg_id, {})
        elif msg_id is not None:
            reply(msg_id, error={"code": -32601, "message": f"unknown method {method}"})
        # notifications (no id) need no reply


if __name__ == "__main__":
    main()
