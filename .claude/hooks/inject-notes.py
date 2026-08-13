#!/usr/bin/env python3
"""UserPromptSubmit hook: surface sovereign notes WITHOUT drowning the turn.

Lineage (all three surfaces measured, order seat-boot-block):

- 2026-08-07 rewrite: one-line index per note (kind, short id, first line),
  full bodies ONLY for frame-cited notes, dedupe per session. Replaced the
  shell firehose that printed every note body on every prompt (~15.4k
  tokens/injection, registered twice).
- 2026-08-12 (R1, f240c201): the firehose's registration and files removed.
- 2026-08-13 (this): the seat boot block + the E2 retrieval-log stream and
  first-prompt budget REGRESSED with the 2026-07-28 rewrite of the shell
  hook are restored here, in the surviving hook:
    * E2 RETRIEVAL LOGGING (MEMORY_MODEL §5 E2) — one record per prompt at
      ~/.svrnmesh/retrieval-log/<session>.jsonl, one note entry each with id,
      kind, symbols, files, terms and a `delivered` flag set AFTER the budget
      is enforced (the audit's honest denominator; `sovereign notes
      retrieval-audit` joins these against the transcript).
    * FIRST-PROMPT BUDGET (MEMORY_MODEL §5 E5, spec: notes ≤3200 chars) —
      the frame-bearing first prompt is capped; overflow degrades to
      dereferenceable pointers (P1), never silent truncation. Later prompts
      cap at 6000 chars.
    * SEAT BOOT BLOCK — a seat session's first prompt also carries ONE
      assembled rail block (scripts/co-boot-block.sh: anchor todos, recent
      seat decisions, open orders, directive-log stats) at a fixed ~3k-token
      budget, replacing the manual boot query loop the spike measured
      (10-12 round-trips, 14.5k-51.7k tokens per seat session). Once per
      session, marked by boot-block.json — the injected-notes.json dedupe
      pattern. Frame injection itself stays at SessionStart
      (session-boot.sh, own + predecessor) — this hook only READS the
      booted frame for citations; no second frame path.

Fails silently (exit 0, no output) whenever anything goes wrong — the daemon
being down must never block a prompt. Absence is REPORTED (one honest line),
never defaulted (ARCH §18.3).
"""

import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

# Same override the CLI and session-boot.sh honour, so all three can never read
# different stores.
SESSIONS_ROOT = os.path.expanduser(
    os.environ.get("SVRNMESH_SESSIONS_DIR")
    or os.environ.get("SOVEREIGN_SESSIONS_DIR")
    or "~/.svrnmesh/sessions"
)
RETRIEVAL_LOG_DIR = os.path.expanduser(
    os.environ.get("SVRNMESH_RETRIEVAL_LOG_DIR")
    or os.environ.get("SOVEREIGN_RETRIEVAL_LOG_DIR")
    or "~/.svrnmesh/retrieval-log"
)
PORT = os.environ.get("SOVEREIGN_PORT", "9741")
NOTE_LIMIT = 20
# One index line must stay scannable. Long enough to carry the claim, short
# enough that 20 of them are still a glance.
SUMMARY_CHARS = 120
# A cited note body longer than this is truncated with a dereference pointer
# (P1) rather than allowed to eat the budget alone.
NOTE_MAX_CHARS = int(os.environ.get("SOVEREIGN_NOTE_MAX_CHARS", "2000"))
# E5 Phase 2 budget (MEMORY_MODEL §5): the first prompt's notes payload is
# capped at 3200 chars (the frame-bearing turn, per spec); later prompts cap
# at 6000. Frame injection moved to SessionStart, so this hook only enforces
# the notes side.
FIRST_PROMPT_NOTES_BUDGET = int(
    os.environ.get("SOVEREIGN_FIRST_PROMPT_NOTES_BUDGET", "3200")
)
NOTES_BUDGET_CHARS = int(os.environ.get("SOVEREIGN_NOTES_BUDGET_CHARS", "6000"))
# The boot block has its own fixed budget (target ≤3k tokens), enforced inside
# co-boot-block.sh; this hook prints the block verbatim and never re-budgets.
# The script lives at REPO ROOT scripts/ (sibling of the co-* seat scripts
# it delegates to), not under .claude/ — three dirnames from hooks/.
BOOT_BLOCK_SCRIPT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
    "scripts",
    "co-boot-block.sh",
)

