#!/usr/bin/env python3
"""Is the complementarity between two judges STRUCTURED by corruption kind?

Follows the M-axis negative (Sec 8 of VERIFICATION_SCALING_AXES.md). That result
killed a jury of judges asked the SAME question. This asks the prior question a
jury of SPECIALISTS depends on: do two different-family judges fail on
DIFFERENT kinds of corruption, or on the same ones?

  structured  -> a partition of the question across nodes (the C axis, fanned
                 out) has real headroom; specialists beat generalists.
  unstructured-> one judge dominates everywhere and there is nothing to
                 specialise, whatever the combiner.

Both judges are thresholded to the SAME false-alarm rate on grounded items
first, so 'catches more' cannot be bought with 'suspects more'.
Bank: runs/headroom/scored.jsonl, 2510 constructed corruptions (SEP).
"""
import json, collections
from pathlib import Path

rows = [json.loads(l) for l in open(Path(__file__).resolve().parent.parent / "runs/headroom/scored.jsonl") if l.strip()]
G = [r for r in rows if r["label"] == "grounded"]
U = [r for r in rows if r["label"] == "ungrounded"]
inc = lambda r: r["incumbent_max_support"]
ours = lambda r: r["our_margin"]

for TARGET_FA in (0.05, 0.10, 0.20):
    def tau(f):
        gs = sorted(f(r) for r in G)
        return gs[max(0, int(round(TARGET_FA * len(gs))) - 1)]
    ti, to = tau(inc), tau(ours)
    fa_i = sum(1 for r in G if inc(r) <= ti) / len(G)
    fa_o = sum(1 for r in G if ours(r) <= to) / len(G)
    print(f"=== both judges matched at FA={TARGET_FA:.0%} on {len(G)} grounded "
          f"(actual: incumbent {fa_i:.1%}, rung-1000 {fa_o:.1%}) ===")
    hdr = f"{'corruption kind':24s} {'n':>4} {'inc':>7} {'rung':>7} {'union':>7} {'inc-only':>12}"
    print(hdr); print("-" * len(hdr))
    kinds = collections.Counter(r["kind"] for r in U)
    T = collections.Counter()
    for k, _ in sorted(kinds.items(), key=lambda x: -x[1]):
        sub = [r for r in U if r["kind"] == k]
        ci = {r["id"] for r in sub if inc(r) <= ti}
        co = {r["id"] for r in sub if ours(r) <= to}
        only = ci - co
        T["n"] += len(sub); T["i"] += len(ci); T["o"] += len(co)
        T["u"] += len(ci | co); T["only"] += len(only)
        print(f"{k:24s} {len(sub):4d} {len(ci)/len(sub):6.1%} {len(co)/len(sub):6.1%} "
              f"{len(ci|co)/len(sub):6.1%} {len(only):4d} ({len(only)/len(sub):5.1%})")
    n = T["n"]
    print("-" * len(hdr))
    print(f"{'ALL':24s} {n:4d} {T['i']/n:6.1%} {T['o']/n:6.1%} {T['u']/n:6.1%} "
          f"{T['only']:4d} ({T['only']/n:5.1%})")
    print(f"  union lift over rung-1000 alone: {(T['u']-T['o'])/n:+.1%}\n")

print("inc-only = fabrications the 35B incumbent catches that rung-1000 misses.")
print("That column IS the jury's ceiling: signal no combiner can invent, and")
print("none can exceed. Read it per kind -- concentrated means specialise,")
print("flat-and-small means the second judge is redundant everywhere.")
