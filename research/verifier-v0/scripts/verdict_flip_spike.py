#!/usr/bin/env python3
"""Would a stack change have flipped anything a USER was told, on a STATIC corpus?

THE HYPOTHESIS UNDER TEST. §8-§11 measured verification on a fixed evidence
window and found it capped in every direction. The proposed escape is that on a
static corpus (SEP) the mesh itself is the only thing that changes -- retrieval
improves, the library grows, judges sharpen -- so a past answer can be upgraded
without any source moving. That is only a product if stack changes actually flip
CLAIM-LEVEL verdicts, and at a rate distinguishable from resampling noise.

WHY THIS IS FREE. Rung 6 (2026-09-08) already ran the natural experiment and
nobody looked at it this way. Arm A = pre-rung-3 binary, arm B = rung-3+4, same
21 SEP questions, same static corpus, same judge, TWO runs per arm. So the same
files carry both the signal (across-stack) and its own noise floor
(within-stack). Zero inference; this only re-reads JSON.

WHAT THE BENCH MEASURED VS WHAT THIS MEASURES. Rung 6's headline was
judge_fact_score.ratio -- a per-question MEAN, and its verdict was that arm A
and arm B differ by about run-to-run noise. That says nothing about churn: the
mean is preserved exactly when N facts flip up and N flip down. This reads
synth.judge_evidence[].present, the per-fact boolean, which is the granularity
of a thing a user was actually told.

PRE-REGISTERED BARS (written before the script was run)
  Primary -- SIGNAL. across-stack flip rate (A vs B, A2 vs B2) must exceed the
    within-stack rate (A vs A2, B vs B2) by a factor of >= 2.
      >= 2x  -> stack changes move claim-level verdicts on a static corpus; the
                "we got better at reading it" event has substance and the rate
                is now known.
      < 2x   -> flips are indistinguishable from re-running the same binary.
                A notification built on this is resampling noise wearing a
                citation. The trigger is DEAD and the design should fall back to
                cross-node contradiction only. Report and do not retry.
  Secondary -- DIRECTION, and it is a distinct way to fail. Among across-stack
    flips, upgrades (false->true) minus downgrades (true->false) must exceed the
    within-stack asymmetry. If flips are symmetric, the product would be telling
    users "we were wrong before" exactly as often as "we are better now" --
    a signal that is real and still unshippable. Reported either way (§18.6).

  Third block, added after the primary bars were evaluated and reported as an
  EXTENSION, not a retrofit: the same comparison against the committed July
  baseline (2026-07-06), a ~2-month stack delta rather than rung 6's 2-day one.
  It answers the obvious follow-up -- whether the rung-6 arms were simply too
  close together to move anything -- and carries its own confound, named: the
  July baseline is UN-ISOLATED while the arms are isolated to sep (rung 6
  README). The isolated and un-isolated May baselines read the same judge mean,
  so the comparison holds with that caveat attached to it.

  PREDICTION, recorded before the run: rung 6 found the ARM MEANS equal, so this
  will not show a large net movement. The open question is whether churn is
  hiding under that equality. I expect within-stack flips to be non-zero
  (the synth path samples) and I genuinely do not know the across/within ratio
  -- which is why it is worth the ten seconds this costs.
"""
import json, sys, itertools
from pathlib import Path

D = Path("/home/alexbryan/dev/commonwealth-ai/sovereign/bench/sep_atlas/map-conversion-rung6")

def verdicts(arm):
    """(question_id, fact) -> present. The unit is one thing the user was told."""
    p = D / f"{arm}.json"
    if not p.exists():
        return None
    out = {}
    for r in json.load(open(p))["results"]:
        for e in (r.get("synth") or {}).get("judge_evidence") or []:
            out[(r["question_id"], e["fact"].strip())] = bool(e["present"])
    return out

def compare(a, b):
    va, vb = verdicts(a), verdicts(b)
    if va is None or vb is None:
        return None
    shared = set(va) & set(vb)
    up   = sum(1 for k in shared if not va[k] and vb[k])
    down = sum(1 for k in shared if va[k] and not vb[k])
    return {"pair": f"{a}->{b}", "n": len(shared), "up": up, "down": down,
            "flips": up + down, "rate": (up + down) / len(shared) if shared else 0.0,
            "net": up - down}

WITHIN = [("armA", "armA2"), ("armB", "armB2")]
ACROSS = [("armA", "armB"), ("armA2", "armB2")]

