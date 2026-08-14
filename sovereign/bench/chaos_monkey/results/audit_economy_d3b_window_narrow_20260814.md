# D3 candidate B — answer-conditioned window narrowing: REFUSED on both bars (order audit-economy)

2026-08-14, run under directive c67d075e resolution "(3)-then-(1)": B priced
first for the record, then A' ships through the full live discipline. All
model calls on the local daemon — zero external model tokens. Candidate
build: branch `measure/audit-economy-d3b-window-narrow` (6df83313, based on
main 9e4a7aed), preserved. Verdict files:
`judge_replay_20260814_d3b_scan.verdicts.jsonl` (live, `--repeat 2`),
`judge_replay_20260814_d3b_render.jsonl` (render-only fingerprints). Bars:
the D0 pre-registration (approved 573c4c48).

## The candidate

`scan_unsupported_specifics` keeps whole chunks ranked by distinct
content-word overlap with the answer, greedily under a byte budget, emitted
in original (leaf-then-summary) order — conditioning, never intra-chunk
truncation, per the D0 constraint. Budget derived, not guessed: scan wall
bar <=5.0s at the D0-measured 9.7s/39K-char rate => prompt <=~19.4K chars
=> ~12K evidence chars (a 62% cut of the bank's 31.7K window, 36 chunks ->
~13 kept).

## Prediction registered before the run

Mechanism expected to fail should_not_flag (dropped support manufactures
absences, the 95b82f97 class) and to floor near ~4.6-5s — far above A's
1.17s. Both halves were WRONG in the details, and the record keeps that:
the failure landed on the catch side instead, and the cost model was
falsified upward.

## Instrument validation

- Render facet: candidate touches ONLY the scan register —
  per_claim_judge 28/28, chunk_judge 3/3, batched_support 23/23 rendered
  byte-identical to main (fnv match against
  `judge_replay_20260814_main/batched.verdicts.jsonl`).
- Model facet: `--repeat 2`, item-level output identical 9/9 cases.

## Quality — the registered bars

| arm | should_not_flag (6, bar: <=3 flagged) | should_flag (3, bar: 3/3) | total |
|---|---|---|---|
| main (production) | 6/6 flagged — FP engine | 3/3 caught | 3/10 |
| A' (family join, ad46c715) | 2 flagged — PASS | 2/3 — FAIL | 7/10 |
| **B (window narrowing)** | **1 flagged — PASS** | **0/3 — FAIL** | **6/10** |

B loses ALL THREE catches: Chisholm-and-Pereboom (misattribution),
Kane-bridge parenthetical (stitch), Keynes (parametric garnish). On each of
those cases it flags something ELSE instead (a Locke framing item, an
Edwards/Dennett-Wolfram item, a Fischer/Russell item) — plausible
manufactured-absence annotations on UNLABELED items, i.e. the 95b82f97
class surfacing on the unlabeled side while the labeled catches vanish. The
one labeled FP it does keep is "No Forking Paths" (support likely dropped
from the window). A judge that cannot see the whole evidence cannot testify
about fabrication against it, in either direction.

## Cost — the projected win did not survive measurement

Daemon-log per-call mechanics (16 of 18 calls isolated; the items=0 case's
small responses fell below the extraction filter):

- First scan of a case (the live-turn shape): **median 8.69s** (8.58s
  excluding one 25.4s contended outlier), tokens ~4.0-4.3K prefill.
- Repeat (pin restored, ~17-18ms, suffix ~1.2-1.4K tok): median 5.63s.
- Production baseline: 9.7s. Candidate A: 1.17s.

The 62% window cut bought **~11%**, not the ~52% the linear-prefill model
projected: at this prompt size the decode (~150-330 tokens at temp 0) and
non-linear prefill throughput dominate, and the narrowed window still pays
its own-family prefill every fresh turn. **Fails the <=5.0s scan bar even
at its warmest repeat shape.** The only lever that actually moved the scan
term is A's family join (prefill shared with the judges), which is a
different mechanism entirely.

## VERDICT: REFUSED — both registered bars failed

should_flag 0/3 (bar 3/3; strictly worse than A'/A's 2/3) AND first-call
median 8.6s (bar <=5.0s). B is dominated by A' on every axis measured:
catches (0/3 vs 2/3), FP damage (tie-adjacent: 1 vs 2 flagged), cost (8.6s
vs 1.17s). The record now prices both registered D3 mechanisms; the
c67d075e trade decision (ship A' with the Kane loss as a priced, named
(c)-class headline) proceeds with B's curve behind it rather than an
assumption.

Label-supply note, stated per E-naive-baseline honesty: the scan bank
remains 9 cases / 10 item labels — the thinnest register bank; every number
above carries that n.
