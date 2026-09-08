#!/usr/bin/env python3
"""The table, and the PRE-REGISTERED verdict (manifest.md, written before the run).

Counts are of MATCHED ITEMS, not of ratios averaged: `sources 48/66` is what
the campaign's rows are in, and the exact retrieval band means any delta of
>= 1 fact or source is real (there is nothing to average away — the scorer is
deterministic given a fixed index).
"""
import json, os, sys

out, ARM_M, ARM_L, trials = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])

def totals(path):
    d = json.load(open(path))
    s_m = s_t = f_m = f_t = 0
    for r in d["results"]:
        s_m += len(r["source_score"]["matched"]); s_t += r["source_score"]["total_expected"]
        f_m += len(r["fact_score"]["matched"]);   f_t += r["fact_score"]["total_expected"]
    return s_m, s_t, f_m, f_t, len(d["results"])

rows, missing = {}, []
for arm in (ARM_M, ARM_L):
    rows[arm] = []
    for t in range(1, trials + 1):
        p = os.path.join(out, f"bank-{arm}-t{t}.json")
        if not os.path.exists(p):
            missing.append(p); continue
        rows[arm].append(totals(p))

print(f"{'arm':<34}{'trial':>6}{'sources':>12}{'facts':>12}{'questions':>11}")
for arm in (ARM_M, ARM_L):
    for i, (sm, st, fm, ft, n) in enumerate(rows[arm], 1):
        print(f"{arm:<34}{i:>6}{f'{sm}/{st}':>12}{f'{fm}/{ft}':>12}{n:>11}")

if missing:
    print("\nVERDICT-COULD-NOT-JUDGE  a trial produced no JSON:")
    for m in missing:
        print(f"  missing {m}")
    print("A run that did not produce its own inputs is a verdict about the RUNNER, not about pooling.")
    sys.exit(1)
if not rows[ARM_M] or not rows[ARM_L]:
    print("\nVERDICT-COULD-NOT-JUDGE  an arm produced no trials at all")
    sys.exit(1)

def best(arm, i):   # max across trials — retrieval is deterministic, so a
    return max(r[i] for r in rows[arm])   # spread means something moved, and is printed above.

sm_M, fm_M = best(ARM_M, 0), best(ARM_M, 2)
sm_L, fm_L = best(ARM_L, 0), best(ARM_L, 2)
d_s, d_f = sm_L - sm_M, fm_L - fm_M
print(f"\nL - M:  sources {d_s:+d}   facts {d_f:+d}")

spread = [max(r[i] for r in rows[a]) - min(r[i] for r in rows[a])
          for a in (ARM_M, ARM_L) for i in (0, 2)]
if any(spread):
    print(f"NOTE: trial spread {spread} is non-zero — prod-pipeline retrieval was expected to be "
          f"deterministic at a fixed index, so read the delta against this spread, not against 0.")

# Pre-registered, from manifest.md. Exact band: any delta >= 1 is real.
if d_s > 0 or d_f > 0:
    if d_s < 0 or d_f < 0:
        print("\nVERDICT-MIXED  one metric up, the other down — not the clean L>M the decision "
              "was pre-registered on; report both and do not recommend on this alone.")
    else:
        print("\nVERDICT-L-BETTER  the last-pooled arm retrieves more. RECOMMEND re-embed + "
              "re-publish of sep and wikipedia to the operator.")
elif d_s == 0 and d_f == 0:
    print("\nVERDICT-EQUAL  the reranker/FTS mask the space; re-publish is for PORTABILITY only, "
          "not for measured retrieval gain.")
else:
    print("\nVERDICT-M-BETTER  the last-pooled stack REGRESSES on the bench against the model's "
          "own spec. That is the finding, and it outranks the re-publish question.")
