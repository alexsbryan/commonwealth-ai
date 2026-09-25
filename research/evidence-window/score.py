#!/usr/bin/env python3
"""Score an evidence window against gold facts. No model call, deterministic, re-scorable.

  recall  = gold facts present in the window   — upper bound on answer correctness
  density = gold-bearing chars / window chars  — what the rest of the window costs

Report the PAIR. Widening the window raises recall and drops density for free,
so either number alone is gameable.

Input is whatever `svrn eval run --prod-pipeline --output <f>` already writes.
Gold lives in a sidecar TSV: no bank schema change, no Rust change.
"""
import collections, json, pathlib, re, sys, unicodedata


def norm(s):
    return re.sub(r"\s+", " ", unicodedata.normalize("NFKD", s).casefold()).strip()


def load_gold(path):
    gold = {}
    for line in pathlib.Path(path).read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            qid, cls, facts = line.split("\t")[:3]
            gold[qid] = (cls, [norm(f) for f in facts.split("||") if f.strip()])
    return gold


def score_row(row, facts):
    raw = [c.get("snippet", "") for c in row.get("retrieved", [])]
    txt = [norm(c) for c in raw]
    found = {f for f in facts if any(f in c for c in txt)}
    bearing = sum(len(r) for r, t in zip(raw, txt) if any(f in t for f in facts))
    total = sum(len(r) for r in raw)
    return (len(found) / len(facts), bearing / total if total else 0.0,
            total, sorted(set(facts) - found))


def main(eval_json, gold_tsv, arm, results_tsv):
    ev, gold = json.load(open(eval_json)), load_gold(gold_tsv)
    by_class, unlabelled, misses = collections.defaultdict(list), 0, collections.Counter()

    for row in ev["results"]:
        g = gold.get(str(row["question_id"]))
        if not g:
            unlabelled += 1
            continue
        cls, facts = g
        rec, den, chars, missing = score_row(row, facts)
        by_class[cls].append((rec, den, chars))
        misses.update(missing)

    print(f"\n{arm}")
    with open(results_tsv, "a") as fh:
        for cls in sorted(by_class):
            rs = by_class[cls]
            n = len(rs)
            rec, den, chars = (sum(c) / n for c in zip(*rs))
            print(f"  {cls:<22} n={n:<4} recall={rec:.3f}  density={den:.3f}  chars={chars:.0f}")
            fh.write(f"{arm}\t{cls}\t{n}\t{rec:.4f}\t{den:.4f}\t{chars:.0f}\n")

    if not by_class:
        print("  (no labelled rows)")
    if unlabelled:
        print(f"  {unlabelled} retrieved rows had no gold entry — not scored")
    if misses:
        print("  most-missed: " + ", ".join(f"{f!r}x{c}" for f, c in misses.most_common(5)))


if __name__ == "__main__":
    if len(sys.argv) != 5:
        sys.exit("usage: score.py <eval.json> <gold.tsv> <arm-name> <results.tsv>")
    main(*sys.argv[1:])
