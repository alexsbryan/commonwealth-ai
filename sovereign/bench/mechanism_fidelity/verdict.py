#!/usr/bin/env python3
"""Mechanism-Fidelity verdict — read ResultRow JSONL, emit a tiered,
power-annotated read.

This is the only throwaway component of the harness (the doc's design):
the Rust core in `sovereign-eval::mechanism_fidelity` generates cases,
elicits decisions, scores them, and emits one ResultRow per probe. This
sidecar reads that JSONL and answers the go/no-go question:

  Does a mechanism-faithful agent show the P1 collapse (~0.95 -> ~0.01
  when exit becomes expensive), stay flat on P2 (saturation) and I1
  (identity invariance), while the feature-stripped negative control
  sits at chance?

Stdlib only (no numpy) so it runs anywhere. Mirrors the argparse ->
load-JSONL -> print-table shape of bench/atlas_retrieval/run_bench.py.

Usage:
  python3 verdict.py results/dev.jsonl [more.jsonl ...] [--manifest manifest.toml]
"""

import argparse
import json
import math
import statistics
import sys
from collections import defaultdict
from pathlib import Path

# Defaults mirror sovereign-eval::mechanism_fidelity::score::Bands and the
# manifest's [negative_control]. The manifest, when supplied, overrides.
DEFAULTS = {
    "collapse_min": 0.40,
    "flat_max": 0.10,
    "inv_max": 0.05,
    "big_struct": 0.50,
    "small_struct": 0.05,
    "control_max_directional_accuracy": 0.55,
    "control_max_abs_delta": 0.10,
    "tier1_min_magnitude_pass": 0.80,
    "tier1_min_models": 2,
}


def load_rows(paths):
    rows = []
    for p in paths:
        path = Path(p)
        if not path.exists():
            print(f"warning: {path} does not exist, skipping", file=sys.stderr)
            continue
        with path.open() as f:
            for ln, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError as e:
                    print(f"warning: {path}:{ln} bad JSON: {e}", file=sys.stderr)
    return rows


def load_thresholds(manifest_path):
    t = dict(DEFAULTS)
    if not manifest_path:
        return t
    try:
        import tomllib  # Python 3.11+
    except ModuleNotFoundError:
        print(
            "note: tomllib unavailable (Python < 3.11); using default thresholds",
            file=sys.stderr,
        )
        return t
    p = Path(manifest_path)
    if not p.exists():
        print(f"note: manifest {p} not found; using default thresholds", file=sys.stderr)
        return t
    with p.open("rb") as f:
        m = tomllib.load(f)
    bands = m.get("bands", {})
    for k in ("collapse_min", "flat_max", "inv_max", "big_struct", "small_struct"):
        if k in bands:
            t[k] = bands[k]
    nc = m.get("negative_control", {})
    if "max_directional_accuracy" in nc:
        t["control_max_directional_accuracy"] = nc["max_directional_accuracy"]
    if "max_abs_delta" in nc:
        t["control_max_abs_delta"] = nc["max_abs_delta"]
    tier1 = m.get("acceptance", {}).get("tier1_consistency", {})
    if "min_large_delta_magnitude_pass" in tier1:
        t["tier1_min_magnitude_pass"] = tier1["min_large_delta_magnitude_pass"]
    if "min_models" in tier1:
        t["tier1_min_models"] = tier1["min_models"]
    return t


def mean(xs):
    xs = [x for x in xs if x is not None and isinstance(x, (int, float)) and math.isfinite(x)]
    return statistics.fmean(xs) if xs else float("nan")


def sem(xs):
    """Standard error of the mean across cases."""
    xs = [x for x in xs if x is not None and math.isfinite(x)]
    if len(xs) < 2:
        return float("nan")
    return statistics.pstdev(xs) / math.sqrt(len(xs))


def pass_frac(rows, key):
    """Fraction True among rows whose `key` is non-null (band applied)."""
    vals = [r[key] for r in rows if r.get(key) is not None]
    if not vals:
        return float("nan"), 0
    return sum(1 for v in vals if v) / len(vals), len(vals)


def sign(x):
    return (x > 0) - (x < 0)


def fmt(x, nd=3):
    return "  nan" if x != x else f"{x:+.{nd}f}" if nd else f"{x}"


