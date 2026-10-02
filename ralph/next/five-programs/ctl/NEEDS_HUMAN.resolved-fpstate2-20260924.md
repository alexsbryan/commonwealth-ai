# NEEDS_HUMAN — REVIEW-mint-fp-state-pure-rust (second pass): the census counted files that NAME the crate, not files that USE the store; measured need is 13 rows against cap 11

## (a) Unit and row

`ralph/next/five-programs/STATE.md:181`, `- [ ] REVIEW-mint-fp-state-pure-rust — depends [fp-6] — RESUMED by the director (… decision five-programs-35 …) … cap 11 …`.
No rows were minted. STATE.md is unedited: the `[~]` mark was reverted, so the row stays `[ ]`. No code was touched. Boundary is still 54 (target/ralph/five-programs/boundary.log, 17:58). The two edges in scope are the only commonwealth-state ones: `sovereign-daemon → commonwealth-state` (log line 69) and `sovereign-cli-llm → commonwealth-state` (line 61).

## (b) What was measured (HEAD 7d1b2c11b)

The resumed row says "MINTS from that census and does not re-measure it". The census's daemon-src figure does not survive a premise check, and minting from it would give rows that close the edge by laundering (a fake zero). That is why I stopped instead of minting.

1. **The daemon src surface is 30 files, not 16.** `git grep -l commonwealth_state -- sovereign/crates/sovereign-daemon/src` returns 16 files, which is the census figure. Adding `git grep -l 'mesh_store\|contribution_emitter\|activity_emitter\|peer_preferences\|inference_store'` over the same tree gives 30 unique files. The other 14 call the store and the typed writers through `AppState` fields and never spell the crate path. Examples:
   - `routes_status.rs:18,85,92` and `routes_knowledge.rs:81` call `inference_store.list_models()`.
   - `routes_inference.rs:363,437` also call `list_models`, and `:673` calls `activity_emitter`.
   - `mesh_admin.rs:265-279` calls `set_model_info` and `remove_model_info`.
   - `corpus_queue.rs:85,86,114,151` uses `mesh_store`, `contribution_emitter` and `get_local_embed_model`.
   - `corpus_ingest.rs:876,894` calls `activity_emitter.record`, and `knowledge.rs:250` calls `contribution_emitter.record`.
   - `newsworthy_status.rs:161,204` and `corpus_grant.rs:177` use `mesh_store`.
   - `bootstrap.rs:2481-2526`, `daemon_cmd/boot.rs:474,700,708,1056` and `daemon_services.rs:290` use `sovereign_mesh::peer_adapter::MeshReplicatedKv`, a MeshStore wrapper (peer_adapter.rs:27,33).

   If only the 16 naming files are flipped, the Cargo edge drops and the gate goes green while the daemon still reads and writes an in-process MeshStore. Under the ≤10-files rule, 30 files is **3 rows**, not 2.
2. **The store instance lives in sovereign-mesh's Fabric, and the daemon hands it in.** `FabricPart::new(…, mesh_store: Arc<MeshStore>, …)` at sovereign-mesh/src/fabric.rs:405-412 builds `ContributionEmitter` over it in-process. The daemon constructs that store at daemon.rs:3035 and bootstrap.rs:2526, and spawns gossip, ring_sync and `rail_kv_pump` over `fabric` at daemon.rs:3961-3998. None of the census rows owns FabricPart's constructor taking the store as a port, or building a cache itself. Without that, the daemon cannot drop the dependency while it still constructs the Fabric: **+1 row** (sovereign-mesh fabric.rs plus its in-crate readers, which are gossip.rs, rail_kv_pump.rs and peer_adapter.rs).
3. **The typed doors carry synchronous hot-path readers.** `InferenceStateStore::list_models`, `get_local_embed_model` and `set_model_info` (store_adapter.rs:90,110,169) are sync. They are called on `/v1/models` and on the status routes. For D4's "read-through cache and dial", those readers stay local and sync over a cache that the rails doors refill. That fits inside the typed-doors row, but it is a shape the row has to name. This is not a new fork.

Measured row need:

| # | Row | Count |
|---|---|---|
| 1 | pure backend | 1 |
| 2 | local-only journal + PLANT | 1 |
| 3 | forbid-row widening + store/pump host + `/v1/mesh/kv/*` doors | 1 |
| 4 | typed ledger doors | 1 |
| 5 | Fabric takes a port, not a `MeshStore` | **1 (new)** |
| 6 | daemon src | **3 (was 2)** |
| 7 | daemon tests (35 naming files; 35/10 → 4) | 4 |
| 8 | cli-llm flip + migration | 1 |
| | **Total** | **13** |

The cap is 11. Per PROMPT §4, growth past a cap is a design finding, so I did not mint.

## (c) What the operator must decide

1. **Cap 11 → 13, or a compression.** One compression keeps 11: fold row 5 (the Fabric port) into the widening/host row, which is about 8 files (ARCH_LAYERS.toml plus rails lib/api/Cargo and fabric.rs, gossip.rs, rail_kv_pump.rs, peer_adapter.rs), and fold the third daemon-src row into the tests rows by moving `mesh_admin/tests.rs` and `tests/daemon.rs` there. But that crosses two crates and two programs in one commit, which breaks one-dimension-per-move (ARCH 2). My recommendation is **13**, for the same reason as five-programs-35: the cap came before the measurement.
2. **The no-laundering bar for the daemon rows.** Is the check `git grep -n commonwealth_state sovereign/crates/sovereign-daemon` returning 0, or is it also 0 hits for `mesh_store|MeshReplicatedKv` in daemon src/tests? I recommend the second. The first is satisfiable while the daemon still holds the store through `sovereign_mesh::fabric`, and that is the fake zero the campaign forbids.

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
