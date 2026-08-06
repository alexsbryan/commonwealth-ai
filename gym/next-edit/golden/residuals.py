#!/usr/bin/env python3
"""Mine the model's failure pool for structure worth building against.

Spec: `sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §4.

THE QUESTION THIS ANSWERS: "what does the raw model do poorly at, and is
there a residual pattern we could scoop up?" A headline useful-fire rate
cannot answer it — it collapses every kind of failure into one number.
This splits the pool apart along the axes that imply DIFFERENT fixes:

  the model was never asked          -> a gate/routing fix
  asked, produced nothing            -> a prompt/region fix
  asked, produced something rejected -> a verifier-calibration question
  asked, accepted, and wrong         -> a model-capability finding

Only the last is evidence about the model. Conflating it with the other
three is how "the model is bad at this" gets asserted about a system
that never let the model speak.

READ `far_from_cursor` FIRST. A forced consult has no needle
(`next_edit_model.rs`: `Consult::No {..} => ("forced", None)`), so its
region falls back to the cursor line. When the truth is far from the
cursor the region cannot contain it and the model CANNOT succeed — that
is a region-selection ceiling, not a capability result, and it must be
excluded before any claim about what the model "can't do".

    python3 gym/next-edit/golden/residuals.py \
        --rows rows-forced.json [--baseline rows-gated.json]
"""

from __future__ import annotations

import argparse
import collections
import gzip
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from score_golden import read_cases, wilson  # noqa: E402

WON = ("useful", "partial")


def band(k: int, n: int) -> str:
    if not n:
        return "    n/a"
    lo, hi = wilson(k, n)
    return f"{100*k/n:5.1f}% [{100*lo:4.1f}-{100*hi:4.1f}]  n={n}"


