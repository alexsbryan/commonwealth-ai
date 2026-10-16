"""Score ward-story's queried deal records against the enron-ward gold.

Reads only the public surface: `sovereign enrich atlas-query --typed --json`;
or, with `--records=<file>`, a one-shot's records (runs/ceiling/oneshot.py),
put in the same row shape so one matcher judges both arms.
A placed statement matches a gold stage update when it comes from the same
message and shares most of the gold quote's words. From those matches:
splits (one gold deal on several records), false merges (one record holding
statements of several gold deals), omissions (a gold deal with no record).

runs/slice1/instruments/score_ward.py with three instrument fixes (2026-10-13; it lived in a session
scratchpad as score_fixed.py until loop.sh made it the loop's one scorer), each switchable with --fixes=<comma list> (default all; `none` reproduces score_ward.py):
  cap  every stage_update statement is read (the typed enumerate stops at 64 rows), partitioned by
       stage, refusing if a partition is still capped or the union misses a statement;
  msg  a statement matches a gold update only from the same message (document), as said above,
       not from any message in the same folder;
  own  a statement's match text is its own statement and its own stage evidence (its reading in the
       stage fold), not every fold's evidence pooled (counterparty emails, delivery points, ...);
  fold a one-shot record's stage is its latest-dated statement's, as the product folds a record (the recipe's
       deal_stage: the latest report by document date; statements of that date that disagree leave none),
       not the stage the model declares for the record (2026-10-14: the declared stage served deals whose
       statements end elsewhere). A record none of whose statements states a stage keeps the declared one: the
       one-shot wrote no per-statement stage at all on world-s4.
  best a statement is credited to a deal only when no other deal's gold sentence on its message is held more
       wholly in it (the 60% rule's own measure: the share of the gold sentence's words the statement holds); a
       tie keeps both, as when a statement cites a line holding both deals' sentences whole, or gold files one
       sentence under two deals. Without it "The Cal 01 Kingsgate 4k/d call option is being replaced" (held
       whole) is credited to the Q1 deal too, because "the Q1 Kingsgate 4k/d call option at .50" has four of
       its six words in it (world-s2 verent/3., 2026-10-15). Measured from the statement's side instead, the
       rule broke world-s3 quanova/6., one unpunctuated line holding both deals' sentences whole.
Everything else is score_ward.py's. The report also carries `matches` {claim: [gold update index]}.
"""
import inspect, json, os, re, subprocess, sys
from email.utils import parsedate_to_datetime
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import corpora, misses

RECORDS = next((a.split("=", 1)[1] for a in sys.argv[1:] if a.startswith("--records=")), None)
_fx = next((a.split("=", 1)[1] for a in sys.argv[1:] if a.startswith("--fixes=")), "cap,msg,own,fold,best")
FIXES = set() if _fx == "none" else set(_fx.split(","))
assert FIXES <= {"cap", "msg", "own", "fold", "best"}, FIXES
sys.argv = [a for a in sys.argv if not a.startswith("--records=") and not a.startswith("--fixes=")]
MSGIDS = sys.argv[1]
OUT = sys.argv[2] if len(sys.argv) > 2 else None
CORPUS = sys.argv[3] if len(sys.argv) > 3 else "ward-story"
# Optional: another gold dir and its folders (comma-separated), e.g. a generated world's.
GOLD = sys.argv[4] if len(sys.argv) > 4 else "/Users/alexsbryan/.svrnmesh/bench-corpora/enron-ward/gold"
FOLDERS = sys.argv[5].split(",") if len(sys.argv) > 5 else ("tep", "pasadena")


def q(typed):
    r = subprocess.run(["sovereign", "enrich", "atlas-query", CORPUS, "--typed", json.dumps(typed), "--json"],
                       capture_output=True, text=True, check=True)
    return json.loads(r.stdout)


STOP = set("a an the and or of to in on for at by with from as is are was were be been it its this that these those we you i he she they them our your my me us if so but not no do does did have has had will would can could should please any all also there here than then just about into over under up out".split())


