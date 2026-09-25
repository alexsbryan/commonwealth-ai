<!-- ledger -->

**five-programs-55 · 2026-09-25 · fp-88 (mints fp-109, amends fp-83) · director** — this commit
- Needed: fp-88 made itself conditional on one premise: peer ops that the daemon's ring sync admits must reach cw-rails' store. The premise fails. cw-rails' ingest door (commonwealth-rails/src/rail.rs:524) calls `journal.ingest_all` and never projects. `KvHost::project_namespace` (kv.rs:196) has one caller, the start-time `project_all_on_disk` (kv.rs:118, via `run_forever` kv.rs:487). Today the only fold of peer ops is the daemon's ring round (sovereign-mesh ring_sync.rs:408), and it folds into the daemon's own store. Flipping the reads without fixing this would leave every rail-carried namespace stale until cw-rails restarts.
- Chose: option (A). cw-rails owns the re-projection. The ingest route marks the namespace dirty when `ingested > 0`, and `run_forever` folds each dirty namespace once per tick, before `pump_once`. This is minted as fp-109, with no dependencies, and fp-88 now depends on it. The daemon's ring-round projection stays through fp-88, still folding into Fabric's private store (dead work), and fp-83 deletes it along with the KV half of the pump.
- Because: ARCH 12. The process that owns the store owns its fold. (B) would leave the decision about when the store is fresh with a daemon that no longer owns the store, and it adds a door and a port method. (C) re-folds every journal on every tick even when idle. (A) is the smallest change and is batched like the current round: one fold per namespace per 2 s PUMP_INTERVAL, never one per chunk.

<!-- appendix -->

## five-programs-55 · 2026-09-25: cw-rails re-projects on admit (fp-109) before fp-88 flips the reads

<details><summary>reasoning, evidence, package</summary>

Reproduced at a9acb7df2. `grep -rn project_namespace commonwealth/crates/commonwealth-rails/src` finds only the definition (kv.rs:196) and the call from `project_all_on_disk` (kv.rs:118). `ingest_answer` (rail.rs:524-533) returns `{namespace, ingested}` and nothing else. `ingest_all` is called from no other cw-rails site. The ingest route gets `State<Arc<RailsDaemon>>` (rail.rs:620-642), and `RailsDaemon.kv: Arc<KvHost>` (lib.rs:211), so the route can reach the dirty set without a new handle.

On the package's second question: fp-88 does not delete ring_sync.rs:408. That would add a second dimension to a flip row. fp-83 already deletes the daemon pump's KV half, and `rail_kv_pump::project_namespace` is part of it. fp-83's text now names the ring-round step and moves `ring_sync_projection_tests.rs` beside fp-109's test with its assertions verbatim.

Latency changes from "the next ring round" to "the ring round's ingest plus at most one PUMP_INTERVAL (2 s)". The two are the same order of magnitude, and the change is not observable at a reader.

This is falsified if some peer-op path writes cw-rails' journals without going through `/v1/rail/ingest`. cw-rails' own gossip or a future append door would bypass the dirty mark. fp-109's premise greps for `ingest_all` callers. A second writer would need to mark the namespace dirty too, or the design would move to (C).

</details>
