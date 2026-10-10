"""GVC's stage ladder from one end-to-end run: the reader's line-level statements aligned to gold's token mentions.

    RUN holds recorded/atoms.json (a scaffold leg) or data/indexes/<corpus>/atlas/atoms.json (exactly one), and
    job.log, job.debug.log for the trace (their recorded half; ladder.run_atlas, ladder.trace)

The end-to-end reader states a claim over whole lines (passes.rs: a statement is a run of located lines, its
evidence the raw text from the first line's start to the last's end); gold marks event mentions as token spans
(cdcr/prepare.py: one sentence per line, character offsets). ALIGNMENT: a claim's evidence text (its `anchor`) is
found in the document body, exactly once (whitespace folded when the exact text is absent; twice is ambiguous and
counted, never guessed); its cited lines are the body lines that span overlaps; it covers a mention when the
mention's characters lie inside those lines. A claim's document is its `document_id` stamp, else its evidence's
source_doc_id, read as a gold document id or url; one that names no gold document is counted.

Items: every mention in gold.json (mention id -> chain; default the baseline's dev event mentions).
READ: an aligned claim covers the mention (and, with a trace, Locate saw its document); PLACE: one covering claim
sits on the chain's record, the record its mentions share most with, one to one (ladder.assign); FOLD: that
record's declared field (cdcr/fold_values.json) names the chain's gold type. A chain whose gold type has no
declared value, a value the map does not name, or a run whose records carry no such field is could-not-judge.
Identity: mention -> record for mentions covered on exactly one record, through support/score.py's er_score.
A run that names its sections (ladder.run_documents) is scored on the mentions of their documents only, identity too.

A record is any atom a declared type's records are written as (ladder.records_of): the happening type is an
event type, so its records are Event atoms. Tested on fixtures/gvc-e2e-mini (built from gold.json and the raw
documents) and on that fixture shaped as a scaffold leg (test_ladder.scaffold_leg).
"""
import bisect, collections, json, pathlib, re, sys

import ladder as L

HERE = pathlib.Path(__file__).resolve().parent
CORPUS = pathlib.Path.home() / ".svrnmesh/bench-corpora/gvc"
GOLD = L.BASELINE / "gvc/statements/gold.json"
FOLD_VALUES = HERE / "cdcr/fold_values.json"
U = L.load_module("support_score", HERE / "support/score.py")


def jsonl(p):
    with open(p, encoding="utf-8") as f:
        return [json.loads(x) for x in f if x.strip()]


def load_gold(gold_path, corpus):
    """(mentions {id: dict}, bodies {doc: body}, keys {reader key: doc}). Refuses a mention whose offsets do not
    read back its own text: the alignment would silently score the wrong characters."""
    chain = json.loads(pathlib.Path(gold_path).read_text())
    docs = {d["id"]: d for d in jsonl(pathlib.Path(corpus) / "raw/documents.jsonl")}
    mentions = {}
    for m in jsonl(pathlib.Path(corpus) / "gold/mentions.jsonl"):
        if m["mention_id"] not in chain:
            continue
        body = docs[m["doc_id"]]["body"]
        if body[m["start"]:m["end"]] != m["text"]:
            sys.exit(f"gold mention {m['mention_id']} does not read back its text at {m['start']}..{m['end']}")
        mentions[m["mention_id"]] = {"doc": m["doc_id"], "start": m["start"], "end": m["end"],
                                     "chain": chain[m["mention_id"]], "type": m["type"]}
    missing = set(chain) - set(mentions)
    if missing:
        sys.exit(f"{len(missing)} gold.json mentions are not in gold/mentions.jsonl, e.g. {sorted(missing)[0]}")
    used = {m["doc"] for m in mentions.values()}
    keys = {}
    for d in docs.values():
        if d["id"] in used:
            keys[d["id"]] = d["id"]
            if d.get("url"):
                keys[d["url"]] = d["id"]
    return mentions, {d: docs[d]["body"] for d in used}, keys


