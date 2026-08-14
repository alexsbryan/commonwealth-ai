# Portfolio shared baseline — 21 desktop turns, 2026-08-14 (seat-run)

Instrument: launchd one-shot driving the command bridge (:9745), fresh
conversation per turn, runner modeled on desktop_bridge.rs::run_bridge_live
(runs/portfolio-baseline/baseline_runner.py). Build: HEAD at cc19f26e-era
landed set + 323faf3d (replay seams, markdown/docs only at runtime).
Primary: turn 0 (139.0s) treated as warmup and EXCLUDED; turns 1-20 warm.
Quiet box: seat held daemon claim; workers under a no-cargo hold for the
whole window. Forensics ledger for the same turns:
gate_audit_forensics_20260814_portfolio_baseline.jsonl (567 rows).

## Walls (n=20 warm)

median 95.75s (bar <=75)   p90 118.2s (bar <=90)   min 52.8   max 157.4

This is the pre-portfolio BEFORE-ARM at the landed set, not a new
E-wall-time transition arm — the bar transitions on the composed arm after
the portfolio lands.

## Composition — the question this draw existed to settle

actions: released 5 / rewrite_released 6 / rewrite_annotated 9
CLEAN RATE 5/20 = 25%.
clean walls  52.8 / 59.0 / 60.7 / 68.0 / 68.2  — ALL under the 75s bar,
  and 52.8s is the fastest app turn ever measured (prior best 62.1s).
rewrite walls 67.9-157.4 (one fast outlier 67.9; the band proper is 84+).

Fisher, this draw 5/20 vs pre-B 3/6: p=0.330.
Pooled post-B 7/32 (22%) vs pre-B 3/6 (50%): p=0.310.

VERDICT ON THE COMPOSITION QUESTION: the post-B clean rate is now
well-estimated at ~22-25%. Whether it DIFFERS from pre-B remains formally
unresolvable — the binding constraint is the pre-B comparator's size (n=6),
which no amount of new sampling can fix. Recorded as: estimate settled,
delta question closed as could-not-judge-by-construction. What matters
forward is the estimate itself: at 25% clean, the median sits in the
rewrite band (95.75s), and the two levers in flight aim exactly at it —
etiology D2 raises the clean rate (58%-class failures are input artifacts),
the tombstone caps what the other 75% of turns pay.

## E-draft-grounding baseline for this draw

Zero-failure (released) turns: 5/20. Per-claim ratios live in the
forensics ledger; the etiology worker's D3 scores them against the D0
taxonomy.
