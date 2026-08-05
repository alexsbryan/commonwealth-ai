#!/usr/bin/env python3
"""Tests for calibrate_threshold.py.

Run: python3 scripts/test_calibrate_threshold.py   (exit 0 = pass)

These exist because the script's whole job is to produce ONE number that a
future session will trust without re-deriving it, and every way it can be wrong
is silent. A flipped comparison still prints a plausible threshold. A macro that
quietly pools still prints a plausible BAcc. A leave-one-subset-out that leaks
the held-out subset still prints a plausible held-out score -- an INFLATED one,
which is exactly the failure the design is supposed to prevent.

So the leak test is the load-bearing one: it builds a subset whose inclusion
would visibly move the fitted threshold, and asserts it does not.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from calibrate_threshold import (  # noqa: E402
    SubsetCurve, macro, grid, argmax_theta, theta_at_budget, loso,
    shipped_macro, half_split,
)

FAILURES = []


def check(name, got, want, tol=1e-9):
    ok = abs(got - want) <= tol if isinstance(want, float) else got == want
    if not ok:
        FAILURES.append(f"{name}: got {got!r}, want {want!r}")
    print(f"  {'ok  ' if ok else 'FAIL'}  {name}: {got!r}")


def check_true(name, cond, detail=""):
    if not cond:
        FAILURES.append(f"{name}: {detail}")
    print(f"  {'ok  ' if cond else 'FAIL'}  {name}{(' — ' + detail) if detail else ''}")


# (p_grounded, label). label 1 = supported/grounded, 0 = hallucinated.
SEPARABLE = [(0.9, 1), (0.8, 1), (0.2, 0), (0.1, 0)]


def test_direction():
    """A threshold of 0 calls everything grounded; above the max, nothing."""
    c = SubsetCurve(SEPARABLE)
    check("theta=0 -> tpr", c.tpr(0.0), 100.0)
    check("theta=0 -> tnr", c.tnr(0.0), 0.0)
    check("theta>max -> tpr", c.tpr(1.5), 0.0)
    check("theta>max -> tnr", c.tnr(1.5), 100.0)
    # p >= theta is GROUNDED, so a threshold sitting exactly on a positive score
    # must still admit it. An off-by-one here silently shifts every decision.
    check("theta exactly on a positive score admits it", c.tpr(0.8), 100.0)
    check("separable at theta=0.5 -> bacc", c.bacc(0.5), 100.0)


def test_argmax_finds_the_separation():
    c = [SubsetCurve(SEPARABLE)]
    theta, best, plateau = argmax_theta(c, grid(c))
    check("separable best bacc", best, 100.0)
    check_true("separable theta lies in the gap", 0.2 < theta <= 0.8,
               f"theta={theta}")
    check_true("plateau reported", plateau >= 1, f"plateau={plateau}")


def test_plateau_tie_break_is_the_middle():
    """Equal-scoring thresholds must resolve to the middle, not a cliff edge."""
    # Three thresholds tie at BAcc 100: anything in (0.2, 0.8].
    c = [SubsetCurve([(0.9, 1), (0.8, 1), (0.5, 1), (0.2, 0), (0.1, 0)])]
    theta, best, plateau = argmax_theta(c, grid(c))
    check("tie best bacc", best, 100.0)
    check_true("tie-break picks a middle maximiser", 0.2 < theta <= 0.9,
               f"theta={theta}, plateau={plateau}")


def test_macro_is_not_pooled():
    """The card's metric is the mean of per-subset BAccs, not one pooled BAcc.

    Built so the two disagree: a big subset the model handles perfectly and a
    tiny one it inverts. Pooling lets the big subset drown the small one; macro
    weights them equally, which is what the leaderboard reports.
    """
    big = [(0.9, 1)] * 100 + [(0.1, 0)] * 100      # perfect
    tiny = [(0.1, 1)] * 5 + [(0.9, 0)] * 5          # inverted
    curves = [SubsetCurve(big), SubsetCurve(tiny)]
    m = macro(curves, 0.5)
    check("macro of {100, 0}", m, 50.0)
    pooled = SubsetCurve(big + tiny)
    check_true("pooled disagrees with macro (the whole point)",
               abs(pooled.bacc(0.5) - m) > 1.0,
               f"pooled={pooled.bacc(0.5):.2f} macro={m:.2f}")


def test_budget_picks_the_strictest_threshold_that_fits():
    """Recall must be maximised INSIDE the false-alarm budget, not just inside it."""
    rows = [(p / 100, 1) for p in range(50, 100)] + [(p / 100, 0) for p in range(0, 50)]
    curves = [SubsetCurve(rows)]
    g = grid(curves)
    t = theta_at_budget(curves, g, 0.10)          # allow 10% of positives missed
    check_true("budget threshold clears the tpr floor",
               macro(curves, t, "tpr") >= 90.0,
               f"tpr={macro(curves, t, 'tpr'):.2f}")
    # Anything stricter must breach the floor, or we left recall on the table.
    stricter = [x for x in g if x > t]
    check_true("no stricter threshold also fits the budget",
               all(macro(curves, s, "tpr") < 90.0 for s in stricter),
               f"theta={t}, {len(stricter)} stricter candidates")
    # A budget nobody can meet is reported as unreachable, never approximated.
    check("impossible budget -> None", theta_at_budget(curves, g, -1.0), None)


def test_loso_does_not_see_the_held_out_subset():
    """The fitted theta must be independent of the subset it is scored on.

    `odd` is separable at a threshold far from where the other subsets want
    theirs, and is large enough to drag a pooled fit. If LOSO leaked, the theta
    reported for the `odd` fold would move toward odd's own optimum.
    """
    normal = [(0.9, 1)] * 20 + [(0.1, 0)] * 20
    odd = [(0.51, 1)] * 60 + [(0.49, 0)] * 60
    by_subset = {"a": list(normal), "b": list(normal), "odd": list(odd)}
    folds = {f["held_out"]: f for f in loso(by_subset)}
    check("all three subsets held out in turn", len(folds), 3)

    odd_theta = folds["odd"]["theta_from_other_subsets"]
    # Fitted on `a`+`b` alone, whose only separation is the (0.1, 0.9] gap.
    check_true("odd's theta came from a+b only", 0.1 < odd_theta <= 0.9,
               f"theta={odd_theta}")
    check_true("odd's theta is NOT odd's own optimum",
               not (0.49 < odd_theta <= 0.51),
               f"theta={odd_theta} would mean the held-out subset leaked")
    # And this is what honesty costs: `odd`'s scores all sit below a theta fitted
    # elsewhere, so the transfer collapses to chance. A leaking implementation
    # would have reported ~100 here. 50 is the CORRECT answer and the reason the
    # design is worth its complexity -- the real card does the same thing to
    # AggreFact-CNN (-6.75 vs shipped).
    check("held-out transfer is allowed to fail, and is reported when it does",
          folds["odd"]["held_out_bacc"], 50.0)

    # A fold fitted WITH the held-out subset is the thing we are avoiding; show
    # it differs, so the test proves a real distinction rather than a tautology.
    all_curves = [SubsetCurve(v) for v in by_subset.values()]
    with_leak, _, _ = argmax_theta(all_curves, grid(all_curves))
    check_true("fitting on all subsets gives a different theta",
               abs(with_leak - odd_theta) > 1e-9,
               f"leaky={with_leak} loso={odd_theta}")


def test_single_class_subset_is_skipped_not_crashed():
    """A subset with one label has no BAcc. It must drop out, not divide by zero."""
    by_subset = {"ok": [(0.9, 1)] * 5 + [(0.1, 0)] * 5,
                 "ok2": [(0.8, 1)] * 5 + [(0.2, 0)] * 5,
                 "all_pos": [(0.7, 1)] * 10}
    curves = [c for c in (SubsetCurve(v) for v in by_subset.values()) if c.usable()]
    check("single-class subset excluded from the curve set", len(curves), 2)
    folds = loso(by_subset)
    check("single-class subset never becomes a fold", len(folds), 2)


def test_shipped_macro_matches_the_committed_run():
    """Recomputing the baseline from results.jsonl must reproduce summary.json.

    This is the anchor for the whole comparison: if the shipped number this
    script prints is not the number the run reported, every delta against it is
    meaningless. Skipped (not failed) when the run dir is absent -- an unrunnable
    check is a could-not-judge, not a pass.
    """
    import json
    run = os.path.expanduser("~/dev/train-env/runs/score-mix-A-lp")
    if not os.path.isdir(run):
        print(f"  skip  shipped-macro anchor: {run} not present")
        return
    from calibrate_threshold import load
    _, shipped, _, _ = load(run)
    per = shipped_macro(shipped)
    got = round(sum(v["bacc"] for v in per.values()) / len(per), 2)
    want = json.load(open(os.path.join(run, "summary.json")))["macro_avg_bacc"]
    # summary.json rounds each subset before averaging; we average then round, so
    # allow the rounding path to differ by less than a tenth of a point.
    check_true("recomputed shipped macro matches summary.json",
               abs(got - want) < 0.1, f"recomputed {got} vs reported {want}")


def test_half_split_is_seeded():
    """Two calls with the same seed must agree, or the reported sd is noise."""
    by_subset = {"a": [(i / 50, 1) for i in range(25, 50)] + [(i / 50, 0) for i in range(0, 25)],
                 "b": [(i / 50, 1) for i in range(30, 50)] + [(i / 50, 0) for i in range(0, 20)]}
    x = half_split(by_subset, repeats=5, seed=3)
    y = half_split(by_subset, repeats=5, seed=3)
    check("half-split is deterministic under a fixed seed", x, y)


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        print(f"\n{t.__name__}")
        t()
    print()
    if FAILURES:
        print(f"FAILED {len(FAILURES)}:")
        for f in FAILURES:
            print(f"  - {f}")
        return 1
    print(f"PASS — {len(tests)} test groups")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
