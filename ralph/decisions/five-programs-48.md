<!-- ledger -->

**five-programs-48 · 2026-09-24 · fp-84 · director** — this commit
- Needed: fp-84's worker stopped before editing. The row assumed each of its 11 files could build AppState over fp-80's recording double, but `assemble_with_fabric` (sovereign-daemon/src/state.rs:1015-1041) always builds `LocalLedger` and `MeshReplicatedKv::over` over the constructor's `Arc<MeshStore>`, so the double has no way in and zero users. Four files (emitter_origin_concurrency, load_awareness_e2e, rail_e2e, models_http_e2e) had no store-free constructor to route through.
- Chose: mint fp-97, a `StoreSeed` in state/store.rs beside `StorePart` (the existing Seed pattern), with `StoreSeed::local` holding the lines moved out of `assemble_with_fabric`, plus ONE all-seeds test constructor. fp-84, fp-85, fp-86 and fp-88 now depend on it. fp-84 is rescoped per file from the worker's census: three exact-equivalent reroutes, one comment edit, one already clean, four onto fp-97's constructor, and the double gains a seeded `list_models_with_origins` answer so the offline-peer `/v1/models` test keeps its HTTP half. work_atlas_store.rs moves to sovereign-mesh/tests, where `MeshReplicatedKv::over` replaces its `MeshPeer` twin. Boundary gate 54 (EXIT=1), unchanged; no code moved in this commit.
- Because: fp-88 needs the same seam to swap `LocalLedger` for `rails_client`, so building it once, before both, is the smaller step (package option A, ARCH 8/11). Routing only the four exact-equivalent files (option B) would leave the flipped tests reading a store the constructor hides, and that breaks again at fp-88.

<!-- appendix -->

## five-programs-48 · 2026-09-24 — fp-84's premise failed: mint the StoreSeed seam (fp-97) before the test flips and fp-88

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp84-20260924.md (git-excluded), reproduced at dcdb44c51.

- state.rs:748-790: `new`, `new_with_serving`, `new_with_node` each build `MeshStore::in_memory()`. Every other constructor takes `mesh_store: Arc<MeshStore>`. `assemble_with_fabric` :1024-1040 builds `MeshReplicatedKv::over` and `LocalLedger::new` unconditionally. state/store.rs (57 lines) holds `StorePart`'s six port fields.
- tests/main/common/ledger_double.rs: `RecordingLedger` answers every read empty, `list_models_with_origins` at :198 included.
- models_http_e2e.rs:188-191 writes a peer-origin `ModelInfo` through `InferenceStateStore::new(fabric.mesh_store, offline_peer_id)`, and the test's assertions need that row present. A seeded double answer supplies it without a key scheme.
- src/tests/daemon.rs:383-386 builds a fresh in-memory store plus a fresh registry and passes None. That is what `AppState::new` does.
- work_atlas_store.rs:50-62 is a `MeshPeer(Arc<MeshStore>)` adapter to `ReplicatedKv`, the same job as `sovereign_mesh::peer_adapter::MeshReplicatedKv::over` (peer_adapter.rs:65). sovereign-mesh depends on commonwealth-state (Cargo.toml:61). ARCH_LAYERS.toml:6 says layer-gate exempts dev-deps. commonwealth-state has no sovereign-* dep today, although its forbid row excepts sovereign-contracts and sovereign-work-atlas (:706-709). Placing the test there would keep the twin, so it goes to sovereign-mesh instead.
- quality/baselines/oversized.txt:53 pins state.rs at 2736, and it is 2342 now. The row still requires state.rs to end no longer than it starts.
- `cargo xtask boundary-gate` (corpus-engine/, toolbox): 54 violations, EXIT=1.
- STATE.md already held fp-96 (`[x]` 488dc1150), so the new row is fp-97.

Options: (A) the seed seam first (chosen). (B) reroute only the exact-equivalent files and leave four blocked. For models_http, the alternative was moving the test beside `InferenceStateStore` and dropping the HTTP half, which is a weakening and was rejected.

Falsified if fp-88 can swap the backing without a seam in `assemble_with_fabric` (then fp-97 was scope), if sovereign-mesh's new dev edge on sovereign-work-atlas raises BOUNDARY, or if fp-85/fp-86's census shows a construction shape the all-seeds constructor cannot express.

</details>
