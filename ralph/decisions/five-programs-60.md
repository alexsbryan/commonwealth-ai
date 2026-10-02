<!-- ledger -->

**five-programs-60 · 2026-09-25 · fp-87 · director** — this commit
- Needed: fp-87's premise failed in three places. The crate-wide bar cannot reach 0 while `StoreSeed::local` (kept as residue by -59) names `commonwealth_state::MeshStore` and `MeshReplicatedKv`. rails_client/ledger/tests.rs:193-194 records through `ActivityEmitter`, and no row names it. The row also names `mesh_kv_client.rs` as the client, which cannot serve.
- Chose: (a) the package's option A. `FabricPart` gains one method that returns its private store's two backings as ports, and `StoreSeed::local(&fabric, id)` builds from them. (b) The ledger test builds its `ActivityEvent` as a literal. (c) The client is the daemon's existing `RailsKv`, with its base from `resolve_rails_base`. The row is rewritten in place and stays one commit.
- Because: all three use owners that already exist (principles 8, 11). Fabric already owns the store (fp-111), cli-llm already depends on sovereign-daemon, and `RailsKv` is the one cw-rails KV client. So both edges close without a new type, edge, or client. Option C would split §12 D4's "same commit" for a fix that is one method.

<!-- appendix -->

## five-programs-60 · 2026-09-25 — fp-87 closes both edges: `StoreSeed::local` seeds through a FabricPart port method, the ledger test builds its event literally, and cli-llm dials through `RailsKv`

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp87-20260925.md. Reproduced at 154df6d8e:

- `git grep -nw 'commonwealth_state\|MeshStore\|MeshReplicatedKv' -- sovereign/crates/sovereign-daemon` finds 7 hits: ledger/tests.rs:193,194; state/store.rs:24,67,68,70; tests/main/store_seed_double.rs:5 (a doc line). `StoreSeed::local` has one caller, state.rs:956, which passes `Arc::clone(&fabric.mesh_store)`.
- sovereign-mesh fabric.rs:350 declares `pub mesh_store: Arc<MeshStore>`, built at :407 as `MeshStore::in_memory()`. sovereign-mesh already imports commonwealth_state (fabric.rs:25). `LocalLedger::new` takes `Arc<commonwealth_state::MeshStore>` (ledger_port.rs:179), and `MeshReplicatedKv::over` takes the same (peer_adapter.rs:65). A method on the part that owns the store, returning `Arc<dyn ReplicatedKv>` and `Arc<LocalLedger>`, lets the daemon hold ports only. That is the fp-97 seam, and it is not laundering. Laundering would re-export or alias the concrete type into the daemon, and this does neither.
- `ActivityEvent` is `commonwealth_core::activity::ActivityEvent { node_id, timestamp, kind }` (activity.rs:42), and the daemon already names commonwealth-core. The test's claim is that the dial round-trips a served event. Its assertion compares against the same `Vec` it serves, so a literal keeps it verbatim.
- sovereign-cli-llm/Cargo.toml:69 already has `sovereign-daemon`. sovereign-daemon lib.rs:120 has `pub mod rails_client`. `RailsKv::new(base)` (rails_client/kv.rs:143) implements `ReplicatedKv` over cw-rails' `/v1/mesh/kv/*`, and bootstrap.rs:2495 makes it the daemon's one KV. `resolve_rails_base` (rails_client.rs:42) is the one reader of the key. `mesh_kv_client` is `mod mesh_kv_client;`, private, in cli-dev lib.rs:86, and it dials the daemon's own doors.

Rejected: (B) re-seeding the `AppState::new` family over the recording double. That touches 17 files and is its own row, and a src unit test cannot reach tests/main/common (-59). (C) keeping the daemon edge. That splits D4 and leaves a named edge open for a one-method fix. Moving `mesh_kv_client` to a shared crate would add a second client host.

What would falsify this: the FabricPart method needs a type in its signature that sovereign-daemon cannot name without commonwealth-state; `RailsKv`'s sync-over-dedicated-thread shape panics or deadlocks inside cli-llm's runtime; or the boundary gate counts a dev or transitive path that keeps sovereign-daemon → commonwealth-state after the Cargo line goes.

</details>