# The seat is detected by WHAT THIS SESSION RUNS, not by an env var (order
# commons-fluency, item 10): the comaintainer skill's invocation in the
# session transcript opts the session into the operational rail. The literal
# marker from the order is '"skill":"comaintainer"'; the regex also accepts
# the spaced form the transcript actually serializes ("skill": "comaintainer").
_SEAT_MARKER_RE = re.compile(r'"skill"\s*:\s*"comaintainer"')


def seat_override():
    """SOVEREIGN_SEAT remains ONLY an explicit one-off override (back-compat).

    Never required — the skill marker in the transcript is the mechanism.
    """
    return bool(os.environ.get("SOVEREIGN_SEAT"))


def transcript_is_seat(transcript_path):
    """Is THIS session a seat session, from the transcript — the durable truth?

    The hook receives transcript_path on every UserPromptSubmit. Scanning is
    bounded by an early break: a seat session invoked the skill at boot, so
    the marker lives in the first lines and the scan costs a fraction of the
    daemon round-trip that follows. No per-session cache is needed, and an
    always-live scan is also correct where a cache would go stale: a session
    that invokes the comaintainer skill mid-session becomes a seat on its
    next prompt. Missing/unreadable transcript = not a seat; a hook must
    never fail a prompt.
    """
    if not transcript_path:
        return False
    try:
        with open(transcript_path, "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if _SEAT_MARKER_RE.search(line):
                    return True
    except Exception:
        pass
    return False


def is_seat_session(envelope, session_id):
    """One decider for the seat read path: explicit override, else transcript."""
    if seat_override():
        return True
    return transcript_is_seat(envelope.get("transcript_path") or "")


def fetch_notes(seat=False):
    """Global (+ active feature) invariants and decisions, newest first."""
    feature_id = os.environ.get("SOVEREIGN_FEATURE_ID", "").strip()
    args = {
        "kinds": ["invariant", "decision"],
        "limit": NOTE_LIMIT,
        "scope": ["global", "feature"] if feature_id else ["global"],
    }
    # The seat's ambient read (UC-D4 inverse, order seat-durable-rail): the
    # seat opt-in is decided in is_seat_session() — the comaintainer skill
    # marker in this session's transcript (SOVEREIGN_SEAT=1 as explicit
    # override only). Seat sessions carry the operational records; every
    # other session is shielded from them — the withheld report in main()
    # is the guard that keeps seat bookkeeping out of their context.
    if seat:
        args["include_operational"] = True
    if feature_id:
        args["feature_id"] = feature_id
    payload = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "read_notes", "arguments": args},
        }
    ).encode()
    req = urllib.request.Request(
        f"http://localhost:{PORT}/mcp",
        data=payload,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=2) as resp:
        outer = json.load(resp)
    inner = json.loads(outer["result"]["content"][0]["text"])
    return inner  # {"notes": [...], "withheld_operational": N, "withheld_anchors": [...]}


def frame_text(session_id):
    """This session's frame plus whichever frame the boot hook injected.

    Both matter: a successor is handed its PREDECESSOR's frame, and that is
    exactly the frame whose note citations it must honour.
    """
    paths = []
    if session_id:
        paths.append(os.path.join(SESSIONS_ROOT, session_id, "frame.md"))
        boot = os.path.join(SESSIONS_ROOT, session_id, "boot.json")
        try:
            with open(boot, encoding="utf-8") as fh:
                other = (json.load(fh).get("frame_session") or "").strip()
            if other:
                paths.append(os.path.join(SESSIONS_ROOT, other, "frame.md"))
        except Exception:
            pass
    out = []
    for p in paths:
        try:
            with open(p, encoding="utf-8") as fh:
                out.append(fh.read())
        except Exception:
            continue
    return "\n".join(out)


# A uuid, or the 8-hex prefix people actually type in prose ("note f93937ed").
_ID_RE = re.compile(r"\b[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b|\b[0-9a-f]{8}\b")


def cited_ids(text):
    return {m.group(0) for m in _ID_RE.finditer(text or "")}


def seen_path(session_id):
    return os.path.join(SESSIONS_ROOT, session_id, "injected-notes.json")


def load_seen(session_id):
    try:
        with open(seen_path(session_id), encoding="utf-8") as fh:
            return set(json.load(fh))
    except Exception:
        return set()


def save_seen(session_id, ids):
    try:
        p = seen_path(session_id)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as fh:
            json.dump(sorted(ids), fh)
    except Exception:
        pass


