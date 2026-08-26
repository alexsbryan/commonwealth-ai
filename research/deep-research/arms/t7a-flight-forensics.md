# t7a DRB-I flight — forensics write-up

Flight: 2026-08-20, order deep-research-t7a (amended), DRB-I RACE measurement of
the loop AS-IS. Driver: `drb/run-drb-arms.py` std arm (`--backend auto
--search-source web --consent personal --search 12 --fetch 12 --max-rounds 3`),
one systemd-run unit, pre-registered cumulative-search cap 96. Ten frozen
DRB-I tasks; **9 flew, task 95 never-ran** (cumulative 102 ≥ 96, boundary stop,
per-task allowance untouched).

## Flight ledger

| task | rounds | ev | stalled | density | search | fetch | gap trace | empty rounds | claims | ungrounded |
|---|---|---|---|---|---|---|---|---|---|---|
| 56 | 3 | 1 | 0 | 0.333 | 6 | 1 | [0,1],[1,1],[1,2] | r1 failed, r3 failed | 5 | 0 |
| 58 | 3 | 3 | 0 | 1.000 | 12 | 12 | [0,1],[1,9],[9,8] | — | 14 | 0 |
| 59 | 3 | 3 | 0 | 1.000 | 12 | 10 | [0,1],[1,7],[7,7] | — | 8 | 0 |
| 62 | 2 | 2 | 0 | 1.000 | 12 | 8 | [0,1],[1,9] | — | 22 | 0 |
| 65 | 3 | 3 | 0 | 1.000 | 12 | 10 | [0,1],[1,2],[2,1] | — | 7 | 0 |
| 69 | 3 | 3 | 0 | 1.000 | 12 | 16 | [0,1],[1,8],[8,13] | — | 28 | 0 |
| 78 | 2 | 2 | 0 | 1.000 | 12 | 4 | [0,1],[1,7] | — | 8 | 0 |
| 83 | 2 | 2 | 0 | 1.000 | 12 | 6 | [0,1],[1,3] | — | 10 | 0 |
| 90 | 3 | 3 | 0 | 1.000 | 12 | 12 | [0,1],[1,19],[19,21] | — | 35 | 0 |

Totals: 102 searches (cap stop), 79 fetches, 137 claims, **0 ungrounded
(honesty 1.0 on all 9)**. `truncation_declared=true` on 9/9 manifests.

## Score (official RACE, 27B judge; flights/race-20260820T151943)

**Overall 15.6783** — dims C 15.53 / I 13.42 / IF 17.84 / R 17.41;
9/9 scored. Vs the official 9-task references (42.0849 gemini-era,
44.9237 GPT-5.5 era): **61-63% below on every dimension**, no dim
recovers. Weakest relative: Insight (0.350 of reference), strongest:
Readability (0.401). Above the t5a-era local arm (8.0848, 122B judge)
— the t6d word-number/re-draft fixes landed — same regime, ~2.6x
below the official reference.

Per-task overalls (recipe-computed): 56: 5.93, 58: 16.76, 59: 19.30,
62: 26.52, 65: 7.00, 69: 28.11, 78: 7.39, 83: 2.84, 90: 27.25. The
four worst tasks are exactly the four with the strongest acquisition
pathology: 83 (F4 2-round stop, 6 fetches) 2.84, 65 (F5 7 claims)
7.00, 56 (F2 fetch-failure, 1 fetch) 5.93, 78 (F4 2-round, 4 fetches)
7.39. The three best (69, 90, 62) are the growth-with-evidence class
(28-35 claims, 8-16 fetches). Estate size tracks score: fetch count
1→16 across tasks maps onto 2.84→28.11.

## Taxonomy (by frequency, flight n=9; battery n=26 for the refused class)

