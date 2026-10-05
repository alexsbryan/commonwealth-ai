#!/usr/bin/env python3
"""Join the three hand-labelled batches into one gold set, gold/cases.json (GOLD_SPEC.md, Output).

    merge_gold.py [--root ~/.svrnmesh/bench-corpora/uv-support]

Batches label disjoint threads, so a case that crosses a batch boundary arrives as two cases. The
joins below are the labellers' own findings, each with the maintainer's words that decide it; every
other case is kept as labelled. Then folds are closed over threads: threads that share a case are one
component, and a component takes the fold most of its documents already had, so no case spans both.
Every join and every moved thread is printed. Exit 1 when a check fails.
"""
import argparse, collections, hashlib, json, pathlib, sys

# (case id, issue whose case it is, evidence) — the cross-batch findings from the labellers' uncertain lists
JOINS = [
    ("b1-c26", 313, "#1589 closed by a maintainer 'to merge into #313' ('I believe this is the same as #313')"),
    ("b3-c16", 1367, "maintainer on #1416: 'I believe this is the same as #1367'; PR #1421 closed both"),
    ("b2-c12", 1427, "maintainer routes the spin-off: 'track ... the OS detection issue over in #1427'"),
    ("b2-c28", 1699, "same case as #1699 ('devise testing strategy for uv's cache')"),
    ("b2-c30", 1396, "#1598 closed 'in favor of' #1396"),
    ("b2-c31", 1632, "#1598 closed 'in favor of' #1632"),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", type=pathlib.Path, default=pathlib.Path.home() / ".svrnmesh/bench-corpora/uv-support")
    a = ap.parse_args()
    docs = {str(d["id"]): d for d in map(json.loads, (a.root / "raw/documents.jsonl").read_text().splitlines())}
    plan = json.loads((a.root / "plan.json").read_text())
    area_fold = {ar: f for f, v in plan["folds"].items() for ar in v["areas"]}
    thread_fold = {str(i): area_fold[k["provisional_area"]] for k in plan["clusters"] for i in k["issues"]}
    batches = [json.loads((a.root / f"gold/batch-{b}.json").read_text()) for b in ("b1", "b2", "b3")]
    cases = {c["id"]: dict(c, documents=[str(x) for x in c["documents"]]) for g in batches for c in g["cases"]}
    states = [dict(s, document=str(s["document"])) for g in batches for s in g["case_states"]]
    none = {str(x) for g in batches for x in g["none"]}
    read = [str(x) for g in batches for x in g["documents_read"]]

    parent = {i: i for i in cases}

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]; x = parent[x]
        return x
    for cid, issue, why in JOINS:
        target = [i for i, c in cases.items() if str(issue) in c["documents"] and i != cid]
        if len(target) != 1:
            print(f"FAIL join {cid} -> #{issue}: {len(target)} cases hold the issue body"); return 1
        parent[find(cid)] = find(target[0])
        print(f"join {cid} -> {target[0]} (#{issue}): {why}")
    merged = collections.defaultdict(list)
    for i in cases:
        merged[find(i)].append(i)

    # folds closed over threads: threads sharing a case are one component, which takes its documents' majority fold
    tparent = {t: t for t in set(thread_fold) | {str(docs[d]["thread"]) for d in read}}

    def tfind(x):
        while tparent[x] != x:
            tparent[x] = tparent[tparent[x]]; x = tparent[x]
        return x
    for root, ids in merged.items():
        ts = sorted({str(docs[d]["thread"]) for i in ids for d in cases[i]["documents"]})
        for t in ts[1:]:
            tparent[tfind(t)] = tfind(ts[0])
    comp = collections.defaultdict(list)
    for t in tparent:
        comp[tfind(t)].append(t)
    final_fold, moved = {}, []
    for ts in comp.values():
        n = collections.Counter(thread_fold.get(str(docs[d]["thread"])) for d in read if str(docs[d]["thread"]) in ts)
        n.pop(None, None)
        f = n.most_common(1)[0][0] if n else None
        for t in ts:
            final_fold[t] = f
            if thread_fold.get(t) and thread_fold[t] != f:
                moved.append((t, thread_fold[t], f))
    for t, was, now in sorted(moved):
        print(f"thread #{t} moved {was} -> {now} (shares a case with the other fold)")

    out_cases, cid_of = [], {}
    for root, ids in sorted(merged.items()):
        members = sorted({d for i in ids for d in cases[i]["documents"]}, key=lambda d: (len(d), d))
        new = "case-" + hashlib.sha1("|".join(members).encode()).hexdigest()[:10]  # identity from its members
        first = cases[min(ids, key=lambda i: -len(cases[i]["documents"]))]
        folds = {final_fold[str(docs[d]["thread"])] for d in members}
        out_cases.append({"id": new, "summary": first["summary"], "kind": first["kind"], "area": first["area"],
                          "documents": members, "fold": folds.pop() if len(folds) == 1 else "SPANS",
                          "labelled_as": sorted(ids)})
        for i in ids:
            cid_of[i] = new
    out_states = [dict(s, case=cid_of[s["case"]]) for s in states]
    uncertain = [dict(u, batch=g["cases"][0]["id"].split("-")[0]) for g in batches for u in g["uncertain"]]

    member = collections.defaultdict(set)
    for c in out_cases:
        for d in c["documents"]:
            member[d].add(c["id"])
    checks = {
        "every read document is in a case or none": set(read) == set(member) | none,
        "no document is in both a case and none": not (set(member) & none),
        "every state's document is a member of its case": all(s["document"] in next(c for c in out_cases if c["id"] == s["case"])["documents"] for s in out_states),
        "every quote is verbatim in its document": all(s["quote"] in docs[s["document"]]["body"] for s in out_states),
        "no case spans both folds": all(c["fold"] in ("tune", "read") for c in out_cases),
    }
    gold = {"repo": batches[0]["repo"], "fetched_at": batches[0]["fetched_at"], "documents_read": read,
            "cases": out_cases, "case_states": out_states, "none": sorted(none), "uncertain": uncertain,
            "joins": [{"case": c, "issue": i, "evidence": w} for c, i, w in JOINS],
            "moved_threads": [{"thread": t, "from": w, "to": n} for t, w, n in moved]}
    (a.root / "gold/cases.json").write_text(json.dumps(gold, indent=1))
    fc = collections.Counter(c["fold"] for c in out_cases)
    multi = sum(1 for c in out_cases if len({docs[d]["thread"] for d in c["documents"] if docs[d]["kind"] == "issue"}) > 1)
    print(f"cases {len(out_cases)} (tune {fc['tune']}, read {fc['read']}; {multi} span 2+ issues)  states {len(out_states)} "
          f"{dict(collections.Counter(s['state'] for s in out_states))}  none {len(none)}  documents {len(read)}  "
          f"in 2+ cases {sum(1 for v in member.values() if len(v) > 1)}  uncertain {len(uncertain)}")
    for k, ok in checks.items():
        print(f"{'ok  ' if ok else 'FAIL'} {k}")
    return 0 if all(checks.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
