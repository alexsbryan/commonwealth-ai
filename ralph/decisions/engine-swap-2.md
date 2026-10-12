<!-- ledger -->

**engine-swap-2 · 2026-10-12 · es-m4-planner-out-of-embedded · director** — this commit
- Needed: the row moved the planner into `sovereign-compute` and had the engine keep calling it; compute depends on inference, so that is a crate cycle, and `plan_distribution` is FFI-bound except its middle.
- Chose: the planner leaves `embedded/` for a top-level, FFI-free `sovereign_inference::rpc_warm_cache` (pure split extracted first, `ggml_row_size` replaced by a table with an FFI golden test), callers repointed with no `embedded::` re-export; it reaches `sovereign-compute` in es-m3-move-agnostic-after-g2, and G2 deletes the golden test.
- Because: same shape and rule as engine-swap-1 (engine-used, engine-agnostic code waits in inference for G2); no new crate, no hook, no detour through `sovereign-contracts`, and the final home stays the fabric (ARCH 12).

<!-- appendix -->

## engine-swap-2 · 2026-10-12 — the planner leaves embedded/ now and reaches compute after G2

<details><summary>reasoning, evidence, package</summary>

Reproduced at 62d8efb55:

- `serve/crates/sovereign-compute/Cargo.toml:32` is `sovereign-inference = { workspace = true }`;
  `sovereign-inference/Cargo.toml` names compute only in a comment.
- Engine callers of the planner inside SI: `embedded/rpc_distribution.rs` (22 hits),
  `embedded/model_slot.rs` (3), `rpc_worker_main.rs` (doc link); all deleted in G2.
  `capacity.rs:128` mentions `shard_fits` only in a comment.
- `plan_distribution` (`rpc_distribution.rs:932`) calls `placed_devices()` (`:651`, ggml
  device enumeration), `projected_overheads` (`:839` -> `fit::get_device_memory_data`, `:865`)
  and `ggml_backend_dev_buffer_type` (`:1173`). Only the split between them is pure.
- `rpc_warm_cache.rs` (1952 lines, tests from `:1234`) has one FFI site, `:251-253`.
- Outside callers use `sovereign_inference::embedded::` from 9 compute files, 5 serve files
  and `sovereign-stock/tests/executor_native_e2e.rs`.

The worker's options and why each lost:
- A, `sovereign-contracts`: contracts owns wire DTOs (charter); a placement planner and a
  cache-file writer there is ownership drift, and it would move a second time after G2.
- B, a new leaf crate: a new workspace member, ARCH_LAYERS row and concept for an
  arrangement whose end state the plan already names (the fabric, in compute).
- C, an inverted hook from compute: an unregistered engine (SI's own tests,
  `rpc_worker_main`) needs a named absence, a behaviour delta the row does not grant.

The chosen path keeps the row's outcome (out of `embedded/`, FFI-free, behaviour preserved,
the engine still calls it) and changes only the interim crate. The last step into compute is
added to es-m3-move-agnostic-after-g2, which already carries engine-used modules for this
reason. Lift: the move is a `git mv`, so moved lines count once.

Found, not acted on: `rpc_warm_cache::parse_gguf` (`:180`) is a second GGUF header reader
beside `sovereign_contracts::gguf_meta` (ARCH 8). Not this row's scope.

Falsified if: the extracted split or the type-size table changes a plan on any existing
test fixture; or a caller of the moved names cannot be repointed without reaching into
`embedded/` (the module would then still depend on the engine).

</details>
