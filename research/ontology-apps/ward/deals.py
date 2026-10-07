#!/usr/bin/env python3
"""Where each gold transaction deal is won or lost, per fold — the deals loop's instrument.

    deals.py --corpus crm-ward-acts --atoms <out>/atoms.json [--fold tune|read|all] [--json out.json]

score.py reads the crm-deals bar (one deal atom per gold deal, cited message + resolving
counterparty). This explains the number. Each gold transaction deal gets ONE outcome:

  matched        a deal atom cites one of its messages and its counterparty resolves (the bar's hit)
  stolen         such an atom exists, but the bar gave it to another gold deal first (merged deals)
  wrong_party    deal atoms cite its messages, none with a counterparty that resolves to gold's
  no_atom        member claims cite its messages, but none was composed into a deal atom
  not_extracted  no member claim cites any of its messages: the first-order pass missed it

and each deal atom one of: matched, extra_on_gold (cites a gold deal's messages but matched none:
a split or a wrong party), on_master (cites only master-agreement messages), unlabelled (cites no
gold deal message: noise or an unlabelled deal). Grouping is B-cubed over labelled member claims
(grouping.py). Folds split the gold by folder, so a counterparty never sits in both: tune what you
iterate on, read the other at gates. The pre-registered bar stays score.py's holdout read.
"""
import argparse, collections, difflib, json, pathlib, sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import score as S  # noqa: E402

# Balanced by transaction count (39 / 40); folder = mailbox folder = mostly one counterparty family.
FOLDS = {"tune": {"smurfit", "mesa", "pasadena", "bhp", "tep"},
         "read": {"gas_customers___chris_foster", "citizens_utilities", "palo_alto", "smud", "el_paso_electric"}}


def resolver(g, ent):
    """score.py's one company rule, over a deal atom's counterparty values."""
    _, company_resolves = S.company_resolver(g, ent)

    def resolves(deal_atom, gold_cp):
        return any(company_resolves(v, gold_cp) for v in S.attr_list(deal_atom, "counterparty"))
    return resolves


def bcubed(pairs):
    """pairs: [(member id, gold deal, predicted cluster)]."""
    if not pairs:
        return None
    pred, gold = collections.defaultdict(set), collections.defaultdict(set)
    for m, gd, pc in pairs:
        pred[pc].add(m); gold[gd].add(m)
    lab = {m: gd for m, gd, _ in pairs}; cl = {m: pc for m, _, pc in pairs}
    P = sum(len({x for x in pred[cl[m]] if lab[x] == lab[m]}) / len(pred[cl[m]]) for m in lab) / len(lab)
    R = sum(len({x for x in pred[cl[m]] if lab[x] == lab[m]}) / len(gold[lab[m]]) for m in lab) / len(lab)
    return {"P": round(P, 3), "R": round(R, 3), "F": round(2 * P * R / (P + R), 3) if P + R else 0.0, "n": len(lab)}


def ceaf_e(pairs):
    """Entity-level CEAF (phi4 = Dice of mention sets) over labelled members: a one-to-one alignment of gold
    deals to composed atoms, so a split and a merge both cost; greedy on Dice, which equals the optimum
    whenever no atom overlaps two gold deals by the same amount."""
    if not pairs:
        return None
    gold, pred = collections.defaultdict(set), collections.defaultdict(set)
    for m, gd, pc in pairs:
        gold[gd].add(m); pred[pc].add(m)
    sims = sorted(((2 * len(G & P) / (len(G) + len(P)), gk, pk) for gk, G in gold.items() for pk, P in pred.items() if G & P), key=lambda t: -t[0])
    used_g, used_p, total = set(), set(), 0.0
    for sim, gk, pk in sims:
        if gk not in used_g and pk not in used_p:
            used_g.add(gk); used_p.add(pk); total += sim
    P, R = total / len(pred), total / len(gold)
    return {"P": round(P, 3), "R": round(R, 3), "F": round(2 * P * R / (P + R), 3) if P + R else 0.0}


def labeller(g, folders, overlap=30):
    """A member's gold deal (any kind): the one deal whose messages it cites, else, on a message several deals
    share, the one deal whose gold quote there shares `overlap`+ verbatim characters with the member's cited
    text; None when neither decides. Masters are candidates, so a master's mention never labels a transaction."""
    alld = [d for d in g["deals"] if d["counterparty"].split(":", 1)[0] in folders]
    quotes = collections.defaultdict(list)
    for s in g["stage_updates"]:
        quotes[(s["file"], s["deal"])].append(S.squash(s["quote"]))

    def shared(a, q):
        return difflib.SequenceMatcher(None, a, q, autojunk=False).find_longest_match(0, len(a), 0, len(q)).size

    def label(c):
        hit = [d["id"] for d in alld if d["files"] & c["_files"]]
        if len(hit) <= 1:
            return hit[0] if hit else None
        a = S.squash(c.get("anchor"))
        best = [d for d in hit if any(shared(a, q) >= overlap for f in c["_files"] for q in quotes.get((f, d), []))]
        return best[0] if len(best) == 1 else None
    return label


