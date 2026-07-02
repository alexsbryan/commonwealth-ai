#!/usr/bin/env python3
"""spec-eval: diff reconcile's findings against a human answer key.

The whole flywheel, minus the optimisation: reconcile already wrote its verdicts
to spec_findings.json, so measuring is just joining that against an answer_key.json
of ground-truth verdicts (on the claim statement) and printing where they differ.
No LLM, no daemon — two files in, a report out, so tuning metrics/thresholds is
instant. Re-run reconcile (the slow, model half) only when you change the prompt
or the recall; everything downstream of the saved JSON iterates here for free.

Usage:
  ./score.py [answer_key.json] [spec_findings.json]
  defaults: answer_key.CODE_INTEL_CHAT.json
            ~/.sovereign/specs/CODE_INTEL_CHAT/spec_findings.json
  (point arg 2 at spec_findings.4b.json to compare a different model run)
"""
import json
import os
import sys
from collections import Counter

KINDS = ["corroborated", "todo", "drift", "gap", "unverifiable"]


def load(p):
    with open(os.path.expanduser(p)) as f:
        return json.load(f)


def pct(a, b):
    return f"{100 * a / b:.0f}%" if b else "n/a"


def ratio(a, b):
    return f"{a}/{b} ({pct(a, b)})" if b else "n/a (none)"


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    ak_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "answer_key.CODE_INTEL_CHAT.json")
    ak = load(ak_path)
    spec = ak["spec"]
    fj_path = sys.argv[2] if len(sys.argv) > 2 else f"~/.sovereign/specs/{spec}/spec_findings.json"
    fj = load(fj_path)

    labels = ak["labels"]                                    # statement -> {expect, conf, why}
    reported = {f["statement"]: f["kind"] for f in fj["findings"]}

    rows = [(s, lab["expect"], reported.get(s), lab.get("conf", "?"), lab["why"])
            for s, lab in labels.items()]
    matched = [r for r in rows if r[2] is not None]
    unmatched_label = [r for r in rows if r[2] is None]
    unmatched_finding = [s for s in reported if s not in labels]

    agree = [r for r in matched if r[1] == r[2]]
    disagree = [r for r in matched if r[1] != r[2]]
    hi = [r for r in matched if r[3] == "high"]
    hi_agree = [r for r in hi if r[1] == r[2]]

    rep_drift = [r for r in matched if r[2] == "drift"]
    exp_drift = [r for r in matched if r[1] == "drift"]
    drift_tp = [r for r in rep_drift if r[1] == "drift"]

    false_gap = [r for r in matched if r[2] == "gap" and r[1] in ("corroborated", "todo")]
    false_corrob = [r for r in matched if r[2] == "corroborated" and r[1] in ("gap", "drift", "todo")]

    conf = Counter((r[1], r[2]) for r in matched)

    print(f"\nspec-eval — {spec}  vs  {os.path.basename(fj_path)}")
    print("=" * 66)
    print(f"claims labelled: {len(rows)}   matched to a finding: {len(matched)}")
    if unmatched_label:
        print(f"  ! {len(unmatched_label)} labelled claim(s) had NO finding (statement drift?)")
    if unmatched_finding:
        print(f"  ! {len(unmatched_finding)} reported finding(s) had NO label")
    print()
    print(f"agreement (all):       {len(agree)}/{len(matched)}  ({pct(len(agree), len(matched))})")
    print(f"agreement (high-conf): {len(hi_agree)}/{len(hi)}  ({pct(len(hi_agree), len(hi))})   <- trust this one")
    print()
    print(f"drift precision: {ratio(len(drift_tp), len(rep_drift))}   real drifts / reported")
    print(f"drift recall:    {ratio(len(drift_tp), len(exp_drift))}   caught / labelled")
    print()
    print("error modes:")
    print(f"  false GAP  (said gap, is built):       {len(false_gap)}   <- recall miss: candidate bundle lacked the fn")
    print(f"  false CORROBORATED (said done, isn't): {len(false_corrob)}   <- the trust-killer")
    print()
    print("confusion (expect -> got):")
    for e in KINDS:
        for g in KINDS:
            n = conf.get((e, g), 0)
            if n:
                print(f"  {e:>13} -> {g:<13} {n}{'   AGREE' if e == g else ''}")
    print()
    print("disagreements (the actionable list):")
    for s, exp, got, c, why in sorted(disagree, key=lambda r: r[1]):
        print(f"  [{c:>4}] expect {exp:<13} got {got:<13} | {s[:66]}")
        print(f"          why: {why}")
    print()


if __name__ == "__main__":
    main()
