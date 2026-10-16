#!/usr/bin/env python3
"""Why each current Ward gold deal has no matched deal record, on one run (CONVERGE Session 2's closing reading).

    ward_residual.py RUN [-v]      # RUN absolute: the ladder's own run_atlas/atlas_view read it

Each unmatched deal gets the first of these that holds, over the deal records citing any of its messages: none cites
them; a record names its party (ward/score.py's company_resolver, frozen) but one-to-one matching gave it to another
gold deal; a record names it only as a domain string; records name other parties only; records carry no counterparty.
The last line is a side reading, never the bar: the same score() with a raw domain string also resolving by gold
domain, the one concept the frozen resolver leaves out (it resolves a raw value by written form only).
"""
import collections, email.utils, json, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402
import ladder_ward as LW  # noqa: E402

S = LW.S


def main(run, verbose):
    found = L.run_atlas(run)
    secfiles = S.section_files(str(found["index"]))
    for e in json.loads((S.WARD / "manifest.json").read_text()):
        try:
            S.DATES.setdefault(e["path"], email.utils.parsedate_to_datetime(e["date"]).isoformat())
        except (TypeError, ValueError):
            pass
    g = S.load_gold(S.WARD / "gold")
    view = L.atlas_view(run, found)
    ent, claims = S.load_atlas(view["atoms"], secfiles, view["decisions"])
    tune = {f for f in g["files"] if f.split("/", 1)[0] in LW.TUNE}
    res = S.score(g, ent, claims, tune)
    deals = [e for e in ent.values() if e.get("entity_type") == "deal"]
    gold = {d["id"]: d for d in g["deals"]}
    doms = {c["id"]: {S.fold(d) for d in c.get("domains") or []} for c in g["companies"]}
    _, resolves = S.company_resolver(g, ent)
    by_domain = lambda v, gid: v not in ent and S.fold(v) in doms.get(gid, set())  # noqa: E731
    kinds = collections.Counter("record" if v in ent else "domain" if "." in v and " " not in v else "text"
                                for e in deals for v in S.attr_list(e, "counterparty"))
    kinds["none"] = sum(1 for e in deals if not S.attr_list(e, "counterparty"))
    cls, rows = collections.Counter(), []
    for gid, out in res["stage_served_by_deal"].items():
        if out != "unmatched_deal":
            continue
        d = gold[gid]
        on = [e for e in deals if e.get("_files", set()) & d["files"]]
        vals = [v for e in on for v in S.attr_list(e, "counterparty")]
        c = ("no deal record cites its messages" if not on
             else "a record names its party; one-to-one matching gave it to another gold deal"
             if any(resolves(v, d["counterparty"]) for v in vals)
             else "a record names its party only as a domain string" if any(by_domain(v, d["counterparty"]) for v in vals)
             else "records name other parties only" if vals else "records carry no counterparty")
        cls[c] += 1
        rows.append((gid, c, sorted({ent[v].get("canonical_name") if v in ent else v for v in vals})[:4]))
    print(f"{run}: {len(res['stage_served_by_deal'])} current gold deals, {sum(cls.values())} with no matched record")
    for k, n in cls.most_common():
        print(f"  {n:3d}  {k}")
    if verbose:
        for r in rows:
            print("     ", *r)
    print(f"  deal records {len(deals)}; counterparty values by kind {dict(kinds)}")
    orig = S.company_resolver

    def side(g2, ent2):
        a, r = orig(g2, ent2)
        return a, (lambda v, gid: r(v, gid) or by_domain(v, gid))
    S.company_resolver = side
    try:
        res2 = S.score(g, ent, claims, tune)
    finally:
        S.company_resolver = orig
    pick = lambda r: {k: "could-not-judge" if k in view["unjudged"] else f"{r[k]['hit']}/{r[k]['n']}"  # noqa: E731
                      for k in ("deals", "master_agreements", "stage_served")}
    print(f"  frozen scorer {pick(res)}; side reading, a domain string also resolving by domain {pick(res2)}")


if __name__ == "__main__":
    main(pathlib.Path(sys.argv[1]).resolve(), "-v" in sys.argv[2:])
