<!-- ledger -->

**phase-b-92 · 2026-10-01 · pb-mesh-dissolve · operator, ruling where the work-atlas replication test lives (option 1 of the row's NEEDS_HUMAN, recommended by the director)** — this commit
- Needed: sovereign-mesh was down to `MeshReplicatedKv` (src/peer_adapter.rs) and the composed work-atlas-over-`MeshStore` test (tests/main/work_atlas_store.rs). Every home for that test put one package's crate in the other's dev closure ([cmnwlth] <-> [code]), so the row's two constraints, "no new cross-package dev edge" and "never deleted", could not both hold. The worker's permissions refused the trial because it removes a test file. BOUNDARY 1, the one violation that dev edge.
- Chose: split by subject. commonwealth-state gains `store/work_atlas_tests.rs`, driving the two app ids `sovereign_contracts::peer::WORK_ATLAS_APP_ID_*` through A's outbox, the KV payload and B's projection: a public row crosses byte-identical and is filed under its author; a private row is never offered, never lands, and is refused by name when a peer offers it anyway; a release crosses as a tombstone; and both ids are classified by the constants (local-only and gossip-excluded, or neither). `peer_preferences`' literal pin now names the constants too. The work atlas keeps its record semantics over the port (sovereign-work-atlas tests/port_fake.rs, eight tests). commonwealth-state takes sovereign-contracts as a dev dependency, and the layer forbid's `sovereign-work-atlas` except goes. `MeshReplicatedKv` is deleted rather than moved: its one consumer, cli-mesh's `kv-export`, used `over` and `scan` only, so `kv_export.rs` scans the store and maps each row to the port's wire row itself. The crate goes with its ARCH_LAYERS rows, its root manifest lines and its CI lines.
- Because:
  - Principle 12. The two subjects meet at the app ids and nowhere else: no production path composes `WorkAtlasStore` with `MeshStore` in-process (every atlas outside tests dials cw-rails' KV, director's census at 65eff97bb). Each side tests what it owns, and the constant is the joint both name.
  - Principle 8. One spelling of each app id is pinned from both sides: work-atlas's `Privacy::app_id()` returns the constants, and commonwealth-state's tests classify the constants.
  - Pure outcome. BOUNDARY 1 -> 0 by an edge closing, with no `[[exception]]`; option 2 reached 0 by exception, which the objective rules out.
  - What is lost, named: the one end-to-end pass of the atlas's actual record JSON through `MeshStore`. The bytes are opaque to the store (`set` takes `Bytes`), so the public-row test's byte-identity check covers what the store could do to them.
- REVIEW-AFTER: the next change to `commonwealth_rail_core::LOCAL_ONLY_NAMESPACES` or to the atlas's `Privacy`. Falsified if dropping "work-atlas-private" from the local-only list, or skipping tombstones in `apply_projection`, leaves `store::work_atlas_tests` green (watched red at this commit).

<!-- appendix -->

## phase-b-92 · 2026-10-01 — the work-atlas replication test splits by subject; sovereign-mesh is deleted

<details><summary>evidence</summary>

Package: ralph/next/phase-b/ctl/parked/pb-mesh-dissolve.md (worker and director, 2026-10-01). Operator answer in the seat session of 2026-10-01: "Split by subject".

Gates at the commit: lint --human exit 0 (workspace scope, all targets); TEST(commonwealth-state) + TEST(sovereign-cli-mesh) 266 pass, 0 fail, with the five new or changed tests in the JUnit report; boundary-gate exit 0 ("every declared package reaches only itself + the shared leaves"); layer, docs, concept, lock, env, layout and arch gates exit 0. `cargo metadata` lists no sovereign-mesh.

PLANT (reverted): "work-atlas-private" removed from `LOCAL_ONLY_NAMESPACES` and tombstones skipped in `apply_projection`. Red: `gossip_excludes_work_atlas_private_app_id`, `the_work_atlas_app_ids_are_classified_by_the_contract_constants`, `a_private_work_atlas_row_is_never_offered_and_never_lands`, `a_released_work_atlas_row_crosses_as_a_tombstone` (and the existing `a_tombstone_does_not_take_a_row_written_after_it`). The public-row test stayed green, the control.

</details>
