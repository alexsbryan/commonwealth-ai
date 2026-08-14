# D0 — audit#1 priced: the prefill premise is stale, the scan and the suffixes are the bill (order audit-economy)

2026-08-14. All numbers below are from EXISTING instruments — no new model
calls were spent on this deliverable. Populations are named per table; the
canonical population is the 688f8eba composed after-arm (the order's own
baseline instrument).

## Instrument validation before any result (ARCH §18.4)

- **Sources**: (1) the after-arm walltime file
  `ewalltime_desktop_20260814_portfolio_afterarm.jsonl` (stage_attribution
  strips, n=20 warm); (2) the grounding journal
  `~/.svrnmesh/journal/grounding-2026-08-14.jsonl` — every gate model call
  rides the per-call census (`call_census.rs`: mechanism, ms, prompt_chars,
  out_chars, stable_prefix_bytes, start_offset); (3) the daemon log
  (`~/.svrnmesh/logs/daemon.err`) for prefix_state/prefix_cache mechanics.
- **Cross-check**: journal call-sum vs stage strip on the same 21 turns:
  audit stage median 27.8s (strip) vs 24.9s (call-sum); the 2.9s gap is
  non-model time (claim-conditioned corpus search + overhead) and is
  itself priced below. The strip median reproduces the order's 28.0s
  baseline to rounding.
- **Honest limit**: the daemon log was rotated at 09:16Z, after the
  03:59-04:26Z after-arm window; per-request prefill mechanics are
  therefore taken from a later same-day window (12:31Z, different config —
  repair live) and used ONLY for mechanism identification, never for the
  baseline numbers. The census rows for the after-arm window itself are
  intact and carry the per-call timings used in every table.

## The decomposition (after-arm, n=20 warm turns, build 688f8eba)

Stage audit#1 (strip): **median 27.8s**, mean 29.0, p90 44.6, range 16.9-45.7.

| term | median s/turn | share of 27.8 | calls/turn | per-call shape |
|---|---|---|---|---|
| claim extraction (`claim_list`) | 4.1 | 15% | 1 | 5.4K-char prompt (answer, not evidence), ~615-char decode. No stable prefix. |
| per-claim judges (`per_claim_judge`) | 11.1 | 40% | 6 (3-10) | **1.78s median per call** (p10 1.13, p90 2.49). Prompt 34.9K chars, of which 31.9K declared stable prefix; ~4.0K-char claim-conditioned suffix. Out = 1 forced-choice token. |
| specifics scan (`specifics_scan`) | 9.7 | 35% | 1 | 37.7K-char prompt (~9.4K tok), 628-char decode. Declares a stable prefix but pays full prefill in practice (own prompt family; ~235MB pins, LRU churn). |
| non-call gap (corpus claim-search + overhead) | 5.0 (max 33.2 incl. warmup) | ~10-18% | — | one hybrid search per claim per allowed corpus |

Claims per audit: median 6 (3-12 extracted; 129 judged rows over 21 audits).
Failed claims per audit: **median 1, mean 1.10, max 3** (23/139 rows failed).

## FINDING 1 — the order's premise (inventory item 2) is STALE: prefill is already amortized

The batched-verify rationale ("N per-claim calls re-prefill the same
evidence N times — ~11x prefill / ~9x slower", doc comment + config.rs,
recorded 2026-07-20) predates the D1a prefix-state landing. Measured now:

- Every per-claim call declares the shared window (129/129 rows,
  stable_prefix ~31.9KB ≈ 8.25K tokens) and the engine restores it:
  `prefix_state: HIT — restored pinned prefix (single-token path)
  restored_tokens=8252 restore_ms=34-53`, with the suffix (claim + extras,
  ~460-580 tokens) as the only new prefill. Per-claim cost is 1.78s, not
  the 16-22s prefill-bound class the 2026-07-20 census recorded.
- First-call-vs-rest medians in the after-arm: 1862ms vs 1760ms — even the
  family prefill is mostly amortized on this instrument, because the bank
  repeats evidence windows (only 2 distinct stable-prefix sizes across 21
  turns). One cold turn paid 9.4s. **Instrument caveat, stated**: fresh
  question traffic re-pays the ~8.2K-token family prefill (~10.5s observed
  at 12:31Z) once per turn; on the composed-arm instrument it is nearly
  free. Both regimes are priced below.
- The prefix-cache "veto" the batch-verify comment cites is about llama.cpp
  partial-KV reuse on Gated DeltaNet; `SOVEREIGN_PREFIX_STATE` whole-state
  restore sidesteps it (note e08dfd3f) and is live on this host for the
  qwen35moe primary.

**Kill-clause arithmetic (order: "not worth continuing if batched buys
<20%").** The batched register AS SHAPED TODAY (`claims_support_batched`:
own prompt shape, own system turn, truncates chunks at 1500 chars, declares
NO stable prefix) would replace the 11.1s per-claim term with one full
~9K-token prefill + N-line decode ≈ 10-12s: **buys ≈0% on the composed-arm
instrument, and can lose time**. As shaped, it dies here — priced, not
argued.

What survives D0 is the register the order's title actually names — one
prefill, N verdicts — RESHAPED to join the judges' evidence family:
byte-identical `EvidenceFamily` prefix + `CHUNK_JUDGE_SYSTEM` + numbered
claims suffix, stable_prefix declared, so the ONE prefill becomes a
~40ms restore on warm turns and is SHARED with the judges on cold ones.
Projected per-claim term with asymmetric trust (batch "supported" clears;
batch "unsupported"/parse-gap falls through to the calibrated per-claim
judge with its rescue search — flags stay fully calibrated by
construction): batch ~2.5s + 1.1 failed x 1.8s ≈ **4.5s vs 11.1s
(-6.6s, 24% of the audit stage)** — above the 20% kill bar. The catch-side
risk concentrates in exactly one measurable place: the batched register's
false-"supported" rate on labeled negatives, which is what D1's replay
recalibration prices.

