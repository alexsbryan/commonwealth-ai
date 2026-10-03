#!/usr/bin/env python3
"""EI1's unit — cited-with-evidence — computed the way the campaign bar does.

The bar's instrument reads a COMMITTED dated baseline; this prints the same
number off a fresh artifact so a run can be read before it is committed. The
denominator excludes errored turns: a failed turn is not a zero, it is an
exclusion, and it is counted so the exclusion is visible (§18.3).
"""
import json, sys

path, tag = sys.argv[1], sys.argv[2]
try:
    rows = (json.load(open(path)).get("results") or [])
except Exception as e:
    print(f"EI1_{tag}: UNREADABLE ({e})")
    raise SystemExit(0)

den = named = evid = cited = skipped = 0
for r in rows:
    if r.get("error"):
        skipped += 1
        continue
    fs = r.get("fact_score") or {}
    cs = ((r.get("synth") or {}).get("chunks_fact_score")) or {}
    exp = fs.get("total_expected", 0)
    n = set(fs.get("matched") or [])
    e = set(cs.get("matched") or [])
    den += exp
    named += len(n)
    evid += len(e)
    cited += len(n & e)

r4 = lambda x: f"{x}/{den} {x/max(den,1):.4f}"
print(f"EI1_{tag}: cited-with-evidence {r4(cited)} | named {r4(named)} | "
      f"evidenced {r4(evid)} | questions={len(rows)} errored={skipped}")
