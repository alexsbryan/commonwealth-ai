<!-- ledger -->

**phase-b-61 · 2026-09-29 · pb-bench-dials-rerank · director** — this commit
- Needed: the worker built the row green (1b6e8858a, 67da5b9b9, 1e8a46204) and stopped on the PROOF's live half: two promote arms at temperature 0 against a live svrn, dialed delta beside the in-process one at a75bacf87. The deployed svrn predates the row, and no probe set exists to run.
- Chose: the package's option 4. The row is `[x]` on its structural chain, and the live arm-delta moves to phase-c pc-bench-dials as a named owed measurement, recorded there as never-ran rather than passed.
- Because:
  - The live half's premise, "promote's probe set", was false: nothing is committed for promote to probe (sovereign/bench/flywheel/ holds only redteam/ and regressions/), so choosing a corpus and bank is the operator's call, not a worker's or the director's.
  - The two deltas would not measure the same arms. In-process, enabled=true set DEDUP_ONLY and dropped the cross-encoder. Dialed, it reranks with whatever cross-encoder svrn has wired (1e8a46204 delta 3). A match or a mismatch between them says little about the wire.
  - What the row needs to hold is that the pin reaches retrieval, and each link of that chain has a test that has been watched fail: settings become pins (`settings_become_a_turns_rerank_pins`), the dial puts them on the socket (`a_pinned_rerank_rides_the_turn`, which reads what the stub received), and svrn applies them to that turn only (`a_turns_rerank_pin_dedups_its_retrieval_and_no_other_turns`, whose PLANT went red).
  - Rejected: restarting the deployed svrn and running both halves now. It needs a cold worktree build that loads models in-process next to the resident 35B, on a probe set nobody has chosen. That is a measurement with no instrument (principle 7), and pb-cli-llm-bench-move does not need it.
  - Boundary gate: 20 violations, delta 0 (the worker's run at 1e8a46204; this commit changes no Rust).
- REVIEW-AFTER: pc-bench-dials' live run. This decision is falsified if the dialed enabled=true arm scores identically to enabled=false on a probe set where the in-process arms differed.

<!-- appendix -->

## phase-b-61 · 2026-09-29 — pb-bench-dials-rerank done on its structural chain; the live arm delta is owed to pc-bench-dials

<details><summary>reasoning, evidence, package</summary>

Reproduced in this session:

- `git grep -n 'set_var("SOVEREIGN_RERANK' -- sovereign/crates/sovereign-cli-llm/src`: 0 matches.
- Tests exist: scaffolding_param.rs:228 `settings_become_a_turns_rerank_pins`; subject_tests.rs:193 `a_pinned_rerank_rides_the_turn`; sovereign-daemon tests/main/turn_surface/rerank_pins.rs:160 `a_turns_rerank_pin_dedups_its_retrieval_and_no_other_turns`.
- target/debug/sovereign-stock mtime 03:40, and 70f36a999 committed 05:12, so the deployed svrn predates both wire rows.
- `ls sovereign/bench/flywheel/`: redteam/, regressions/ only.
- scaffolding_param.rs:29-32 scopes the knobs to the DEDUP_ONLY path without a cross-encoder.

Package (worker's NEEDS_HUMAN, removed in this commit): built and green (CLEAN, LINT, TEST sovereign-contracts 491, -core 1561, -daemon 1241, -turn-client 52, -cli-llm 877, all fail 0; PLANT red then reverted green; LAYER ok; BOUNDARY 20 delta 0). Options were (1) restart svrn, (2) name a probe set, (3) run both halves, (4) accept the per-turn test and owe the live run to a named later row. Chose (4), with the owed run named in pc-bench-dials.

</details>
