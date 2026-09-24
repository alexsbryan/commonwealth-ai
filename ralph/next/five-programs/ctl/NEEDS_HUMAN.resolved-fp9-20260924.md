# NEEDS_HUMAN — fp-9 (sovereign-daemon → commonwealth-discovery)

## (a) Unit

`- [ ] fp-9 — depends [fp-6] — DIAL mesh_discovery + iroh + the discovery reads (§12 decision 2): the daemon's mdns::MdnsDiscovery + hardware::read_disk_free_bytes become cw-rails reads, and the join-key validation half extracts to a pure leaf per the TSV … — check: scoped lint 0; daemon Cargo.toml drops commonwealth-discovery; gate drops 1.`

fp-6 is `[x]` (86b03b9b7), so the row is dep-ready. The row already carries a
SCOUT FINDING (fw-1 session) saying its named moves do not close the edge.
five-programs-20 (ce4e2aeac) sent the loop to fp-9 without resolving that
finding, and itself lists "fp-9's membership bootstrap" among the classes
behind a parked decision. Premise check at ce4e2aeac confirms the finding; no
edit was made.

## (b) What I ran, at ce4e2aeac

`grep -rn commonwealth_discovery sovereign/crates/sovereign-daemon/src` plus
`grep -n "membership::"` over daemon.rs / mesh_admin.rs — the daemon's uses:

| site | symbol | kind |
|---|---|---|
| daemon.rs:17 | `mdns::{BrowseHandle, DiscoveredPeer, MdnsDiscovery}` | LAN advertise + browse (daemon.rs:344 "Live mDNS advertiser + discovery") |
| daemon.rs:1294 | `membership::init_mesh_with_identity` | founder bootstrap, builds `commonwealth_core::Mesh` |
| daemon.rs:1565 | `membership::validate_join_key_format` | pure — leaf-extractable |
| daemon.rs:1585 | `membership::init_mesh_with_node_id` | joiner bootstrap, builds `Mesh` |
| daemon.rs:2164-2165 | `generate_join_key`, `hash_join_key` | invite minting (rand + sha) |
| mesh_http.rs:1589,1607 | `hash_join_key` | pure hash |
| mesh_admin.rs:577 | `hardware::read_disk_free_bytes` | fs read |
| mesh_admin.rs:723 | `membership::accept_join_with_identity` | timing-safe verify + `Mesh` mutation |

commonwealth-rails/src/lib.rs:26 — "**It does not admit joiners.** There is no
`/internal/join` here and no invite minting: a mesh is founded and grown by a
full daemon". lib.rs:30 — "**It does not join over LAN/mDNS.**"

`RALPH_QUEUE=five-programs scripts/ralph-check.sh boundary` →
`boundary-gate FAILED (62 violation(s))`.

So the row's three moves (mdns + disk-free become rails reads, the
validation half becomes a leaf) leave `init_mesh_with_identity`,
`init_mesh_with_node_id`, `accept_join_with_identity` and `generate_join_key`
in the daemon. Those construct or mutate `commonwealth_core::Mesh`, so no
leaf can hold them, and rails disclaims both the admitter role and LAN. The
edge does not close (gate delta 0), and turning `MdnsDiscovery` into a
rails read would silently drop the daemon's LAN advertisement (principle 6).

## (c) What the operator must decide

1. **Who owns membership bootstrap and acceptance** (daemon.rs:1294, :1585,
   :2164; mesh_admin.rs:723)? Options: (a) cw-rails grows a founder/admit
   surface, reversing its lib.rs:26 disclaimer (new capability, principle
   11); (b) the daemon keeps them for good, making daemon→commonwealth-discovery
   an allowed edge (an `[[exception]]` row, or a classification change in
   the TSV — five-programs-20's own falsifier names this outcome as "a keep,
   not residue"); (c) a different home you name.
2. **Does LAN/mDNS advertisement survive the dial** (daemon.rs:17, :344)? If
   yes, something must advertise, and rails refuses to (lib.rs:30); if no,
   the removal is a behaviour change and needs its decision recorded.
3. Given 1 and 2, re-scope fp-9: which of its three named moves still ship
   (the `validate_join_key_format`/`hash_join_key` leaf is only worth a
   `[[package_leaf]]` admission if the edge actually closes — otherwise it is
   a leaf with no gate delta).

## (d) Resume

Edit or mark the row in ralph/next/five-programs/STATE.md, then
`rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`.
