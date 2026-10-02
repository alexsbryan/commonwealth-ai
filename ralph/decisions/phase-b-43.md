<!-- ledger -->

**phase-b-43 · 2026-09-28 · pb-ingest-dial-tools → closed at 6c956fe13 on what landed; split into -atlas, -local, -close; the port seam fixed by §12 3a · director** — this commit
- Needed: the worker stopped at census with the row `[~]`. After 18 commits (+4,784 / −4,891) it had used more than twice the ~3,000-line LIFT, and three families were left. It asked three things: ratify where the ports live, choose per-call ports with engine types moved to leaves (a) or pipelines moved into ingest (b), and approve the split.
- Chose:
  1. Port home ratified. Ports live in the leaf that already owns every type they name: corpus-index beside `CorpusReadPort` for the landed ports, and corpus-engine-atlas-reader for the atlas family.
  2. Neither (a) nor (b) as posed. No engine-internal type moves to a leaf (3a rung 2, last bullet: not vocabulary, so never a leaf), and no whole module moves into ingest (these modules name `sovereign_core` 94 times, so that would open [ingest]→[svrn]). The seam is per pipeline. Code that names an engine-internal type moves into ingest's crate as the port's implementation, and the svrn tool shell stays with its `sovereign-core` half. FIVE_PROGRAMS §2c is edited to match, and its "port in sovereign-contracts" claim is corrected.
  3. Split. pb-ingest-dial-tools is `[x]` at 6c956fe13, with its outcome narrowed to families (1), (2), (6) and BOUNDARY 24 → 23. pb-ingest-dial-tools-atlas (family 4, 42 sites) and -local (family 3, 42 sites, BOUNDARY −1) follow, then -close (family 5 folded in at 9 sites, the dependency drop, the stock ingest face, the parent's PROOF and PLANT, BOUNDARY −1). pb-ingest-dial-daemon and pb-cli-llm-ingest-move now depend on -close.
- Because: principle 12 and §12 3a (a leaf holds vocabulary, and the engine's internal types are ingest's); FIVE_PROGRAMS §2c "extend, never re-own" (the implementation joins the engine that owns the types); the addendum scope guard (over 2× LIFT means stop with the split); and the charter's trial rule (every new row cites a compile trial run at HEAD). Boundary gate: EXIT=1, 23 violations at 6c956fe13 (`scripts/ralph-check.sh boundary`); this commit touches no Rust.

<!-- appendix -->

## phase-b-43 · 2026-09-28 — pb-ingest-dial-tools split at census; engine types stay ingest's and the pipeline seam moves instead

<details><summary>reasoning, evidence, package</summary>

Reproduced this session at 6c956fe13:
- `git log --oneline 56f70ded9..HEAD` shows 18 commits. `git diff --shortstat` reports 93 files, +4,784 / −4,891.
- `scripts/ralph-check.sh boundary` reports `boundary-gate FAILED (23 violation(s))`.
- Trial `t-ingest-dtools-rest-6c956fe` (trial.py, fresh worktree /home/alexbryan/dev/cw-pb-trial at HEAD, reflinked target): deleting sovereign-tools → corpus-engine and → sovereign-enrichment-catalog (normal deps) gives E0433 ×82 and E0432 ×11, 93 paths in 28 files, BOUNDARY 23 → 21, LAYER pass, reverted clean. At f7238e6d3 the same trial gave 246 paths in 54 files. The per-file split into families is in each new row.
- corpus-index/Cargo.toml:29 depends on sovereign-contracts, so contracts cannot name `corpus_index::Error`, `IndexInfo` or `CatalogConfig`. That confirms the worker's deviation from the row text. corpus-index/src/ingest_port.rs holds `CatalogIngestPort` (:65) and `IngestPluginPort` (:110).
- Import census of the remaining pipeline modules (typed_extension, summary_atoms, atlas_postinstall, atlas_phase, atlas_context_manager, conv_tiered_provider, enrichment_bootstrap, raptor_atlas, local_corpus, knowledge_view): 94 `sovereign_core::` paths. Among them are `traits::InferenceProvider` ×8+, `types::AssetState` ×8, `conv_tiered` rows, `StateStore`, `memory::EntityInventory` and `atlas_context::AtlasGraph`. Moving whole modules into an ingest crate is refused on that count alone.
- phase-b-30's falsifier pre-named the "move into ingest's library" alternative, and phase-b-33 item 7 authorised narrow ports per family. This decision keeps both: narrow ports, and behind them the engine-facing half moves into ingest.
- work_in_flight could not judge, because cw-rails was down at :9747. The loop was parked on this package.

Not trialled: the atlas port's exact method list, or which ingest crate (corpus-engine, understanding-host, enrichment-build) receives each moved half. Each row decides those by "whichever crate owns the type", and its worker census names them. The LIFTs extrapolate the parent's measured rate (≈67 changed lines per site). That rate was set by the light families, so it may undercount.

What would falsify this: a family whose port has to name an engine-internal type that no ingest crate can take without an [ingest]→[svrn] edge. Then the seam is wrong for that family, and it goes to the operator as a program-boundary question. A second falsifier is -atlas or -local passing 2× its LIFT; then the per-pipeline split is too fine, and whole-subsystem ownership (for example, knowledge_view as ingest's) needs pricing.

</details>
