<!-- ledger -->

**phase-b-58 · 2026-09-29 · pb-bench-dials-whitebox · seat (operator autonomy, 2026-09-29)** — this commit
- Needed: phase-b-57 parked the whitebox row for the operator. eval_cmd's three in-process modes (routing, `--prod-pipeline`, and the default raw-index mode) reuse bench's bank, scorers, run records and `RunArgs` parser, so no move within cli-llm can place them. The operator directed the seat to resolve parks overnight.
- Chose: phase-b-57's option (d).
  - svrn gains one internal probe verb, hidden from `svrn --help`, that writes raw classifications and retrieved pools.
  - bench's `eval run` modes keep their spelling and output, exec the probe, and score the evidence with what they already own.
  - The default raw-index mode becomes the probe's `retrieve` form; it is not retired.
  - promote's rerank ablation arms become explicit probe flags.
  - The evidence record is one sovereign-contracts type.
  - The same-verdict PROOF is pre-registered: identical `results[]` and metrics, in-process against probe-plus-score, on one bank at temperature 0.
  - The row is rewritten and unparked, and depends on pb-bench-dials-whitebox-dispatch.
- Because:
  - Principle 12: svrn owns describing its internals; bench owns banks and verdicts.
  - Principle 6: an env arm that configures nothing reads as a delta of zero.
  - Principle 8: one parser (bench's `RunArgs`) and one evidence type.
  - Principle 11: bench_cmd/all.rs:570 already execs `eval run --routing-only` and reads its JSON, and sovereign-cli main.rs:1254 already routes hidden introspection verbs.
  - Principle 7: the bar is fixed before any data.
  - Rejected:
    - (a) new HTTP routes, which phase-b-54 refused.
    - (b) a new leaf, which is the operator's.
    - (c) as written, because it changed verb output.
    - (e) eval_cmd staying in svrn, which contradicts FIVE_PROGRAMS §11. It would also leave bench's harness inside svrn for the developer who wants svrn without bench.
  - Nothing a user sees changes: no verb, flag or output moves, and the probe is hidden.
  - Boundary gate: 20 at 6f905904b. This commit changes no Rust.

<!-- appendix -->

## phase-b-58 · 2026-09-29 — the probe/judge line for eval_cmd

<details><summary>evidence</summary>

The parked package, with the worker's census and the director's options (a) to (e), is archived at target/ralph/phase-b/parked-pb-bench-dials-whitebox.phase-b-57.md. Both precedents were read in this session: all.rs:570-592 spawns `eval run --routing-only` and parses `RoutingRun` loosely as `serde_json::Value`; the dispatcher comment at main.rs:1254 routes "the hidden introspection verbs" on `rest[0]`. The LIFT of ~1,300 is phase-b-57's (d) price, which is unmeasured beyond that census. The census may split the row into -probe and -score by proof.

</details>
