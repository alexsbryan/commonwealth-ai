# DEMO-3 — the fixed acquisition loop rendering the v1 report-class question

Order `deep-research-t1d` (T1.5 acquisition re-cut). The v1 question is the
report-class question the acquisition fixes exist to serve:

> "How did American cities change across four decades (1980 to 2024)?"

This demo shows that question rendered by the **fixed** loop — breadth
frontier on round 1, fetch dedup on rounds 2+ , floor-capped second-origin
queries — as a real report where every number is attributable and every
absence is named.

## What is in this directory

| File | What it is |
|---|---|
| `report-v1-fixed.md` | The v1 flight's report (verdict-stamped claims, chunk-level citations), with the re-measured bars beside it |
| `bars.md` | The re-measured bars — **the scorer's own numbers** (score-report-t1d.json), never hand-typed |
| `verify-demo3.sh` | The honesty strips — the demo is only as strong as its verification |
| `README.md` | This file |

The raw artifacts live in the battery's run dir
`research/deep-research/arms/runs/loop/v1/dr-*/` — the manifest, the plan,
the per-round fetch lists, gap lists, evidence windows, skip ledgers and
the budget ledger are all there, as recorded by the shipped CLI on the
mock-deck surface.

## What the fixed loop did differently on this question

1. **Breadth (fix 2).** Round 1 carries the acquisition frontier: the
   plan's sub-questions are materialized as round-1 queries
   (`formed_by: "plan-subquestion"` in `plan.json` + `fetch-list-1.json`).
   On this flight: 4 frontier queries beyond the gap-template (the t1c
   shape had 1), lifting deck coverage from 4/11 (question alone) to
   6/11. The 5 still-uncovered hits are the figure-token sources the
   daemon's thematic sub-questions never surfaced — Gini 0.5469,
   Case-Shiller 325.78, New Orleans income-inequality ratio, white-share
   bachelor, manufacturing jobs — named, not hidden, and scored below.
   The unit contract (round-1 covers every deck hit when the frontier
   carries the figure tokens) is pinned by
   `round1_queries_cover_every_deck_hit` in deep_research/mod.rs.
2. **Fetch dedup (fix 1).** A URL fetched in round 1 is refused in
   rounds 2-3 (`dedup_refused` in the evidence windows) — the loop
   spends no budget re-fetching what it already holds.
3. **Second-origin targeting (fix 3).** When the corroboration floor
   caps a claim (single origin), the next round's gap query is a
   **fact query** built from the claim's load-bearing figures, and the
   floor's record rides the formed query (`corroboration` on the
   fetch-list row) — the artifact answers "why this query".

## How to verify

```bash
./verify-demo3.sh
```

The strips, in order:

1. the v1 flight exists and terminated (report.md + terminal manifest);
2. every claim in the report is verdict-stamped — a claim with no
   verdict is a silent number;
3. every figure token in the report's claims appears in the run's
   accumulated evidence window, or the claim is flagged
   could-not-judge/never-ran (absence named);
3b. the acquisition mechanics on THIS flight are the fixed loop's —
   round-1 carries the acquisition frontier (4 plan-subquestion
   queries beyond the gap-template, vs the question alone), and the
   frontier measurably lifts deck coverage (6/11 vs 4/11 with the
   question alone) with the uncovered hits NAMED, never hidden;
   rounds 2+ refused re-fetches (4 dedup refusals), and floor-capped
   second-origin queries carry the floor's record (15 on this
   flight);
4. `bars.md` carries the scorer's per-question covered fractions and
   bar legs verbatim — the bars are the scorer's numbers, never
   hand-typed;
5. the two-arm lift is computed by the same scorer over the same pairs.

The floor is the unweakened `CORROBORATION_FLOOR = 2`: a claim passes
only on ≥2 distinct source origins. This demo's reports are the
floor's honest output — passed where corroborated, could-not-judge
where the deck capped it, never a silent number.

## Re-produce

```bash
cd research/deep-research/arms
./run-arms.sh                       # 13 flights: 12 v0 + v1 (12/12 budget, pre-registered)
python3 score-arms.py --pairs runs/pairs.json \
    --loop runs/loop --oneshot runs/oneshot \
    --out runs/../score-report-t1d.json
cd ../demo/demo3
./verify-demo3.sh
```
