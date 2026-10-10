#!/usr/bin/env python3
"""E4's first read: why does a blind recipe's reading find uv case states ours misses?

    reader_residual.py --join-check     # step 1: the two runs' traces joined per document; no gold row is read
    reader_residual.py                  # step 2: every gold state classed; writes reader-residual/{rows.jsonl,summary.json}
    reader_residual.py --other RUN --out DIR   # the same classing with RUN in the `blind` slot (E4: our reader after a change)

Order ontology-layer-8-reader-residual. Both runs are read through ladder_uv.measure on the same 132 gold states (ours
on its own sections.ids, the blind run restricted to ours with `sections_of`), so the per-state rungs are the ladder's
own and must sum to its totals (checked, exit 1 otherwise). Beside each rung, from each run's recorded half of
job.debug.log (ladder.recorded_half): the Locate lines on the state's document (kind, p, distribution, text), the
Choose answers, and the model calls made on that document by phase; from the run's atlas, the claims on the document
under their own kind (the blind run's raw kind kept beside its mapped one, ladder.translate).

The reader both runs used (one binary, beded4650) asks Locate once per line with one label per declared claim kind and
shows the kind's name and description only: no attribute values, no guidance, no document metadata
(ingest/crates/corpus-engine/src/enrichment/pipeline/document_read/passes.rs: locate_question). Choose asks once per
closed-valued field per statement with the type, the attribute and its description, and the value names
(resolve_records/read.rs: choice_question). So declaring more kinds adds no Locate call; more fields add Choose calls.

CAUSES is the closed list, fixed and committed before any row was read (step 1). Each state gets exactly one cause, by
the first rule in CAUSE_RULES that holds at the stage where the two runs first diverge; a state both runs lose at the
same rung gets the cause of the shared loss. Nothing is defaulted: a state no rule decides reads could_not_attribute.
"""
import argparse, collections, json, pathlib, re, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402
import ladder_uv  # noqa: E402

REPO = HERE.parents[1]
OURS = REPO / "runs/c2-lines/uv-third"
BLIND = REPO / "runs/blind-r2/uv-third"
OUT = HERE / "reader-residual"

