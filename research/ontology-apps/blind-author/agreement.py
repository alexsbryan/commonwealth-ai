#!/usr/bin/env python3
"""Two authors, one corpus (campaign ontology-layer D3; order ontology-layer-3-any-author step 9).

    agreement.py --blind RUNS_DIR --ours RUN [--ours RUN ...] --out DIR [--classes CLASSES.json]

Per system, the blind run and ours are read by the same ladder (ladder.run_system; a blind run through the
evaluator's frozen map, ladder.translate). Over the ladder's own gold identity units (placements: Ward tune gold files,
uv tune gold documents, GVC gold event mentions) each run puts a unit on one record or none; the two partitions are
compared with B3 (support/score.py's er_score, the one partition scorer: blind as prediction, ours as reference, over
the units both placed). GVC RESOLVE alone compares the two clustering.json partitions over gold.json's mentions.

Every unit whose co-members differ between the runs is one difference row in DIR/differences.jsonl: what differs
(`kind`), which run's co-members agree better with gold (`closer`), and its `class` from CLASSES (rules written after
reading the residual: example gap, method gap or author error; ONTOLOGY_METHOD §The example checks the work). A row
no rule classes reads `unclassed`, counted, never defaulted. DIR/agreement.json holds the side-by-side summary.
"""
import argparse, collections, json, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402

U = L.load_module("support_score", HERE.parent / "support/score.py")
GOLD_GVC = L.BASELINE / "gvc/statements/gold.json"
LEGS = {"gvc": "gvc", "ward": "ward-tune", "uv": "uv-third"}


def mates(placed):
    """unit -> frozenset of the units on its record (itself included); unplaced units are absent."""
    by = collections.defaultdict(set)
    for u, r in placed.items():
        by[r].add(u)
    return {u: frozenset(by[r]) for u, r in placed.items()}


def f1(a, b):
    i = len(a & b)
    return 2 * i / (len(a) + len(b)) if a or b else 1.0


def compare(system, units, gold, ours, blind):
    """(summary, rows): gold/ours/blind are {unit: chain or record} (ours/blind None = unplaced)."""
    po = {u: r for u, r in ours.items() if r is not None and u in units}
    pb = {u: r for u, r in blind.items() if r is not None and u in units}
    both = set(po) & set(pb)
    mo, mb = mates({u: po[u] for u in both}), mates({u: pb[u] for u in both})
    mg = mates({u: gold[u] for u in units})
    r = U.er_score({u: pb[u] for u in both}, {u: po[u] for u in both}) if both else None
    rows = []
    for u in sorted(units):
        if u not in po and u not in pb:
            continue
        if u in po and u in pb and mo[u] == mb[u]:
            continue
        if u not in pb:
            kind = "blind_unplaced"
        elif u not in po:
            kind = "ours_unplaced"
        elif mb[u] > mo[u]:
            kind = "blind_joins"
        elif mb[u] < mo[u]:
            kind = "blind_splits"
        else:
            kind = "regrouped"
        g = mg[u] & (set(po) | set(pb))
        fo = f1(mo.get(u, frozenset({u})), g & both) if u in both else None
        fb = f1(mb.get(u, frozenset({u})), g & both) if u in both else None
        closer = (None if fo is None else "ours" if fo > fb else "blind" if fb > fo else "tie")
        rows.append({"system": system, "unit": u, "gold": gold[u], "ours": po.get(u), "blind": pb.get(u), "kind": kind,
                     "closer": closer, "ours_mates": len(mo.get(u, ())), "blind_mates": len(mb.get(u, ())),
                     "gold_mates": len(g & both) if u in both else None})
    pick = lambda m: {k: round(r[m][k], 3) for k in ("precision", "recall", "f1")} if r else None  # noqa: E731
    summary = {"units": len(units), "ours_placed": len(po), "blind_placed": len(pb), "both_placed": len(both),
               "b_cubed": pick("b_cubed"), "ceaf_e": pick("ceaf_e"), "lea": pick("lea"),
               "differences": len(rows), "differences_by_kind": dict(collections.Counter(x["kind"] for x in rows))}
    return summary, rows


def ladder_side(r):
    if r.get("status") != "judged":
        return {"status": r.get("status"), "reason": r.get("reason")}
    lad = r["ladder"]
    return {"status": "judged", "run": r["run"], "vocabulary": r.get("vocabulary"), "population": r["population"],
            "n": lad["n"], "hit": lad["hit"], "lost": {s: v["lost"] for s, v in lad["stages"].items()},
            "could_not_judge": {s: v["could_not_judge"] for s, v in lad["stages"].items() if v["could_not_judge"]},
            "identity": r.get("identity"), "bars": r.get("bars"), "documents_read": lad.get("documents_read")}


