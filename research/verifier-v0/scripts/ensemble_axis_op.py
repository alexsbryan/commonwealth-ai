#!/usr/bin/env python3
"""M axis, part 2: the two PRE-SPECIFIED parameter-free combiners, and the
operating-point reading the gate actually cares about.

Part 1 tested equal-weight rank-average and it lost (-0.0346, CI [-0.0505,
-0.0201]). But miss-overlap showed real complementary signal: rung-1000 and
the incumbent between them catch 60/78 while rung alone catches 54/78. So the
question part 1 cannot answer is whether ANY combiner reaches that ceiling.

Exactly two more rules are tested, both chosen before looking and both
parameter-free -- this is not a combiner sweep, which on n=222 would be
fitting the instrument:
  MIN  = most-suspicious-wins (the union rule the miss-overlap ceiling implies)
  MAX  = most-confident-wins  (its dual; included so MIN is not cherry-picked)

And the metric moves to the gate's own terms: catch on ungrounded at a false
alarm rate MATCHED to rung-1000's, on the control bank's own grounded items.
AUC is a ranking metric; a fixed-tau gate lives or dies at one operating point.
"""
import json, bisect, random
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent / "runs" / "headroom"
def load(p):
    return {d["id"]: d for d in (json.loads(l) for l in open(HERE/p) if l.strip())}

ctl_r, ctl_v = load("control_joined_scored.jsonl"), load("vanilla4b_control_joined_scored.jsonl")
ids = sorted(set(ctl_r) & set(ctl_v))
lab = {i: ctl_r[i]["label"] for i in ids}
G = [i for i in ids if lab[i]=="grounded"]; U = [i for i in ids if lab[i]=="ungrounded"]

JUDGES = {
    "incumbent":  lambda i: ctl_r[i]["incumbent_max_support"],
    "rung-1000":  lambda i: ctl_r[i]["our_margin"],
    "vanilla-4b": lambda i: ctl_v[i]["our_margin"],
}
def ranks(vals):
    order = sorted(range(len(vals)), key=lambda i: vals[i]); r=[0.0]*len(vals); i=0
    while i < len(order):
        j=i
        while j+1 < len(order) and vals[order[j+1]] == vals[order[i]]: j+=1
        for k in range(i,j+1): r[order[k]] = (i+j)/2.0
        i=j+1
    m = max(1.0, len(vals)-1.0); return [x/m for x in r]
R = {n: dict(zip(ids, ranks([f(i) for i in ids]))) for n,f in JUDGES.items()}

def auc(pos, neg):
    ns = sorted(neg); n=0.0
    for p in pos:
        b = bisect.bisect_left(ns,p); t = bisect.bisect_right(ns,p)-b
        n += b + 0.5*t
    return n/(len(pos)*len(neg))

RULES = {
    "mean": lambda vs: sum(vs)/len(vs),
    "min ": min,   # most-suspicious-wins (union)
    "max ": max,   # most-confident-wins
}

def catch_at_matched_fa(score, target_fa):
    """Threshold set so FA on grounded == target_fa; return catch on ungrounded."""
    gs = sorted(score(i) for i in G)
    k = int(round(target_fa*len(G)))                 # this many grounded flagged
    tau = gs[k-1] if k>0 else float("-inf")          # flag score <= tau
    caught = sum(1 for i in U if score(i) <= tau)
    fa     = sum(1 for i in G if score(i) <= tau)
    return caught/len(U), fa/len(G)

# rung-1000's own operating point at its native tau 0.5 on p_grounded
rung_fa = sum(1 for i in G if ctl_r[i]["our_max_p"] < 0.5)/len(G)
rung_catch = sum(1 for i in U if ctl_r[i]["our_max_p"] < 0.5)/len(U)
print(f"=== reference operating point: rung-1000 @ tau 0.5 ===")
print(f"  catch {rung_catch*100:.1f}%  FA {rung_fa*100:.1f}%  (n={len(U)} ungrounded / {len(G)} grounded)\n")

