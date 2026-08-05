#!/usr/bin/env python3
"""Decide the verdict threshold on `p_grounded`, and report what it HONESTLY buys.

WHY THIS EXISTS. The shipped verifier decides by reading the token the model
emitted after `<classification>`. That is one point on an operating curve, and
it is not a point anyone chose -- it is wherever ORPO happened to leave the
model's argmax. `operating_curve.py` showed arm A's curve reaches a pooled BAcc
of 71.8 against a shipped 64.95, so several points are sitting there unclaimed.

This script exists because CLAIMING them requires more care than reading the
maximum off the curve. Three things separate a decided threshold from an
overfit one, and this script does all three:

  1. THE METRIC IS MACRO, NOT POOLED. The card's headline is the mean of 11
     per-subset BAccs. Pooling all 2,186 items answers a different question and
     the two numbers differ by whole points, because subset sizes and base
     rates differ. `operating_curve.py` pools; this does not.

  2. THE THRESHOLD MUST BE HELD OUT. A threshold chosen on the same items it is
     scored on has seen its own test set. The primary design here is
     LEAVE-ONE-SUBSET-OUT: fit theta on 10 subsets, score the 11th, rotate.
     That tests the thing deployment actually asks -- does a threshold tuned on
     known domains transfer to an unseen one? A repeated stratified half-split
     runs alongside it to separate item-level noise from domain transfer.
     ARCH_PRINCIPLES §18.4 (validate the instrument), §18.5 (one run is not a
     measurement).

  3. THE PRODUCT METRIC IS NOT BACC. Operator directive 2026-08-04:
     hallucination detection is the point. BAcc is the leaderboard number; the
     product number is hallucination recall at a bounded false-alarm rate. A
     threshold that wins BAcc by flagging 60% of supported claims is not
     shippable. Both are reported, at their own operating points.

The output is a DECIDED VALUE written to findings/, not a curve to admire.

  calibrate_threshold.py <run-dir> [--transfer <other-run-dir>] [--emit <json>]
"""
import argparse
import bisect
import json
import os
import random
import statistics
import sys
from collections import defaultdict

# False-alarm budgets to report the product metric at: the fraction of SUPPORTED
# claims the gate is allowed to wrongly flag. 10% is the loosest a user-facing
# gate can be before "it flags everything" becomes the operator's experience.
FALSE_ALARM_BUDGETS = (0.05, 0.10, 0.20, 0.30)

HALF_SPLIT_REPEATS = 200
HALF_SPLIT_SEED = 17


# --------------------------------------------------------------------------
# loading
# --------------------------------------------------------------------------

def load(run_dir):
    """{subset: [(p_grounded, label)]} plus the shipped token-decision rows.

    `shipped` carries (subset, label, pred) for EVERY scored row, including the
    ones with no p_grounded, so the baseline we compare against is the run's own
    reported number and not a subset of it.
    """
    by_subset = defaultdict(list)
    shipped = []
    dropped_no_p = dropped_no_label = 0
    path = os.path.join(run_dir, "results.jsonl")
    with open(path) as f:
        for line in f:
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if r.get("label") is None:
                dropped_no_label += 1
                continue
            if r.get("pred") is not None:
                shipped.append((r.get("subset", "?"), r["label"], r["pred"]))
            if r.get("p_grounded") is None:
                dropped_no_p += 1
                continue
            by_subset[r.get("subset", "?")].append((r["p_grounded"], r["label"]))
    return dict(by_subset), shipped, dropped_no_p, dropped_no_label


def shipped_macro(shipped):
    """Macro BAcc of the run as it was actually scored, from the emitted token."""
    per = defaultdict(lambda: [0, 0, 0, 0])  # tp, fn, tn, fp
    for subset, label, pred in shipped:
        c = per[subset]
        if label == 1:
            c[0 if pred == 1 else 1] += 1
        else:
            c[2 if pred == 0 else 3] += 1
    out = {}
    for subset, (tp, fn, tn, fp) in per.items():
        tpr = 100.0 * tp / (tp + fn) if tp + fn else 0.0
        tnr = 100.0 * tn / (tn + fp) if tn + fp else 0.0
        out[subset] = {"bacc": (tpr + tnr) / 2, "tpr": tpr, "tnr": tnr,
                       "n": tp + fn + tn + fp}
    return out


