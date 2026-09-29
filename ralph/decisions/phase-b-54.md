<!-- ledger -->

**phase-b-54 · 2026-09-29 · pb-bench-dials · director** — this commit
- Needed: pb-bench-dials stopped at its census with no code written. The row named two wire deltas; the census found five more classes of in-process reach (isolated state roots with planted memories, sampling pins, internal probes, env-var ablation arms, document-attached turns) and priced the row well past 2x its ~1,200 lift.
- Chose: split by proof into three rows. `pb-bench-dials-wire` puts the existing `SamplingOverrides` on `TurnRequest::Message` as an optional field. `pb-bench-dials-whitebox` keeps inner_chaos, voice_eval, eval_cmd's probe modes and scaffolding_param's env arms svrn-side, spelling unchanged. `pb-bench-dials`, narrowed to the black-box turn lanes, depends on both. REVIEW-AFTER: this redraws the §11 cli-llm partition (two modules leave the bench half), which the charter covers only through "a decision that changes the design edits FIVE_PROGRAMS".
- Because:
  - Principle 12 answers package forks 1, 3 and 4. A lane that plants into svrn's store, calls its router or retriever, or sets its env to ablate its reranker is svrn testing itself, like knowledge_gym production.rs, which the row already left svrn-side. Dialing those would either write synthetic memories into the operator's store or need new routes that expose internals for a test.
  - The edge the row serves does not need them. cli-llm → sovereign-eval is carried by bench_cmd (14 files name `sovereign_eval`; inner_chaos, voice_eval and eval_cmd name none), and nothing in bench_cmd, eval_cmd or quality_lane_cmd names inner_chaos or voice_eval.
  - Principle 6 answers fork 2. Every black-box lane pins temperature 0 by default, so dialing without a wire form moves every eval number. Principle 11 picks the form: `SamplingOverrides` (sovereign-core role.rs:104) already has the shape, and the phase-b-51 precedent covers additive optional turn fields.
  - Boundary gate: 20 violations, EXIT=1 at 237639e4b (`cargo xtask boundary-gate`, corpus-engine/). This commit changes no Rust.

<!-- appendix -->

## phase-b-54 · 2026-09-29 — pb-bench-dials splits into wire, whitebox and the black-box dial

<details><summary>reasoning, evidence, package</summary>

Reproduced at 237639e4b (sovereign-cli-llm/src):
- `git grep -l sovereign_eval`: bench_cmd 14 files; eval_cmd, inner_chaos, voice_eval, search_gym_cmd, knowledge_gym_cmd, gym_judge, quality_lane_cmd 0.
- `crate::(eval_cmd|bench_cmd|quality_lane_cmd)` inside inner_chaos and voice_eval: 0. `crate::bench_cmd` from inner_chaos and voice_eval: 0. inner_chaos's one coupling is the dispatch arm at eval_cmd/mod.rs:162; voice_eval dispatches from lib.rs:192.
- bench_cmd names eval_cmd only for record types (`EvalRun`, `EvalResult`, `RoutingMetrics`, `ThreadEvalRun`; all.rs:33, :649, gate.rs:648, render.rs:191-434), not its runner.
- Reach lines (`handle_message|handle_turn|runtime.|build_session|ChatSession`): 132 in 23 files; inner_chaos + voice_eval 34 in 8; bench_cmd 48; eval_cmd 46 (runner.rs 26); scaffolding_param 4. The worker's grep gave 162 with a wider pattern; the split does not rest on either total.
- Temperature-0 defaults: eval_cmd/mod.rs:435, chaos_monkey.rs:376, flywheel.rs:151, parity_compare.rs:217, promote.rs:164, redteam.rs:143, live_runner.rs:355/:692, book_report.rs:1492. In-process they reach `inference_config` at chat_cmd/bootstrap.rs:261-266. No turn-route path takes a per-turn temperature today.
- `SamplingOverrides` at sovereign-core role.rs:104 (temperature, top_p, max_tokens; `None` falls back). Documents routes at documents_http.rs:174 and :186.
- scaffolding_param names no `sovereign_eval`. promote.rs:39 and redteam.rs:42 use only its `decide`/`PromoteDecision`.

The package's fork 1(a) was rejected: a subject daemon on a temp root that each lane starts is phase-c's separate-subject dial, and it would make the lane hold the daemon's lifecycle. Fork 1(c), could-not-judge without an isolated subject, would retire two working lanes for no edge.

No trial: none of the three rows moves a manifest line. The move they prepare is trialled on pb-cli-llm-bench-move, whose module list this commit shortens.

What would falsify this:
- A white-box file turns out to be needed by code that moves: a bench_cmd or eval_cmd item calls into inner_chaos, voice_eval or a probe mode in a way the dispatch arm does not cover. Then the dependency needs a ladder placement, or the lane dials after all.
- The per-turn sampling override cannot be applied without changing the daemon's shared inference config for concurrent turns. Then the wire row goes NEEDS_HUMAN with the design.
- A record type both sides need (`RoutingMetrics`, `EvalRun`) has no rung on the §12 3a ladder. The whitebox row stops on it by name.

</details>