def folded(text):
    """Whitespace runs as one space, and each folded character's index in `text`."""
    out, at, gap = [], [], False
    for i, ch in enumerate(text):
        if ch.isspace():
            gap = bool(out)
            continue
        if gap:
            out.append(" ")
            at.append(i - 1)
            gap = False
        out.append(ch)
        at.append(i)
    return "".join(out), at


def find(anchor, body):
    """(start, end) of `anchor` in `body`, or why not: "absent", "ambiguous"."""
    hits = [m.start() for m in re.finditer(re.escape(anchor), body)] if anchor else []
    if len(hits) == 1:
        return hits[0], hits[0] + len(anchor)
    if not hits:
        fa, _ = folded(anchor)
        fb, at = folded(body)
        hits = [m.start() for m in re.finditer(re.escape(fa), fb)] if fa else []
        if len(hits) == 1:
            return at[hits[0]], at[hits[0] + len(fa) - 1] + 1
    return "ambiguous" if len(hits) > 1 else "absent"


def line_spans(body):
    """[(start, end)] of each line of `body`, newline excluded."""
    spans, at = [], 0
    for raw in body.split("\n"):
        spans.append((at, at + len(raw)))
        at += len(raw) + 1
    return spans


def cited(span, lines):
    """The cited lines' extent: from the first line the span touches to the last."""
    starts = [s for s, _ in lines]
    first = bisect.bisect_right(starts, span[0]) - 1
    last = bisect.bisect_right(starts, max(span[1] - 1, span[0])) - 1
    return lines[first][0], lines[last][1]


def statements(atoms, bodies, keys):
    """[{doc, start, end, record, kind}] for every claim about a record that aligns, and the counts of those that don't."""
    records = L.records_of(atoms)
    lines = {d: line_spans(b) for d, b in bodies.items()}
    out, counts = [], collections.Counter()
    for a in atoms:
        c = a.get("data") or {}
        if a.get("atom_type") != "Claim":
            continue
        counts["claims"] += 1
        if c.get("subject") not in records:
            counts["claims_not_about_a_record"] += 1
            continue
        key = (c.get("attributes") or {}).get("document_id") or next(
            (e.get("source_doc_id") for e in c.get("evidence") or [] if e.get("source_doc_id")), None)
        doc = keys.get(str(key)) if key is not None else None
        if doc is None:
            counts["claims_document_not_in_gold"] += 1
            continue
        at = find(c.get("anchor") or "", bodies[doc])
        if isinstance(at, str):
            counts[f"claims_anchor_{at}"] += 1
            continue
        counts["claims_aligned"] += 1
        s, e = cited(at, lines[doc])
        out.append({"i": len(out), "doc": doc, "start": s, "end": e, "record": c["subject"], "kind": c.get("claim_kind"),
                    "record_type": records[c["subject"]]["record_type"]})
    return out, records, dict(counts)


def fold_value(record, spec):
    """The gold type the record's declared field names: a type, "unmapped", or None when the record has no value."""
    v = (record.get("attributes") or {}).get(spec["field"])
    vals = v if isinstance(v, list) else [v] if v is not None else []
    if not vals:
        return None
    types = {spec["values"].get(x, "unmapped") for x in vals}
    return types.pop() if len(types) == 1 else "several"


