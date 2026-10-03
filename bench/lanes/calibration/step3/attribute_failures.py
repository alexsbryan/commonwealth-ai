#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""D3 — component attribution over the D2 failure corpus.

Order native-grounding-step3-tuning, deliverable D3. Reads
failure_corpus.jsonl and attributes every case to exactly one primary
component, by rules stated here and nowhere else (one decider, one name).
Secondary mechanisms are reported separately and never double-counted.

The counterfactual evidence per rule:
- "admission forced open" is not simulated — it is the committed flag-off
  arm, which differs from flag-on only in SOVEREIGN_NATIVE_GROUNDING
  (FINDINGS.md "How the comparison was made honest").
- presence-of-answer is the bank's grep-verified gold keyword matched
  verbatim against the retrieved pool.
- H1 determinism: r1 and r2 admission records are identical (33/33
  decisions and margins) — the attribution is not riding one noisy run.

Components (closed set for this corpus; a case matching no rule is
reported UNATTRIBUTED, never defaulted):
    h1_admission_calibration  wrong Abstain from a miscalibrated tau on an
                              answerable turn whose pool held the answer
    routing                   intent misroute upstream of retrieval
                              (incl. probes leaving the grounded path)
    retrieval                 evidence-pool regression in a HARD lane
    span_resolver             (no rule fires on this corpus — resolver
                              never ran on a failing case; its measured
                              weakness lives in resolver-precision/)
    incumbent_judge           a released wrong verdict by the judge
    abstention_action         failure caused by what the abstention DID,
                              not whether it should have fired
    synthesis                 wrong prose with correct evidence available
"""
import json, sys
from collections import Counter, defaultdict
from pathlib import Path

CORPUS = Path(__file__).resolve().parent / "failure_corpus.jsonl"

MECHANISM = {
    "h1_admission_calibration":
        "tau_abstain (margin 5.885, p 0.348) was fitted on 99.5% SEP-family pairs; on "
        "chaos-saltgrass the answerable turns' margins sit below it (p25-p75 of the failing "
        "turns 2.4-5.1), so H1 abstains on turns whose pool verifiably holds the answer — "
        "the flag-off counterfactual answers every one of them correctly.",
    "routing":
        "the probe is classified out of the path that carries the failing capability: A/B "
        "case ood-css-center exits to CodeQuery where no admission, judge, or caveat "
        "discipline exists; HARD-lane misroutes fall from the embed layer to the coarse-LLM "
        "after the classifier-embedding change (4d589963) and misroute there.",
    "retrieval":
        "the composed evidence pool lost bank-declared facts/sources relative to the "
        "2026-07-16/17 pools — drift accumulated from landed retrieval commits (atom-enum "
        "reorder d04a1100/f40f8e72 and siblings), adjudicated in D1_remint_adjudication.md.",
    "abstention_action":
        "once H1 abstains, the action withholds the evidence and reroutes to a parametric "
        "general-knowledge turn: the model then asserts specifics it cannot know (e.g. "
        "'Percival in The Last of Us Part II') behind a 'Not in your sources' caveat that "
        "the honesty classifier accepts — a confabulation surface, and on one probe "
        "(longneg-fabspec-fraud-figures) a pass/fail coin: same Abstain both runs, pass r1, "
        "fail r2.",
    "synthesis":
        "the model produced wrong prose despite reachable evidence or answerable framing "
        "(no case in this corpus attributes here primarily).",
    "incumbent_judge":
        "the judge released a wrong verdict (no case in this corpus attributes here "
        "primarily; gate_action is null on every failing A/B case because the early "
        "decline skips the gate).",
    "span_resolver":
        "no failing case exercised span resolution (citation_located=0, resolver skipped "
        "on parametric turns); the resolver's own measured failure — precision 0.7429 vs "
        "bar 0.98, Verbatim tier 4/130 all wrong — lives in calibration/resolver-precision/ "
        "and is carried into D4 as a candidate on its own evidence, not this corpus's.",
}

def attribute(row):
    fam = row["family"]
    st = row["stage_trace"]
    if fam in ("comp_loss", "comp_loss_r2_only"):
        adm = st["admission"]["r1"]
        pool = st["retrieval"]["answer_in_pool"]["present"]
        off_pass = st["synthesis"]["off"]["pass"]
        if adm and adm["decision"] == "Abstain" and pool and off_pass:
            # comp_loss_r2_only: the admission decision is identical (Abstain,
            # same margin) in both runs; what flipped between r1-pass and
            # r2-fail is the parametric fallback's output — the action's coin.
            return ("abstention_action" if fam == "comp_loss_r2_only"
                    else "h1_admission_calibration")
        return "UNATTRIBUTED"
    if fam == "absent_uncaptured":
        # never reached H1: routed to CodeQuery (both arms), no gate, no caveat.
        return "routing"
    if fam in ("retrieval_fact_loss", "retrieval_source_loss"):
        return "retrieval"
    if fam == "routing_misroute":
        return "routing"
    return "UNATTRIBUTED"

def main():
    rows = [json.loads(l) for l in CORPUS.read_text().splitlines()]
    counts = Counter()
    per = defaultdict(list)
    for r in rows:
        c = attribute(r)
        counts[c] += 1
        per[c].append(r["case_id"])
    out = {
        "schema": "step3-attribution/v1",
        "corpus": str(CORPUS.name),
        "n_cases": len(rows),
        "attribution": {
            comp: {
                "count": counts.get(comp, 0),
                "cases": per.get(comp, []),
                "mechanism": MECHANISM[comp],
            }
            for comp in MECHANISM
        },
        "unattributed": per.get("UNATTRIBUTED", []),
        "repeat_counts": {
            "ab_admission": "r1 vs r2 identical on all 33 admitted turns (decisions and margins); "
                            "comp-loss pass/fail reproduced in r2 for all 15 r1 cases",
            "hard_lanes": "fail signature identical across the step2-order run (2026-08-09) and "
                          "the seat control run (2026-08-09/10); routing misroutes stable in the "
                          "2026-08-10 re-mint capture as well",
        },
    }
    outp = Path(__file__).resolve().parent / "attribution.json"
    outp.write_text(json.dumps(out, indent=2) + "\n")
    print(f"{len(rows)} cases")
    for comp, d in out["attribution"].items():
        print(f"  {comp:26s} {d['count']:3d}  {', '.join(d['cases'][:4])}{'...' if d['count']>4 else ''}")
    if out["unattributed"]:
        print("  UNATTRIBUTED:", out["unattributed"])
    print(f"-> {outp}")

if __name__ == "__main__":
    sys.exit(main())
