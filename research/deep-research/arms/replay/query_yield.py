#!/usr/bin/env python3
"""Query-formation scoreboard for DRB flights — the acquisition metric that
sits UPSTREAM of ranking and fetching (campaign drb1-race, 2026-08-24).

WHY THIS METRIC. The fetch budget and the admission ranker both operate on a
pool the QUERIES retrieved. If the pool holds nothing on-topic, no cap and no
re-ranker can recover it. Measured on the logged t7a flight, that pool quality
varies THIRTY-FOLD across tasks — from 1.9% on-topic (task 65) to 56.7% (task
69) — which is the widest spread anywhere in the acquisition path. It is the
binding constraint, and until now it was not a number anyone tracked.

THE TWO SCORES, per (task, round):

  yield        on-topic hits / hits retrieved.  The query set's PRECISION —
               "of what these queries dragged back, how much was worth
               reading?"

  sufficiency  min(1, on-topic hits / cap).  The query set's RECALL against
               the round's actual fetch budget — "did these queries retrieve
               enough on-topic sources to SPEND the round's fetches on?"
               A round can post a fine yield and still starve if the pool is
               small, and a large pool with 3% yield starves the ranker
               instead. Sufficiency is the one that says whether the round
               could possibly have gone well.

WHAT IS *NOT* HERE, deliberately. Two structural proxies for "good query
shape" were pre-registered and BOTH FALSIFIED on these 17 rounds: the
figure-hunt fraction (measure-word or year present) scored Pearson r = 0.035,
and named-entity density scored r = -0.046. The eyeball pattern is real —
task 69's queries name documents that exist ("official specification of MCP")
while task 65's name statistics that may not ("median accuracy percentage of
deep learning models in wheat kernel classification") — but neither proxy
captures it, and a third hypothesis against the same 17 rounds would be
fishing, not measuring. So this file scores the OUTCOME and takes no position
on what causes it. The way to learn the cause is an intervention with this
metric pre-registered (AIQ's planner rules are the candidate: self-contained
narrow queries, no umbrella query, each mapped to a required component,
internal sources consulted first), not another correlate hunted out of the
data that suggested it.

Usage:  python3 query_yield.py [admission-labels.jsonl] [--cap 4]
"""
import argparse
import json
import os
import sys
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('labels', nargs='?',
                    default=os.path.join(HERE, 'admission-labels.jsonl'))
    # The DRB-I round fetch share: ceil(web_fetch_pages 12 / max_rounds 3).
    ap.add_argument('--cap', type=int, default=4,
                    help="the round's fetch budget, for sufficiency")
    a = ap.parse_args()
    if not os.path.exists(a.labels):
        sys.exit(f'exit 2: no labels at {a.labels} — run label_admission.py first')

    rows = [json.loads(l) for l in open(a.labels) if l.strip()]
    unlabelled = [r for r in rows if r.get('label') in (None, 'error', 'unparsed')]
    if unlabelled:
        # Absence is reported, never averaged away (§18.3).
        print(f'WARNING: {len(unlabelled)} of {len(rows)} rows carry no usable '
              f'label; they are EXCLUDED from every figure below.', file=sys.stderr)
    rows = [r for r in rows if r.get('label') not in (None, 'error', 'unparsed')]

    by = defaultdict(list)
    for r in rows:
        by[(r['task'], r['round'])].append(r)

    print(f'QUERY FORMATION SCOREBOARD   (fetch cap {a.cap}/round, n={len(by)} rounds)')
    print()
    print('%-6s %-6s %-8s %-8s %-9s %-8s %-13s' %
          ('task', 'round', 'queries', 'hits', 'on-topic', 'yield', 'sufficiency'))
    print('-' * 62)
    per_task = defaultdict(lambda: [0, 0])
    suff = []
    for k in sorted(by, key=lambda x: (int(x[0]), x[1])):
        g = by[k]
        qs = {q for r in g for q in r['queries']}
        ot = sum(1 for r in g if r['label'] == 'on-topic')
        y = ot / len(g)
        s = min(1.0, ot / a.cap)
        suff.append(s)
        per_task[k[0]][0] += len(g)
        per_task[k[0]][1] += ot
        flag = '  <-- STARVED' if s < 1.0 else ''
        print('%-6s %-6s %-8d %-8d %-9d %-8s %-8s%s' %
              (k[0], k[1], len(qs), len(g), ot, '%.0f%%' % (100 * y),
               '%.2f' % s, flag))

    print()
    print('per task (all rounds pooled):')
    for t in sorted(per_task, key=lambda x: int(x)):
        n, ot = per_task[t]
        y = 100 * ot / n
        print('  task %-4s %5.1f%%  %s' % (t, y, '#' * int(round(y / 2))))

    tot_hits = sum(v[0] for v in per_task.values())
    tot_ot = sum(v[1] for v in per_task.values())
    starved = sum(1 for s in suff if s < 1.0)
    print()
    print('HEADLINE  yield %.1f%% (%d on-topic of %d retrieved) | '
          'rounds that could not fill the fetch budget: %d of %d | '
          'mean sufficiency %.2f'
          % (100 * tot_ot / tot_hits, tot_ot, tot_hits, starved, len(suff),
             sum(suff) / len(suff)))
    return 0


if __name__ == '__main__':
    sys.exit(main())
