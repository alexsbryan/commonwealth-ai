<!-- ledger -->

**phase-b-55 · 2026-09-29 · pb-bench-dials-wire, -whitebox, pb-cli-llm-bench-move · seat** — this commit
- Needed: the seat checked phase-b-54 against the tree and found two things its rows did not carry.
- Chose:
  1. A correction: phase-b-54 says "nothing in bench_cmd, eval_cmd or quality_lane_cmd names inner_chaos or voice_eval". But eval_cmd/mod.rs:162 dispatches `"inner-chaos" => crate::inner_chaos::run_inner_chaos`. `svrn eval inner-chaos` keeps its spelling by routing at the top-level dispatcher to cli-llm's svrn remainder, so the eval_cmd that pb-cli-llm-bench-move moves names no svrn-side module. `default_globals_for_voice_eval` (bench_cmd/book_report.rs:34, vault_report.rs:92) is chat_cmd::config's and only shares the name.
  2. `SamplingOverrides` exists twice with identical fields: sovereign-core role.rs:104 ("lifted from the agent-tools shape") and sovereign-agent-tools role/profile.rs:29. The copy -wire moves to contracts becomes the only definition, and both old paths re-export it.
- Because:
  - Principle 4: a claim in a decision binds like one in a commit body.
  - Principle 12: a bench crate that dispatches into svrn's module holds svrn's code.
  - Principle 8: one schema, one definition.
  - Boundary gate: 20 at 1660a37fd. This commit changes no Rust.

<!-- appendix -->

## phase-b-55 · 2026-09-29 — seat corrections to phase-b-54

<details><summary>evidence</summary>

- `git grep -n 'inner_chaos\|voice_eval' -- sovereign-cli-llm/src/{bench_cmd,eval_cmd,quality_lane_cmd}`, code lines only: eval_cmd/mod.rs:162 (the dispatch arm), and book_report.rs:34,563,875 and vault_report.rs:92,810 (`default_globals_for_voice_eval`, which comes from chat_cmd::config).
- `sovereign_eval` per module: bench_cmd 14 files, every other bench-group module 0. That part of phase-b-54 holds.
- sovereign-agent-tools/Cargo.toml names no sovereign-contracts today. If re-exporting at the agent-tools path would open an edge LAYER refuses, -wire says so and names the alternative, rather than keeping the twin.

</details>
