#!/usr/bin/env python3
"""Render `claude -p --output-format stream-json --verbose` into a ralph iter log.

stdin: one JSON event per line. stdout: assistant text, `$ cmd` per Bash
call, other tools by name, tool results trimmed to head+tail, every refusal
as an `auto-rejecting` line (the literal scripts/ralph.py counts to raise its
"permission auto-rejections" warning), and one `=== result:` line last.
Non-JSON lines pass through untouched.
"""
import json
import sys

# Matched against a tool_result marked is_error at the moment of refusal, so
# the count survives a session the supervisor kills before its result event.
# The result event's permission_denials is rendered as a summary only, so one
# refusal is one `auto-rejecting` line.
REFUSAL_MARKS = (
    "has been denied",
    "requested permissions",
    "permission denied",
    "not allowed",
    "has not been granted",
    "haven't granted",
)


def text_of(content):
    if isinstance(content, list):
        return "\n".join(x.get("text", "") for x in content if isinstance(x, dict))
    return content or ""


def brief(inp):
    return {k: (v if len(str(v)) < 120 else str(v)[:117] + "...") for k, v in inp.items()}


def main():
    for line in sys.stdin:
        try:
            ev = json.loads(line)
        except ValueError:
            sys.stdout.write(line)
            continue
        kind = ev.get("type")
        if kind == "system" and ev.get("subtype") == "init":
            servers = ", ".join(f"{s.get('name')}={s.get('status')}" for s in ev.get("mcp_servers") or [])
            print(f"=== session {ev.get('session_id')} model={ev.get('model')} "
                  f"permissionMode={ev.get('permissionMode')} mcp[{servers}]")
        elif kind == "assistant":
            for b in ev.get("message", {}).get("content", []):
                if b.get("type") == "text" and b.get("text", "").strip():
                    print(b["text"].rstrip())
                elif b.get("type") == "tool_use":
                    name, inp = b.get("name"), b.get("input", {}) or {}
                    if name == "Bash":
                        print("$ " + str(inp.get("command", "")).rstrip())
                    else:
                        print(f"tool {name} {json.dumps(brief(inp))}")
        elif kind == "user":
            for b in ev.get("message", {}).get("content", []):
                if b.get("type") != "tool_result":
                    continue
                body = text_of(b.get("content")).rstrip()
                low = body.lower()
                if b.get("is_error") and any(m in low for m in REFUSAL_MARKS):
                    first = body.splitlines()[0] if body else ""
                    print("auto-rejecting: " + first[:200])
                    continue
                lines = body.splitlines()
                if len(lines) > 24:
                    lines = lines[:12] + ["  ..."] + lines[-12:]
                if lines:
                    print("\n".join(lines))
        elif kind == "result":
            denials = ev.get("permission_denials") or []
            for d in denials:
                inp = d.get("tool_input", {})
                what = inp.get("command") if isinstance(inp, dict) and "command" in inp else json.dumps(inp)[:200]
                print(f"  denied: {d.get('tool_name')} {what}")
            print(
                f"=== result: {ev.get('subtype')} turns={ev.get('num_turns')} "
                f"cost_usd={ev.get('total_cost_usd')} duration_ms={ev.get('duration_ms')} "
                f"denials={len(denials)}"
            )
            if ev.get("is_error"):
                print(ev.get("result", ""))
        sys.stdout.flush()


if __name__ == "__main__":
    main()
