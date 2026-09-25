<!-- ledger -->

**five-programs-49 · 2026-09-24 · fp-84 · director** — this commit
- Needed: fp-84's dev edge `sovereign-mesh → sovereign-work-atlas` (a688f7627) raised boundary-gate from 54 to 55. The row's premise, "layer-gate exempts dev edges", is true only of layer-gate. boundary-gate counts dev edges on purpose (`corpus-engine/xtask/src/boundary_gate.rs:668-671`). fp-84's commit body says "after: 54", but that number was measured before the Cargo.toml edit. Reproduced at bb18215b3: 55 violations, EXIT=1, and the edge is the only one added.
- Chose: keep `work_atlas_store.rs` in sovereign-mesh and record the edge as a NEEDS-OPERATOR appendix line under [cmnwlth], which is an owner REVIEW-handoff-phase-b accepts. Only the operator can close it: either an `[[exception]] package = "cmnwlth"` or a crate outside every package. The Cargo.toml comment now says the gate counts the edge. Boundary gate 55 (EXIT=1). No code changes.
- Because: every host inside a package adds at least one edge. Moving the test back to the daemon (i) adds nothing today, but fp-87 then fails its crate-wide `MeshStore|MeshReplicatedKv|commonwealth_state` bar and its daemon dev-dep removal. Hosting it in sovereign-work-atlas (ii) needs sovereign-mesh, commonwealth-state and commonwealth-core as dev-deps, so +3 [code] edges. The four crates outside every package are oicp-conformance, whose minimal dependency budget is its whole purpose, and the desktop, mobile and studio apps. None of them fits.

<!-- appendix -->

## five-programs-49 · 2026-09-24 — fp-84's dev edge stays, owned by a NEEDS-OPERATOR line; the fix is an exception or a new crate outside every package, both the operator's call

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp84b-20260924.md (git-excluded), written by fp-85's worker.

- `cargo xtask boundary-gate` (corpus-engine/, toolbox, bb18215b3): 55 violations, EXIT=1. The only [cmnwlth] line not in `target/ralph/five-programs/boundary.log` (22:37, before a688f7627) is `sovereign-mesh → sovereign-work-atlas: a dev dependency leaves the package closure`.
- boundary_gate.rs:668-671: "a build- or dev-edge breach counts ... a third party who lifts a package carries its tests". ARCH_LAYERS.toml:6's dev-edge exemption applies to layer-gate only.
- The test needs `commonwealth_state::MeshStore`, `sovereign_mesh::peer_adapter::MeshReplicatedKv` and `sovereign_work_atlas`. Those crates belong to [cmnwlth] and [code] (ARCH_LAYERS.toml:1327-1340). sovereign-work-atlas depends only on sovereign-contracts, kernel-types and sovereign-time.
- The crates outside every package and every leaf are oicp-conformance, sovereign-desktop, sovereign-mobile, sovereign-studio and xtask (census over the root `[workspace] members`). boundary_gate.rs:716: "A crate in no package is not this gate's business."
- Decision 48 named this exact falsifier ("if sovereign-mesh's new dev edge on sovereign-work-atlas raises BOUNDARY"). It fired. The placement it chose (sovereign-mesh over commonwealth-state, so the `MeshPeer` twin is not kept) still costs less than every other packaged host, so the placement stands and only the claim that it was free was false.

Operator options, in the appendix line: (a) `[[exception]] package = "cmnwlth"` for the dev edge, citing the cross-package contract `private_app_id_matches_gossip_exclusion_list`. It costs one row and changes nothing else. DIRECTOR'S RECOMMENDATION. (b) A new test-only crate outside every package that hosts compositions across packages. It costs a crate, and it sets the precedent for a place where cross-package tests go. (c) Move the private app-id literal into sovereign-contracts so the pin becomes structural, and move the replication tests to the ring_sync tests. It weakens the atlas-over-real-store coverage and is not prescribed by any row.

REVIEW-AFTER: the operator's answer on the appendix line. This is falsified if some host inside a package can run the test with its assertions verbatim and add no counted edge.

</details>
