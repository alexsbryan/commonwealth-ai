<!-- ledger -->

**five-programs-44 · 2026-09-24 · fp-80 · director** — this commit
- Needed: the attempt-2 site re-census (package ctl/NEEDS_HUMAN.resolved-fp80b-20260924.md) priced the state chain past 20 rows. It found four false premises: fp-80's `From` bridge has no job, fp-90's port has no write half, fp-91/fp-93 leave sync sites that the cache cannot serve, and two out-of-daemon consumers take Fabric's concrete store (sovereign-grants, and commonwealth-state's storage snapshot loop). It escalated the grants seam to the operator.
- Chose: rewrite fp-80, fp-81, fp-82, fp-87, fp-88, fp-89, fp-90, fp-91 and fp-93 per the census. Mint fp-95, which moves the snapshot loop onto `ContributionLedgerPort` in sovereign-mesh; the charter covers it and it needs no layer change. Mint fp-94 for the grants flip, BLOCKED on a new HUMAN-fp94-grants-seam row. fp-88 now depends on both. The chain goes from 19 rows to 21. Folding the two new rows into one would gate the snapshot loop on an unrelated operator question and make a single row span two crates. Boundary 54, unchanged, since no code moved.
- Because: row rewriting and minting are the director's job (charter, "a false row premise"). The grants seam needs an `except` on a `[[forbid]]` row, which the charter reserves to the operator. Parking only that row lets the loop serve fp-80 → fp-93 → fp-81 → fp-82 → fp-95 in the meantime.

<!-- appendix -->

## five-programs-44 · 2026-09-24 — fp-80's chain rewritten per the site census; the snapshot loop gets a row; the grants seam is parked on the operator with a corrected recommendation

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp80b-20260924.md, reproduced at b1475fbeb.

- The grants forbid has no except (ARCH_LAYERS.toml:722-725), and grants depends on commonwealth-core and commonwealth-state (its Cargo.toml). `FoldRecovery` holds `Arc<MeshStore>` and `ContributionEmitter` (auto_recover.rs:205,207), and `ShardManager` does the same (shard_manager.rs:20,27,73,92). The package missed that grants also does sync KV `get`/`set` on `corpus-engine` handoff keys (shard_manager.rs:185,196) in addition to the scan (:924) and `record(ShardTransferred)` (:575,:892).
- The package's recommendation (b), moving `ContributionLedgerPort` into sovereign-contracts, is false as priced. The port names commonwealth-core types (ledger_port.rs:25-30), and sovereign-contracts' leaf budget is `[oicp-types, kernel-types, sovereign-time]` (ARCH_LAYERS.toml:897-910). It would widen a leaf budget as well as add the except. The HUMAN row recommends (c) instead: except sovereign-contracts only; grants takes the existing `ReplicatedKv` (peer.rs:175-192, sync get/set/scan) plus a fact method in the `LedgerEmitter` shape (venue_host.rs:19-23).
- `run_storage_snapshot_loop(emitter: ContributionEmitter, ..)` is at contributions.rs:174. `git grep` finds its only non-test caller at daemon.rs:4054 (`fabric.contribution_emitter` read at :4049) and its tests at :378 and :411.
- `MeshReplicatedKv` has `in_memory()` and `open()` only, with `inner` private (peer_adapter.rs:41,57,67). `PeerPreferencesPort` has `list`/`get` only (ledger_port.rs:72-76). The cw-rails doors are list and get (commonwealth-rails/src/ledger.rs:75-76), while the daemon calls `.set` (peer_preference.rs:150; routes_oicp.rs tests :506-562) and `.clear` (:175). `apply_peer_preference`'s two callers (:255, :365) are both inside `async fn capabilities` (:229).
- `InferenceCache` carries list_models, list_models_with_origins, get_local_embed_model and set_model_info (rails_client/ledger.rs:298-317). `get_plan`, `get_llama_address`, `set_llama_address` and `remove_model_info` exist only on the async port (:157-196).
- The ralph planner skips `HUMAN-` rows and holds their dependents (scripts/ralph.py:531), so a pending operator row does not halt the queue until nothing else is ready.

Options weighed for the chain: fold the grants and snapshot rows (20 rows) or keep them separate (21 rows). Kept separate: one row is gated on the operator and the other is not.

REVIEW-AFTER: sequencing fp-94 after fp-81 (it shares corpus_queue.rs) and before fp-88 is the director's call; no row or §12 decision names it.

Falsified if fp-95's loop cannot take the port without a sync bridge; if grants names a store operation beyond the four listed; if fp-80 still needs the bridge for a test file to compile (the census says every AppState constructor keeps `Arc<MeshStore>`); or if the operator's answer to HUMAN-fp94 makes fp-94 depend on something before fp-82.

</details>
