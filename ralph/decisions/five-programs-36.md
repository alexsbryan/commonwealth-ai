<!-- ledger -->

**five-programs-36 · 2026-09-24 · REVIEW-mint-fp-state-pure-rust · director** — this commit
- Needed: the resumed mint halted again. The census behind five-programs-35 counted daemon files that name `commonwealth_state` (16), not files that use the store through AppState fields (30), and no row owned FabricPart's constructor, which takes a concrete `MeshStore`. The measured need is 13 rows against a cap of 11.
- Chose: the cap is raised to 13 and the Fabric port stays its own sovereign-mesh row. Every daemon row, and fp-42's close, must meet a no-laundering bar: zero `commonwealth_state|MeshStore|MeshReplicatedKv` hits in the daemon crate, and a test that watches the typed writers reach the cw-rails doors. Boundary gate 54, unchanged.
- Because: a bar that only checks the Cargo edge can pass while 14 files still read an in-process store, which is a gate with no failing input (ARCH 5). Folding the Fabric row into the host row would cross two crates and two programs in one commit (ARCH 2). REVIEW-mint-fp-mesh-dial depends on fp-42, so the mesh residue cannot absorb the port.

<!-- appendix -->

## five-programs-36 · 2026-09-24 — state mint resumes at cap 13 with a no-laundering bar

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpstate2-20260924.md. Reproduced by the director at 7d1b2c11b:

- `git grep -l commonwealth_state -- sovereign/crates/sovereign-daemon/src` gives 16 files. Adding the files matching `mesh_store|contribution_emitter|activity_emitter|peer_preferences|inference_store` gives 30 unique files. The tests tree has 35 files naming the crate and 38 naming or using it, which is still 4 rows at ≤10 files each.
- `MeshReplicatedKv` appears in daemon src at bootstrap.rs, daemon_services.rs and routes_mesh_kv.rs. It wraps `commonwealth_state::MeshStore` (sovereign-mesh peer_adapter.rs:26,32).
- `FabricPart::new` at sovereign-mesh/src/fabric.rs:405 takes `mesh_store: Arc<MeshStore>` and builds a `ContributionEmitter` over it. The daemon constructs `commonwealth_state::MeshStore::in_memory()` at daemon.rs:3035.
- `list_models`, `set_model_info` and `get_local_embed_model` are sync (commonwealth-state store_adapter.rs:90,110,169).
- The boundary log (target/ralph/five-programs/boundary.log, 17:58) shows 54 violations. The only commonwealth-state edges are daemon (line 69) and cli-llm (line 61). sovereign-mesh is in the [cmnwlth] closure, so its own commonwealth-state dep is legal. daemon → sovereign-mesh is red (line 72), and REVIEW-mint-fp-mesh-dial, which owns its residue, depends on fp-42.

Options weighed. Compressing to 11 means folding the Fabric port into the host row and a third daemon-src row into the test rows. That gives multi-crate commits and breaks the ≤10-files atomicity the other rows keep. The cap was set before this measurement, the same situation five-programs-35 corrected. For the bar, the worker proposed zero `mesh_store|MeshReplicatedKv` hits. The director checks for types instead of field names: after the flip, an AppState field called `mesh_store` may legitimately hold the port or cache, and renaming it would be scope growth. The dial test covers what a field-name grep was meant to catch.

Falsified if the Fabric row turns out to need daemon edits to compile. In that case the row's one-crate premise is false and it must merge with the first daemon-src row. It is also falsified if a daemon row meets the grep bar while a writer still lands in a process-local store, which would mean the dial test is not watching the right call.

</details>