# ---------------------------------------------------------------- the closed cause list (step 1, before any row)
# The order's six, then the two outside the reader. Each names the domain-free reader change it would point at; the
# change is a hypothesis named here and read against the counts in step 2, never a recipe edit.
CAUSES = {
    "kind_factoring": "the state follows from WHICH kind Locate put the line under: the winner's right state is set by "
                      "a specific kind (report, request, commitment: the map's `set`), or the loser's line went to a "
                      "kind that carries a fixed wrong state or no state (diagnosis, an unmapped kind)",
    "description_wording": "the same question shape (one kind whose value is chosen) answered differently: the loser "
                           "located no line on the document while the winner located it under a value-bearing kind "
                           "(our case_state, the blind decision), or both read a claim and the loser chose a wrong "
                           "value that maps one to one; for a state both lose, an issue or comment line no kind took",
    "value_set": "both read a claim on the document and the right value was reachable for one only: the winner's raw "
                 "value is one of several mapping to the gold state (fixed/released, not-planned/not-a-bug), or the "
                 "loser's value maps to no state (needs-reproduction, closed-in-error) or was 'none of them'",
    "document_kind": "a state both runs lose on an event document (tracker-generated text) that no kind took. The "
                     "reader shows both recipes the same text and no metadata, so document kind cannot by itself "
                     "separate the two recipes: in a difference it is a stratum (the row's doc_kind), never a cause",
    "body_floor": "the loser's Locate never saw the document because its chapter was skipped under "
                  "min_section_body_words (questions.json failures)",
    "chaptering": "the loser's Locate never saw the document though its chapter was not floor-skipped (the document "
                  "lies outside the chapters it read), or both saw it with different lines (chunking composed the "
                  "document differently)",
    "placement": "not the reader: both read the gold state on the document and the loser's claim sits on a record "
                 "not matched to the gold case (RESOLVE)",
    "could_not_attribute": "no rule above decides; reported, never defaulted (principle 6)",
}
# The domain-free reader change each cause points at. Written AFTER the rows were read (step 2), unlike CAUSES; each
# is a function of the contract, never of one recipe, and none is measured here.
READER_CHANGES = {
    "kind_factoring": "Locate factors a claim kind whose closed-valued field carries the state into one label per "
                      "declared value, each with the value's declared description: the state is answered where the "
                      "line is found, as the blind recipe's report/request/commitment kinds answer it, at the same "
                      "one Locate call per line",
    "description_wording": "Locate shows each kind's closed-valued fields with their declared values and descriptions "
                           "(today it shows the kind's name and description only, passes.rs locate_question): our "
                           "recipe already says 'a pull request is open' and 'fixed ... released', in the state "
                           "attribute's description, where Locate never looks; the weaker form of kind_factoring's change",
    "value_set": "Choose decides only above its measured precision (ONTOLOGY_METHOD §Reading, not built): below the "
                 "bar the field stays unknown with its distribution; these rows show a misread, not a missing value",
    "document_kind": "the document's declared facts (its kind and structured fields) prefilled into the Locate "
                     "prefix, and a value a declared structured field carries read by code (E4's own text)",
    "body_floor": "no chapter floor ahead of the passes reader: min_section_body_words guards chapter analysis, but "
                  "Locate asks per line and a one-line event is a whole document",
    "chaptering": "none in the reader: the blind recipe's chunk overlap and title handling composed these documents' "
                  "lines differently; both runs' readers saw a different text",
    "placement": "none in the reader: RESOLVE",
    "could_not_attribute": "none",
}
CAUSE_RULES = list(CAUSES)  # precedence is applied in classify(), stage by stage; this is the closed set
RANK = {"document_not_read": 0, "no_case_state_claim": 1, "wrong_state": 2, "record_not_matched": 3, "hit": 4}
# blind raw values that are one of several mapping to one gold state (the frozen map, r2-mapping.json uv.decision)
MANY_TO_ONE = {"fixed", "released", "not-planned", "not-a-bug"}

# ---------------------------------------------------------------- the runs' own logs
LOCATED_TEXT = re.compile(r'located document="?([^" ]+)"? line=(\d+) kind="([^"]+)" p=(\S+) dist=(.*?) text=(.*)$')
DECISION_CALL = re.compile(r'decision call document="([^"]+)" statement="([^"]*)" phase=Some\("([^"]+)"\)')
CHOSE = re.compile(r'passes: chose (none of the values )?document="([^"]+)" statement="([^"]*)" field=(\S+) '
                   r'(?:value=(\S+) )?p=(\S+)')
FLOOR = "min_section_body_words"


def doc_of_url(url, url_id):
    """A trace names a document by its url (a comment's carries its #anchor); gold by id. One map, raw/documents."""
    return url_id.get(url)


def read_trace(run, url_id):
    """{doc id: {"lines": [...], "calls": Counter(phase), "chose": [...]}} from the recorded half of job.debug.log."""
    lines, stop = L.recorded_half(run / "job.debug.log")
    out = collections.defaultdict(lambda: {"lines": [], "calls": collections.Counter(), "chose": []})
    unnamed = collections.Counter()
    for raw in lines:
        m = LOCATED_TEXT.search(raw)
        if m:
            d = doc_of_url(m.group(1), url_id)
            if d is None:
                unnamed["located"] += 1
                continue
            dist = {k: float(v) for k, v in (x.rsplit(" ", 1) for x in m.group(5).split(", "))}
            out[d]["lines"].append({"line": int(m.group(2)), "kind": m.group(3), "p": float(m.group(4)),
                                    "dist": dist, "text": m.group(6).strip()})
            continue
        m = DECISION_CALL.search(raw)
        if m:
            d = doc_of_url(m.group(1), url_id)
            if d is None:
                unnamed["call"] += 1
                continue
            out[d]["calls"][m.group(3)] += 1
            continue
        m = CHOSE.search(raw)
        if m:
            d = doc_of_url(m.group(2), url_id)
            if d is None:
                unnamed["chose"] += 1
                continue
            out[d]["chose"].append({"statement": m.group(3), "field": m.group(4),
                                    "value": None if m.group(1) else m.group(5), "p": float(m.group(6))})
    return dict(out), {"stopped_at": stop, "unnamed": dict(unnamed)}


