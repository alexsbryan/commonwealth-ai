<!-- ledger -->

**five-programs-14 · 2026-09-24 · fp-57 (authoring-harness → corpus-engine: neither arm closes it) · director** — this commit
- Needed: fp-57 halted (ctl/NEEDS_HUMAN.md). Neither of the row's two arms closes the edge. A vocabulary carve cannot, because checks.rs judges `recipe.extract/filters/chunk/index` through corpus-engine's `Recipe` config tree and `StageOutputs` carries `ExtractedDoc`. A dial of drive.rs cannot either, because checks.rs still names `Recipe`. The row's cited "fp-24 bench-dial precedent" was never queued.
- Chose: the package's option 1. `sovereign-authoring-harness` moves from [bench] to [ingest] in ARCH_LAYERS.toml. The dead `pub use sovereign_authoring_harness as authoring_harness` in sovereign-eval (lib.rs:15-20) and its Cargo.toml dep (:24) are dropped in the same commit. Predicted boundary 64 → 63.
- Because: principle 12. The crate judges ingest's recipe stages in ingest's own config language. It is driven by ingest's runner and consumed only by svrn (daemon recipe_http.rs, cli-llm recipe_cmd.rs). No bench member uses it. The same file's noun-convergence rung-3 adjudication (ARCH_LAYERS.toml:57-61) already calls it a shipped end-user capability ("the product does NOT ship without it"), and that contradicts [bench]'s "an evaluator that serves no wire". The 2026-09-22 director resolution that kept it in [bench] rested on a dial row that was never minted. REVIEW-AFTER: this reverses a prior director placement. The charter neither reserves package placement for the operator nor explicitly grants it.

<!-- appendix -->

## five-programs-14 · 2026-09-24 — re-place sovereign-authoring-harness from [bench] to [ingest]; drop sovereign-eval's dead re-export

<details><summary>reasoning, evidence, package</summary>

Reproduced by the director at 92ea98e6f:

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → `FAILED (64 violation(s))`, including `[bench] sovereign-authoring-harness → corpus-engine`, `[svrn] sovereign-cli-llm → sovereign-authoring-harness`, and `[svrn] sovereign-daemon → sovereign-authoring-harness`.
- checks.rs:6-10 imports `corpus_engine::harness::{coverage, doc_id, recipe_hash, …}` and `corpus_engine::Recipe`. It reads `recipe.extract` (:172, :238), `recipe.filters` (:268), `recipe.chunk` (:355) and `recipe.index` (:436). corpus-engine/src/harness/stage_output.rs:6 carries `crate::extractors::ExtractedDoc`.
- `git grep authoring_harness` across sovereign-eval, agent-bench, tdd and agent-tools finds only sovereign-eval/src/lib.rs, which is the re-export and its comment. `sovereign_eval::authoring_harness::` has no consumers.
- The crate's deps are corpus-engine (an ingest member), serde, serde_json, sha2 and workspace-hack (a leaf). No new red edge opens from [ingest].

Refused:
- Option 2, keeping it in [bench] behind a port trait. That is a new abstraction over about 600 lines with three callers, which is scope, not a row.
- Option 3, a 3a leaf for Recipe + ExtractedDoc + StageOutputs. It is reserved for the operator, and it fails rung 2 because only one program speaks that language (recipe.rs bundles TOML and errors).

Consequences for other rows. fp-43 keeps its purpose, now as an ingest dial: the svrn → authoring-harness edges stay red as svrn → [ingest]. The corpus-engine fan-in stays at 17, because the harness's dep remains. fp-57's "fan_in back to 16" check is struck.

Falsified if the fp-57 commit's boundary count is not 63. Also falsified if moving the crate reds any [ingest] edge, or if a bench member turns out to need the harness after all (the build breaks when the eval dep is dropped).

</details>
