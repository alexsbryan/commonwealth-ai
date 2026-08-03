#!/usr/bin/env python3
"""Print the M2 mix-study verdict table. Safe to run any time, including
mid-run — missing arms show as (missing) rather than failing.

    .venv/bin/python scripts/summarize_mix_study.py
"""
import json
import pathlib
import sys

# scripts/ -> research/verifier-v0 -> runs/mix-study (artifacts live outside git)
ROOT = pathlib.Path(__file__).resolve().parent.parent / "runs" / "mix-study"
RUNS = [
    ("ref (100 it, A)", ROOT / "ref-probe100/eval/summary.json"),
    ("arm A  (A only)", ROOT / "A/eval/summary.json"),
    ("arm AB (A+B)", ROOT / "AB/eval/summary.json"),
]


def main() -> int:
    rows = []
    for label, path in RUNS:
        rows.append((label, json.loads(path.read_text()) if path.exists() else None))

    print(f"{'run':<18} {'BAcc tolerant':>14} {'BAcc strict':>12} {'parse-fail':>11}")
    print("-" * 58)
    for label, d in rows:
        if d is None:
            print(f"{label:<18} {'(missing)':>14}")
            continue
        print(
            f"{label:<18} {d['macro_avg_bacc_tolerant']:>14.2f} "
            f"{d['macro_avg_bacc']:>12.2f} {d['parse_failures']:>11}"
        )

    by = dict(rows)
    a, ab, ref = by["arm A  (A only)"], by["arm AB (A+B)"], by["ref (100 it, A)"]

    if a and ab:
        delta = ab["macro_avg_bacc_tolerant"] - a["macro_avg_bacc_tolerant"]
        print(f"\nStream B effect (tolerant): {delta:+.2f} BAcc")

        if ref:
            lift = a["macro_avg_bacc_tolerant"] - ref["macro_avg_bacc_tolerant"]
            print(f"A-arm lift over the 100-iter reference: {lift:+.2f} BAcc")
            # The whole reason stage 0 exists: distinguish "B doesn't help" from
            # "nothing trained". Without a lift, a null on B says nothing.
            if abs(lift) < 1.0:
                print(
                    "  -> The arms did not move off the reference. A null on B is\n"
                    "     NOT interpretable here — extend both arms (see README)\n"
                    "     before drawing any conclusion about Stream B."
                )
            elif abs(delta) < 1.0:
                print(
                    "  -> Both arms trained, and B changed nothing at this budget.\n"
                    "     That is a real (budget-scoped) null on Stream B."
                )
    return 0


if __name__ == "__main__":
    sys.exit(main())
