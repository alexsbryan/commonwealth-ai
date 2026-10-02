<!-- ledger -->

**phase-b-59 · 2026-09-29 · pb-bench-dials-whitebox · director** — this commit
- Needed: the worker built everything on the row except one bullet. That bullet said promote's rerank ablation arms become probe flags. The worker found the premise false and stopped with NEEDS_HUMAN.
- Chose: the package's option (i), with one correction.
  - The row closes at 17135b45c without the bullet, and its census test keeps four needles.
  - The bullet's outcome moves to pb-bench-dials: no `SOVEREIGN_RERANK_*` is set in bench's process. The fifth needle, `set_var("SOVEREIGN_RERANK`, moves into that row's PROOF.
  - The correction: pb-bench-dials does NOT pre-decide that `--param rerank.*` is refused. Its census prices a wire form for the two knobs on the `sampling` precedent. If that does not fit, it writes NEEDS_HUMAN.
- Because:
  - The premise is false, reproduced here. `run_arm` (promote.rs:395-404) calls `set_env` and then `build_session`, and runs full answer turns through `run_live`. It then judges abstention from the visible answer. No probe stage produces an answer.
  - The env's only reader is `build_session`, and pb-bench-dials removes it. A row owns an outcome when it owns the thing that makes it true.
  - Rejected: (ii), a fourth answer-turn stage on the probe. It would be a second turn driver beside pb-bench-dials' HTTP dial (principle 8).
  - Not decided here: (iii), a per-turn rerank wire field. It is a new svrn contract field that this session cannot trial (the charter's census rule), so pb-bench-dials' census prices it.
  - The package said refusing the flag was "already stated" on pb-bench-dials. It was not. That row names only chaos_monkey's `--warm-atlas`. `rerank.*` are promote's only supported `--param`s, so refusing them would make `svrn bench promote` a verb that no longer works. The charter reserves that for the operator.
  - Boundary gate: EXIT=1, 20 violations (delta 0). This commit changes no Rust.
- REVIEW-AFTER: pb-bench-dials' census. It is falsified if promote's arms turn out to reach svrn some other way than `build_session`'s env read.

<!-- appendix -->

## phase-b-59 · 2026-09-29 — promote's rerank arms are pb-bench-dials', not the probe's

<details><summary>reasoning, evidence, package</summary>

Reproduced in this session:

- promote.rs:395-404: `run_arm` calls `settings.set_env(&args.corpus)`, then `build_session(globals)`. promote.rs:447: `run_live(session, corpus, &probe.query)`.
- scaffolding_param.rs:77-89: `set_env` sets the three `SOVEREIGN_RERANK_*` vars. Its only caller is promote.rs:403 (grep over cli-llm src).
- promote's HELP (promote.rs:53): the only supported `--param`s are `rerank.enabled` and `rerank.candidates_k`.
- The census needles at cli-llm lib.rs:296-301 are `.router.classify(`, `.retrieve_evidence(`, `.search_with_rerank(` and `.lane_sources`.
- The same-verdict comparator was re-run on the saved outputs in target/ralph/phase-b/whitebox-proof/:
  - routing: IDENTICAL, 0 diffs.
  - raw: IDENTICAL, 0 diffs.
  - prod: 7 diffs, and every one is in the pre-registered noise classes. The worker's figure matches.

The package is the worker's NEEDS_HUMAN at 17135b45c. It is removed in this commit, and its content is recorded here and in the rewritten rows.

</details>