def words(s):
    return {w for w in re.findall(r"[a-z0-9]+", (s or "").lower()) if w not in STOP and len(w) > 1}


def slug(mid):
    return re.sub(r"[^a-z0-9]+", "-", mid.lower()).strip("-")


msgids = json.load(open(MSGIDS))
doc_of_file = {k: slug(v) for k, v in msgids.items() if v}
file_of_doc = {v: k for k, v in doc_of_file.items()}

gold_deals, gold_updates = {}, []
for folder in FOLDERS:
    g = json.load(open(f"{GOLD}/{folder}.json"))
    for d in g["deals"]:
        gold_deals[d["id"]] = dict(d, folder=folder)
    for s in g["stage_updates"]:
        gold_updates.append(dict(s, folder=folder, doc=doc_of_file.get(f"{folder}/{s['file']}")))


def current_stage(deal_id):
    ups = [u for u in gold_updates if u["deal"] == deal_id]
    if not ups:
        return None
    ups.sort(key=lambda u: parsedate_to_datetime(re.sub(r"\s*\(.*\)$", "", u["date"])))
    return ups[-1]["stage"]




def date_of(f):
    """A cited file's message date, from its Date header in the corpus story (gold's sibling dir)."""
    p = os.path.join(os.path.dirname(GOLD), "story", f)
    if not os.path.exists(p): return None
    m = re.search(r"^Date:\s*(.+)$", open(p, errors="replace").read(), re.M)
    try: return parsedate_to_datetime(re.sub(r"\s*\(.*\)$", "", m.group(1).strip())) if m else None
    except (TypeError, ValueError): return None


def from_records(path):
    """A one-shot's deal records as atlas rows: each stage_update claim becomes a stage fold reading
    whose document is the cited file itself."""
    deals, claims = [], {}
    for i, r in enumerate(json.load(open(path))["records"]):
        if r.get("type") != "deal":
            continue
        readings = []
        for j, c in enumerate(r.get("claims", [])):
            if c.get("kind") != "stage_update":
                continue
            cid, f = f"r{i}c{j}", (c.get("file") or "").strip("/")
            f = f if f.endswith(".") else f + "."  # the corpus names files "23."; a model cites "23"
            file_of_doc.setdefault(f, f)
            claims[cid] = {"name": c.get("quote") or "", "record": {"attributes": c.get("attributes") or {}}}
            readings.append({"claim": cid, "document": f, "evidence": c.get("quote")})
        attrs = dict(r.get("attributes") or {})
        if "fold" in FIXES:
            dated = [(date_of(rd["document"]), (claims[rd["claim"]]["record"]["attributes"] or {}).get("stage")) for rd in readings]
            dated = [(d, st) for d, st in dated if d and st]
            last = {st for d, st in dated if d == max(d for d, _ in dated)} if dated else set()
            if dated: attrs["stage"] = next(iter(last)) if len(last) == 1 else None
        deals.append({"atom_id": f"r{i}", "name": r.get("name") or "", "record": {
            "attributes": attrs, "folds": {"stage": {"readings": readings}},
            "refs": [{"attribute": "counterparty", "name": str(attrs.get("counterparty"))}]}})
    return deals, claims


if RECORDS:
    deals, claims = from_records(RECORDS)
else:
    deals = q({"target_type": "deal"})["rows"]
    if "cap" not in FIXES:
        claims = {r["atom_id"]: r for r in q({"target_type": "stage_update"})["rows"]}
    else:
        # As $SP/score_ward_all.py: partition by stage so every statement is read.
        claims = {}
        STAGES = ("lead", "proposal", "negotiating", "won", "lost")
        parts = [[{"attribute": "stage", "op": "eq", "value": v}] for v in STAGES]
        parts.append([{"attribute": "stage", "op": "eq", "value": v, "negate": True} for v in STAGES])
        for fs in parts:
            ans = q({"target_type": "stage_update", "filters": fs})
            if any("showing the first" in n for n in ans.get("notes", [])):
                sys.exit(f"stage_update partition {fs} is still capped: {ans['notes']}")
            claims.update({r["atom_id"]: r for r in ans["rows"]})
        total = q({"target_type": "stage_update"})["matched"]
        if len(claims) != total:
            sys.exit(f"read {len(claims)} stage_update statements of {total}")

