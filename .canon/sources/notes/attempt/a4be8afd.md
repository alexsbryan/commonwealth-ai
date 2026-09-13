# THE CLAIM-SEARCH LADDER'S TARGETING IS CORRECT AND ITS STAGE 1 IS TOO EXPENSIVE — NET +5s, NOT A WIN. Built and measured 2026-08-05,…

THE CLAIM-SEARCH LADDER'S TARGETING IS CORRECT AND ITS STAGE 1 IS TOO EXPENSIVE — NET +5s, NOT A WIN. Built and measured 2026-08-05, `SOVEREIGN_GATE_CLAIM_SEARCH_LADDER=1`, one question (`summary_cosmological_argument`), uncommitted.

WHAT WORKS — the targeting is exactly as designed:
  18 claims -> 11 searched, 7 SKIPPED (39% of the fan-out avoided)
  time in claim search: 82.4s -> 62.9s (-19.5s, -24%)
  gate outcome IDENTICAL: verdict=Mixed holdings=14 claims_revised=7 gate_action=rewrite_annotated
  every `stage1_vp` matched the shadow run's `vp_chunks_only` to 3 decimals — implementation and instrument agree, which is the cross-validation that the targeting rule is what I claimed it was.

WHAT DOES NOT WORK — WALL TIME WENT UP: baseline 256.9s -> ladder 261.9s (+5.0s).
The arithmetic: the ladder judges EVERY claim at stage 1 (18 calls) and then re-judges the 11 that fail (11 more) = 29 forced-choice calls vs the baseline's 18. That is +11 calls at ~2.2s each (~+24.5s) against -19.5s of avoided search. Net +5s, which is what the clock showed.
MY STATED ASSUMPTION WAS WRONG. I predicted stage 1 would be cheap because its passages are exactly the pinned shared prefix. The pin DOES hit (prefix_state HIT lines throughout), but a restored-prefix forced-choice still costs ~2.2s — the same order as the 2.9s corpus search it replaces. Restoring the prefix does not make the call free.

DO NOT CONCLUDE THE LADDER IS DEAD. The expensive part is the SHAPE of stage 1 (N per-claim judges), not the idea. The fix is already in the codebase: `claims_support_batched` (judge.rs:1180) scores ALL claims in ONE generation off a single evidence prefill — measured ~11x less prefill / ~9x faster on a longform turn (project_35b_moe_gate_latency_2026_07_20). Use THAT as stage 1 and the ladder costs ~1 extra call total instead of 18, keeping the -19.5s.
Its default-off caveat does not bite here: `SOVEREIGN_GATE_BATCH_VERIFY` is STUDY-only because the batched verdict is a text A/B rather than the calibrated forced-choice logit, so tau semantics shift. As a TRIAGE signal for "should this claim get a corpus search?" it never needs tau calibration — it only needs to be conservative (a false "unsupported" costs one search we would have done anyway; a false "supported" is the only real risk and is the thing to measure).

DETERMINISM CAVEAT THAT LIMITS ALL SINGLE-QUESTION DELTAS: the three runs of the SAME question extracted different claim sets — baseline-clean holdings=15/revised=9, ladder-on and the shadow run holdings=14/revised=7. Claim extraction is not deterministic, so a 5s wall delta on n=1 is inside the noise. Any wall-clock verdict needs several questions and ideally repeats.

STATE: uncommitted; lint 0 errors both times; full test suite NOT run. Flags `SOVEREIGN_GATE_CLAIM_SEARCH` (guard), `_SHADOW` (experiment), `_LADDER` (experiment) registered in quality/env-flags.toml + grounding_gate_flags(); docs/ENV_FLAGS.md regenerated once and is stale again for the two newer flags.
