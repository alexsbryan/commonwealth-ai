"""uv-support's stage ladder, tune fold, from one run directory (moved from the baseline's measure_uv.py).

    RUN holds recorded/atoms.json (a scaffold leg) or its one data/indexes/*/atlas/atoms.json (ladder.run_atlas),
    and job.log, job.debug.log, _tokens.json (their recorded half: ladder.trace)

Identity is support/score.py --fold tune --atlas, unchanged (`svrn bench er-score` underneath). The items are the
tune gold case states. A gold case's record is the one its member documents share most with, one to one
(ladder.assign). READ: a case_state claim exists on the state's document (and, with a trace, Locate saw the
document); PLACE: one sits on the case's record; FOLD: one on that record carries the gold state.
The baseline's rungs check the state before the record, so they are kept beside the stages from the same facts.
"""
import collections, json, pathlib, subprocess, sys

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


def items(atoms_path, locate):
    g = json.loads((U.ROOT / "gold/cases.json").read_text())
    tune = {c["id"] for c in g["cases"] if c["fold"] == "tune"}
    gold_doc, _, _, _ = U.load_gold("tune")
    pred, _ = U.atlas_pred(str(atoms_path), "case_state")
    match = L.assign(dict(collections.Counter((gold_doc[d], r) for d, r in pred.items() if d in gold_doc)))
    claims = collections.defaultdict(list)
    for a in json.loads(pathlib.Path(atoms_path).read_text())["atoms"]:
        d = a.get("data") or {}
        if a.get("atom_type") == "Claim" and d.get("claim_kind") == "case_state":
            claims[str((d.get("attributes") or {}).get("document_id"))].append(d)
    states = [s for s in g["case_states"] if s["case"] in tune]
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
                         "document_not_read": "judged" if judged else "could_not_judge (no trace names a gold document)"}


def measure(run, atoms=None):
    run = pathlib.Path(run)
    atlas = L.run_atlas(run)
    if isinstance(atlas, str):
        return L.never_ran("uv", atlas)
    atoms = pathlib.Path(atoms) if atoms else atlas["dir"] / "atoms.json"
    if not atoms.exists():
        return L.never_ran("uv", f"{atoms} does not exist")
    r = subprocess.run([sys.executable, str(HERE / "support/score.py"), "--fold", "tune", "--atlas", str(atoms)],
                       capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"support/score.py could not judge (exit {r.returncode}): {r.stderr.strip()[-400:]}")
    comp = json.loads(r.stdout)
    locate, cost = L.run_trace(run)
    # The reader logs a document by its url, gold names it by id; raw/documents.jsonl holds both, one to one.
    url_id = {d["url"]: str(d["id"]) for d in map(json.loads, filter(str.strip, (U.ROOT / "raw/documents.jsonl").read_text().splitlines()))}
    locate = {url_id.get(k, k): v for k, v in locate.items()}
    facts, rows, detail = items(atoms, locate)
    return {"system": "uv", "status": "judged", "run": str(run), "atoms": str(atoms),
            "read": L.what_was_read(atlas, cost),
            "population": "tune gold case states",
            "ladder": L.summarize(facts, cost), "rungs": dict(collections.Counter(x["rung"] for x in rows)),
            "identity": {"b_cubed": comp["b_cubed"]["f1"], "ceaf_e": comp["ceaf_e"]["f1"], "lea": comp["lea"]["f1"],
                         "scored": comp["scored"], "coverage": comp["membership_coverage"]},
            "composition": comp, "cost": cost, "detail": detail, "items": rows}
