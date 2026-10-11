"""What H1's 31 held GVC statements were, against gold (seat check of 3ce2b92c1, 2026-10-09).

    python3 held_vs_gold.py   # reads this directory and baseline-3sys-20261009/gvc/statements/gold.json; prints JSON

A named record is read as the gold cluster most of its members (the treatment's clustering) belong to. "rule" is
the control's decider applied to these same sources: the more precise of model choice and proposed answer, i.e.
the proposed answer. The posteriors are recomputed from each row's sources with weigh.rs's ln(p(K-1)/(1-p)) and
checked against the logged ones before anything else is read.
"""
import collections, json, math, pathlib

HERE = pathlib.Path(__file__).resolve().parent
GOLD = pathlib.Path.home() / ".svrnmesh/bench-corpora/baseline-3sys-20261009/gvc/statements/gold.json"


def posteriors(row, drop=()):
    alts = [a for a, _ in row["alternatives"]]
    k = len(alts) + 1  # the named records and "none"
    score = dict.fromkeys(alts, 0.0)
    for s in row["sources"]:
        if s["source"] not in drop and s["record"] in score:
            p = s["precision"]
            score[s["record"]] += math.log(p * (k - 1) / (1 - p))
    z = sum(math.exp(v) for v in score.values()) + 1.0
    return {a: math.exp(v) / z for a, v in score.items()}


def main():
    gold = json.loads(GOLD.read_text())
    clustering = json.loads((HERE / "gvc-treatment2.clustering.json").read_text())
    members = collections.defaultdict(list)
    for s, r in clustering.items():
        members[r].append(s)

    def entity(rec):
        c = collections.Counter(gold.get(m) for m in members.get(rec) or [rec] if m in gold)
        return c.most_common(1)[0][0] if c else None

    rows = [json.loads(l) for l in (HERE / "gvc-treatment2.held.jsonl").read_text().splitlines() if l.strip()]
    worst = max(abs(posteriors(r)[a] - p) for r in rows for a, p in r["alternatives"])
    assert worst < 1e-9, f"recomputed posteriors differ from the logged ones by {worst}"
    out, agreement_rows = collections.Counter(), []
    for r in rows:
        g, by = gold.get(r["statement"]), {s["source"]: s["record"] for s in r["sources"]}
        p, m = by.get("proposed_answer"), by.get("model_choice")
        pr, mr = p is not None and entity(p) == g, m is not None and entity(m) == g
        out["held"] += 1
        out["model_vs_proposed" if p and m and p != m else "sources_agree"] += 1
        out["rule_right" if pr else "rule_wrong"] += 1
        out["model_right" if mr else "model_wrong"] += 1
        out["neither_named_record_right"] += not (pr or mr)
        if p and m and p == m:
            agreement_rows.append({"statement": r["statement"], "posterior": round(max(posteriors(r).values()), 3),
                                   "posterior_without_date": round(max(posteriors(r, ("document_date",)).values()), 3)})
    print(json.dumps({"posteriors_reproduced_within": worst, "counts": dict(out), "agreement_rows": agreement_rows}, indent=1))


if __name__ == "__main__":
    main()
