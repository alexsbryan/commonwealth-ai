#!/usr/bin/env python3
# sovereign inject-notes — UserPromptSubmit hook for Claude Code.
#
# A one-line INDEX of active invariants and decisions (id, kind, claim line)
# with per-session dedupe and a hard budget — never a full-body dump. The
# firehose this replaced printed every body on every prompt (~8.4k tokens,
# 0-1 of 99 notes the seat needed — SMALL_CONTEXT_MEMORY_SPIKE.md F1); the
# index costs a few hundred tokens once per prompt-with-new-notes, and
# bodies are pulled on demand with read_notes.
#
# Also writes ONE retrieval-audit row per prompt (~/.svrnmesh/retrieval-log/
# <session>.jsonl, MEMORY_MODEL §5 E2) so `svrn notes retrieval-audit` can
# score what actually entered context, with `delivered` set AFTER the budget.
#
# Fails silently when the server is not running — offline work is unaffected,
# and a hook must never block a prompt.
import json
import os
import re
import sys
import time
import urllib.request

PORT = os.environ.get("SOVEREIGN_PORT", "9741")
SESSIONS_ROOT = os.environ.get(
    "SOVEREIGN_SESSIONS_DIR", os.path.expanduser("~/.svrnmesh/sessions")
)
LOG_DIR = os.environ.get(
    "SVRNMESH_RETRIEVAL_LOG_DIR", os.path.expanduser("~/.svrnmesh/retrieval-log")
)
FIRST_PROMPT_BUDGET = int(os.environ.get("SOVEREIGN_FIRST_PROMPT_NOTES_BUDGET", "3200"))
NOTES_BUDGET = int(os.environ.get("SOVEREIGN_NOTES_BUDGET_CHARS", "6000"))
NOTE_LIMIT = int(os.environ.get("SOVEREIGN_INJECT_NOTE_LIMIT", "20"))
# The ranker wants terms, not an essay; a long paste should not become the key.
QUERY_CAP = int(os.environ.get("SOVEREIGN_INJECT_QUERY_CAP", "500"))


def first_line(content, cap=110):
    for line in (content or "").splitlines():
        line = " ".join(line.split())
        if line:
            return line[:cap] + ("…" if len(line) > cap else "")
    return "(empty)"


try:
    envelope = json.load(sys.stdin)
except Exception:
    envelope = {}
session_id = (envelope.get("session_id") or "").strip()
# The prompt is the RETRIEVAL KEY, and it is the only artifact that exists at
# the moment the expensive decision is made. Approach-level mistakes — "I'll
# build a reachability closure" — happen before any file is touched, so a
# path-anchored trigger structurally cannot catch them; the prompt can.
# Harness-generated turns are NOT decision moments. Claude Code runs this hook
# on task notifications and system reminders too, and their boilerplate makes a
# meaningless retrieval key — measured live on 2026-09-03, a notification turn
# queried `<task-notification> <task-id>b8j0…`. That matters because the
# per-session dedupe below spends each note's ONE surfacing: a note burned
# against harness XML never reaches the prompt that needed it. Strip the
# wrappers; if nothing human is left, send no query and fall back to newest.
HARNESS_TAGS = ("task-notification", "system-reminder", "local-command-stdout",
                "command-message", "command-name", "command-args")


def human_part(text):
    for tag in HARNESS_TAGS:
        text = re.sub(rf"<{tag}>.*?</{tag}>", " ", text, flags=re.S)
        text = re.sub(rf"</?{tag}[^>]*>", " ", text)
    return " ".join(text.split())


prompt = human_part(envelope.get("prompt") or "")[:QUERY_CAP]

# ATOS scope-aware payload: globals plus the active feature's notes when
# $SOVEREIGN_FEATURE_ID is set (svrn atos start-milestone); globals only
# otherwise, so in-flight feature chatter does not leak into other sessions.
#
# `attempt` is here because the costliest failure in this repo is re-proposing
# something already measured and rejected, and settings.json's own systemPrompt
# instructs every session to WRITE those notes. Reading them back closes a loop
# that was open: the kind existed, the writers existed, the reader excluded it.
args = {
    "kinds": ["invariant", "decision", "attempt"],
    "scope": ["global"],
    "limit": NOTE_LIMIT,
}
# Without a query `read_notes` returns NEWEST-first, and the pool is majority
# harvested commit subjects — so recency structurally evicts the durable notes
# this hook exists to carry. Keying on the prompt is what makes the 20 slots
# situational instead of chronological.
if prompt:
    args["query"] = prompt
