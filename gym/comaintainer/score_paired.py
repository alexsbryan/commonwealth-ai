#!/usr/bin/env python3
"""Compare two scored runs on the episodes they BOTH answered.

    python3 gym/comaintainer/score_paired.py runs/<a> runs/<b>

Why this exists. Comparing two runs by their separate headline numbers is
invalid when they cover different item subsets: on 2026-08-18 that error made
a frontier-vs-local gap read as 5.6 points when the paired gap on shared
items was 13.0. Independent confidence intervals do not answer "is A better
than B" either — the paired discordant counts do (McNemar).

Zero model calls: both runs are already on disk.
"""
from __future__ import annotations

import argparse
import gzip
import json
import sys
from math import comb
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "next-edit" / "golden"))

from score import extract_verdict            # noqa: E402
from score_golden import (                   # noqa: E402  one formula, one home
    cohens_kappa, kappa_band, kappa_ci,
)

MIN_PAIRED = 20          # below this the comparison is not reportable
MIN_DISCORDANT = 25      # below this McNemar is underpowered, and says so


def load_run(d: Path) -> tuple[dict[str, str], dict]:
    meta = json.loads((d / "meta.json").read_text())
    out: dict[str, str] = {}
    for line in (d / "rows.jsonl").read_text().splitlines():
        if not line.strip():
            continue
        r = json.loads(line)
        v, _ = extract_verdict(r.get("raw") or "")
        if v and v.get("verdict"):
            out[r["id"]] = v["verdict"]
    return out, meta


def mcnemar_exact(b: int, c: int) -> float:
    """Two-sided exact binomial on the discordant pairs."""
    n = b + c
    if n == 0:
        return float("nan")
    k = min(b, c)
    return min(1.0, sum(comb(n, j) for j in range(k + 1)) * 2 / 2 ** n)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("run_a")
    ap.add_argument("run_b")
    ap.add_argument("--bank", default=str(HERE / "cases.jsonl.gz"))
    ap.add_argument("--tier", default="A", help="'A' or 'any'")
    ap.add_argument("--json", default=None)
    a = ap.parse_args()

    A, ma = load_run(Path(a.run_a))
    B, mb = load_run(Path(a.run_b))
    bank = {}
    with gzip.open(a.bank, "rt") as fh:
        for line in fh:
            e = json.loads(line)
            bank[e["id"]] = e

    if ma.get("charter_sha256") != mb.get("charter_sha256"):
        print(f"WARNING: different charters ({ma.get('charter_sha256','?')[:8]} vs "
              f"{mb.get('charter_sha256','?')[:8]}) — engine and charter are "
              f"confounded; this is not a clean engine comparison.", file=sys.stderr)

    ids = [i for i in sorted(set(A) & set(B)) if i in bank
           and bank[i].get("scope") != "situated"
           and (a.tier == "any" or bank[i]["tier"] == a.tier)]
    if len(ids) < MIN_PAIRED:
        print(f"COULD-NOT-JUDGE: only {len(ids)} shared episodes (need "
              f"{MIN_PAIRED}). Reported, not defaulted (ARCH §18.3).",
              file=sys.stderr)
        sys.exit(3)

    gold = {i: bank[i]["expect"]["verdict"] for i in ids}
    both = onlyA = onlyB = neither = 0
    for i in ids:
        x, y = A[i] == gold[i], B[i] == gold[i]
        both += x and y
        onlyA += x and not y
        onlyB += y and not x
        neither += not x and not y
    accA, accB = (both + onlyA) / len(ids), (both + onlyB) / len(ids)
    kA = cohens_kappa([(A[i], gold[i]) for i in ids])
    kB = cohens_kappa([(B[i], gold[i]) for i in ids])
    loA, hiA = kappa_ci([(A[i], gold[i]) for i in ids])
    loB, hiB = kappa_ci([(B[i], gold[i]) for i in ids])
    kAB = cohens_kappa([(A[i], B[i]) for i in ids])
    p = mcnemar_exact(onlyA, onlyB)

    nameA = ma.get("model", Path(a.run_a).name)
    nameB = mb.get("model", Path(a.run_b).name)
    print(f"PAIRED on {len(ids)} shared tier-{a.tier} episodes "
          f"(charter {ma.get('charter_sha256','?')[:8]})\n")
    print(f"  {'rater':34} {'raw':>7} {'kappa':>7}  {'95% CI':>16}  band")
    for nm, acc, k, lo, hi in ((nameA, accA, kA[2], loA, hiA),
                               (nameB, accB, kB[2], loB, hiB)):
        print(f"  {nm[:34]:34} {acc:>6.1%} {k:>7.3f}  [{lo:>5.3f},{hi:>5.3f}]  "
              f"{kappa_band(k)}")
    print(f"\n  paired delta (A-B): raw {accA-accB:+.1%}   kappa {kA[2]-kB[2]:+.3f}")
    print(f"  both right {both} · A-only {onlyA} · B-only {onlyB} · both wrong {neither}")
    print(f"  discordant {onlyA+onlyB} -> exact McNemar p = {p:.3f}")
    if onlyA + onlyB < MIN_DISCORDANT:
        print(f"  UNDERPOWERED: {onlyA+onlyB} discordant pairs (< {MIN_DISCORDANT}). "
              f"A non-significant p here means 'not enough data', NOT 'no difference'.")
    print(f"\n  A vs B agreement with EACH OTHER: kappa {kAB[2]:.3f} — if this is "
          f"near\n  their agreement with gold, they are different instruments, "
          f"not one\n  instrument at two quality levels.")

    if a.json:
        Path(a.json).write_text(json.dumps({
            "n": len(ids), "a": nameA, "b": nameB,
            "kappa_a": kA[2], "kappa_b": kB[2], "kappa_ab": kAB[2],
            "raw_a": accA, "raw_b": accB,
            "only_a": onlyA, "only_b": onlyB, "mcnemar_p": p,
            "underpowered": onlyA + onlyB < MIN_DISCORDANT}, indent=1))


if __name__ == "__main__":
    main()
