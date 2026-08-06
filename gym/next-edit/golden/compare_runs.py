#!/usr/bin/env python3
"""Diff two `score_golden.py --json` runs, case by case.

Spec: `sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §4.

THE MEASUREMENT THIS EXISTS FOR. Every model number this project owns was
taken through the consult gate, and the gate admits ~9% of real editing
episodes (`gym/next-edit/golden/README.md`). So a model scored through it
has been judged on a sliver our routing chose — not on what it can do.
Running the same bank with `next_edit_score --force-consult` and diffing
against the gated run separates two things that a single run confounds:

    a gate that protects us from a bad model
    a gate that hides a good one

READING THE DELTA (`sovereign/docs/NEXT_EDIT.md` §9b):

    useful UP,   wrong flat  -> the gate is the bottleneck; widen it
    useful flat, wrong UP    -> the gate earns its keep; fix induction
    both flat                -> the model genuinely cannot do these, and
                                this bank is the training set

A DELTA IS NOT A RESULT UNTIL IT CLEARS THE NOISE. Two runs of the same
config on this bank have drifted 2.6 points (36.0 vs 38.6 useful, one
day apart) because the upstream samples. So this reports Wilson CIs and
says `within noise` when they overlap, rather than narrating a rank.
A single-run delta reported as a result is ARCH §18.5's named smell.

    python3 gym/next-edit/golden/compare_runs.py \
        --a rows-gated.json --a-label gated \
        --b rows-forced.json --b-label forced
"""

from __future__ import annotations

import argparse
import collections
import json

# Reuse the ruler's own interval — one implementation per formula
# (ARCH §10.6). This is the same Wilson score `score_golden.py` prints.
from score_golden import wilson

# Outcomes that mean "the system said something", positives and negatives
# alike. `partial` is a fire: it hit a real edit and offered extra sites.
FIRED = ("useful", "partial", "wrong")


def load(path: str) -> dict[str, dict]:
    rows = json.load(open(path))
    by_id = {r["id"]: r for r in rows}
    if len(by_id) != len(rows):
        raise SystemExit(f"{path}: duplicate case ids — the diff would be silently wrong")
    return by_id


def rates(rows: list[dict]) -> dict:
    """The three headline rates, computed exactly as `score_golden.py`
    does so the two surfaces cannot disagree."""
    pos = [r for r in rows if r["kind"] == "positive"]
    neg = [r for r in rows if r["kind"] == "negative"]
    tot = collections.Counter(r["outcome"] for r in pos)
    ntot = collections.Counter(r["outcome"] for r in neg)
    fires = sum(tot[o] for o in FIRED) + ntot["wrong"]
    wrong = tot["wrong"] + ntot["wrong"]
    useful = tot["useful"] + tot["partial"]
    return {
        "npos": len(pos), "nneg": len(neg),
        "useful": useful, "useful_n": len(pos),
        "wrong": wrong, "wrong_n": fires,
        "missed": tot["missed"], "missed_n": len(pos),
        "neg_wrong": ntot["wrong"],
    }


def band(k: int, n: int) -> str:
    if not n:
        return "   n/a"
    lo, hi = wilson(k, n)
    return f"{100*k/n:5.1f}% [{100*lo:.1f}-{100*hi:.1f}]"


