<!-- ledger -->

**five-programs-24 · 2026-09-24 · REVIEW-mint-fp-atlas-residue (split: core mints, tools goes to the operator) · director** — this commit
- Needed: the mint worker priced both residue edges at ≥17 atomic rows against a cap of 8 and asked four questions: the cap, the core read port's home, where the 35 ingest-executing tool files go, and whether "corpus-engine keeps resolve/read_section_rows" still binds.
- Chose: split the row. `REVIEW-mint-fp-core-residue` carries core's 8 rows with the port home (corpus-index trait, corpus-engine impl), resolve (to the svrn side) and read_section_rows (to the reader leaf) decided in the row text. The tools edge becomes a NEEDS-OPERATOR appendix line. No code changed. Boundary gate FAILED at 62 violations (reproduced at dce70675b, EXIT=1).
- Because: splitting rows is the charter's. The port home follows principle 11: every type in the surface core calls is already in corpus-index, so the trait adds no dependency. Resolve and read_section_rows fall under §12 D1's own split (policy to svrn, raw reads to the reader). The tools placement is not: `svrn ingest` has a CLI-only wire (§2), so there is nothing to dial, and §12 D5 does not name these files.

<!-- appendix -->

## five-programs-24 · 2026-09-24 — atlas-residue split; core's forks decided, tools' placement left to the operator

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpatlas-20260924.md. Reproduced at dce70675b:

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → FAILED (62), EXIT=1.
- `git grep -l corpus_engine -- sovereign/crates/sovereign-core` → 30 files. `git grep -o 'corpus_engine::'` → 66. The same commands on sovereign-tools give 65 files and 287. The package said 20 residue files and 44 refs for core. That is the residue after the 30 carved refs, and it matches fw3-residue.txt.
- `Runtime.corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>` is at runtime.rs:287 and :457, and `acquisition.rs:334` holds the same type. `impl SealedIndexSource for corpus_engine::CorpusEngine` is at grounding/search.rs:52.
- Port surface types: `corpus_index::CorpusIndex` is at corpus-index/src/index/mod.rs:214, `IndexInfo` is at types.rs:264, and corpus-index already depends on corpus-engine-yield (ForegroundLease). `CorpusEngine::open_index` returns `Result<CorpusIndex>` (engine/mod.rs:2001). `usable_indexes` and `installed_indexes` return `Vec<IndexInfo>` (:1595, :1739).
- `resolve_evidence` is at corpus-engine/src/enrichment/atlas/resolve.rs:130. Its module doc (:2-15) says it decides scope, budget, title filter and scoring, which is policy. Its imports are `ChunkRequest` (atlas-reader context/views.rs:306), `ChunkSelector` (atlas-reader evidence_site.rs:206) and `ScoredChunk` (corpus-index types.rs:496), all leaves.
- `read_section_rows` (context.rs:84) reads `chapters.json` through `pipeline::chapter_manifest::ChapterManifest` (chapter_manifest.rs:26). That is a raw read, and the manifest type comes with it.

Why split and not raise the cap: one REVIEW row that mints 17 rows is queue growth by another name, and the two halves have no dependency on each other. Why core's forks are the director's: none of them widens a leaf's dependency budget or admits a leaf. The row halts NEEDS_HUMAN if the DTO or any signature needs a new corpus-index dependency. Option (b), 8 newtype wrappers, builds new where an existing port library serves. That is principle 11. Why tools is the operator's: the ingest-executing tools are svrn MCP verbs that run ingest in-process. A D2 dial needs an ingest server, and none exists. Moving the tools removes or relocates MCP tools (end-user-observable) or mints a serving process, and both are on the charter's operator list. The options are (a) move the tools to an [ingest] crate that serves a wire svrn dials, at the cost of a new serving surface plus at least 4 move rows and the dial rows, or (b) keep them in svrn and name the edge red. My recommendation is (b) until ingest has a wire for some other reason. A second process for one edge is scope the endstate does not otherwise need.

Falsified if the port mint finds that `list_entries`' DTO or `open_index_for_corpus` needs a dependency corpus-index lacks, or if moving resolve drags a corpus-engine-only type that the imports above do not show.

</details>