def analyse(g, ent, claims, folders, member_kinds):
    resolves = resolver(g, ent)
    in_fold = lambda f: f.split("/", 1)[0] in folders  # noqa: E731
    deals = [e for e in ent.values() if e.get("entity_type") == "deal"]
    gd = [d for d in g["deals"] if d.get("kind", "transaction") == "transaction"
          and d["counterparty"].split(":", 1)[0] in folders]
    master_files = {f for d in g["deals"] if d.get("kind") == "master_agreement" for f in d["files"]}
    trans_files = {f for d in gd for f in d["files"]}
    members = [c for c in claims if c.get("claim_kind") in member_kinds and any(in_fold(f) for f in c["_files"])]
    # the bar's own greedy match, in its order
    match, used = {}, set()
    for d in gd:
        for e in deals:
            if e["id"] not in used and e["_files"] & d["files"] and resolves(e, d["counterparty"]):
                match[d["id"]] = e["id"]; used.add(e["id"]); break
    outcome = {}
    for d in gd:
        on = [e for e in deals if e["_files"] & d["files"]]
        if d["id"] in match:
            outcome[d["id"]] = "matched"
        elif any(resolves(e, d["counterparty"]) for e in on):
            outcome[d["id"]] = "stolen"
        elif on:
            outcome[d["id"]] = "wrong_party"
        elif any(c["_files"] & d["files"] for c in members):
            outcome[d["id"]] = "no_atom"
        else:
            outcome[d["id"]] = "not_extracted"
    atom_kind = collections.Counter()
    for e in deals:
        if not any(in_fold(f) for f in e["_files"]):
            continue
        atom_kind["matched" if e["id"] in used else "extra_on_gold" if e["_files"] & trans_files
                  else "on_master" if e["_files"] & master_files else "unlabelled"] += 1
    pairs, label, trans = [], labeller(g, folders), {d["id"] for d in gd}
    for c in members:
        lab = label(c)
        if lab in trans:
            pairs.append((c["id"], lab, c.get("subject") if c.get("subject") in ent else ("solo", c["id"])))
    # stage, on transaction deals only (the part score.py's crm-stage can reach): a stage_update claim on
    # the gold update's message, whose subject is the matched atom, with gold's stage; current = latest by date
    gs = [s for s in g["stage_updates"] if s["deal"] in match or s["deal"] in {d["id"] for d in gd}]
    stage_claims = [c for c in claims if c.get("claim_kind") == "stage_update"]
    sh = sum(1 for s in gs if s["deal"] in match and any(
        s["file"] in c["_files"] and c.get("subject") == match[s["deal"]]
        and (c.get("attributes") or {}).get("stage") == s["stage"] for c in stage_claims))
    cur_ok = cur_n = 0
    for d in gd:
        mine = sorted((s for s in gs if s["deal"] == d["id"]), key=lambda s: S.when(s["file"]))
        if not mine or d["id"] not in match:
            continue
        cur_n += 1
        theirs = [c for c in stage_claims if c.get("subject") == match[d["id"]]]
        last = max(theirs, key=lambda c: max((S.when(f) for f in c["_files"]), default=""), default=None)
        cur_ok += bool(last) and (last.get("attributes") or {}).get("stage") == mine[-1]["stage"]
    return {"gold_deals": len(gd), "outcomes": dict(collections.Counter(outcome.values())),
            "stage": {"hit": sh, "n": len(gs), "recall": round(sh / len(gs), 3) if gs else None},
            "current_stage": {"hit": cur_ok, "n": cur_n},
            "deals_recall": round(sum(v == "matched" for v in outcome.values()) / len(gd), 3) if gd else None,
            "deal_atoms": dict(atom_kind), "grouping": bcubed(pairs), "entities": ceaf_e(pairs),
            "per_deal": {k: v for k, v in sorted(outcome.items())}}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", default="crm-ward-acts")
    ap.add_argument("--atoms", type=pathlib.Path, required=True)
    ap.add_argument("--fold", choices=["tune", "read", "all"], default="tune")
    ap.add_argument("--members", default="deal_mention", help="comma-separated claim kinds composed into deals")
    ap.add_argument("--json", type=pathlib.Path)
    ap.add_argument("--per-deal", action="store_true")
    a = ap.parse_args()
    ent, claims = S.load_atlas(json.loads(a.atoms.read_text())["atoms"], S.section_files(a.corpus))
    g = S.load_gold(S.WARD / "gold")
    folds = ["tune", "read"] if a.fold == "all" else [a.fold]
    out = {f: analyse(g, ent, claims, FOLDS[f], set(a.members.split(","))) for f in folds}
    for f, r in out.items():
        print(f"[{f}] deals {r['deals_recall']} of {r['gold_deals']}  outcomes {r['outcomes']}")
        print(f"       deal atoms {r['deal_atoms']}  grouping {r['grouping']}  entities {r['entities']}")
        print(f"       stage {r['stage']}  current stage {r['current_stage']}")
        if a.per_deal:
            for k, v in r["per_deal"].items():
                print(f"         {v:14} {k}")
    if a.json:
        a.json.write_text(json.dumps(out, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
