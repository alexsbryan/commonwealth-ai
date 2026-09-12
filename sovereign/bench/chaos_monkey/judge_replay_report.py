#!/usr/bin/env python3
"""Score judge-replay verdicts: per-register operating curves, deltas vs the
recorded verdicts, and the naive baseline beside every number.

Inputs: the pinned case set (judge_replay_cases.py) and one or more verdict
files from `svrn bench judge-replay` (each stamped with the build's register
fingerprint). Two arms — e.g. `main=...` and `landC=...` — make this the
offline A/B the live 30-40 min adversarial arms priced until now.

NUMBERS POLICY (E-naive-baseline): every rate is printed with its n and with
the naive always-flag / always-clear ceilings on the same label set. A rate
that beats neither naive is reported as exactly that.

LABEL SEMANTICS are support-in-view (see judge_replay_cases.py). The
operating curve treats `not_supported_in_view` as the flag class:
  catch-rate  = flagged / negatives   (sensitivity)
  clear-rate  = cleared / positives   (specificity)
For chunk_judge rows the replay stores SUPPORT (the register's own
convention); it is converted here — vp = 1 - support — in ONE place.

Usage:
    judge_replay_report.py --cases judge_replay_cases_v1.jsonl \\
        --verdicts main=target/judge-replay/main.jsonl \\
        [--verdicts landC=target/judge-replay/landc.jsonl] [--json out.json]
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# The four-verdict line this report ends with, so a runner reads the verdict
# rather than the tables above it (`scripts/lib/judgement.py`).
REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "scripts" / "lib"))
from judgement import emit as emit_judgement  # noqa: E402

TAU_GRID = [0.50, 0.60, 0.70, 0.80, 0.85, 0.90, 0.95, 0.98]

NEG = "not_supported_in_view"
POS = "supported_in_view"


def read_jsonl(path):
    out = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def vp_of(row, register):
    """The verdict list for one case; chunk_judge support -> vp here, once."""
    vps = [v for v in (row.get("vp") or [])]
    if register == "chunk_judge":
        vps = [None if v is None else 1.0 - v for v in vps]
    return vps


def first(vps):
    return vps[0] if vps else None


def fmt(x, n=None):
    if x is None:
        return "could-not-judge"
    s = f"{x:.3f}"
    return f"{s} (n={n})" if n is not None else s


def curve(cases_by_id, rows, register):
    labeled = [
        (cases_by_id[r["case_id"]], r)
        for r in rows
        if r.get("register") == register
        and r["case_id"] in cases_by_id
        and cases_by_id[r["case_id"]].get("label") in (NEG, POS)
    ]
    neg = [(c, r) for c, r in labeled if c["label"] == NEG]
    pos = [(c, r) for c, r in labeled if c["label"] == POS]
    out = {"n_neg": len(neg), "n_pos": len(pos), "points": [], "could_not_judge": 0}
    for c, r in labeled:
        if first(vp_of(r, register)) is None:
            out["could_not_judge"] += 1
    for tau in TAU_GRID:
        nf = [1 for c, r in neg if (v := first(vp_of(r, register))) is not None and v >= tau]
        pc = [1 for c, r in pos if (v := first(vp_of(r, register))) is not None and v < tau]
        n_scored_neg = sum(1 for c, r in neg if first(vp_of(r, register)) is not None)
        n_scored_pos = sum(1 for c, r in pos if first(vp_of(r, register)) is not None)
        out["points"].append(
            {
                "tau": tau,
                "catch_rate": (sum(nf) / n_scored_neg) if n_scored_neg else None,
                "n_neg_scored": n_scored_neg,
                "clear_rate": (sum(pc) / n_scored_pos) if n_scored_pos else None,
                "n_pos_scored": n_scored_pos,
            }
        )
    # Pinned adversarial specimens: must be flagged at the operating point.
    out["pinned"] = [
        {
            "case_id": c["case_id"],
            "vp": first(vp_of(r, register)),
            "recorded_vp": (c.get("recorded") or {}).get("vp"),
        }
        for c, r in labeled
        if c.get("must_refuse_at_operating_point")
    ]
    return out


def deltas(cases_by_id, rows, register, tau):
    """Verdict flips vs the RECORDED run, over every case that has a recorded
    vp (labeled or not) — the seed list for the claim-by-claim read."""
    flips = {"newly_cleared": [], "newly_flagged": [], "n_compared": 0}
    for r in rows:
        if r.get("register") != register:
            continue
        c = cases_by_id.get(r["case_id"])
        if not c:
            continue
        rec = (c.get("recorded") or {}).get("vp")
        rec_tau = (c.get("recorded") or {}).get("tau") or tau
        v = first(vp_of(r, register))
        if rec is None or v is None:
            continue
        flips["n_compared"] += 1
        was, now = rec >= rec_tau, v >= tau
        if was and not now:
            flips["newly_cleared"].append(
                {"case_id": r["case_id"], "recorded_vp": rec, "vp": v, "label": c.get("label")}
            )
        elif now and not was:
            flips["newly_flagged"].append(
                {"case_id": r["case_id"], "recorded_vp": rec, "vp": v, "label": c.get("label")}
            )
    return flips


def scan_report(cases_by_id, rows):
    """Labeled-item outcomes for the generative scan register."""
    out = []
    for r in rows:
        if r.get("register") != "specifics_scan":
            continue
        c = cases_by_id.get(r["case_id"])
        if not c:
            continue
        replays = r.get("scan_items") or []
        items = replays[0] if replays and replays[0] is not None else None
        for li in c.get("labeled_items") or []:
            hit = None if items is None else any(li["match"].lower() in it.lower() for it in items)
            ok = None if hit is None else (hit if li["label"] == "should_flag" else not hit)
            out.append(
                {
                    "case_id": r["case_id"],
                    "match": li["match"],
                    "label": li["label"],
                    "flagged_in_replay": hit,
                    "ok": ok,
                }
            )
    return out


def scan_by_class(items):
    """Split the labeled scan items by LABEL CLASS.

    THE WHOLE POINT (ARCH principle 7 — a judge is never tuned in one
    direction). Until 2026-09-12 this report printed one aggregate,
    `N/M correct`, and the frozen d3a candidate (commit 17ac931b8) is why that
    is not enough: it scored 6/10 against main's 3/10 while LOSING a
    should_flag catch, so the number that improved was the number a reader
    saw. A single aggregate cannot tell a cured false positive from a
    surrendered catch, and those are the two directions a register trade is
    made of.
    """
    out = {}
    for cls in ("should_flag", "should_not_flag"):
        rows = [i for i in items if i["label"] == cls]
        out[cls] = {
            "n": len(rows),
            "ok": sum(1 for i in rows if i["ok"] is True),
            "could_not_judge": sum(1 for i in rows if i["ok"] is None),
            "wrong": [i["match"] for i in rows if i["ok"] is False],
        }
    return out


def compare_scan(base_items, cand_items):
    """Both directions of a scan register trade, item by item.

    Keyed on (case_id, match) rather than on position: the two arms may carry
    different case sets, and a positional join would silently pair unrelated
    items.
    """
    b = {(i["case_id"], i["match"]): i for i in base_items}
    c = {(i["case_id"], i["match"]): i for i in cand_items}
    shared = sorted(set(b) & set(c))
    out = {
        "n_compared": len(shared),
        "catches_lost": [],
        "catches_gained": [],
        "false_positives_cured": [],
        "false_positives_added": [],
        "could_not_judge": 0,
    }
    for k in shared:
        bi, ci = b[k], c[k]
        if bi["ok"] is None or ci["ok"] is None:
            out["could_not_judge"] += 1
            continue
        if bi["ok"] == ci["ok"]:
            continue
        where = {"case_id": k[0], "match": k[1]}
        if bi["label"] == "should_flag":
            (out["catches_lost"] if bi["ok"] else out["catches_gained"]).append(where)
        else:
            (out["false_positives_added"] if bi["ok"] else out["false_positives_cured"]).append(where)
    return out


def compare_curve(cases_by_id, base_rows, cand_rows, register, tau):
    """Both directions of a scalar register trade at the OPERATING point.

    A catch lost on a labeled negative is the kill condition the 2026-08-14
    calibration names ("(c)-class loss: YES — lessa-watch... flagged by main,
    cleared by C. Kill condition hit"). A false positive cured on a labeled
    positive is what a candidate is usually bought for. Both are counted; they
    are never netted, because the average of the two is the number that hides
    the trade.
    """
    def by_case(rows):
        return {
            r["case_id"]: first(vp_of(r, register))
            for r in rows
            if r.get("register") == register
        }

    b, c = by_case(base_rows), by_case(cand_rows)
    out = {
        "n_compared": 0,
        "catches_lost": [],
        "catches_gained": [],
        "false_positives_cured": [],
        "false_positives_added": [],
        "could_not_judge": 0,
    }
    for case_id in sorted(set(b) & set(c)):
        case = cases_by_id.get(case_id)
        if not case or case.get("label") not in (NEG, POS):
            continue
        out["n_compared"] += 1
        bv, cv = b[case_id], c[case_id]
        if bv is None or cv is None:
            out["could_not_judge"] += 1
            continue
        b_flag, c_flag = bv >= tau, cv >= tau
        if b_flag == c_flag:
            continue
        where = {"case_id": case_id, "base_vp": round(bv, 4), "cand_vp": round(cv, 4)}
        if case["label"] == NEG:
            (out["catches_lost"] if b_flag else out["catches_gained"]).append(where)
        else:
            (out["false_positives_cured"] if b_flag else out["false_positives_added"]).append(where)
    return out


def compare_verdict(cmp_blocks):
    """-> (verdict, reason) for the whole comparison, worst first.

    ONE RULE, and it is the calibration's own: a candidate that surrenders a
    catch at the operating point is REFUSED whatever it cures. Both 2026-08-14
    refusals were exactly this shape, and both had a number moving the other
    way to point at — land C cured two false positives, d3a doubled the scan's
    aggregate. Netting them would have passed both.
    """
    lost = sum(len(c["catches_lost"]) for c in cmp_blocks.values())
    gained = sum(len(c["catches_gained"]) for c in cmp_blocks.values())
    cured = sum(len(c["false_positives_cured"]) for c in cmp_blocks.values())
    added = sum(len(c["false_positives_added"]) for c in cmp_blocks.values())
    compared = sum(c["n_compared"] for c in cmp_blocks.values())
    cnj = sum(c["could_not_judge"] for c in cmp_blocks.values())
    if compared == 0:
        return ("never-ran", "the two arms share no labeled case — nothing was compared")
    if lost:
        return ("failed",
                f"the candidate surrenders {lost} catch(es) the base makes at the operating "
                f"point (it cures {cured} false positive(s) and gains {gained} catch(es); the "
                "two are reported, never netted)")
    if cnj and cnj == compared:
        return ("could-not-judge",
                f"all {compared} compared item(s) were could-not-judge on one arm or the other")
    if cnj:
        return ("could-not-judge",
                f"{cnj} of {compared} compared item(s) could not be scored on one arm; no catch "
                "was surrendered among the rest")
    return ("passed",
            f"{compared} labeled item(s) compared: no catch surrendered, {cured} false "
            f"positive(s) cured, {gained} catch(es) gained, {added} false positive(s) added")


def batched_report(cases_by_id, rows):
    """Claim-level outcomes for the batched register (order audit-economy D1).

    ASYMMETRIC-TRUST SEMANTICS: production (D2 candidate) clears on batch
    "supported" and falls through to the calibrated per-claim judge on
    "unsupported" OR parse-gap. The only quality-affecting error class is
    therefore FALSE-SUPPORTED — batch says supported where the calibrated
    register (or a hand label) says the claim must be flagged. Everything
    else lands on the calibrated path and inherits its calibration.
    """
    out = {
        "n_cases": 0,
        "n_claims": 0,
        "parse_gaps": 0,
        "supported": 0,
        "repeat_disagreements": 0,
        # label-side (the D1 bar): batch verdict on hand-labeled claims
        "labeled_neg": 0,
        "labeled_neg_caught": 0,       # batch unsupported -> calibrated path (safe)
        "labeled_neg_false_supported": [],  # THE (c)-class hazard, listed claim by claim
        "labeled_neg_parse_gap": 0,    # falls through (safe)
        "labeled_pos": 0,
        "labeled_pos_cleared": 0,      # batch supported -> cleared without a calibrated call
        "labeled_pos_flagged": 0,      # falls through; calibrated register decides (cost, not quality)
        "labeled_pos_parse_gap": 0,
        # recorded-side: agreement with the production per-claim outcome
        "recorded_failed_batch_supported": [],  # would flip FLAG->CLEAR vs production; needs the read
        "recorded_passed_batch_unsupported": 0, # extra calibrated call; verdict unchanged
        "recorded_compared": 0,
    }
    for r in rows:
        if r.get("register") != "batched_support":
            continue
        c = cases_by_id.get(r["case_id"])
        if not c:
            continue
        reps = r.get("batched") or []
        if not reps or reps[0] is None:
            continue
        v0 = reps[0]
        out["n_cases"] += 1
        out["n_claims"] += len(v0)
        if any(rep != v0 for rep in reps[1:]):
            out["repeat_disagreements"] += 1
        labels = c.get("claim_labels") or []
        claims = c.get("claims") or []
        for i, v in enumerate(v0):
            lab = labels[i] if i < len(labels) else None
            if v is None:
                out["parse_gaps"] += 1
            elif v:
                out["supported"] += 1
            if lab == NEG:
                out["labeled_neg"] += 1
                if v is None:
                    out["labeled_neg_parse_gap"] += 1
                elif v:
                    out["labeled_neg_false_supported"].append(
                        {"case_id": r["case_id"], "claim_idx": i,
                         "claim": (claims[i] if i < len(claims) else "?")[:110]}
                    )
                else:
                    out["labeled_neg_caught"] += 1
            elif lab == POS:
                out["labeled_pos"] += 1
                if v is None:
                    out["labeled_pos_parse_gap"] += 1
                elif v:
                    out["labeled_pos_cleared"] += 1
                else:
                    out["labeled_pos_flagged"] += 1
        # Recorded production outcomes, aligned by claim text (claim_idx in the
        # ledger skips exempt rows, so index alignment is by content).
        rec_by_claim = {}
        for pc in (c.get("recorded") or {}).get("per_claim") or []:
            if pc.get("claim"):
                rec_by_claim[pc["claim"]] = pc
        for i, v in enumerate(v0):
            if v is None or i >= len(claims):
                continue
            pc = rec_by_claim.get(claims[i])
            if pc is None or pc.get("failed") is None:
                continue
            out["recorded_compared"] += 1
            if pc["failed"] and v:
                out["recorded_failed_batch_supported"].append(
                    {"case_id": r["case_id"], "claim_idx": i, "claim": claims[i][:110],
                     "recorded_vp": pc.get("vp"), "recorded_mechanism": pc.get("mechanism"),
                     "label": labels[i] if i < len(labels) else None}
                )
            elif (not pc["failed"]) and (not v):
                out["recorded_passed_batch_unsupported"] += 1
    return out


def stability(rows):
    """Within-file repeat spread; the mechanical facet must be bit-stable."""
    worst = 0.0
    n_multi = 0
    for r in rows:
        vps = [v for v in (r.get("vp") or []) if v is not None]
        if len(vps) > 1:
            n_multi += 1
            worst = max(worst, max(vps) - min(vps))
    return {"cases_with_repeats": n_multi, "max_vp_spread": worst if n_multi else None}


# The 2026-08-14 refusal, frozen — this report's negative control.
#
# ARCH principle 5: a check nobody has watched fail is not a check. Commit
# 17ac931b8 is a specifics-scan candidate that was REFUSED at the 3/3
# should_flag bar, and its verdicts sit in the tree beside main's. It is the
# ideal control because its AGGREGATE IMPROVED — 6/10 against main's 3/10 —
# while it lost the Kane-bridge catch, so a report that prints one number
# passes it and a report that reads both directions cannot.
CONTROL_CASES = "sovereign/bench/chaos_monkey/judge_replay_cases_v1.jsonl"
CONTROL_BASE = "sovereign/bench/chaos_monkey/results/judge_replay_20260814_main.verdicts.jsonl"
CONTROL_CAND = "sovereign/bench/chaos_monkey/results/judge_replay_20260814_d3a_scan.verdicts.jsonl"
CONTROL_LOST = "bridges both sides"


def self_test() -> int:
    """Both arms of the control: the refused candidate must fail, and an arm
    compared against ITSELF must pass. Without the second, a report that
    answered `failed` unconditionally would pass the first."""
    ok = True

    def load(path):
        return [r for r in read_jsonl(REPO / path) if r.get("kind") == "verdict"]

    cases_by_id = {c["case_id"]: c for c in read_jsonl(REPO / CONTROL_CASES)}
    b_items = scan_report(cases_by_id, load(CONTROL_BASE))
    c_items = scan_report(cases_by_id, load(CONTROL_CAND))

    # The premise the control rests on, asserted rather than assumed: the
    # candidate's aggregate really is BETTER. If a future edit to the case set
    # breaks that, this control stops being the one it claims to be.
    b_ok = sum(1 for i in b_items if i["ok"] is True)
    c_ok = sum(1 for i in c_items if i["ok"] is True)
    if c_ok <= b_ok:
        print(f"  FAIL  the control's premise is gone: candidate aggregate {c_ok}/{len(c_items)} "
              f"is not better than base {b_ok}/{len(b_items)}, so passing it proves nothing")
        ok = False
    else:
        print(f"  ok    the candidate's aggregate is better ({c_ok} vs {b_ok}) — the trap is live")

    blocks = {"specifics_scan": compare_scan(b_items, c_items)}
    verdict, reason = compare_verdict(blocks)
    lost = [x["match"] for x in blocks["specifics_scan"]["catches_lost"]]
    if verdict != "failed":
        print(f"  FAIL  the refused candidate reports {verdict}, not failed: {reason}")
        ok = False
    elif CONTROL_LOST not in " | ".join(lost):
        print(f"  FAIL  the candidate is failed, but for the wrong item: {lost}")
        ok = False
    else:
        print(f"  ok    the refused candidate is caught, by name: {lost}")

    same, same_reason = compare_verdict({"specifics_scan": compare_scan(b_items, b_items)})
    if same != "passed":
        print(f"  FAIL  an arm compared against itself reports {same}: {same_reason}")
        ok = False
    else:
        print("  ok    an arm compared against itself passes")

    if ok:
        emit_judgement("judge-replay-control", "passed",
                       "the 17ac931b8 candidate is still caught for the catch it surrendered, and "
                       "an arm against itself still passes")
        return 0
    emit_judgement("judge-replay-control", "failed",
                   "the report no longer catches the refused 17ac931b8 candidate, or flags an arm "
                   "against itself — a register change could now land unpriced")
    return 1


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", required=True)
    ap.add_argument("--verdicts", action="append", required=True, metavar="NAME=PATH")
    ap.add_argument("--tau", type=float, default=0.9, help="operating point for delta tables")
    ap.add_argument("--json", help="write the full report as JSON")
    ap.add_argument("--self-test", action="store_true",
                    help="run the frozen 17ac931b8 negative control (no cases or arms needed)")
    args = ap.parse_args()

    cases = read_jsonl(args.cases)
    cases_by_id = {c["case_id"]: c for c in cases}

    report = {"cases": args.cases, "arms": {}}
    arm_rows = {}
    for spec in args.verdicts:
        name, _, path = spec.partition("=")
        rows = read_jsonl(path)
        header = next((r for r in rows if r.get("kind") == "header"), {})
        verdicts = [r for r in rows if r.get("kind") == "verdict"]
        arm = {
            "path": path,
            "header": {
                k: header.get(k)
                for k in ("system_turn_fnv", "model", "base_url", "engine", "repeat", "ts")
            },
            "per_claim_judge": {
                "curve": curve(cases_by_id, verdicts, "per_claim_judge"),
                "deltas_vs_recorded": deltas(cases_by_id, verdicts, "per_claim_judge", args.tau),
            },
            "chunk_judge": {"curve": curve(cases_by_id, verdicts, "chunk_judge")},
            "specifics_scan": {"labeled_items": scan_report(cases_by_id, verdicts)},
            "batched_support": batched_report(cases_by_id, verdicts),
            "stability": stability(verdicts),
        }
        report["arms"][name] = arm
        arm_rows[name] = verdicts

        pj = arm["per_claim_judge"]["curve"]
        print(f"\n=== arm {name} (register fingerprint {arm['header']['system_turn_fnv']}, "
              f"engine {arm['header']['engine']}) ===")
        print(f"per_claim_judge labels: {pj['n_neg']} negative / {pj['n_pos']} positive; "
              f"could-not-judge {pj['could_not_judge']}")
        print("  NAIVE BASELINES on this label set: always-flag catch=1.000 clear=0.000; "
              "always-clear catch=0.000 clear=1.000")
        print("  tau    catch-rate (neg)      clear-rate (pos)")
        for p in pj["points"]:
            mark = " <- operating point" if abs(p["tau"] - args.tau) < 1e-9 else ""
            print(f"  {p['tau']:.2f}   {fmt(p['catch_rate'], p['n_neg_scored']):<20} "
                  f"{fmt(p['clear_rate'], p['n_pos_scored']):<20}{mark}")
        if pj["pinned"]:
            print("  pinned adversarial specimens (must be flagged at the operating point):")
            for s in pj["pinned"]:
                v = s["vp"]
                verdict = "could-not-judge" if v is None else ("FLAGGED" if v >= args.tau else "CLEARED (FAIL)")
                print(f"    {s['case_id']:<34} vp={fmt(v)} recorded_vp={s['recorded_vp']} -> {verdict}")
        d = arm["per_claim_judge"]["deltas_vs_recorded"]
        print(f"  deltas vs recorded (n={d['n_compared']}): newly_cleared {len(d['newly_cleared'])} "
              f"(EVERY one needs the (a)/(b)/(c) read), newly_flagged {len(d['newly_flagged'])}")
        sc = arm["specifics_scan"]["labeled_items"]
        if sc:
            ok = sum(1 for i in sc if i["ok"] is True)
            cnj = sum(1 for i in sc if i["ok"] is None)
            cls = scan_by_class(sc)
            arm["specifics_scan"]["by_class"] = cls
            # BY CLASS, never only the aggregate: 6/10 with a catch lost and
            # 6/10 with every catch kept are different results, and the frozen
            # d3a arm is the case where the aggregate moved the flattering way.
            print(f"  specifics_scan labeled items: {ok}/{len(sc)} correct, {cnj} could-not-judge")
            print(f"    should_flag     caught {cls['should_flag']['ok']}/{cls['should_flag']['n']}"
                  + (f"  (could-not-judge {cls['should_flag']['could_not_judge']})"
                     if cls['should_flag']['could_not_judge'] else ""))
            print(f"    should_not_flag cleared {cls['should_not_flag']['ok']}/{cls['should_not_flag']['n']}"
                  + (f"  (could-not-judge {cls['should_not_flag']['could_not_judge']})"
                     if cls['should_not_flag']['could_not_judge'] else ""))
            for i in sc:
                if i["ok"] is not True:
                    print(f"    {'?' if i['ok'] is None else 'X'} {i['label']:<15} {i['match']!r} "
                          f"flagged={i['flagged_in_replay']}")
        st = arm["stability"]
        if st["cases_with_repeats"]:
            print(f"  repeat stability: {st['cases_with_repeats']} cases repeated, "
                  f"max vp spread {st['max_vp_spread']:.6f}")
        b = arm["batched_support"]
        if b["n_cases"]:
            print(f"  batched_support: {b['n_cases']} batch cases, {b['n_claims']} claims; "
                  f"supported {b['supported']}, parse gaps {b['parse_gaps']}, "
                  f"repeat disagreements {b['repeat_disagreements']}")
            print("  NAIVE BASELINES: trust-nothing (all claims fall through) = today's cost, "
                  "zero quality delta; trust-everything = clear rate 1.0, catch rate 0.0")
            fs = b["labeled_neg_false_supported"]
            print(f"    labeled negatives (n={b['labeled_neg']}): caught {b['labeled_neg_caught']}, "
                  f"parse-gap->calibrated {b['labeled_neg_parse_gap']}, "
                  f"FALSE-SUPPORTED {len(fs)} <- the (c)-class bar is ZERO")
            for x in fs:
                print(f"      X {x['case_id']} c{x['claim_idx']}: {x['claim']!r}")
            print(f"    labeled positives (n={b['labeled_pos']}): cleared-without-calibrated-call "
                  f"{b['labeled_pos_cleared']}, fell-through {b['labeled_pos_flagged']}, "
                  f"parse-gap {b['labeled_pos_parse_gap']}")
            rf = b["recorded_failed_batch_supported"]
            print(f"    vs recorded production outcomes (n={b['recorded_compared']}): "
                  f"FLAG->CLEAR flips {len(rf)} (EVERY one needs the (a)/(b)/(c) read); "
                  f"pass->fall-through {b['recorded_passed_batch_unsupported']} (cost only)")
            for x in rf:
                print(f"      ? {x['case_id']} c{x['claim_idx']} label={x['label']} "
                      f"recorded_vp={x['recorded_vp']}: {x['claim']!r}")

    # ── the trade, in both directions ───────────────────────────────────
    #
    # Two arms used to print two blocks and leave the comparison to a reader.
    # That is the state ARCH principle 7 names: "reported in the permissive
    # direction only, a change is not failed, it is not judged". The base is
    # the FIRST --verdicts arm; every later one is a candidate against it.
    names = list(report["arms"])
    verdict, reason = None, None
    if len(names) >= 2:
        base_name = names[0]
        base_rows = arm_rows[base_name]
        for cand_name in names[1:]:
            cand_rows = arm_rows[cand_name]
            blocks = {}
            for register in ("per_claim_judge", "chunk_judge"):
                blk = compare_curve(cases_by_id, base_rows, cand_rows, register, args.tau)
                if blk["n_compared"]:
                    blocks[register] = blk
            b_items = report["arms"][base_name]["specifics_scan"]["labeled_items"]
            c_items = report["arms"][cand_name]["specifics_scan"]["labeled_items"]
            if b_items and c_items:
                blocks["specifics_scan"] = compare_scan(b_items, c_items)
            report["arms"][cand_name]["vs_" + base_name] = blocks
            print(f"\n=== {cand_name} vs {base_name} (operating point tau={args.tau}) ===")
            if not blocks:
                print("  no register has labeled items on BOTH arms — nothing compared")
            for register, blk in blocks.items():
                print(f"  {register}: {blk['n_compared']} compared"
                      + (f", {blk['could_not_judge']} could-not-judge" if blk["could_not_judge"] else ""))
                print(f"    catches LOST      {len(blk['catches_lost'])}"
                      + ("   <- the kill condition" if blk["catches_lost"] else ""))
                for x in blk["catches_lost"]:
                    print(f"      X {x.get('case_id')} {x.get('match', '')!r} {x.get('base_vp', '')}"
                          f" -> {x.get('cand_vp', '')}")
                print(f"    catches gained    {len(blk['catches_gained'])}")
                print(f"    false pos cured   {len(blk['false_positives_cured'])}")
                print(f"    false pos added   {len(blk['false_positives_added'])}")
            verdict, reason = compare_verdict(blocks)
            report["arms"][cand_name]["verdict"] = {"verdict": verdict, "reason": reason}

    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(report, fh, indent=2, ensure_ascii=False)
        print(f"\nwrote {args.json}")

    # ONE arm is a reading, not a trade: there is nothing to price it against,
    # so the run says so rather than reporting a pass it did not earn.
    if verdict is None:
        emit_judgement("judge-replay-report", "never-ran",
                       f"{len(names)} arm(s) given; a register trade needs a base and a candidate, "
                       "so nothing was priced")
    else:
        emit_judgement("judge-replay-report", verdict, reason)
    return 0


if __name__ == "__main__":
    sys.exit(main())
