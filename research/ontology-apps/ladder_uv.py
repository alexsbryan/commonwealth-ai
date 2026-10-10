"""uv-support's stage ladder, tune fold, from one run directory (moved from the baseline's measure_uv.py).

    RUN holds recorded/atoms.json (a scaffold leg) or its one data/indexes/*/atlas/atoms.json (ladder.run_atlas),
    and job.log, job.debug.log, _tokens.json (their recorded half: ladder.trace)

Identity is support/score.py --fold tune --atlas, through its own functions (`svrn bench er-score` underneath). The
items are the tune gold case states; a run that names its sections (ladder.run_documents) is scored on the states
whose documents lie in them, and identity on that slice's gold. A gold case's record is the one its member documents share most with, one to one
(ladder.assign). READ: a case_state claim exists on the state's document (and, with a trace, Locate saw the
document); PLACE: one sits on the case's record; FOLD: one on that record carries the gold state.
The baseline's rungs check the state before the record, so they are kept beside the stages from the same facts.
"""
import collections, json, pathlib, sys

import ladder as L

HERE = pathlib.Path(__file__).resolve().parent
U = L.load_module("support_score", HERE / "support/score.py")


def rung(f, state_on_document, traced):
    """The baseline's rung order (measure_uv.py): not read, no claim, wrong state, record not matched, hit."""
    if traced is False:
        return "document_not_read"
    if not f.read:
        return "no_case_state_claim"
    if not state_on_document:
        return "wrong_state"
    return "hit" if f.folded else "record_not_matched"


def identity(atoms, docs):
    """support/score.py --fold tune --atlas, through its own deciders (load_gold, atlas_pred, evaluation_scope,
    er_score), with gold, its no-case documents and the predictions restricted to `docs` when the run names its
    sections: the slice's records are scored against the slice's gold, never the fold's."""
    gold, ambiguous, none, n_cases = U.load_gold("tune")
    pred, read = U.atlas_pred(atoms, "case_state")
    pred = {str(k): str(v) for k, v in pred.items()}
    outside, full = 0, len(gold)
    if docs is not None:
        gold = {d: c for d, c in gold.items() if d in docs}
        ambiguous, none = ambiguous & docs, none & docs
        outside = len(set(pred) - docs)
        pred = {d: c for d, c in pred.items() if d in docs}
    raw = [json.loads(x) for x in (U.ROOT / "raw/documents.jsonl").read_text().splitlines() if x.strip()]
    try:
        pred, scoped = U.evaluation_scope(pred, gold, ambiguous, none, raw, "tune")
    except ValueError as e:
        sys.exit(f"support/score.py's evaluation_scope could not judge: {e}")
    r = U.er_score(pred, gold)
    pick = lambda m: {k: round(r[m][k], 3) for k in ("precision", "recall", "f1")}  # noqa: E731
    return {"placements": {d: [gold[d], pred.get(d)] for d in sorted(gold)},  # unit -> [gold, its one record or None]
            "fold": "tune", "atlas_read": read, "gold_cases": n_cases, "gold_documents": len(gold),
            "ambiguous_excluded": len(ambiguous), "scored": r["b_cubed"]["n_aligned"],
            "gold_unpredicted": len(set(gold) - set(pred)),
            "membership_coverage": round(len(set(gold) & set(pred)) / len(gold), 3) if gold else None,
            **scoped, "predictions_outside_sections": outside, "gold_left_out": full - len(gold),
            "recovery_b_cubed": pick("recovery_b_cubed"), "b_cubed": pick("b_cubed"), "ceaf_e": pick("ceaf_e"),
            "lea": pick("lea"), "pairwise": pick("pairwise")}


def served_state(atoms, match, states):
    """Served-state accuracy, named apart from the ladder's FOLD (a state CLAIM on the matched record): per gold case
    with a state in scope, whether its matched record's own `state` attribute is the case's latest gold state.
    Could-not-judge when no case record carries a state attribute (the recipe declares no case-state fold)."""
    recs = {k: v.get("attributes") or {} for k, v in L.records_of(atoms).items() if v["record_type"] == "case"}
    if not any("state" in attrs for attrs in recs.values()):
        return {"status": "could_not_judge",
                "reason": f"none of the {len(recs)} case records carries a state attribute"}
    latest = {}
    for s in sorted(states, key=lambda s: s["date"]):
        latest[s["case"]] = s["state"]
    right = sum(recs.get(match.get(c), {}).get("state") == st for c, st in latest.items())
    return {"status": "judged", "cases": len(latest), "served_right": right}


