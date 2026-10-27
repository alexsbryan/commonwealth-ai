<!-- ledger -->

**engine-swap-7 · 2026-10-26 · es-g4-delete-mesh-fabric · director** — this commit
- Needed: the row deletes the planner and the pre-seed (`sovereign_inference::rpc_warm_cache`) and stops on a caller outside the fabric (§6). The census found three: `svrn mesh plan`/`bench`/`warm-cache` with `remote_gguf` (~6.0k lines, dispatched at `sovereign-serve/src/lib.rs:299-302`), model transfer's split-GGUF listing (`model_transfer.rs:453` → `shard_files`), and `/status`'s `rpc_worker` (`routes_status.rs:287,374`, from `SOVEREIGN_RPC_SERVE`). With them the row is ~12.3k lines against its "about 4k".
- Chose: split. `es-g4-delete-mesh-fabric` deletes the named fabric and the wiring its deletion forces, moves `shard_files`/`split_shard_names` to `sovereign-contracts` beside `gguf_meta` (no path left at the old one), and takes `rpc_worker` off `/status` with `RpcServe` and `--rpc-worker`; the planner and the pre-seed stay one row longer. New `es-g4-delete-mesh-fabric-verbs` (depends on it) deletes the three verbs, `remote_gguf`, their CLI-contract and conformance entries, then the rest of `rpc_warm_cache`. es-m3-move-agnostic-after-g2 and es-g3 depend on both. The row's false premise (`distributed_discovery/tests.rs`) is corrected. Acceptance unchanged: everything the row named is still deleted, across two rows; each half gains the TESTs of the crates it now touches, the verbs half DOCS.
- Because: the verbs are not a kept capability (the plan's are FIM, rerank, NER and distributed models); they are front ends of the placement the operator cut on 2026-10-23 and of the planner D4 deleted on 2026-10-24. `mesh plan` dry-runs the planner's split, which no longer runs (upstream's free-memory split does, P F), so keeping it would preview a placement the daemon never uses (ARCH 6); `mesh bench` keys its number on the fabric's `placement_digest` for `mesh plan` to look up, and D4 itself was measured by `d4-split.sh` against llama-server, not by it; `mesh warm-cache` is the pre-seed's CLI, which the row names. Model transfer is not distribution, so its one need (`shard_files`, pure GGUF file layout) moves to the crate that owns GGUF file facts (ARCH 12). `/status`'s `rpc_worker` has one reader, `rpc_discovery.rs:392-405`, which this row deletes. Fabric first compiles: `mesh_plan`/`mesh_bench` import nothing from `sovereign_compute` and read the distribution report over HTTP.

<!-- appendix -->

## engine-swap-7 · 2026-10-26 — es-g4 split at census: the fabric, then the mesh verbs and the planner

<details><summary>reasoning, evidence, package</summary>

Reproduced at 4bbf69f2a.

`git grep -n rpc_warm_cache -- '*.rs'` outside `sovereign-inference/src/rpc_warm_cache*`:
fabric (`distributed_warm.rs:10`, `distributed_warm/worker.rs:14,26,42`,
`distribution.rs:30`, `discovery_policy.rs:228,672`, `manager.rs:1472`); model
transfer (`model_transfer.rs:453`); the verbs (`mesh_plan.rs:249,410`,
`mesh_plan/report.rs:63,120,312`, `mesh_plan/tests.rs:95`, `mesh_bench/shell.rs:518`,
`warm_cache.rs:46,63`, `remote_gguf.rs:67,156`, `examples/warm_cache.rs:10`); and
`sovereign-cli-mesh/tests/weight_verbs_are_serves.rs:33`.

`wc -l` of the verbs: `mesh_plan.rs` + `mesh_plan/` 2397, `mesh_bench.rs` + four
files under `mesh_bench/` 3064 (more under `mesh_bench/tests/`), `remote_gguf.rs`
444, `warm_cache.rs` 82.

`ls serve/crates/sovereign-compute/src/distributed_discovery*` returns only the
`.rs`; it has no test module.

`git grep -n -E '"rpc_worker"|\.rpc_worker\b|RpcWorkerStatus'` outside
`routes_status.rs`: `rpc_discovery.rs:392,400,405` (the only reader),
`clients/desktop/src-tauri/src/mesh_commands.rs:205` (a passthrough),
`clients/desktop/tests/e2e/demo/b8-code-intel.demo.spec.ts:147` (a key list),
`scripts/rpc-distributed-e2e.sh:72` (the fabric's e2e script).

`grep -n 'sovereign_compute::' serve/crates/sovereign-serve/src/mesh_bench*
…/mesh_plan*` is empty; `mesh_bench` reaches `placement_digest` through
`crate::mesh_measurements` from the daemon's report, so deleting `distribution.rs`
first leaves the verbs compiling, reporting no distribution until their own row.

P F (`svrn-docs/ENGINE_SWAP_PLAN.md`): "`-ot` from our planner is NOT sent, because
D4 measured it at parity … and a tie deletes it"; "G4 deletes the planner with the
fabric". P lists the kept capabilities at "Each kept capability, in its layer".

Finding filed, not acted on: P F says upstream's `-c` cache under `LLAMA_CACHE` is
"ggml's own format, which our in-process worker already writes", so an offline
pre-seed of es-f1's child is still possible in principle. The row names the
pre-seed for deletion and this decision does not reverse it; if the operator wants
the thumbdrive path back, it is a small restore aimed at the child's cache dir.

Package: the lane's `control/package.md` (worker census, items 1-5, options 1-5);
options 1, 2, 3 and 5 taken; option 4's split taken with the halves named
`es-g4-delete-mesh-fabric` and `es-g4-delete-mesh-fabric-verbs`.

</details>
