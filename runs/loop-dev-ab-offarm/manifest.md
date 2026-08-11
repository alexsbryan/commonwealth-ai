# Run manifest: loop-dev-ab-offarm

- Order: native-grounding-tuning-loop, directive 44f48dd6 (middle loop:
  "a dev-bank A/B runs ONLY when a component objective improves").
- Trigger: routing objective FAIL->PASS (3/3 A3 probes at the embed
  layer, guard clean) and claims objective converted LIVE (css-center
  pass=true on a one-probe run). Loop journal
  sovereign/bench/calibration/loop/JOURNAL.md n=1-21.
- Branch/HEAD at staging: native-grounding-tuning-loop, see run banner.
- What runs: bench chaos-monkey, saltgrass bank, flag-OFF arm only,
  reranker held constant with the committed A/B. ~21 min by the
  committed run's wall time.
- Pre-registered bars (plan §4.1/§4.2, quoted not invented):
  honesty-when-absent 11/11 = 1.00 [A5 conversion]; competence-when-
  present >= 0.74 = the committed off-arm level (non-regression).
  Secondary read: css-center row pass=true with caveat_present=true.
- Outputs: target/loop-ab/loop_saltgrass_off.{jsonl,transcripts.jsonl,run.log}
  — deliberately NOT under bench/calibration/ab/ so committed evidence
  is never clobbered.
- No daemon restart. No flag flips. One arm, serial.
