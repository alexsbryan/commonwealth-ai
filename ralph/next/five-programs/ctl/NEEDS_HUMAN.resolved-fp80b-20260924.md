# NEEDS_HUMAN — fp-80, escalated by the director: the site-level re-census prices the state chain past 20 rows, and one fork crosses a layer forbid

Director, 2026-09-24, supervisor resolution attempt 2. The seat's note on the
worker's fp-80 package said: re-verify every open row of this chain at the SITE
level (constructors, functions that take concrete types, sync/async shape), and
if that prices the chain past 20 rows (it is at 19, fp-75..fp-93), write
NEEDS_HUMAN with the count instead of deciding. That condition fired. No source
file and no row was edited; STATE.md carries only the worker's `[~]` on fp-80.

## Count

19 rows today → at least 21, and one of the two new rows cannot be written
until you answer (1) below. The row-level fixes that fold into existing rows
are listed after it so the answer can be applied in one pass.

## (1) The operator's fork: sovereign-grants holds the concrete store

fp-81's premise is false and no row in the chain covers it. corpus_queue.rs
hands Fabric's concrete values to sovereign-grants:

- `FoldRecovery { mesh_store: Arc<MeshStore>, contribution_emitter: ContributionEmitter }`
  (sovereign-grants/src/auto_recover.rs:205,207), built at corpus_queue.rs:85-86.
- `ShardManager::new(.., mesh_store: Arc<MeshStore>)` and
  `.with_emitter(ContributionEmitter)` (shard_manager.rs:73,92; it scans
  processed-shards at :924 and records `ShardTransferred` at :575,:892), built
  at corpus_queue.rs:151/245 and :584/594.

sovereign-grants cannot take the ports: its `[[forbid]] sovereign-grants →
sovereign-*` row has no except (quality/ARCH_LAYERS.toml:722-725), and both
`ReplicatedKv` (sovereign-contracts) and the ledger ports (sovereign-mesh) are
sovereign-*. After fp-88 moves the backing to cw-rails and fp-87 drops
commonwealth-state from the daemon, the daemon can no longer build what grants
needs, and until then grants writes to a store the ledger no longer reads.

Options:

- (a) sovereign-grants takes closures for the two things it does with them
  (scan processed-shards for a corpus, record a `LedgerEventKind`) and the
  daemon adapts its ports into them. No layer change. Two files in grants plus
  corpus_queue.rs, and grants' three test files that build a MeshStore. Cost:
  a closure seam is a second shape of the ledger port in all but name (ARCH 8
  pressure), though it names no type.
- (b) Add `sovereign-contracts` to the grants forbid's except and move the
  port vocabulary (`ContributionLedgerPort`, `ProcessedShardsPort`,
  `LedgerAbsent`) from sovereign-mesh into sovereign-contracts beside
  `ReplicatedKv`. One decider for the port shape; a layer-policy change the
  charter reserves to you.
- (c) grants keeps the concrete types and runs over cw-rails through its own
  dial. Rejected by me: a second client of the same doors (ARCH 8).

Recommendation: (b). The ports are pure vocabulary (async trait objects over
commonwealth-core types), so the move should pass the charter's
sovereign-contracts test, and `ReplicatedKv` already sets the precedent there.
Either way it is one new row before fp-81 (fp-94).

## (2) The second new row, which the director can mint once (1) is answered

daemon.rs:4049 (in fp-80) passes Fabric's concrete emitter to
`commonwealth_state::contributions::run_storage_snapshot_loop(emitter: ContributionEmitter, ..)`
(contributions.rs:174). The daemon is its only non-test caller; its tests are
contributions.rs:378,411. fp-88's premise ("no reader of `.contribution_emitter`
since fp-82") and its no-laundering bar both need it gone. It is the same
move in kind as (1): the loop goes over `ContributionLedgerPort` in whatever
crate (1) settles, the commonwealth-state copy is deleted, and its two tests
move with it (fp-95, before fp-88). fp-80 keeps daemon.rs:4049 on Fabric beside
RetentionGc, and its BAR widens to those two lines.

## (3) Premises that fold into existing rows (no new rows)

Verified by a multiline census of every method called on each field
(`perl -0777` over the daemon crate; the fp-78 census was a single-line
`git grep`, which is why `.peer_preferences\n.set(` and friends were missed),
and each site's enclosing fn classified async or sync.

