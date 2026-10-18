"""generality.py: Bar G and Bar R as generality-prereg.md (revised 2026-10-16) defines them, from bank.json files.

Reads only verdict counts and per-question agreement counts; prints no question of a held-out corpus.
"""
import glob, itertools, json, statistics, sys

L = "/Users/alexsbryan/dev/commonwealth-ai/runs/loop"
READS = {
    "asked": [f"{L}/gen-rescore/asked-r1.bank.json", f"{L}/gen-fresh/saltgrass-asked/bank.json"],
    "ledger": [f"{L}/gen-rescore/ledger-r1.bank.json", f"{L}/gen-fresh/saltgrass-ledger/bank.json"],
    "detective": [f"{L}/gen-fresh/saltgrass-detective/bank.json"],
    "editor": [f"{L}/gen-fresh/saltgrass-editor/bank.json"],
    "engineer": [f"{L}/gen-fresh/saltgrass-engineer/bank.json"],
}
# Read at 20k-character windows (determinism-prereg.md step 4): generality.py --w20
W20 = {"asked": ["w20-sg1", "w20-sg2"], "ledger": ["w20-ledger1", "w20-ledger2"], "detective": ["w20-held"],
       "editor": ["w20-held"], "engineer": ["w20-held"]}
if "--w20" in sys.argv:
    READS = {r: [f"{L}/{a}/saltgrass-{r}/bank.json" for a in arms] for r, arms in W20.items()}
ONESHOT = [f"{L}/gen-rescore/oneshot1.bank.json", f"{L}/gen-rescore/oneshot2.bank.json"]

load = lambda p: json.load(open(p))
judges = set()
def verdicts(p):
    d = load(p); judges.add(d["summary"]["judge"])
    return {q["id"]: q["verdict"] for q in d["questions"]}
right = lambda v: sum(x == "right" for x in v.values())
agree = lambda a, b: sum(a[i] == b[i] for i in a)

# Read noise (generality.py --noise): one recipe, one reader code, fresh reads; and the bank alone, on fixed records.
SAME_CODE = [(f"asked {a} {b}", f"{L}/{a}/saltgrass-asked/bank.json", f"{L}/{b}/saltgrass-asked/bank.json")
             for a, b in (("fit-sg1", "fit-sg2"), ("voice-sg1", "voice-sg2"), ("voice2-sg1", "voice2-sg2"))] \
    + [(f"{r} gen reads", *ps) for r, ps in READS.items() if len(ps) == 2]
