#!/usr/bin/env python3
"""Each ledger decision judged against gold, tune fold only: which rule loses the deals.

    audit.py --run <chain dir with S/ and C/ledger-deal.json>

Replays C/ledger-deal.json (one row per act: the transactions it could join and the one it did) with each
act's gold deal from deals.py's labeller. An act joins RIGHT when its transaction already holds its deal,
MERGES when the transaction holds only other deals, SPLITS when it starts a new transaction while an earlier
one in its block holds its deal. A transaction holding only unlabelled acts cannot be judged. A gold deal
whose acts fall in several blocks is lost before the ledger runs (party), and is counted apart.
"""
import argparse, collections, json, pathlib, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import deals as D  # noqa: E402
import score as S  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--run", type=pathlib.Path, required=True)
    ap.add_argument("--examples", type=int, default=0, help="print this many rows per error class")
    ap.add_argument("--status", default="status", help="the member attribute carrying a read information status")
    ap.add_argument("--rule", default="", help="dump every labelled act decided by this rule (a prefix), with its candidates")
    a = ap.parse_args()
    g = S.load_gold(S.WARD / "gold")
    fold = D.FOLDS["tune"]
    label = D.labeller(g, fold)
    deal = {d["id"]: d for d in g["deals"]}
    _, claims = S.load_atlas(json.loads((a.run / "S/atoms.json").read_text())["atoms"], S.section_files("crm-ward-acts"))
    lab = {c["id"]: label(c) for c in claims if c.get("claim_kind") == "deal_mention"}
    anchor = {c["id"]: (c.get("anchor") or "")[:300].replace("\n", " ") for c in claims}
    attrs = {c["id"]: c.get("attributes") or {} for c in claims}
    trace = json.loads((a.run / "C/ledger-deal.json").read_text())
    txns = collections.defaultdict(list)  # (block, position) -> labels of its acts so far
    ids = collections.defaultdict(list)  # (block, position) -> its act ids so far
    out, ex, ev_by = collections.Counter(), collections.defaultdict(list), collections.Counter()
    blocks_of = collections.defaultdict(set)
    for r in trace:
        blk, mine = r["block"], lab.get(r["act"])
        if mine:
            blocks_of[mine].add(blk)
        prior = {i: txns[(blk, i)] for i in range(r["before"])}
        holds = [i for i, ls in prior.items() if mine in ls]
        rule = r["why"].split(" (")[0]
        if not mine:
            verdict = "unlabelled act"
        elif r["to"] is not None:
            ls = {x for x in prior[r["to"]] if x}
            verdict = "right" if mine in ls else "unjudged (transaction unlabelled)" if not ls else \
                      "MERGE, its deal elsewhere" if holds else "MERGE, its deal not yet open"
        elif not holds:
            verdict = "right"
        else:
            tgt = holds[-1]
            if str(tgt) in {str(k) for k in r["illegal"]}:
                verdict = f"SPLIT, target illegal by {r['illegal'][str(tgt)]}"
            elif str(tgt) in r["evidence"]:
                verdict = "SPLIT, target legal with evidence"
            else:
                verdict = "SPLIT, target legal, no evidence"
        out[(rule, verdict)] += 1
        if r["to"] is not None and verdict.split(",")[0] in ("right", "MERGE"):
            ev_by[("+".join(r["evidence"].get(str(r["to"]), [])) or "forced/none", verdict.split(",")[0])] += 1
        if verdict.startswith(("SPLIT", "MERGE")):
            at = {k: attrs[r["act"]].get(k) for k in ("deal_ref", "delivery_point", "period_start", "stage") if attrs[r["act"]].get(k)}
            ex[verdict].append(f"{mine} [{blk.split(':')[-1]}] {rule}; legal {len(r['legal'])} ev {r['evidence']} | {at} | {anchor[r['act']]}")
        if a.rule and mine and rule.startswith(a.rule):
            print(f"\n## {verdict} :: {mine} [{blk.split(':')[-1]}] legal {r['legal']} illegal {r['illegal']}")
            print(f"   ACT {attrs[r['act']]}\n       {anchor[r['act']]}")
            for i in r["legal"][-6:]:
                last = ids[(blk, i)][-1]
                print(f"   T{i} {sorted({x for x in txns[(blk, i)] if x})} n={len(ids[(blk, i)])} | {anchor[last]}")
        txns[(blk, r["to"] if r["to"] is not None else r["before"])].append(mine)
        ids[(blk, r["to"] if r["to"] is not None else r["before"])].append(r["act"])
    by_rule = collections.defaultdict(collections.Counter)
    for (rule, v), n in out.items():
        by_rule[rule][v] += n
    for rule, c in sorted(by_rule.items(), key=lambda x: -sum(x[1].values())):
        print(f"{sum(c.values()):4}  {rule}")
        for v, n in c.most_common():
            print(f"      {n:4}  {v}")
    tot = collections.Counter()
    for (_, v), n in out.items():
        tot[v.split(",")[0]] += n
    print("totals:", dict(tot))
    # a read information status against gold's: the first member of its deal in its block (ledger order) is new
    seen, conf = set(), collections.Counter()
    for r in trace:
        mine = lab.get(r["act"])
        if mine:
            gold = "new" if (r["block"], mine) not in seen else "given"
            seen.add((r["block"], mine))
            conf[(gold, attrs[r["act"]].get(a.status))] += 1
    if any(read for _, read in conf):
        for v in ("new", "given"):
            tp, said, real = conf[(v, v)], sum(n for (g, r), n in conf.items() if r == v), sum(n for (g, r), n in conf.items() if g == v)
            print(f"status {v:5}: precision {tp}/{said}  recall {tp}/{real}  (unread: {conf[(v, None)]})")
    print("attach decisions on labelled acts, by the evidence the chosen transaction had:")
    for e in sorted({e for e, _ in ev_by}, key=lambda e: -(ev_by[(e, "right")] + ev_by[(e, "MERGE")])):
        print(f"      {e:34} right {ev_by[(e, 'right')]:3}  merge {ev_by[(e, 'MERGE')]:3}")
    gd = [d for d in lab.values() if d and deal[d].get("kind", "transaction") == "transaction"]
    multi = {d: b for d, b in blocks_of.items() if len(b) > 1}
    print(f"gold deals with labelled acts in the ledger: {len(blocks_of)}; spread over >1 block (lost before the ledger): {len(multi)}")
    for d, b in sorted(multi.items()):
        print(f"      {d}: {sorted(x.split(':')[-1] for x in b)}")
    unblocked = [i for i, d in lab.items() if d and i not in {r['act'] for r in trace}]
    print(f"labelled acts never reaching the ledger (no block): {len(unblocked)} of {sum(1 for d in lab.values() if d)}")
    for v, rows in ex.items():
        for row in rows[:a.examples]:
            print(f"  [{v}] {row}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
