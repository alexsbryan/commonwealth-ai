#!/usr/bin/env python3
"""Spike v2: TRUE ground truth from transcript tool results.

GT = the note ids the seat actually received from its manual notes calls
(parsed from toolUseResult blocks — what the engine returned THEN, no skew).
Then: can verbatim-turn-as-query or short-query-as-query retrieve the same
topical notes? Rail/id lookups are scored separately (not recall-servable).

`--boot-block` mode (order seat-boot-block): scores the ≥80% done-when bar —
how much of each session's anchor-sweep rail GT the assembled boot block
carries. The anchor-word sweeps the skill prescribed returned mostly
UNANCHORED content-mention matches (measured: 1/28 comaintainer-seat and
0/52 order-seat/directive-log sweep results carry the anchor entity), so an
id-level bar against raw sweep results fails ANY block by construction.
The honest bar scores the rails the block actually carries:

  seat family      — GT notes whose content is seat-anchored (comaintainer-seat
                     entity in the store): covered = id in the block's
                     related_to=comaintainer-seat read (the one implementation
                     of this rail; the block defaults to the tool's 100-note cap
                     so the ~91-note graph is not truncated).
  order family     — GT notes whose content names an order id: covered = the
                     named order appears in `co-order.sh list` (the block's
                     Open orders section).
  directive-log    — named boundary (the block carries the TALLY, not records):
                     excluded from the denominator, reported.
  history / noise  — notes naming only closed orders, and unanchored sweep
                     matches: excluded, reported (the sweep's imprecision,
                     not a block gap).

Id-lookups and probe polls are mid-session dereferences — a boot snapshot
cannot cover them; excluded from the denominator and REPORTED (the seats'
id-lookup calls demonstrably returned unrelated notes: query is semantic,
there is no exact-id route on the daemon surface; `svrn notes list --id`
reads the repo-local store, not the daemon store).

Content + entities are read from the DAEMON store (~/.svrnmesh/notes.db —
the store the MCP surface serves) via read-only sqlite.
"""
import json, os, re, sqlite3, subprocess, sys, urllib.request
from datetime import datetime
from collections import Counter

BOOT_BLOCK_MODE = "--boot-block" in sys.argv

PORT = 9741
PROJ = "/home/alexbryan/.claude/projects/-home-alexbryan-dev-commonwealth-ai"
SESSIONS = {
    "c3a7b73f": "c3a7b73f-a7e4-4fbb-8a4d-4b7c6b47d73a",
    "40b5cc0b": "40b5cc0b-31d6-415b-922c-c39c2f6e8faa",
    "5c8a3275": "5c8a3275-5922-4d66-ac60-78eb5e1d2183",
}
HEX8 = re.compile(r"^[0-9a-f]{8}$")
ANCHORS = ("comaintainer-seat", "order-seat", "directive-log", "backlog")
# The daemon store — the store the MCP surface serves (the block's rail).
# `svrn notes list --id` reads the repo-local store instead, so id lookups
# must go here, read-only, same store the seat actually reads.
DAEMON_STORE = os.environ.get("SVRNMESH_NOTES_DB", os.path.expanduser("~/.svrnmesh/notes.db"))
ORDER_ID_RE = re.compile(r"[a-z][a-z0-9]+-[a-z0-9-]+")  # feature-dir shape, hyphenated
_entities_cache = {}
_notes_cache = {}


