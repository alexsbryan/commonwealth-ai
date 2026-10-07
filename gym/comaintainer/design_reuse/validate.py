#!/usr/bin/env python3
"""Validate the design-reuse bank: provenance, excerpts, leakage, splits.

    python3 gym/comaintainer/design_reuse/validate.py [--json]

Exit codes (four verdicts, house rule):
  0  clean
  1  problems found (each printed with its case)
  4  empty bank  — a run that checked nothing verified nothing
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common as C  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    cases = C.load_cases()
    if not cases:
        print("validate: EMPTY BANK — nothing verified", file=sys.stderr)
        return 4

    problems: list[str] = []
    report = []
    for case in cases:
        bad = C.verify_case(case)
        problems += bad
        report.append({"id": case["id"], "base": case["provenance"]["base_sha"][:9],
                       "outcome": case["provenance"]["outcome_sha"][:9],
                       "label": case["provenance"]["label"],
                       "candidates": len(case["dossier"]["candidates"]),
                       "problems": bad})

    splits = C.assign_splits(cases)
    dev = sum(1 for s in splits.values() if s == "dev")

    if args.json:
        print(json.dumps({"cases": report, "problems": problems,
                          "splits": splits, "dev": dev, "holdout": len(splits) - dev},
                         indent=2))
    else:
        for row in report:
            mark = "ok " if not row["problems"] else "BAD"
            print(f"{mark} {row['id']:<52} {row['base']}..{row['outcome']} "
                  f"cand={row['candidates']} {row['label']}")
            for p in row["problems"]:
                print(f"    ✗ {p}")
        print(f"\nbank: {len(cases)} cases, {dev} dev / {len(splits) - dev} holdout, "
              f"{len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
