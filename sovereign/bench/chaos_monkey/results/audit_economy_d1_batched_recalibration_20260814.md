# D1 — the family-joined batched register, recalibrated through replay (order audit-economy)

2026-08-14, build 78c3fae2. All model calls on the local daemon (model
`primary`, http://localhost:9741) — zero external model tokens. Verdict
files: `judge_replay_20260814_batched.verdicts.jsonl` (pinned v2 set,
repeat 2) and `judge_replay_20260814_batched_pbase14_full.verdicts.jsonl`
(the full 59-audit pbase14 population). Bars are the D0 pre-registration
(a85cede1), approved via directive 573c4c48.

## The candidate register

`claims_support_batched` reshaped (78c3fae2): byte-identical
`EvidenceFamily` prefix + `CHUNK_JUDGE_SYSTEM` + numbered-claims suffix +
declared boundary. Family membership is asserted by
`batched_register_joins_the_judges_prefix_family` (wire-boundary test);
replay-seam byte identity by `replay_render_matches_the_batched_register`.
Study flag unchanged (`SOVEREIGN_GATE_BATCH_VERIFY`, default OFF).
Production semantics under a D2 flip are ASYMMETRIC TRUST: batch
"supported" clears; batch "unsupported" or parse-gap falls through to the
calibrated per-claim judge with its rescue search — every FLAG the user
ever sees still comes from the calibrated register or the deterministic
veto.

## Instrument validation (ARCH §18.4)

- Render facet: `--render-only` twice, 23/23 identical prompt FNVs; 23/23
  declare the family boundary.
- Model facet: `--repeat 2` — 0 repeat disagreements over 23 cases / 167
  claims; 0 parse gaps in 574 claim-verdicts across both files (the
  numbered-line format parses clean at every batch size seen, 4-12).
- Cost, measured server-side: 41/46 pinned-set calls RESTORED the family
  prefix (28-30ms); warm batched call median 1.17s (prefill = claims
  suffix ~230-370 tok + N verdict lines of decode).

## Register quality — the D1 bars, judged

Labeled set (claim-level, leaf-view semantics): 20 negatives / 8 positives.

| gate (as registered) | result | verdict |
|---|---|---|
| (a) zero (c)-class loss on pinned set | see the read below — no negative that main catches at tau .9 is batch-cleared | **PASS** |
| (b) catch >= 0.900 on 20 neg | **0.950** (19/20) | **PASS** |
| (b) clear >= 0.750 on 8 pos | **1.000** (8/8) | **PASS** |
| (c) naive baselines printed | trust-nothing = today's cost/quality; trust-everything = catch 0.0 (report output) | **PASS** |
| (d) frozen-3 3/3 | live-arm duty at the D2 flip — NOT YET RUN | pending |
| (e) dropped-catch read, claim-by-claim | below, zero unexplained (c)-class | **PASS** |

Reference points on the same labels: the calibrated register at tau 0.9 is
catch 0.900 / clear 0.750 (judge_replay_20260814_calibration.md). The
batched register dominates it on both axes on this label set. There is no
tau dimension for this register (binary text A/B) — the operating point IS
the verdict; stated rather than defaulted.

## The dropped-catch read (every clear-direction divergence, by hand)

1. **motive-coverup c1** (labeled negative, batch "supported"): main's own
   vp is .881 — BELOW tau .9, so production also clears it today. Parity
   with the shipping operating point, not a lost catch. Recorded as the
   register's one labeled miss, shared with main. (The other pinned
   specimen main misses at .9, officials-selfinflicted .853, the batched
   register CATCHES.)
2. **James-coinage** (landed14-202b6f84 c8, recorded FAILED vp .9845):
   verbatim at leaf[13]; documented calibrated-register false positive
   (etiology (vi), calibration Finding 3). Batch clears it — (a)-class,
   correction.
3. **Hobbes dilution** (pbase14-79179042 c1, recorded FAILED vp .9669):
   THE dilution specimen (leaf[18]-supported). Batch clears — (a)-class,
   correction.
4. **Frankfurt worth-wanting** (pbase14-3d9d10b5 c3, recorded FAILED vp
   .9159): the only flip not previously documented. Hand read (worker,
   this order): the SEP passage reads "Compatibilists of this stripe
   reject the idea that such freedom is necessary for meaningful forms of
   free will (e.g., Frankfurt 1969, 1971; Watson 1975, Dennett 1984)—the
   'varieties of free will worth wanting' (Dennett 1984)". The claim
   ("Frankfurt argues for varieties of free will worth wanting without
   requiring alternative possibilities") is supported by assembly of that
   sentence under the register's own standard; the claim does not assert
   Dennett's coinage. Classified (a)-class (borderline calibrated-register
   false positive at .916, same attribution-precision family as
   James-coinage). Flagged for seat review since this read is new.

Population sweep (all 59 pbase14 audits, 407 claims): **3 FLAG->CLEAR
flips total (0.74%)** — items 2-4 above; zero flips on any labeled
negative; 40 (pinned set) / ~178 (population) pass->fall-through rows are
cost-only (the calibrated judge re-decides them identically by
construction).

## Cost model for D2 (the tension to resolve BEFORE the smoke)

Batch supported rate: 53.3% pinned set; **53.7% on the audit#1-only
pbase14 population** (n=229 claims); 59.6% on re-audit passes. The D0
projection assumed only ~18% fall-through (the failed-claim rate); the
batch is measured more conservative — 46.3% falls through on audit#1.

Predicted per-claim + batch term on the composed instrument (6 claims
median): 1.2s batch + 0.463 x 6 x 1.8s = **~6.2s**, against the
registered D2 smoke bar of **<=5.5s**. As registered, the smoke likely
REFUSES despite every quality gate passing and a real -4.9s (18% of
stage). With the ladder additionally skipping searches on batch-supported
claims (separately gated on lost_rescue=0): ~-7.6s (27%). The registered
bar was derived from the wrong fall-through estimate; the quality result
is unambiguous. Decision on whether the 5.5s sub-bar stands or is
re-registered belongs to the seat/operator — flagged in the D1 report
message, not decided here.
