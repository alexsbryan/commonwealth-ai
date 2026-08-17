# DEMO-5 bars — the re-measured dr-local-loop battery (term-ranked retrieval)

Order `deep-research-t1f` (T1.9 realistic mock retrieval — term-ranked gym
search, pre-registered instrument change, re-measurement). The numbers here
are the scorer's own (`arms/score-report-t1f.json`, `score-arms.py` C-class
deterministic) — this file is generated from that JSON, never hand-typed.

Protocol as pre-registered (`adversarial/pre-registration.md`): the full
battery re-ran against the FROZEN banks with the NEW instrument — the deck's
term index (per-hit relevance counts instead of the old exact-value
instrument's flat 0.9 ties) — same decks, same scorer, same model pin (daemon
:9741 — Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0 embed,
tau 0.9, max-rounds 3), same driver (`arms/run-arms.sh`), budget 12/12. The
old-instrument numbers (t1e: P4-v0 52/72, P4-v1 3/16 loop vs 7/16 one-shot)
are cited as old-instrument numbers, never mixed.

## Per-question coverage — loop vs one-shot (scorer verbatim)

| seed | loop | one-shot | loop density | one-shot density | P3 | R-12 |
|---|---|---|---|---|---|---|
| seed-01 | 4/6 | 3/6 | 1.0 | 1.0 | passed | failed |
| seed-02 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-03 | 4/7 | 5/7 | 1.0 | 1.0 | passed | failed |
| seed-04 | 4/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-05 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-06 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| seed-07 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-08 | 5/5 | 2/5 | 1.0 | never-ran | passed | failed |
| seed-09 | 2/6 | 3/6 | 1.0 | 1.0 | passed | failed |
| seed-10 | 4/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-11 | 4/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-12 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| v1 | 9/16 | 9/16 | 1.0 | 0.9473684210526315 | failed | failed |

pooled lift: **0.02100000000000002** (loop 1.0 vs one-shot 0.979; bar loop >=
one-shot + 0.10)

## The bar legs (scorer verbatim)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 53/72 | >=58/72 | failed — single-origin decks; the corroboration floor keeps coverage in open questions (honesty, reported separately) |
| P4-v1 (loop) | 9/16 | >=12/16 | failed — evidence-arbiter corrected forms applied per the frozen journal |
| P3 | 12/13 passed (+0 could-not-judge) | >=10/13 | passed — the v0 seeds all re-fetch the same exemplar (no fetch dedup); the v1 flight passed (round-2 fetched 0, coverage not worse) |
| R-12 | 0/12 v0 seeds | >=10/12 | failed — gap sets GROW on every seed (audits add single-origin floor caps); v1 journaled 1->26, not gated |
| T1.7 plan presence | 12/12 scoped flights | all scoped flights carry | passed — every figure-implying flight's plan sub-questions carry a digit or a measure word |
| two-arm lift (pooled) | 1.0 vs 0.979 | loop >= one-shot + 0.10 | failed — one-shot traces every numeric claim; the loop's flagged open-question claims stay untraced (see the honesty journal) |
| two-arm lift (v1) | 1.0 vs 0.9473684210526315 | loop >= one-shot + 0.15 | failed — single-question comparison |
| honesty not worse | ungrounded loop 0.0 vs one-shot 0.02100000000000002 | loop ungrounded <= one-shot | passed — letter leg: the loop's verdict-flagged claims (failed/could-not-judge) count as ungrounded; zero untraced numbers sit in [passed] position in ANY arm (both epochs, journaled) — t1e loop 0.117 < t1d 0.497 under the same instrument |

The instrument journal (the T1.9 term-ranked change, pre-registered BEFORE
the re-measure; the scorer itself unchanged, C-class) is in
`adversarial/pre-registration.md`, t1f execution section. Two verdicts-note
sentences above are stale copies of the t1e-era prose (P3's "the v1 flight
passed" — the pair detail shows the v1 coverage drop 10 -> 9, journaled; the
pooled-lift note's "flagged open-question claims stay untraced" — t1f loop
density is 1.0, nothing untraced): reproduced verbatim, never edited — the
scorer is frozen, and the discrepancy is journaled, not silently corrected.
P5 (poisoned-drill battery, 6/6, no noise band) is verified by
`demo/p5/verify.sh` — a separate gate, not scored here.
