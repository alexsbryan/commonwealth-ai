"""crm-ward's stage ladder, tune scope, from one run directory (moved from the baseline's measure_ward.py).

    RUN holds data/indexes/crm-ward/{atlas,chapters.json,chunks.lance} (the atlas read is recorded/ when a scaffold
    leg kept one: ladder.run_atlas), job.log, job.debug.log, _tokens.json (their recorded half: ladder.trace)

The bars are ward/score.py's score() (ward-score-v3), unchanged, and so is the record matching: score() matches
gold deals to deal records by evidence and party (deal_matches). The items are the tune gold deals with a current
stage. READ: a stage_update claim sits on the deal's latest message; PLACE: one of them is about the deal's
matched record; FOLD: the record serves the gold current stage (score()'s stage_served_by_deal is "served").
Sections map to files through the RUN's own index, not the live one.

`identity` is layer-identity-ward's measure: deal records scored the way support/score.py scores uv's case
records (er_score, the one partition scorer).
"""
import collections, email.utils, json, pathlib, re

import ladder as L

HERE = pathlib.Path(__file__).resolve().parent
S = L.load_module("ward_score", HERE / "ward/score.py")
U = L.load_module("support_score", HERE / "support/score.py")

TUNE = ("smurfit", "mesa", "pasadena", "bhp", "tep")  # the tune folders (crm-loop17 prereg)
KEEP = ("deals", "master_agreements", "stage_served", "stage_served_missed", "stage", "commitments", "made_up",
        "deal_atoms_unmatched")


def doc_id(message_id):
    return re.sub(r"[.@]", "-", message_id.strip("<>").lower())


def identity(g, atoms, tune_files):
    """Deal identity as support/score.py reads case identity (atlas_pred, evaluation_scope, er-score): each claim
    about a deal record puts the message its document_id stamp names into that record, and a message naming several
    records is unplaced and counted, never voted. Gold: tune files in exactly one gold deal; files in several are
    excluded; tune files in no deal are known no-deal placements."""
    of = collections.defaultdict(set)
    for d in g["deals"]:
        for f in d["files"] & tune_files:
            of[f].add(d["id"])
    gold = {f: next(iter(ds)) for f, ds in of.items() if len(ds) == 1}
    ambiguous = {f for f, ds in of.items() if len(ds) > 1}
    noise = tune_files - set(of)
    paths = collections.defaultdict(set)
    for e in json.loads((S.WARD / "manifest.json").read_text()):
        for m in e.get("message_ids") or []:
            paths[m.strip("<>").lower()].add(e["path"])
    deals = {x["data"]["id"] for x in atoms if x["atom_type"] == "Entity" and x["data"].get("entity_type") == "deal"}
    votes, counts = collections.defaultdict(set), collections.Counter()
    for x in atoms:
        c = x["data"]
        if x["atom_type"] != "Claim" or c.get("subject") not in deals:
            continue
        counts["claims"] += 1
        doc = (c.get("attributes") or {}).get("document_id")
        if not doc:
            counts["claims_unstamped"] += 1
            continue
        for f in paths.get(str(doc).strip("<>").lower(), ()):
            votes[f].add(c["subject"])
    counts["documents_in_several_records"] = sum(len(r) > 1 for r in votes.values())
    counts["records"] = len({r for rs in votes.values() for r in rs})
    pred = {f: next(iter(rs)) for f, rs in votes.items() if len(rs) == 1}
    r = U.er_score({f: c for f, c in pred.items() if f in gold or f in noise}, gold)
    pick = lambda m: {k: round(r[m][k], 3) for k in ("precision", "recall", "f1")}  # noqa: E731
    return {"atlas_read": dict(counts), "gold_deals": len(set(gold.values())), "gold_files": len(gold),
            "ambiguous_excluded": len(ambiguous), "scored": r["b_cubed"]["n_aligned"],
            "membership_coverage": round(len(set(gold) & set(pred)) / len(gold), 3) if gold else None,
            "ambiguous_predictions_excluded": len(set(pred) & ambiguous),
            "predictions_outside_fold": len(set(pred) - tune_files),
            "no_deal_files": len(noise), "no_deal_files_placed": len(set(pred) & noise),
            "recovery_b_cubed": pick("recovery_b_cubed"), "b_cubed": pick("b_cubed"), "ceaf_e": pick("ceaf_e"),
            "lea": pick("lea"), "pairwise": pick("pairwise")}


def rung(read, rec):
    """The baseline's rung order (measure_ward.py): the reading first, then the record's outcome."""
    if read != "latest_right_stage":
        return read
    if rec == "unmatched_deal":
        return "no_record_with_its_party"
    return "served" if rec == "served" else f"matched_not_serving:{rec}"


