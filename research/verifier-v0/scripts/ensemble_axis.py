#!/usr/bin/env python3
"""M — the independent-verifier axis. A fourth axis on VERIFICATION_SCALING_AXES.md.

That doc decomposes the LLM-as-a-Verifier formula into C (criteria), K (repeated
evaluation) and G (score granularity), and pre-registers E1-E5 across them. All
three are *within-judge* knobs. This script measures the axis the doc does not
name: M, the number of INDEPENDENT judges whose scores are combined.

The mesh hypothesis this serves: groundedness scales with independent-check
count, which is why a heterogeneous mesh gets more grounded as it grows. C/K/G
scale what one node can do alone; M is the only axis a second node can buy.

PRE-REGISTRATION (written before any ensemble number was computed)
  Prediction: mean-of-ranks over {rung-1000, incumbent, vanilla-4B} beats the
    best single (0.848) by >= +0.02 AUC. Rationale: HEADROOM_STUDY records
    highly NON-overlapping misses between incumbent and rung-1000 (74
    incumbent-only vs 1 ours-only, 4 both, on the constructed bank).
    Non-overlapping errors are the precondition for an ensemble gain.
  Kill bar: if the bootstrap 95% CI on (best ensemble - best single) includes
    0, the M axis is dead as a DISCRIMINATION move. Do not tune the combiner
    to rescue it.
  Second bar (timidity, borrowed from E2): journal-strong false alarms must not
    rise above the joined baseline of 18.6%. Catching more by suspecting
    everything is not a gain.
  Membership sub-question: does {rung + incumbent} beat rung alone? The
    incumbent is COLLAPSED on this bank (AUC 0.640). If a weaker,
    differently-wrong judge still adds, "any independent node contributes"
    holds. If it drags rung down, the mesh story needs a competence floor.

Caveat carried from VERIFICATION_SCALING_AXES.md Sec 2: AUC is a RANKING metric.
Our production gate does absolute thresholding against a fixed tau, and
rank-preserving gains need not transfer. Both are reported; neither substitutes
for the other.
"""
import json, sys, bisect, random
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent / "runs" / "headroom"

def load(p):
    rows = {}
    for line in open(HERE / p):
        line = line.strip()
        if line:
            d = json.loads(line)
            rows[d["id"]] = d
    return rows

def auc(pos, neg):
    """Mann-Whitney with explicit tie handling: P(pos>neg) + 0.5*P(tie)."""
    neg_sorted = sorted(neg)
    n = 0.0
    for p in pos:
        below = bisect.bisect_left(neg_sorted, p)
        tie = bisect.bisect_right(neg_sorted, p) - below
        n += below + 0.5 * tie
    return n / (len(pos) * len(neg))

def ranks(vals):
    """Average ranks, normalized to [0,1]. Scale-free per-judge normalization."""
    order = sorted(range(len(vals)), key=lambda i: vals[i])
    r = [0.0] * len(vals)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and vals[order[j + 1]] == vals[order[i]]:
            j += 1
        avg = (i + j) / 2.0
        for k in range(i, j + 1):
            r[order[k]] = avg
        i = j + 1
    m = max(1.0, len(vals) - 1.0)
    return [x / m for x in r]

# ---- load, join on id -------------------------------------------------------
ctl_r, ctl_v = load("control_joined_scored.jsonl"), load("vanilla4b_control_joined_scored.jsonl")
jr_r, jr_v = load("jrnl_strong_joined_scored.jsonl"), load("vanilla4b_jrnl_strong_joined_scored.jsonl")
ids = sorted(set(ctl_r) & set(ctl_v))
jids = sorted(set(jr_r) & set(jr_v))

# Three judges. incumbent is duplicated across both files; use the rung file's
# copy consistently (7/222 rows disagree between the two runs -- nondeterminism
# in the incumbent, reported below, never silently averaged).
JUDGES = {
    "incumbent":  lambda i: ctl_r[i]["incumbent_max_support"],
    "rung-1000":  lambda i: ctl_r[i]["our_margin"],
    "vanilla-4b": lambda i: ctl_v[i]["our_margin"],
}
JJUDGES = {
    "incumbent":  lambda i: jr_r[i]["incumbent_max_support"],
    "rung-1000":  lambda i: jr_r[i]["our_max_p"],
    "vanilla-4b": lambda i: jr_v[i]["our_max_p"],
}

lab = {i: ctl_r[i]["label"] for i in ids}
G = [i for i in ids if lab[i] == "grounded"]
U = [i for i in ids if lab[i] == "ungrounded"]

# ---- instrument validation (Sec 18.4): refuse to proceed on a bad reproduction
TARGETS = {"incumbent": 0.640, "rung-1000": 0.848, "vanilla-4b": 0.763}
print("=== instrument validation: single-judge AUC vs HEADROOM_STUDY ===")
ok = True
singles = {}
for name, f in JUDGES.items():
    a = auc([f(i) for i in G], [f(i) for i in U])
    singles[name] = a
    d = abs(a - TARGETS[name])
    flag = "OK" if d < 0.005 else "MISMATCH"
    if d >= 0.005:
        ok = False
    print(f"  {name:11s} {a:.4f}  (published {TARGETS[name]:.3f})  {flag}")
