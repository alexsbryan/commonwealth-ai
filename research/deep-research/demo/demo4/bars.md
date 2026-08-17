# DEMO-4 bars — the re-measured dr-local-loop battery

Order `deep-research-t1e` (T1.7 query formation — figure-hunting sub-questions,
triage admission, re-measurement). The numbers here are the scorer's own
(`arms/score-report-t1e.json`, `score-arms.py` C-class deterministic) — this file
is generated from that JSON, never hand-typed.

Protocol as pre-registered (`adversarial/pre-registration.md`): the full battery
re-ran against the FROZEN banks — same decks, same scorer, same model pin
(daemon :9741 — Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0
embed, tau 0.9, max-rounds 3), same driver (`arms/run-arms.sh`), budget 12/12.

## Per-question coverage — loop vs one-shot (scorer verbatim)

| seed | loop | one-shot | loop density | one-shot density | P3 | R-12 |
|---|---|---|---|---|---|---|
| seed-01 | 3/6 | 3/6 | never-ran | 1.0 | passed | failed |
| seed-02 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-03 | 5/7 | 5/7 | 1.0 | 1.0 | passed | failed |
| seed-04 | 5/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-05 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-06 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| seed-07 | 3/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-08 | 4/5 | 4/5 | 1.0 | 1.0 | passed | failed |
| seed-09 | 3/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-10 | 4/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-11 | 4/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-12 | 5/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| v1 | 3/16 | 7/16 | 0.731 | 1.0 | passed | failed |

pooled lift: **-0.11699999999999999** (loop 0.883 vs one-shot 1.0; bar loop >= one-shot + 0.10)

## The bar legs (scorer verbatim)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 52/72 | >=58/72 | failed — single-origin decks; the corroboration floor keeps coverage in open questions (honesty, reported separately) |
| P4-v1 (loop) | 3/16 | >=12/16 | failed — evidence-arbiter corrected forms applied per the frozen journal |
| P3 | 13/13 passed (+0 could-not-judge) | >=10/13 | passed — the v0 seeds all re-fetch the same exemplar (no fetch dedup); the v1 flight passed (round-2 fetched 0, coverage not worse) |
| R-12 | 0/12 v0 seeds | >=10/12 | failed — gap sets GROW on every seed (audits add single-origin floor caps); v1 journaled 1->26, not gated |
| T1.7 plan presence | 12/12 scoped flights | all scoped flights carry | passed — every figure-implying flight's plan sub-questions carry a digit or a measure word |
| two-arm lift (pooled) | 0.883 vs 1.0 | loop >= one-shot + 0.10 | failed — one-shot traces every numeric claim; the loop's flagged open-question claims stay untraced (see the honesty journal) |
| two-arm lift (v1) | 0.7307692307692307 vs 1.0 | loop >= one-shot + 0.15 | failed — single-question comparison |
| honesty not worse | ungrounded loop 0.11699999999999999 vs one-shot 0.0 | loop ungrounded <= one-shot | failed — letter leg: the loop's verdict-flagged claims (failed/could-not-judge) count as ungrounded; zero untraced numbers sit in [passed] position in ANY arm (both epochs, journaled) — t1e loop 0.117 < t1d 0.497 under the same instrument |

The instrument journal (scorer defects fixed before these numbers were committed,
before -> after, both directions) is in `adversarial/pre-registration.md`, t1e execution
section. P5 (poisoned-drill battery, 6/6, no noise band) is verified by
`demo/p5/verify.sh` and recorded in the DEMO-2 README — a separate gate, not scored here.
