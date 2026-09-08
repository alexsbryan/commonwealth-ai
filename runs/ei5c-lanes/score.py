#!/usr/bin/env python3
"""Facts and sources off one `eval run --format json` artifact.

One scorer for every lane in this unit, so two rows in the report cannot have
been computed two ways (ARCH §10.6). Reads the same fields the committed
baselines carry, and says `absent` rather than 0 for a field a run did not
produce (§18.3) — a zero and a missing key look identical in a table.
"""
import json, sys

path, tag = sys.argv[1], sys.argv[2]
try:
    d = json.load(open(path))
except Exception as e:
    print(f"SCORE_{tag}: UNREADABLE ({e})")
    raise SystemExit(0)
rows = d.get("results") or []
errs = sum(1 for r in rows if r.get("error"))

def over(key, num, den):
    m = t = 0
    seen = False
    for r in rows:
        s = r.get(key)
        if not isinstance(s, dict):
            continue
        seen = True
        m += len(s.get(num, []))
        t += s.get(den, 0)
    if not seen:
        return "absent"
    return f"{m}/{t} ({100*m/max(t,1):.1f}%)"

print(f"SCORE_{tag}: questions={len(rows)} errors={errs} "
      f"limit={d.get('limit', 'absent')} "
      f"facts={over('fact_score', 'matched', 'total_expected')} "
      f"sources={over('source_score', 'matched', 'total_expected')}")
