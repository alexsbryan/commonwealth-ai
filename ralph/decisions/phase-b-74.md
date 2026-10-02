<!-- ledger -->

**phase-b-74 · 2026-09-30 · pb-cli-llm-ingest-move-remainder (worktree B) · seat as B's director (operator autonomy)** — this commit
- Needed: B's -remainder worker landed seven reaches, then stopped on four forks. (1) awareness runs linked into sovereign-cli's own process (main.rs:1122-1134, feature `awareness`), where no `HostedIngest` exists. (2) bench_atlas is placed by the row's own rule. (3) probe vault-build's tiered run is metered: its own provider and a metered GLiNER extractor (vault_build.rs:625-700), which the composed ports do not take. (4) Remainder modules also reach ingest crates through `crate::enrich_cmd::*` aliases.
- Chose:
  - (1) sovereign-cli's `awareness` arm execs the composed LLM sibling with the spelling unchanged. awareness writes and extracts through two new AtlasPort methods, `write_atlas` and `extract_entities`. sovereign-cli's optional edge to cli-llm goes.
  - (2) bench_atlas moves with the parent, and `svrn bench atlas` execs `svrn-ingest` with the spelling unchanged.
  - (3) An ingest-face runner that takes vault-build's provider and extractor, plus `gliner_chunk_extractor`, so the metering stays.
  - (4) -remainder's census covers alias spellings. The parent's census covers the modules that move.
  - Written into B's -remainder row (B-state commit). This record lands the ruling on `cut`.
- Because:
  - Principle 6: option 1(b) would make two awareness subcommands name ingest absent on the awareness build, a user-visible loss. The exec path is the dispatcher's one route for LLM verbs (principle 8).
  - Principle 12: bench_atlas touches only ingest's config, manifest and pipeline.
  - Principle 7: an instrument keeps measuring what it measured.
  - A spelling that resolves to an ingest crate is an edge, whatever the alias.
  - Nothing a user sees changes. This commit changes no Rust.

<!-- appendix -->
