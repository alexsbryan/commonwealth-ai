#!/usr/bin/env python3
"""Validation harvest report for the longform-negative dev banks.

WHAT THIS ANSWERS. The H4 gate returned could-not-judge twice because the
dev banks' held-out label set was 23 supported / 0 not — a set on which a
scorer answering "supported" unconditionally scores 1.0000 and clears the
0.90 beat bar outright. Ten longform-negative probes were authored to fix
that. This script measures whether they did, from a live `--gv-shadow`
harvest, and reports three things the order asks for:

  1. class counts and spread (turns, probes, failure classes, per split side)
  2. the NAIVE ALWAYS-SUPPORTED CEILING on the resulting label set
  3. the routed_intent distribution, flagging the evidence-blind class

THE LABEL RULE IS NOT INVENTED HERE. It is the H4 gate's own, read off
`bench_cmd/h4/transcript.rs:74-81` so there is ONE decider for what a
negative is:

    verification == "verified"    -> supported      (positive)
    verification == "failed_once" -> NOT supported  (negative)
    "fail_open" | "unverified"    -> could-not-judge, EXCLUDED
    anything else                 -> could-not-judge, EXCLUDED and named

`fail_open` means the verifier errored or declined and the claim shipped
unchecked; `unverified` means no verifier ran. Neither is a failure, and
counting either as one would manufacture a negative class out of a
telemetry gap. An unrecognised string is reported as unreadable rather
than silently bucketed (ARCH §18.3).

THE LONGFORM PIVOT IS MEASURED, NOT ASSUMED. A probe only exercises the
per-claim ladder H4 exists to replace if its DRAFT crossed the profile's
`longform_chars` (1,800 on KnowledgeQuery — grounding/config.rs:423,
grounding/mod.rs:741-756). A "longform" probe whose answer came in under
the pivot took the short path and is reported as such, because a probe
that did not go longform cannot be evidence about longform.

Usage:
    longform_negative_report.py --transcripts holdout=<a.jsonl> \\
        --transcripts calibration=<b.jsonl> [--json out.json]
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from collections import Counter
from datetime import datetime

# The gate's own thresholds, quoted so a reader does not have to go find them.
LONGFORM_PIVOT_CHARS = 1_800  # grounding/config.rs:423 (KnowledgeQuery)
H4_BEAT_BAR = 0.90  # NATIVE_GROUNDING.md §7.3 H4 (a)
# The route whose sealed evidence universe is a step-summary transcript that
# is never projected into `retrieved_chunks` — the class the H4 findings call
# structurally blind. A new probe landing here is a finding, not a pass.
EVIDENCE_BLIND_INTENT = "ComplexTask"

SUPPORTED = "verified"
NEGATIVE = "failed_once"
CANNOT_JUDGE = ("fail_open", "unverified")


def failure_class(probe_id: str) -> str | None:
    """`longneg-<class>-<slug>` -> `<class>`; None for a pre-existing probe."""
    if not probe_id.startswith("longneg-"):
        return None
    parts = probe_id.split("-")
    return parts[1] if len(parts) > 2 else None


def read_turns(path: str) -> list[dict]:
    turns = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                turns.append(json.loads(line))
    return turns


def score_side(name: str, turns: list[dict]) -> dict:
    pos = neg = 0
    unreadable: Counter[str] = Counter()
    cannot_judge = 0
    neg_turns: list[str] = []
    neg_by_class: Counter[str] = Counter()
    pos_by_class: Counter[str] = Counter()
    per_probe: list[dict] = []
    intents: Counter[str] = Counter()
    blind: list[str] = []
    longform_ok: list[str] = []
    longform_short: list[tuple[str, int]] = []

    for t in turns:
        pid = t.get("id", "?")
        cls = failure_class(pid)
        holdings = (t.get("epistemic_state") or {}).get("holdings") or []
        tp = tn = tcnj = 0
        for h in holdings:
            v = h.get("verification")
            if v == SUPPORTED:
                tp += 1
            elif v == NEGATIVE:
                tn += 1
            elif v in CANNOT_JUDGE:
                tcnj += 1
            else:
                unreadable[str(v)] += 1
        pos += tp
        neg += tn
        cannot_judge += tcnj
        if tn:
            neg_turns.append(pid)
        if cls:
            neg_by_class[cls] += tn
            pos_by_class[cls] += tp
            # The pivot check only means something for the authored probes.
            answer = t.get("answer") or ""
            n_chars = len(answer)
            if n_chars > LONGFORM_PIVOT_CHARS:
                longform_ok.append(pid)
            else:
                longform_short.append((pid, n_chars))

        intent = t.get("routed_intent")
        intents[intent if intent else "(absent)"] += 1
        if intent == EVIDENCE_BLIND_INTENT:
            blind.append(pid)

        if cls:
            per_probe.append(
                {
                    "id": pid,
                    "class": cls,
                    "qtype": t.get("qtype"),
                    "routed_intent": t.get("routed_intent"),
                    "answer_chars": len(t.get("answer") or ""),
                    "gate_action": t.get("gate_action"),
                    "holdings": len(holdings),
                    "supported": tp,
                    "not_supported": tn,
                    "could_not_judge": tcnj,
                }
            )

    labeled = pos + neg
    return {
        "side": name,
        "turns": len(turns),
        "claims_supported": pos,
        "claims_not_supported": neg,
        "claims_could_not_judge": cannot_judge,
        "claims_unreadable": dict(unreadable),
        "labeled_two_class_total": labeled,
        # Absence is reported, never defaulted: a single-class set has no
        # meaningful ceiling and says so rather than printing 1.0000 as if
        # it were a measurement.
        "naive_always_supported_ceiling": (pos / labeled) if labeled and neg else None,
        "ceiling_note": None
        if neg
        else "single-class label set — no ceiling is meaningful",
        "negative_carrying_turns": sorted(set(neg_turns)),
        "negative_carrying_turn_count": len(set(neg_turns)),
        "negatives_by_failure_class": dict(neg_by_class),
        "positives_by_failure_class": dict(pos_by_class),
        "routed_intent_distribution": dict(intents),
        "evidence_blind_probes": blind,
        "longform_probes_over_pivot": sorted(longform_ok),
        "longform_probes_under_pivot": sorted(longform_short),
        "per_longneg_probe": per_probe,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--transcripts",
        action="append",
        required=True,
        metavar="SIDE=PATH",
        help="split side and its transcripts.jsonl, e.g. holdout=a.jsonl",
    )
    ap.add_argument("--json", help="also write the full report as JSON")
    ap.add_argument(
        "--binary",
        help="the CLI that produced these transcripts; stat'd for provenance",
    )
    args = ap.parse_args()

    # PROVENANCE IS COMPUTED, NOT ASSERTED. The script stats the binary
    # itself rather than printing a string the caller supplied, and it
    # re-checks the one property the report depends on — whether this
    # binary can emit `routed_intent` at all. A report whose central column
    # is empty because the binary predates the field must say so on its own
    # face, not leave a reader to infer it from a column of nulls.
    provenance = None
    if args.binary:
        st = os.stat(args.binary)
        try:
            blob = subprocess.run(
                ["strings", args.binary], capture_output=True, text=True, check=True
            ).stdout
            emits = blob.count("routed_intent") > 0
        except (OSError, subprocess.SubprocessError):
            emits = None  # could not judge — never defaulted to True
        provenance = {
            "binary": os.path.abspath(args.binary),
            "built": datetime.fromtimestamp(st.st_mtime).astimezone().isoformat(),
            "bytes": st.st_size,
            "emits_routed_intent": emits,
        }

    sides = []
    for spec in args.transcripts:
        if "=" not in spec:
            print(f"--transcripts wants SIDE=PATH, got {spec!r}", file=sys.stderr)
            return 2
        name, path = spec.split("=", 1)
        sides.append(score_side(name, read_turns(path)))

    total_pos = sum(s["claims_supported"] for s in sides)
    total_neg = sum(s["claims_not_supported"] for s in sides)
    total_labeled = total_pos + total_neg
    overall_ceiling = (total_pos / total_labeled) if total_labeled and total_neg else None

    # Classes that actually CARRY a negative. `negatives_by_failure_class`
    # keeps a zero row for every class that ran, which is the honest per-side
    # view; folding those zeros into this set would report a class as
    # carrying negatives when it carried none, and the class-diversity
    # verdict below reads this set.
    classes: set[str] = set()
    for s in sides:
        classes |= {k for k, n in s["negatives_by_failure_class"].items() if n > 0}

    w = print
    w("=" * 72)
    w(" LONGFORM-NEGATIVE VALIDATION HARVEST")
    w("=" * 72)
    if provenance:
        w(f"  produced by  {provenance['binary']}")
        w(f"  built        {provenance['built']}  ({provenance['bytes']} bytes)")
        emits = provenance["emits_routed_intent"]
        w(f"  routed_intent capable  "
          f"{'yes' if emits else ('COULD NOT CHECK' if emits is None else 'NO')}")
    else:
        w("  produced by  UNRECORDED — pass --binary so the artifact carries "
          "its own provenance")
    for s in sides:
        w("")
        w(f"── {s['side']} ─────────────────────────────────────────────")
        w(f"  turns                          {s['turns']}")
        w(f"  claims supported (verified)    {s['claims_supported']}")
        w(f"  claims NOT supported           {s['claims_not_supported']}")
        w(f"  claims could-not-judge         {s['claims_could_not_judge']}")
        if s["claims_unreadable"]:
            w(f"  claims UNREADABLE              {s['claims_unreadable']}")
        w(f"  negative-carrying turns        {s['negative_carrying_turn_count']}")
        for t in s["negative_carrying_turns"]:
            w(f"      · {t}")
        w(f"  negatives by failure class     {s['negatives_by_failure_class']}")
        c = s["naive_always_supported_ceiling"]
        if c is None:
            w(f"  NAIVE always-supported ceiling  n/a — {s['ceiling_note']}")
        else:
            verdict = "BELOW the 0.90 bar" if c < H4_BEAT_BAR else "AT OR ABOVE the 0.90 bar"
            w(f"  NAIVE always-supported ceiling  {c:.4f}   ({verdict})")
        w(f"  routed_intent                  {s['routed_intent_distribution']}")
        if s["evidence_blind_probes"]:
            w(f"  !! routed to {EVIDENCE_BLIND_INTENT} (evidence-blind): "
              f"{s['evidence_blind_probes']}")
        if s["longform_probes_under_pivot"]:
            w(f"  !! longneg probes UNDER the {LONGFORM_PIVOT_CHARS}-char pivot "
              f"(took the SHORT path, so they are not longform evidence):")
            for pid, n in s["longform_probes_under_pivot"]:
                w(f"      · {pid}  {n} chars")
        w(f"  longneg probes over the pivot  {len(s['longform_probes_over_pivot'])}")

    w("")
    w("── overall ────────────────────────────────────────────────")
    w(f"  labeled claims (two-class)     {total_labeled}"
      f"  ({total_pos} supported / {total_neg} not)")
    if overall_ceiling is None:
        w("  NAIVE always-supported ceiling  n/a — single-class label set")
    else:
        w(f"  NAIVE always-supported ceiling  {overall_ceiling:.4f}")
    w(f"  failure classes carrying negatives  {sorted(classes)}")

    # The order's success condition, stated as a verdict rather than left
    # for a reader to compute.
    w("")
    per_side_ok = all(
        s["naive_always_supported_ceiling"] is not None
        and s["naive_always_supported_ceiling"] < H4_BEAT_BAR
        for s in sides
    )
    two_per_side = all(s["negative_carrying_turn_count"] >= 2 for s in sides)
    if per_side_ok and two_per_side and len(classes) >= 2:
        w(f"  VERDICT: the naive ceiling is strictly below {H4_BEAT_BAR} on every "
          f"side, with >=2 negative-carrying turns per side. The set can "
          f"demonstrate discernment.")
        rc = 0
    else:
        reasons = []
        for s in sides:
            c = s["naive_always_supported_ceiling"]
            if c is None:
                reasons.append(f"{s['side']}: single-class label set")
            elif c >= H4_BEAT_BAR:
                reasons.append(f"{s['side']}: ceiling {c:.4f} >= {H4_BEAT_BAR}")
            if s["negative_carrying_turn_count"] < 2:
                reasons.append(
                    f"{s['side']}: {s['negative_carrying_turn_count']} "
                    f"negative-carrying turn(s), needs >=2"
                )
        if len(classes) < 2:
            reasons.append(f"only {len(classes)} failure class(es) carry negatives")
        w("  VERDICT: NOT YET — " + "; ".join(reasons))
        rc = 1

    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(
                {
                    "sides": sides,
                    "provenance": provenance,
                    "overall": {
                        "claims_supported": total_pos,
                        "claims_not_supported": total_neg,
                        "labeled_two_class_total": total_labeled,
                        "naive_always_supported_ceiling": overall_ceiling,
                        "failure_classes_with_negatives": sorted(classes),
                        "h4_beat_bar": H4_BEAT_BAR,
                        "longform_pivot_chars": LONGFORM_PIVOT_CHARS,
                    },
                },
                fh,
                indent=2,
            )
        w(f"\n  [json] {args.json}")
    return rc


if __name__ == "__main__":
    sys.exit(main())