SHUFFLES = sorted(glob.glob(f"{L}/answerer-noise/shuffles/*.json"))
if sys.argv[1:] == ["--noise"]:
    S = [{q["id"]: q["verdict"] for q in load(p)["questions"]} for p in SHUFFLES]  # single presentations, unstamped
    maj = lambda sub: {i: max(("right", "wrong"), key=lambda x: sum(s[i] == x for s in sub)) for i in sub[0]}
    halves = [(a, [i for i in range(len(S)) if i not in a]) for a in itertools.combinations(range(len(S)), len(S) // 2) if 0 in a]
    floor = [agree(maj([S[i] for i in a]), maj([S[i] for i in b])) for a, b in halves]
    print(f"bank alone, fixed records: majority-of-{len(S) // 2} halves agree {statistics.mean(floor):.1f} ({min(floor)}-{max(floor)})")
    for label, a, b in SAME_CODE:
        x, y = verdicts(a), verdicts(b)
        print(f"same recipe, same code: {right(x):>2} {right(y):>2}  agree {agree(x, y)}  {label}")
    print(f"judges: {sorted(judges)}"); sys.exit()
V = {r: [verdicts(p) for p in ps] for r, ps in READS.items()}
O = [verdicts(p) for p in ONESHOT]
for r, vs in V.items(): print(f"{r:<10} reads {[right(v) for v in vs]}  mean {statistics.mean(right(v) for v in vs):.1f}")
print(f"one-shot   reads {[right(v) for v in O]}  mean {statistics.mean(right(v) for v in O):.1f}")

sys_mean, os_mean = statistics.mean(right(v) for v in V["ledger"]), statistics.mean(right(v) for v in O)
print(f"\nBar G (ledger): system {sys_mean:.1f} >= one-shot {os_mean:.1f} - 2 = {os_mean - 2:.1f}: {'PASS' if sys_mean >= os_mean - 2 else 'FAIL'}")

def bar_r(V):
    """Bar R per recipe pair: (score gap, cross agreement, passes), against the yardstick pooled over recipes read twice."""
    two = [r for r, vs in V.items() if len(vs) == 2]
    gap_y = statistics.mean(abs(right(V[r][0]) - right(V[r][1])) for r in two)
    agree_y = statistics.mean(agree(V[r][0], V[r][1]) for r in two)
    pairs = {}
    for a, b in itertools.combinations(V, 2):
        gap = abs(statistics.mean(right(v) for v in V[a]) - statistics.mean(right(v) for v in V[b]))
        cross = statistics.mean(agree(x, y) for x in V[a] for y in V[b])
        pairs[a, b] = (gap, cross, gap <= gap_y and cross >= agree_y - 3)
    return two, gap_y, agree_y, pairs
spread = lambda V: statistics.pvariance([statistics.mean(right(v) for v in vs) for vs in V.values()])
# Bar R when the recipe means nothing (generality.py --w20 --null): the same reads dealt to the recipes' slots every
# distinct way. A bar that fails as often dealt as observed cannot tell an author's recipe from read noise.
if "--null" in sys.argv:
    reads = [v for vs in V.values() for v in vs]
    shape = [(r, len(vs)) for r, vs in V.items()]; ends = list(itertools.accumulate(n for _, n in shape))
    dealt = {}
    for order in itertools.permutations(range(len(reads))):
        key = tuple(frozenset(order[e - n:e]) for (_, n), e in zip(shape, ends))
        if key not in dealt:
            D = {r: [reads[i] for i in k] for (r, _), k in zip(shape, key)}
            dealt[key] = (sum(p for *_, p in bar_r(D)[3].values()), spread(D))
    n, ob_pass, ob_spread = len(dealt), sum(p for *_, p in bar_r(V)[3].values()), spread(V)
    print(f"\n{n} deals of {len(reads)} reads: pairs passing Bar R mean {statistics.mean(p for p, _ in dealt.values()):.1f} of "
          f"{len(bar_r(V)[3])}; dealt <= observed {ob_pass}: {sum(p <= ob_pass for p, _ in dealt.values()) / n:.2f}")
    print(f"variance of recipe means {ob_spread:.1f}; dealt >= observed: {sum(s >= ob_spread - 1e-9 for _, s in dealt.values()) / n:.2f}")
    # agreement is the score gap again unless reads swap questions: disagreements = gap + 2 * swaps
    swaps = lambda x, y: (sum(x[i] != y[i] for i in x) - abs(right(x) - right(y))) / 2
    same = [swaps(*vs) for vs in V.values() if len(vs) == 2]
    cross = [swaps(x, y) for a, b in itertools.combinations(V, 2) for x in V[a] for y in V[b]]
    print(f"questions swapped per read pair: same recipe {statistics.mean(same):.1f} ({len(same)} pairs), "
          f"cross recipe {statistics.mean(cross):.1f} ({len(cross)} pairs, {min(cross)}-{max(cross)})")
    print(f"judges: {sorted(judges)}"); sys.exit()
two, gap_y, agree_y, R = bar_r(V)
print(f"\nBar R yardstick (pooled over {two}): read-to-read gap {gap_y:.1f}, within-recipe agreement {agree_y:.1f}/42")
for (a, b), (gap, cross, ok) in R.items():
    print(f"  {a:<10} vs {b:<10} gap {gap:4.1f} (<= {gap_y:.1f})  agreement {cross:4.1f} (>= {agree_y - 3:.1f})  {'PASS' if ok else 'FAIL'}")
print(f"\nmean cross-recipe agreement over {len(R)} pairs: {statistics.mean(c for _, c, _ in R.values()):.1f}/42")
print(f"\njudges: {sorted(judges)}" + ("" if len(judges) == 1 else "  MIXED JUDGES: not comparable"))
