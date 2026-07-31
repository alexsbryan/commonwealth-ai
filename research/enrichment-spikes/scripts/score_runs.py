#!/usr/bin/env python3
"""Aggregate eval-run JSON files per the frozen G2 metric definition.

Per run: mean over questions of |fact matched| / (|matched| + |missing|)
and of source_score.ratio (README G2 original-fixtures paragraph).

Usage: python scripts/score_runs.py runs/armA/*.json
"""

import json
import sys


def score(path: str) -> tuple[float, float, int]:
    d = json.load(open(path))
    facts, sources = [], []
    for r in d["results"]:
        fs = r["fact_score"]
        m, miss = len(fs["matched"]), len(fs["missing"])
        facts.append(m / (m + miss) if (m + miss) else 0.0)
        sources.append(r["source_score"]["ratio"])
    n = len(d["results"])
    return sum(facts) / n, sum(sources) / n, n


def main() -> None:
    print(f"{'run':<55} {'fact':>8} {'source':>8} {'n':>3}")
    for path in sys.argv[1:]:
        f, s, n = score(path)
        print(f"{path.split('/')[-1]:<55} {f:>8.4f} {s:>8.4f} {n:>3}")


if __name__ == "__main__":
    main()
