#!/usr/bin/env python3
"""UserPromptSubmit hook: surface sovereign notes WITHOUT drowning the turn.

Why this was rewritten (2026-08-07)
-----------------------------------
The shell version fetched the 20 most recent global invariants + decisions and
printed every one IN FULL on EVERY user prompt. Measured on this repo: 61,664
bytes (~15.4k tokens) per injection. It was also registered TWICE in
settings.json, so each prompt paid it twice (~31k tokens), and because hook
output becomes a turn in the transcript, every one of those copies is re-billed
as cache-read on every subsequent turn. Four prompts into a session that is
~123k tokens of notes.

The observed result was the opposite of the intent: a payload that large is not
read. In the session that prompted this rewrite the agent re-derived, from raw
journals, a median it already had — because the note carrying it arrived inside
a 60KB block it had skimmed past, while the frame's own explicit pointer
("CITATIONS IN NOTE f93937ed — read it, do not re-derive") was never followed.
The operator's summary was exact: "we just write notes and nobody consumes
them."

So the fix is not more injection. It is three changes:

  1. INDEX, NOT FIREHOSE. One line per note — kind, short id, first line. The
     agent learns WHAT EXISTS and pulls bodies with `notes(query=...)` when a
     note is relevant. ~25x smaller and actually readable.
  2. FULL BODY ONLY WHEN THE FRAME CITES IT. A note id named in your session
     frame (frontmatter `notes:` list, or inline in the prose) is load-bearing
     by construction — the predecessor said so. Those bodies are inlined whole,
     under a header that says to read them first. This is the specific failure
     above, made structural rather than remembered (§7, principle 10).
  3. DEDUPE PER SESSION. The same notes were re-sent verbatim every prompt.
     Each note id is now surfaced ONCE per session; later prompts carry only
     what is new. Silent when there is nothing new.

Fails silently (exit 0, no output) whenever anything goes wrong — the daemon
being down must never block a prompt.
"""

import json
import os
import re
import sys
import urllib.error
import urllib.request

# Same override the CLI and session-boot.sh honour, so all three can never read
# different stores.
SESSIONS_ROOT = os.path.expanduser(
    os.environ.get("SVRNMESH_SESSIONS_DIR")
    or os.environ.get("SOVEREIGN_SESSIONS_DIR")
    or "~/.sovereign/sessions"
)
PORT = os.environ.get("SOVEREIGN_PORT", "9741")
NOTE_LIMIT = 20
# One index line must stay scannable. Long enough to carry the claim, short
# enough that 20 of them are still a glance.
SUMMARY_CHARS = 120

# The seat is detected by WHAT THIS SESSION RUNS, not by an env var (order
# commons-fluency, item 10): the comaintainer skill's invocation in the
# session transcript opts the session into the operational rail.
#
# THERE ARE TWO WAYS TO TAKE THE SEAT, and a detector that knows only one is
# a detector for a UI affordance rather than for the fact:
#
#   * Skill tool call   -> '"skill": "comaintainer"'  (spaced or not)
#   * slash command     -> '<command-name>/comaintainer</command-name>'
#
# Measured on the live seat transcript `53a08260` (2026-08-13): ZERO Skill
# tool calls, one `<command-name>/comaintainer</command-name>` at line 242 —
# the seat took its seat by typing the slash command, so this regex never
# matched and the seat ran all day with its own rail shielded from it. Its
# transcript carries 60 copies of this hook's own "operational record(s)
# withheld … the seat's coordination rail, not this session's context".
#
# The skill LISTING (`"type":"skill_listing"`, "- comaintainer: Take the
# comaintainer director seat…") is present in every session including every
# worker, and must never count. Neither pattern can match it: both require
# invocation syntax, not the skill's name.
_SEAT_MARKER_RE = re.compile(
    r'"skill"\s*:\s*"comaintainer"'
    r'|<command-name>\s*/?comaintainer\s*</command-name>'
)


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


def publish_seat_role(session_id):
    """Hand the frame WRITER what only this hook can see.

    `session_state` (sovereign-tools) banks the frame, but it never receives
    a transcript_path, so it cannot run the detection above — and a second
    detector reading a guessed transcript path would be two deciders for one
    fact. Same seam the predecessor hand-off already uses: a file beside the
    frame (`~/.sovereign/sessions/<id>/role`), written by the surface that
    holds the truth and read by the writer that needs it.

    Idempotent and best-effort by construction: a hook must never fail a
    prompt over bookkeeping, and an absent sidecar simply means "not a seat",
    which is the correct reading for every worker session on the box.
    """
    if not session_id:
        return
    try:
        d = os.path.join(SESSIONS_ROOT, session_id)
        os.makedirs(d, exist_ok=True)
        path = os.path.join(d, "role")
        if os.path.isfile(path):
            return
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("seat\n")
    except Exception:
        pass


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
    if relation in (None, "self", "unattributed", "unknown", "ambiguous"):
        # Self is the common case and needs no marker — the boot hook
        # already told the session which machine it is. The rest carry no
        # information worth the tokens on a per-note line.
        return ""
    return ""


def main():
    try:
        envelope = json.load(sys.stdin)
    except Exception:
        envelope = {}
    session_id = (envelope.get("session_id") or "").strip()

    seat = is_seat_session(envelope, session_id)
    # Publish BEFORE the daemon call: the sidecar is what makes a seat frame
    # self-describing at the next boot, and a daemon that is down must not
    # cost the successor its "take the seat" line.
    if seat:
        publish_seat_role(session_id)

    try:
        env = fetch_notes(seat=seat)
    except Exception:
        return  # daemon down / offline — never block the prompt
    notes = env.get("notes") or []

    seen = load_seen(session_id) if session_id else set()
    cited = cited_ids(frame_text(session_id))

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

    out = []
    if cited_notes:
        out.append("## Notes your session frame cites — READ THESE FIRST")
        out.append("")
        out.append(
            "Your frame names these by id, which means a predecessor marked them "
            "load-bearing. Do not re-derive what they already state."
        )
        out.append("")
        for n in cited_notes:
            nid = n.get("id", "")
            out.append(f"### [{n.get('kind', 'note')}] {nid}{author_tag(n)}")
            out.append((n.get("content") or "").strip())
            out.append("")

    if index_notes:
        out.append("## Sovereign notes index (bodies via `notes(query=\"…\")`)")
        out.append("")
        out.append(
            "Titles only — one line each. Pull the body when one is relevant to "
            "what you are about to do; do not act on a title alone."
        )
        out.append("")
        for n in index_notes:
            nid = (n.get("id") or "")[:8]
            kind = n.get("kind", "note")
            scope = n.get("scope", "global")
            tag = kind if scope == "global" else f"{kind}/{scope}"
            out.append(
                f"- `{nid}` [{tag}]{author_tag(n)} {summarize(n.get('content'))}"
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