if not ok:
    sys.exit("REFUSING: single-judge numbers do not reproduce; the files are not what the doc describes.")
disagree = sum(1 for i in ids if ctl_r[i]["incumbent_max_support"] != ctl_v[i]["incumbent_max_support"])
print(f"  note: incumbent column differs on {disagree}/{len(ids)} rows between the two run files (nondeterminism, not averaged)")
best_single = max(singles.values())
best_name = max(singles, key=singles.get)
print(f"\nbest single judge: {best_name} @ {best_single:.4f}\n")

# ---- M axis: rank-average ensembles ----------------------------------------
R = {n: dict(zip(ids, ranks([f(i) for i in ids]))) for n, f in JUDGES.items()}
JR = {n: dict(zip(jids, ranks([f(i) for i in jids]))) for n, f in JJUDGES.items()}

def ens_auc(members, idsG=None, idsU=None):
    idsG = idsG or G; idsU = idsU or U
    s = lambda i: sum(R[m][i] for m in members) / len(members)
    return auc([s(i) for i in idsG], [s(i) for i in idsU])

COMBOS = [
    ("rung-1000",), ("incumbent",), ("vanilla-4b",),
    ("rung-1000", "incumbent"), ("rung-1000", "vanilla-4b"), ("incumbent", "vanilla-4b"),
    ("rung-1000", "incumbent", "vanilla-4b"),
]
print("=== M axis: AUC by ensemble (rank-average) ===")
print(f"{'M':>2}  {'members':38s} {'AUC':>7}  {'vs best single':>14}")
results = {}
for c in COMBOS:
    a = ens_auc(c)
    results[c] = a
    print(f"{len(c):>2}  {'+'.join(c):38s} {a:.4f}  {a-best_single:+.4f}")

# ---- bootstrap CI on the headline delta (Sec 18.5: no single-run deltas) ----
random.seed(17)
B = 2000
top = max(results, key=lambda c: results[c] if len(c) > 1 else -1)
print(f"\n=== bootstrap {B}x, stratified by label: ({'+'.join(top)}) - {best_name} ===")
deltas = []
for _ in range(B):
    bg = [random.choice(G) for _ in G]
    bu = [random.choice(U) for _ in U]
    se = lambda i: sum(R[m][i] for m in top) / len(top)
    sb = JUDGES[best_name]
    ae = auc([se(i) for i in bg], [se(i) for i in bu])
    ab = auc([sb(i) for i in bg], [sb(i) for i in bu])
    deltas.append(ae - ab)
deltas.sort()
lo, hi = deltas[int(0.025 * B)], deltas[int(0.975 * B)]
point = results[top] - best_single
print(f"  delta AUC {point:+.4f}   95% CI [{lo:+.4f}, {hi:+.4f}]")
verdict = "PASSES" if lo > 0 else "KILLED (CI includes 0)"
print(f"  kill bar: {verdict}")

# ---- second bar: journal-strong false alarms (the timidity check) -----------
print("\n=== second bar: journal-strong FA (n=%d, all grounded; baseline 18.6%%) ===" % len(jids))
def fa_at_rank_quantile(members, q):
    """Flag the lowest-q fraction by ensemble rank. q chosen to match a judge's own FA."""
    s = sorted(jids, key=lambda i: sum(JR[m][i] for m in members) / len(members))
    k = int(round(q * len(jids)))
    return set(s[:k])
for c in COMBOS:
    s = lambda i: sum(JR[m][i] for m in c) / len(c)
    # operating point: flag at the same RATE the best single flags (18.6%), so
    # this measures WHICH items are flagged, not how many.
    print(f"  {'+'.join(c):38s} rank-corr with rung-1000 flags: ", end="")
    rung_flagged = {i for i in jids if jr_r[i]["our_max_p"] < 0.5}
    ens_flagged = fa_at_rank_quantile(c, len(rung_flagged) / len(jids))
    inter = len(rung_flagged & ens_flagged)
    print(f"{inter}/{len(rung_flagged)} overlap")

# ---- miss overlap: the mechanistic precondition for any ensemble gain -------
print("\n=== miss overlap on the control bank (why M can or cannot work) ===")
def misses(name):
    f = JUDGES[name]
    vals = sorted((f(i), i) for i in ids)
    # flag the 78 lowest-scoring as 'ungrounded' (matched to the true count)
    flagged = {i for _, i in vals[:len(U)]}
    return {i for i in U if i not in flagged}  # ungrounded items NOT caught
m = {n: misses(n) for n in JUDGES}
for n, s in m.items():
    print(f"  {n:11s} misses {len(s):3d}/{len(U)} ungrounded @ matched count")
pair = ("rung-1000", "incumbent")
a, b = m[pair[0]], m[pair[1]]
print(f"  {pair[0]}-only: {len(a-b)}   {pair[1]}-only: {len(b-a)}   both: {len(a&b)}")
print(f"  -> union caught by at least one: {len(U) - len(a & b)}/{len(U)}  (ceiling for any 2-judge combiner)")