# --------------------------------------------------------------------------
# the curve, computed per subset so macro is cheap at any threshold
# --------------------------------------------------------------------------

class SubsetCurve:
    """Sorted score arrays for one subset. Predict GROUNDED when p >= theta.

    Sorting once turns every (threshold -> tpr/tnr) query into two binary
    searches, which is what makes 1,800 thresholds x 11 folds x 200 repeats
    finish in seconds rather than minutes.
    """

    def __init__(self, rows):
        self.pos = sorted(p for p, y in rows if y == 1)
        self.neg = sorted(p for p, y in rows if y == 0)
        self.n = len(self.pos) + len(self.neg)

    def usable(self):
        return bool(self.pos) and bool(self.neg)

    def tpr(self, theta):
        if not self.pos:
            return None
        return 100.0 * (len(self.pos) - bisect.bisect_left(self.pos, theta)) / len(self.pos)

    def tnr(self, theta):
        if not self.neg:
            return None
        return 100.0 * bisect.bisect_left(self.neg, theta) / len(self.neg)

    def bacc(self, theta):
        return (self.tpr(theta) + self.tnr(theta)) / 2


def macro(curves, theta, key="bacc"):
    """Mean over subsets of the per-subset metric at one GLOBAL threshold."""
    vals = [getattr(c, key)(theta) for c in curves if c.usable()]
    return sum(vals) / len(vals) if vals else 0.0


def grid(curves):
    """Candidate thresholds: every distinct score, plus one above the maximum.

    Predicting on `p >= theta` means the achievable operating points change only
    at observed scores, so this grid is exhaustive rather than a sampling of it.
    """
    vals = set()
    for c in curves:
        vals.update(c.pos)
        vals.update(c.neg)
    if not vals:
        return [0.0]
    return sorted(vals) + [max(vals) * 1.0000001 + 1e-12]


def argmax_theta(curves, thetas):
    """Threshold maximising macro BAcc; ties broken to the MIDDLE of the plateau.

    Taking the first maximiser puts the decided value on a cliff edge, where the
    next item to arrive can move it. The median maximiser is the most robust
    point of an equally-scoring set, and the plateau width is reported so a
    knife-edge optimum is visible rather than implied.
    """
    best = None
    winners = []
    for t in thetas:
        m = macro(curves, t)
        if best is None or m > best + 1e-12:
            best, winners = m, [t]
        elif abs(m - best) <= 1e-12:
            winners.append(t)
    return statistics.median(winners), best, len(winners)


def theta_at_budget(curves, thetas, budget):
    """Strictest theta whose macro tpr still meets the false-alarm budget.

    macro tpr falls monotonically as theta rises, so the LAST theta clearing the
    bar is the one with the best hallucination recall inside the budget.
    """
    floor = 100.0 * (1.0 - budget)
    ok = [t for t in thetas if macro(curves, t, "tpr") >= floor]
    if not ok:
        return None
    return max(ok)


# --------------------------------------------------------------------------
# held-out designs
# --------------------------------------------------------------------------

def loso(by_subset):
    """Leave-one-subset-out. Fit theta on 10 subsets, score the held-out 11th.

    This is the primary design because it mirrors deployment: the threshold will
    meet documents from domains it was never calibrated on.
    """
    names = sorted(by_subset)
    folds = []
    for held in names:
        fit_curves = [SubsetCurve(by_subset[s]) for s in names if s != held]
        fit_curves = [c for c in fit_curves if c.usable()]
        test = SubsetCurve(by_subset[held])
        if not test.usable() or not fit_curves:
            continue
        theta, fit_bacc, _ = argmax_theta(fit_curves, grid(fit_curves))
        folds.append({
            "held_out": held,
            "theta_from_other_subsets": theta,
            "fit_macro_bacc": round(fit_bacc, 2),
            "held_out_bacc": round(test.bacc(theta), 2),
            "held_out_tpr": round(test.tpr(theta), 2),
            "held_out_tnr": round(test.tnr(theta), 2),
            "n": test.n,
        })
    return folds


