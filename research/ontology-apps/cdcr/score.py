#!/usr/bin/env python3
"""Score a `svrn enrich resolve-statements` run against gold: one row of the loop's table.

    score.py RUN_DIR GOLD_JSON [--svrn svrn]

Clustering measures come from `svrn bench er-score` (MUC, B3, CEAF-e, LEA, CoNLL F1), never recomputed here.
Beside them: the proposer's recall (a statement whose gold chain already sat in a record before its document
is reachable when that record was among the document's candidates), the refusals, and the model calls per
document. A refused statement is scored alone (summary.json says so).
"""
import argparse, itertools, json, pathlib, subprocess, sys


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("run", type=pathlib.Path)
    ap.add_argument("gold", type=pathlib.Path)
    ap.add_argument("--svrn", default="svrn")
    a = ap.parse_args()
    gold = json.loads(a.gold.read_text())
    summary = json.loads((a.run / "summary.json").read_text())
    p = subprocess.run([a.svrn, "bench", "er-score", str(a.run / "clustering.json"), str(a.gold)],
                       capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit(f"er-score exited {p.returncode}: {p.stderr.strip()}")
    er = json.loads(p.stdout)

    chains_of = {}  # record -> gold chains folded into it so far
    reachable = joinable = 0
    for line in (a.run / "decisions.jsonl").read_text().splitlines():
        d = json.loads(line)
        shown = set(d["candidates"])
        decided = []
        for o in d["outcomes"]:
            chain = gold.get(o["statement"])
            before = {r for r, cs in chains_of.items() if chain in cs}
            if before:
                joinable += 1
                reachable += bool(shown & before)
            out = o["outcome"]
            if "decided" in out:
                decided.append((out["decided"]["record"], chain))
        for r, chain in decided:
            chains_of.setdefault(r, set()).add(chain)

    def links(clusters):  # coreference links, split by whether both mentions are in one document
        by = {}
        for s, c in clusters.items():
            by.setdefault(c, []).append(s)
        within, cross = set(), set()
        for ms in by.values():
            for x, y in itertools.combinations(sorted(ms), 2):
                (within if x.split("/")[0] == y.split("/")[0] else cross).add((x, y))
        return within, cross

    def prf(p, g):
        tp = len(p & g)
        P, R = (tp / len(p) if p else 0.0), (tp / len(g) if g else 0.0)
        return {"p": round(P, 3), "r": round(R, 3), "f1": round(2 * P * R / (P + R), 3) if P + R else 0.0, "n": len(p)}

    clustering = json.loads((a.run / "clustering.json").read_text())
    (pw, px), (gw, gx) = links(clustering), links(gold)
    row = {"run": str(a.run), "documents": summary["documents"], "statements": summary["statements"],
           "within_doc_links": prf(pw, gw), "cross_doc_links": prf(px, gx),
           "records": summary["records"], "gold_chains": len(set(gold.values())),
           "calls_per_document": round(summary["calls_per_document"], 2),
           "proposer_recall": round(reachable / joinable, 3) if joinable else None, "joinable": joinable,
           "tally": summary["tally"], "tokens": summary["tokens"], "wall_seconds": round(summary["wall_seconds"]),
           "er": er}
    print(json.dumps(row, indent=1))


if __name__ == "__main__":
    main()
