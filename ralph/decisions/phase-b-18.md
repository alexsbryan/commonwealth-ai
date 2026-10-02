<!-- ledger -->

**phase-b-18 · 2026-09-26 · the mesh is a layer · operator** — this commit
- Needed: most of this campaign's HUMAN rows asked the same question, "which process should hold this for the mesh?":
  - HUMAN-pb-mesh-traffic;
  - fp-12's grants host, from five-programs;
  - fp-47's app registry, from five-programs;
  - the premises carried by pb-meshapp-grants and pb-mesh-exit-mesh.

  The daemon was the default answer because it already held everything. The design gave no rule to answer the question otherwise, so each instance escalated. The operator's diagnosis: the ambiguity is "what the daemon really should be and how it composes its functionality".
- Chose: the operator's direction, "daemons serve, mesh added by cw-rails — it should all gracefully LAYER rather than enmesh and embed", becomes FIVE_PROGRAMS §4 rule 8. The phase-b CHARTER gains a "Decide these" bullet, so a resolution session places anything mesh-facing by that rule without escalating. DAEMON_CORE §1 and TOPOLOGY §3.5 each gain a pointer where their node model predates the rule.
- Because:
  - Principle 12. cw-rails owns what makes a node a member: key, endpoint, roster, admission and advertisement. A program owns its surface, and svrn owns what a principal may see. Hosting a program's feature inside the mesh process, or the mesh inside a program, is a component holding another's lifecycle.
  - Principle 10. A rule a resolution session can apply replaces a question the operator has to be asked each time.
  - Principle 11. The rule describes what cw-rails already is for media (commonwealth-rails lib.rs:5-22: a shim "needs four things from a mesh and none of the rest of one"; `X-Mesh-*` headers on every forwarded request). It generalises from there.
  - Boundary gate: 51, unchanged. No code is in this commit.

<!-- appendix -->

## phase-b-18 · 2026-09-26 — FIVE_PROGRAMS §4 rule 8: the mesh is a layer, never a host

<details><summary>reasoning, evidence, package</summary>

Where the rule comes from:
- The operator, in session on 2026-09-26, after the HUMAN-pb-mesh-traffic due diligence.
- quality/TOPOLOGY.md §3.5, which gives the ring rule (a capability is placed by what its absence costs) and the nesting lattice (construction variants nest, "the nesting is in the type").
- quality/DAEMON_CORE.md §1, which gives the WireGuard node model.

What was dated: TOPOLOGY and DAEMON_CORE both put the node key and peer admission inside the daemon, which is the embed shape the rule replaces. Their durable parts carry forward. The `principal → Scope` table stays svrn's: cw-rails authenticates the peer, and svrn authorises what the peer may see.

How the rule is checked:
- Every program's lift must pass with cw-rails absent. LIFT(serve) already runs that way (pb-serve-program's PROOF).
- The boundary gate at 0 forbids a program crate linking the mesh transport.
- pb-rails-origins' PLANT forbids per-class hosting code in cw-rails.

What would falsify it: a capability whose journey cannot be served on loopback by any §2 program. For example, a peer-to-peer primitive with no local meaning. That capability is the operator's under the charter bullet, and it would show that a program needs a mesh-native surface of its own.

</details>
