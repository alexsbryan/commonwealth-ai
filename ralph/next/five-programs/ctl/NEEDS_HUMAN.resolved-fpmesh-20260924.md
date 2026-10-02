# NEEDS_HUMAN — REVIEW-mint-fp-mesh-dial: the residue needs 12 rows, 3 of them blocked on parked decisions; the cap is 8

## (a) The unit

`ralph/next/five-programs/STATE.md:132`:

    - [~] REVIEW-mint-fp-mesh-dial — depends [fp-0, fp-6, fp-8] — ... REVIEW-MINT the daemon→sovereign-mesh
      residue (TSV sovereign-daemon→sovereign-mesh, 250 refs; cell question decided by D2 — cw-rails is
      the owner): after fp-6/7/8, measure the remaining sovereign_mesh:: sites in sovereign-daemon,
      classify dial/DTO, mint atomic rows directly below. ... cap 8 — past it, NEEDS_HUMAN

Dependencies all `[x]`: fp-0 475f0bc0e, fp-6 86b03b9b7, fp-8 efa0b4793. No rows minted, no code
touched, nothing committed. The row is marked `[~]` in STATE.md, uncommitted.

## (b) What I measured (HEAD 01d6dbfe5)

    $ grep -rho "sovereign_mesh::" sovereign/crates/sovereign-daemon/src   --include=*.rs | wc -l  -> 160 (26 files)
    $ grep -rho "sovereign_mesh::" sovereign/crates/sovereign-daemon/tests --include=*.rs | wc -l  -> 127 (51 files)

(287 true sites. A bare `grep sovereign_mesh` reads 365 because it also matches `sovereign_meshapp::`
— meshapp_http.rs's 35 hits are fp-12's pair, not this one.)

By module, src / tests:
iroh_access 27/6, ring_roster 15/10, peer_adapter 12/1, rail_port 9/9, measurements_rail 9/0,
fabric 8/0, deep_link 8/1, persist 7/5, iroh_watchdog 7/0, gossip 7/6, mesh_measurements 6/4,
ring_sync 5/15, rail_kv_pump 5/30, guest_source 5/1, guest_pages 5/0, worker_eligibility 4/0,
guest_lender 4/7, mesh_discovery 3/0, rail_bind 2/0, pinned_pod_snapshot 2/0, measurements_wire 2/0,
join 2/1, state 1/0, reading_formatters 1/0, daemon 1/0, canonical_pull 1/2,
peer_inference 0/9, inference_adapter 0/9, decision_log 0/7, capabilities 0/2, guest_tunnel 0/1,
decision_trace 0/1.

cw-rails today serves /v1/mesh/{status,media,app,offers,media/presence,fanout,publish,forget-member,
roster-names}, /v1/rail/*, /v1/work/projection (commonwealth-rails/src/api.rs:44-97). It serves NO
ring-sync loop, gossip, guest, measurements or worker-eligibility surface.

Classified one row per owning surface (the core mint's grammar):

Mintable now (9):
1. REPOINT leaf-homed vocabulary — deep_link (9) → mesh_join_vocab (fp-52 moved it;
   shared/crates/mesh-join-vocab/src/lib.rs), iroh_access::MemberIdentity → kernel_types::member (fp-46),
   daemon::InferenceVenue (daemon.rs) → sovereign_contracts::venue (fp-1). Closes nothing alone.
2. MOVE the ring-rail port vocabulary (rail_port::RingRailPort/RingJournal, fabric::ForgottenMember)
   to its D3 owner — state.rs:548, routes_rail.rs:38, work_donor.rs:599,1000, rails_client.rs hold
   or implement the trait; work_atlas_broadcaster.rs:104 constructs LocalRingRail.
3. DIAL the ring-sync + roster loop — ring_sync::spawn_ring_sync_loop (daemon.rs:3965),
   ring_roster::MeshRoster/REGISTERED_NAMESPACES/is_daemon_owned, fabric::FabricPart (daemon.rs,
   state.rs), 20 + 25 + 8 sites; cw-rails has no sync loop to dial yet (SERVE half first).
4. DIAL the iroh endpoint lifecycle — MeshIrohAccess (7, daemon.rs), iroh_watchdog::spawn +
   Reach* (daemon.rs), rail_bind (daemon.rs), mesh_discovery::{relay_candidates,reachable_addresses};
   ~45 sites. Overlaps fp-9 (open, "DIAL mesh_discovery + iroh") and fp-47 (media Route types).
5. DIAL gossip announcements — announce_presence_change/announce_departure/GossipHandle
   (daemon.rs), 13 sites; no cw-rails surface.
6. DIAL the guest door — guest_source/guest_lender/guest_pages/guest_tunnel (state.rs,
   bootstrap.rs, guest_door.rs, routes_internal/guest_route.rs), 23 sites; interacts with
   HUMAN-fp54-guest-write-remount.
7. DIAL the measurements rail — measurements_rail/mesh_measurements/measurements_wire
   (mesh_http.rs, bootstrap.rs), 21 sites; no cw-rails surface.
8. DIAL worker eligibility + pinned-pod snapshots + reading_formatters (bootstrap.rs:581 region),
   7 sites — each needs its owner named first.
9. The test tree's inference fixtures — peer_inference/inference_adapter/decision_log/decision_trace
   /capabilities, 28 test sites in ~30 files (e.g. tests/main/embeddings_e2e.rs:36). This is fp-10's
   residue or finish condition 2's mesh test tree, which the header says is deliberately LAST.

Blocked on decisions already parked (3 — must exist before the edge can close):
10. peer_adapter::MeshReplicatedKv/MeshConvergence (daemon_services.rs:291,835,863; 13 sites) and
    rail_kv_pump outbox (30 test sites + daemon.rs:3972) — §12 D4's durable owner, fp-42 PARKED
    (five-programs-8).
11. Membership bootstrap — join::perform_join/perform_encrypted_join, persist::load/active_mesh_id/
    resolve_known/resolve_self_node_id/client_exposed, state::MeshState; 18 sites — fp-9's scout
    finding: cw-rails refuses the joiner-admitter role ("It does not admit joiners").
12. canonical_pull (auto_ingest.rs:354 + 2 test sites) — fp-7 PARKED on HUMAN-fp7-ingest-surface;
    counted, not minted, as the row instructs.

## (c) What the operator must decide

1. The daemon→sovereign-mesh edge cannot close inside cap 8: 9 rows are mintable today and 3 more
   classes sit behind parked operator questions (fp-42/D4, fp-9 membership bootstrap, fp-7). Even
   minting all 9 leaves the edge red. Raise the cap, split the mint (e.g. mint only rows 1-2 now —
   the vocabulary moves — and defer the dials until fp-9/fp-42 resolve), or re-sequence this row
   behind fp-9 and fp-42 the way REVIEW-mint-fp-core-dial was re-sequenced (five-programs-17).
2. Rows 3, 5, 6 and 7 each need a cw-rails SERVE half that does not exist (api.rs:44-97 has no
   ring-sync, gossip, guest or measurements route). Each is a fp-44/46-shaped SERVE+FLIP pair, so
   the honest row count is closer to 13 than 9 if SERVE and FLIP stay separate rows as precedent has.
3. Row 9 (test-tree inference fixtures): fold into fp-10, or leave for finish condition 2?
4. Row 4 overlaps fp-9 and fp-47; say which row owns MeshIrohAccess and the iroh watchdog.

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
