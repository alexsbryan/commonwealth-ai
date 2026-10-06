#!/usr/bin/env python3
"""Score a `svrn enrich resolve-statements` run against gold: one row of the loop's table.

    score_resolve.py RUN_DIR GOLD_JSON [--svrn svrn]

The one scorer for every example (cdcr, ward, support): a statement's document is the one decisions.jsonl
resolved it in, never parsed out of its id.

Clustering measures come from `svrn bench er-score` (MUC, B3, CEAF-e, LEA, CoNLL F1), never recomputed here.
Beside them: the proposer's recall (a statement whose gold chain already sat in a record before its document
is reachable when that record was among the document's candidates), the refusals, and the model calls per
document. A refused statement is scored alone (summary.json says so).
"""
import argparse, bisect, itertools, json, math, pathlib, random, subprocess, sys


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
    document_of = {}
    reachable = joinable = 0
    for line in (a.run / "decisions.jsonl").read_text().splitlines():
        d = json.loads(line)
        document_of.update((o["statement"], d["document"]) for o in d["outcomes"])
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
                (within if document_of[x] == document_of[y] else cross).add((x, y))
        return within, cross

    def prf(p, g):
        tp = len(p & g)
        P, R = (tp / len(p) if p else 0.0), (tp / len(g) if g else 0.0)
        return {"p": round(P, 3), "r": round(R, 3), "f1": round(2 * P * R / (P + R), 3) if P + R else 0.0, "n": len(p)}

    verdict = verdict_block(a.run, gold)
    clustering = json.loads((a.run / "clustering.json").read_text())
    (pw, px), (gw, gx) = links(clustering), links(gold)
    row = {"run": str(a.run), "documents": summary["documents"], "statements": summary["statements"],
           "within_doc_links": prf(pw, gw), "cross_doc_links": prf(px, gx),
           "records": summary["records"], "gold_chains": len(set(gold.values())),
           "calls_per_document": round(summary["calls_per_document"], 2),
           "proposer_recall": round(reachable / joinable, 3) if joinable else None, "joinable": joinable,
           "tally": summary["tally"], "tokens": summary["tokens"], "wall_seconds": round(summary["wall_seconds"]),
           "verdict": verdict, "er": er}
    print(json.dumps(row, indent=1))


def verdict_block(run, gold, reps=500, seed=7):
    """How much the model's verdict tells, the same way for every answer form: over each statement's shown
    candidates (a candidate is the same particular when gold puts its opening statement in the statement's chain),
    the verdict says "same" for the candidate it decided on (cited, selected, or linked by an evidential field). LR+ = P(says same | same) / P(says same | different),
    LR- likewise for not choosing; in nats, with a 90% bootstrap over documents. Where a forced choice left its
    distribution, Brier and expected calibration error (10 bins) of p(candidate) over the same pairs, and per
    statement: detection AUC (1 - p(none) between statements shown a same candidate and those not) and how often
    the most probable candidate is a same one when one was shown (ranking, apart from where "none" falls)."""
    docs, stmts = [], []
    for line in (run / "decisions.jsonl").read_text().splitlines():
        d = json.loads(line)
        pairs, probs = [], []
        for o in d["outcomes"]:
            dec = o["outcome"].get("decided")
            chose = dec["record"] if dec and dec["decision"] in ("cited", "selected", "field") else None
            p_of = dict(map(tuple, o["choice"]["candidates"])) if o.get("choice") else {}
            if p_of:
                is_same = lambda c: gold.get(c) is not None and gold.get(c) == gold.get(o["statement"])  # noqa: E731
                stmts.append((1 - o["choice"]["none"], any(map(is_same, p_of)), is_same(max(p_of, key=p_of.get))))
            for c in d["candidates"]:
                same = gold.get(c) is not None and gold.get(c) == gold.get(o["statement"])
                pairs.append((c == chose, same))
                if c in p_of:
                    probs.append((p_of[c], same))
        docs.append((pairs, probs))

    def lrs(sample):
        t = [[0, 0], [0, 0]]  # [says][same]
        for pairs, _ in sample:
            for says, same in pairs:
                t[says][same] += 1
        same_n, diff_n = t[0][1] + t[1][1], t[0][0] + t[1][0]
        if not (t[1][1] and t[1][0] and t[0][1] and t[0][0]):
            return None
        return (math.log((t[1][1] / same_n) / (t[1][0] / diff_n)), math.log((t[0][1] / same_n) / (t[0][0] / diff_n)), t)

    point = lrs(docs)
    if point is None:
        return None
    rng = random.Random(seed)
    boots = [b for b in (lrs([rng.choice(docs) for _ in docs]) for _ in range(reps)) if b]
    ci = lambda k: [round(sorted(b[k] for b in boots)[int(q * (len(boots) - 1))], 2) for q in (.05, .95)]  # noqa: E731
    t = point[2]
    out = {"pairs": sum(map(sum, t)), "base_rate_same": round((t[0][1] + t[1][1]) / sum(map(sum, t)), 3),
           "says_same": t[1][1] + t[1][0], "precision": round(t[1][1] / max(t[1][1] + t[1][0], 1), 3),
           "recall": round(t[1][1] / max(t[1][1] + t[0][1], 1), 3),
           "lr_plus_nats": round(point[0], 2), "lr_plus_ci90": ci(0),
           "lr_minus_nats": round(point[1], 2), "lr_minus_ci90": ci(1)}
    probs = [x for _, ps in docs for x in ps]
    if probs:
        bins = [[] for _ in range(10)]
        for p, same in probs:
            bins[min(int(p * 10), 9)].append((p, same))
        ece = sum(len(b) / len(probs) * abs(sum(p for p, _ in b) / len(b) - sum(s for _, s in b) / len(b)) for b in bins if b)
        out.update({"brier": round(sum((p - same) ** 2 for p, same in probs) / len(probs), 4), "ece": round(ece, 4),
                    "mean_p": round(sum(p for p, _ in probs) / len(probs), 3), "auc": auc(probs),
                    "detect_auc": auc([(p, has) for p, has, _ in stmts]),
                    "top_right_given_same": round(sum(r for _, has, r in stmts if has) / max(sum(h for _, h, _ in stmts), 1), 3)})
    return out


def auc(probs):
    """P(a same pair's p exceeds a different pair's), ties half: whether the distribution ranks candidates
    regardless of where argmax puts "none". None when either class is empty."""
    pos = sorted(p for p, same in probs if same)
    neg = sorted(p for p, same in probs if not same)
    if not pos or not neg:
        return None
    wins = sum(bisect.bisect_left(neg, p) + (bisect.bisect_right(neg, p) - bisect.bisect_left(neg, p)) / 2 for p in pos)
    return round(wins / (len(pos) * len(neg)), 3)


if __name__ == "__main__":
    main()
