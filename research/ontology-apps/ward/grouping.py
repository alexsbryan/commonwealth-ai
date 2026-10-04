#!/usr/bin/env python3
"""B-cubed of composed deals over deal_act members, against gold transaction deals (dev scope).
A member is labelled with the gold deal whose messages contain its cited file, when exactly one does.

    grouping.py <corpus> <atoms.json>     # e.g. crm-ward-acts <out>/atoms.json from compose.py

The deals bar rewards splitting (any fragment may match) and the stage bar rewards grouping, so neither
judges a compose rule; this does. The two reference policies bound it: singletons are precision 1 with
low recall, one group per block is the most recall a within-block linking rule can reach."""
import sys, json, collections, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import score as S

def labelled(corpus, atoms_p):
    secfiles = S.section_files(corpus)
    ent, claims = S.load_atlas(json.loads(pathlib.Path(atoms_p).read_text())["atoms"], secfiles)
    g = S.load_gold(S.WARD / "gold")
    smoke = set((S.HERE / "smoke_sections.txt").read_text().strip().split(","))
    dev = {f for s in smoke for f, _ in secfiles.get(s, [])} & g["files"]
    gd = [d for d in g["deals"] if d.get("kind", "transaction") == "transaction" and d["files"] & dev]
    out, amb = [], 0
    for c in claims:
        if c.get("claim_kind") != "deal_act" or not c["_files"] & dev:
            continue
        hit = {d["id"] for d in gd if d["files"] & c["_files"]}
        if len(hit) == 1:
            blk = (ent.get(c.get("subject")) or {}).get("attributes", {}).get("counterparty")
            out.append((c["id"], next(iter(hit)), c.get("subject") if c.get("subject") in ent else None, blk))
        elif len(hit) > 1:
            amb += 1
    return out, amb

def bcubed(rows, cluster_of):
    pred = collections.defaultdict(set); gold = collections.defaultdict(set)
    for r in rows:
        pred[cluster_of(r)].add(r[0]); gold[r[1]].add(r[0])
    lab = {r[0]: r[1] for r in rows}; cl = {r[0]: cluster_of(r) for r in rows}
    P = sum(len({m for m in pred[cl[i]] if lab[m] == lab[i]}) / len(pred[cl[i]]) for i in lab) / len(lab)
    R = sum(len({m for m in pred[cl[i]] if lab[m] == lab[i]}) / len(gold[lab[i]]) for i in lab) / len(lab)
    return round(P, 3), round(R, 3), round(2 * P * R / (P + R), 3) if P + R else 0

if __name__ == "__main__":
    rows, amb = labelled(sys.argv[1], sys.argv[2])
    print(f"labelled members {len(rows)} (ambiguous {amb}), gold deals {len({r[1] for r in rows})}")
    print("  composed           P/R/F", bcubed(rows, lambda r: r[2] or ("solo", r[0])))
    print("  ref: singletons    P/R/F", bcubed(rows, lambda r: ("solo", r[0])))
    print("  ref: one per block P/R/F", bcubed(rows, lambda r: r[3] or ("solo", r[0])))
