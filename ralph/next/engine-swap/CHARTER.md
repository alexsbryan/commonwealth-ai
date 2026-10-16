# ralph director charter — engine-swap

Standing authorization for the supervisor's resolution session: the loop
decides its own forks instead of stalling on a sleeping human.
`docs/ARCH_PRINCIPLES.md` is the compass; where it and a row disagree, the
principle wins and the decision says so.

**Objective.** The daemon serves through a supervised llama-server router by
default, every kept capability (FIM, rerank, NER, distributed models)
included, and then the embedded engine, the vendored llama.cpp crates and the
C++ toolchain are deleted (plan: `/home/alexbryan/dev/svrn-docs/ENGINE_SWAP_PLAN.md`).
Done when every row is `[x]`, a native `cargo build --workspace` succeeds on
the host, all tests are green and the pre-push gate passes.

**Scope is frozen.** The rows in STATE.md are the scope. A new finding is
filed as a decision and reported, never added as a row without the operator.
A rewrite asks only for what the row's outcome and proof require.

**The pool.** Four streams (router, tools, packaging, moves) run beside each
other at 3 lanes; distribution joins when its prerequisites land.
conflicts.txt keeps rows on the same files out of one wave; heavy.txt allows
one model-loading row per wave; a row marked `*` in conflicts.txt runs alone.
`HUMAN-` rows are the seat's: the pool parks them and runs on.

## Decide these

- A false row premise: verify the census, rewrite the row with the reasoning.
- Row order within a stream; folding two rows on the same files; splitting a
  row whose proofs differ (`<id>-<suffix>`); BLOCKED when a premise waits on
  an unlanded row; `[x]` with the evidence when the tree already has the
  outcome.
- Where a moved item lands, by what each side owns about itself (ARCH 12):
  process supervision and model kinds in `sovereign-compute`, wire DTOs in
  `sovereign-contracts`, the server's spelling in `oicp-client`.
- Accepting a server behaviour over an engine habit when no caller depends on
  the habit (the plan's test), with the caller census in the decision.
- Re-running a red gate to understand it, fixing the code it names, splitting
  a file a gate flags (never re-pin).

## Leave these for the operator (write the package and stop)

- Changing the default engine anywhere but `REVIEW-build-es-e2-flip-default`,
  or that row before its dependencies are `[x]`.
- Reversing a decision of 2026-10-11 or 2026-10-12 (the plan's two decision
  lists), or dropping a kept capability.
- A bar a row cannot meet: report the numbers and stop. Never re-tune a bar
  after seeing the data.
- Touching the deployed daemon or `~/.sovereign/config.toml`; pushing;
  rewriting history; `--no-verify`; `--update-baseline` on a dirty tree;
  deleting a test to make a gate pass.
