<!-- ledger -->

**five-programs-59 · 2026-09-25 · fp-83 · director** — this commit
- Needed: fp-83's premise that the KV drain has no caller is false. `MeshBroadcaster::broadcast` (sovereign-daemon work_atlas_broadcaster.rs:83), which `finalize_work_atlas` swaps in at bootstrap.rs:2363, calls `rail_kv_pump::pump_once`. The row's closing bar ("fp-88's src-wide command = 0 with no residue") also cannot be met, because `StoreSeed::local` is what the `AppState::new` family of test constructors seeds over, and no row removes that family.
- Chose: the package's option C, without a replacement. fp-83 deletes `MeshBroadcaster` and step 1 of `finalize_work_atlas`, and the `DeferredBroadcaster` stays unset, which is a no-op. The closing bar is amended so that `StoreSeed::local` and its doc lines are named residue, kept as test support.
- Because: since fp-88 the work atlas writes the one `RailsKv` (bootstrap.rs:2525), so the outbox `pump_once` drains is Fabric's private store, which nothing fills. `appended` is always 0 in production, so the nudge never fires and `broadcast` only traces. Deleting it preserves behaviour. Option A as written adds a nudge that production does not raise today. Option B adds a new cw-rails door, which is new scope.

<!-- appendix -->

## five-programs-59 · 2026-09-25 — fp-83 deletes the dead `MeshBroadcaster`, and its bar keeps `StoreSeed::local` as named test-support residue

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp83-20260925.md. Reproduced at 0b468d87d:

- `git grep -n "rail_kv_pump::\|pump_once" -- sovereign commonwealth`, excluding commonwealth-rails and rail_kv_pump.rs itself, finds production callers only at daemon.rs:3992 (`spawn_plane_seal`, the kept arms), ring_sync.rs:408 (named by the row), and work_atlas_broadcaster.rs:83. Every other hit is a doc or one of the row's 8 test files.
- bootstrap.rs:2525-2533 builds the `WorkAtlasStore` over `RailsKv::new(rails_base)`. `MeshBroadcaster` drains `app_state.inner.fabric`'s store, and since fp-111 that store is Fabric's own `MeshStore::in_memory()`. In production no writer reaches it: `broadcast` gets `appended == 0`, skips `notify_one`, and emits one debug line. The hurry that WORK_ATLAS.md:150-162 describes has been gone since d18f4514b (fp-88). A claim now travels on cw-rails' pump tick (`PUMP_INTERVAL` = 2 s, commonwealth-rails kv.rs:52) and then the daemon's ring round.
- The `AppState::new` family (state.rs:747-956) all funnel into `StoreSeed::local` at state.rs:956. fp-84/85/86 are `[x]` and route their files through `AppState::new`/`new_with_serving` as exact equivalents, so no queued row removes the last caller. Removing it would mean choosing what the store-free constructors default to: the recording double lives in tests/main/common and a src unit test cannot reach it, and a `RailsKv` needs a live cw-rails. That is a design question and not strictly necessary for fp-83.

The two in-src tests in work_atlas_broadcaster.rs go with the component, and their store assertions are not lost. The private-claim pin is asserted by commonwealth-rails kv/tests.rs:317 `a_local_only_write_is_journaled_and_never_offered`. The public-claim "queued, then on the journal after a pump" assertion is either named in an existing cw-rails pump test or moved beside it verbatim, as the row already requires of the KV loop tests. The nudge assertion tested `MeshBroadcaster`'s own `notify_one`, so it is deleted with the thing it tested.

REVIEW-AFTER: the claim latency. fp-88 silently moved a public claim from "journaled before `declare_scope` returns, round asked for" to "up to 2 s + one ring round (60 s)". This decision does not change that further, and it does not restore it. Restoring it is a cw-rails "drain now" door, which is the package's option B and needs a new row. The operator decides whether the Spec §7 immediacy is still wanted. A second REVIEW-AFTER: `ClaimBroadcaster`/`DeferredBroadcaster` are left with no real implementation. Deleting them from sovereign-work-atlas is a later cleanup that fp-83 does not do.

What would falsify this: a production path that writes Fabric's private store (so `appended > 0` somewhere); a test that watches the claim hurry end to end against cw-rails and passes today; or a reader of `StoreSeed::local` in a non-test production path.

</details>
