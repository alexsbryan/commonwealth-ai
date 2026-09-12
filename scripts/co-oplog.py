#!/usr/bin/env python3
"""co-oplog.py — the claim oplog: what an agent told the operator, per turn.

ORDER: .sovereign/features/bs-1-oplog/order.md (approved 2026-09-12).
STEPS 1-2 ONLY: record. No grade, no hook. Both are gated on the backtest.

WHY PER TURN. Measured 2026-09-12 over the 40 most recent transcripts
(1,413 assistant text blocks, 11,009 sentences): 375 promissory sentences,
9.4 per session, and FOUR of them -- 1% -- appear in the session's final
message. A checker that reads the final report sees one percent of what was
promised. The ledger reads every turn or it is looking at the wrong artifact.

WHY A CLASSIFIER IS THE RISK. Surface-form patterns put 87% of real report
sentences in "other". Deciding what CLASS a sentence is costs more than
adjudicating it, so the mechanical stage here takes only the cases it can
win outright and hands everything else to the daemon. Order step 3 measures
the result against the operator BEFORE any grade is computed.

STORE. Rows append to ~/.sovereign/comaintainer/verdicts.jsonl -- the seat's
existing ledger -- under kind="claim". A new kind, not a new store (§19).

ENGINE. Pinned on the wire and the reply's model id recorded on every row.
An alias ("primary") is deliberately NOT the default: pinning an alias lets
config.toml silently change the engine behind a ledger (§18.3).
"""
from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

STATE_DIR = Path(os.environ.get("SOVEREIGN_STATE_DIR",
                                Path.home() / ".sovereign" / "comaintainer"))
VERDICTS_LOG = STATE_DIR / "verdicts.jsonl"
TRANSCRIPTS = Path.home() / ".claude" / "projects"
DAEMON = os.environ.get("SOVEREIGN_DAEMON_URL", "http://localhost:9741")
DEFAULT_PIN = os.environ.get("SOVEREIGN_OPLOG_MODEL", "Qwen3.6-35B-A3B-UD-MTP-IQ4_NL")

# ---- the taxonomy -----------------------------------------------------
#
# The column that matters is not what a claim is ABOUT, it is WHEN it
# becomes decidable. Every downstream verdict hangs off that.

CLASSES = {
    "promissory":    "the agent says it WILL do something — decidable at session end",
    "universal":     "asserts something holds for ALL or NONE of a set — one counterexample kills it",
    "retrospective": "asserts what was done, observed, or exists now — decidable against the record",
    "predictive":    "asserts a future outcome needing a measurement — may stay open",
    "evaluative":    "judgement, preference, recommendation — NEVER decidable, never graded",
    "none":          "not a claim: a question, a transition, a restatement of the operator",
}

SYSTEM = """You classify one sentence from a coding agent's message to its operator.
Answer with the class whose DECIDABILITY matches, not whose topic matches.

promissory    — the agent commits to future work of its own ("I'll wire X next").
universal     — asserts all/none/only/never/every about a set ("the only impl is X").
retrospective — asserts what was done, observed, or exists ("the tests pass", "it lives at a.rs:12").
predictive    — asserts an outcome that needs a future measurement ("this will cut latency").
evaluative    — judgement, preference, or recommendation ("cleaner", "I'd rather", "worth doing").
none          — a question, a transition, a restatement of what the operator said, or meta-talk.

Precedence when two fit: universal > promissory > retrospective > predictive > evaluative > none.
A sentence that merely MENTIONS work without asserting it happened is `none`."""

# High-precision mechanical shortcuts. Deliberately narrow: a wrong class
# here is invisible (no model ever sees the sentence), so only shapes with
# no plausible counter-reading are taken.
RX_PROMISSORY = re.compile(r"\b(I'll|I will|I'm going to|I am going to|next I'll|I plan to)\s+\w", re.I)
RX_UNIVERSAL = re.compile(r"\b(the only|nothing else|no other|none of|never\s+\w+s\b|every\s+\w+\s+(?:goes|is|has))\b", re.I)
RX_QUESTION = re.compile(r"\?\s*$")

def mechanical(sent: str) -> str | None:
    """A class this stage can win outright, or None to ask the daemon."""
    if RX_QUESTION.search(sent):
        return "none"
    if RX_UNIVERSAL.search(sent):
        return "universal"
    if RX_PROMISSORY.search(sent):
        return "promissory"
    return None

# ---- transcript reading ------------------------------------------------

def strip_reminders(text: str) -> str:
    return re.sub(r"<system-reminder>.*?</system-reminder>", "", text, flags=re.S).strip()

def strip_fences(text: str) -> str:
    out, fenced = [], False
    for line in text.split("\n"):
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if not fenced:
            out.append(line)
    return "\n".join(out)

