<!-- ledger -->

**phase-b-50 · 2026-09-29 · pre-flight of the 19 open rows at HEAD: two re-orders, three splits, and each census stop the rows would have met written into the row that meets it · seat** — this commit
- Needed: the operator asked for the frozen queue to run to completion without stalling. Three read-only audits of every open row at 1cf9b3602–68e91b44b, each claim re-checked by the seat in the tree, found rows that would stop as written. Every one of the 23 red edges and 4 svrn exceptions already has an owning row, so the gaps are in ordering, in the premises the rows state, and in PROOFs.
  - pb-ingest-dial-daemon cannot compile as ordered. After the row the daemon holds only ports, yet it still hands a concrete `CorpusEngine` to three crates whose own rows come later: grants (auto_recover.rs:204, shard_manager.rs:74), mesh (gossip.rs:152,257,462; capabilities.rs:65) and the authoring harness (drive.rs:81, called at recipe_http.rs:620). Its size is 124 non-comment `corpus_engine::` lines in 36 src files, plus 44 test constructions in 36 test files, against a LIFT of ~2,000.
  - pb-serve-distributes' discovery has nothing to read. cw-rails' `/v1/mesh/status` members carry no capabilities and no dial data (commonwealth-rails api.rs:192-218), and `AnchorProfile` has no RPC port (oicp-types capabilities.rs:178).
  - pb-cli-llm-bench-move contradicts itself: bullet 1 refuses corpus-engine-atlas-reader, and the repoint bullet names it. Five ingest-group sites name `eval_cmd`, so the move would open cli-llm → bench-crate.
  - pb-mesh-dissolve deletes the shims that `sovereign-cli-mesh/src/mesh_pod.rs:396-603` reaches serving-host through (sovereign-mesh lib.rs:100-101). pb-pods-verb and pb-mesh-dissolve each name the other as that code's owner.
  - pb-distribution's LIFT(svrn) test phase has never run. About 20 svrn test files resolve paths outside their crate. The svrn smoke abstains without `SERVE_BIN` (program-lift.toml:81). landing/install.sh:32 installs no sovereign-stock, which `svrn daemon start` execs (daemon_bin.rs:19).
  - pb-meshapp-rest removes `POST /v1/edit_predictions` from svrn while `[lift.svrn.run]` asserts it (program-lift.toml:92).
  - pb-mesh-exit-transport's cutover touches live host state the row does not spell out. After the switchover, nothing starts cw-rails at boot: sovereign.service is the only enabled unit.
- Chose:
  - Re-order: pb-grants-merge runs before pb-ingest-dial-daemon.
  - Split: pb-ingest-dial-daemon into -ports, -tests and the closing row. pb-distribution gains -svrn-lift.
  - Every other finding goes into the row that meets it as a "Pre-flight (phase-b-50)" bullet, with the evidence and the seat's ruling.
  - Rulings by the rule that already governs each case:
    - Port vocabulary goes with its port (phase-b-49): `MergePhaseProgress` to corpus-index with grants' merge port, and `axis_catalog` to understanding-vocab.
    - Additive wire fields with serde defaults, so no existing client changes: the roster's capabilities, dial data and `rpc_port`, and the turn's skill and sealed-scope.
    - The old-path re-export rule is waived where the re-export would itself be the red edge (bench-move, serve-placement).
    - In-process over a new route where the verb today needs no daemon (`svrn corpus pull`).
    - `svrn mesh create` and `svrn mesh join` bring cw-rails up themselves.
    - install.sh installs every sibling the dispatchers exec, under one pin test.
  - One question goes to the operator: whether the switchover installs a cw-rails user unit so the node rejoins after a reboot (the default) or names that as a delta.
- Because:
  - Principle 11: every red edge has an owner, so no new row is needed, only re-chunks.
  - Principle 8: one roster reader and one mesh-of-two harness, reused by -distributes, -ranks and the switchover.
  - Principle 6: no lane verdict or verb behaviour changes silently.
  - The charter's rule: a row that meets a stated premise false stops, and a stop now costs a director session each.
  - Boundary gate: 23 violations at 68e91b44b (`scripts/ralph-check.sh boundary`, target/ralph/phase-b/boundary.log). This commit changes no Rust.

<!-- appendix -->

## phase-b-50 · 2026-09-29 — pre-flight: re-order, split, and premises written where they bind

<details><summary>reasoning, evidence, package</summary>

Method. Three read-only auditors each took a third of the open rows (ingest and cli-llm; serve and mesh-exit; meshapp, dissolve and distribution). They had no cargo and no edits. They were asked for per-row verdicts: PROOF feasibility on this host, stale premises, size against the 7200 s session, ordering hazards, and charter tripwires. The seat re-checked every claim used below:
- the grants and mesh signatures;
- that mesh's only engine calls are `installed_indexes` (capabilities.rs:101) and `index_dir` (:271), both on `CorpusReadPort` (corpus-index/src/source.rs:33,39), and that mesh links corpus-index (Cargo.toml:117);
- `MergePhaseProgress` at corpus-engine sharding.rs:1203;
- the counts: 124 lines in 36 files, and 44 constructions in 36 files;
- the cw-rails status JSON and `AnchorProfile`;
- the bench-move bullet contradiction (row lines 3 and 14);
- `axis_catalog.rs`, which names only `understanding_vocab::taxonomy::DiscourseMode`, no fs;
- the mesh_pod.rs shim sites;
- `ancestors()` in daemon_variant_census.rs:52;
- `SERVE_BIN` at program-lift.toml:81;
- install.sh's BINS;
- the enabled user units (sovereign.service, sovereign-toolbox.service; no cw-rails unit);
- Qwen3.5-4B.Q6_K.gguf in sovereign/models.

Host facts (2026-09-29): `sovereign mesh status` shows 1 of 16 online (RuggedFox). Every PROOF runs on this host, so the mesh-of-two is two processes on loopback. That suffices, because every bar in these rows is relative (the same topology on both sides).

Not checked by the seat (the auditors reported them, and each row's census confirms or corrects): exact line drift beyond the cited ones, the 60,715 and 44,483 line counts of the two cli-llm groups, and the 51 `sovereign_tools` sites in the ingest group.

What would falsify this: a row that still stops on a premise this pre-flight wrote into it. That would mean the pre-flight read the tree wrong, and the director corrects the bullet rather than working around it.

</details>
