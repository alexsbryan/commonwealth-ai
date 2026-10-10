#!/usr/bin/env python3
"""Each author's GVC records against gold at the concept that author declared: the incident (order
ontology-layer-3-any-author step 12; the seat's request of 2026-10-10 for the operator's re-definition of the author gap).

    declared_concept.py --run NAME=DIR [--run NAME=DIR ...] [--resolve NAME=DIR ...]

Gold scores sub-event chains (gold.json: mention -> chain); an author who declares one record per incident is read
here against gold's INCIDENT for each mention instead (gold/mentions.jsonl `incident`; mentions gold leaves unlinked,
incident null, are each their own incident, as gold/README.md makes chain 0 one singleton per mention). A job run's placements are the GVC ladder's (ladder_gvc.measure), a
RESOLVE-alone run's are its clustering.json. Scored by support/score.py's er_score (the one partition scorer), over
the mentions the run placed (`placed`, of `units`), the same restriction the ladder's identity row uses.
"""
import argparse, json, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402
import ladder_gvc as G  # noqa: E402

U = L.load_module("support_score", HERE.parent / "support/score.py")
GOLD_CHAINS = L.BASELINE / "gvc/statements/gold.json"


def incidents():
    return {m["mention_id"]: m.get("incident") for m in G.jsonl(G.CORPUS / "gold/mentions.jsonl")}


def score(placed, units, inc):
    keep = {u: r for u, r in placed.items() if r is not None and u in units}
    r = U.er_score(keep, {u: inc.get(u) or f"unlinked:{u}" for u in keep}) if keep else None
    return {"units": len(units), "placed": len(keep), "gold_unlinked_singletons": sum(1 for u in keep if inc.get(u) is None),
            **({m: round(r[m]["f1"], 3) for m in ("muc", "b_cubed", "ceaf_e", "lea")} if r else {}),
            "conll_f1": round((r["muc"]["f1"] + r["b_cubed"]["f1"] + r["ceaf_e"]["f1"]) / 3, 3) if r else None}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--run", action="append", default=[], help="NAME=job run's gvc leg dir")
    ap.add_argument("--resolve", action="append", default=[], help="NAME=RESOLVE-alone run dir")
    a = ap.parse_args()
    inc = incidents()
    out = {}
    for spec in a.run:
        name, d = spec.split("=", 1)
        m = G.measure(pathlib.Path(d))
        items = m["placements"]["items"]
        out[name] = {"run": d, "kind": "e2e", **score({u: rec for u, (_, rec) in items.items()}, set(items), inc)}
    gold = json.loads(GOLD_CHAINS.read_text())
    for spec in a.resolve:
        name, d = spec.split("=", 1)
        c = json.loads((pathlib.Path(d) / "resolve/clustering.json").read_text())
        out[name] = {"run": d, "kind": "resolve-alone", **score(c, set(gold), inc)}
    print(json.dumps(out, indent=1, sort_keys=True))


if __name__ == "__main__":
    main()
