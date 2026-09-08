#!/usr/bin/env python3
"""The WALK's own counters off a run log — never inferred from the score.

STRIP ANSI FIRST. `tracing` writes escapes BETWEEN a field name and its `=`,
so a naive `summary_seeds=(\\d+)` matches nothing and returns a clean,
plausible ZERO — indistinguishable from "the walk reached no Summary", which
is the finding these lanes exist to measure. That trap cost ei-7a a false
result in its first round; the substitution below is the fix (§18.4).
"""
import re, sys

ansi = re.compile(r"\x1b\[[0-9;]*m")
txt = ansi.sub("", open(sys.argv[1], errors="replace").read())
tag = sys.argv[2]
lines = [l for l in txt.splitlines() if "ground: walk ledger" in l]

def tot(f):
    return sum(int(m) for l in lines for m in re.findall(rf"\b{f}=(\d+)", l))

kinds = {}
for l in lines:
    k = re.search(r'kind="?([a-z]+)"?', l)
    if k:
        kinds[k.group(1)] = kinds.get(k.group(1), 0) + 1

# The absence line ei-5c added: a corpus with RAPTOR rows and no Summary atoms.
absent = [l.split("atlas-grounding:")[-1].strip()[:160]
          for l in txt.splitlines() if "carry RAPTOR summary rows but no" in l]

print(f"YIELD_{tag}: walks={len(lines)} seeds={tot('seeds')} "
      f"dropped_seed_kind={tot('dropped_seed_kind')} "
      f"dropped_seed_budget={tot('dropped_seed_budget')} "
      f"summary_seeds={tot('summary_seeds')} "
      f"suppressed={tot('summary_expansions_suppressed')} "
      f"summaries_appended={tot('summaries_appended')} rows={kinds}")
if absent:
    print(f"ABSENCE_{tag}: {len(absent)} named line(s), first: {absent[0]}")
else:
    print(f"ABSENCE_{tag}: none logged")
