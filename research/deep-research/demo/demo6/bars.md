# DEMO-6 bars — the re-measured dr-local-loop battery (corpus search source)

Order `deep-research-t1g` (T1 rung 2 — the acquisition's search source
dispatch `mock | corpus`, pre-registered instrument change, re-measurement).
The numbers here are the scorer's own (`arms/score-report-t1g.json`,
`score-arms.py` C-class deterministic) — this file is generated from that
JSON, never hand-typed.

Protocol as pre-registered (`adversarial/pre-registration.md`): the full
battery re-ran against the FROZEN banks with the NEW instrument — the v1
report-class flight searched the ESTATE (`--search-source corpus --corpora
dr-demo6-v1`, the corpus built ONCE from the verbatim frozen v1 deck bodies
under `demo/demo6/deck-extract/` via the estate's shipped ingest surface);
the 12 v0 seeds kept the mock deck surface unchanged. Same decks, same
scorer, same model pin (daemon :9741 — Qwen3.6-35B-A3B-MTP-UD-Q6_K draft,
Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9, max-rounds 3), same driver
(`arms/run-arms.sh`), budget 12/12. The t1f numbers (P4-v0 53/72, P4-v1
9/16) are old-instrument numbers, never mixed.

## Per-question coverage — loop vs one-shot (scorer verbatim)

| seed | loop | one-shot | loop density | one-shot density | P3 | R-12 |
|---|---|---|---|---|---|---|
| seed-01 | 3/6 | 3/6 | 1.0 | 1.0 | passed | failed |
| seed-02 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-03 | 4/7 | 5/7 | 1.0 | 1.0 | passed | failed |
| seed-04 | 4/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-05 | 5/6 | 5/6 | 1.0 | 0.75 | passed | failed |
| seed-06 | 6/6 | 6/6 | 1.0 | 1.0 | passed | failed |
| seed-07 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-08 | 2/5 | 3/5 | None | 1.0 | passed | failed |
| seed-09 | 3/6 | 2/6 | 1.0 | 1.0 | passed | failed |
| seed-10 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-11 | 4/6 | 4/6 | 1.0 | 1.0 | passed | failed |
| seed-12 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| v1 | 2/16 | 11/16 | 0.7 | 1.0 | passed | failed |

pooled lift: **-0.04300000000000004** (loop 0.938 vs one-shot 0.981; bar loop
>= one-shot + 0.10) — the direction flipped AGAIN, this time the one-shot
side: the corpus flight's thin window (3 chunks, none value-shaped) left the
loop's report with era-year figures that do not trace (see the honesty note).

## The bar legs (scorer verbatim)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 51/72 | >=58/72 | failed — single-origin decks; the corroboration floor keeps coverage in open questions (honesty, reported separately) |
| P4-v1 (loop) | 2/16 | >=12/16 | failed — evidence-arbiter corrected forms applied per the frozen journal |
| P3 | 13/13 passed (+0 could-not-judge) | >=10/13 | passed — the v0 seeds all re-fetch the same exemplar (no fetch dedup); the v1 flight passed (round-2 fetched 0, coverage not worse) |
| R-12 | 0/12 v0 seeds | >=10/12 | failed — gap sets GROW on every seed (audits add single-origin floor caps); v1 journaled 1->26, not gated |
| T1.7 plan presence | 12/12 scoped flights | all scoped flights carry | passed — every figure-implying flight's plan sub-questions carry a digit or a measure word |
| two-arm lift (pooled) | 0.938 vs 0.981 | loop >= one-shot + 0.10 | failed — one-shot traces every numeric claim; the loop's flagged open-question claims stay untraced (see the honesty journal) |
| two-arm lift (v1) | 0.7 vs 1.0 | loop >= one-shot + 0.15 | failed — single-question comparison |
| honesty not worse | ungrounded loop 0.062000000000000055 vs one-shot 0.019000000000000017 | loop ungrounded <= one-shot | failed — letter leg: the loop's verdict-flagged claims (failed/could-not-judge) count as ungrounded; zero untraced numbers sit in [passed] position in ANY arm (both epochs, journaled) — t1e loop 0.117 < t1d 0.497 under the same instrument |

The instrument journal (the T1 rung-2 source-dispatch change, pre-registered
BEFORE the re-measure; the scorer itself unchanged, C-class) is in
`adversarial/pre-registration.md`, t1g execution section. The v1 corpus
flight's measured mechanism — LanceDB's hybrid relevance scores quantize to
identical f32 buckets (~0.0333) for the top hit of every query, the triage's
figure-bearing tie-break reads only the TITLE (chunk titles are digit-free
document names), so the top-k admission degenerates to insertion order and
the admitted chunks carried no value-shaped figures — is journaled there and
in `demo/demo6/README.md`. P5 (poisoned-drill battery, 6/6, no noise band)
is verified by `demo/p5/verify.sh` — a separate gate, not scored here.