def letters(trace):
    """{Locate letter: kind name}, read off the lines a kind won (argmax letter of a located line)."""
    seen = collections.defaultdict(collections.Counter)
    for v in trace.values():
        for ln in v["lines"]:
            if ln["kind"] != "none":  # the none label ("0") never names a kind, even on a rounded tie
                seen[max((k for k in ln["dist"] if k != "0"), key=ln["dist"].get)][ln["kind"]] += 1
    return {letter: c.most_common(1)[0][0] for letter, c in seen.items()}


def chapters_of(run):
    """({doc id: chapter id}, {floor-skipped chapter ids}) through the run's own index (ladder.run_documents' path)."""
    atlas = L.run_atlas(run)
    if isinstance(atlas, str) or atlas["index"] is None:
        sys.exit(f"{run}: {atlas}")
    import lance  # noqa: PLC0415
    rows = lance.dataset(str(atlas["index"] / "chunks.lance")).to_table(columns=["id", "source_doc_id"]).to_pylist()
    doc_of = {str(r["id"]): r["source_doc_id"] for r in rows}
    chap = {}
    for c in json.loads((atlas["index"] / "chapters.json").read_text())["chapters"]:
        for ch in c["chunk_ids"]:
            if doc_of.get(str(ch)):
                chap.setdefault(doc_of[str(ch)], c["id"])
    q = json.loads((run / "questions.json").read_text())
    floor = {f["chapter_id"] for f in q.get("failures") or [] if FLOOR in (f.get("reason") or "")}
    sections = {s.strip() for s in (run / L.SECTIONS).read_text().replace("\n", ",").split(",") if s.strip()}
    return chap, floor, sections


def claims_of(run):
    """{doc id: [{"raw_kind", "kind", "state", "raw": {attrs}}]} from the run's atlas, raw and in our names."""
    atlas = L.run_atlas(run)
    atoms = json.loads((atlas["dir"] / "atoms.json").read_text())["atoms"]
    vocab = L.run_vocabulary(run)
    out = collections.defaultdict(list)
    for a in atoms:
        if a.get("atom_type") != "Claim":
            continue
        raw = a["data"]
        (t,), _, _ = L.translate([a], [], vocab)
        d = t["data"]
        attrs = {k: v for k, v in (raw.get("attributes") or {}).items() if not k.startswith("__")}
        out[str(attrs.get("document_id"))].append({"raw_kind": raw.get("claim_kind"), "kind": d.get("claim_kind"),
                                                    "state": (d.get("attributes") or {}).get("state"),
                                                    "raw": {k: v for k, v in attrs.items() if k != "document_id"}})
    return out, vocab


# ---------------------------------------------------------------- the join (step 1)
def load():
    url_id = {d["url"]: str(d["id"]) for d in map(json.loads, filter(str.strip, (
        ladder_uv.U.ROOT / "raw/documents.jsonl").read_text().splitlines()))}
    raw = {str(d["id"]): d for d in map(json.loads, filter(str.strip, (
        ladder_uv.U.ROOT / "raw/documents.jsonl").read_text().splitlines()))}
    runs = {}
    for name, run in (("ours", OURS), ("blind", BLIND)):
        trace, meta = read_trace(run, url_id)
        chap, floor, sections = chapters_of(run)
        claims, vocab = claims_of(run)
        runs[name] = {"run": run, "trace": trace, "meta": meta, "letters": letters(trace),
                      "chapter": {url_id.get(u, u): c for u, c in chap.items()}, "floor": floor,
                      "sections": sections, "claims": claims, "vocab": vocab}
    slice_docs = L.run_documents(OURS, L.run_atlas(OURS))
    docs = {url_id[u] for u in slice_docs["documents"] if u in url_id}
    return runs, docs, raw, url_id