- **fp-80.** `MeshReplicatedKv` has only `in_memory()`/`open(path)`
  (sovereign-mesh/src/peer_adapter.rs:57,67); `inner` is private. fp-80 needs
  a 4-line `MeshReplicatedKv::over(Arc<MeshStore>)` in peer_adapter.rs (one
  file past the list). The `From<Arc<MeshStore>>` bridge has no job: every
  AppState constructor keeps taking `Arc<MeshStore>` and builds `LocalLedger`
  inside `assemble_with_fabric` (state.rs:1016-1027), and `LocalLedger::new`
  needs a `NodeId` a `From` cannot supply (ledger_port.rs:117). Strike the
  bridge from fp-80 and every row that names it (fp-87 (1), the BARs of fp-89,
  fp-90, fp-93, fp-87 (4)). `DaemonLedger` (venue_host.rs:29) implements a sync
  `LedgerEmitter` (sovereign-contracts/src/venue_host.rs:23) over the async
  port: spawn on the current runtime and trace `LedgerAbsent` as warn, the
  same shape `InferenceCache::set_model_info` (rails_client/ledger.rs:317) uses.
- **fp-89 holds.** All five `activity_emitter` sites are in async fns or async
  blocks (routes_inference.rs:674,1190,1422; corpus_ingest.rs:876,894→973).
- **fp-90 is false as written.** `PeerPreferencesPort` carries only `list`/`get`
  (ledger_port.rs:72-76), and so do `RailsLedger` and cw-rails' doors; the
  daemon calls `set` (peer_preference.rs:150; routes_oicp.rs tests :506-562;
  peer_preference_manifest.rs:134,185,214) and `clear` (peer_preference.rs:175).
  fp-90 must first add `set`/`clear` to the port, `LocalLedger`, `RailsLedger`
  and the cw-rails door (the door set fp-78 meant to mint). Its sync read
  `apply_peer_preference` (routes_oicp.rs:373/381, called from the async
  `capabilities` handler) becomes `async`; it is not a §6.
- **fp-91..fp-93.** Sync inference sites: routes_inference.rs:363,368,437
  (`route_with_oicp`, `find_model_by_name`), daemon.rs:4846
  (`register_local_model_slots`), mesh_admin.rs:265,274,279, routes_oicp.rs:195
  (`apply_v04_enrichment`), and state.rs:1219-1247's four accessors. The cache
  carries `list_models`, `list_models_with_origins`, `get_local_embed_model`,
  `set_model_info` (rails_client/ledger.rs:298-317) and NOT `get_plan`,
  `get_llama_address`, `set_llama_address`, `remove_model_info`, which these
  sync sites call. fp-93's "the cache serves only the sync accessors" needs
  either those four in the cache (same shapes) or the sync fns made async
  where every caller is. The rows should say which per site; fp-91 already
  asks for the classification, so this is a sentence in fp-91 and fp-93.
- **fp-82** already names the `outbox_len` premise (work_atlas_broadcaster.rs
  :168,179,222 — all in `#[tokio::test]`s there). No change.
- **fp-88** callers of `FabricPart::new` are as the row says (daemon.rs:3256,
  state.rs:971, sovereign-mesh tests/main/dst.rs:74). bootstrap.rs:2519-2528
  shares the same store with `WorkAtlasStore`; that is the
  `sovereign-work-atlas` question the charter already reserves to you.

## (4) What to decide

1. The grants seam: (a), (b), or other. I recommend (b).
2. Whether a 21-row chain is acceptable, or the snapshot-loop move folds into
   the grants row (they are the same move, one crate each; folding keeps the
   chain at 20 but makes that row ~7 files across two crates).

With those answered, the next director session rewrites fp-80, fp-87..fp-93
per (3), mints the grants and snapshot-loop rows, records one decision, and
removes this file.

## Resolution — director, attempt 3, 2026-09-24 (five-programs-44)

Every fact above reproduced at b1475fbeb, with two corrections. Grants also does sync KV `get`/`set` on `corpus-engine` handoff keys (shard_manager.rs:185,196). And (b) does not pass the sovereign-contracts test: `ContributionLedgerPort` names commonwealth-core types (ledger_port.rs:25-30), which sovereign-contracts' leaf budget does not admit (ARCH_LAYERS.toml:897-910).

(3) is applied to fp-80, fp-81, fp-82, fp-87, fp-88, fp-89, fp-90, fp-91 and fp-93. (2) is minted as fp-95, with no layer change. (1) is minted as fp-94, BLOCKED on HUMAN-fp94-grants-seam, which carries the options and recommends (c): except sovereign-contracts on grants' forbid only, the existing `ReplicatedKv`, and a `LedgerEmitter`-shaped fact method. The chain is 21 rows; they are not folded, because only one of the two new rows waits on the operator. The loop resumes on fp-80.