def moved(ka: int, na: int, kb: int, nb: int) -> str:
    """Did this rate actually move, or is the delta inside the interval?
    Overlapping Wilson intervals is a deliberately conservative test —
    it under-calls significance rather than over-calling it, which is
    the right bias when the answer authorizes a fine-tune."""
    if not na or not nb:
        return "?"
    alo, ahi = wilson(ka, na)
    blo, bhi = wilson(kb, nb)
    if blo > ahi:
        return "UP"
    if bhi < alo:
        return "DOWN"
    return "within noise"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--a", required=True, help="baseline rows json")
    ap.add_argument("--b", required=True, help="comparison rows json")
    ap.add_argument("--a-label", default="A")
    ap.add_argument("--b-label", default="B")
    ap.add_argument("--shapes", action="store_true", help="per-shape transition detail")
    args = ap.parse_args()

    A, B = load(args.a), load(args.b)
    shared = sorted(set(A) & set(B))
    only_a, only_b = set(A) - set(B), set(B) - set(A)
    if only_a or only_b:
        # Never silently intersect: a case present in one run and not the
        # other is an error in one of them (ARCH §18.3).
        print(f"!! {len(only_a)} cases only in {args.a_label}, "
              f"{len(only_b)} only in {args.b_label} — diffing the {len(shared)} shared")

    ra = rates([A[i] for i in shared])
    rb = rates([B[i] for i in shared])

    print(f"\n{'':<14} {args.a_label:>22} {args.b_label:>22}   verdict")
    for name, k, n in (("useful-fire", "useful", "useful_n"),
                       ("wrong-fire", "wrong", "wrong_n"),
                       ("missed-fire", "missed", "missed_n")):
        print(f"{name:<14} {band(ra[k], ra[n]):>22} {band(rb[k], rb[n]):>22}   "
              f"{moved(ra[k], ra[n], rb[k], rb[n])}")
    print(f"{'wrong fires':<14} {ra['wrong']:>22} {rb['wrong']:>22}   "
          f"(of which on negatives: {ra['neg_wrong']} -> {rb['neg_wrong']})")

    # PAIRED CHANGE. The verdicts above come from overlapping Wilson
    # intervals, which is an UNPAIRED test: it asks whether two
    # independent samples could share a rate. These runs are not
    # independent samples — they are the same 1,098 cases through a
    # deterministic pipeline (two identical runs churn 0 cases), so the
    # only thing that carries information is the DISCORDANT pairs. A
    # change of 23-removed / 0-added reads as "within noise" on the
    # unpaired test and is in fact as one-directional as evidence gets.
    # Report both; never let the conservative one stand alone.
    def discordant(pred) -> tuple[int, int]:
        gone = sum(1 for i in shared if pred(A[i]) and not pred(B[i]))
        came = sum(1 for i in shared if not pred(A[i]) and pred(B[i]))
        return gone, came

    print("\nPAIRED CHANGE (same cases, deterministic pipeline — discordant pairs only)")
    for lbl, pred in (("wrong fires", lambda r: r["outcome"] == "wrong"),
                      ("useful fires", lambda r: r["outcome"] in ("useful", "partial"))):
        gone, came = discordant(pred)
        net = came - gone
        print(f"  {lbl:<14} removed {gone:>4}   added {came:>4}   net {net:+d}")
    print("  A one-directional change (X removed, 0 added) is decisive regardless")
    print("  of the interval overlap above.")

    # Admission — the share of episodes the model was actually asked
    # about. Read this before any rate above it: a rate is conditioned
    # on it, and it is a property of the gate, not of the model.
    print(f"\n{'ADMISSION':<24} {args.a_label:>10} {args.b_label:>10}")
    sa = collections.Counter(A[i]["model_state"] for i in shared)
    sb = collections.Counter(B[i]["model_state"] for i in shared)
    for st in sorted(set(sa) | set(sb), key=lambda s: -(sa[s] + sb[s])):
        print(f"{st:<24} {sa[st]:>10} {sb[st]:>10}")
    reach = lambda s: sum(v for k, v in s.items() if not k.startswith("skipped:"))
    print(f"{'-> reached the model':<24} {reach(sa):>10} {reach(sb):>10}")

    # The transition matrix. The headline rates can stay flat while the
    # population underneath churns — a run that fixes 40 cases and breaks
    # 40 others reports the same useful-fire as one that changed nothing,
    # and those are not the same system.
    print(f"\nTRANSITIONS  ({args.a_label} -> {args.b_label})")
    for kind in ("positive", "negative"):
        ids = [i for i in shared if A[i]["kind"] == kind]
        t = collections.Counter((A[i]["outcome"], B[i]["outcome"]) for i in ids)
        churn = sum(v for (x, y), v in t.items() if x != y)
        print(f"  {kind}s ({len(ids)}): {churn} changed outcome, "
              f"{len(ids)-churn} unchanged")
        for (x, y), k in sorted(t.items(), key=lambda kv: -kv[1]):
            if x != y:
                print(f"    {x:>8} -> {y:<8} {k:>4}")

    # The population the gate was hiding: cases the baseline never asked
    # the model about. This is the only slice where forcing can teach us
    # anything, and its useful-rate IS the answer to "is the gate hiding
    # a good model?"
    hidden = [i for i in shared
              if A[i]["model_state"].startswith("skipped:gate")
              and A[i]["kind"] == "positive"]
    if hidden:
        hb = collections.Counter(B[i]["outcome"] for i in hidden)
        won = hb["useful"] + hb["partial"]
        print(f"\nTHE HIDDEN SLICE — {len(hidden)} positives the {args.a_label} run "
              f"never asked the model about")
        print(f"  under {args.b_label}: {band(won, len(hidden))} useful  "
              f"({hb['useful']} useful, {hb['partial']} partial, "
              f"{hb['wrong']} wrong, {hb['missed']} still missed)")
        nhidden = [i for i in shared
                   if A[i]["model_state"].startswith("skipped:gate")
                   and A[i]["kind"] == "negative"]
        nb = collections.Counter(B[i]["outcome"] for i in nhidden)
        print(f"  the same widening on {len(nhidden)} negatives it also hid: "
              f"{nb['wrong']} wrong fires ({100*nb['wrong']/max(1,len(nhidden)):.1f}%)")
        print(f"  -> widening the gate buys {won} edits and costs {nb['wrong']} wrong fires")

    if args.shapes:
        print("\nPER-SHAPE useful-fire (positives)")
        shapes = sorted({A[i]["shape"] for i in shared if A[i]["kind"] == "positive"})
        print(f"  {'shape':<22} {args.a_label:>16} {args.b_label:>16}")
        for s in shapes:
            ids = [i for i in shared if A[i]["shape"] == s]
            ua = sum(A[i]["outcome"] in ("useful", "partial") for i in ids)
            ub = sum(B[i]["outcome"] in ("useful", "partial") for i in ids)
            print(f"  {s:<22} {band(ua, len(ids)):>16} {band(ub, len(ids)):>16}")


if __name__ == "__main__":
    main()