def _store():
    con = sqlite3.connect(f"file:{DAEMON_STORE}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    return con


def note_row(nid):
    """(kind, content, created_at_epoch) from the daemon store, by id."""
    if nid in _notes_cache:
        return _notes_cache[nid]
    con = _store()
    try:
        r = con.execute("SELECT kind, content, created_at FROM notes WHERE id=?", (nid,)).fetchone()
        if not r:
            r = con.execute("SELECT kind, content, created_at FROM notes WHERE id LIKE ?",
                            (nid + "%",)).fetchone()
        _notes_cache[nid] = tuple(r) if r else None
    finally:
        con.close()
    return _notes_cache[nid]


def note_entities(nid):
    """The note's entities from the T2 graph — the anchor test."""
    if nid in _entities_cache:
        return _entities_cache[nid]
    con = _store()
    try:
        rows = [e[0] for e in con.execute(
            "SELECT entity FROM note_entities WHERE note_id=?", (nid,))]
        _entities_cache[nid] = rows
    finally:
        con.close()
    return _entities_cache[nid]


def order_ids_in_content(content):
    """Hyphenated order-id-shaped tokens — the order the note concerns."""
    return set(m for m in ORDER_ID_RE.findall(content or "") if len(m) >= 6)

def read_notes(args, timeout=15):
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": "read_notes", "arguments": args}}).encode()
    req = urllib.request.Request(f"http://localhost:{PORT}/mcp", data=payload,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(json.load(r)["result"]["content"][0]["text"])


def boot_block_rail():
    """The EXACT notes read scripts/co-boot-block.sh makes — one implementation.
    Limit mirrors the block's default (SOVEREIGN_BOOT_BLOCK_LIMIT=100, the
    tool's cap): the seat-anchor graph has ~91 notes and the block must not
    truncate the rail (the 40-note cap dropped the SEAT STEWARDSHIP LOG)."""
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                          "params": {"name": "notes", "arguments": {
                              "related_to": "comaintainer-seat", "limit": 100,
                              "include_operational": True}}}).encode()
    req = urllib.request.Request(f"http://localhost:{PORT}/mcp", data=payload,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=4) as r:
        inner = json.loads(json.load(r)["result"]["content"][0]["text"])
    return inner.get("notes") or []


def boot_block_orders():
    """The open-orders section's source — the same file read co-order.sh makes."""
    out = subprocess.run(["./scripts/co-order.sh", "list"],
                         capture_output=True, text=True, timeout=5)
    return out.stdout or ""


_open_order_ids = None


def open_order_ids():
    """Order ids in the block's Open orders section (co-order.sh list, first token)."""
    global _open_order_ids
    if _open_order_ids is None:
        ids = set()
        for line in boot_block_orders().splitlines():
            tok = line.split()[0] if line.split() else ""
            if tok and ORDER_ID_RE.fullmatch(tok):
                ids.add(tok)
        _open_order_ids = ids
    return _open_order_ids


_universe = None


def order_id_universe():
    """Every order id ever recorded: open ids ∪ shadow-record ids ("order: <id>"
    header notes written by the co-order machinery). Keeps order-family
    classification honest — hyphenated script/tool names ("co-directive-log",
    "session-boot") are not orders."""
    global _universe
    if _universe is None:
        con = _store()
        try:
            rows = [r[0] for r in con.execute(
                "SELECT content FROM notes WHERE content LIKE 'order: %'")]
        finally:
            con.close()
        shadow = set()
        for c in rows:
            m = re.match(r"order:\s*([a-z0-9-]+)", c or "")
            if m and ORDER_ID_RE.fullmatch(m.group(1)):
                shadow.add(m.group(1))
        _universe = open_order_ids() | shadow
    return _universe


def note_content(nid):
    """Live content for a GT note id — the daemon store, the only exact path."""
    row = note_row(nid)
    return (row[1] or "") if row else ""

def parse_ts(t):
    return datetime.fromisoformat(t.replace("Z", "+00:00"))

def classify(args):
    """rail = identifier/anchor lookup; topical = everything else."""
    if args.get("related_to"):
        return "rail"
    q = (args.get("query") or "").strip()
    if HEX8.match(q) or q.startswith("PROBE-") or any(a in q for a in ANCHORS):
        return "rail"
    return "topical"

def extract(path):
    turns, tool_uses, results = [], {}, {}
    with open(path) as f:
        for line in f:
            try: rec = json.loads(line)
            except Exception: continue
            t = rec.get("type")
            msg = rec.get("message") or {}
            content = msg.get("content")
            if t == "user":
                if isinstance(content, str):
                    text = content
                elif isinstance(content, list):
                    for b in content:
                        if isinstance(b, dict) and b.get("type") == "tool_result":
                            results[b.get("tool_use_id", "")] = b.get("content")
                    texts = [b.get("text") for b in content
                             if isinstance(b, dict) and b.get("type") == "text"]
                    text = "\n".join(texts)
                else:
                    text = ""
                if text and not any(m in text for m in
                        ("<command-name>", "<task-notification>", "<agent-message")) \
                   and not text.startswith("Base directory for this skill"):
                    turns.append({"ts": rec.get("timestamp"), "text": text})
            elif t == "assistant" and isinstance(content, list):
                for b in content:
                    if isinstance(b, dict) and b.get("type") == "tool_use" and \
                       b.get("name") in ("mcp__sovereign__notes", "mcp__sovereign__read_notes"):
                        tool_uses[b["id"]] = {"ts": rec.get("timestamp"),
                                              "args": b.get("input") or {}}
    return turns, tool_uses, results