def summarize(content):
    """First non-empty line, collapsed and clipped — the note's own claim."""
    for line in (content or "").splitlines():
        line = " ".join(line.split())
        if line:
            return line[:SUMMARY_CHARS] + ("…" if len(line) > SUMMARY_CHARS else "")
    return "(empty)"


def author_tag(note):
    """Which machine wrote this note, as a short display token.

    A note can be about the CODE (true on every box) or about the BOX it
    was written on — "GPU busy", "holding the daemon lock", "long run in
    progress". Without the author those are indistinguishable, and agents
    were routing around peers' machine-state notes as if they were local
    (operator report, 2026-08-07).

    Returns "" when the daemon reports no usable attribution, so an
    unattributed note reads as unattributed rather than as ours. The
    self-vs-peer judgement is the daemon's (`NodeRoster::resolve`); this
    only picks the rendering.
    """
    relation = note.get("author_relation")
    author = note.get("author") or ""
    if relation == "peer":
        # Name-only: `author` is already "BeefyMac (peer)".
        return f" _{author}_"
    return ""


# ── E2 retrieval logging (MEMORY_MODEL §5 E2, restored 2026-08-13) ───────────
# One record per injection, appended after the budget is enforced, so
# `delivered` describes what ACTUALLY entered context — the honest
# denominator `sovereign notes retrieval-audit` requires. Append-only,
# fail-silent side effect: the injection is the hook's primary duty, logging
# must NEVER interfere with it.
def sha16(s):
    return hashlib.sha256((s or "").encode()).hexdigest()[:16]


def distinctive_terms(content, cap=15):
    # Token-shape heuristic shared with scripts/co-boot-block.sh — KEEP THE
    # TWO IN SYNC (one instrument, two write sites). Identifier-like tokens
    # the audit can match against the session's downstream actions without
    # re-reading the note store: snake_case/CamelCase/dotted/slashed shapes
    # and long tokens — the classes unlikely to co-occur in generic prose.
    out, seen = [], set()
    for t in re.findall(r"[A-Za-z_][A-Za-z0-9_./-]{4,}", content or ""):
        # a single char repeated (e.g. a 4900-char filler run) is not a
        # distinctive term — measured: it bloated a dereference hint past the
        # budget (inject-budget.sh, 2026-08-13).
        if len(set(t)) == 1:
            continue
        tl = t.lower()
        if tl in seen:
            continue
        distinctive = (
            "_" in t or "." in t or "/" in t
            or any(c.isupper() for c in t[1:])
            or len(t) >= 8
        )
        if not distinctive:
            continue
        seen.add(tl)
        out.append(t)
        if len(out) >= cap:
            break
    return out


def deref_hint(note):
    """A WORKING dereference instruction for one note's body. There is no
    exact-id route on the daemon MCP surface: `query` is semantic/FTS, so
    `notes(query: "<id>")` returns notes that merely mention the id (measured
    2026-08-13), and `svrn notes list --id` reads the repo-local store, not
    the daemon store. The path that works is a semantic read on the note's
    distinctive terms (the engine's T1 retrieval), so the pointer names those
    instead of the id. Length-capped so a degenerate term can never inflate a
    pointer past the budget."""
    terms = distinctive_terms(note.get("content") or "", cap=3)
    if terms:
        q = " ".join(terms[:2])[:64].rstrip()
        return f"`notes(query: \"{q}\")`"
    return "`notes(query: \"…\")`"


def log_injection(session_id, query, label, decisions, budget_chars):
    """Append one retrieval-log record; `decisions` = [(note, delivered, truncated)]."""
    if not session_id:
        return  # no join key for the audit — skip rather than write junk
    try:
        os.makedirs(RETRIEVAL_LOG_DIR, exist_ok=True)
        record = {
            "ts": int(time.time()),
            "session_id": session_id,
            "prompt_sha": sha16(query),
            "query": (query or "")[:200],
            "label": label,
            "count": len(decisions),
            "delivered_count": sum(1 for _, d, _ in decisions if d),
            "budget_chars": budget_chars,
            "payload_chars": 0,
            "notes": [
                {
                    "rank": i,
                    "id": n.get("id"),
                    "kind": n.get("kind", "note"),
                    "content_sha": sha16((n.get("content") or "").strip()),
                    "symbols": n.get("symbols") or [],
                    "files": n.get("files") or [],
                    "terms": distinctive_terms(n.get("content") or ""),
                    "approx_tokens": len((n.get("content") or "")) // 4,
                    "delivered": delivered,
                    "truncated": truncated,
                }
                for i, (n, delivered, truncated) in enumerate(decisions)
            ],
        }
        with open(
            os.path.join(RETRIEVAL_LOG_DIR, f"{session_id}.jsonl"),
            "a",
            encoding="utf-8",
        ) as fh:
            fh.write(json.dumps(record) + "\n")
    except (OSError, TypeError, ValueError):
        pass


