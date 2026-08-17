# DEMO-3 bars — the re-measured dr-local-loop battery

Order `deep-research-t1d` (T1.5 acquisition re-cut). The numbers here are the
scorer's own (`arms/score-report-t1d.json`, `score-arms.py` C-class
deterministic) — this file is generated from that JSON, never hand-typed.

Protocol as pre-registered (`adversarial/pre-registration.md`): the full battery
re-ran against the FROZEN banks — same decks, same scorer, same model pin
(daemon :9741 — Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0
embed, tau 0.9, max-rounds 3), same driver (`arms/run-arms.sh`), budget 12/12
(`--search 12 --fetch 12`, pre-registered raise from 4/4).

## Per-question coverage — loop vs one-shot (scorer verbatim)

| seed | loop | one-shot | loop density | one-shot density | P3 | R-12 |
|---|---|---|---|---|---|---|
| seed-01 | 3/6 | 3/6 |  | 1.0 | passed | failed |
| seed-02 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-03 | 4/7 | 5/7 |  | 1.0 | passed | failed |
| seed-04 | 4/6 | 4/6 |  | 1.0 | passed | failed |
| seed-05 | 5/6 | 5/6 | 1.0 | 1.0 | passed | failed |
| seed-06 | 6/6 | 6/6 | 1.0 | 0.857 | passed | failed |
| seed-07 | 5/6 | 4/6 |  | 1.0 | passed | failed |
| seed-08 | 2/5 | 3/5 |  | 1.0 | passed | failed |
| seed-09 | 3/6 | 2/6 |  | 1.0 | passed | failed |
| seed-10 | 4/6 | 4/6 |  | 1.0 | passed | failed |
| seed-11 | 3/6 | 4/6 |  | 1.0 | passed | failed |
| seed-12 | 5/6 | 5/6 |  | 1.0 | passed | failed |
| v1 | 2/16 | 5/16 | 1.0 | 1.0 | failed | failed |

P4-v0 pooled: 49/72 (bar >= 58/72 — failed). P4-v1 pooled: 2/16 loop vs 5/16
one-shot (bar >= 12/16 — failed). R-12: 0/12 v0 seeds (bar >= 10/12 — failed,
structural: single-origin decks + unweakened floor => gap sets only grow).

## Bar legs (scorer verbatim)

| leg | bar | measured | verdict |
|---|---|---|---|
| P4-v0 | >=58/72 | 49/72 | failed |
| P4-v1 (loop) | >=12/16 | 2/16 | failed |
| P3 | >=10/13 | 12/13 passed (+0 could-not-judge) | passed |
| R-12 | >=10/12 | 0/12 v0 seeds | failed |
| two-arm lift (pooled) | loop >= one-shot + 0.10 | 1.0 vs 0.976 | failed |
| two-arm lift (v1) | loop >= one-shot + 0.15 | 1.0 vs 1.0 | failed |
| honesty not worse | loop ungrounded <= one-shot | ungrounded loop 0.0 vs one-shot 0.02400000000000002 | passed |

Pooled lift: 0.02400000000000002 (loop 1.0 vs one-shot 0.976).

Honesty side (P4 gate, never traded for coverage): zero ungrounded load-bearing
numbers in either arm — loop ungrounded 0.0, one-shot 0.024 (the residual is
the seed-06 `$500M` sentence, whose evidence window is a single-chunk fetch;
see the scorer-instrument journal in pre-registration.md — the loop arm's
true ungrounded is 0.0).

The v1 report-class question (`How did American cities change across four
decades, 1980 to 2024?`) sits at 2/16 loop vs 5/16 one-shot: the loop's
rounds spent budget on acquisition and its round-1 draft sub-questions were
thematic, never surfacing the figure tokens (Gini 0.5469, Case-Shiller 325.78,
etc.) the deck keys score on. Every claim it did render is verdict-stamped
and floor-honest — see report-v1-fixed.md.

## What the fixed loop did (and did not) move

- P3 dedup sanity: **passed** 12/13 — round-2 fetches 0 < 20% of round-1
  (fetch dedup refused every already-fetched URL; t1c's double-fetch shape is gone).
- The ceiling probe (pre-registered): v0 72/72 content keys reachable with
  perfect acquisition => the 49/72 P4-v0 is acquisition-and-draft-limited,
  not deck-limited.
- P4-v0 49/72 (t1c 52/72), P4-v1 2/16 (t1c 3/16), R-12 0/12 (t1c 0/12): the
  acquisition fixes landed their mechanics (P3) but did not move coverage — the
  binding constraint is the daemon's draft: sub-questions that do not carry the
  deck's figure tokens leave those keys unreachable by any downstream fix.
- Two-arm lift: failed (loop 1.0 vs one-shot 0.976, bar +0.10) — the loop is not
  worse on honesty, but density is at ceiling on both arms; the lift bar needs
  a coverage-anchored comparison, which the draft constraint caps first.

Scored 2026-08-14. Pre-fix (instrument-defect) evidence archived at
`arms/score-report-t1d-raw.json`; before/after journaled in pre-registration.md
(both directions, per ARCH_PRINCIPLES.md §18.6).