def join_check(runs, docs):
    """Per document over the slice: traced by each run, by both, and line sets equal where both traced."""
    t = {k: set(r["trace"]) & docs for k, r in runs.items()}
    both = t["ours"] & t["blind"]
    same_lines = sum(1 for d in both if [x["text"] for x in runs["ours"]["trace"][d]["lines"]]
                     == [x["text"] for x in runs["blind"]["trace"][d]["lines"]])
    return {"slice_documents": len(docs), "traced_ours": len(t["ours"]), "traced_blind": len(t["blind"]),
            "traced_both": len(both), "traced_neither": len(docs - t["ours"] - t["blind"]),
            "both_same_lines": same_lines,
            "in_chapters_ours": len(docs & set(runs["ours"]["chapter"])),
            "in_chapters_blind": len(docs & set(runs["blind"]["chapter"])),
            "locate_letters": {k: r["letters"] for k, r in runs.items()},
            "log": {k: r["meta"] for k, r in runs.items()}}


# ---------------------------------------------------------------- the classing (step 2)
def facts(r, doc, gold_state, row):
    tr = r["trace"].get(doc)
    vocab = r["vocab"]
    state_kinds = {"case_state"} if vocab is None else {k for k, v in vocab["kinds"].items() if v["as"] == "case_state"}
    state_letters = {l for l, k in r["letters"].items() if k in state_kinds}
    lines = tr["lines"] if tr else []
    best = max((sum(ln["dist"].get(l, 0.0) for l in state_letters) for ln in lines), default=None)
    claims = r["claims"].get(doc, [])
    ch = r["chapter"].get(doc)
    return {"rung": row["rung"], "traced": tr is not None, "chapter": ch,
            "chapter_in_sections": ch in r["sections"] if ch else None, "chapter_floor_skipped": ch in r["floor"],
            "lines": len(lines),
            "located": [{"line": ln["line"], "kind": ln["kind"], "p": round(ln["p"], 3)} for ln in lines
                        if ln["kind"] != "none"],
            "best_state_p": round(best, 3) if best is not None else None,
            "claims": [{"kind": c["raw_kind"], "as": c["kind"], "state": c["state"],
                        "value": c["raw"].get("outcome") or c["raw"].get("state")} for c in claims],
            "chose": tr["chose"] if tr else [],
            "calls": dict(tr["calls"]) if tr else {},
            "right_claim": any(c["state"] == gold_state and c["kind"] == "case_state" for c in claims)}


def is_set_kind(run, kind):
    v = run["vocab"]
    return v is not None and "set" in (v["kinds"].get(kind) or {})


