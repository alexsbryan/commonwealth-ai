<!-- ledger -->

**five-programs-37 · 2026-09-24 · fp-76 · director** — this commit
- Needed: fp-76 would have skipped every `GOSSIP_EXCLUDED_APP_IDS` entry on the ring, but one of the eight, `mesh-measurements`, is excluded from KV gossip because it travels on the ring rail. Doing the row as written stops measured throughput crossing the mesh.
- Chose: option (A). commonwealth-rail-core owns `LOCAL_ONLY_NAMESPACES` (the other seven) and `is_local_only`. commonwealth-state keeps `RAIL_CARRIED_APP_IDS = ["mesh-measurements"]` and builds `GOSSIP_EXCLUDED_APP_IDS` at compile time as the union of the two. `is_gossip_excluded` keeps its path and its answers. The ring skip goes in `RingJournal::ops_missing_from_within`, which both wire answer sites call. `ops_missing_from`, the in-process honest total, is left as it is. Boundary gate 54, unchanged, since no code moved.
- Because: "never on the KV gossip wire" and "never offered on the ring" are two questions, so each gets its own list and the KV list is composed from them, not copied (ARCH 8). Option (B) would put a second decider at a call site. Option (C) would drop the inbound refusal that peer_preferences.rs:245-249 documents, which changes behaviour.

<!-- appendix -->

## five-programs-37 · 2026-09-24 — local-only ring namespaces are the gossip list minus the rail-carried one

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp76-20260924.md. The director reproduced it at 9530ef59d.

- The doc on `GOSSIP_EXCLUDED_APP_IDS` (commonwealth-state/src/peer_preferences.rs:238-251) says `mesh-measurements` "is NOT private and it still travels; it travels on the ring rail instead". Its entry is the receiving half of `apply_projection`.
- sovereign-mesh/src/measurements_rail.rs:109 appends under `MEASUREMENTS_APP_ID` (mesh_measurements.rs:1497, `"mesh-measurements"`). The daemon test ring_sync_by_roster.rs:46 replicates that namespace over ring sync.
- There are two wire answer sites, cw-rails rail.rs:504 (`missing_answer`) and sovereign-mesh rail_port.rs:298. Both call `RingJournal::ops_missing_from_within` (commonwealth-rail/src/journal.rs:220), and the journal knows its own namespace. A skip there covers both sites from one place in the row's own crate. `ops_missing_from` (journal.rs:206) has callers only in commonwealth-rail/src/tests.rs.
- `GOSSIP_EXCLUDED_APP_IDS` is read as a slice outside its file: store.rs:825/843/1026 and the daemon tests work_atlas_broadcaster.rs:203 and ring_sync_snapshot_tests.rs:142/229. So it has to stay a `&[&str]` at its path. A const-fn concat of the two lists gives that without a twin literal.
- `PEER_PREFERENCES_APP_ID` is `"peer_preferences"` (peer_preferences.rs:48). That is a valid ring namespace, so rail-core carries the literal, and a commonwealth-state test pins that the two agree. The two `wikipedia-newsworthy:*` ids stay on the local-only list, because "never leaves this node" is true of them. `valid_namespace` rejects `:`, so their ring skip does nothing.

This fork is covered by the charter: which of two options follows from principle 8 and the row's own "one decider" text. Nothing a user can observe changes. The KV predicate returns the same answer for every id, and the ring stops offering only namespaces that were already private.

Falsified if some entry in `LOCAL_ONLY_NAMESPACES` turns out to have a ring publisher that peers rely on. That would be the same mistake as `mesh-measurements`, and its daemon ring-sync test would go red at fp-76. It is also falsified if a third wire answer site exists that does not go through `ops_missing_from_within`, because the skip would then not be structural.

</details>