def half_split(by_subset, repeats=HALF_SPLIT_REPEATS, seed=HALF_SPLIT_SEED):
    """Stratified half-split within every subset, repeated.

    Separates item-level overfitting from the domain transfer LOSO measures: the
    same 11 domains appear on both sides, so whatever gap survives here is the
    threshold memorising individual items.
    """
    rng = random.Random(seed)
    scores = []
    for _ in range(repeats):
        fit, test = defaultdict(list), defaultdict(list)
        for s, rows in by_subset.items():
            pos = [r for r in rows if r[1] == 1]
            neg = [r for r in rows if r[1] == 0]
            for pool in (pos, neg):
                idx = list(range(len(pool)))
                rng.shuffle(idx)
                cut = len(idx) // 2
                fit[s].extend(pool[i] for i in idx[:cut])
                test[s].extend(pool[i] for i in idx[cut:])
        fit_curves = [c for c in (SubsetCurve(v) for v in fit.values()) if c.usable()]
        test_curves = [c for c in (SubsetCurve(v) for v in test.values()) if c.usable()]
        if not fit_curves or not test_curves:
            continue
        theta, _, _ = argmax_theta(fit_curves, grid(fit_curves))
        scores.append(macro(test_curves, theta))
    return scores


# --------------------------------------------------------------------------
# report
# --------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run_dir")
    ap.add_argument("--transfer", action="append", default=[],
                    help="another scored run dir to apply the DECIDED theta to, "
                         "unchanged. Cross-dataset transfer is the only check "
                         "that no part of the calibration saw.")
    ap.add_argument("--emit", help="write the decided value + evidence as JSON")
    ap.add_argument("--repeats", type=int, default=HALF_SPLIT_REPEATS)
    args = ap.parse_args()

    by_subset, shipped, no_p, no_label = load(args.run_dir)
    if not by_subset:
        print(f"NO USABLE ROWS in {args.run_dir} ({no_p} without p_grounded). "
              f"Was the run scored with --logprobs?", file=sys.stderr)
        return 2

    curves = [c for c in (SubsetCurve(v) for v in by_subset.values()) if c.usable()]
    thetas = grid(curves)
    n_items = sum(c.n for c in curves)

    print(f"{args.run_dir}")
    print(f"  {len(curves)} subsets, {n_items} items with p_grounded "
          f"(dropped {no_p} no p_grounded, {no_label} no label)")
    print(f"  {len(thetas)} candidate thresholds")

    # --- 1. what is shipped today -----------------------------------------
    ship = shipped_macro(shipped)
    ship_macro = sum(v["bacc"] for v in ship.values()) / len(ship)
    ship_tnr = sum(v["tnr"] for v in ship.values()) / len(ship)
    ship_tpr = sum(v["tpr"] for v in ship.values()) / len(ship)
    print(f"\n  SHIPPED (emitted token, no threshold): macro BAcc {ship_macro:.2f}"
          f"  (macro tpr {ship_tpr:.1f}, macro tnr {ship_tnr:.1f})")

    # --- 2. the in-sample maximum, which is NOT a result -------------------
    theta_in, bacc_in, plateau = argmax_theta(curves, thetas)
    print(f"\n  IN-SAMPLE BEST theta {theta_in:.6g} -> macro BAcc {bacc_in:.2f}"
          f"  (macro tpr {macro(curves, theta_in, 'tpr'):.1f}, "
          f"tnr {macro(curves, theta_in, 'tnr'):.1f})")
    print(f"    plateau: {plateau} thresholds tie at this maximum")
    print(f"    NOT REPORTABLE — chosen on the items it is scored on.")

    # --- 3. leave-one-subset-out: the honest number ------------------------
    folds = loso(by_subset)
    loso_mean = sum(f["held_out_bacc"] for f in folds) / len(folds)
    print(f"\n  LEAVE-ONE-SUBSET-OUT ({len(folds)} folds) — theta fit on the other subsets:")
    print(f"    {'held-out subset':<20} {'theta':>12} {'BAcc':>7} {'tpr':>7} {'tnr':>7}"
          f"  {'vs shipped':>11}")
    for f in folds:
        d = f["held_out_bacc"] - ship.get(f["held_out"], {}).get("bacc", 0.0)
        print(f"    {f['held_out']:<20} {f['theta_from_other_subsets']:>12.6g} "
              f"{f['held_out_bacc']:>7.2f} {f['held_out_tpr']:>7.1f} "
              f"{f['held_out_tnr']:>7.1f}  {d:>+11.2f}")
    print(f"    MEAN HELD-OUT macro BAcc {loso_mean:.2f}   "
          f"({loso_mean - ship_macro:+.2f} vs shipped {ship_macro:.2f})")
    print(f"    optimism of the in-sample number: {bacc_in - loso_mean:+.2f}")

    thetas_fit = [f["theta_from_other_subsets"] for f in folds]
    print(f"    theta across folds: min {min(thetas_fit):.6g} "
          f"max {max(thetas_fit):.6g} median {statistics.median(thetas_fit):.6g}")

    # --- 4. half-split: item-level noise ----------------------------------
    hs = half_split(by_subset, repeats=args.repeats)
    if hs:
        print(f"\n  STRATIFIED HALF-SPLIT ({len(hs)} repeats, same 11 domains both sides):")
        print(f"    held-out macro BAcc {statistics.mean(hs):.2f} "
              f"+/- {statistics.pstdev(hs):.2f} sd   "
              f"[{min(hs):.2f}, {max(hs):.2f}]")

    # --- 5. the product metric --------------------------------------------
    print(f"\n  PRODUCT OPERATING POINTS — hallucination recall at a bounded")
    print(f"  false-alarm rate (operator directive: detection is the point):")
    print(f"    {'budget':>8} {'theta':>12} {'macro tnr':>10} {'macro tpr':>10} {'BAcc':>7}"
          f"  {'held-out tnr':>13}")
    budget_rows = []
    for b in FALSE_ALARM_BUDGETS:
        t = theta_at_budget(curves, thetas, b)
        if t is None:
            print(f"    {b:>8.0%}  unreachable — macro tpr never clears {100*(1-b):.0f}")
            continue
        # Same LOSO discipline: fit the budget threshold without the held-out
        # subset, then read that subset's recall.
        names = sorted(by_subset)
        ho = []
        for held in names:
            fc = [c for c in (SubsetCurve(by_subset[s]) for s in names if s != held)
                  if c.usable()]
            test = SubsetCurve(by_subset[held])
            if not test.usable() or not fc:
                continue
            tb = theta_at_budget(fc, grid(fc), b)
            if tb is not None:
                ho.append(test.tnr(tb))
        ho_tnr = sum(ho) / len(ho) if ho else float("nan")
        row = {"false_alarm_budget": b, "theta": t,
               "macro_tnr_hallucinated": round(macro(curves, t, "tnr"), 2),
               "macro_tpr_supported": round(macro(curves, t, "tpr"), 2),
               "macro_bacc": round(macro(curves, t), 2),
               "held_out_macro_tnr": round(ho_tnr, 2)}
        budget_rows.append(row)
        print(f"    {b:>8.0%} {t:>12.6g} {row['macro_tnr_hallucinated']:>10.2f} "
              f"{row['macro_tpr_supported']:>10.2f} {row['macro_bacc']:>7.2f}"
              f"  {row['held_out_macro_tnr']:>13.2f}")

    # --- 6. the decided value ---------------------------------------------
    # Decided as the MEDIAN of the leave-one-subset-out thetas. Every fold's
    # theta is a legitimate estimate fitted without one domain; the median is
    # the one least moved by any single domain, and unlike the in-sample argmax
    # it was never fitted on all 11 at once.
    decided = statistics.median(thetas_fit)
    print(f"\n  DECIDED THRESHOLD  p_grounded >= {decided:.6g}")
    print(f"    (median of the {len(folds)} leave-one-subset-out thetas)")
    print(f"    on all 11 subsets: macro BAcc {macro(curves, decided):.2f}, "
          f"tpr {macro(curves, decided, 'tpr'):.1f}, tnr {macro(curves, decided, 'tnr'):.1f}")
    print(f"    HONEST EXPECTED GAIN: {loso_mean - ship_macro:+.2f} macro BAcc "
          f"({ship_macro:.2f} -> {loso_mean:.2f})")

    # --- 7. transfer to a dataset no fold ever saw -------------------------
    transfers = []
    for other in args.transfer:
        ob, oship, _, _ = load(other)
        oc = [c for c in (SubsetCurve(v) for v in ob.values()) if c.usable()]
        if not oc:
            print(f"\n  TRANSFER {other}: no p_grounded rows — skipped")
            continue
        os_ = shipped_macro(oship)
        os_macro = sum(v["bacc"] for v in os_.values()) / len(os_)
        t = {"run": other,
             "shipped_macro_bacc": round(os_macro, 2),
             "at_decided_theta": {
                 "macro_bacc": round(macro(oc, decided), 2),
                 "macro_tpr": round(macro(oc, decided, "tpr"), 2),
                 "macro_tnr": round(macro(oc, decided, "tnr"), 2)},
             "its_own_in_sample_best": round(argmax_theta(oc, grid(oc))[1], 2)}
        transfers.append(t)
        print(f"\n  TRANSFER -> {other}  (nothing here was used to fit theta)")
        print(f"    shipped macro BAcc {t['shipped_macro_bacc']:.2f}")
        print(f"    at decided theta   {t['at_decided_theta']['macro_bacc']:.2f} "
              f"(tpr {t['at_decided_theta']['macro_tpr']:.1f}, "
              f"tnr {t['at_decided_theta']['macro_tnr']:.1f})")
        print(f"    its own in-sample ceiling {t['its_own_in_sample_best']:.2f} "
              f"— the most any threshold could reach here")

    if args.emit:
        payload = {
            "run_dir": args.run_dir,
            "n_items": n_items,
            "n_subsets": len(curves),
            "shipped": {"macro_bacc": round(ship_macro, 2),
                        "macro_tpr": round(ship_tpr, 2),
                        "macro_tnr": round(ship_tnr, 2),
                        "per_subset": {k: {m: round(x, 2) for m, x in v.items()}
                                       for k, v in ship.items()}},
            "in_sample_best": {"theta": theta_in, "macro_bacc": round(bacc_in, 2),
                               "plateau_width": plateau,
                               "reportable": False},
            "loso": {"folds": folds,
                     "mean_held_out_macro_bacc": round(loso_mean, 2),
                     "optimism_of_in_sample": round(bacc_in - loso_mean, 2)},
            "half_split": ({"repeats": len(hs), "mean": round(statistics.mean(hs), 2),
                            "sd": round(statistics.pstdev(hs), 2)} if hs else None),
            "product_operating_points": budget_rows,
            "decided": {
                "threshold": decided,
                "rule": "predict GROUNDED when p_grounded >= threshold",
                "chosen_by": "median of leave-one-subset-out thetas",
                "expected_macro_bacc": round(loso_mean, 2),
                "gain_vs_shipped": round(loso_mean - ship_macro, 2)},
            "transfer": transfers,
        }
        os.makedirs(os.path.dirname(os.path.abspath(args.emit)), exist_ok=True)
        with open(args.emit, "w") as f:
            json.dump(payload, f, indent=2)
        print(f"\n  wrote {args.emit}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
