<!-- ledger -->

**phase-b-57 · 2026-09-29 · pb-bench-dials-whitebox · director** — this commit
- Needed: the whitebox census falsified the row's premise. The probe modes are not a ~500-line move within cli-llm: the rerank reads sit inside eval_cmd's default raw-index mode, and every white-box mode reuses eval_cmd's bank, scorers, run records and `RunArgs` parser, which the §12 3a ladder cannot place.
- Chose: split the independent half out as `pb-bench-dials-whitebox-dispatch` (the inner-chaos arm moves to cli-llm's own `eval` dispatch, ~40 lines) and make pb-cli-llm-bench-move depend on it. Park the remainder for the operator at ctl/parked/pb-bench-dials-whitebox.md, which recommends option (d): svrn gains an internal probe verb that emits raw evidence, and bench's unchanged `eval run` modes exec it and score the result.
- Because:
  - The charter covers splitting a row when its proofs differ. The dispatch arm's proof is a dispatch test plus a census test, and the placement fork has none yet.
  - Every placement of the shared bank/scorer code is operator-only: a new leaf (b), a changed verb output (c), a retired mode or a promote arm refused once dialed, or moving eval_cmd off FIVE_PROGRAMS §11's bench half (e). New routes (a) were already refused by phase-b-54. None of them can be trialled inside one crate, and the charter forbids a rewrite that cannot be trialled.
  - A blocked row parks and the loop runs on. It does not stop the whole campaign.
  - Boundary gate: 20 (unchanged, EXIT=1).

<!-- appendix -->

## phase-b-57 · 2026-09-29 — pb-bench-dials-whitebox split: the inner-chaos dispatch runs, the probe placement is parked for the operator

<details><summary>reasoning, evidence, package</summary>

Package: the worker's NEEDS_HUMAN at b6b92a28c (four items; its full text is preserved under "Worker's original package" in the parked file, ralph/next/phase-b/ctl/parked/pb-bench-dials-whitebox.md, which is untracked by .git/info/exclude like all of ctl/).

Reproduced at b6b92a28c:
- `grep -n rerank eval_cmd/runner.rs` → :990-1088, inside `run_question`. mod.rs:990 calls `runner::run_bank` from the flagless arm. `--routing-only`/`--prod-pipeline` are parsed at mod.rs:481, :487 by `cmd_run` (:412).
- `RerankSettings::set_env` is called only from bench_cmd/promote.rs:403.
- inner-chaos is dispatched at eval_cmd/mod.rs:162. cli-llm's `eval` arm is lib.rs:191. `inner_chaos` is named outside its own tree only in lib.rs and eval_cmd/mod.rs.

Missed by the census, and it bears on the options:
- eval_cmd names `sovereign_eval` 0 times.
- bench_cmd reaches eval_cmd only through record types: `EvalRun`/`EvalResult` at render.rs:191-202 and :434, all.rs:33 and :1499; `RoutingMetrics` at all.rs:649 and :652; `ThreadEvalRun` at gate.rs:648.
- bench's `all` lane already runs the routing probe as a subprocess and parses its JSON loosely (all.rs:570, :588).

Option (d) therefore follows an existing pattern. So does option (e), which leaves eval_cmd in svrn and closes boundary.log:60 with bench_cmd alone, but fails the take-alone test for svrn and contradicts §11. The parked file prices each option and gives the recommendation.

Falsified if: the operator's chosen option turns out to need the inner-chaos arm to stay in eval_cmd, for example if (e) is chosen and eval_cmd stays whole in cli-llm. The dispatch split would then be harmless but unnecessary.

REVIEW-AFTER: the operator's answer on the parked package.

</details>
