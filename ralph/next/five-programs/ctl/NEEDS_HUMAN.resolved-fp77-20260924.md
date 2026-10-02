# NEEDS_HUMAN — fp-77 (the row's local-only clause has no mechanism in the tree)

## (a) Unit

`fp-77` — HOST the store in cw-rails and MOVE the `/v1/mesh/kv/*` doors to it
(census row 3; five-programs-35/-36). Row: `ralph/next/five-programs/STATE.md:193`,
marked `[~]` (uncommitted). No code written; tree unchanged besides the mark.

## (b) What I ran and found

Premise checks, all true except the one below:

- `cargo tree -e normal -p commonwealth-rails` (before): 813 packages, `grep -c sqlite` = 0;
  `grep sqlite shared/crates/workspace-hack/Cargo.toml` = nothing. The +1 edge is viable.
- The daemon's rail is `RailsRingRail` (sovereign-daemon/src/rails_client.rs:346), so both
  pumps would append to cw-rails' journals. The twin is safe per write, as the row says.

Where the row and the tree disagree. The row says "local-only namespaces (fp-76) are now
journaled" and tests "a local-only row survives a restart". Four places in the tree refuse
that, and none of them is in the row's read list:

1. **The outbox refuses them.** `commonwealth-state/src/backend.rs:519-528` (`enqueue_on`)
   and `backend/memory.rs:78-82` skip every `is_gossip_excluded` id. That list is
   LOCAL_ONLY ∪ RAIL_CARRIED (peer_preferences.rs:246-281). It is pinned by
   `store.rs:818 an_excluded_namespace_never_enters_the_outbox`. A local-only `set` never
   reaches any pump.
2. **Projection refuses them.** `store.rs:356-362` (`apply_projection`) returns `Err` for
   any excluded id. Decision five-programs-37 explicitly refused dropping that inbound
   refusal (its option C), and daemon test `ring_sync_snapshot_tests.rs:213-229` (d2) pins it.
3. **Enumeration skips them.** `RingRail::namespaces` omits local-only (fp-76), so
   `project_all_on_disk` never sees them. fp-76's own commit body says "fp-77 must rehydrate
   them another way".
4. **Two of the seven can never be journals.** `wikipedia-newsworthy:status` and `:portal`
   contain `:`, which `valid_namespace` (commonwealth-rail/src/lib.rs:42-48) rejects. If the
   outbox accepts them, the pump drops every such write with a warn every tick.

A second, smaller gap is in the seal policy the row says to MOVE. `rail_kv_pump.rs:562-573`
seals and snapshots `mesh-measurements` from `sovereign_mesh::mesh_measurements::load()` +
`measurements_rail::republish`. cw-rails may not name sovereign-* (ARCH_LAYERS.toml forbid
row), so that arm cannot move. The `work` arm could move. But while the daemon pump twin
runs, both pumps would seal the same journals under the same actor (rails' key).

## (c) What the operator must decide

1. **Where local-only enters the journal.** Pick one:
   - (A) The outbox guard narrows to "rail-carried, or not a valid journal name". Both
     stores then queue local-only rows (the daemon's pump would journal them into cw-rails
     too, never offered, per fp-76). The pinning test at store.rs:818 is rewritten to the
     new rule.
   - (B) A store-level opt-in (e.g. a `MeshStore` constructor flag cw-rails alone sets).
     The daemon's behaviour is unchanged. It is one predicate with a mode.
   - (C) Drop the clause from fp-77. Local-only stays unjournaled until the daemon
     dials (fp-80+).
2. **How cw-rails rehydrates them without weakening -37's inbound refusal.** My proposal
   is a separate `MeshStore` method that merges only rows signed by THIS node's own actor,
   fed by iterating `LOCAL_ONLY_NAMESPACES` that are valid names. `apply_projection` and
   d2 stay untouched. This mints a second door into the store, so it is the operator's call.
3. **The `:` ids.** Rename them to valid journal names (their readers are
   newsworthy_cmd.rs / the leader step), or keep them unjournaled and name that gap.
4. **Seal ownership during the twin.** My proposal: cw-rails seals only the KV namespaces
   its own outbox feeds. `mesh-measurements` and `work` stay with the daemon pump. fp-83
   (which deletes that pump) must then be told where the measurements seal goes, because
   it cannot go to cw-rails.

## (d) To resume

Edit or mark the row in ralph/next/five-programs/STATE.md (fold the answers into its text),
then `rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
