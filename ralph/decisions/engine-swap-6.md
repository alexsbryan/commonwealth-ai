<!-- ledger -->

**engine-swap-6 · 2026-10-26 · es-g2-delete-engine · director** — this commit
- Needed: the row deleted `embedded/` and `llama.rs` and left `rpc_worker_main` and the fabric to es-g4, which depends on es-g2. Both call the engine, so the row could not leave the tree compiling; swapping the order fails too, since the engine calls the planner es-g4 deletes.
- Chose: es-g2 also deletes every item outside the engine whose only consumer is the engine (`rpc_worker_main` + `Launch::RpcWorker`, `distributed_respawn` whole, the fabric's engine-hook call sites and what goes dead with them, `engine_state`'s engine fields, `mesh_plan`'s FFI projection); es-g4 keeps the rest, the planner included. The generate and embed child roles are in es-g2, as P G2 lists them. Acceptance unchanged; ENV added to es-g2's checks because it removes env reads.
- Because: P G2 already lists `rpc_worker_main` here, so the row contradicted the plan. The cut follows the compiler and ARCH 12: a deletion takes its own call sites, and what keeps a reader outside the engine (`distributed_role`, `SOVEREIGN_RPC_SERVE`, discovery, the planner) stays with the row that owns the fabric. Merging the two rows (~33k lines, one session) and running es-g4 first (it would edit the engine G2 deletes) were both rejected.

<!-- appendix -->

## engine-swap-6 · 2026-10-26 — es-g2/es-g4 seam: the engine's deletion takes its call sites

<details><summary>reasoning, evidence, package</summary>

Reproduced at c2b65cc15 with
`git grep -n -E "sovereign_inference::(embedded|llama|llama_logs|rpc_worker_main)\b" -- '*.rs' ':!serve/crates/sovereign-inference'`:
`distributed_discovery.rs:12,104`, `distributed_respawn.rs:53-239`,
`distributed_warm.rs:15,71`, `distribution.rs:86-87`, `engine_state.rs:15-44`,
`mesh_plan.rs:259,266,483-484`, `sovereign-serve/src/lib.rs:259` (`Launch::RpcWorker`),
plus the within-row live callers `llama_router.rs:615` (`inference_deadline_secs`),
`assembly.rs:121,882`, `executor_native_e2e.rs:32`. `rpc_worker_main.rs` reads
`crate::llama` (:164, :217), `crate::llama_logs` (:171) and three `pub(crate)` items
of `embedded/rpc_distribution.rs` (:130, :173, :231; defined :2172, :2197, :2490).
The reverse edge: `embedded/rpc_distribution.rs:45,2508` and `embedded/model_slot.rs:63,99`
call `crate::rpc_warm_cache`, whose other readers (`distributed_warm/worker.rs:26`,
`mesh_plan.rs:410`) are fabric, so the planner survives G2 with no engine caller
and es-g4 deletes it as its row says.

`SOVEREIGN_RPC_SERVE` stays with es-g4: `distributed_role.rs:17,72` sets it and
`RpcServe` has readers outside the engine (note 65f1a395).

P G2 (`svrn-docs/ENGINE_SWAP_PLAN.md`): "`vocab_cache`, `rpc_worker_main`,
`reranker_standalone`, … the generate and embed child roles; the Llama arm of the
assembly and its idle monitors".

Package: the lane's `control/package.md` (worker census, options 1-4); option 1
taken with the compiler as the rule for where it stops, option 4 confirmed.

</details>
