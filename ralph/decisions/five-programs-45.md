<!-- ledger -->

**five-programs-45 · 2026-09-24 · fp-93 · director** — this commit
- Needed: fp-93's worker applied the flip as written and got 5 sovereign-daemon reds, all read-after-write. The row put `register_model` on `InferenceCache`, and `InferenceCache::set_model_info` spawns its port write while every reader since fp-91/92 reads through the port. The worker asked how `register_model` should keep read-after-write, what happens to `set_llama_server_address`, and whether doc mentions of `InferenceStateStore` pass the BAR.
- Chose: the package's recommendation. `register_model` and `set_llama_server_address` become async and `.await` the port. Their two sync callers (`register_local_model_slots`, `register_extras_in_store`) become async by fp-91's rule, since every caller is already async. StorePart holds no cache and the boot `refill()` is struck. The BAR counts non-comment hits only. Row rewritten in place; no row minted. Boundary count unchanged (no code moved in this commit).
- Because: a flip that makes a just-registered model invisible on the next read changes end-user behaviour (`/v1/models` after `models_load`; mesh_admin.rs:200 says "advertises it immediately"), and the row promised behaviour unchanged. With no sync reader left, a cache in AppState would be a second path to the same state that nobody reads (ARCH 8, 11). Going async is the smaller change and can be reverted. It reverses fp-91/92's "stays sync" for this one writer, whose ARCH 8 reason (one accessor per path) still holds.

<!-- appendix -->

## five-programs-45 · 2026-09-24 — fp-93 writes through the port; the cache arm is struck

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp93-20260924.md (patch ctl/fp-93-flip.patch, both git-excluded), reproduced at a86c76bd0.

- `InferenceCache::set_model_info` updates the filled cache and then does `runtime.spawn(port.set_model_info(..))` (rails_client/ledger.rs:331-359). This was read directly.
- Every inference-state accessor in state.rs except two is an `async fn` returning `Result<_, LedgerAbsent>` and would read the port after the flip (state.rs:1250-1345). The two exceptions are `register_model` (:1231) and `set_llama_server_address` (:1236). `git grep set_llama_server_address -- sovereign` finds only its definition.
- `register_local_model_slots` (daemon.rs:4739, a sync fn) has one production caller, daemon.rs:3395, which sits inside `async fn start_daemon` (:2896) under a tokio `RwLock` read guard. Holding that guard across `.await` is sound. `register_extras_in_store` (mesh_admin.rs:234) is called at :202 inside `pub async fn models_load` (:181).
- `InferenceStatePort` already has async `set_model_info` and `set_llama_address` (sovereign-mesh ledger_port.rs:101,106), so no port change is needed.
- I did not re-run the worker's test run (pass 1277 / fail 6). The five races follow from the spawn above plus through-port reads. The sixth red (local_only_boot 10s timeout) is unclassified, and the row now requires one isolated re-run of it. The boot `refill().await` suspected of causing it no longer exists under this decision.

Options weighed: (a) keep the cache for the writer and accept "eventually visible", which changes behaviour and is refused; (b) have the cache write synchronously and readers consult the cache, which splits one state across two readers (ARCH 8); (c) async write-through, chosen.

REVIEW-AFTER: `InferenceCache` now has no AppState user. Whether fp-88 or a later row strikes it (fp-78's tests keep it compiling) is not covered by the charter's "strictly necessary" rule, so it is left in place. venue_host.rs:31 still cites it as the shape for the sync `LedgerEmitter` bridge.

Falsified if some caller of `register_model` or `register_local_model_slots` turns out to be sync (a `block_on` or a non-async trait method); if the five named tests stay red after the async write-through; or if the local_only_boot red reproduces in isolation with no refill present.

</details>
