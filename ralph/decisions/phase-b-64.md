<!-- ledger -->

**phase-b-64 · 2026-09-29 · pb-cli-llm-bench-move · seat (operator autonomy, 2026-09-29)** — this commit
- Needed: pb-cli-llm-bench-move parked a second time with the leaf clause. Its census found four sites the rows and phase-b-63 had not placed:
  - faithfulness.rs and verifier.rs, which name sovereign_eval;
  - knowledge_gym production.rs, which drives svrn's Executor in-process;
  - search_gym runner.rs, which runs svrn's search tool over a mock backend and checks production prompt text;
  - chaos_monkey's use of `role::default_profile_for`.
- Chose: no leaf.
  - (1) faithfulness and verifier move with bench; their svrn primitives go through the probe's `judge`/`assess` modes. This corrects phase-b-60's grouping on the evidence.
  - (2) knowledge_gym production and (3) search_gym's in-process search and drift check stay svrn-side, with their verbs dispatched svrn-side and spelled as before.
  - (4) role policy stays svrn's; the probe's `judge` mode runs under svrn's Critic, so chaos_monkey stops naming it.
  - The package is archived at target/ralph/phase-b/parked-pb-cli-llm-bench-move.phase-b-64.md.
- Because:
  - Principle 12: a lane that scores with bench's harness is bench's, and a lane that exercises svrn's own executor or search tool is svrn testing itself.
  - Principle 8: bench never copies svrn's policy, prompt text or thresholds.
  - The edge `cli-llm → sovereign-eval` still closes: only bench_cmd names sovereign_eval, and (2) and (3) name none of it.
  - A role table or prompt constants in a leaf would be policy in vocabulary.
  - Nothing a user sees changes.
  - This commit changes no Rust.

<!-- appendix -->
