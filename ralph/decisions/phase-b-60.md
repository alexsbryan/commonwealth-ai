<!-- ledger -->

**phase-b-60 · 2026-09-29 · pb-bench-dials · director** — this commit
- Needed: the worker's census at f5660f279 priced pb-bench-dials at ~1,850-2,000 lines over five crates against a stated ~900, and found the PROOF's census needle (`sovereign_core::runtime`) also matched 22 lines of svrn's grounding-gate primitives that are scorers, not turns. It stopped at census under the scope guard.
- Chose: the package's option (a) and its split.
  - The needle narrows to what the row's outcome removes: `sovereign_core::runtime::Runtime`, `collect_turn`, `chat_cmd::bootstrap::build_session*`, `set_var("SOVEREIGN_RERANK`.
  - The row splits by proof into pb-bench-dials-turns (~1,200: plain turns through TurnClient, stub-svrn same-verdict, the census test with a named OWED list), pb-bench-dials-rerank (~250: per-turn rerank override on the wire after `sampling`, promote dials) and pb-bench-dials-docs (~500: document turns via /v1/documents, vault ingest via lc_*, store reads via record_messages/get_conversation; asserts the OWED list empty).
  - pb-cli-llm-bench-move depends on all three and gains a census bullet placing the grounding primitives; a leaf for them stays the operator's.
- Because:
  - The row's premise that every `sovereign_core::runtime` line in the bench group is a turn was false (reproduced: 23 non-comment lines in bench_cmd/eval_cmd; 4 drive turns, `Runtime` at book_report.rs:25 and `collect_turn` at live_runner.rs:100, eval_cmd/runner.rs:576, runner_threads.rs:214; the rest are grounding-gate primitives and AtlasWalkEcho types). The scope guard sends what the outcome does not require to the row that owns it, and placing svrn items is the move's work.
  - The three pieces need different proofs (a lane verdict, a wire round-trip plus an arm delta, a document-route verdict), which is the charter's split test. The rerank piece matches pb-bench-dials-wire's precedent in size and crates (212 insertions, the same four crates, 70f36a999).
  - Rejected: (c) a grounding-primitives leaf now. Admitting a leaf is operator-only, and no row in flight needs it.
  - No symbol moves in any of the three rows, so the census rule's trial does not apply; the move is trialled on pb-cli-llm-bench-move (t-clillm-bench).
  - FIVE_PROGRAMS §11 amended: it still said scaffolding_param's env arms stay svrn-side, which phase-b-59 falsified.
  - Boundary gate: EXIT=1, 20 violations (delta 0). This commit changes no Rust.
- REVIEW-AFTER: pb-cli-llm-bench-move's census. Falsified if the gate-replay lanes cannot stay in cli-llm's remainder without a bench → svrn edge, or if pb-bench-dials-turns' census finds a plain-turn lane whose metadata `get_conversation` does not return.

<!-- appendix -->

## phase-b-60 · 2026-09-29 — pb-bench-dials splits by proof into turns, rerank and docs; its needle narrows to the turn drive

<details><summary>reasoning, evidence, package</summary>

Reproduced in this session:

- `git grep -n 'sovereign_core::runtime' -- bench_cmd eval_cmd` in sovereign-cli-llm/src: 23 non-comment lines (the package said 27 across the group; verifier.rs:36 is a string literal). Turn drivers: book_report.rs:25 `Runtime`, live_runner.rs:100, eval_cmd/runner.rs:576, runner_threads.rs:214 `collect_turn`. Primitives: chaos_monkey.rs:1024,1176,1336,1591,1775; faithfulness.rs:36; judge_replay.rs:42; live_runner.rs:686-785; resolver_precision/mod.rs:58; verifier.rs:19; eval_cmd/atlas_walk_meta.rs:16,57; runner.rs:155.
- sovereign-turn-client lib.rs:232 `pub struct TurnClient`; :4129 `sampling: None`; :2722 `upload_document`.
- sovereign-core runtime/capabilities.rs:66 `scope_sampling`; sovereign-daemon turn_http.rs:1329 calls it; corpus_search.rs:460 reads `lane.rerank.config`.
- promote.rs:55 lists `rerank.enabled` and `rerank.candidates_k` as the only supported `--param`s (the rows previously cited :53).
- `git show --stat 70f36a999`: 14 files, 212 insertions, crates sovereign-contracts, -core, -daemon, -turn-client.

The package was the worker's NEEDS_HUMAN at f5660f279, removed in this commit; its content is recorded here and in the rewritten rows.

</details>
