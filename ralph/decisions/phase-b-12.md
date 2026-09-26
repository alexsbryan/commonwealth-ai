<!-- ledger -->

**phase-b-12 · 2026-09-26 · mesh traffic after the endpoint · seat** — this commit
- Needed: phase-b-11 moved the key flip into pb-mesh-exit-mesh and gave that row a premise: any of the daemon's iroh traffic classes (peer inference, guests, offers, rpc-worker) that no earlier row re-homed onto cw-rails is re-homed there, or the row halts. But pb-mesh-exit-transport runs first and closes the daemon's iroh and `peer_contact` residue with no such guard. And no row re-homes those classes: cw-rails' acceptor carries only gossip, media and apps (acceptor.rs:1-35), with `inference_capable: false` (gossip.rs:79). Carrying inference is phase-c's pc-inference-origin, which phase-b-2 deferred as closing no edge.
- Chose: the fork becomes `HUMAN-pb-mesh-traffic`, placed LAST in the file, with pb-mesh-exit-transport depending on it. The options in the row are (a) a narrowed inference origin in Phase B (recommended), (b) the endpoint stays until phase-c and the gate does not reach 0, and (c) accept the dark window.
- Because:
  - The CHARTER leaves an end-user-observable delta to the operator, and a worker would only find this mid-row.
  - Placed last, `Queue.current()` reaches the row only once nothing else is ready. A simulation at 991b73ec2 runs 23 units first, and only mesh-exit ×3, pb-daemon-adopts and pb-distribution wait.
  - Principle 10: the question lives in the queue, not in a seat's memory.
  - Boundary gate: 51, unchanged. No code is in this commit.

<!-- appendix -->

## phase-b-12 · 2026-09-26 — the mesh-traffic question becomes an operator row the loop reaches last

<details><summary>reasoning, evidence, package</summary>

Evidence is pb-membership's census package (the NEEDS_HUMAN resolved by phase-b-11): the daemon's `node_key` keys `MeshIrohAccess::start` (sovereign-mesh iroh_access.rs:648). That endpoint forwards gossip, `/internal/join`, `peer_addr` (peer inference, daemon.rs:3763), `guest_addr`, media, offers, the rpc-worker ALPN and the apps ALPN (daemon.rs:4355-4378). `install_iroh_access` (daemon.rs:417-444) routes every iroh class out through it.

Falsifier: if pb-mesh-exit-transport's census shows the transport residue is separable from the endpoint (the endpoint living wholly in sovereign-mesh, which pb-mesh-exit-mesh owns), the dependency moves to pb-mesh-exit-mesh and transport runs unblocked.

</details>
