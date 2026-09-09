#!/usr/bin/env python3
"""How much is a PERFECT selector worth, and how small a window can it use?

§14 measured that retrieval is nearly free in time (+29% wall for 8x the hits)
while context is the scarce resource (7.4x the chars from k=10 to k=80). That
relocated the mesh mechanism to SELECTION: reduce a large candidate set to a
small high-value window. Before pricing any selector -- the facility-location
coverage selection already in `index/search.rs:659`, or the cross-encoder
rejected in 2026-08 on TTFT and slot cost rather than on quality -- this bounds
what ANY of them could win.

METHOD. One retrieval per question at k=80 (the §14 ceiling: 93.0% coverage in
the candidate set). Then, for each window size N, score facts under two policies
over the SAME candidates:
  rank   -- the first N hits, i.e. what top-k truncation gives you free. This is
            the PAIRED CONTROL, and §14's lesson is that a bar without one is
            not a bar.
  oracle -- greedily pick the N hits that maximise expected-fact coverage. It
            reads the answer key, so no real selector can reach it; it is the
            CEILING on the whole family, in the same sense as §13's oracle query.
The gap between them at fixed N is the headroom selection has to play for.

PRE-REGISTERED BARS (written before the first retrieval)
  Primary -- headroom at N=28, the rung-6 arms' window size:
    oracle - rank >= 8 pts -> selection has real headroom; the integration fight
        (mesh peer computing margins off the TTFT path) is worth having.
    < 3 pts -> rank order is already near-optimal at this window. Selection is
        NOT the lever, the §14 hypothesis dies here, and no selector should be
        built or re-opened on coverage grounds. Report and stop.
    3-8 -> modest; reported as modest.
  Secondary -- the product number: the smallest N at which the ORACLE reaches
    the k=80 ceiling. If a perfect selector hits ceiling coverage at N <= 20,
    that is a >= 4x context reduction at equal coverage, which is the entire
    "small high-value window" argument stated as a ratio.

  PREDICTION: vector rank order is a similarity ordering, not a coverage
  ordering, so it should waste slots on near-duplicates of the same passage --
  which is precisely why facility-location was written. I expect real headroom.
  Recorded so the result can contradict it.
"""
import json, re, subprocess, time
from pathlib import Path

D = Path("/home/alexbryan/dev/commonwealth-ai/sovereign/bench/sep_atlas/map-conversion-rung6")
K = 80
NS = [5, 10, 15, 20, 28, 40, 80]

def tok(f):
    return [t for t in f.lower().replace("-", " ").split() if len(t) >= 3]

def matched(fact, hay):
    h = hay.lower()
    return all(t in h for t in tok(fact))

def hits(query, limit=K, timeout=300):
    """Rank-ordered list of hit texts (title + snippet), as the CLI prints them."""
    p = subprocess.run(["sovereign", "corpus", "search", "sep", query, "--limit", str(limit)],
                       capture_output=True, text=True, timeout=timeout)
    out = p.stdout or ""
    parts = re.split(r"^\s*\d+\.\s+\[", out, flags=re.M)[1:]
    return [x.strip() for x in parts]

def cover(facts, chunks):
    hay = "\n".join(chunks)
    return sum(1 for f in facts if matched(f, hay))

def greedy(facts, chunks, n):
    """Facility-location in the metric that matters: pick the chunk adding the
    most uncovered facts, repeat. Ties break toward the higher-ranked chunk."""
    remaining = [f for f in facts]
    chosen, pool = [], list(range(len(chunks)))
    while len(chosen) < n and pool and remaining:
        best, gain = None, 0
        for i in pool:
            g = sum(1 for f in remaining if matched(f, chunks[i]))
            if g > gain:
                best, gain = i, g
        if best is None:
            break
        chosen.append(best); pool.remove(best)
        remaining = [f for f in remaining if not matched(f, chunks[best])]
    # pad with rank order so the window is genuinely size n
    for i in pool:
        if len(chosen) >= n: break
        chosen.append(i)
    return [chunks[i] for i in chosen[:n]]

R = {a: {r["question_id"]: r for r in json.load(open(D / f"{a}.json"))["results"]}
     for a in ("armA", "armA2", "armB", "armB2")}
QS = sorted(R["armA"])

data, wall = {}, 0.0
for i, q in enumerate(QS, 1):
    rec = R["armA"][q]
    facts = ([x.strip() for x in rec["fact_score"]["matched"]] +
             [x.strip() for x in rec["fact_score"]["missing"]])
    t0 = time.time(); hs = hits(rec["question"]); wall += time.time() - t0
    data[q] = (facts, hs)
    print(f"[{i}/{len(QS)}] {q}  ({len(hs)} hits)", flush=True)

total = sum(len(f) for f, _ in data.values())
print(f"\ncandidate pool: k={K}, {wall/len(QS):.2f}s/question, {total} expected facts\n")
print(f"{'N':>4} {'rank':>14} {'oracle':>14} {'headroom':>9} {'chars@rank':>11}")
rows = []
for n in NS:
    r_cov = sum(cover(f, h[:n]) for f, h in data.values())
    o_cov = sum(cover(f, greedy(f, h, n)) for f, h in data.values())
    ch = sum(len("\n".join(h[:n])) for _, h in data.values())
    rows.append((n, r_cov, o_cov, ch))
    print(f"{n:>4} {r_cov:>5}/{total} {r_cov/total:>6.1%} {o_cov:>5}/{total} {o_cov/total:>6.1%} "
          f"{(o_cov-r_cov)/total:>+8.1%} {ch:>11,}")

ceiling = max(o for _, _, o, _ in rows)
r28 = next(r for n, r, o, c in rows if n == 28)
o28 = next(o for n, r, o, c in rows if n == 28)
head = (o28 - r28) / total
min_n = next((n for n, _, o, _ in rows if o >= ceiling), None)
rank_ceiling_n = next((n for n, r, _, _ in rows if r >= ceiling), None)

print(f"\n=== verdict against the pre-registered bars ===")
print(f"  headroom at N=28: {o28-r28:+d} facts = {head:+.1%}   (bar: >= 8pts live, < 3pts dead)")
if head >= 0.08:
    print("  PRIMARY: LIVE — a perfect selector is worth real coverage at a fixed window.")
elif head < 0.03:
    print("  PRIMARY: DEAD — rank order is already near-optimal; selection is not the lever.")
else:
    print("  PRIMARY: MODEST — reported as modest.")
print(f"\n  ceiling coverage (oracle, any N): {ceiling}/{total} = {ceiling/total:.1%}")
print(f"  smallest N where ORACLE reaches it: {min_n}")
print(f"  smallest N where RANK reaches it:   {rank_ceiling_n}")
if min_n and rank_ceiling_n:
    print(f"  SECONDARY: context reduction at equal coverage = {rank_ceiling_n/min_n:.1f}x")
elif min_n:
    print(f"  SECONDARY: rank order NEVER reaches the ceiling within k={K}; "
          f"the oracle needs N={min_n}.")