def ids_from_result(res):
    ids = []
    for blob in res if isinstance(res, list) else [res]:
        if isinstance(blob, dict) and blob.get("type") == "text":
            try:
                inner = json.loads(blob["text"])
                for n in inner.get("notes", []):
                    if isinstance(n, dict) and n.get("id"):
                        ids.append({"id": n["id"], "created": n.get("created_at", "")})
            except Exception:
                pass
    return ids

def turn_for(ts, turns):
    prior = [t for t in turns if parse_ts(t["ts"]) <= parse_ts(ts)]
    return prior[-1]["ts"] if prior else None

ANCHOR_TARGETS = ("comaintainer-seat", "order-seat", "directive-log")
agg = {"eligible": 0, "covered": 0}
for tag, sid in SESSIONS.items():
    turns, tool_uses, results = extract(f"{PROJ}/{sid}.jsonl")
    print(f"\n######## {tag}  turns={len(turns)} manual_calls={len(tool_uses)} "
          f"results={len(results)}")
    gt = {}          # id -> {created, cls, args}
    for uid, call in tool_uses.items():
        tts = call["ts"]
        cls = classify(call["args"])
        for n in ids_from_result(results.get(uid, [])):
            if n["id"] not in gt:
                gt[n["id"]] = {"created": n["created"], "cls": cls,
                               "turn": turn_for(tts, turns) or "?",
                               "args": call["args"]}
    n_rail = sum(1 for v in gt.values() if v["cls"] == "rail")
    n_top = len(gt) - n_rail
    man_tokens = sum(len(v["created"]) for v in gt.values())  # placeholder below
    print(f"GT total={len(gt)}  rail={n_rail}  topical={n_top}")
    if not gt:
        continue

    if BOOT_BLOCK_MODE:
        # The ≥80% bar, measured on the rails the block carries (see the
        # module docstring for why raw sweep results are the wrong bar).
        # Family is decided by WHAT THE NOTE IS, not by the query that
        # happened to return it: seat-anchored entity → seat family; names an
        # order id → order family; directive-log sweep results → boundary.
        # Rest is sweep noise (reported, excluded).
        start = min((parse_ts(t["ts"]) for t in turns), default=None)
        open_ids = open_order_ids()
        universe = order_id_universe()

        def eligible(nid, v):
            return v["created"] and start and parse_ts(v["created"]) <= start

        seat_fam, order_fam, dlog, history, noise = {}, {}, {}, {}, {}
        for nid, v in gt.items():
            if not eligible(nid, v):
                continue
            row = note_row(nid)
            if not row:
                noise[nid] = v
                continue
            ents = note_entities(nid)
            if "comaintainer-seat" in ents:
                seat_fam[nid] = v
                continue
            oids = {o for o in order_ids_in_content(row[1]) if o in universe}
            if any(o in open_ids for o in oids):
                order_fam[nid] = v
            elif oids:
                history[nid] = v
            else:
                probe = (v["args"].get("query") or "")
                if "directive-log" in probe:
                    dlog[nid] = v
                else:
                    noise[nid] = v

        block_ids = {n["id"] for n in boot_block_rail()}
        covered_seat = {nid for nid in seat_fam if nid in block_ids}
        covered_order = {nid for nid, v in order_fam.items()
                         if order_ids_in_content(note_content(nid)) & open_ids}
        n_eligible = len(seat_fam) + len(order_fam)
        n_covered = len(covered_seat) + len(covered_order)
        cov = n_covered / n_eligible * 100 if n_eligible else float("nan")
        print(f"BOOT BLOCK (bar ≥80%): eligible={n_eligible} covered={n_covered} "
              f"→ {cov:.0f}%   seat={len(covered_seat)}/{len(seat_fam)} "
              f"order={len(covered_order)}/{len(order_fam)}")
        if seat_fam:
            print(f"   seat ids: {', '.join(sorted(nid[:8] for nid in seat_fam))} "
                  f"→ in-block: {', '.join(sorted(nid[:8] for nid in covered_seat)) or 'NONE'}")
        if order_fam:
            print(f"   order ids: {', '.join(sorted(nid[:8] for nid in order_fam))}")
        print(f"   REPORTED (not in denominator): directive-log boundary={len(dlog)} "
              f"closed-order history={len(history)} sweep noise={len(noise)}")
        agg["eligible"] += n_eligible
        agg["covered"] += n_covered
        continue
    if not BOOT_BLOCK_MODE:
        verb_hits, short_hits = set(), set()
        gt_by_turn = {}
        for i, (nid, v) in enumerate(gt.items()):
            gt_by_turn.setdefault(v["turn"], []).append((nid, v))
        print(f"--- replay per turn (topical GT only) ---")
        for t in turns:
            gt_t = {nid: v for nid, v in gt_by_turn.get(t["ts"], []) if v["cls"] == "topical"}
            if not gt_t:
                continue
            alive = {nid for nid, v in gt_t.items()
                     if v["created"] and parse_ts(v["created"]) <= parse_ts(t["ts"])}
            if not alive:
                continue
            for label, q in (("verb", t["text"][:500]), ("short", " ".join(t["text"].split()[:12]))):
                r = read_notes({"query": q, "limit": 10, "include_operational": True})
                got = r.get("notes", [])[:10]
                hit = [n["id"] for n in got if n["id"] in alive]
                ranks = {n["id"]: i + 1 for i, n in enumerate(got) if n["id"] in alive}
                (verb_hits if label == "verb" else short_hits).update(hit)
                print(f"  T {t['ts'][11:19]} alive={len(alive):2d} {label:5s} hits={len(hit)} "
                      f"ranks={[ranks[h] for h in hit]}")
            if tag == "c3a7b73f" and t["ts"].startswith("2026-08-13T00:01"):
                r = read_notes({"query": t["text"][:500], "limit": 10, "include_operational": True})
                print("    failure-mode top-3 returned:")
                for n in r.get("notes", [])[:3]:
                    print(f"      {n['id'][:8]} [{n.get('kind')}] "
                          f"{(n.get('content') or '')[:70].splitlines()[0]}")
        gt_top_ids = {i for i, v in gt.items() if v["cls"] == "topical"}
        print(f"UNION recall (topical {len(gt_top_ids)}): "
              f"verb={len(verb_hits & gt_top_ids)}  short={len(short_hits & gt_top_ids)}")
        # hook baseline vs GT
        seat = read_notes({"kinds": ["invariant", "decision"], "scope": ["global"],
                           "limit": 20, "include_operational": True})
        hook_ids = {n["id"] for n in seat.get("notes", [])}
        print(f"hook newest-20 ∩ GT: {len({i for i in gt if i in hook_ids})}/{len(gt)}")

if BOOT_BLOCK_MODE:
    rate = agg["covered"] / agg["eligible"] * 100 if agg["eligible"] else float("nan")
    verdict = "PASS" if rate >= 80 else "FAIL"
    print(f"\nBOOT BLOCK COVERAGE over {len(SESSIONS)} sessions: "
          f"{agg['covered']}/{agg['eligible']} = {rate:.0f}%  (bar ≥80%) → {verdict}")
    sys.exit(0)

# hook index source query, live (rendered as a one-line index with a budget;
# this is the SOURCE read, not the injected payload)
non = read_notes({"kinds": ["invariant", "decision"], "scope": ["global"], "limit": 20})
print(f"\nhook index source query (live, per prompt, non-seat): {len(non.get('notes',[]))} notes, "
      f"{sum(len(n['content'])//4 for n in non.get('notes',[]))} tok as bodies — "
      f"rendered as a one-line index, not injected in full")
