#!/usr/bin/env python3
"""Does a sequential refinement loop converge or degrade? An offline simulation
on measured verifier scores.

Sections 8 and 8.1 measured PARALLEL passes (ensembles) and killed them. This is
the other regime -- SEQUENTIAL passes, where "convergence" is literally the
right word -- and nothing measured so far touches it.

THE HYPOTHESIS UNDER TEST
  A pass removes defects at (catch x b) and INTRODUCES them at (FA x (1-b)),
  because a "fix" applied to a claim that was already correct is a new defect.
  Net progress needs catch*b > FA*(1-b), i.e. precision > 50%. So the loop has
  an ATTRACTOR at

        b* = FA / (catch + FA)

  and it converges TO that rate from either side: above b* it removes, below b*
  it adds. The loop cannot drive the defect rate to zero; it drives it to a
  value set by the verifier's operating point.

PRE-REGISTERED PREDICTIONS (written before the simulation was run)
  Using this session's measured curve for rung-1000 on the constructed bank:
      FA  5% / catch 78.9%  ->  b* =  5.96%
      FA 10% / catch 86.7%  ->  b* = 10.34%
      FA 20% / catch 92.5%  ->  b* = 17.78%
  P1. Starting at the bank's own base rate (45%), every operating point
      IMPROVES, converging down onto its b*.
  P2. Starting at the production blatant-confab rate (9%, EPISTEMIC_STATE_PROOF
      secret_agent runs), FA=5% improves (9 -> 6.0) while FA=10% and FA=20%
      DEGRADE (9 -> 10.3, 9 -> 17.8). Same loop, same verifier, opposite sign,
      decided entirely by the base rate.
  KILL BAR: if a simulated equilibrium misses its predicted b* by more than
      1.5pt, the arithmetic behind the general hypothesis is wrong and the
      hypothesis is revised, not the simulation.

  P3 (second condition, separate): a loop whose action is DROP rather than
      REPAIR has a stable fixed point at the empty answer. Predicted: coverage
      falls monotonically toward 0 while the measured defect rate on what
      SURVIVES also falls -- i.e. the loop scores better on groundedness the
      whole way down to saying nothing.

WHAT IS MEASURED VS WHAT IS ASSUMED (the honest boundary)
  MEASURED: every verifier score. Claims are drawn from the real 2,510-item
    bank and carry their real `our_margin`; taus come from the real grounded
    score distribution. The catch/FA curve is not modelled, it is observed.
  ASSUMED: (a) a repair on a truly-defective claim succeeds; (b) a repair on a
    truly-correct claim damages it (p_damage=1.0); (c) a revised claim is
    re-scored by drawing from the distribution of its NEW class -- the verifier
    has no memory and judges the revision on its merits; (d) untouched claims
    keep their score, so a deterministic verifier cannot re-flag them.
  (b) at 1.0 is the assumption the formula makes (one defect introduced per
  false alarm) and is the WORST case for the loop. A declared sensitivity row
  at p_damage=0.5 is reported; it is not a sweep, and the primary is (b)=1.0.
"""
import json, random, statistics
from pathlib import Path

BANK = Path(__file__).resolve().parent.parent / "runs/headroom/scored.jsonl"
rows = [json.loads(l) for l in open(BANK) if l.strip()]
G = [r["our_margin"] for r in rows if r["label"] == "grounded"]
U = [r["our_margin"] for r in rows if r["label"] == "ungrounded"]
Gs = sorted(G)

def tau_for(fa):
    return Gs[max(0, int(round(fa * len(Gs))) - 1)]

def rates(tau):
    catch = sum(1 for s in U if s <= tau) / len(U)
    fa = sum(1 for s in G if s <= tau) / len(G)
    return catch, fa

M_CLAIMS, ITERS, TRIALS = 20, 8, 3000
random.seed(17)

def simulate(fa_target, base, p_damage=1.0, action="repair"):
    tau = tau_for(fa_target)
    hist = [[] for _ in range(ITERS + 1)]
    cov = [[] for _ in range(ITERS + 1)]
    for _ in range(TRIALS):
        # a claim is (is_defective, score). drawn from the REAL score pools.
        claims = []
        for _ in range(M_CLAIMS):
            d = random.random() < base
            claims.append([d, random.choice(U if d else G)])
        hist[0].append(sum(1 for d, _ in claims if d) / max(1, len(claims)))
        cov[0].append(len(claims) / M_CLAIMS)
        for it in range(1, ITERS + 1):
            nxt = []
            for d, s in claims:
                if s > tau:                      # not flagged: untouched, stable
                    nxt.append([d, s]); continue
                if action == "drop":
                    continue                     # the claim leaves the answer
                if d:                            # flagged, truly defective -> repaired
                    nxt.append([False, random.choice(G)])
                else:                            # flagged, truly correct -> damaged
                    if random.random() < p_damage:
                        nxt.append([True, random.choice(U)])
                    else:
                        nxt.append([False, random.choice(G)])
            claims = nxt
            hist[it].append(sum(1 for d, _ in claims if d) / max(1, len(claims)) if claims else 0.0)
            cov[it].append(len(claims) / M_CLAIMS)
    return [statistics.mean(h) for h in hist], [statistics.mean(c) for c in cov]

print("=== measured operating points (rung-1000, constructed bank) ===")
POINTS = []
for fa in (0.05, 0.10, 0.20):
    t = tau_for(fa); c, f = rates(t)
    bstar = f / (c + f)
    POINTS.append((fa, t, c, f, bstar))
    print(f"  FA {fa:.0%}: catch {c:.1%}  actual FA {f:.1%}  ->  predicted b* = {bstar:.2%}")

for base, tag in ((0.45, "bank's own rate"), (0.09, "production blatant-confab")):
    print(f"\n=== P{1 if base>0.4 else 2}: refinement loop from base rate {base:.0%} ({tag}) ===")
    print(f"{'op point':10s} " + " ".join(f"it{i}" .rjust(6) for i in range(ITERS + 1)) + "   b*     verdict")
    for fa, t, c, f, bstar in POINTS:
        h, _ = simulate(fa, base)
        eq = h[-1]
        miss = abs(eq - bstar)
        verdict = "OK" if miss <= 0.015 else f"MISS {miss:+.1%}"
        arrow = "improves" if eq < base - 0.005 else ("DEGRADES" if eq > base + 0.005 else "flat")
        print(f"FA {fa:5.0%}   " + " ".join(f"{x:6.1%}" for x in h) + f"  {bstar:5.1%}  {arrow:9s} {verdict}")

print("\n=== declared sensitivity: p_damage=0.5 (a false alarm damages half the time) ===")
for fa, t, c, f, bstar in POINTS:
    h, _ = simulate(fa, 0.09, p_damage=0.5)
    print(f"FA {fa:5.0%}   " + " ".join(f"{x:6.1%}" for x in h) + f"   (p_damage=1.0 b* was {bstar:.1%})")

print("\n=== P3: the same loop with action=DROP instead of REPAIR (base 9%) ===")
print(f"{'op point':10s} {'metric':10s} " + " ".join(f"it{i}".rjust(6) for i in range(ITERS + 1)))
for fa, t, c, f, bstar in POINTS:
    h, cv = simulate(fa, 0.09, action="drop")
    print(f"FA {fa:5.0%}   {'defect%':10s} " + " ".join(f"{x:6.1%}" for x in h))
    print(f"{'':10s} {'coverage':10s} " + " ".join(f"{x:6.1%}" for x in cv))
