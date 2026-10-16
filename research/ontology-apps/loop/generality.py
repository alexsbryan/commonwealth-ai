"""generality.py: Bar G and Bar R as generality-prereg.md (revised 2026-10-16) defines them, from bank.json files.

Reads only verdict counts and per-question agreement counts; prints no question of a held-out corpus.
"""
import itertools, json, statistics, sys

L = "/Users/alexsbryan/dev/commonwealth-ai/runs/loop"
READS = {
    "asked": [f"{L}/gen-rescore/asked-r1.bank.json", f"{L}/gen-fresh/saltgrass-asked/bank.json"],
    "ledger": [f"{L}/gen-rescore/ledger-r1.bank.json", f"{L}/gen-fresh/saltgrass-ledger/bank.json"],
    "detective": [f"{L}/gen-fresh/saltgrass-detective/bank.json"],
    "editor": [f"{L}/gen-fresh/saltgrass-editor/bank.json"],
    "engineer": [f"{L}/gen-fresh/saltgrass-engineer/bank.json"],
}
ONESHOT = [f"{L}/gen-rescore/oneshot1.bank.json", f"{L}/gen-rescore/oneshot2.bank.json"]

load = lambda p: json.load(open(p))
judges = set()
def verdicts(p):
    d = load(p); judges.add(d["summary"]["judge"])
    return {q["id"]: q["verdict"] for q in d["questions"]}
right = lambda v: sum(x == "right" for x in v.values())
agree = lambda a, b: sum(a[i] == b[i] for i in a)

V = {r: [verdicts(p) for p in ps] for r, ps in READS.items()}
O = [verdicts(p) for p in ONESHOT]
for r, vs in V.items(): print(f"{r:<10} reads {[right(v) for v in vs]}  mean {statistics.mean(right(v) for v in vs):.1f}")
print(f"one-shot   reads {[right(v) for v in O]}  mean {statistics.mean(right(v) for v in O):.1f}")

sys_mean, os_mean = statistics.mean(right(v) for v in V["ledger"]), statistics.mean(right(v) for v in O)
print(f"\nBar G (ledger): system {sys_mean:.1f} >= one-shot {os_mean:.1f} - 2 = {os_mean - 2:.1f}: {'PASS' if sys_mean >= os_mean - 2 else 'FAIL'}")

two = [r for r, vs in V.items() if len(vs) == 2]
gap_y = statistics.mean(abs(right(V[r][0]) - right(V[r][1])) for r in two)
agree_y = statistics.mean(agree(V[r][0], V[r][1]) for r in two)
print(f"\nBar R yardstick (pooled over {two}): read-to-read gap {gap_y:.1f}, within-recipe agreement {agree_y:.1f}/42")
for a, b in itertools.combinations(V, 2):
    gap = abs(statistics.mean(right(v) for v in V[a]) - statistics.mean(right(v) for v in V[b]))
    cross = statistics.mean(agree(x, y) for x in V[a] for y in V[b])
    ok = gap <= gap_y and cross >= agree_y - 3
    print(f"  {a:<10} vs {b:<10} gap {gap:4.1f} (<= {gap_y:.1f})  agreement {cross:4.1f} (>= {agree_y - 3:.1f})  {'PASS' if ok else 'FAIL'}")
print(f"\njudges: {sorted(judges)}" + ("" if len(judges) == 1 else "  MIXED JUDGES: not comparable"))
