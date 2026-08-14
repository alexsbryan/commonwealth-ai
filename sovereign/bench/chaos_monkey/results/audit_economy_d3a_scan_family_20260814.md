# D3 candidate A — the scan joins the judges' prefix family: REFUSED at the bar, with the trade quantified (order audit-economy)

2026-08-14. Candidate build: branch `measure/audit-economy-d3a-scan-family`
(c1b0d030), worktree-isolated; main untouched. All model calls on the local
daemon — zero external model tokens. Verdict files:
`judge_replay_20260814_d3a_scan.verdicts.jsonl` (candidate A) and
`judge_replay_20260814_d3a2_scan.verdicts.jsonl` (A': + stitched-relation
kind). Bars: the D0 pre-registration (approved 573c4c48); candidate shape
approved by seat directive (scan-family-join as primary D3 arm, window
narrowing as fallback).

## The candidate

`scan_unsupported_specifics` re-rendered as a family member: byte-identical
`EvidenceFamily` leaf prefix + `CHUNK_JUDGE_SYSTEM` + summaries appended
after the declared boundary + the scan instruction as the suffix (item
budget folded into the user prompt; the shared system turn cannot carry
`max_items`). Family membership wire-asserted by the flipped
`the_gate_shares_one_prefix_family` scan block (5/5 tests green in the
candidate build). Unlike land C this touches NO forced-choice register —
render-fingerprint cross-check: per_claim_judge 28/28, chunk_judge 3/3,
batched_support 23/23 byte-identical between candidate and main builds.

## Instrument validation

- Render facet: bit-stable across runs; only the scan register differs from
  main (fingerprint table above).
- Model facet: `--repeat 2`, item-level output identical on 9/9 cases in
  both arms.

## Cost — measured, and it is the whole prize

> **CORRECTION 2026-08-14 (directive 6fdc5796, D2-smoke gap analysis —
> `audit_economy_d2smoke_analysis_20260814.md`).** The 1.17s figure below
> was a row-misattribution: it is the BATCHED register's median (out ~30
> chars), not the scan's. The daemon log for this run's own window
> (15:40-16:15Z) shows the candidate scan calls at 2.8-10.5s, median
> ~8.4s (out 372-1,773 chars — a 400-token decode cannot fit in 1.17s).
> The A' mechanism is real but its prize is the prefill share only:
> measured live post-land, scan 9.7s -> 5.6s median (-4.1s), cost model
> ~3.1s floor + ~4ms/char of flagged-item decode. The -8.5s projection
> below is retracted. The QUALITY verdict of this doc is untouched and
> was reproduced bit-identically across a daemon restart (9/9 item
> lists, same 7/10, same Kane miss).

Candidate scan calls on the replay arm: **median 1.17s** (77 restores at
~30ms, 1 learn) against the production scan's **9.7s median** (own-family
full prefill of ~9.4K tokens every audit#1). The projected composed-arm
saving is ~8.5s/turn, taking the scan term under the registered <=5.0s bar
with a wide margin — and unlike the LRU/byte-budget lever it holds on
fresh-question turns too, because the scan reuses the prefill the
batch/judges already paid.

## Quality — the registered bars, judged honestly

| arm | should_not_flag (6, bar: <=3 flagged) | should_flag (3, bar: 3/3) | total |
|---|---|---|---|
| main (production) | 6/6 flagged — a false-positive engine | 3/3 caught | 3/10 |
| A (family join) | 3 flagged — PASS (at the boundary) | 2/3 — **FAIL** | 6/10 |
| A' (+ stitched-relation kind) | 2 flagged — PASS | 2/3 — **FAIL** | 7/10 |

The lost catch, both arms: `(like Robert Kane, though he bridges both
sides)` — a real stitch fabrication main's scan flags. On its case the
candidate returns zero items (A) or an anchoring artifact (`Compatibilists`,
A'). Adding an explicit stitched-relation kind (A') did not recover it.
Iteration stopped at two arms: further prompt-tuning against a 10-item label
set is overfitting, not calibration.

**VERDICT: REFUSED as shaped.** The 3/3 should_flag bar exists because the
traded-catch class is the only failure class the dropped-catch instrument
has ever caught (invariant 4cf5268e); A' dominates main 7/10 vs 3/10 and
halves the false-annotation damage, but it pays for that with one real
catch, and that trade is not the worker's to accept.

## What this leaves (for the seat/operator)

1. **Accept the trade** (operator decision, recorded like the tau-0.85
   finding): A' is strictly better on 6 labels, worse on 1; scan cost drops
   9.7s -> ~1.2s. The frozen-3/live chaos discipline would still gate a
   ship.
2. **Close D3 failed-with-finding** and keep main's scan: the composed
   <=16.8s target then needs the ladder (lost_rescue evidence outstanding)
   on top of D2.
3. **Window narrowing (registered candidate B)** remains unpriced, but its
   mechanism fights the scan's charter (a narrowed window manufactures
   absences — the exact 95b82f97 class) and cannot approach A's cost floor;
   priced on request.
4. Label-supply note, stated per E-naive-baseline honesty: the scan bank is
   9 cases / 10 item labels — the thinnest register bank. Every verdict
   above carries that n.