def classify(o, b, runs, doc_kind, gold_state):
    """(pair, cause, the rule that fired) — the first rule that holds at the first divergence."""
    ro, rb = RANK[o["rung"]], RANK[b["rung"]]
    if ro == rb:
        pair = "both_hit" if ro == 4 else f"both_{o['rung']}"
        if ro == 4:
            return pair, None, "no loss"
        if ro == 0:
            return pair, ("body_floor" if o["chapter_floor_skipped"] and b["chapter_floor_skipped"] else "chaptering"), "shared: not read"
        if ro == 1:
            return pair, ("document_kind" if doc_kind == "event" else "description_wording"), "shared: no kind took a line"
        if ro == 2:
            return pair, "value_set", "shared: both read a wrong state"
        return pair, "placement", "shared: both placed off the gold case"
    win, lose = ("ours", "blind") if ro > rb else ("blind", "ours")
    W, Lf = (o, b) if win == "ours" else (b, o)
    pair = f"{win}_better" + ("_hit" if RANK[W["rung"]] == 4 else "")
    if RANK[Lf["rung"]] == 0:
        return pair, ("body_floor" if Lf["chapter_floor_skipped"] else "chaptering"), "loser never saw the document"
    if Lf["lines"] != W["lines"]:
        return pair, "chaptering", "both saw the document, different lines"
    Wr, Lr = runs[win], runs[lose]
    if RANK[Lf["rung"]] == 1:  # the loser has no case_state claim on the document
        if Lf["located"]:
            if any(not is_set_kind(Lr, x["kind"]) and (Lr["vocab"] or {}).get("kinds", {}).get(x["kind"]) is None
                   and x["kind"] != "case_state" for x in Lf["located"]):
                return pair, "kind_factoring", "loser located the line under a kind that carries no state"
            return pair, "could_not_attribute", "loser located a state kind but holds no claim"
        right = [c for c in W["claims"] if c["state"] == gold_state and c["as"] == "case_state"] or \
                [c for c in W["claims"] if c["as"] == "case_state"]
        if any(is_set_kind(Wr, c["kind"]) for c in right[:1]):
            return pair, "kind_factoring", "winner's state set by its specific kind"
        return pair, "description_wording", "loser located nothing; winner located a value-bearing kind"
    if RANK[Lf["rung"]] == 2:  # the loser read a wrong state, the winner the right one
        wright = [c for c in W["claims"] if c["state"] == gold_state and c["as"] == "case_state"]
        lclaims = [c for c in Lf["claims"] if c["as"] == "case_state"]
        if wright and is_set_kind(Wr, wright[0]["kind"]):
            return pair, "kind_factoring", "winner's right state set by its specific kind"
        if lclaims and all(is_set_kind(Lr, c["kind"]) for c in lclaims):
            return pair, "kind_factoring", "loser's wrong state set by its specific kind"
        if any(c["state"] is None or str(c["state"]).startswith(L.UNMAPPED) for c in lclaims):
            return pair, "value_set", "loser's value maps to no state, or none of them"
        if wright and str(wright[0]["value"]) in MANY_TO_ONE:
            return pair, "value_set", "winner's value one of several mapping to the gold state"
        return pair, "description_wording", "loser chose a wrong value that maps one to one"
    if RANK[Lf["rung"]] == 3:
        return pair, "placement", "both read the gold state; loser's record not matched"
    return pair, "could_not_attribute", "no rule"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--join-check", action="store_true")
    # E4 (order 9): the same classing of our reader after a change, read in the `blind` slot against the old run
    ap.add_argument("--other", type=pathlib.Path, help="the run read in the `blind` slot (default runs/blind-r2/uv-third)")
    ap.add_argument("--out", type=pathlib.Path, help="where rows.jsonl and summary.json go (default reader-residual/)")
    a = ap.parse_args()
    global BLIND, OUT
    BLIND, OUT = a.other or BLIND, a.out or OUT
    runs, docs, raw, _ = load()
    if a.join_check:
        print(json.dumps({"causes": CAUSES, "join": join_check(runs, docs)}, indent=1))
        return
    ours = ladder_uv.measure(OURS)
    blind = ladder_uv.measure(BLIND, sections_of=OURS)
    key = lambda x: (x["case"], x["state"], x["document"])  # noqa: E731
    if [key(x) for x in ours["items"]] != [key(x) for x in blind["items"]]:
        sys.exit("the two ladders do not read the same gold states in the same order")
    rows = []
    for xo, xb in zip(ours["items"], blind["items"]):
        doc = xo["document"]
        d = raw.get(doc, {})
        kind = d.get("kind")
        fo, fb = facts(runs["ours"], doc, xo["state"], xo), facts(runs["blind"], doc, xb["state"], xb)
        pair, cause, rule = classify(fo, fb, runs, kind, xo["state"])
        rows.append({"case": xo["case"], "state": xo["state"], "document": doc, "doc_kind": kind,
                     "event_type": d.get("event_type"), "words": len((d.get("body") or "").split()),
                     "pair": pair, "cause": cause, "rule": rule, "ours": fo, "blind": fb})
    # principle 7: the classes sum to each run's ladder totals before any cause is read
    for name, lad in (("ours", ours), ("blind", blind)):
        got = dict(collections.Counter(r[name]["rung"] for r in rows))
        if got != lad["rungs"]:
            sys.exit(f"{name}: classed rungs {got} != ladder {lad['rungs']}")
    OUT.mkdir(exist_ok=True)
    with open(OUT / "rows.jsonl", "w") as f:
        for r in rows:
            f.write(json.dumps(r, sort_keys=True) + "\n")
    summary = summarize(rows, ours, blind, runs, docs)
    (OUT / "summary.json").write_text(json.dumps(summary, indent=1, sort_keys=True) + "\n")
    print(json.dumps(summary, indent=1, sort_keys=True))