fid = os.environ.get("SOVEREIGN_FEATURE_ID", "").strip()
if fid:
    args["scope"] = ["global", "feature"]
    args["feature_id"] = fid
payload = json.dumps(
    {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "read_notes", "arguments": args},
    }
).encode()
try:
    req = urllib.request.Request(
        f"http://localhost:{PORT}/mcp",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=2) as resp:
        outer = json.load(resp)
    inner = json.loads(outer["result"]["content"][0]["text"])
except Exception:
    sys.exit(0)  # daemon down / offline — never block the prompt

notes = inner.get("notes") or []

# Per-session dedupe: a note surfaces once, on the first prompt after it was
# written; later prompts never re-bill the same note into context.
seen_path = ""
seen = set()
if session_id:
    seen_path = os.path.join(SESSIONS_ROOT, session_id, "injected-notes.json")
    try:
        with open(seen_path, encoding="utf-8") as fh:
            seen = set(json.load(fh))
    except Exception:
        pass

fresh = [n for n in notes if n.get("id") and n.get("id") not in seen]

# The first prompt carries a tighter budget (the session frame already fills
# the window); later prompts get the steady-state budget. Overflow is NAMED
# — never silent truncation (ARCH §18.3).
first_prompt = bool(session_id) and not os.path.exists(seen_path)
budget = FIRST_PROMPT_BUDGET if first_prompt else NOTES_BUDGET
budget_spent = 0
out = []
delivered = []  # (note, delivered) — the E2 record

if fresh:
    head = "## Active sovereign notes (injected by hook) — index\n\n"
    head += "Titles only, one line each. Pull the body with "
    head += "`read_notes(query=…)` when one is relevant to what you are "
    head += "about to do; do not act on a title alone.\n\n"
    out.append(head)
    budget_spent = len(head)
    for n in fresh:
        nid = (n.get("id") or "")[:8]
        kind = n.get("kind", "note")
        line = f"- `{nid}` [{kind}] {first_line(n.get('content'))}\n"
        if budget_spent + len(line) <= budget:
            budget_spent += len(line)
            out.append(line)
            delivered.append((n, True))
        else:
            delivered.append((n, False))
    if len(delivered) < len(fresh):
        out.append(
            f"_… {len(fresh) - len(delivered)} more note(s) exceed the "
            f"{budget}-char budget — query by topic._\n"
        )

# D4: when the daemon withheld operational records, that absence is NAMED
# even for a session that was never a seat — the guard that keeps seat
# bookkeeping out of ordinary sessions' context.
withheld = int(inner.get("withheld_operational") or 0)
if withheld > 0:
    anchors = ", ".join(inner.get("withheld_anchors") or [])
    out.append(
        f"_Note: {withheld} operational record(s) withheld (anchored to "
        f"{anchors})._\n"
    )

if out:
    print("\n".join(out))
    if session_id:
        try:
            os.makedirs(os.path.join(SESSIONS_ROOT, session_id), exist_ok=True)
            with open(seen_path, "w", encoding="utf-8") as fh:
                json.dump(sorted(n.get("id") for n in fresh if n.get("id")), fh)
            os.makedirs(LOG_DIR, exist_ok=True)
            record = {
                "ts": int(time.time()),
                "session_id": session_id,
                "query": prompt
                if prompt
                else "injected: newest {limit} (no prompt in envelope)".format(
                    limit=NOTE_LIMIT
                ),
                "label": "first-prompt notes" if first_prompt else "notes",
                "count": len(fresh),
                "delivered_count": sum(1 for _, d in delivered if d),
                "budget_chars": budget,
                "payload_chars": budget_spent,
                "notes": [
                    {
                        "id": n.get("id"),
                        "kind": n.get("kind", "note"),
                        "symbols": n.get("symbols") or [],
                        "files": n.get("files") or [],
                        "terms": [],
                        "content": (n.get("content") or "")[:200],
                        "delivered": d,
                        "truncated": not d,
                    }
                    for n, d in delivered
                ],
            }
            with open(
                os.path.join(LOG_DIR, session_id + ".jsonl"), "a", encoding="utf-8"
            ) as fh:
                fh.write(json.dumps(record) + "\n")
        except Exception:
            pass  # the E2 side-channel must never fail the injection
