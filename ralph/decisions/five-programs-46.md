<!-- ledger -->

**five-programs-46 · 2026-09-24 · fp-81 · director** — this commit
- Needed: fp-81's worker stopped before editing because two readers in its 11 files had no port field to move onto. The first is corpus_collaborate.rs:295 `union_processed_shards(&fabric.mesh_store, ..)`. The second is mesh_admin/contribution.rs:400 `activity_recent`, which scans `ACTIVITY_APP_ID` raw. The worker also asked for the error mapping to be confirmed.
- Chose: the package's recommendations. fp-81 adds a StorePart field `processed_shards: Arc<dyn ProcessedShardsPort>` over the existing `LocalLedger` and moves :295 onto it; fp-82 moves auto_ingest.rs:727 onto its `publish`. A new row, fp-96, runs before fp-81 and adds `ActivityLedgerPort::events` over the existing `ActivityEmitter::events` (LocalLedger, RailsLedger, a `get` on cw-rails `/v1/ledger/activity`, and the test double), mirroring `ContributionLedgerPort::events`. Error mapping as proposed: 503 where a route already errored, trace-and-serve for knowledge.rs:250's record, and 503 on `/internal/newsworthy/status` in place of `unwrap_or_default`. The grants lines are re-cited at their drifted positions. Boundary gate 54 (EXIT=1), unchanged; no code moved in this commit.
- Because: both deciders already exist (`union_processed_shards` behind `ProcessedShardsPort`, and `read_activity_events` behind `ActivityEmitter::events`). A daemon-side scan and decode would be a second copy of each key scheme (ARCH 8), and would reach past the port the campaign is flipping onto (ARCH 11). The field is the one missing piece for processed-shards. Activity needs a trait method and a door, which fall outside fp-81's files, so they get their own row (one dimension per move).

<!-- appendix -->

## five-programs-46 · 2026-09-24 — fp-81 gets a processed_shards field; activity events get their own row (fp-96)

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp81-20260924.md (git-excluded), reproduced at f74fe60e6.

- StorePart (state/store.rs:31-54) holds `inference_store`, `peer_preferences`, `rpc_shard_warmer`, `mesh_store: Arc<dyn ReplicatedKv>` and `contribution_emitter: Arc<dyn ContributionLedgerPort>`. `grep ProcessedShards` over state.rs and state/*.rs returns 0. StorePart is built at exactly one site, state.rs:1093.
- `ProcessedShardsPort { publish, union }` is at sovereign-mesh ledger_port.rs:84-90. It is implemented by `LocalLedger` (:237), `RailsLedger` (rails_client/ledger.rs:155, which posts to `/v1/ledger/processed-shards` and `/union`) and `RecordingLedger` (tests/main/common/ledger_double.rs:151).
- `ActivityLedgerPort` has only `record` and `current_activity` (ledger_port.rs:62-68). commonwealth-rails routes `/v1/ledger/activity` as `post(activity_record)` only (ledger.rs:74). `ActivityEmitter::events` already exists (commonwealth-state activity.rs:91, delegating to `read_activity_events`). The contribution counterpart is `get(contribution_events)` (ledger.rs:66-68, :201-208).
- The corpus_queue.rs grants sites are :85-86, :166, :260, :599 and :609 today (git grep), not the row's :151/:245/:584/:594.
- newsworthy_status.rs:200-205 reads `contribution_emitter.events().unwrap_or_default()`, so a store failure currently yields leader=None and a zero peer count, which reads as "no peers" rather than as an absence.
- `cargo xtask boundary-gate` (corpus-engine/, toolbox): 54 violations, EXIT=1.

Options weighed for activity: (a) a port method and a door (chosen); (b) re-export `ACTIVITY_APP_ID` through `sovereign_mesh::ledger_port` and scan `store.mesh_store`. (b) is smaller by one door, but it keeps a second decode of the activity rows in the daemon. It also launders a commonwealth-state constant through a re-export, which the no-laundering bar exists to stop.

REVIEW-AFTER: the 503 on `/internal/newsworthy/status` replaces a response that used to degrade (leader=None). The row's "never an empty list" sentence covers it, but the desktop reads that route. If a caller treats 503 as a hard failure where it used to render "no leader", the operator may prefer a named-absence field in the 200 body.

Falsified if `ActivityEmitter::events` decodes differently from the daemon's inline decode at contribution.rs:400-414 (for example, it drops rows the route used to show); if some AppState test constructor builds StorePart outside state.rs:1093; or if `LocalLedger`'s `ProcessedShardsPort::union` disagrees with `commonwealth_state::union_processed_shards` over the same store.

</details>