def slice_table(title: str, pool: list[tuple], key, minimum: int = 1) -> None:
    """`pool` is [(case, row)]. Prints useful-rate per bucket, biggest
    bucket first — the shape of the failure, not just its size."""
    by: dict = collections.defaultdict(list)
    for c, r in pool:
        by[key(c, r)].append((c, r))
    print(f"\n  {title}")
    for k, items in sorted(by.items(), key=lambda kv: -len(kv[1])):
        if len(items) < minimum:
            continue
        won = sum(r["outcome"] in WON for _, r in items)
        print(f"    {str(k):<26} {band(won, len(items))}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rows", required=True, help="rows json from the FORCED run")
    ap.add_argument("--baseline", help="rows json from the GATED run (for the hidden slice)")
    ap.add_argument("--cases", default="gym/next-edit/golden/cases.jsonl.gz")
    args = ap.parse_args()

    cases = {c["id"]: c for c in read_cases(args.cases)}
    rows = {r["id"]: r for r in json.load(open(args.rows))}
    base = {r["id"]: r for r in json.load(open(args.baseline))} if args.baseline else {}

    pairs = [(cases[i], rows[i]) for i in rows if i in cases]
    pos = [(c, r) for c, r in pairs if c["kind"] == "positive"]
    neg = [(c, r) for c, r in pairs if c["kind"] == "negative"]

    # 1. THE FUNNEL. Where model output dies, in order. This is the
    # difference between a generation problem and a plumbing problem.
    print("=" * 72)
    print("THE RAW-MODEL FUNNEL — what became of the model's output")
    print("=" * 72)
    st = collections.Counter(r["model_state"] for _, r in pairs)
    n = sum(st.values())
    for k, v in st.most_common():
        print(f"  {k:<26} {v:>5}  {100*v/n:5.1f}%")
    asked = sum(v for k, v in st.items() if not k.startswith("skipped:"))
    fired = st.get("fired", 0)
    print(f"  {'-> asked':<26} {asked:>5}  {100*asked/n:5.1f}%")
    print(f"  {'-> survived the verifier':<26} {fired:>5}  "
          f"{100*fired/max(1,asked):5.1f}% of asked")

    # 2. THE CEILING. Excluded before any capability claim.
    print("\n" + "=" * 72)
    print("REGION CEILING — cases the model could not have won")
    print("=" * 72)
    far = [(c, r) for c, r in pos if c.get("far_from_cursor")]
    near = [(c, r) for c, r in pos if not c.get("far_from_cursor")]
    print(f"  far_from_cursor  {band(sum(r['outcome'] in WON for _, r in far), len(far))}")
    print(f"  near cursor      {band(sum(r['outcome'] in WON for _, r in near), len(near))}")
    print("  A forced consult has no needle, so its region is the cursor line.")
    print("  If these differ, the gap is REGION SELECTION, not model ability.")

    # 3. THE FAILURE POOL, split by the fix each kind implies.
    print("\n" + "=" * 72)
    print("THE FAILURE POOL — positives the model did not win")
    print("=" * 72)
    lost = [(c, r) for c, r in near if r["outcome"] not in WON]
    print(f"  {len(lost)} of {len(near)} near-cursor positives lost"
          f"  ({100*len(lost)/max(1,len(near)):.1f}%)")
    kinds = collections.Counter(
        "accepted but WRONG" if r["outcome"] == "wrong" and r["model_state"] == "fired"
        else ("silent (rule lane too)" if r["model_state"].startswith("skipped:")
              else r["model_state"])
        for _, r in lost)
    for k, v in kinds.most_common():
        print(f"    {k:<26} {v:>5}  {100*v/max(1,len(lost)):5.1f}%")

    # Slice the FULL near-cursor pool, not the lost pool: a useful-rate
    # computed over losses is 0% by construction and says nothing. What
    # we want is where the system is weak, and then why it lost there.
    slice_table("useful-rate by shape (near-cursor positives)", near,
                lambda c, r: c["shape"], minimum=3)
    slice_table("useful-rate by truth edit-count", near,
                lambda c, r: f"{len(c['expect']['truth'])} edit(s)")
    slice_table("useful-rate by language", near, lambda c, r: c["language"], minimum=5)

    # And the mode of failure per shape — the same loss count can mean
    # "the model was never asked" or "the model answered and was wrong",
    # and those fund completely different work.
    print("\n  dominant failure mode per shape")
    by_shape: dict = collections.defaultdict(collections.Counter)
    for c, r in lost:
        mode = ("accepted but WRONG" if r["outcome"] == "wrong" and r["model_state"] == "fired"
                else ("never asked" if r["model_state"].startswith("skipped:")
                      else r["model_state"]))
        by_shape[c["shape"]][mode] += 1
    for s, ctr in sorted(by_shape.items(), key=lambda kv: -sum(kv[1].values())):
        top = ", ".join(f"{m}×{k}" for m, k in ctr.most_common(3))
        print(f"    {s:<22} {sum(ctr.values()):>4} lost  {top}")

    # 4. THE SCOOP. Where a NEW mechanism would pay, ranked by size:
    # near-cursor positives that neither lane won, grouped by the bank's
    # own account of why the gate declines them.
    print("\n" + "=" * 72)
    print("THE SCOOP — near-cursor positives NO lane won, by gate rationale")
    print("=" * 72)
    scoop = [(c, r) for c, r in lost if r["model_state"] != "fired"]
    by = collections.defaultdict(list)
    for c, r in scoop:
        by[str(c.get("gate", "unknown"))].append((c, r))
    for k, items in sorted(by.items(), key=lambda kv: -len(kv[1])):
        shapes = collections.Counter(c["shape"] for c, _ in items)
        top = ", ".join(f"{s}×{n}" for s, n in shapes.most_common(3))
        print(f"  {len(items):>4}  {k[:44]:<44} {top}")

    # 5. What forcing actually bought, on the slice the gate was hiding.
    if base:
        hidden = [i for i in rows if i in base
                  and base[i]["model_state"].startswith("skipped:gate")
                  and cases[i]["kind"] == "positive"]
        won = sum(rows[i]["outcome"] in WON for i in hidden)
        wrong = sum(rows[i]["outcome"] == "wrong" for i in hidden)
        nh = [i for i in rows if i in base
              and base[i]["model_state"].startswith("skipped:gate")
              and cases[i]["kind"] == "negative"]
        nwrong = sum(rows[i]["outcome"] == "wrong" for i in nh)
        print("\n" + "=" * 72)
        print("THE HIDDEN SLICE — what the gate was refusing to ask")
        print("=" * 72)
        print(f"  {len(hidden)} positives the gated run never asked about")
        print(f"    forcing wins {won} of them ({100*won/max(1,len(hidden)):.1f}%), "
              f"and gets {wrong} wrong")
        print(f"  {len(nh)} negatives it also hid: {nwrong} wrong fires "
              f"({100*nwrong/max(1,len(nh)):.1f}%)")
        net = won - (wrong + nwrong)
        print(f"  -> widening buys {won} edits for {wrong + nwrong} wrong fires "
              f"(net {net:+d})")


if __name__ == "__main__":
    main()