## FINDING 2 — the window term is material (D3 is live)

- The scan is 35% of the stage and is prefill-bound (~9.4K tok in, 157 tok
  out). It is also a measured false-positive engine (3/10 on labels,
  `judge_replay_20260814_calibration.md` Register 3) — every FP feeds the
  repair chain. Cost and quality point the same direction.
- The per-claim term's residual cost is the ~4K-char per-claim SUFFIX
  prefill (~1.2s of the 1.78s), i.e. claim-search extras — not the shared
  window. Window narrowing does not touch it; the ladder (existing flag,
  `SOVEREIGN_GATE_CLAIM_SEARCH_LADDER`) does, by skipping the search — and
  the non-call gap (5.0s median) with it. lost_rescue is its pre-built
  safety counter.
- The dilution finding stands as the quality half: 36-chunk view, verdict
  rides the phrasing (pbase14 79179042-c1 at vp .9679 leaf-supported).
  CONSTRAINT carried into D3: the window is currently DERIVED ("auditor
  sees what the drafter saw" — 56% of failed claims had support past chunk
  8 before it landed, note 95b82f97). Naive re-capping is a regression by
  construction; D3 candidates must keep drafter-visible support judgeable
  (ordering / conditioning, not blind truncation), priced through replay.

## Pre-registered bars (the D0 contract; changes only via the seat)

**Decider, fixed now**: "audit#1 median" = the stage_attribution `audit`
row (recheck=false) median over 20 warm turns, same bank, protocol and
instrument as the 688f8eba after-arm. Baseline 27.8s (=the order's 28.0s);
target ≤16.8s.

- **D1 (batched register, replay recalibration).** Candidate register:
  family-prefix batched (byte-identical EvidenceFamily prefix,
  CHUNK_JUDGE_SYSTEM, numbered-claims suffix, stable_prefix declared;
  asymmetric trust as above). Bars: (a) zero (c)-class loss on the pinned
  41-case set — no negative that main catches at tau 0.9 is batch-cleared;
  the 5 pinned specimens of the land-C table reproduced individually;
  (b) catch ≥ 0.900 on the 20 labeled negatives AND clear ≥ 0.750 on the 8
  positives (main's operating point); (c) naive baselines printed
  (always-flag / always-clear); (d) frozen-3 3/3 on any live judge change;
  (e) dropped-catch read claim-by-claim, zero unexplained (c)-class.
- **D2 (flip or refuse).** Flip `SOVEREIGN_GATE_BATCH_VERIFY` only if D1
  is green AND a 5-turn live smoke shows per-claim term median ≤5.5s/turn
  (≥50% of the 11.1s term). DEFAULTS_LEDGER row in the same commit.
  Refusal ships the curve that says no. Ladder flip is a separate decision
  gated on bank-level lost_rescue = 0 from shadow rows.

  **POST-DATA AMENDMENT (2026-08-14, operator directive 6686251c —
  recorded here because this is the bar it changes).** The ≤5.5s smoke
  sub-bar above was AMENDED AFTER THE D1 DATA to the measured shape:
  **batch+judges call-sum ≤6.5s on the live smoke.** Why after the data:
  the 5.5s figure was derived from D0's projected ~82% batch support
  rate; D1 measured 53.7% on audit#1, making the honest prediction for
  the same mechanism 6.2s — the bar was re-shaped to the measured
  support rate rather than the stale projection, by explicit operator
  resolution, option (ii), unedited. This is an after-the-numbers
  amendment of a pre-registered bar and is recorded as exactly that.
  The ladder search-skip stays separately gated on lost_rescue = 0 as
  registered, and the composed-arm ≤16.8s done-when is UNCHANGED and
  remains authoritative.
- **D3 (window/scan arm, replay-first, ~6min per candidate).** Per
  candidate: zero (c)-class loss on the pinned set; the dilution specimen
  (vp .9679) clears below tau for a candidate to count as the accuracy
  win; scan candidates must flag ≤3 of the 6 should_not_flag items while
  holding 3/3 should_flag. Wall bar: scan term ≤5.0s median on the
  composed arm.
- **D4 (incremental re-audit; judge-INPUT discipline).** Re-verify only
  repaired claims. Bars: re-audit term ≤9s median on repaired turns (vs
  28.4s recorded full re-audit); frozen-3 3/3; dropped-catch read;
  CONFAB-LEAK NEW≤OLD on the paired chaos run; p90 clause of E-wall-time
  re-judged with D4 in the composed arm.
- **Composed after-arm (once, at the end).** audit#1 median ≤16.8s; p90
  ≤90s re-judged on the same arm; chunk-judge verdict parity on the pinned
  replay set; CONFAB-LEAK NEW≤OLD paired chaos on affected banks.

Projection against the 40% bar, for honesty about margin: D1+D2 -6.6s,
D3 scan -4.7s, ladder search-skip -3s ⇒ ~13.5s ≈ 51% if all land;
D1+D2+D3 without the ladder ⇒ ~16.5s ≈ 41% — the bar is reachable but has
no slack for a lever failing its quality gate. The kill clauses stand as
written in the order.
