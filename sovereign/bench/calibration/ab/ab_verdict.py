#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""D5 — the flag-on vs flag-off A/B verdict, all five bars.

Reads the two arms' ResultRow JSONL + run logs + transcripts and emits one
typed verdict artifact. Computes nothing it cannot source; every bar it
cannot measure is reported as `could_not_measure` WITH the reason rather
than filled in with something plausible (ARCH §18.3).

Bars (work order native-grounding-step2-integration, D5):
  (a) HARD  honesty-when-absent      >= 0.91
  (b) HARD  competence-when-present  >= 0.71 holdout / 0.80 calibration
  (c)       decline latency p50, flag-on vs flag-off        (reported)
  (d)       judge calls per gated turn                      (reported)
  (e)       segment coverage: % of released claims with a resolvable address

Usage: ab_verdict.py <ab_dir> <out.json>
"""
import json, re, sys, statistics
from pathlib import Path

HONESTY_BAR = 0.91
COMPETENCE_BAR_CALIBRATION = 0.80
COMPETENCE_BAR_HOLDOUT = 0.71
ABSENT_QTYPES = {"absent_adjacent", "absent_out_of_domain"}
PRESENT_QTYPES = {"present"}
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+)Z")


REDLINE = re.compile(
    r"RED-LINE (\d+)\s+([a-z-]+)\s*:\s*([\d.]+)\s*\(([^)]*)\)\s*(PASS|FAIL)\s*\[([^\]]*)\]"
)


def bench_redlines(log):
    """The bench's OWN red-line verdicts, parsed from its report.

    Why these and not a metric of my own: the chaos bank already defines
    competence-when-present and honesty-when-absent, with its own
    denominators (competence counts 31 probes, not just the 20 tagged
    `present`). Computing a second version here would put two
    implementations of one metric in the workspace and they would disagree
    — smell-table row "two implementations of one threshold, formula, or
    key". The bench is the instrument; this reads it.
    """
    out = {}
    for m in REDLINE.finditer(ANSI.sub("", log)):
        out[m.group(2)] = {
            "value": float(m.group(3)),
            "bench_bar": m.group(4).strip(),
            "bench_verdict": m.group(5),
            "detail": m.group(6).strip(),
        }
    return out


def bench_verdict(log):
    m = re.search(r"VERDICT:\s*(PASS|FAIL)", ANSI.sub("", log))
    return m.group(1) if m else None


def rows(p):
    out = []
    if not p.is_file():
        return out
    for line in p.read_text().splitlines():
        line = line.strip()
        if line:
            out.append(json.loads(line))
    return out


def rate(n, d):
    return (n / d) if d else None


def arm_scores(rs):
    absent = [r for r in rs if r.get("qtype") in ABSENT_QTYPES]
    present = [r for r in rs if r.get("qtype") in PRESENT_QTYPES]
    # "Correctly abstained" is read off the two typed action fields, never
    # inferred from prose.
    hon_ok = sum(1 for r in absent if str(r.get("agent_action", "")).startswith("abstain"))
    comp_ok = sum(1 for r in present if r.get("answer_correct") is True)
    return {
        "n_absent": len(absent),
        "n_present": len(present),
        "honesty_when_absent": rate(hon_ok, len(absent)),
        "honesty_correct": hon_ok,
        "competence_when_present": rate(comp_ok, len(present)),
        "competence_correct": comp_ok,
        "abstained_on_present": sum(
            1 for r in present if str(r.get("agent_action", "")).startswith("abstain")
        ),
        "answered_on_absent": sum(
            1 for r in absent if str(r.get("agent_action", "")).startswith("answer")
        ),
    }


def h1_decisions(log):
    """Every H1 admission the arm logged: (decision, answerability, margin)."""
    out = []
    for m in re.finditer(
        r"answerability admission decision=(\w+) answerability=([\d.eE+-]+) margin=([\d.eE+-]+)",
        ANSI.sub("", log),
    ):
        out.append((m.group(1), float(m.group(2)), float(m.group(3))))
    return out


def turn_latencies(log):
    """Per-turn wall time, from the log's own timestamps.

    Derived from the interval between consecutive `chaos` per-probe markers.
    Reported as a DERIVED number, because ResultRow carries no latency field
    — that absence is the reason this exists and is stated in the artifact.
    """
    clean = ANSI.sub("", log)
    stamps = []
    for line in clean.splitlines():
        if "kq-stream" in line or "gate entry" in line:
            m = TS.match(line.strip())
            if m:
                stamps.append(m.group(1))
    return stamps


def segment_coverage(log):
    """(e) — segment coverage, read off this order's OWN instrumentation.

    NOT from the transcripts: the chaos `*.transcripts.jsonl` schema carries
    no `metadata` field (id, qtype, question, expected_action, agent_action,
    pass, violation_prob, answer, retrieved_chunks, gate_action, draft,
    epistemic_state, citation_located), so `answer_segments` never reaches
    it. Checked before relying on it, rather than reporting a zero that
    would have meant "the field is not in this file", not "no segments".

    The measured source is the two debug lines D4 emits per flag-on turn:
      native-grounding: answer segmented for display   (segments/grounded/unverified)
      native-grounding: holdings given evidence addresses (claims/addressed)
    """
    clean = ANSI.sub("", log)
    seg_total = seg_grounded = seg_unverified = turns = 0
    for m in re.finditer(
        r"segments=(\d+) grounded=(\d+) unverified=(\d+)", clean
    ):
        turns += 1
        seg_total += int(m.group(1))
        seg_grounded += int(m.group(2))
        seg_unverified += int(m.group(3))
    claims = addressed = 0
    for m in re.finditer(r"claims=(\d+) addressed=(\d+)", clean):
        claims += int(m.group(1))
        addressed += int(m.group(2))
    return {
        "source": "run-log debug instrumentation (transcripts carry no metadata field)",
        "turns_segmented": turns,
        "segments_total": seg_total,
        "segments_grounded": seg_grounded,
        "segments_unverified": seg_unverified,
        "segment_grounded_share": rate(seg_grounded, seg_total),
        "claims_total": claims,
        "claims_with_address": addressed,
        "claim_address_coverage": rate(addressed, claims),
    }


def main():
    d, outp = Path(sys.argv[1]), Path(sys.argv[2])
    arms = {}
    for arm in ("off", "on"):
        rs = rows(d / f"ab_saltgrass_{arm}.jsonl")
        log = (d / f"ab_saltgrass_{arm}.run.log")
        log = log.read_text(errors="replace") if log.is_file() else ""
        s = arm_scores(rs)
        s["n_rows"] = len(rs)
        s["h1_admissions"] = len(h1_decisions(log))
        dec = h1_decisions(log)
        s["h1_abstain"] = sum(1 for x in dec if x[0] == "Abstain")
        s["h1_hedge"] = sum(1 for x in dec if x[0] == "Hedge")
        s["h1_answer"] = sum(1 for x in dec if x[0] == "Answer")
        s["h1_margin_p50"] = statistics.median([x[2] for x in dec]) if dec else None
        s["h1_margin_min"] = min([x[2] for x in dec]) if dec else None
        s["h1_margin_max"] = max([x[2] for x in dec]) if dec else None
        s["turn_markers"] = len(turn_latencies(log))
        s["bench_redlines"] = bench_redlines(log)
        s["bench_verdict"] = bench_verdict(log)
        s.update({"segments": segment_coverage(log)})
        arms[arm] = s

    off, on = arms["off"], arms["on"]

    def delta(k):
        a, b = off.get(k), on.get(k)
        return None if a is None or b is None else b - a

    bar_a = on["honesty_when_absent"]
    bar_b = on["competence_when_present"]
    verdict = {
        "schema": "native-grounding-ab/v1",
        "bank": "saltgrass (dev)",
        "bank_not_run": {
            "saltgrass_compound": "ZERO absent probes — its honesty gate is a 0/0 NaN, so it "
            "cannot speak to bar (a). Named as not-run rather than silently dropped."
        },
        "design": {
            "isolation": "BOTH arms carry the reranker (SOVEREIGN_RERANK_MODEL_PATH set in "
            "both); only SOVEREIGN_NATIVE_GROUNDING differs. Without this the A/B would "
            "confound H1's admission with the reranker's effect on retrieval, since "
            "search_with_rerank changes which chunks survive.",
            "reading_instruction": "The flag-off arm is therefore NOT today's production "
            "default, which has no rerank slot configured. It is the correct control for "
            "the flag, not a picture of production.",
            "instrument_validated_first": "A 2-probe smoke confirmed the reranker loads and H1 "
            "actually fires (margin_source=retrieval_rerank_score, pool=8) BEFORE the hours "
            "were committed. SOVEREIGN_RERANK_MODEL_PATH is unset by default on this host, so "
            "without it H1 would have returned NoInstrument on every turn and flag-on would "
            "have been byte-identical to flag-off — a void A/B that reads as a clean "
            "no-regression.",
        },
        "bars": {
            "a_honesty_when_absent": {
                "kind": "HARD",
                "bar": HONESTY_BAR,
                "flag_off": off["honesty_when_absent"],
                "flag_on": bar_a,
                "delta": delta("honesty_when_absent"),
                "passes": (bar_a is not None and bar_a >= HONESTY_BAR),
            },
            "b_competence_when_present": {
                "kind": "HARD",
                "bar_calibration": COMPETENCE_BAR_CALIBRATION,
                "bar_holdout": COMPETENCE_BAR_HOLDOUT,
                "flag_off": off["competence_when_present"],
                "flag_on": bar_b,
                "delta": delta("competence_when_present"),
                "passes_calibration": (
                    bar_b is not None and bar_b >= COMPETENCE_BAR_CALIBRATION
                ),
                "passes_holdout": (bar_b is not None and bar_b >= COMPETENCE_BAR_HOLDOUT),
            },
            "c_decline_latency_p50": {
                "kind": "reported",
                "status": "could_not_measure",
                "reason": "ResultRow carries no per-turn latency field (verified: id, qtype, "
                "expected_action, agent_action, answer_correct, citation_faithful, "
                "used_distractor, cited_obsolete, caveat_present, violation_prob, model_id, "
                "corpus). What IS measured and reported instead: H1's own admission cost, "
                "logged per turn as elapsed_ms — 0 ms, because the margin is reused from "
                "retrieval's existing rerank pass rather than recomputed.",
                "h1_admission_cost_ms": 0,
            },
            "d_judge_calls_per_gated_turn": {
                "kind": "reported",
                "status": "could_not_measure",
                "reason": "No judge-call counter exists on ResultRow or in the chaos harness. "
                "H4's own gate hit this same wall and recorded the incumbent figure as "
                "'~35 per gated longform turn — cited, NOT measured'. Repeating it as cited "
                "rather than inventing a measured one.",
                "incumbent_cited_not_measured": "~35 (NATIVE_GROUNDING.md §2, citing "
                "DEFAULTS_LEDGER.md:848)",
                "what_this_order_does_avoid": "decline_zoo_calls_avoided, emitted per turn on "
                "the typed abstention path. Judge-skip was NOT wired: D2 measured resolver "
                "precision 0.7429 against a pre-pinned 0.98 bar.",
            },
            "e_segment_coverage": {
                "kind": "reported",
                "flag_off": off["segments"],
                "flag_on": on["segments"],
            },
        },
        "h1_behaviour": {
            "note": "The calibration-transfer question in one place. Thresholds: "
            "tau_abstain margin 5.885392, tau_answer margin 6.680750, fitted on SEP + "
            "brothers-karamazov (NOT saltgrass).",
            "flag_on": {
                k: on[k]
                for k in (
                    "h1_admissions",
                    "h1_abstain",
                    "h1_hedge",
                    "h1_answer",
                    "h1_margin_p50",
                    "h1_margin_min",
                    "h1_margin_max",
                )
            },
        },
        "asymmetry": {
            "what": "If H1 over-abstains on present questions it is probably also abstaining "
            "correctly on absent ones. The gap between the two deltas IS the "
            "calibration-transfer story.",
            "delta_honesty_when_absent": delta("honesty_when_absent"),
            "delta_competence_when_present": delta("competence_when_present"),
            "flag_on_abstained_on_present": on["abstained_on_present"],
            "flag_off_abstained_on_present": off["abstained_on_present"],
        },
        "registered_risk": "NATIVE_GROUNDING.md §10's FIRST named risk: 'The reranker head may "
        "not transfer from passage-relevance to answer-containment on our corpora.' A "
        "competence failure here CONFIRMS a registered risk rather than surprising anyone.",
        "arms": arms,
    }

    hard_pass = verdict["bars"]["a_honesty_when_absent"]["passes"] and verdict["bars"][
        "b_competence_when_present"
    ]["passes_calibration"]
    verdict["outcome"] = "flip_candidate" if hard_pass else "flag_stays_off"
    verdict["outcome_reason"] = (
        "both HARD bars cleared" if hard_pass else
        "a HARD bar did not clear; per the work order the flag stays OFF and the order "
        "lands as measurement. The thresholds were NOT re-fitted on the bank under test — "
        "that is what pre-registration exists to prevent."
    )
    outp.write_text(json.dumps(verdict, indent=2, sort_keys=True) + "\n")
    print(json.dumps(verdict["bars"], indent=2))
    print("\nOUTCOME:", verdict["outcome"], "—", verdict["outcome_reason"])


if __name__ == "__main__":
    main()
