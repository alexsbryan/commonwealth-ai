<!-- ledger -->

**phase-b-63 · 2026-09-29 · pb-cli-llm-bench-move · seat (operator autonomy, 2026-09-29)** — this commit
- Needed: the supervisor parked pb-cli-llm-bench-move as "operator-only: admitting any leaf other than the host kit". After the gate-replay lanes stay svrn-side (phase-b-60), the census still found svrn's grounding primitives used as bench scorers in chaos_monkey.rs and live_runner.rs, and the `AtlasWalkEcho` types in eval_cmd.
- Chose: no leaf.
  - (1) The scorer primitives become `assess` and `judge` modes of the existing hidden `svrn __probe`, which bench execs. The same-verdict proof is pre-registered.
  - (2) `AtlasWalkEcho`, `AtlasWalkNodeEcho` and `ATLAS_WALK_META_KEY` move to sovereign-contracts as wire, re-exported at the old path.
  - The row is unparked; the package is archived at target/ralph/phase-b/parked-pb-cli-llm-bench-move.phase-b-63.md.
- Because:
  - Principle 8: bench never re-implements svrn's gate threshold or judge prompts. A copy would drift into a plausible score with nothing red.
  - Principle 11: the probe route already exists and was extended twice (phase-b-58 for eval, phase-b-62 for vault and RAPTOR).
  - Principle 12: svrn owns its gate; bench owns banks and verdicts.
  - A leaf would put gate policy into a vocabulary crate, the same problem pc-tiered-classify-back records.
  - The echo types cross the chat response as metadata, so the ladder's wire rung places them.
  - Nothing a user sees changes: the probe verb is hidden.
  - Boundary gate: 20 (the last recorded count; this commit changes no Rust).

<!-- appendix -->
