<!-- ledger -->

**phase-b-45 · 2026-09-28 · pb-ingest-dial-tools-local → the catalog edge folds into -close; -local is the corpus-engine half (BOUNDARY 23) · director** — this commit
- Needed: the worker stopped at census with no code edited. The row's −1 (`sovereign-tools → sovereign-enrichment-catalog`) cannot close here. The three `EnrichConfig` sites need an implementor that links sovereign-enrichment-catalog, and that crate depends on corpus-engine, so corpus-engine cannot implement the port. The daemon links neither catalog nor enrichment-build, so building it there opens a new red edge. The stock process has no ingest face to supply it, and building that face is -close's OUTCOME.
- Chose: package option (a). -local keeps the 42 corpus-engine sites behind `impl LocalCorpusPort for CorpusEngine`, expects BOUNDARY 23 unchanged, and is proven at its library entry points or the existing watched-folder and knowledge_view e2e tests, with a per-family grep PLANT. -close takes the catalog edge (its three sites, its port, an [ingest]-side implementor) and the standalone-svrn absence half for this family, and expects BOUNDARY −2.
- Because: charter "a false row premise" and "fold rows that touch the same files". Both halves that cannot run now depend on the stock ingest face, which -close already owns. This is the phase-b-44 precedent. Option (b) moves -close's LIFT into -local, and option (c) opens a red edge the prompt forbids. Boundary gate: EXIT=1, 23 violations (`cargo xtask boundary-gate` from corpus-engine/, this session). This commit touches no Rust.

<!-- appendix -->

## phase-b-45 · 2026-09-28 — pb-ingest-dial-tools-local's catalog half folds into -close

<details><summary>reasoning, evidence, package</summary>

Reproduced this session at 8fe678058:
- `grep -rn sovereign_enrichment_catalog sovereign/crates/sovereign-tools/src | grep -v '//'` finds atlas_context_manager.rs:61, local_corpus/atlas_dispatch.rs:83 and local_corpus/watched/enrich.rs:50,108. -atlas did not remove the atlas_context_manager read, so the row's proviso failed.
- sovereign-enrichment-catalog/Cargo.toml:26 is `corpus-engine = { workspace = true }`. Its src/config.rs:131 names `corpus_engine::enrichment::pipeline::CustomAtlasSpec` and :297 names `PhaseCache`. The implementor therefore cannot live in corpus-engine.
- `grep -n enrichment sovereign/crates/sovereign-daemon/Cargo.toml` is empty. The daemon constructs `LocalCorpusManager::init_with_recipes_dir` at bootstrap.rs:1684.
- sovereign-stock/src/main.rs:87 calls `sovereign_daemon::process::run` with the served and code faces only. There is no ingest face.
- boundary-gate: 23 violations, EXIT=1. sovereign-tools holds three of them (→ enrichment-catalog, → recipe-author, → corpus-engine).
- The row's trial (6c956fe13, removing both deps gives 23 → 21) already covers the full move. This rewrite reassigns which row closes which edge. It does not change any symbol, so no new trial was owed. -close's finish now names both edges and the trial's 21.
- FIVE_PROGRAMS is not edited. The design (ports, ingest face on the stock distribution, §12 3a placement) is unchanged; only the row split moved.

What would falsify this:
- -local's corpus-engine half turns out to need a catalog type in the port's signature (for example the watched config write needs `EnrichConfig` to cross). Then the halves are not separable, and -local should be folded into -close whole.
- -close finds a home for the catalog implementor that needs no stock face (some svrn crate already linking an [ingest] crate legitimately). Then the fold was unnecessary, and -local could have taken the −1.

</details>
