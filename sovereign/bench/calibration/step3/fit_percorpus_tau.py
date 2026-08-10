#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""D5 — fit the per-corpus tau operating point for chaos-saltgrass.

Order native-grounding-step3-tuning, D4 rank 1. The fit bank is
saltgrass_compound (25 probes, ALL answerable, zero absent): its labels can
only price false alarms, never honesty, so the judged bank saltgrass's
labels are untouched by this fit.

Rule (pre-registered before the judging runs; the seat-logged bar text is
authoritative): with n compound margins sorted ascending and an FA budget
of 5%, allowed = floor(0.05 * n); tau'_abstain_margin = the (allowed)-th
sorted margin — the largest threshold abstaining on at most `allowed`
compound turns. tau'_answer_margin shifts by the same margin-space delta
as tau_abstain (the band translates; its width is not re-fitted).
Answerability-space values come through the SAME committed Platt fit the
runtime uses — this script re-fits nothing.

Inputs:
  --harvest   run log of ONE flag-on chaos-monkey run over
              saltgrass_compound.toml (H1 admission lines carry margins)
  --calibration  the committed h1_admission_calibration.json

Output: percorpus_tau_saltgrass.json + the two env values to export.
Refuses (exit 2) if the harvest carries fewer than 20 H1 admission lines —
a thin harvest is an instrument problem, not a fit input (ARCH §18.4).
"""
import argparse, gzip, json, math, re, sys
from pathlib import Path

ANSI = re.compile(r"\x1b\[[0-9;]*m")
FA_BUDGET = 0.05
MIN_TURNS = 20

def margins_from_log(path):
    op = gzip.open if str(path).endswith(".gz") else open
    out = []
    with op(path, "rt") as fh:
        for line in fh:
            line = ANSI.sub("", line)
            if "native-grounding H1: answerability admission" in line:
                out.append(float(re.search(r"margin=(\S+)", line).group(1)))
    return out

def platt(cal, margin):
    a, b = cal["platt"]["a"], cal["platt"]["b"]
    z = a * margin + b
    return 1.0 / (1.0 + math.exp(-z)) if z >= 0 else math.exp(z) / (1.0 + math.exp(z))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--harvest", required=True)
    ap.add_argument("--calibration", default=str(Path(__file__).resolve().parents[1] / "h1-port/h1_admission_calibration.json"))
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent / "percorpus_tau_saltgrass.json"))
    args = ap.parse_args()

    margins = sorted(margins_from_log(args.harvest))
    if len(margins) < MIN_TURNS:
        print(f"REFUSING: harvest has {len(margins)} H1 admission lines (< {MIN_TURNS}) — "
              f"validate the instrument before the fit", file=sys.stderr)
        return 2
    cal = json.loads(Path(args.calibration).read_text())
    committed_abstain_m = cal["thresholds"]["tau_abstain_margin"]
    committed_answer_m = cal["thresholds"].get("tau_answer_margin", 6.680750846862793)

    n = len(margins)
    allowed = math.floor(FA_BUDGET * n)
    tau_abstain_m = margins[allowed]  # abstains on exactly the strictly-below ones
    abstained = sum(1 for m in margins if m < tau_abstain_m)
    delta = committed_abstain_m - tau_abstain_m
    tau_answer_m = committed_answer_m - delta

    out = {
        "schema": "percorpus-tau/v1",
        "corpus": "chaos-saltgrass",
        "fit_bank": "sovereign/bench/chaos_monkey/saltgrass_compound.toml (all answerable)",
        "fa_budget": FA_BUDGET,
        "n_compound_turns": n,
        "allowed_abstains": allowed,
        "abstains_at_tau": abstained,
        "compound_margins": margins,
        "committed": {"tau_abstain_margin": committed_abstain_m, "tau_answer_margin": committed_answer_m,
                      "tau_abstain": cal["thresholds"]["tau_abstain"], "tau_answer": cal["thresholds"]["tau_answer"]},
        "fitted": {
            "tau_abstain_margin": tau_abstain_m,
            "tau_answer_margin": tau_answer_m,
            "margin_delta": delta,
            "tau_abstain": platt(cal, tau_abstain_m),
            "tau_answer": platt(cal, tau_answer_m),
        },
        "env": {
            "SOVEREIGN_NG_TAU_ABSTAIN": f'{platt(cal, tau_abstain_m):.12f}',
            "SOVEREIGN_NG_TAU_ANSWER": f'{platt(cal, tau_answer_m):.12f}',
        },
    }
    Path(args.out).write_text(json.dumps(out, indent=2) + "\n")
    print(f"n={n} allowed_abstains={allowed} abstains_at_tau={abstained}")
    print(f"tau_abstain: margin {committed_abstain_m:.3f} -> {tau_abstain_m:.3f}  (p {cal['thresholds']['tau_abstain']:.4f} -> {out['fitted']['tau_abstain']:.4f})")
    print(f"tau_answer:  margin {committed_answer_m:.3f} -> {tau_answer_m:.3f}  (p {cal['thresholds']['tau_answer']:.4f} -> {out['fitted']['tau_answer']:.4f})")
    print(f"export SOVEREIGN_NG_TAU_ABSTAIN={out['env']['SOVEREIGN_NG_TAU_ABSTAIN']}")
    print(f"export SOVEREIGN_NG_TAU_ANSWER={out['env']['SOVEREIGN_NG_TAU_ANSWER']}")
    print(f"-> {args.out}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
