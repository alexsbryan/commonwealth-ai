# NEEDS_HUMAN — fp-83 (premise check failed before any edit)

## (a) The unit

`fp-83 — depends [fp-88, fp-111, fp-107]` (all three `[x]`). FLIP the pump and
ring daemon tests, DELETE the daemon-side pump's KV half. The row carries two
stop clauses of its own: "premise: `callers` on the KV entry points = 0" and
"If a caller outside these 8 files remains, name it; §6". The first is false,
and a caller outside the 8 files remains. The tree is untouched; the only
change is the row marked `[~]` in STATE.md.

## (b) What I ran and what came back (HEAD 0b468d87d)

```
$ git grep -n "rail_kv_pump::\|pump_once\|project_all_on_disk\|project_namespace\|spawn_plane_seal" \
    -- sovereign commonwealth   (minus rail_kv_pump.rs, cw-rails, and the 8 test files)
sovereign/crates/sovereign-daemon/src/daemon.rs:3992      spawn_plane_seal(   -- the kept arms, fine
sovereign/crates/sovereign-daemon/src/work_atlas_broadcaster.rs:83
        let out = rail_kv_pump::pump_once(&self.app_state.inner.fabric).await;
sovereign/crates/sovereign-mesh/src/ring_sync.rs:408      project_namespace   -- named by the row, fine
```

`work_atlas_broadcaster.rs:83` is PRODUCTION: `MeshBroadcaster::broadcast`
(the `ClaimBroadcaster` the work-atlas MCP tools call after a public claim
write) is swapped in at `bootstrap.rs:2363` (`finalize_work_atlas`). It calls
`pump_once` — the KV drain + KV seal this row deletes — over
`fabric.mesh_store`, then nudges the ring round. It also has two in-src tests
(`work_atlas_broadcaster.rs:143` "watched RED both ways: deleting the
`pump_once` call leaves the row in the outbox"), which write through
`fabric.mesh_store` directly. `sovereign/docs/WORK_ATLAS.md:160,294` narrate
the same call.

Since fp-88 the work atlas writes the ONE `RailsKv` (bootstrap.rs:2526), not
Fabric's private store, so in production this `pump_once` drains an outbox
nothing fills: the "hurry" it provides is already gone, and a claim now waits
on cw-rails' own pump tick (`PUMP_INTERVAL`, 2 s) plus the ring round. That is
a latency regression fp-88 already shipped silently; deleting `pump_once` makes
it a compile question instead of a silent one.

Second, smaller conflict (for the same decision): the row's closing bar
"fp-88's src-wide command = 0 with no residue" cannot be met by this row.

```
$ git grep -nw 'commonwealth_state\|MeshStore\|MeshReplicatedKv\|LocalLedger' \
    -- sovereign/crates/sovereign-daemon/src   (minus test files)
state/store.rs:24,67,68,70,72   -- StoreSeed::local
work_atlas_broadcaster.rs:201   -- inside its #[cfg(test)] mod
```

`StoreSeed::local`'s one caller is the test ladder (`state.rs:956`,
`new_with_platform_and_engine_and_gauge_and_fabric`), and the ladder is reached
by 17 daemon test files OUTSIDE this row's 8: canonical_pull_e2e,
capabilities_published, corpus_lifecycle, corpus_sharing_over_iroh_e2e,
daemon_wiring, emitter_origin_concurrency, fold_ingest_coverage_refusal_e2e,
fold_ingest_cross_node_merge_e2e, gossip_auth, injection_order,
join_handshake, load_awareness_e2e, models_http_e2e, store_seed_double,
rail_e2e/{ceiling,main,replication}. The row says the ladder "goes with its
last caller"; this row does not remove the last caller ("daemon tests 1/4"), so
the residue stays and the bar reads non-zero.

## (c) What the operator must decide

1. **`MeshBroadcaster` (work_atlas_broadcaster.rs:81-93).** Pick one:
   (A) reduce `broadcast` to the ring nudge alone (drop the `pump_once` call);
       the claim still reaches cw-rails synchronously through `RailsKv`, and the
       hurry becomes cw-rails' 2 s tick. Its two in-src tests move/flip with
       this row. Named latency change in the commit body.
   (B) mint a cw-rails "drain now" door (a `rails_client` call) so the hurry is
       restored where the store lives — a new door, so a new row before fp-83.
   (C) delete `MeshBroadcaster` and wire `NullBroadcaster` (the CLI already
       uses it), accepting the 2 s + ring-round latency for claims.
   Whichever, `sovereign/docs/WORK_ATLAS.md:160,294` changes in the same commit.
2. **The closing bar.** Either amend it to "= 0 minus `StoreSeed::local`
   (state/store.rs:24-72) until the last ladder caller goes (daemon tests
   4/4)", or name which later row deletes `StoreSeed::local`.

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
