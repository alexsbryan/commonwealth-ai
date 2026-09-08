#!/usr/bin/env python3
"""Read the judged revisions and report delta -- plus the quantity that turned
out to matter more, p_damage.

Everything here is derived IN-RUN: tau comes from this run's cal_grounded
margins, catch from this run's cal_ungrounded. Nothing depends on transferring
a stored threshold across runs (headroom_study early-exits, so its recorded
margins are maxima over chunk prefixes -- see judge_revisions.py header).
"""
import json, math, sys, statistics
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT/"scripts"))
from delta_witness import witness as w_new

def wilson(k, n, z=1.96):
    if n == 0: return (float("nan"), float("nan"))
    p = k/n; d = 1+z*z/n
    c = (p+z*z/(2*n))/d
    h = z*math.sqrt(p*(1-p)/n + z*z/(4*n*n))/d
    return c-h, c+h

J = [json.loads(l) for l in open(ROOT/"runs/delta/judged_fa20.jsonl") if l.strip()]
by = {}
for r in J: by.setdefault(r["set"], []).append(r)
for k in by: print(f"{k:15s} n={len(by[k])}  margin=None on {sum(1 for r in by[k] if r['margin'] is None)}")
print()

FA = 0.20
calg = [r["margin"] for r in by.get("cal_grounded", []) if r["margin"] is not None]
calu = [r["margin"] for r in by.get("cal_ungrounded", []) if r["margin"] is not None]
gs = sorted(calg)
tau = gs[max(0, int(round(FA*len(gs)))-1)]
catch = sum(1 for m in calu if m <= tau)/len(calu)
fa_act = sum(1 for m in calg if m <= tau)/len(calg)
print(f"=== in-run operating point (tau from {len(calg)} cal_grounded, catch on {len(calu)} cal_ungrounded) ===")
print(f"  tau = {tau:+.3f}   FA = {fa_act:.1%}   catch = {catch:.1%}")
print()

revs = {r["id"]: r for r in by.get("revision", []) if r["margin"] is not None}
orig = {r["id"]: r for r in by.get("fa_original", []) if r["margin"] is not None}
pair = [(i, orig[i]["margin"], revs[i]["margin"]) for i in revs if i in orig]
print(f"=== the loop's actual behaviour on {len(pair)} false alarms ===")
o_flag = sum(1 for _, o, _ in pair if o <= tau)
r_flag = sum(1 for _, _, v in pair if v <= tau)
print(f"  originals still flagged at in-run tau : {o_flag}/{len(pair)} = {o_flag/len(pair):.1%}")
print(f"  REVISIONS flagged (loop would churn)  : {r_flag}/{len(pair)} = {r_flag/len(pair):.1%}")
lo, hi = wilson(r_flag, len(pair))
print(f"      95% CI [{lo:.1%}, {hi:.1%}]")
shifts = [v-o for _, o, v in pair]
print(f"  paired margin shift: median {statistics.median(shifts):+.2f}  mean {statistics.mean(shifts):+.2f}")
print(f"  revisions that ESCAPED the flag      : {sum(1 for _,o,v in pair if o<=tau<v)}/{o_flag}")
print()

# damage + delta
wit = json.load(open(ROOT/"runs/delta/revisions_witnessed.json"))
wmap = {w["id"]: w for w in wit}
chk = [i for i, _, _ in pair if wmap.get(i, {}).get("witness_new", {}).get("checkable")]
dmg = [i for i in chk if wmap[i]["witness_new"]["damaged"]]
print(f"=== damage (corrected witness: value asserted, absent from evidence AND from the original) ===")
lo_d, hi_d = wilson(len(dmg), len(chk))
print(f"  p_damage = {len(dmg)}/{len(chk)} = {len(dmg)/len(chk):.2%}   95% CI [{lo_d:.2%}, {hi_d:.2%}]")
print()
print("=== delta = P(verifier flags the revision | revision is damaged) ===")
if len(dmg) < 10:
    fl = sum(1 for i in dmg if revs[i]["margin"] <= tau)
    print(f"  COULD NOT JUDGE (ARCH §18.1): only {len(dmg)} damaged item(s); "
          f"{fl} of them flagged. n is too small for a rate.")
    print(f"  The reason IS the finding: damage is too rare to estimate its detectability.")
else:
    fl = sum(1 for i in dmg if revs[i]["margin"] <= tau)
    lo_x, hi_x = wilson(fl, len(dmg))
    print(f"  delta = {fl}/{len(dmg)} = {fl/len(dmg):.1%}   95% CI [{lo_x:.1%}, {hi_x:.1%}]")
print()

print("=== the attractor, recomputed with MEASURED damage instead of the assumed 1.0 ===")
print("  b* = d*FA / (catch + d*FA)")
print(f"  {'d':>22} {'b*':>8}")
for d, lbl in ((1.0, "1.0 (§9 assumption)"), (len(dmg)/len(chk), "measured"), (hi_d, "CI upper bound")):
    print(f"  {lbl:>22} {d*fa_act/(catch + d*fa_act):8.2%}")
print(f"  {'starting defect rate':>22} {0.09:8.2%}")
