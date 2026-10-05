#!/usr/bin/env python3
"""The ceiling of each ledger decision class, tune fold only: gold decides one class, code decides the rest.

    oracle.py --run <chain dir with S/> --facets <compose.toml with method = "ledger">

compose.py runs in process with ledger.compose_blocks wrapped: for a labelled act whose decision falls in the
named class, gold picks the move from the same moves code had (the latest transaction among those it could
join that already holds the act's deal, else NEW). An unlabelled act keeps code's move. Every arm is scored
by deals.analyse, as ablate.py scores its arms, so a class's ceiling is read on the bar it would move. The N arms
write gold's information status into compose's input instead, so ledger.py's own `status` path applies it.
"""
import argparse, collections, contextlib, copy, functools, io, json, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import ablate as A  # noqa: E402
import compose as C  # noqa: E402
import deals as D  # noqa: E402
import ledger as L  # noqa: E402
import score as S  # noqa: E402

CLASSES = {
    "O0 code decides all": (),
    "O1 gold: legal, none with evidence": ("none with evidence",),
    "O2 gold: the one with evidence": ("code: the one legal instance",),
    "O3 gold: latest of several with evidence": ("code: the latest of",),
    "O4 gold: no legal instance": ("new: no legal instance",),
    "O5 gold: forced by a deal ref": ("forced",),
    "O6 gold decides every class": ("",),
}
# gold's information status on each labelled act, through ledger.py's `status` path: the first act of its deal
# in its block (in the ledger's own order) is new, every later one given; code applies it
NOVELTY = {"N1 gold new/given": "both", "N2 gold new only": "new", "N3 gold given only": "given"}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--run", type=pathlib.Path, required=True)
    ap.add_argument("--facets", type=pathlib.Path, required=True)
    ap.add_argument("--out", type=pathlib.Path, required=True)
    a = ap.parse_args()
    g = S.load_gold(S.WARD / "gold")
    fold = D.FOLDS["tune"]
    label = D.labeller(g, fold)
    secfiles = S.section_files("crm-ward-acts")
    _, claims = S.load_atlas(copy.deepcopy(json.loads((a.run / "S/atoms.json").read_text())["atoms"]), secfiles)
    lab = {c["id"]: label(c) for c in claims if c.get("claim_kind") == "deal_mention"}
    orig = L.compose_blocks
    rows = {}
    trace = json.loads((a.run / "C/ledger-deal.json").read_text())
    seen, first = set(), {}
    for r in trace:  # the ledger's order and blocks, from the code run being oracled
        d = lab.get(r["act"])
        if d:
            first[r["act"]] = (r["block"], d) not in seen
            seen.add((r["block"], d))
    base = json.loads((a.run / "S/atoms.json").read_text())
    for name, which in NOVELTY.items():
        atoms = copy.deepcopy(base)
        n = collections.Counter()
        for x in atoms["atoms"]:
            i = x["data"].get("id")
            if x["atom_type"] == "Claim" and i in first and which in ("both", "new" if first[i] else "given"):
                x["data"].setdefault("attributes", {})["status"] = "new" if first[i] else "given"; n["new" if first[i] else "given"] += 1
        d_ = a.out / name.split()[0]
        d_.mkdir(parents=True, exist_ok=True)
        (d_ / "in.json").write_text(json.dumps(atoms))
        CLASSES[name] = (d_ / "in.json", n)
    for name, prefixes in CLASSES.items():
        src = a.run / "S/atoms.json"
        if isinstance(prefixes, tuple) and prefixes and isinstance(prefixes[0], pathlib.Path):
            (src, set_n), prefixes = prefixes, ()
        n = collections.Counter()

        def override(c, legal, t, why, prefixes=prefixes, n=n):
            mine = lab.get(c["id"])
            if not mine or not prefixes or not any(p in why for p in prefixes):
                return t, why
            hold = [x for x in legal if any(lab.get(m["id"]) == mine for m in x.members)]
            pick = hold[-1] if hold else None
            n["changed" if pick is not t else "kept"] += 1
            return pick, f"oracle {'attach' if pick else 'new'} ({why})"
        L.compose_blocks = functools.partial(orig, override=override)
        out = a.out / name.split()[0]
        sys.argv = ["compose.py", "--atoms", str(src), "--facets", str(a.facets), "--out", str(out)]
        with contextlib.redirect_stdout(io.StringIO()):
            C.main()
        ent, cl = S.load_atlas(json.loads((out / "atoms.json").read_text())["atoms"], secfiles)
        res = D.analyse(g, ent, cl, fold, {"deal_mention"})
        res.pop("per_deal")
        opt = A.optimal(g, ent)
        if src != a.run / "S/atoms.json":
            n.update(set_n)
        rows[name] = {"oracle moves": dict(n), "optimal": opt, **res}
        gr, en = res["grouping"], res["entities"]
        print(f"{name:42} {dict(n)!s:28} bar {res['deals_recall']} opt {opt['recall']} B3 {gr['P']}/{gr['R']}/{gr['F']} "
              f"CEAF {en['F']} stage {res['stage']['hit']} cur {res['current_stage']['hit']}/{res['current_stage']['n']} atoms {res['deal_atoms']}")
    L.compose_blocks = orig
    (a.out / "oracle.json").write_text(json.dumps(rows, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