def sentences(text: str) -> list[str]:
    """Line first, then sentence. A markdown table row or bullet is a claim
    on its own; run whole-text sentence splitting and a nine-row table
    becomes one 'sentence'."""
    out = []
    for line in strip_fences(text).split("\n"):
        line = line.strip().lstrip("-*> ").strip()
        if len(line) < 25:
            continue
        for s in re.split(r"(?<=[.!?])\s+", line):
            s = s.strip()
            if len(s) >= 25:
                out.append(s)
    return out

def turns(path: Path) -> list[tuple[int, str]]:
    """(turn index, assistant text) for every substantive message."""
    out, i = [], 0
    with path.open() as fh:
        for line in fh:
            try:
                rec = json.loads(line)
            except Exception:
                continue
            if rec.get("type") != "assistant":
                continue
            content = (rec.get("message") or {}).get("content")
            if not isinstance(content, list):
                continue
            for b in content:
                if isinstance(b, dict) and b.get("type") == "text":
                    t = strip_reminders(b.get("text", ""))
                    if len(t) >= 120:
                        i += 1
                        out.append((i, t))
    return out

# ---- the daemon --------------------------------------------------------

class DaemonDown(RuntimeError):
    pass

BATCH_SCHEMA = {
    "type": "object",
    "properties": {
        "classes": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "n": {"type": "integer"},
                    "class": {"type": "string", "enum": list(CLASSES)},
                },
                "required": ["n", "class"],
            },
        }
    },
    "required": ["classes"],
}

def call_daemon(system: str, user: str, pin: str, max_tokens: int,
                schema: dict | None, timeout: float) -> tuple[str, str]:
    body = {
        "model": pin,
        "messages": [{"role": "system", "content": system},
                     {"role": "user", "content": user}],
        "temperature": 0,
        "max_tokens": max_tokens,
    }
    if schema is not None:
        body["response_format"] = {
            "type": "json_schema",
            "json_schema": {"name": "classes", "schema": schema, "strict": True},
        }
    req = urllib.request.Request(
        f"{DAEMON}/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            d = json.load(r)
    except urllib.error.URLError as e:
        raise DaemonDown(f"daemon unreachable at {DAEMON}: {e}") from e
    ch = d["choices"][0]
    return (ch["message"]["content"].strip(), d.get("model", "?"),
            ch.get("finish_reason", "?"))

# The daemon pretty-prints its schema-forced JSON: an entry costs ~20
# tokens of `{\n  "n": 1,\n  "class": "retrospective"\n},`. The first
# budget here was 16/entry, every reply truncated mid-array, every parse
# failed, and the gap-filler wrote "none" -- so a run that classified
# NOTHING reported 330 of 342 sentences as non-claims and looked plausible.
# That is the exact defect this tool exists to catch, so both halves are
# fixed: the budget fits, and a gap is never a class.
TOKENS_PER_ENTRY = 40
TOKENS_OVERHEAD = 128

def classify_batch(sents: list[str], pin: str,
                   timeout: float) -> tuple[list[str | None], str, str]:
    """-> (classes, model_id, note). A slot the reply did not cover comes
    back None -- could-not-judge -- never a class. Raises DaemonDown."""
    numbered = "\n".join(f"{i+1}. {s}" for i, s in enumerate(sents))
    out, model, finish = call_daemon(
        SYSTEM,
        f"Classify each sentence. Reply with one entry per number.\n\n{numbered}",
        pin, TOKENS_PER_ENTRY * len(sents) + TOKENS_OVERHEAD, BATCH_SCHEMA, timeout,
    )
    got, note = {}, ""
    try:
        for row in json.loads(out).get("classes", []):
            n = int(row.get("n", 0))
            c = row.get("class")
            if 1 <= n <= len(sents) and c in CLASSES:
                got[n] = c
    except json.JSONDecodeError as e:
        note = f"reply did not parse ({e}); finish_reason={finish}"
    if finish == "length":
        note = note or "reply hit the token ceiling"
    missing = len(sents) - len(got)
    if missing and not note:
        note = f"reply covered {len(got)} of {len(sents)} sentences"
    return [got.get(i + 1) for i in range(len(sents))], model, note

# ---- rows --------------------------------------------------------------

def now() -> str:
    return _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="seconds")

DECIDABLE_AT = {
    "promissory": "session-end",
    "universal": "now",
    "retrospective": "now",
    "predictive": "measurement",
    "evaluative": "never",
    "none": "never",
}