def summarize_model(rows, model, t):
    def sel(variant, control, paraphrase=False):
        return [
            r
            for r in rows
            if r["model_id"] == model
            and r["variant"] == variant
            and r["control"] == control
            and (control or r["paraphrase"] == paraphrase)
        ]

    p1 = sel("dir_p1", control=False)
    p1_ctrl = sel("dir_p1", control=True)
    p2 = sel("dir_p2", control=False)
    inv = sel("inv_i1", control=False)

    # K actually achieved (effective draws), for the power annotation.
    ks = [r["k_draws"] for r in rows if r["model_id"] == model and r.get("k_draws")]
    k_med = int(statistics.median(ks)) if ks else 0
    se_d = 0.707 / math.sqrt(k_med) if k_med else float("nan")

    mag_pass, mag_n = pass_frac(p1, "magnitude_ok")
    dir_pass, _ = pass_frac(p1, "direction_ok")
    p2_pass, _ = pass_frac(p2, "flat_ok")
    inv_pass, _ = pass_frac(inv, "invariance_ok")

    # Control directional accuracy on P1: should sit at chance.
    ctrl_dir = [
        sign(r["d_agent"]) == sign(r["d_struct"])
        for r in p1_ctrl
        if math.isfinite(r["d_agent"]) and r["d_struct"] != 0
    ]
    ctrl_dir_acc = (sum(ctrl_dir) / len(ctrl_dir)) if ctrl_dir else float("nan")

    return {
        "model": model,
        "k_med": k_med,
        "se_d": se_d,
        "p1_delta": mean([r["d_agent"] for r in p1]),
        "p1_delta_sem": sem([r["d_agent"] for r in p1]),
        "p1_mag_pass": mag_pass,
        "p1_mag_n": mag_n,
        "p1_dir_pass": dir_pass,
        "p2_abs": mean([abs(r["d_agent"]) for r in p2]),
        "p2_flat_pass": p2_pass,
        "inv_abs": mean([abs(r["d_agent"]) for r in inv]),
        "inv_flat_pass": inv_pass,
        "ctrl_p1_delta": mean([r["d_agent"] for r in p1_ctrl]),
        "ctrl_dir_acc": ctrl_dir_acc,
        "n_cases": len({r["case_id"].split("~")[0] for r in p1}),
    }


def control_fails(s, t):
    """The control must FAIL sensitivity: near-chance direction AND
    movement within the flat band. (NaN -> treat as not-failing so a
    missing control surfaces rather than passing silently.)"""
    acc_ok = (
        math.isfinite(s["ctrl_dir_acc"])
        and s["ctrl_dir_acc"] <= t["control_max_directional_accuracy"]
    )
    flat_ok = (
        math.isfinite(s["ctrl_p1_delta"])
        and abs(s["ctrl_p1_delta"]) < t["control_max_abs_delta"]
    )
    return acc_ok and flat_ok


def model_is_faithful(s, t):
    """A model shows mechanism-consistency: strong P1 collapse with the
    magnitude band passing, plus flat P2 and INV."""
    return (
        s["p1_delta"] < -t["collapse_min"]
        and math.isfinite(s["p1_mag_pass"])
        and s["p1_mag_pass"] >= t["tier1_min_magnitude_pass"]
        and math.isfinite(s["p2_flat_pass"])
        and s["p2_flat_pass"] >= t["tier1_min_magnitude_pass"]
        and math.isfinite(s["inv_flat_pass"])
        and s["inv_flat_pass"] >= t["tier1_min_magnitude_pass"]
    )