def summarize(rows, ours, blind, runs, docs):
    def cost(sel, run):
        seen, c = set(), collections.Counter()
        for r in sel:
            if r["document"] in seen:
                continue
            seen.add(r["document"])
            for k, v in r[run]["calls"].items():
                c[k] += v
        return {"documents": len(seen), "locate": c["document_passes_locate"], "choose": c["document_passes_choose"]}
    by = collections.defaultdict(list)
    for r in rows:
        by[(r["pair"], r["cause"])].append(r)
    table = []
    for (pair, cause), sel in sorted(by.items(), key=lambda kv: (kv[0][0], str(kv[0][1]))):
        table.append({"pair": pair, "cause": cause, "n": len(sel),
                      "doc_kind": dict(collections.Counter(r["doc_kind"] for r in sel)),
                      "cost_ours": cost(sel, "ours"), "cost_blind": cost(sel, "blind")})
    gains = lambda w: [r for r in rows if r["pair"] == f"{w}_better_hit"]  # noqa: E731
    return {"ladders": {"ours": {"hit": ours["ladder"]["hit"], "rungs": ours["rungs"], "n": ours["ladder"]["n"]},
                        "blind_on_our_slice": {"hit": blind["ladder"]["hit"], "rungs": blind["rungs"],
                                               "n": blind["ladder"]["n"]}},
            "pairs": dict(collections.Counter(r["pair"] for r in rows)),
            "blind_extra_hits_by_cause": dict(collections.Counter(r["cause"] for r in gains("blind"))),
            "ours_extra_hits_by_cause": dict(collections.Counter(r["cause"] for r in gains("ours"))),
            "all_by_cause": dict(collections.Counter(str(r["cause"]) for r in rows)),
            "winner_rung_by_cause": {f"{c}": dict(collections.Counter(r[r["pair"].split("_")[0]]["rung"] for r in rows
                                                                    if r["cause"] == c and "_better" in r["pair"]))
                                     for c in CAUSES},
            "choose_calls_by_field": {k: dict(collections.Counter(ch["field"] for d in docs
                                                                  for ch in runs[k]["trace"].get(d, {}).get("chose", [])))
                                      for k in runs},
            "ours_no_claim_by_doc_kind": dict(collections.Counter(
                f"{r['doc_kind']}:{r['event_type']}" if r["event_type"] else r["doc_kind"]
                for r in rows if r["ours"]["rung"] == "no_case_state_claim")),
            "ours_no_claim_best_p": dict(collections.Counter(
                "<.2" if r["ours"]["best_state_p"] < .2 else ".2-.5" for r in rows
                if r["ours"]["rung"] == "no_case_state_claim")),
            "reader_changes": READER_CHANGES,
            "table": table, "slice_cost": {"ours": cost([{"document": d, "ours": {"calls": dict(runs["ours"]["trace"].get(d, {}).get("calls", {}))}} for d in docs], "ours"),
                                           "blind": cost([{"document": d, "blind": {"calls": dict(runs["blind"]["trace"].get(d, {}).get("calls", {}))}} for d in docs], "blind")},
            "causes": CAUSES}


if __name__ == "__main__":
    main()
