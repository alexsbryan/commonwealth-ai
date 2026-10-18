"""generality.py: Bar G and Bar R as generality-prereg.md defines them (revised 2026-10-16; Bar R again 2026-10-18), from
bank.json files.

Reads only verdict counts and per-question agreement counts; prints no question of a held-out corpus.
"""
import glob, itertools, json, random, statistics, sys

L = "/Users/alexsbryan/dev/commonwealth-ai/runs/loop"
READS = {
    "asked": [f"{L}/gen-rescore/asked-r1.bank.json", f"{L}/gen-fresh/saltgrass-asked/bank.json"],
    "ledger": [f"{L}/gen-rescore/ledger-r1.bank.json", f"{L}/gen-fresh/saltgrass-ledger/bank.json"],
    "detective": [f"{L}/gen-fresh/saltgrass-detective/bank.json"],
    "editor": [f"{L}/gen-fresh/saltgrass-editor/bank.json"],
    "engineer": [f"{L}/gen-fresh/saltgrass-engineer/bank.json"],
}
# Read at 20k-character windows (determinism-prereg.md step 4): generality.py --w20. --first keeps one read per
# persona, the 2026-10-16 design (aea6993bd's numbers).
W20 = {"asked": ["w20-sg1", "w20-sg2"], "ledger": ["w20-ledger1", "w20-ledger2"], "detective": ["w20-held", "w20-held2"],
       "editor": ["w20-held", "w20-held2"], "engineer": ["w20-held", "w20-held2"]}
if "--first" in sys.argv:
    W20 = {r: arms if r in ("asked", "ledger") else arms[:1] for r, arms in W20.items()}
if "--w20" in sys.argv:
    READS = {r: [f"{L}/{a}/saltgrass-{r}/bank.json" for a in arms] for r, arms in W20.items()}
ONESHOT = [f"{L}/gen-rescore/oneshot1.bank.json", f"{L}/gen-rescore/oneshot2.bank.json"]
# Bar R's control (2026-10-18): asked read at 40k windows, a known 9-point reader effect the author test must see.
CONTROL = [f"{L}/gen-rescore/asked-r1.bank.json", f"{L}/gen-fresh/saltgrass-asked/bank.json"]

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

avg = lambda xs: (lambda l: sum(l) / len(l))(list(xs))
def bar_r(V, right=right, agree=agree):
    """Bar R (2026-10-16) per recipe pair: (score gap, cross agreement, passes), against the yardstick pooled over
    recipes read twice. right/agree read V's reads; a deal passes read indices and the tables dealt() built."""
    two = [r for r, vs in V.items() if len(vs) == 2]
    gap_y = avg(abs(right(V[r][0]) - right(V[r][1])) for r in two)
    agree_y = avg(agree(V[r][0], V[r][1]) for r in two)
    pairs = {}
    for a, b in itertools.combinations(V, 2):
        gap = abs(avg(right(v) for v in V[a]) - avg(right(v) for v in V[b]))
        cross = avg(agree(x, y) for x in V[a] for y in V[b])
        pairs[a, b] = (gap, cross, gap <= gap_y and cross >= agree_y - 3)
    return two, gap_y, agree_y, pairs
def spread(V, right=right):
    """The variance of the recipes' mean scores, which Bar R's author test (2026-10-18) holds to random deals."""
    means = [avg(right(v) for v in vs) for vs in V.values()]
    return avg((m - avg(means)) ** 2 for m in means)
DEALS = 100_000
def dealt(V, seed=0):
    """V's reads dealt to its recipes' slots at random, DEALS times: each deal {recipe: [read index]} with the score
    and agreement tables to read it through, so a bar on a deal is the same function as the bar on V."""
    reads = [v for vs in V.values() for v in vs]; S = [right(v) for v in reads]
    A = [[agree(x, y) for y in reads] for x in reads]
    shape = [(r, len(vs)) for r, vs in V.items()]; idx = list(range(len(reads))); rng = random.Random(seed)
    for _ in range(DEALS):
        rng.shuffle(idx); it = iter(idx)
        yield {r: [next(it) for _ in range(k)] for r, k in shape}, S.__getitem__, lambda i, j: A[i][j]
