<!-- ledger -->

**phase-b-15 · 2026-09-26 · pb-serving-kinds · director** — this commit
- Needed: pb-serving-kinds' worker halted at census (NEEDS_HUMAN, tree clean at 89170f100). The rerank half held. The NER half rested on a false premise: the row served GLiNER through the bare-string `EntityExtractor` port (traits.rs:207), but the daemon's boot load (bootstrap.rs:92-101) returns `LabeledEntityExtractor` + a corpus-engine `ChunkEntityExtractor`, and neither of its readers can take bare strings. The row's BOUNDARY −2 was also unreachable: registering the NER loader in sovereign-inference adds a red [cmnwlth] → [ingest] edge to sovereign-gliner, and moving gliner alone turns its own gliner → corpus-engine edge red.
- Chose:
  - Split the row. pb-serving-kinds keeps the kind registry and rerank (fp-69 retired, 51 → 51). The new pb-serving-ner, placed after HUMAN-pb-lanes-rerank, takes NER (51 → 49).
  - sovereign-gliner moves from [ingest] to [cmnwlth], and on to `serve` with pb-serve-program. `LabeledEntityExtractor`, `EntityMention` and `GlinerGeneration` move to sovereign-contracts as the one NER port, with `EntityExtractor` served by one adapter over it. `GlinerChunkExtractor` and `bounded_input` move into corpus-engine.
  - NER registers in-process only, and its route, client method and child role are named absences. pb-svrn-dials-serve mints the route, because that row makes the daemon's readers the route's first callers.
- Because:
  - Ladder rung 1: after the row, the NER kind's registration is gliner's only in-tree loader. cli-llm → gliner is pb-cli-llm's either way. No [ingest] crate depends on gliner, so the package move turns no edge red.
  - Extend, never re-own: corpus-engine already owns `ChunkEntityExtractor` and links sovereign-contracts, and contracts already holds `EntityExtractor`. One port, one loader.
  - A route with no caller is inventory. Every NER reader today is in-process.
  - The charter splits a row when its proofs differ. Rerank's proof is the registry PLANT plus the HUMAN lane delta; NER's is one `Arc` per process plus −2.
  - Boundary gate: 51, unchanged (`cargo xtask boundary-gate` from corpus-engine/, EXIT=1). No code in this commit.

<!-- appendix -->

## phase-b-15 · 2026-09-26 — pb-serving-kinds splits; gliner becomes a serving crate with its port in contracts

<details><summary>reasoning, evidence, package</summary>

Reproduced, 2026-09-26:
- `cargo xtask boundary-gate` shows 51 violations. The gliner edges are cli-llm, daemon and runtime-recipe → sovereign-gliner. runtime-recipe → sovereign-inference is excused (ARCH_LAYERS.toml:1620-1625, fp-69).
- runtime-recipe's only `sovereign_inference` use is `reranker_standalone::load_from_env` (lib.rs:985-1009). Its gliner use is `load_gliner` at lib.rs:835-859, which uses `DEFAULT_MODEL_ID`.
- sovereign-gliner's only corpus_engine uses are bootstrap.rs:8 and chunk_extractor.rs:10. chunk_extractor.rs and bounded_input.rs depend only on the trait, corpus-index and contracts.
- corpus-engine/Cargo.toml:70 already depends on sovereign-contracts.
- Workspace crates that depend on sovereign-gliner: cli-llm, daemon, runtime-recipe and desktop. sovereign-tools has only a comment.

The operator reserves new exceptions, leaves and user-observable changes. This decision takes none of them: no exception row, no leaf, and a default install sees no behaviour change, because with the env knob unset both loads resolve to the same model id.

Alternative considered: keep gliner in [ingest] and accept +1 red (inference → gliner) until pb-serve-program. Rejected because it adds a red edge to close two, and it leaves the kind's loader in a package the serve developer would then have to link.

Falsifier: pb-serving-ner's census finds an [ingest] crate, or the ingest lift, that needs GLiNER in-process and cannot take it through the contracts port. In that case gliner stays with ingest, and the NER kind's loader is injected into the registry by its host.

</details>
