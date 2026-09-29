<!-- ledger -->

**phase-b-44 · 2026-09-28 · pb-ingest-dial-tools-atlas → closed at 936838db2 on entry-point proof; the absence half moves to -close · director** — this commit
- Needed: the worker built the outcome (82b4afd0b..70194753a, proof record 936838db2) and stopped because half the PROOF could not run at this tree. The row said typed_extension and summary_atoms were "called through the tool registry", but neither is registered. It also asked for "ingest absent by name on a standalone svrn", and that composition does not exist until -close.
- Chose: accept the substitute proof. `run_typed_extension` and `write_summary_atoms` write through `AtlasPort` via `IngestAtlas`, and the next read sees the write (entry-point tests), plus the per-family grep PLANT. The row is rewritten with the false premise named and marked `[x]`. The run-time absence half moves to pb-ingest-dial-tools-close, whose PROOF now also requires `FolderTieredProvider::post_finalize_corpus`, the one svrn run-time caller of `run_typed_extension`, to report ingest absent on the standalone svrn.
- Because: charter "a false row premise" (the worker's census is the input). The absence half belongs to the row that builds the standalone composition, since -close's OUTCOME already says "a standalone svrn reports ingest absent by name". Folding it there keeps one proof per outcome and adds no row. Boundary gate: EXIT=1, 23 violations (`cargo xtask boundary-gate` from corpus-engine/, this session), matching the row's "expect 23". This commit touches no Rust.

<!-- appendix -->

## phase-b-44 · 2026-09-28 — pb-ingest-dial-tools-atlas proven at its library entry points; run-time absence folds into -close

<details><summary>reasoning, evidence, package</summary>

Reproduced this session at 936838db2:
- `grep -rn -E 'typed_extension|summary_atoms' sovereign/crates/sovereign-contracts/tool-manifests/` finds nothing. A grep for string ids `"typed_extension…"`/`"summary_atoms…"` across sovereign/crates hits only a bench phase label (bench_cmd/vault_report.rs:613). The only callers are cli-llm atlas_cmd/typed_extension.rs:145, enrich_cmd/summary_atoms.rs:46 and conv_tiered_provider.rs:1125. The registry premise was false.
- `bash target/ralph/phase-b/famgrep.sh` gives 0. With `fn _plant() { let _ = corpus_engine::enrichment::atlas::write_atlas_gaps; }` inserted at the head of atlas_phase/gaps.rs it gives 1, and 0 again after `git checkout --`. (A plant appended at the end of the file reads 0, because the grep stops at `#[cfg(test)]`. That is the grep's intended scope, not a hole.)
- `sovereign-test.sh --package sovereign-tools --filter typed_extension`: pass 27, fail 0. `--filter summary_atoms`: pass 5, fail 0.
- `cargo xtask boundary-gate`: 23 violations, EXIT=1.
- The port is `corpus_engine_atlas_reader::ports::AtlasPort` (corpus-engine-atlas-reader/src/ports.rs), implemented once by `corpus_engine::IngestAtlas` (corpus-engine/src/engine/atlas_port.rs:31).
- Size: `git diff --shortstat 44d3beb42..936838db2 -- . ':!ralph'` gives 56 files, +1,355 / −654. That is under the ~2,800 LIFT, so phase-b-43's second falsifier (over 2× LIFT) did not fire.
- The worker's lint and the full-crate test runs (sovereign-tools 725, cli-llm 866, atlas-reader 181) were not re-run here. They are the worker's reported numbers.

What would falsify this: -close cannot make `post_finalize_corpus` (or any atlas entry point) answer "ingest absent" at run time without re-introducing a corpus-engine name into the atlas family, which the famgrep would show. That would mean the port is not the whole seam. A second falsifier is a real tool-registry surface for these pipelines turning up (an MCP or tool id that calls them). Then the registry proof was runnable after all, and the -atlas row owes it.

</details>
