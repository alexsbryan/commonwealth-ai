#!/usr/bin/env python3
"""Compare two pipeline runs over generated worlds, gold deal by gold deal and stage update by stage update.

    python3 compare_runs.py --a <world>/gold:<run A>/score.json [...] [--b <world>/gold:<run B>/score.json ...]

Each pair is a world's gold dir and the score.json that score_ward.py wrote for it; tags.jsonl beside the gold dir
gives the breakdown. A gold deal is served (exactly one record, holding no other deal's statements, at the right
current stage), split, merged, wrong_stage or not_found. A gold stage update is attached when a statement matching it
sits on a record of its own deal. B - A is paired by deal (or update) and carries a 95% bootstrap interval that
resamples worlds, then items within each chosen world.
"""
import argparse, json, random
from collections import Counter, defaultdict
from pathlib import Path

BUCKETS = ["served", "split", "merged", "wrong_stage", "not_found"]
IDS = {"folder", "file", "deal", "kind", "person"}


def load(pairs):
    """{(world, deal): outcome}, {(world, folder, file, deal): attached}, {(world, folder, file, deal): tags}"""
    deals, ups, tags = {}, {}, {}
    for pair in pairs:
        gold, score = pair.rsplit(":", 1)
        world = Path(gold).expanduser().resolve().parent
        rep = json.load(open(Path(score).expanduser()))
        recs = {r["id"]: r for r in rep["deals"]}
        for g, v in rep["gold"].items():
            rs = v["records"]
            r = recs.get(rs[0]) if len(rs) == 1 else None
            deals[(world.name, g)] = ("not_found" if not rs else "split" if len(rs) > 1 else
                                      "merged" if len(r["gold"]) > 1 or r["strays"] else
                                      "wrong_stage" if r["stage"] != v["gold_stage"] else "served")
        for u in rep.get("updates", []):
            ups[(world.name, u["folder"], u["file"], u["deal"])] = bool(u["attached"])
        for line in open(world / "tags.jsonl"):
            t = json.loads(line)
            if t["kind"] == "stage_update":
                tags[(world.name, t["folder"], t["file"], t["deal"])] = t
    return deals, ups, tags


def interval(pairs, n=2000):
    """95% interval of mean(b - a) over {key: (a, b)}, keys starting with the world; worlds, then items, resampled."""
    by = defaultdict(list)
    for key, (a, b) in pairs.items():
        by[key[0]].append(b - a)
    worlds, rng, means = list(by), random.Random(0), []
    for _ in range(n):
        s = [x for w in (rng.choice(worlds) for _ in worlds) for x in rng.choices(by[w], k=len(by[w]))]
        means.append(sum(s) / len(s))
    means.sort()
    return means[int(0.025 * n)], means[int(0.975 * n) - 1]


def rates(label, a, b, keys):
    """One line: n, A rate, B rate, and the paired difference with its interval when B is there."""
    keys = [k for k in keys if k in a and (b is None or k in b)]
    if not keys:
        return f"  {label:<38} n=0"
    ra = sum(a[k] for k in keys) / len(keys)
    out = f"  {label:<38} n={len(keys):<4} A {ra:.3f}"
    if b is not None:
        rb = sum(b[k] for k in keys) / len(keys)
        lo, hi = interval({k: (a[k], b[k]) for k in keys})
        out += f"  B {rb:.3f}  B-A {rb - ra:+.3f} [{lo:+.3f}, {hi:+.3f}]"
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--a", nargs="+", required=True, help="gold dir:score.json, one per world")
    ap.add_argument("--b", nargs="+", help="the same worlds, run B")
    args = ap.parse_args()
    da, ua, tags = load(args.a)
    db, ub, tb = load(args.b) if args.b else (None, None, {})
    tags.update(tb)
    sa = {k: int(v == "served") for k, v in da.items()}
    sb = {k: int(v == "served") for k, v in db.items()} if db else None
    ia = {k: int(v) for k, v in ua.items()}
    ib = {k: int(v) for k, v in ub.items()} if ub else None
    print(f"{len({k[0] for k in da})} worlds, {len(da)} gold deals, {len(ua)} gold stage updates")
    print(rates("deals served", sa, sb, list(da)))
    print(rates("stage updates attached to their deal", ia, ib, list(ua)))
    for name, d in (("A", da), ("B", db)):
        if d:
            c = Counter(d.values())
            print(f"  error buckets {name}: " + "  ".join(f"{x} {c[x]}" for x in BUCKETS))
    # Deal-level tags: a deal has a tag value when any of its stage updates has it.
    dtags = defaultdict(lambda: defaultdict(set))
    for (w, _, _, deal), t in tags.items():
        for f, v in t.items():
            if f not in IDS:
                dtags[(w, deal)][f].add(v)
    fields = sorted({f for t in tags.values() for f in t} - IDS)
    for f in fields:
        print(f"by {f}:")
        for v in sorted({t[f] for t in tags.values()}, key=str):
            ukeys = [k for k, t in tags.items() if t[f] == v]
            dkeys = [k for k, ts in dtags.items() if v in ts[f]]
            print(rates(f"{f}={v} updates attached", ia, ib, ukeys))
            print(rates(f"{f}={v} deals served", sa, sb, dkeys))
            buckets = Counter(da[k] for k in dkeys if k in da)
            print(f"  {'':<38} A buckets " + " ".join(f"{x}:{buckets[x]}" for x in BUCKETS if buckets[x]))


if __name__ == "__main__":
    main()