def rows_for(session_id: str, path: Path, pin: str, batch: int,
             timeout: float, use_daemon: bool) -> list[dict]:
    rows, pending = [], []   # pending: (turn, sent, row_index)
    for turn, text in turns(path):
        for sent in sentences(text):
            cls = mechanical(sent)
            row = {
                "kind": "claim", "schema": "claim-oplog/v1",
                "session": session_id, "turn": turn,
                "text": sent, "class": cls,
                "decided_by": "mechanical" if cls else None,
                "engine": None,
                "decidable_at": DECIDABLE_AT.get(cls) if cls else None,
                "status": "open", "verdict": None,
                "at": now(),
            }
            rows.append(row)
            if cls is None:
                pending.append((len(rows) - 1, sent))

    if not use_daemon:
        for idx, _ in pending:
            rows[idx].update(decided_by="not-run", **{"class": None})
        return rows

    for i in range(0, len(pending), batch):
        chunk = pending[i:i + batch]
        try:
            classes, model, note = classify_batch([s for _, s in chunk], pin, timeout)
        except DaemonDown as e:
            # Absence is reported, never defaulted (§18.3).
            for idx, _ in chunk:
                rows[idx].update({"class": None, "decided_by": "could-not-judge",
                                  "verdict": str(e)})
            continue
        for (idx, _), c in zip(chunk, classes):
            if c is None:
                rows[idx].update({"class": None, "decided_by": "could-not-judge",
                                  "engine": model, "verdict": note})
            else:
                rows[idx].update({"class": c, "decided_by": "daemon", "engine": model,
                                  "decidable_at": DECIDABLE_AT[c]})
    return rows

def append(rows: list[dict], log: Path = VERDICTS_LOG) -> None:
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("a") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")

# ---- cli ---------------------------------------------------------------

def resolve(project: str, ident: str) -> Path:
    d = TRANSCRIPTS / project
    p = Path(ident)
    if p.exists():
        return p
    hits = sorted(d.glob(f"{ident}*.jsonl"))
    if len(hits) == 1:
        return hits[0]
    raise SystemExit(f"co-oplog: {'no' if not hits else 'ambiguous'} transcript for {ident!r} in {d}")

def cmd_extract(a) -> int:
    path = resolve(a.project, a.session)
    sid = path.stem
    rows = rows_for(sid, path, a.pin, a.batch, a.timeout, not a.no_daemon)
    counts = {}
    for r in rows:
        counts[r["class"] or r["decided_by"]] = counts.get(r["class"] or r["decided_by"], 0) + 1
    if a.append:
        append(rows)
    if a.json:
        print(json.dumps({"session": sid, "rows": rows}, indent=2))
    else:
        print(f"session {sid}  turns={len({r['turn'] for r in rows})}  sentences={len(rows)}")
        for k in sorted(counts, key=lambda k: -counts[k]):
            print(f"  {k:16} {counts[k]:5}")
        adjudicable = sum(counts.get(c, 0) for c in ("promissory", "universal", "retrospective"))
        print(f"  {'adjudicable':16} {adjudicable:5}  ({100*adjudicable/max(len(rows),1):.1f}%)")
        if a.append:
            print(f"appended {len(rows)} rows to {VERDICTS_LOG}")
    return 0

def cmd_self_test(_a) -> int:
    fails = []
    def eq(got, want, what):
        if got != want:
            fails.append(f"{what}: got {got!r} want {want!r}")
    eq(mechanical("I'll wire the Stop hook next."), "promissory", "promise")
    eq(mechanical("The only impl is ClaimSearcher."), "universal", "universal")
    eq(mechanical("Should we ship it?"), "none", "question")
    eq(mechanical("The tests pass."), None, "defers to daemon")
    eq(len(sentences("short\n" + "A claim long enough to count as one sentence here.")), 1, "min length")
    eq(len(sentences("```\nthe only impl is Fake in here.rs\n```")), 0, "fences dropped")
    eq(len(sentences("| a row of a table that is long enough to be a claim | x |")), 1, "table row")
    eq(DECIDABLE_AT["evaluative"], "never", "evaluative never decidable")
    eq(TOKENS_PER_ENTRY >= 40, True, "entry budget fits the daemon's pretty JSON")
    eq("none" in CLASSES, True, "none is a real class, not a gap-filler")
    for f in fails:
        print("FAIL", f)
    print(f"co-oplog self-test: {len(fails)} failure(s)")
    return 1 if fails else 0

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--self-test", action="store_true",
                    help="run the built-in checks and exit")
    sub = ap.add_subparsers(dest="cmd")
    e = sub.add_parser("extract", help="classify one session's claims")
    e.add_argument("session")
    e.add_argument("--project", default="-Users-alexsbryan-dev-commonwealth-ai")
    e.add_argument("--pin", default=DEFAULT_PIN)
    e.add_argument("--batch", type=int, default=10)
    e.add_argument("--timeout", type=float, default=120.0)
    e.add_argument("--no-daemon", action="store_true", help="mechanical stage only")
    e.add_argument("--append", action="store_true", help="write rows to the ledger")
    e.add_argument("--json", action="store_true")
    e.set_defaults(fn=cmd_extract)
    a = ap.parse_args()
    if a.self_test:
        return cmd_self_test(a)
    if not a.cmd:
        ap.print_help()
        return 2
    return a.fn(a)

if __name__ == "__main__":
    sys.exit(main())