# Each deal's placed statements, from its stage fold readings and its referrers.
placed = {}
for d in deals:
    rec = d["record"]
    ids = set()
    for f in (rec.get("folds") or {}).values():
        for rd in f.get("readings", []):
            if rd.get("claim"):
                ids.add(rd["claim"])
    for rb in rec.get("referenced_by", []):
        if rb.get("type") == "stage_update":
            ids.add(rb["id"])
    # Only stage updates answer to gold stage updates; a statement of no
    # declared kind (product, term readings) is not scored here.
    placed[d["atom_id"]] = {i for i in ids if i in claims}

claim_doc, claim_text = {}, {}
for d in deals:
    for fname, f in (d["record"].get("folds") or {}).items():
        for rd in f.get("readings", []):
            if rd.get("claim") and rd.get("document"):
                claim_doc[rd["claim"]] = rd["document"]
            # own: a stage_update's own evidence is its stage reading; other folds' readings of the
            # same claim carry what it said about other fields (an email, a delivery point).
            if rd.get("claim") and isinstance(rd.get("evidence"), str) and ("own" not in FIXES or fname == "stage"):
                claim_text.setdefault(rd["claim"], set()).add(rd["evidence"])


def message_of(doc):
    """The message a cited document is: an atlas document id is one already; a cited file
    ('folder/file.') is the message msgids.json names for it, or itself when it names none."""
    return doc_of_file.get(doc, doc) if doc else None


_cache = {}


def match(cid):
    """The gold stage updates this statement is, by message and quote."""
    if cid in _cache:
        return _cache[cid]
    c = claims.get(cid)
    text = " ".join([c["name"] if c else ""] + sorted(claim_text.get(cid, ())))
    folder = (file_of_doc.get(claim_doc.get(cid)) or "/").split("/")[0]
    msg = message_of(claim_doc.get(cid))
    hits = []
    for i, u in enumerate(gold_updates):
        if "msg" in FIXES:
            if msg is None or (u["doc"] or f"{u['folder']}/{u['file']}") != msg:
                continue
        # Same account; a duplicate message repeats a quote gold files once.
        elif folder and u["folder"] != folder:
            continue
        gw, cw = words(u["quote"]), words(text)
        if len(gw) >= 2 and len(gw & cw) / len(gw) >= 0.6:
            hits.append(i)
    if "best" in FIXES and hits:
        cw = words(text)
        held = {i: len(words(gold_updates[i]["quote"]) & cw) / len(words(gold_updates[i]["quote"])) for i in hits}
        top = max(held.values())
        deals_kept = {gold_updates[i]["deal"] for i in hits if held[i] >= top - 1e-9}
        hits = [i for i in hits if gold_updates[i]["deal"] in deals_kept]
    _cache[cid] = hits
    return hits


report = {"deals": [], "gold": {}, "unmatched_statements": [], "unknown_docs": sorted(
    {v for v in claim_doc.values() if v not in file_of_doc})}
