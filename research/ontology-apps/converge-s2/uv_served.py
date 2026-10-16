#!/usr/bin/env python3
"""uv's served case state on one run, broken into its outcomes (CONVERGE Session 2's closing reading).

    uv_served.py RUN      # RUN absolute

The ladder's served_state (ladder_uv.served_state) counts, per gold case with a state on the run's documents, whether
its matched record's own `state` is the latest gold state. This splits the misses: no record matched, a wrong state
served, no state served; and lists which gold state became which served one.
"""
import collections, json, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402
import ladder_uv as LU  # noqa: E402

U = LU.U


def main(run):
    found = L.run_atlas(run)
    atoms = L.atlas_view(run, found)["atoms"]
    sliced = L.run_documents(run, found)
    url_id = {d["url"]: str(d["id"]) for d in map(json.loads, filter(str.strip, (U.ROOT / "raw/documents.jsonl").read_text().splitlines()))}
    docs = None if sliced is None or isinstance(sliced, str) else {url_id[u] for u in sliced["documents"] if u in url_id}
    g = json.loads((U.ROOT / "gold/cases.json").read_text())
    tune = {c["id"] for c in g["cases"] if c["fold"] == "tune"}
    gold_doc, _, _, _ = U.load_gold("tune")
    pred, _ = U.atlas_pred(atoms, "case_state")
    match = L.assign(dict(collections.Counter((gold_doc[d], r) for d, r in pred.items() if d in gold_doc)))
    states = [s for s in g["case_states"] if s["case"] in tune and (docs is None or str(s["document"]) in docs)]
    recs = {k: v.get("attributes") or {} for k, v in L.records_of(atoms).items() if v["record_type"] == "case"}
    latest = {}
    for s in sorted(states, key=lambda s: s["date"]):
        latest[s["case"]] = s["state"]
    out, swaps = collections.Counter(), collections.Counter()
    for c, st in latest.items():
        if match.get(c) is None:
            out["no record matched"] += 1
            continue
        got = recs.get(match[c], {}).get("state")
        out["served right" if got == st else "no state served" if not got else "wrong state served"] += 1
        if got != st:
            swaps[f"{st} -> {got}"] += 1
    print(f"{run}: {len(latest)} gold cases with a state on the run's documents: {dict(out)}")
    print(f"  case records {len(recs)}, serving a state {sum(1 for a in recs.values() if a.get('state'))}")
    print(f"  gold -> served, where they differ: {dict(swaps.most_common())}")


if __name__ == "__main__":
    main(pathlib.Path(sys.argv[1]).resolve())