**F1 — search front-loading; later rounds never re-search (8/9 tasks).**
Round-1 `search_calls=12`; final-round `search_calls=0` on 62, 78, 83, 90
(and 2, 3, 6 on 56/90's middle rounds). Rounds 2+ fetch from the SAME
admitted query set — gaps opened by those fetches are never re-searched, so
the estate caps at round-1 query breadth. This is the t6a "searches are not
content" signature now measured at round granularity. Most load-bearing
class: it bounds the Comprehension/Insight dims directly.

**F2 — fetch-failure empties (1/9; task 56, density 0.333).** Rounds 1 and 3
fetched 0 with `empty_round_reason=failed` (admitted fetches failed at the
web layer), burning 2 of 3 rounds and 6 of 12 searches for 1 fetch. No retry
or backoff in the fetch path. Task 56 round 3 also shows the battery's
**growth-on-empty** class recurring as the tail of the fetch-failure class
(gaps 1→2 with fetched=0).

**F3 — render truncation on every task (9/9).** `truncation_declared=true`
on all manifests; the strict-shape re-draft caps the report. The t6d battery
#5 already identified this as the P4-v1/P3 re-draft loss; it constrains
Readability / Instruction-Following everywhere.

**F4 — early 2-round termination with open gaps (3/9: 62, 78, 83).** The
loop ended after 2 rounds with gaps still growing (1→9, 1→7, 1→3). The
runner's own stop condition — not the driver's max-rounds 3 — ended the
acquire loop; round 3 was in budget and unused.

**F5 — under-fetch vs search budget (5/9: 56, 65, 69, 78, 83).** Fetches
(1–16) run below the 12/12 allowance; admission is conservative.

**Positive: growth-with-evidence (69: 1→8→13, 16 fetches; 90: 1→19→21, 12
fetches, 35 claims)** — the estate grows AND evidence arrives; the class the
battery's growth-on-empty is the pathology of. Closing/flat loops (58, 59,
65) are the healthy signature.

## Per-dimension loss weighting (which class costs which dim)

The loss is flat across dims (all ~60-63% below reference), so the
weighting is by acquisition pathology, not by dim asymmetry — the
class that caps the estate caps ALL dims. Estimated loss share by
class (frequency × per-task spread):

- **F1 (search front-loading, 8/9)** — largest single lever: it
  bounds C and I directly (the two dims at the bottom of the
  relative table) by capping the estate at round-1 query breadth;
  the four worst tasks all have flat round profiles. Every dim's
  shortfall carries it.
- **F4 (early 2-round stop, 62/78/83)** — the strongest
  per-task cost: 83's 2.84 (worst of flight) and 78's 7.39 are
  the two smallest estates with the round budget unused. Fix
  priority 2 is worth more than its 3/9 frequency suggests.
- **F2 (fetch-failure empty, 56)** — 5.93 with 1 fetch: one task,
  but a pure-total loss of its rounds (2 of 3 rounds empty).
- **F3 (render truncation, 9/9)** — caps IF/R (17.84/17.41, the
  strongest dims but still 60% below): the re-draft cap is a
  ceiling on what the judge can read, so it weighs on every dim's
  top end.
- **F5 (under-fetch, 5/9)** — interacts with F1/F4: admission
  conservatism is what leaves the estate small even where the
  budget existed.

## Ranked fix priorities (vs the AIQ teardown)

1. **F1 — re-search from the new gap set in later rounds** (t6f gap-driven
   acquisition scope; this flight is its measurement). The loop derives
   queries once at round 1; later rounds must derive queries FROM the new
   gaps. Structural, no model change: the query-derivation step runs per
   round over `gaps_after` instead of once.
2. **F4 — a round-3 acquire rule.** The runner's stop condition must
   distinguish "gaps closed" from "round budget exhausted": a done-partial
   with open gaps and remaining round budget must consume the round.
3. **F2 — fetch retry/backoff** (t7b silent-empty-window scope; the
   `empty_round_reason` instrumentation this flight used is the diagnosis,
   the retry is the fix). Web fetch failure of an admitted query should
   retry before the round is declared empty.
4. **F3 — render truncation** (t6d-identified): the strict-shape re-draft cap
   is the P4-v1/P3 loss; measured here as 9/9 truncation_declared.
5. **AIQ adoptions** (teardown verdicts, aligned):
   - citation whitelist registry stamp (writer cites only captured sources —
     cheap structural upgrade to the render step; our honesty is already 1.0,
     the registry makes it structural),
   - configurable-downward limits (the driver enforced the 96-search cap
     here; the runner should own the ceiling so it can only tighten),
   - concurrent dispatch — only after the cost/throughput ledger decides
     (teardown item 4; this flight's ledger: 102 searches / 79 fetches / 9
     tasks, serial fetch wall-time).
   - verification semantics: **skip** (identity-only; keep our floor).