from collections import Counter
gold_to_records = {g: set() for g in gold_deals}
for d in deals:
    rec = d["record"]
    cp = [r.get("key") or r.get("name") for r in rec.get("refs", []) if r.get("attribute") == "counterparty"]
    sets = []
    for cid in placed[d["atom_id"]]:
        hs = match(cid)
        if not hs:
            report["unmatched_statements"].append({"deal": d["atom_id"], "claim": cid,
                                                   "file": file_of_doc.get(claim_doc.get(cid)),
                                                   "text": (claims.get(cid) or {}).get("name", "")[:140],
                                                   "stage": ((claims.get(cid) or {}).get("record") or {}).get("attributes", {}).get("stage")})
            continue
        sets.append({gold_updates[h]["deal"] for h in hs})
    # The record is the gold deal(s) most of its statements are about; a
    # statement gold files under two deals votes for both.
    votes = Counter(g for st in sets for g in st)
    top = max(votes.values()) if votes else 0
    assigned = {g for g, n in votes.items() if n == top} if votes else set()
    strays = [sorted(st) for st in sets if not (st & assigned)]
    for g in assigned:
        gold_to_records[g].add(d["atom_id"])
    report["deals"].append({"id": d["atom_id"], "name": d["name"][:120], "counterparty": cp,
                            "stage": rec.get("attributes", {}).get("stage"),
                            "statements": len(placed[d["atom_id"]]), "matched": len(sets),
                            "gold": sorted(assigned), "strays": strays})

# Each gold stage update: did a statement matching it land on a record of its own gold deal?
attached = {}
for d in deals:
    for cid in placed[d["atom_id"]]:
        for h in match(cid):
            attached.setdefault(h, set()).add(d["atom_id"])
report["updates"] = [{"folder": u["folder"], "file": u["file"], "deal": u["deal"], "stage": u["stage"],
                      "attached": bool(attached.get(i, set()) & gold_to_records[u["deal"]])} for i, u in enumerate(gold_updates)]

for g, gd in gold_deals.items():
    recs = gold_to_records[g]
    report["gold"][g] = {"records": sorted(recs), "gold_stage": current_stage(g),
                         "record_stages": [next(x["stage"] for x in report["deals"] if x["id"] == r) for r in sorted(recs)],
                         "description": gd["description"][:100]}

merged = [x for x in report["deals"] if x["strays"]]
split = {g: v for g, v in report["gold"].items() if len(v["records"]) > 1}
omitted = [g for g, v in report["gold"].items() if not v["records"]]
report["summary"] = {
    "deal_records": len(deals), "gold_deals": len(gold_deals),
    "gold_deals_found": len(gold_deals) - len(omitted), "omitted": omitted,
    "split": {g: len(v["records"]) for g, v in split.items()},
    "false_merges": [{"record": x["id"], "is": x["gold"], "also": x["strays"]} for x in merged],
    "records_matching_no_gold": len([x for x in report["deals"] if not x["gold"]]),
    "found_with_right_stage": sorted(g for g, v in report["gold"].items() if v["records"] and v["gold_stage"] in v["record_stages"]),
    "stage_updates_placed": sum(len(v) for v in placed.values()),
    "stage_updates_matching_no_gold": len(report["unmatched_statements"]),
    "gold_updates": len(gold_updates),
    "gold_updates_matched": len({h for d in deals for cid in placed[d["atom_id"]] for h in match(cid)}),
}
report["fixes"] = sorted(FIXES)
# This file, its fixes, the served rule that counts a deal (misses.served) and the gold it held records to: the
# stamp table.py checks.
report["summary"]["judge"] = corpora.judge_id([open(__file__).read(), inspect.getsource(misses.served), ",".join(sorted(FIXES))], [f"{GOLD}/{f}.json" for f in FOLDERS])
report["matches"] = {cid: match(cid) for d in deals for cid in sorted(placed[d["atom_id"]])}
report["placed"] = {d["atom_id"]: sorted(placed[d["atom_id"]]) for d in deals}
report["claim_doc"] = {cid: claim_doc.get(cid) for cid in report["matches"]}
report["claim_match_text"] = {cid: " ".join([(claims.get(cid) or {}).get("name", "")] + sorted(claim_text.get(cid, ()))) for cid in report["matches"]}
s = json.dumps(report, indent=1)
if OUT:
    open(OUT, "w").write(s)
print(json.dumps(report["summary"], indent=1))