def system_row(system, blind_run, our_run):
    o = L.run_system(system, our_run)
    if system == "uv":  # a blind index may chapter differently: read the blind run on OUR run's documents
        import ladder_uv  # noqa: PLC0415
        b = ladder_uv.measure(blind_run, sections_of=our_run)
        own = ladder_side(L.run_system(system, blind_run))
    else:
        b, own = L.run_system(system, blind_run), None
    side = {"blind": ladder_side(b), "ours": ladder_side(o)}
    if own:
        side["blind_on_its_own_sections"] = own
    if b.get("status") != "judged" or o.get("status") != "judged":
        return {**side, "agreement": "could-not-judge: a ladder did not judge"}, []
    pb, po = b["placements"]["items"], o["placements"]["items"]
    units = set(pb) & set(po)
    gold = {u: po[u][0] for u in units}
    summary, rows = compare(system, units, gold, {u: po[u][1] for u in units}, {u: pb[u][1] for u in units})
    summary["unit"] = o["placements"]["unit"]
    summary["units_only_blind"], summary["units_only_ours"] = len(set(pb) - set(po)), len(set(po) - set(pb))
    return {**side, "agreement": summary}, rows


def resolve_alone(blind_dir, our_dir, gold_path=GOLD_GVC):
    gold = json.loads(pathlib.Path(gold_path).read_text())
    side = {}
    parts = {}
    for name, d in (("blind", blind_dir), ("ours", our_dir)):
        c = pathlib.Path(d) / "resolve/clustering.json"
        e = pathlib.Path(d) / "er-score.json"
        if not c.exists():
            side[name] = {"status": "never-ran", "reason": f"{c} does not exist"}
            continue
        parts[name] = json.loads(c.read_text())
        er = json.loads(e.read_text()) if e.exists() else None
        side[name] = {"status": "judged", "run": str(d), "conll_f1": er.get("conll_f1") if er else None,
                      **({m: round(er[m]["f1"], 3) for m in ("muc", "b_cubed", "ceaf_e", "lea") if m in er} if er else {})}
    if len(parts) < 2:
        return {**side, "agreement": "could-not-judge: a run is missing"}, []
    units = set(gold)
    summary, rows = compare("gvc-resolve-alone", units, gold, {u: parts["ours"].get(u) for u in units},
                            {u: parts["blind"].get(u) for u in units})
    summary["unit"] = "gold event mention (gold.json)"
    return {**side, "agreement": summary}, rows


def classify(rows, rules):
    """Each row's class: the first rule whose fields all equal the row's (rule keys other than class/why)."""
    for x in rows:
        hit = next((r for r in rules if all(x.get(k) == v for k, v in r.items() if k not in ("class", "why"))), None)
        x["class"] = hit["class"] if hit else "unclassed"
        if hit:
            x["why"] = hit.get("why")
    return dict(collections.Counter((x["system"], x["class"]) for x in rows))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--blind", required=True, help="the blind run dir (legs gvc, ward-tune, uv-third, gvc-resolve-alone)")
    ap.add_argument("--ours", required=True, help="our job run dir (legs gvc, ward-tune, uv-third)")
    ap.add_argument("--ours-resolve", required=True, help="our RESOLVE-alone run dir")
    ap.add_argument("--out", required=True)
    ap.add_argument("--classes", default=None, help="rules file: {\"rules\": [{system, kind, closer?, class, why}]}")
    a = ap.parse_args()
    blind, ours, out = pathlib.Path(a.blind), pathlib.Path(a.ours), pathlib.Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    result, rows = {}, []
    for system, leg in LEGS.items():
        result[system], r = system_row(system, (blind / leg).resolve(), (ours / leg).resolve())
        rows += r
    result["gvc-resolve-alone"], r = resolve_alone(blind / "gvc-resolve-alone", pathlib.Path(a.ours_resolve))
    rows += r
    rules = json.loads(pathlib.Path(a.classes).read_text())["rules"] if a.classes else []
    result["classes"] = {f"{s} {c}": n for (s, c), n in sorted(classify(rows, rules).items())}
    (out / "differences.jsonl").write_text("".join(json.dumps(x, sort_keys=True) + "\n" for x in rows))
    (out / "agreement.json").write_text(json.dumps(result, indent=1, sort_keys=True, default=sorted) + "\n")
    for k, v in result.items():
        if k != "classes":
            print(k, json.dumps(v.get("agreement"), sort_keys=True))
    print("classes", json.dumps(result["classes"]))


if __name__ == "__main__":
    main()