# ── Seat boot block (order seat-boot-block) ──────────────────────────────────
def run_boot_block(session_id):
    """Inject the one-time seat boot block via scripts/co-boot-block.sh.

    Returns the text to print, or None when the block already fired this
    session (boot-block.json marker) or there is nothing to say. The script
    enforces its own fixed budget and writes ~/.svrnmesh/sessions/<id>/
    boot-block.json — the marker AND the E2 source. The hook prints the
    block verbatim (no re-budgeting), converts the record into one
    retrieval-log row, and reports absence honestly on failure — a failed
    run writes NO marker, so the next prompt retries.
    """
    if not session_id:
        return None
    marker = os.path.join(SESSIONS_ROOT, session_id, "boot-block.json")
    if os.path.exists(marker):
        return None  # already injected this session
    try:
        proc = subprocess.run(
            [BOOT_BLOCK_SCRIPT, session_id],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except Exception as exc:
        return (
            "_boot block unavailable ({type}: {e}) — run "
            "`scripts/co-boot-block.sh` manually_".format(
                type=type(exc).__name__, e=exc
            )
        )
    if proc.returncode != 0:
        return (
            f"_boot block unavailable (co-boot-block.sh rc={proc.returncode}) — "
            "run `scripts/co-boot-block.sh` manually_"
        )
    block = (proc.stdout or "").strip()
    if not block:
        return (
            "_boot block empty (daemon down?) — run "
            "`scripts/co-boot-block.sh` manually_"
        )
    # E2: convert the script's record (which knows delivered, from the budget
    # it enforced) into one retrieval-log row, same schema the audit reads.
    try:
        with open(marker, encoding="utf-8") as fh:
            rec = json.load(fh)
        decisions = [
            (
                {
                    "id": n.get("id"),
                    "kind": n.get("kind", "note"),
                    "symbols": n.get("symbols") or [],
                    "files": n.get("files") or [],
                    # The script extracts terms at its write site (KEEP IN
                    # SYNC) so the audit can score anchorless seat notes.
                    "terms": n.get("terms") or [],
                    "content": "",
                },
                bool(n.get("delivered")),
                bool(n.get("truncated")),
            )
            for n in rec.get("notes") or []
        ]
        log_injection(
            session_id,
            "boot block: related_to=comaintainer-seat + order-seat + directive-log",
            "seat boot block",
            decisions,
            int(rec.get("budget_chars") or 0),
        )
    except Exception:
        pass  # logging must never fail the block
    return block + "\n"


def main():
    try:
        envelope = json.load(sys.stdin)
    except Exception:
        envelope = {}
    session_id = (envelope.get("session_id") or "").strip()
    prompt = (envelope.get("prompt") or "").strip()
    is_seat = is_seat_session(envelope, session_id)

    # 0) The seat boot block, once per session, before anything that can fail
    #    on the daemon — it IS the seat's boot ritual, made structural.
    out = []
    if is_seat:
        block = run_boot_block(session_id)
        if block:
            out.append(block)
            out.append("")

    try:
        env = fetch_notes(seat=is_seat)
    except Exception:
        if out:
            print("\n".join(out))
        return  # daemon down / offline — never block the prompt
    notes = env.get("notes") or []

    seen = load_seen(session_id) if session_id else set()
    cited = cited_ids(frame_text(session_id))
    first_prompt = session_id and not os.path.exists(seen_path(session_id))
    budget = FIRST_PROMPT_NOTES_BUDGET if first_prompt else NOTES_BUDGET_CHARS

    # A note is cited when the frame names its full id or its 8-char prefix.
    def is_cited(note_id):
        return bool(note_id) and (
            note_id in cited or note_id.split("-")[0] in cited
        )

    fresh = [n for n in notes if n.get("id") not in seen]
    # Frame-cited notes are re-surfaced even if already seen ONLY on the first
    # prompt of a session; after that the agent has them and repetition is the
    # very cost this hook exists to cut.
    cited_notes = [n for n in fresh if is_cited(n.get("id"))]
    index_notes = [n for n in fresh if not is_cited(n.get("id"))]

    # ── the first-prompt budget, enforced BEFORE logging (delivered = truth) ──
    # Greedy fill in priority order: cited bodies first (the predecessor said
    # they are load-bearing), then index lines (each is itself a pointer).
    # Overflow degrades to a named pointer line — never silent truncation.
    budget_spent = 0
    decisions = []  # (note, delivered, truncated) — the E2 record

    def fits(chars):
        nonlocal budget_spent
        if budget_spent + chars <= budget:
            budget_spent += chars
            return True
        return False

    if cited_notes:
        body = "## Notes your session frame cites — READ THESE FIRST"
        body += "\n\nYour frame names these by id, which means a predecessor marked "
        body += "them load-bearing. Do not re-derive what they already state.\n\n"
        if fits(len(body)):
            out.append(body)
            for n in cited_notes:
                nid = n.get("id", "")
                content = (n.get("content") or "").strip()
                truncated = False
                if len(content) > NOTE_MAX_CHARS:
                    content = (
                        content[:NOTE_MAX_CHARS].rstrip()
                        + f"\n… [truncated; full note: {deref_hint(n)}]"
                    )
                    truncated = True
                blk = f"### [{n.get('kind', 'note')}] {nid}{author_tag(n)}\n\n{content}\n\n"
                if fits(len(blk)):
                    out.append(blk)
                    decisions.append((n, True, truncated))
                else:
                    # Budget spent: the note still surfaces as a pointer.
                    ptr = f"- `{nid[:8]}` [{n.get('kind', 'note')}] " \
                          f"{summarize(n.get('content'))} — _frame cites it; " \
                          f"body at {deref_hint(n)}_\n"
                    if fits(len(ptr)):
                        out.append(ptr)
                        decisions.append((n, True, True))
                    else:
                        decisions.append((n, False, True))
                        out.append(
                            f"_note `{nid[:8]}` (frame-cited) exceeds the "
                            f"{budget}-char budget — {deref_hint(n)}_\n"
                        )
        else:
            for n in cited_notes:
                decisions.append((n, False, True))
            out.append(
                f"_notes budget ({budget} chars) spent before cited notes — "
                "`notes(query: \"…\")` for bodies_"
            )
            out.append("")

    if index_notes:
        head = "## Sovereign notes index (bodies via `notes(query=\"…\")`)"
        head += "\n\nTitles only — one line each. Pull the body when one is "
        head += "relevant to what you are about to do; do not act on a title "
        head += "alone.\n\n"
        if fits(len(head)):
            out.append(head)
            for n in index_notes:
                nid = (n.get("id") or "")[:8]
                kind = n.get("kind", "note")
                scope = n.get("scope", "global")
                tag = kind if scope == "global" else f"{kind}/{scope}"
                line = (
                    f"- `{nid}` [{tag}]{author_tag(n)} "
                    f"{summarize(n.get('content'))}\n"
                )
                if fits(len(line)):
                    out.append(line)
                    decisions.append((n, True, False))
                else:
                    decisions.append((n, False, True))
            dropped = sum(1 for _, d, _ in decisions if not d)
            if dropped:
                out.append(
                    f"_(… {dropped} further note(s) exceeded this hook's "
                    f"{budget}-char budget and are NOT in your context — "
                    "`notes(query: \"…\")` to pull them.)_"
                )
            out.append("")
        else:
            for n in index_notes:
                decisions.append((n, False, True))
            out.append(
                f"_notes budget ({budget} chars) spent before the index — "
                "`notes(query: \"…\")` for the rest_"
            )
            out.append("")

    # The D4 guard, reported not dropped (ARCH §18.3): when the daemon
    # withheld operational records, that absence is NAMED even on a
    # prompt with nothing else to say — an ordinary session learns it
    # was shielded, never silently spared. Seat sessions (comaintainer
    # skill marker in the transcript) ask for these records, so they
    # report nothing.
    withheld = int(env.get("withheld_operational") or 0)
    if withheld > 0:
        anchors = ", ".join(env.get("withheld_anchors") or [])
        out.append(
            f"_Note: {withheld} operational record(s) withheld (anchored to "
            f"{anchors}) — the seat's coordination rail, not this session's "
            "context._"
        )
        out.append("")

    if decisions:
        log_injection(session_id, prompt, "ambient index", decisions, budget)
    if out:
        print("\n".join(out))
    if session_id:
        save_seen(session_id, seen | {n.get("id") for n in fresh if n.get("id")})


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass  # a hook must never fail a prompt
    sys.exit(0)