def items(mentions, stmts, records, spec, locate, keys):
    by_doc = collections.defaultdict(list)
    for s in stmts:
        by_doc[s["doc"]].append(s)
    cover = {mid: [s for s in by_doc[m["doc"]] if s["start"] <= m["start"] and m["end"] <= s["end"]]
             for mid, m in mentions.items()}
    weight = collections.Counter((mentions[mid]["chain"], r) for mid, ss in cover.items() for r in {s["record"] for s in ss})
    match = L.assign(dict(weight))
    traced = {keys[k] for k in locate if k in keys}
    judged = bool(traced)
    field_seen = any(spec["field"] in (r.get("attributes") or {}) for r in records.values())
    facts, rows = [], []
    for mid, m in sorted(mentions.items()):
        ss = cover[mid]
        seen = (m["doc"] in traced) if judged else None
        rec = match.get(m["chain"])
        on = [s for s in ss if s["record"] == rec]
        fold = None
        if on and field_seen and m["type"] not in spec["unfoldable_gold_types"]:
            v = fold_value(records[rec], spec)
            fold = None if v == "unmapped" else v == m["type"]
        f = L.Facts(read=bool(ss) and seen is not False, placed=bool(on), folded=fold)
        facts.append(f)
        rows.append({"mention": mid, "chain": m["chain"], "type": m["type"], "ladder_stage": f.name(),
                     "rung": "document_not_read" if seen is False else f.name(),
                     "records": sorted({s["record"] for s in ss}), "matched_record": rec})
    chains_of = collections.defaultdict(set)  # statement index -> the gold chains of the mentions it covers
    for mid, ss in cover.items():
        for s in ss:
            chains_of[s["i"]].add(mentions[mid]["chain"])
    shared = sum(len(c) > 1 for c in chains_of.values())
    detail = {"chains": len({m["chain"] for m in mentions.values()}), "chains_matched": len(match),
              "statements_covering_several_chains": shared, "fold_field": spec["field"],
              "fold": "judged" if field_seen else f"could_not_judge (no record carries `{spec['field']}`)",
              "document_not_read": "judged" if judged else "could_not_judge (no trace names a gold document)"}
    return facts, rows, cover, detail


def identity(mentions, cover):
    pred = {mid: next(iter(rs)) for mid, ss in cover.items() if len(rs := {s["record"] for s in ss}) == 1}
    gold = {mid: m["chain"] for mid, m in mentions.items()}
    if not pred:
        return None
    r = U.er_score(pred, gold)
    return {"b_cubed": round(r["b_cubed"]["f1"], 3), "ceaf_e": round(r["ceaf_e"]["f1"], 3),
            "lea": round(r["lea"]["f1"], 3), "scored": len(pred), "gold": len(gold),
            "coverage": round(len(pred) / len(gold), 3),
            "several_records": sum(len({s["record"] for s in ss}) > 1 for ss in cover.values())}


def measure(run, atoms=None, gold=GOLD, corpus=CORPUS, spec=FOLD_VALUES):
    run = pathlib.Path(run)
    atlas = L.run_atlas(run)
    if isinstance(atlas, str):
        return L.never_ran("gvc", atlas)
    atoms = pathlib.Path(atoms) if atoms else atlas["dir"] / "atoms.json"
    spec = json.loads(pathlib.Path(spec).read_text())
    mentions, bodies, keys = load_gold(gold, corpus)
    sliced = L.run_documents(run, atlas)
    if isinstance(sliced, str):
        return L.could_not_judge("gvc", sliced)
    total = len(mentions)
    if sliced is not None:  # the mentions on the run's sections' documents: items, alignment and identity alike
        docs = {keys[k] for k in sliced["documents"] if k in keys}
        mentions = {mid: m for mid, m in mentions.items() if m["doc"] in docs}
    stmts, records, counts = statements(json.loads(atoms.read_text())["atoms"], bodies, keys)
    locate, cost = L.run_trace(run)
    facts, rows, cover, detail = items(mentions, stmts, records, spec, locate, keys)
    return {"system": "gvc", "status": "judged", "run": str(run), "atoms": str(atoms),
            "read": L.what_was_read(atlas, cost),
            "population": L.population("gold event mentions (gold.json)", sliced),
            "scope": L.scope(sliced, total, len(mentions)),
            "ladder": L.summarize(facts, cost), "rungs": dict(collections.Counter(x["rung"] for x in rows)),
            "identity": identity(mentions, cover), "alignment": counts, "detail": detail, "cost": cost, "items": rows}