def items(atoms, locate, docs):
    g = json.loads((U.ROOT / "gold/cases.json").read_text())
    tune = {c["id"] for c in g["cases"] if c["fold"] == "tune"}
    gold_doc, _, _, _ = U.load_gold("tune")
    pred, _ = U.atlas_pred(atoms, "case_state")
    match = L.assign(dict(collections.Counter((gold_doc[d], r) for d, r in pred.items() if d in gold_doc)))
    claims = collections.defaultdict(list)
    for a in atoms:
        d = a.get("data") or {}
        if a.get("atom_type") == "Claim" and d.get("claim_kind") == "case_state":
            claims[str((d.get("attributes") or {}).get("document_id"))].append(d)
    states = [s for s in g["case_states"] if s["case"] in tune]
    total = len(states)
    if docs is not None:  # the record matching above still reads every document the run placed
        states = [s for s in states if str(s["document"]) in docs]
    traced = {str(x) for x in locate}
    judged = bool(traced & {str(s["document"]) for s in states})
    label = L.kind_label(locate, "case_state")
    facts, rows = [], []
    for s in states:
        doc, on = str(s["document"]), claims.get(str(s["document"]), [])
        state = lambda c: (c.get("attributes") or {}).get("state")  # noqa: E731
        seen = (doc in traced) if judged else None
        mine = [c for c in on if c.get("subject") == match.get(s["case"])]
        f = L.Facts(read=bool(on) and seen is not False, placed=bool(mine), folded=any(state(c) == s["state"] for c in mine))
        r = rung(f, any(state(c) == s["state"] for c in on), seen)
        facts.append(f)
        rows.append({"case": s["case"], "state": s["state"], "document": doc, "rung": r, "ladder_stage": f.name(),
                     "claimed": sorted({state(c) or "?" for c in on}),
                     "near": L.near(locate, [doc], label) if r == "no_case_state_claim" and label else None})
    return facts, rows, {"cases_matched": len(match), "tune_cases": len(tune), "case_state_label": label,
                         "served_state": served_state(atoms, match, states),
                         "document_not_read": "judged" if judged else "could_not_judge (no trace names a gold document)",
                         "tune_case_states": total}


def measure(run, atoms=None, sections_of=None):
    """`sections_of`: read the population from another run's sections.ids (a like-for-like pair: a whole-fold run
    scored on a slice run's documents); default the run's own."""
    run = pathlib.Path(run)
    atlas = L.run_atlas(run)
    if isinstance(atlas, str):
        return L.never_ran("uv", atlas)
    atoms = pathlib.Path(atoms) if atoms else atlas["dir"] / "atoms.json"
    if not atoms.exists():
        return L.never_ran("uv", f"{atoms} does not exist")
    view = L.atlas_view(run, atlas, atoms)
    if isinstance(view, str):
        return L.could_not_judge("uv", view)
    src = pathlib.Path(sections_of) if sections_of else run
    src_atlas = atlas if src == run else L.run_atlas(src)
    sliced = src_atlas if isinstance(src_atlas, str) else L.run_documents(src, src_atlas)
    if isinstance(sliced, str):
        return L.could_not_judge("uv", sliced)
    # The reader logs a document by its url, gold names it by id; raw/documents.jsonl holds both, one to one.
    url_id = {d["url"]: str(d["id"]) for d in map(json.loads, filter(str.strip, (U.ROOT / "raw/documents.jsonl").read_text().splitlines()))}
    docs = None
    if sliced is not None:
        docs = {url_id[u] for u in sliced["documents"] if u in url_id}
        unnamed = len(sliced["documents"]) - len(docs)
        if unnamed:
            return L.could_not_judge("uv", f"{unnamed} of the sections' documents name no raw/documents.jsonl url")
    comp = identity(view["atoms"], docs)
    placements = comp.pop("placements")
    locate, cost = L.run_trace(run)
    locate = {url_id.get(k, k): v for k, v in locate.items()}
    facts, rows, detail = items(view["atoms"], locate, docs)
    return {"system": "uv", "status": "judged", "run": str(run), "atoms": str(atoms),
            "read": L.what_was_read(atlas, cost), "vocabulary": view["vocabulary"],
            "population": L.population("tune gold case states", sliced)
                          + (f" of {src}" if sliced is not None and src != run else ""),
            "scope": L.scope(sliced, detail["tune_case_states"], len(rows)),
            "ladder": L.summarize(facts, cost), "rungs": dict(collections.Counter(x["rung"] for x in rows)),
            "fold_checks": "a state claim on the matched case record states the gold state (state-claim accuracy)",
            "served_state": detail["served_state"],
            "identity": {"b_cubed": comp["b_cubed"]["f1"], "ceaf_e": comp["ceaf_e"]["f1"], "lea": comp["lea"]["f1"],
                         "scored": comp["scored"], "gold": comp["gold_documents"], "coverage": comp["membership_coverage"],
                         "recovery_b_cubed": comp["recovery_b_cubed"]["f1"], "gold_left_out": comp["gold_left_out"]},
            "composition": comp, "placements": {"unit": "tune gold document", "items": placements},
            "cost": cost, "detail": detail, "items": rows}
