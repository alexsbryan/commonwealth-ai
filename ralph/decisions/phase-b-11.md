<!-- ledger -->

**phase-b-11 · 2026-09-26 · pb-membership · director** — this commit
- Needed: pb-membership's census (NEEDS_HUMAN, before any edit) found that retiring the daemon's `node_key` retires the daemon's iroh endpoint, which carries peer inference, guests, apps, offers, rpc-worker, gossip and admission. The row stated none of that delta, and the only alternative (both processes keyed from one file) makes two endpoints for one member.
- Chose:
  - pb-membership narrows to what cw-rails owns alone with no daemon delta: found, admit (reusing `commonwealth_discovery::membership::accept_join_with_identity`), mDNS, and the cmnwlth lift founding its own two-node mesh. Its boundary expectation is 51, unchanged.
  - The key flip, the two-key migration, "the daemon signs through rails", the fp-9 retirement and the `cw-rails-lift` roster cleanup move to pb-mesh-exit-mesh, the row where the daemon stops being an endpoint. Its lift goes ~1,200 → ~1,800, and it gains a premise check for the ALPN routes no earlier row re-homed.
  - pb-work-doors' "sealed with the one node key" becomes a census question: if acts must verify against the roster identity, it depends on pb-mesh-exit-mesh.
- Because:
  - The key is the endpoint's identity (sovereign-mesh iroh_access.rs:648), so retiring the two is one outcome with one proof. By the charter, that outcome belongs to the row that already owns the endpoint's exit.
  - Option 2 (keep the row whole and let mesh traffic go dark) is end-user-observable and left to the operator. Option 1 is behaviour-preserving, and the charter lets the director fold and split.
  - The roster cleanup cannot happen before the flip. The daemon forwards `forget-member` to cw-rails (roster_repair.rs:59), and cw-rails' roster is its solo one (d8704a23f), so there is no single roster to retire the members through until the key is one.
  - Boundary gate: 51, unchanged. No code is in this commit.

<!-- appendix -->

## phase-b-11 · 2026-09-26 — pb-membership narrows to cw-rails founding and admitting; the key flip joins pb-mesh-exit-mesh

<details><summary>reasoning, evidence, package</summary>

Reproduced at d52092565:

- `grep -rn load_or_generate_node_key` finds daemon.rs:1291 (create_mesh_with), :1614 (join) and :3115 (start_daemon), plus sovereign-mesh iroh_access.rs:648, where `MeshIrohAccess::start` binds the endpoint's `SecretKey` from that same key.
- commonwealth-rails gossip.rs:79 has `inference_capable: false`. acceptor.rs:1-35 lists three ALPNs (http to its own gossip listener, media, app), and there is no inference forward.
- commonwealth-rails lib.rs:25-31 says "It does not admit joiners", with no `/internal/join` and no invite minting.
- routes_internal/mesh_admin.rs:743 calls `membership::accept_join_with_identity`, which is defined at commonwealth-discovery membership.rs:236.
- ARCH_LAYERS.toml:1584-1589 is the fp-9 exception row, and :694 has commonwealth-discovery on cw-rails' except list.
- roster_repair.rs:59 forwards forget-member to `rails_client::forget_member`.
- boundary-gate (worker's run, `scripts/ralph-check.sh boundary`): FAILED, 51 violations. No code changed since.

The package's questions, answered:

1. Where the key flip lands: pb-mesh-exit-mesh (the recommendation).
2. Keep the row whole with the delta: declined. It is the operator's fork, and the behaviour-preserving option makes it unnecessary.
3. The roster cleanup moves with the flip, for the reason in the ledger.

Cost accepted: until pb-mesh-exit-mesh, two processes on one node can found meshes, each with its own key. The narrowed row writes this down.

What would falsify this: if pb-membership's worker finds that cw-rails cannot admit without the daemon's key or roster (for example, if an invite minted by cw-rails must name the daemon's identity for existing peers), the split is wrong and the row goes back to the operator as option 2. The same holds if pb-mesh-exit-mesh's premise check finds a traffic class that cannot be re-homed within its lift.

</details>
