#!/usr/bin/env python3
"""co_notes.py — shared MCP client for the comaintainer scripts.

One JSON-RPC implementation over the daemon's /mcp surface for the
seat-durable-rail write-throughs (scripts/co-order.sh,
scripts/co-directive-log.sh) and the two-seat conformance drill
(scripts/co-mesh-drill.sh).

WHY WRITES MUST GO THROUGH THE DAEMON: the notes store gossips and
stamps author + node attribution at write time (NodeRoster). A direct
sqlite append would carry no origin, would not gossip, and would read
back as "unknown origin" on the peer — the exact failure this order
exists to fix.

Exit contract for bash callers: 0 on success (JSON on stdout), 1 when
the daemon cannot be reached (connection failure / timeout), 2 when
the daemon answered isError. Callers with a local fallback (the
directive log's tally) branch on the exit code and name the fallback
on their own stderr.

Bash usage:
    python3 scripts/co_notes.py write-note --kind decision --content "... \
        [--related-entity order-seat] [--scope global] [--session-id mcp]
    python3 scripts/co_notes.py read-notes [--query q] [--kinds a,b] \
        [--limit 100] [--include-operational] [--related-to X]
    python3 scripts/co_notes.py retire-note --id <id> --reason <why>

Python usage (co scripts import it; add the scripts dir to sys.path):
    from co_notes import write_note, read_notes, retire_note, NotesDaemonError
"""

import json
import os
import sys
import urllib.request

PORT = int(os.environ.get("SOVEREIGN_PORT", "9741"))
MCP_URL = f"http://localhost:{PORT}/mcp"
# Scripts tolerate a busy daemon (mid-embed, mid-ingest); the prompt hook
# keeps its own shorter timeout.
REQUEST_TIMEOUT = 15.0


class NotesDaemonError(Exception):
    """The daemon answered isError — the message names what failed."""


def _call(name, arguments):
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
    ).encode()
    req = urllib.request.Request(
        MCP_URL, data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
        outer = json.load(resp)
    if outer.get("error"):
        raise NotesDaemonError(f"{name}: MCP error {outer['error']}")
    result = outer.get("result") or {}
    if result.get("isError"):
        content = result.get("content") or []
        text = content[0].get("text", "unknown error") if content else "unknown error"
        raise NotesDaemonError(f"{name}: {text}")
    content = result.get("content") or []
    text = content[0].get("text", "{}") if content else "{}"
    return json.loads(text)


def write_note(kind, content, related_entity=None, symbols=None, files=None,
               session_id="mcp", scope=None, feature_id=None, supersedes=None):
    """Create a note. The daemon stamps author + origin node and, for
    global notes, gossips it to peers — the write-through path."""
    args = {"kind": kind, "content": content}
    if related_entity is not None:
        args["related_entity"] = related_entity
    if symbols:
        args["symbols"] = symbols
    if files:
        args["files"] = files
    if session_id is not None:
        args["session_id"] = session_id
    if scope is not None:
        args["scope"] = scope
    if feature_id is not None:
        args["feature_id"] = feature_id
    if supersedes is not None:
        args["supersedes"] = supersedes
    return _call("note", args)


def read_notes(query=None, kinds=None, limit=100, include_operational=False,
               related_to=None):
    """Read notes. include_operational=True is the seat path: the
    operational-anchor withholding is skipped entirely, so the caller
    (a seat tool, not an ordinary session) gets the operational rail."""
    args = {"limit": limit}
    if query is not None:
        args["query"] = query
    if kinds:
        args["kinds"] = kinds
    if related_to is not None:
        args["related_to"] = related_to
    if include_operational:
        args["include_operational"] = True
    return _call("notes", args)


def retire_note(note_id, reason):
    """Retire a note. Also tombstones it, which is what propagates the
    hide to peers (UC-D1 — 'B closes; A sees it gone')."""
    return _call("retire_note", {"id": note_id, "reason": reason})


def _cli():
    args = sys.argv[1:]
    verb = args[0] if args else ""
    pairs = {}
    i = 1
    while i < len(args):
        a = args[i]
        if not a.startswith("--"):
            print(f"co_notes: expected --key, got {a!r}", file=sys.stderr)
            return 2
        key = a[2:].replace("-", "_")
        i += 1
        if i < len(args) and not args[i].startswith("--"):
            pairs[key] = args[i]
            i += 1
        else:
            pairs[key] = "true"
    try:
        if verb == "write-note":
            kind, content = pairs.get("kind"), pairs.get("content")
            if not kind or not content:
                raise NotesDaemonError("write-note requires --kind and --content")
            out = write_note(
                kind,
                content,
                related_entity=pairs.get("related_entity"),
                scope=pairs.get("scope"),
                session_id=pairs.get("session_id", "mcp"),
            )
        elif verb == "read-notes":
            out = read_notes(
                query=pairs.get("query"),
                kinds=[k for k in pairs.get("kinds", "").split(",") if k],
                limit=int(pairs.get("limit", "100")),
                include_operational=pairs.get("include_operational") == "true",
                related_to=pairs.get("related_to"),
            )
        elif verb == "retire-note":
            nid, reason = pairs.get("id"), pairs.get("reason")
            if not nid or not reason:
                raise NotesDaemonError("retire-note requires --id and --reason")
            out = retire_note(nid, reason)
        else:
            print(
                "usage: co_notes.py write-note|read-notes|retire-note [--key value ...]",
                file=sys.stderr,
            )
            return 2
    except NotesDaemonError as exc:
        print(f"co_notes: {exc}", file=sys.stderr)
        return 2
    except Exception as exc:  # connection refused, timeout, bad JSON
        print(f"co_notes: daemon unreachable: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(_cli())
