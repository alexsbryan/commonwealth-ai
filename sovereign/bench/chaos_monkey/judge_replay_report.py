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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", required=True)
    ap.add_argument("--verdicts", action="append", required=True, metavar="NAME=PATH")
    ap.add_argument("--tau", type=float, default=0.9, help="operating point for delta tables")
    ap.add_argument("--json", help="write the full report as JSON")
    args = ap.parse_args()

    cases = read_jsonl(args.cases)
    cases_by_id = {c["case_id"]: c for c in cases}

    report = {"cases": args.cases, "arms": {}}
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
            "stability": stability(verdicts),
        }
        report["arms"][name] = arm

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
            print(f"  specifics_scan labeled items: {ok}/{len(sc)} correct, {cnj} could-not-judge")
            for i in sc:
                if i["ok"] is not True:
                    print(f"    {'?' if i['ok'] is None else 'X'} {i['label']:<15} {i['match']!r} "
                          f"flagged={i['flagged_in_replay']}")
        st = arm["stability"]
        if st["cases_with_repeats"]:
            print(f"  repeat stability: {st['cases_with_repeats']} cases repeated, "
                  f"max vp spread {st['max_vp_spread']:.6f}")

    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(report, fh, indent=2, ensure_ascii=False)
        print(f"\nwrote {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