def author(V):
    """Bar R's author test (2026-10-18): V's recipe-mean variance, and the share of random deals at or above it."""
    ob = spread(V)
    return ob, sum(spread(D, s) >= ob - 1e-9 for D, s, _ in dealt(V)) / DEALS
# The 2026-10-16 bar when the recipe means nothing (generality.py --w20 --null): the same reads dealt to the recipes at
# random. A bar that fails as often dealt as observed cannot tell an author's recipe from read noise.
if "--null" in sys.argv:
    ob = sum(p for *_, p in bar_r(V)[3].values())
    P = [sum(p for *_, p in bar_r(D, s, a)[3].values()) for D, s, a in dealt(V)]
    print(f"\n{DEALS} random deals of the reads (seed 0): pairs passing Bar R (2026-10-16) mean {avg(P):.1f} of "
          f"{len(bar_r(V)[3])}; dealt <= observed {ob}: {sum(p <= ob for p in P) / DEALS:.2f}")
    # agreement is the score gap again unless reads swap questions: disagreements = gap + 2 * swaps
    swaps = lambda x, y: (sum(x[i] != y[i] for i in x) - abs(right(x) - right(y))) / 2
    same = [swaps(*vs) for vs in V.values() if len(vs) == 2]
    cross = [swaps(x, y) for a, b in itertools.combinations(V, 2) for x in V[a] for y in V[b]]
    print(f"questions swapped per read pair: same recipe {avg(same):.1f} ({len(same)} pairs), "
          f"cross recipe {avg(cross):.1f} ({len(cross)} pairs, {min(cross)}-{max(cross)})")
    print(f"judges: {sorted(judges)}"); sys.exit()
two, gap_y, agree_y, R = bar_r(V)
print(f"\nBar R yardstick (pooled over {two}): read-to-read gap {gap_y:.1f}, within-recipe agreement {agree_y:.1f}/42")
for (a, b), (gap, cross, ok) in R.items():
    print(f"  {a:<10} vs {b:<10} gap {gap:4.1f} (<= {gap_y:.1f})  agreement {cross:4.1f} (>= {agree_y - 3:.1f})  {'PASS' if ok else 'FAIL'}")
print(f"\nmean cross-recipe agreement over {len(R)} pairs: {avg(c for _, c, _ in R.values()):.1f}/42")

# Bar R as revised 2026-10-18: judged only on two reads of every recipe, and its author half only once the control
# (a known reader effect dealt in as a sixth recipe) fails; otherwise could-not-judge, never a pass.
judged = all(len(vs) == 2 for vs in V.values())
c_ob, c_share = author({**V, "asked-40k": [verdicts(p) for p in CONTROL]})
ob, share = author(V)
means = {r: avg(right(v) for v in vs) for r, vs in V.items()}
print(f"\nBar R (2026-10-18), {DEALS} random deals, seed 0" + ("" if judged else ": COULD NOT JUDGE, a recipe has one read"))
print(f"  control, asked at 40k as a sixth recipe: variance {c_ob:.1f}, dealt >= it {c_share:.3f}: "
      + ("fails, as it must" if c_share <= 0.05 else "DOES NOT FAIL: the author test cannot see a known effect"))
print(f"  author: variance of recipe means {ob:.1f}, dealt >= it {share:.3f}: "
      + ("could not judge" if not judged or c_share > 0.05 else "PASS" if share > 0.05 else "FAIL"))
print(f"  floor: every recipe's mean >= one-shot {os_mean:.1f} - 2 = {os_mean - 2:.1f} (lowest {min(means.values()):.1f}, "
      f"{min(means, key=means.get)}): " + ("could not judge" if not judged else "PASS" if min(means.values()) >= os_mean - 2 else "FAIL"))
print(f"\njudges: {sorted(judges)}" + ("" if len(judges) == 1 else "  MIXED JUDGES: not comparable"))
