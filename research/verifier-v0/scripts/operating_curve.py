#!/usr/bin/env python3
"""Sweep p_grounded into a tpr/tnr operating curve, and locate another arm on it.

WHY THIS EXISTS. Arms A and AB were compared at one operating point each --
A at (tpr 39.8, tnr 91.0), AB at (85.6, 56.8) -- and macro BAcc, being the mean
of those two columns, said AB won by 4.91. But over the 2,186 items both arms
scored, `A==1 AND B==1` reproduced arm A EXACTLY and `A==1 OR B==1` reproduced
arm AB EXACTLY, so A's GROUNDED set is a strict SUBSET of AB's. Nested
decisions are the signature of one classifier at two thresholds, and two
isolated points cannot distinguish "AB discriminates better" from "AB sits at a
friendlier threshold". That distinction decides whether Stream B earns a place
in the M3 run or whether the same gain is available for free by recalibrating
arm A.

THE TEST. Sweep arm A's own p_grounded to the threshold where its tpr matches
arm AB's 85.6, then read off arm A's tnr there:

  - arm A's tnr lands at ~56.8  -> AB is ON arm A's curve. Stream B bought a
    threshold move, not discrimination. Recalibrate instead of retraining.
  - arm A's tnr lands well BELOW 56.8 -> AB is ABOVE arm A's curve. Stream B
    genuinely improved the model and earns its place.

Also reports AUC, which summarises the whole curve without picking a point,
and the best-BAcc threshold, which is the operating point a BAcc-chasing
process would have selected.

  operating_curve.py <run-dir> [--locate tpr=<v>,tnr=<v> ...]
"""
import argparse
import json
import os
import sys


def load(run_dir):
    """(p_grounded, label) pairs, plus what had to be dropped and why."""
    rows, no_p, no_label = [], 0, 0
    path = os.path.join(run_dir, "results.jsonl")
    with open(path) as f:
        for line in f:
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if r.get("label") is None:
                no_label += 1
                continue
            if r.get("p_grounded") is None:
                no_p += 1
                continue
            rows.append((r["p_grounded"], r["label"]))
    return rows, no_p, no_label


def curve(rows):
    """Every distinct operating point, as (threshold, tpr, tnr, bacc).

    Predict GROUNDED when p_grounded >= threshold, so a LOW threshold is
    permissive (high tpr, low tnr) and a HIGH threshold is strict.
    """
    pos = sum(1 for _, y in rows if y == 1)
    neg = len(rows) - pos
    if not pos or not neg:
        return []
    pts = []
    for thr in sorted({p for p, _ in rows} | {0.0, 1.0000001}):
        tp = sum(1 for p, y in rows if y == 1 and p >= thr)
        tn = sum(1 for p, y in rows if y == 0 and p < thr)
        tpr, tnr = 100.0 * tp / pos, 100.0 * tn / neg
        pts.append((thr, tpr, tnr, (tpr + tnr) / 2))
    return pts


def auc(rows):
    """Rank-based AUC (Mann-Whitney), ties counted at half weight."""
    pos = [p for p, y in rows if y == 1]
    neg = [p for p, y in rows if y == 0]
    if not pos or not neg:
        return None
    ranked = sorted(rows, key=lambda r: r[0])
    # Average ranks over ties so a model that emits few distinct values is not
    # penalised or flattered by the tie-breaking order.
    ranks, i = {}, 0
    while i < len(ranked):
        j = i
        while j + 1 < len(ranked) and ranked[j + 1][0] == ranked[i][0]:
            j += 1
        avg = (i + j) / 2 + 1
        for k in range(i, j + 1):
            ranks.setdefault(ranked[k][0], avg)
        i = j + 1
    s = sum(ranks[p] for p in pos)
    return (s - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg))


def at_tpr(pts, target):
    """The curve point whose tpr is closest to `target`."""
    return min(pts, key=lambda q: abs(q[1] - target))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run_dir")
    ap.add_argument("--locate", action="append", default=[],
                    help="NAME:tpr=<v>,tnr=<v> — another arm's operating point "
                         "to locate on THIS arm's curve. Reports the tnr this "
                         "arm achieves at that arm's tpr; a gap means this "
                         "curve sits below that point.")
    args = ap.parse_args()

    rows, no_p, no_label = load(args.run_dir)
    if not rows:
        print(f"NO USABLE ROWS in {args.run_dir} "
              f"({no_p} without p_grounded, {no_label} without a label). "
              f"Was the run scored with --logprobs?", file=sys.stderr)
        return 2

    pos = sum(1 for _, y in rows if y == 1)
    print(f"{args.run_dir}")
    print(f"  scored {len(rows)} items ({pos} grounded / {len(rows)-pos} hallucinated)"
          f"  dropped: {no_p} no p_grounded, {no_label} no label")
    print(f"  distinct p_grounded values: {len({p for p, _ in rows})}")

    a = auc(rows)
    print(f"  AUC {a:.4f}" if a is not None else "  AUC n/a")

    pts = curve(rows)
    best = max(pts, key=lambda q: q[3])
    print(f"  best BAcc {best[3]:.2f} at threshold {best[0]:.6g} "
          f"(tpr {best[1]:.1f}, tnr {best[2]:.1f})")

    print("\n  operating curve (tpr -> tnr this arm achieves):")
    for want in (10, 20, 30, 40, 50, 60, 70, 80, 85, 90, 95, 99):
        thr, tpr, tnr, b = at_tpr(pts, want)
        print(f"    tpr {tpr:6.1f}   tnr {tnr:6.1f}   bacc {b:6.2f}   thr {thr:.6g}")

    for spec in args.locate:
        name, _, kv = spec.partition(":")
        d = dict(p.split("=") for p in kv.split(","))
        t_tpr, t_tnr = float(d["tpr"]), float(d["tnr"])
        thr, tpr, tnr, b = at_tpr(pts, t_tpr)
        gap = tnr - t_tnr
        print(f"\n  LOCATING {name} (tpr {t_tpr}, tnr {t_tnr}) on this curve:")
        print(f"    at tpr {tpr:.1f} this arm reaches tnr {tnr:.1f} "
              f"(bacc {b:.2f}, thr {thr:.6g})")
        print(f"    gap {gap:+.1f} tnr points — ", end="")
        if abs(gap) <= 2.0:
            print(f"{name} sits ON this curve. Same discrimination, different "
                  f"threshold: the difference is recalibration, not learning.")
        elif gap < 0:
            print(f"{name} is ABOVE this curve. It genuinely discriminates "
                  f"better at that operating point.")
        else:
            print(f"{name} is BELOW this curve. This arm dominates it.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