def items(g, claims, res, locate, label):
    paths = {e["path"]: [doc_id(x) for x in e.get("message_ids") or []]
             for e in json.loads((S.WARD / "manifest.json").read_text())}
    subject = {**res["deal_matches"]["transactions"], **res["deal_matches"]["master_agreements"]}
    su = [c for c in claims if c.get("claim_kind") == "stage_update"]
    ups = collections.defaultdict(list)
    for s in g["stage_updates"]:
        ups[s["deal"]].append(s)
    facts, rows, cross = [], [], collections.Counter()
    for d in g["deals"]:
        rec = res["stage_served_by_deal"].get(d["id"])
        if rec is None:  # not a current deal in scope: score() decides the population
            continue
        u = ups[d["id"]]
        lw = max(S.when(s["file"]) for s in u)
        latest = [s for s in u if S.when(s["file"]) == lw]
        gs, lf = latest[0]["stage"], {s["file"] for s in latest}
        on_deal = [c for c in su if c["_files"] & d["files"]]
        on_latest = [c for c in su if c["_files"] & lf]
        if not on_deal:
            read = "no_claim_on_any_message"
        elif not on_latest:
            read = "latest_not_located"
        elif not any((c.get("attributes") or {}).get("stage") == gs for c in on_latest):
            read = "latest_wrong_stage"
        else:
            read = "latest_right_stage"
        f = L.Facts(read=bool(on_latest),
                    placed=subject.get(d["id"]) is not None and any(c.get("subject") == subject[d["id"]] for c in on_latest),
                    folded=rec == "served")
        facts.append(f)
        cross[f"{read} x {rec}"] += 1
        close = None
        if read in ("no_claim_on_any_message", "latest_not_located") and label:
            close = L.near(locate, [x for fl in lf for x in paths.get(fl, [])], label)
        rows.append({"deal": d["id"], "stage": gs, "latest": sorted(lf), "rung": rung(read, rec), "record": rec,
                     "ladder_stage": f.name(), "near": close})
    return facts, rows, dict(cross)


def measure(run):
    run = pathlib.Path(run)
    found = L.run_atlas(run)
    if isinstance(found, str):
        return L.never_ran("ward", found)
    if found["index"] is None:
        return L.never_ran("ward", f"{run} holds no single data/indexes/*/atlas: its sections cannot be mapped to files")
    index, atlas = found["index"], found["dir"]
    secfiles = S.section_files(str(index))  # an absolute path: section_files joins it under ~/.svrnmesh/indexes
    for e in json.loads((S.WARD / "manifest.json").read_text()):
        try:
            S.DATES.setdefault(e["path"], email.utils.parsedate_to_datetime(e["date"]).isoformat())
        except (TypeError, ValueError):
            pass
    g = S.load_gold(S.WARD / "gold")
    tune_files = {f for f in g["files"] if f.split("/", 1)[0] in TUNE}
    atoms = json.loads((atlas / "atoms.json").read_text())["atoms"]
    ent, claims = S.load_atlas(atoms, secfiles, S.load_decisions(atlas / "derived_decisions.jsonl"))
    res = S.score(g, ent, claims, tune_files)
    locate, cost = L.run_trace(run)
    label = L.kind_label(locate, "stage_update")
    facts, rows, cross = items(g, claims, res, locate, label)
    idn = identity(g, atoms, tune_files)
    return {"system": "ward", "status": "judged", "run": str(run), "instrument_version": S.INSTRUMENT_VERSION,
            "read": L.what_was_read(found, cost),
            "population": "tune gold deals with a current stage",
            "ladder": L.summarize(facts, cost), "rungs": dict(collections.Counter(x["rung"] for x in rows)),
            "identity": {"b_cubed": idn["b_cubed"]["f1"], "ceaf_e": idn["ceaf_e"]["f1"], "lea": idn["lea"]["f1"],
                         "scored": idn["scored"], "coverage": idn["membership_coverage"]},
            "bars": {k: res[k] for k in KEEP}, "deal_identity": idn, "reading_x_record": cross,
            "claims": dict(collections.Counter(c.get("claim_kind") for c in claims)),
            "deal_records": sum(1 for e in ent.values() if e.get("entity_type") == "deal"),
            "tune_gold_files": len(tune_files),
            "locate_trace": {"documents": len(locate), "lines": sum(len(v) for v in locate.values()),
                             "stage_update_label": label},
            "cost": cost, "items": rows}