COMBOS = [("rung-1000","incumbent"), ("rung-1000","vanilla-4b"),
          ("incumbent","vanilla-4b"), ("rung-1000","incumbent","vanilla-4b")]
print("=== M axis: AUC and catch@matched-FA by combiner ===")
print(f"{'members':36s} {'rule':5s} {'AUC':>7} {'dAUC':>8} {'catch@FA':>9} {'dcatch':>8}")
base_auc = auc([R['rung-1000'][i] for i in G], [R['rung-1000'][i] for i in U])
best = None
for c in COMBOS:
    for rn, rf in RULES.items():
        s = lambda i, c=c, rf=rf: rf([R[m][i] for m in c])
        a = auc([s(i) for i in G], [s(i) for i in U])
        ct, fa = catch_at_matched_fa(s, rung_fa)
        d = ct - rung_catch
        print(f"{'+'.join(c):36s} {rn:5s} {a:.4f} {a-base_auc:+8.4f} {ct*100:8.1f}% {d*100:+7.1f}pt")
        if best is None or ct > best[0]: best = (ct, c, rn, a)
print(f"\nbaseline (rung-1000 alone, matched): catch {rung_catch*100:.1f}% @ FA {rung_fa*100:.1f}%")
ct, c, rn, a = best
print(f"best combiner on catch: {'+'.join(c)} [{rn.strip()}] -> {ct*100:.1f}% ({(ct-rung_catch)*100:+.1f}pt)")

# bootstrap the best combiner's catch delta
random.seed(17); B=2000; deltas=[]
sbest = lambda i: RULES[rn]([R[m][i] for m in c])
for _ in range(B):
    bg=[random.choice(G) for _ in G]; bu=[random.choice(U) for _ in U]
    gs=sorted(sbest(i) for i in bg); k=int(round(rung_fa*len(bg)))
    tau = gs[k-1] if k>0 else float("-inf")
    e = sum(1 for i in bu if sbest(i)<=tau)/len(bu)
    b = sum(1 for i in bu if ctl_r[i]["our_max_p"]<0.5)/len(bu)
    deltas.append(e-b)
deltas.sort(); lo,hi = deltas[int(.025*B)], deltas[int(.975*B)]
print(f"bootstrap {B}x: catch delta {(ct-rung_catch)*100:+.1f}pt  95% CI [{lo*100:+.1f}, {hi*100:+.1f}]pt")
print(f"verdict: {'PASSES (CI excludes 0)' if lo>0 else 'KILLED (CI includes 0)'}")

# the ceiling the miss-overlap implies, at this same matched FA
print("\n=== oracle ceiling: what a PERFECT combiner could reach at this FA ===")
def flagged(name):
    f=JUDGES[name]; gs=sorted(f(i) for i in G); k=int(round(rung_fa*len(G)))
    tau = gs[k-1] if k>0 else float("-inf")
    return {i for i in U if f(i)<=tau}
fl = {n: flagged(n) for n in JUDGES}
for n,s in fl.items(): print(f"  {n:11s} catches {len(s):3d}/{len(U)} = {len(s)/len(U)*100:.1f}%")
union2 = fl["rung-1000"] | fl["vanilla-4b"]
union3 = union2 | fl["incumbent"]
print(f"  ORACLE union(rung,vanilla)  {len(union2)}/{len(U)} = {len(union2)/len(U)*100:.1f}%  (+{(len(union2)/len(U)-rung_catch)*100:.1f}pt over rung)")
print(f"  ORACLE union(all three)     {len(union3)}/{len(U)} = {len(union3)/len(U)*100:.1f}%  (+{(len(union3)/len(U)-rung_catch)*100:.1f}pt over rung)")
print("  (oracle = an item counts as caught if ANY judge flags it at ITS OWN matched-FA tau;")
print("   unreachable without knowing which judge to trust per item -- it is a bound, not a method)")