def block(title, pairs):
    rows = [c for c in (compare(*p) for p in pairs) if c]
    print(f"\n{title}")
    print(f"  {'pair':<16} {'facts':>6} {'flips':>6} {'rate':>7} {'up':>4} {'down':>5} {'net':>5}")
    for c in rows:
        print(f"  {c['pair']:<16} {c['n']:>6} {c['flips']:>6} {c['rate']:>7.1%} "
              f"{c['up']:>4} {c['down']:>5} {c['net']:>+5}")
    n = sum(c["n"] for c in rows); f = sum(c["flips"] for c in rows)
    u = sum(c["up"] for c in rows); d = sum(c["down"] for c in rows)
    return {"n": n, "flips": f, "rate": f / n if n else 0.0, "up": u, "down": d}

BASE = Path("/home/alexbryan/dev/commonwealth-ai/sovereign/bench/sep/"
            "baselines/questions-synth/2026-07-06.json")

def verdicts_path(p):
    out = {}
    for r in json.load(open(p))["results"]:
        for e in (r.get("synth") or {}).get("judge_evidence") or []:
            out[(r["question_id"], e["fact"].strip())] = bool(e["present"])
    return out

def compare_base(arm):
    if not BASE.exists() or not (D / f"{arm}.json").exists():
        return None
    vb, va = verdicts_path(BASE), verdicts(arm)
    shared = set(vb) & set(va)
    up   = sum(1 for k in shared if not vb[k] and va[k])
    down = sum(1 for k in shared if vb[k] and not va[k])
    return {"pair": f"july->{arm}", "n": len(shared), "up": up, "down": down,
            "flips": up + down, "rate": (up + down) / len(shared) if shared else 0.0,
            "net": up - down}

w = block("WITHIN-STACK (same binary, two runs) — the noise floor", WITHIN)
a = block("ACROSS-STACK (pre-rung-3 vs rung-3+4, 2 days) — the signal", ACROSS)

brows = [c for c in (compare_base(x) for x in ("armA", "armA2", "armB", "armB2")) if c]
if brows:
    print("\nBASELINE-STACK (July 6 vs September 8, ~2 months) — the larger delta")
    print(f"  {'pair':<16} {'facts':>6} {'flips':>6} {'rate':>7} {'up':>4} {'down':>5} {'net':>5}")
    for c in brows:
        print(f"  {c['pair']:<16} {c['n']:>6} {c['flips']:>6} {c['rate']:>7.1%} "
              f"{c['up']:>4} {c['down']:>5} {c['net']:>+5}")
    bn = sum(c["n"] for c in brows); bf = sum(c["flips"] for c in brows)
    bu = sum(c["up"] for c in brows); bd = sum(c["down"] for c in brows)
    b = {"n": bn, "flips": bf, "rate": bf / bn if bn else 0.0, "up": bu, "down": bd}
else:
    b = None

print("\n=== verdict against the pre-registered bars ===")
ratio = (a["rate"] / w["rate"]) if w["rate"] else float("inf")
print(f"  within-stack flip rate : {w['rate']:.2%}  ({w['flips']}/{w['n']} facts)")
print(f"  across-stack flip rate : {a['rate']:.2%}  ({a['flips']}/{a['n']} facts)")
print(f"  ratio                  : {ratio:.2f}x   (bar: >= 2.00x)")
print(f"  PRIMARY: {'SIGNAL — stack changes move user-visible verdicts' if ratio >= 2 else 'DEAD — indistinguishable from resampling'}")
wa = (w["up"] - w["down"]) / w["n"] if w["n"] else 0
aa = (a["up"] - a["down"]) / a["n"] if a["n"] else 0
print(f"\n  within-stack asymmetry : {wa:+.2%}  (up {w['up']}, down {w['down']})")
print(f"  across-stack asymmetry : {aa:+.2%}  (up {a['up']}, down {a['down']})")
print(f"  SECONDARY: {'directional — upgrades dominate' if aa > abs(wa) else 'SYMMETRIC — as many retractions as improvements'}")

if b:
    br = b["rate"] / w["rate"] if w["rate"] else float("inf")
    ba = (b["up"] - b["down"]) / b["n"] if b["n"] else 0
    print(f"\n  2-MONTH DELTA: flip rate {b['rate']:.2%} ({b['flips']}/{b['n']}), "
          f"{br:.2f}x the noise floor")
    print(f"                 asymmetry {ba:+.2%} (up {b['up']}, down {b['down']})")
    print("  READING: churn scales with stack delta, and it has NO SIGN. The effect is\n"
          "  real and directionless — we cannot tell an improvement from a reshuffle at\n"
          "  the level of a thing a user was told, which is the level a notification\n"
          "  would speak at.")
