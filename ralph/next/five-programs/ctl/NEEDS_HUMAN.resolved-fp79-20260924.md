# NEEDS_HUMAN — fp-79 (premise false; five-programs-36's own falsifier fires)

## (a) The unit

`- [~] fp-79 — depends [fp-78] — PORT FabricPart's store (census row 5, five-programs-36 (1); sovereign-mesh only, not folded): FabricPart::new (sovereign-mesh/src/fabric.rs:405) takes the store as a port (Arc<dyn ReplicatedKv> … or fp-78's ports) and the contribution ledger as fp-78's port, instead of mesh_store: Arc<MeshStore> plus the in-process ContributionEmitter it builds at fabric.rs:412; … The daemon's two call sites (daemon.rs:3035, bootstrap.rs:2526) change ONLY their argument … behaviour at this commit is unchanged.`

five-programs-36 names its falsifier: "Falsified if the Fabric row turns out to need daemon edits to compile. In that case the row's one-crate premise is false and it must merge with the first daemon-src row." It does. No source file was edited; the only change in the tree is the row's `[~]` mark in STATE.md.

## (b) What I ran and found

`FabricPart::new` swaps both PUBLIC fields' types (`pub mesh_store: Arc<MeshStore>` at fabric.rs:350, `pub contribution_emitter: ContributionEmitter` at fabric.rs:392). A port cannot yield the concrete store back (no downcast), so the fields must change type with the constructor. The daemon reads those fields with concrete-type surfaces the ports do not carry:

```
git grep -n "fabric\.mesh_store\|\.contribution_emitter" -- 'sovereign/crates/sovereign-daemon/src/*.rs'   (comments excluded)
31 sites in 14 src files:
auto_ingest.rs:739,1046  daemon.rs:4049,4109  newsworthy_host.rs:61,132  routes_inference.rs:1166,1421
routes_internal/corpus_collaborate.rs:287,615  routes_internal/corpus_queue.rs:85,86,151,245,584,594
routes_internal/knowledge.rs:250  routes_internal/mesh_admin/contribution.rs:242,325,338,351
routes_internal/newsworthy_status.rs:204  routes_mesh_kv.rs:55,91,103  state.rs:1622
venue_host.rs:65  work_atlas_broadcaster.rs:168,179,222  work_donor.rs:793
```

Examples of surfaces that do not exist on either port:
- daemon.rs:4109 hands `fabric.mesh_store` to `commonwealth_state::RetentionGc::for_namespace` (needs `Arc<MeshStore>`).
- mesh_admin/contribution.rs:242,325,351 call `current_contributions(&MeshStore, ..)` / `current_activity(&MeshStore, ..)`.
- work_atlas_broadcaster.rs:168,179,222 call `mesh_store.outbox_len()`.
- corpus_queue.rs:85-86 copy both fields into `FoldRecovery { mesh_store: Arc<MeshStore>, contribution_emitter: ContributionEmitter }` (sovereign-grants types); :245,:594 `.with_emitter(ContributionEmitter)`.
- venue_host.rs:65 builds `DaemonLedger { emitter: ContributionEmitter }`; newsworthy_host.rs:132, routes_inference.rs:1166, knowledge.rs:250, work_donor.rs:793 call the SYNC `ContributionEmitter::{events,record}`; `ContributionLedgerPort` is async (`LedgerFut`).
- The pump: the row says it "takes the store as its own argument", but its only spawn is in the daemon (daemon.rs:3993 `spawn_rail_kv_pump(app_state.inner.fabric.clone(), ..)`) and `pump_once(&fabric)` is called from work_atlas_broadcaster.rs:84 — both daemon edits.
- Tests: 24 files under sovereign-daemon (src+tests) read the two fields; sovereign-mesh/tests/main/dst.rs:74 calls `FabricPart::new` too.

Row line-number drift, for the rewrite: the daemon's `FabricPart::new` call is daemon.rs:3256 (3035 is where it resolves `provided.inner()` into the concrete store); bootstrap.rs:2526 is the `MeshReplicatedKv::in_memory()` construction, not a `FabricPart::new` call; the third src caller is state.rs:971 (`AppState::new_with_platform_and_engine`'s chain, `mesh_store: Arc<MeshStore>` at state.rs:800-1019).

No build, test or boundary run: nothing was edited. Boundary stays at 54 (fp-78's measurement, 909e1f219).

## (c) What the operator must decide

1. Apply five-programs-36's prescription — merge fp-79 into fp-80. Measured cost: fp-80 already lists 10 files; the Fabric field flip adds sovereign-mesh's fabric.rs, rail_kv_pump.rs, peer_adapter.rs plus every daemon src reader above that fp-80 does not already own (auto_ingest, routes_inference, corpus_collaborate, corpus_queue, knowledge, contribution.rs, newsworthy_status, routes_mesh_kv, work_atlas_broadcaster, work_donor — i.e. most of fp-81 and fp-82). That is roughly one 25-file, two-crate commit — past the ≤10-file atomicity -36 kept.
2. Or RESEQUENCE: fp-79 moves after fp-82 (depends [fp-82]). fp-80 builds the ports and a transitional AppState port field set; fp-81/fp-82 repoint the 31 readers of `fabric.mesh_store`/`fabric.contribution_emitter` to those AppState port fields (their files already cover all 14 src files above except daemon.rs, state.rs, venue_host.rs and newsworthy_host.rs, which are fp-80's); then fp-79's premise ("daemon call sites change ONLY their argument") becomes true except for the pump spawn at daemon.rs:3993, which the row would have to name. Needs fp-80's text to say where its port fields live while FabricPart still holds the concrete store (a second holder of one store during fp-80..fp-82 is the twin -36 warned about — name it transitional, the fp-80 bridge precedent).
3. Either way: correct the row's call-site pointers (daemon.rs:3256, state.rs:971, dst.rs:74) and name the pump's spawn (daemon.rs:3993) and `pump_once` caller (work_atlas_broadcaster.rs:84).

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