def main():
    ap = argparse.ArgumentParser(description="Mechanism-fidelity tiered verdict over ResultRow JSONL.")
    ap.add_argument("results", nargs="+", help="ResultRow JSONL file(s)")
    ap.add_argument("--manifest", help="manifest.toml (overrides default bands/control criteria)")
    args = ap.parse_args()

    rows = load_rows(args.results)
    if not rows:
        print("no rows loaded", file=sys.stderr)
        return 2
    t = load_thresholds(args.manifest)
    models = sorted({r["model_id"] for r in rows})
    pool = sorted({r.get("pool", "?") for r in rows})

    print(f"\nmechanism-fidelity verdict   pool={','.join(pool)}   models={len(models)}   rows={len(rows)}")
    print("=" * 92)

    summaries = [summarize_model(rows, m, t) for m in models]

    # Per-model table.
    hdr = (
        f"{'model':<26} {'P1 Δ̄':>8} {'±sem':>6} {'mag%':>6} {'dir%':>6} "
        f"{'P2|Δ̄|':>7} {'flat%':>6} {'INV|Δ̄|':>7} {'inv%':>6} {'ctrlΔ̄':>7} {'ctrlDir':>7} {'K':>4}"
    )
    print(hdr)
    print("-" * len(hdr))
    for s in summaries:
        print(
            f"{s['model'][:26]:<26} "
            f"{fmt(s['p1_delta']):>8} {fmt(s['p1_delta_sem'],3).lstrip('+'):>6} "
            f"{pct(s['p1_mag_pass']):>6} {pct(s['p1_dir_pass']):>6} "
            f"{abs_fmt(s['p2_abs']):>7} {pct(s['p2_flat_pass']):>6} "
            f"{abs_fmt(s['inv_abs']):>7} {pct(s['inv_flat_pass']):>6} "
            f"{fmt(s['ctrl_p1_delta']):>7} {pct(s['ctrl_dir_acc']):>7} {s['k_med']:>4}"
        )

    # Power annotation.
    k_med = max((s["k_med"] for s in summaries), default=0)
    se_d = 0.707 / math.sqrt(k_med) if k_med else float("nan")
    n_cases = max((s["n_cases"] for s in summaries), default=0)
    print(
        f"\npower: K≈{k_med} draws/probe  ->  per-probe SE(d_agent)≈{se_d:.3f}; "
        f"min detectable mean effect ≈ {2*se_d:.3f} at one probe, far finer across {n_cases} cases."
    )
    print(
        "  (synthetic side is high-power by construction; the binding constraint is the\n"
        "   real test pool, out of scope for this go/no-go. This verdict is CONSISTENCY,\n"
        "   not correctness.)"
    )

    # Tiers.
    faithful = [s for s in summaries if model_is_faithful(s, t)]
    controls_fail = all(control_fails(s, t) for s in summaries)
    any_control_present = any(math.isfinite(s["ctrl_dir_acc"]) for s in summaries)

    tier0 = bool(faithful) and any_control_present and controls_fail
    tier1 = (
        len(faithful) >= t["tier1_min_models"]
        and any_control_present
        and controls_fail
    )

    print("\nverdict")
    print("-------")
    print(f"  Tier 0 (instrument valid: control fails while a faithful agent passes): {badge(tier0)}")
    if not any_control_present:
        print("    ! no control probes found — cannot validate the instrument.")
    elif not controls_fail:
        print("    ! a control did NOT fail (it showed sensitivity or biased direction):")
        for s in summaries:
            if not control_fails(s, t):
                print(
                    f"      - {s['model']}: ctrl P1 Δ̄={fmt(s['ctrl_p1_delta'])}, "
                    f"ctrl dir acc={pct(s['ctrl_dir_acc'])} "
                    f"(LEAK if dir acc > {t['control_max_directional_accuracy']:.0%} "
                    f"or |Δ̄| ≥ {t['control_max_abs_delta']})"
                )
    elif not faithful:
        print("    ! control fails correctly, but no model showed the P1 collapse:")
        for s in summaries:
            print(f"      - {s['model']}: P1 Δ̄={fmt(s['p1_delta'])}, mag%={pct(s['p1_mag_pass'])}")

    print(
        f"  Tier 1 (consistency on ≥{t['tier1_min_models']} models, control near chance): {badge(tier1)}"
    )
    print(f"    faithful models: {[s['model'] for s in faithful] or 'none'}")

    go = tier0
    print(f"\n  GO/NO-GO: {'GO — instrument is valid; proceed to corpus + real-holdout work.' if go else 'NO-GO — fix the flagged issue before any corpus/simulation investment.'}")
    return 0 if go else 1


def pct(x):
    return " nan" if x != x else f"{100*x:4.0f}%"


def abs_fmt(x):
    return " nan" if x != x else f"{x:.3f}"


def badge(b):
    return "PASS ✓" if b else "FAIL ✗"


if __name__ == "__main__":
    sys.exit(main())
