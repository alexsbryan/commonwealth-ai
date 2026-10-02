<!-- ledger -->

**phase-b-75 · 2026-09-30 · pb-cli-llm-ingest-move (worktree B) · seat as B's director (operator autonomy)** — this commit
- Needed: B's move reached BOUNDARY 12 (−5), with LIFT(ingest) passed, LINT exit 0 and TEST(sovereign-pipeline) 307/0. The last edge is a dev-dependency, `sovereign-cli-llm → corpus-engine` (Cargo.toml:164-168), used only by two examples: coverage_layers_probe.rs (CorpusEngine, search_raptor_summaries, AnnSeedTable) and epistemic_demo.rs (a real engine handed to svrn's `coverage_probe`, which takes `Arc<dyn CorpusReadPort>`; cited by EPISTEMIC_STATE.md:485). The worker offered delete, move to stock/examples with the face widened, an `[[exception]]`, or accept −5.
- Chose: none of those four. Each instrument is placed by what it measures.
  - coverage_layers_probe measures ingest's search layers, so it moves to an ingest crate's examples/, with its embed function from the corpus-index adapter (684a1813d).
  - epistemic_demo drives svrn's port-typed coverage probe, so it becomes an `epistemic` mode of the existing hidden `svrn __probe`, run through the composed binary. EPISTEMIC_STATE.md:485's run line follows.
  - The dev-dependency goes, and BOUNDARY reaches 11 (−6).
- Because:
  - Principle 12: an instrument lives with what it measures.
  - Principle 11: the probe verb (phase-b-58, -62, -63) and the composed binary (phase-b-70) already exist.
  - Deleting a documented instrument loses an ability.
  - A widened stock face or an exception trades the finish line for convenience.
  - Nothing a user sees changes; the probe is hidden.
  - This commit changes no Rust.

<!-- appendix -->
