#!/usr/bin/env python3
"""O-T0 (feature-fidelity): K1 list recall read straight from the typed atlas, no model.

    records_recall.py [--atlas ~/.svrnmesh/indexes/ei7-ans/atlas] [--bank bank.toml] [--json out.json]

Study 1 scored the ontology only through a chat turn, so a low K1 number could
not say whether extraction missed the members or the answer step dropped them.
This reads the records the read side would serve and splits each K1 row into
the stage where it stops:

  hoard     no hoard entity matches the question's findspot name (or the match
            is ambiguous after the year narrows it)
  link      a hoard matched but no coin record points at it — by the coin's
            `hoard` ref or by a Relation whose participants hold both
  members   coins are linked; recall is what their `mint` / `ruler` /
            `denomination` attributes name, against the gold facts

Recall is reported against `facts` (every gold member) and against
`members_local` (members the text attests in a hoard paragraph — the most any
extractor could find). Matching is the bank's own: `make_bank.fold` / `rx` /
`short`, word-bounded and case-folded, so this cannot disagree with the bank
about what a member is called.
"""
import argparse, collections, json, pathlib, re, sys, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from make_bank import fold, rx, short  # noqa: E402  (one matcher: the bank's)

KIND_ATTR = {"mints": "mint", "rulers": "ruler", "denominations": "denomination"}


def load_atlas(atlas):
    atoms = json.loads((atlas / "atoms.json").read_text())["atoms"]
    ents = {a["data"]["id"]: a["data"] for a in atoms if a["atom_type"] == "Entity"}
    rels = [a["data"] for a in atoms if a["atom_type"] == "Relation"]
    return ents, rels


def names_of(e):
    out = [e.get("canonical_name") or ""] + list(e.get("aliases") or [])
    fs = (e.get("attributes") or {}).get("findspot")
    return [n for n in out + ([fs] if fs else []) if n]


def match_hoards(ents, gold, question):
    pat = rx(gold["name"])
    cands = [e for e in ents.values() if e.get("entity_type") == "hoard"
             and any(pat.search(fold(n)) for n in names_of(e))]
    year = re.search(r"found (?:before )?(\d{4})", question)
    if year and len(cands) > 1:
        dated = [e for e in cands if year.group(1) in json.dumps(e, ensure_ascii=False)]
        cands = dated or cands
    return cands


def linked_coins(ents, rels, hoard_ids):
    coins = {cid for cid, e in ents.items() if e.get("entity_type") == "coin"
             and (e.get("attributes") or {}).get("hoard") in hoard_ids}
    via_rel, direct = set(), set()
    for r in rels:
        p = set(r.get("participants") or [])
        if p & hoard_ids:
            for pid in p - hoard_ids:
                t = ents.get(pid, {}).get("entity_type")
                if t == "coin":
                    via_rel.add(pid)
                elif t in ("mint", "ruler"):
                    direct.add(pid)
    return coins, via_rel - coins, direct


def record_members(ents, coin_ids, direct_ids, kind):
    attr, out = KIND_ATTR[kind], []
    for cid in coin_ids:
        v = (ents[cid].get("attributes") or {}).get(attr)
        if v:
            out.append(ents[v]["canonical_name"] if v in ents else v)
    if kind in ("mints", "rulers"):
        want = "mint" if kind == "mints" else "ruler"
        out += [ents[d]["canonical_name"] for d in direct_ids if ents[d].get("entity_type") == want]
    return out


def found(gold_labels, names):
    folded = [fold(n) for n in names]
    return [g for g in gold_labels if any(rx(short(g)).search(n) for n in folded)]


def row(ents, rels, q, gold):
    hoards = match_hoards(ents, gold, q["question"])
    hid = {h["id"] for h in hoards}
    coins, via_rel, direct = linked_coins(ents, rels, hid)
    names = record_members(ents, coins | via_rel, direct, gold["kind"])
    hit_all, hit_local = found(gold["facts"], names), found(gold["members_local"], names)
    stray = [n for n in set(names) if not found(gold["members"], [n])]
    stage = ("hoard" if not hoards else "link" if not (coins or via_rel or direct)
             else "members")
    return {
        "id": q["id"], "kind": gold["kind"], "stage": stage,
        "hoard_entities": [h.get("canonical_name") for h in hoards],
        "coins_by_ref": len(coins), "coins_by_relation": len(via_rel), "direct_members": len(direct),
        "recall": len(hit_all) / len(gold["facts"]),
        "recall_local": len(hit_local) / len(gold["members_local"]) if gold["members_local"] else None,
        "found": hit_all, "missed_local": [m for m in gold["members_local"] if m not in hit_local],
        "stray": sorted(stray),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--atlas", type=pathlib.Path,
                    default=pathlib.Path.home() / ".svrnmesh/indexes/ei7-ans/atlas")
    ap.add_argument("--bank", type=pathlib.Path, default=HERE / "bank.toml")
    ap.add_argument("--json", type=pathlib.Path)
    a = ap.parse_args()
    ents, rels = load_atlas(a.atlas)
    qs = [q for q in tomllib.loads(a.bank.read_text())["questions"]
          if q["category"] == "k1_list_them_all"]
    rows = [row(ents, rels, q, json.loads((HERE / "gold" / f"{q['id']}.json").read_text()))
            for q in qs]

    hoards = [e for e in ents.values() if e.get("entity_type") == "hoard"]
    coins = [e for e in ents.values() if e.get("entity_type") == "coin"]
    census = {
        "hoard_entities": len(hoards), "coin_entities": len(coins),
        "coins_with_hoard_ref": sum(1 for c in coins if (c.get("attributes") or {}).get("hoard")),
        "relations": len(rels),
        "relation_types": dict(collections.Counter(r.get("relation_type") for r in rels)),
    }
    stages = collections.Counter(r["stage"] for r in rows)
    mean = lambda xs: sum(xs) / len(xs) if xs else float("nan")  # noqa: E731
    loc = [r["recall_local"] for r in rows if r["recall_local"] is not None]
    summary = {"n": len(rows), "stages": dict(stages),
               "mean_recall": mean([r["recall"] for r in rows]), "mean_recall_local": mean(loc)}

    print(f"atlas  {a.atlas}")
    print("census " + json.dumps(census))
    print(f"\n{'id':34} {'stage':8} {'hoards':>6} {'ref':>4} {'rel':>4} {'dir':>4} {'recall':>6} {'local':>6}")
    for r in rows:
        rl = "-" if r["recall_local"] is None else f"{r['recall_local']:.2f}"
        print(f"{r['id']:34} {r['stage']:8} {len(r['hoard_entities']):6} {r['coins_by_ref']:4} "
              f"{r['coins_by_relation']:4} {r['direct_members']:4} {r['recall']:6.2f} {rl:>6}")
    print(f"\nn={summary['n']}  stages={dict(stages)}  mean recall {summary['mean_recall']:.3f}"
          f"  vs local ceiling {summary['mean_recall_local']:.3f}")
    if a.json:
        a.json.write_text(json.dumps({"census": census, "summary": summary, "rows": rows}, indent=1))
    # co-lineage instrument contract: the last stdout line is the value.
    print(json.dumps({"value": round(summary["mean_recall_local"], 4),
                      "artifact": str(a.json) if a.json else None}))


if __name__ == "__main__":
    main()
