#!/usr/bin/env python3
"""Why the loop simulation broke its own kill bar -- the mechanism, and the one
assumption the whole result rests on.

The pre-registered b* = FA/(catch+FA) predicted the loop settles where precision
hits 50%. It does not. It settles far lower and IMPROVES at every operating
point. The one-step prediction was exact, so the error is in the dynamics.

Hypothesised mechanism: DAMAGE IS SELF-CORRECTING. A false alarm damages a
correct claim -- but the damaged claim is now genuinely defective, so it
re-enters the next iteration's flagging pool where the verifier's catch rate
(79-93%) is far higher than its false-alarm rate (5-20%). The loop repairs its
own collateral faster than it creates it. The steady-state formula assumed a
well-mixed population re-exposed each round; the real process freezes claims
that pass and recycles only the ones it touched.

If that is the mechanism, the result rests entirely on assumption (c) of the
simulation: that a claim damaged BY A REVISION is as detectable as an INJECTED
corruption. That is almost certainly optimistic -- injected corruptions are
entity swaps and negation flips, while revision damage is fluent, model-
generated, and is exactly the "shared-bias residual" the audit gate names.

So detectability is parameterised and swept. delta = P(a damaged claim scores
like a real corruption); 1-delta = P(it scores like a grounded claim, i.e. the
damage is invisible). delta=1.0 is the original run. The sweep locates the
delta at which the loop turns destructive -- which IS the general condition,
replacing the precision rule that just failed.
"""
import json, random, statistics
from pathlib import Path

rows = [json.loads(l) for l in open(Path(__file__).resolve().parent.parent / "runs/headroom/scored.jsonl") if l.strip()]
G = [r["our_margin"] for r in rows if r["label"] == "grounded"]
U = [r["our_margin"] for r in rows if r["label"] == "ungrounded"]
Gs = sorted(G)
tau_for = lambda fa: Gs[max(0, int(round(fa * len(Gs))) - 1)]
def rates(t):
    return sum(1 for s in U if s <= t)/len(U), sum(1 for s in G if s <= t)/len(G)

M, ITERS, TRIALS = 20, 8, 3000
random.seed(17)

def sim(fa, base, delta=1.0, iters=ITERS):
    tau = tau_for(fa)
    out = [[] for _ in range(iters+1)]
    recaught = []
    for _ in range(TRIALS):
        claims = [[random.random() < base, None] for _ in range(M)]
        for c in claims:
            c[1] = random.choice(U if c[0] else G)
        out[0].append(sum(1 for d,_ in claims if d)/len(claims))
        for it in range(1, iters+1):
            nxt, dmg = [], []
            for d, s in claims:
                if s > tau: nxt.append([d, s]); continue
                if d: nxt.append([False, random.choice(G)])
                else:
                    # damaged. detectable like a real corruption w.p. delta.
                    sc = random.choice(U) if random.random() < delta else random.choice(G)
                    nxt.append([True, sc]); dmg.append(sc)
            if it == 1 and dmg:
                recaught.append(sum(1 for s in dmg if s <= tau)/len(dmg))
            claims = nxt
            out[it].append(sum(1 for d,_ in claims if d)/len(claims) if claims else 0.0)
    return [statistics.mean(o) for o in out], (statistics.mean(recaught) if recaught else float("nan"))

print("=== 1. one-step check: does the formula predict iteration 1? ===")
print(f"{'FA':>5} {'base':>6} {'predicted b1':>13} {'simulated b1':>13}")
for fa in (0.05, 0.10, 0.20):
    c, f = rates(tau_for(fa))
    for base in (0.09, 0.45):
        pred = base - c*base + f*(1-base)
        h, _ = sim(fa, base, iters=1)
        print(f"{fa:5.0%} {base:6.0%} {pred:13.1%} {h[1]:13.1%}")

print("\n=== 2. mechanism: are damaged claims re-caught on the next pass? ===")
for fa in (0.05, 0.10, 0.20):
    c, f = rates(tau_for(fa))
    _, rc = sim(fa, 0.09)
    print(f"  FA {fa:5.0%}: {rc:.1%} of claims damaged in iter 1 are re-flagged in iter 2 "
          f"(verifier catch {c:.1%}, FA {f:.1%})")
print("  -> the loop repairs its own collateral because catch >> FA. That is the")
print("     dynamic the steady-state formula could not see.")

print("\n=== 3. the load-bearing assumption: how detectable must damage be? ===")
print("delta = P(revision damage looks like a real corruption to the verifier)")
print(f"\n{'delta':>6} " + " ".join(f"FA{int(f*100)}%".rjust(8) for f in (0.05,0.10,0.20)) + "     (equilibrium defect rate, base 9%)")
for delta in (1.0, 0.8, 0.6, 0.4, 0.2, 0.1, 0.0):
    cells = []
    for fa in (0.05, 0.10, 0.20):
        h, _ = sim(fa, 0.09, delta=delta)
        eq = h[-1]
        cells.append(f"{eq:7.1%}" + ("!" if eq > 0.09 else " "))
    print(f"{delta:6.1f} " + " ".join(c.rjust(8) for c in cells))
print("\n  '!' = equilibrium WORSE than the 9% starting defect rate (loop is destructive)")
