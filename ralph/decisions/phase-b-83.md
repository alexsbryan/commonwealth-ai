<!-- ledger -->

**phase-b-83 · 2026-09-30 · pb-mesh-exit-mesh · director** — this commit
- Needed: the worker's census (NEEDS_HUMAN, HEAD ff3693c12) found five modules the daemon still names that no row placed (capabilities, persist's roster read, the namespace decider, the guest route, persist's node-id shim), a false premise (no ingest port has an unpack), and ledger_port leftovers that would add `sovereign-daemon → commonwealth-state`. It asked for six ownership rulings and a split.
- Chose:
  - (1) cw-rails measures the node's hardware through `commonwealth_discovery::hardware`; svrn's claims declare only what svrn owns, and its storage budget still clamps the advertised free storage. FIVE_PROGRAMS §4 rule 8 says so.
  - (2) the note-author roster reads `mesh.json` through `sovereign_contracts::node_identity`, which gains `members[].name` beside `members[].node_id`.
  - (3) the decider and its list move to `sovereign_contracts::ring_namespaces`; four ring names move to oicp-types, each re-exported at its old path.
  - (4) the `/internal/guest/route` door moves to serve; `svrn chat` dials `venue::serve_port()`.
  - (5) `IngestPort` gains `unpack_canonical` beside `pack_canonical`.
  - (6) `peer_preference` callers use `PeerPreference::new` with `sovereign_time`; `STORAGE_SNAPSHOT_INTERVAL` moves to `oicp_types::contributions`; the loop moves with the traits.
  - No split: one outcome, one proof (charter). The row is extended and runs as a series resuming as `[~]`. LIFT +~210.
- Because:
  - Each ruling reuses an existing owner: cw-rails already links the one detector and owns advertisement; node_identity already parses mesh.json's members; pack_canonical is the port precedent; oicp-types already holds `MEASUREMENTS_APP_ID` and `PROCESSED_SHARDS_APP_ID`; serve owns `StoredGuestLink`. None adds a second copy of a drive or a second owner (§2c).
  - Ladder (§12 3a): ring names are federation wire, so oicp-types. The decider has two users until pb-mesh-dissolve (daemon, cli-mesh), and sovereign-mesh cannot name the daemon, so it goes to the svrn contract leaf both link. corpus-index cannot be named from contracts (it depends on contracts), which is why `APP_ID_TRACKED` moves down too.
  - Trial (3): COMPILE green, LAYER pass, BOUNDARY 4 → 4 (no new edge). The other five rest on manifests read at ff3693c12 (appendix).
- REVIEW-AFTER: pb-mesh-exit-mesh's landing. Falsified if the advertised hardware or free storage on this host differs before and after (1), if `svrn chat` with a stored guest link stops answering on the stock install (4), or if any ruling's move turns BOUNDARY or LAYER red where the trial and manifests said it would not.

<!-- appendix -->

## phase-b-83 · 2026-09-30 — pb-mesh-exit-mesh's six census forks ruled in the row, no split

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md (pb-mesh-exit-mesh census, HEAD ff3693c12). Reproduced:

- Daemon sites: `git grep 'sovereign_mesh::'` in sovereign-daemon/src lists peer_origin.rs:26 (capabilities), bootstrap.rs:29,:44 (persist), guest_door.rs:122 and routes_rail.rs:160 (ring_roster), routes_internal/guest_route.rs:19, state.rs:810-812, state/node.rs:237 (guest_lender/guest_source, which are `pub use sovereign_serving_host::…` shims at sovereign-mesh lib.rs:52-53), auto_ingest.rs:356 (canonical_pull), daemon.rs:1830,:1854 and peer_preference.rs:36, routes_oicp.rs:424 (ledger_port leftovers). All as the package says.
- ledger_port.rs:34-38 re-exports `STORAGE_SNAPSHOT_INTERVAL`, `InferencePlan`/`ShardPlan`, `peer_preference`/`PeerPreference` from commonwealth-state. Its other types come through commonwealth-core, whose modules are `pub use oicp_types::…` (activity.rs:39, contributions.rs:40, capabilities.rs:5).
- `STORAGE_SNAPSHOT_INTERVAL` readers: commonwealth-rails ledger.rs:439 and sovereign-daemon daemon.rs:1854, so two programs read it.
- `commonwealth_state::peer_preference` (peer_preferences.rs:56) is `PeerPreference::new(m, r, now)` with an error mapping; oicp-types peer_preference.rs:34 takes `set_at`.
- Hardware: cw-rails `origins::merge_declared` (origins.rs:187) takes hardware from the first declaration reporting any. serve's `anchor_claims` reports zeros, "as cw-rails' own report is", so the daemon is today the only hardware source. commonwealth-rails Cargo.toml:26 links commonwealth-discovery. capabilities.rs:136-151 clamps free storage to svrn's budget.
- node_identity.rs:20-29: it parses only `self_node_id` and `members[].node_id`; adding `name` is the same projection.
- `REGISTERED_NAMESPACES` (ring_roster.rs:279) names constants from oicp-types, commonwealth-state (2), sovereign-contracts (2) and corpus-index (`APP_ID_TRACKED`, newsworthy.rs:217); `is_daemon_owned` adds commonwealth-work's `WORK_NAMESPACE` (lib.rs:102). corpus-index links sovereign-contracts (Cargo.toml:29), not oicp-types, so its re-export goes through `sovereign_contracts::oicp`.
- `IngestPort::pack_canonical` (corpus-index ingest_port/daemon.rs:327) is implemented at corpus-engine engine/daemon_port.rs:455 over `canonical_sync::pack_canonical`; `unpack_canonical` (canonical_sync.rs:150) has no port method. The premise was false and the precedent fixes the shape.
- Guest route: the only caller is cli-llm chat_cmd/config.rs:123. sovereign-serve links serving-host (Cargo.toml:23); `venue::serve_port()` is sovereign-contracts venue.rs:32, linked by cli-llm.

trial (ff3693c12, compile, reverted): ruling (3) applied: four consts moved to oicp-types with re-exports at commonwealth-state store_adapter.rs/contributions.rs, commonwealth-work lib.rs and corpus-index newsworthy.rs; `sovereign_contracts::ring_namespaces` added; sovereign-mesh ring_roster re-exports both items; the daemon's two callers repointed. `cargo check -p oicp-types -p sovereign-contracts -p corpus-index -p commonwealth-state -p commonwealth-work -p commonwealth-rails -p sovereign-mesh -p sovereign-daemon -p corpus-engine --features corpus-engine/treesitter`: Finished. layer-gate pass (LAYER_EXIT=0). boundary-gate EXIT=1, 4 violations, the same four as HEAD. Raw: target/ralph/phase-b/t-director-fork3-trial.{log,patch}. The first attempt named `oicp_types` from corpus-index and failed (corpus-index does not link it); that is why the row names the contracts re-export.

Why no split: the worker proposed -a (stated moves, BOUNDARY 0) and -b (leftovers and the delete). Both serve one outcome with one proof (the daemon links no sovereign-mesh, PLANT re-adds it), and the charter splits only when proofs differ. -a would also land a BOUNDARY-0 row that advances no finish item on its own.

Why (1) is not end-user-observable: peers see the same hardware and clamped storage, measured by the same function, now from cw-rails. The row requires a test that pins both.

</details>
