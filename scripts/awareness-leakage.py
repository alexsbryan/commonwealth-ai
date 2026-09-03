#!/usr/bin/env python3
"""Leakage rate of the always-on notes channel: does a recorded rejection reach
the session about to repeat it?

WHY THIS EXISTS. `.claude/hooks/tests/inject-notes.sh` asserts what the hook
ASKS FOR. That is a contract test and it cannot tell you whether the ranker
ANSWERS. Without this number, tuning the injector is unfalsifiable — which is
the state it was in when `kinds` excluded `attempt` for its whole life and no
check anywhere went red (ARCH §18.1).

THE METHOD, and its one honest weakness. For each live `attempt` note we build
a query from DOMAIN IDENTIFIERS — the note's own `files`/`symbols` metadata
when it has them, else file paths and long identifiers mined from the body.
The claim is that a session about to repeat that mistake would independently
have those tokens in its prompt or working context: it is editing
`grounding/mod.rs`, it is asking about `MAX_CHUNK_CHARS`. It is NOT the note's
rhetorical framing ("KILLED:", "did not answer its question"), which the
session has no way to produce.

This is still a leave-one-in measurement — the note is in the corpus it is
searched against — so read the ABSOLUTE recall as an upper bound. The DELTA
between configurations is the trustworthy half, and it is the half the
decision rests on: under the old configuration `attempt` was not in `kinds`,
so recall is 0 by construction at any K, under any query.

  scripts/awareness-leakage.py                # both configs, recall@5/10/20
  scripts/awareness-leakage.py --kind invariant
  scripts/awareness-leakage.py --show-misses  # what the ranker cannot reach
"""
from __future__ import annotations
import argparse, json, os, re, sqlite3, sys, urllib.request

DB = os.path.expanduser("~/.svrnmesh/notes.db")
PORT = os.environ.get("SOVEREIGN_PORT", "9741")
KS = (5, 10, 20)

# File paths first (most session-plausible), then CamelCase types, then long
# snake_case. Rhetorical words are excluded by construction: they are short,
# lowercase and unhyphenated, and none of these patterns match them.
PATHS = re.compile(r"[A-Za-z_][A-Za-z0-9_/.-]*\.(?:rs|py|sh|md|toml|json|mjs)")
CAMEL = re.compile(r"\b[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]+)+\b")
SNAKE = re.compile(r"\b[a-z][a-z0-9]*(?:_[a-z0-9]+){1,}\b")


def read_notes(args):
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": "read_notes", "arguments": args}}).encode()
    req = urllib.request.Request(f"http://localhost:{PORT}/mcp", data=payload,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(json.load(r)["result"]["content"][0]["text"]).get("notes") or []


def query_for(note):
    """Tokens a session would independently hold, never the note's framing."""
    terms = []
    for field in ("files", "symbols"):
        try:
            terms += [t for t in json.loads(note.get(field) or "[]") if t]
        except Exception:
            pass
    body = note.get("content") or ""
    terms += PATHS.findall(body)[:6]
    terms += CAMEL.findall(body)[:6]
    terms += [t for t in SNAKE.findall(body) if len(t) >= 8][:6]
    seen, out = set(), []
    for t in terms:
        k = t.lower()
        if k not in seen:
            seen.add(k); out.append(t)
    return " ".join(out[:12])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kind", default="attempt")
    ap.add_argument("--show-misses", action="store_true")
    # Operational records (anchored to the seat's coordination rail) are
    # WITHHELD from ordinary sessions by the daemon — the "N operational
    # record(s) withheld" line in every injection. Scoring them measures a
    # population the mechanism cannot deliver, so they are out by default.
    ap.add_argument("--include-operational", action="store_true")
    # A floor, so this can become a warn_gate once it has run across enough
    # real sessions to know its own false-positive behaviour. Not wired yet:
    # it needs the daemon, and pre-push must not require one.
    ap.add_argument("--min-recall", type=float, default=None,
                    help="exit 1 if recall@10 falls below this percentage")
    a = ap.parse_args()

    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    sql = ("SELECT id, content, files, symbols FROM notes "
           "WHERE kind = ? AND retired_at IS NULL AND scope = 'global'")
    if not a.include_operational:
        sql += " AND related_entity IS NULL"
    rows = db.execute(sql, (a.kind,)).fetchall()
    notes = [{"id": r[0], "content": r[1], "files": r[2], "symbols": r[3]} for r in rows]
    label = "all" if a.include_operational else "injectable (operational excluded)"
    print(f"live {a.kind} notes, {label}: {len(notes)}")

    NEW = {"kinds": ["invariant", "decision", a.kind], "scope": ["global"]}
    OLD = {"kinds": ["invariant", "decision"], "scope": ["global"]}

    hits = {"new": {k: 0 for k in KS}, "old": {k: 0 for k in KS}}
    scored, thin, misses = 0, 0, []
    for n in notes:
        q = query_for(n)
        if len(q.split()) < 3:
            thin += 1
            continue
        scored += 1
        got = read_notes({**NEW, "limit": max(KS), "query": q})
        ids = [g.get("id") for g in got]
        rank = ids.index(n["id"]) + 1 if n["id"] in ids else None
        for k in KS:
            if rank and rank <= k:
                hits["new"][k] += 1
        # OLD could not return this kind at all; verify rather than assume.
        old_ids = [g.get("id") for g in read_notes({**OLD, "limit": max(KS)})]
        if n["id"] in old_ids:
            for k in KS:
                if old_ids.index(n["id"]) + 1 <= k:
                    hits["old"][k] += 1
        if rank is None and a.show_misses:
            misses.append((n["id"][:8], q[:90],
                           " ".join((n["content"] or "").split())[:70]))

    print(f"scored: {scored}   skipped (fewer than 3 usable terms): {thin}\n")
    print(f"{'config':10s} " + "".join(f"recall@{k:<7}" for k in KS))
    print("-" * 44)
    for cfg in ("old", "new"):
        cells = "".join(f"{100.0*hits[cfg][k]/scored:>6.1f}%   " if scored else "   n/a   "
                        for k in KS)
        print(f"{cfg:10s} {cells}")
    if scored:
        d = 100.0 * (hits['new'][10] - hits['old'][10]) / scored
        print(f"\ndelta @10: {d:+.1f} percentage points")
    if misses:
        print(f"\nUNREACHED ({len(misses)}) — the ranker cannot find these from their own identifiers:")
        for i, q, c in misses:
            print(f"  {i}  q=[{q}]\n      {c}")

    if a.min_recall is not None:
        got = 100.0 * hits["new"][10] / scored if scored else 0.0
        if got < a.min_recall:
            print(f"\nFLOOR: recall@10 {got:.1f}% is below {a.min_recall:.1f}%")
            sys.exit(1)
        print(f"\nFLOOR: recall@10 {got:.1f}% >= {a.min_recall:.1f}% — ok")


main()
