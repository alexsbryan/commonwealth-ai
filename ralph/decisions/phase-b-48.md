<!-- ledger -->

**phase-b-48 · 2026-09-28 · pb-ingest-dial-tools-doubles → recipe tests move into recipe-author; manager stores become checked-in leaf fixtures; `Recipe` parse split structural/generic (BOUNDARY 23) · director** — this commit
- Needed: the worker stopped -doubles at census with no edits. Two premises were false. The recipe descriptor has no home in sovereign-contracts, and three `AtlasContextManager` tests need engine-written stores on a path that no `AtlasPort` double reaches. The package also asked a judgment question about the `Recipe` parse tests.
- Chose: (1) The three recipe tests move to studio/crates/sovereign-recipe-author/tests, with corpus-engine as an intra-package dev-dependency; the notes-store lifecycle test stays in sovereign-tools. (2) Package option (A): the two stores are checked in as fixtures in corpus-engine-atlas-reader/testdata, reached through the `test-doubles` accessor, with an engine-side parity test and a regeneration test. (3) The worker's default for the parse tests: a structural check on the svrn side, a generic parse on the engine side, and the composed path left to pb-ingest-dial-daemon's PROOF. FIVE_PROGRAMS "Where a cross-program test lives" gains both cases, and the stale sovereign-tools/Cargo.toml comment is corrected.
- Because: charter "a false row premise", with both trials run and reverted. The recipe move compiles and all 17 tests pass; LAYER passes and BOUNDARY stays at 23. The copied stores open under the manager with the original assertions. Neither choice is option (a) (a composed test home) or (C) (lost coverage), so the phase-b-47 falsifier did not fire: with a fixture, the svrn test needs no engine in process. Boundary gate at 940851bb9: EXIT=1, 23 violations. This commit changes no Rust (the Cargo.toml change is a comment). REVIEW-AFTER: pb-ingest-dial-tools-doubles lands (did the fixture's regenerate-and-compare test catch a format change, and did the fixture stay under ~20 KB?).

<!-- appendix -->

## phase-b-48 · 2026-09-28 — recipe tests to their subject's crate; atlas stores as leaf fixtures

<details><summary>reasoning, evidence, package</summary>

Reproduced this session at 940851bb9:
- `grep -rln recipe_schema_descriptor --include=*.rs --include=*.toml .` finds only corpus-engine/src/recipe_schema.rs, corpus-engine/tests/main/recipe_schema.rs, recipe-author's write_structured.rs and recipe_schema.rs, and quality/DOMAINS.toml. Recipe-author's recipe_schema.rs:28-32 says the descriptor is INJECTED. sovereign-tools/Cargo.toml:13-15 claimed recipe-author reads it from contracts, which is false.
- Recipe-author and corpus-engine are both in package ingest's `crates` list (quality/ARCH_LAYERS.toml:1358-1369), so a dev edge between them is inside the closure.
- Trial 1 (reverted): the three files are copied into recipe-author/tests with `sovereign_tools::` → `sovereign_recipe_author::` and `sovereign_core::` → `sovereign_contracts::` (sovereign-core re-exports those from contracts). The lifecycle test is cut from the moved loop copy, and the two moved tests are cut from the remnant. Adding `corpus-engine` to recipe-author's `[dev-dependencies]` and running `cargo test -p sovereign-recipe-author -p sovereign-tools --test recipe_schema --test recipe_author_tools --test recipe_author_loop` gives EXIT=0: 6+8+2 pass in recipe-author and 1 in sovereign-tools. `cargo xtask layer-gate` passes. `boundary-gate` gives EXIT=1 with 23, the same as the clean tree. Raw: target/ralph/phase-b/trials/t-doubles-recipe-move-940851b{.log,-boundary.txt,.diffstat}.
- atlas_context_manager.rs:603 calls `corpus_engine_atlas_reader::opener::open_walk_provider_blocking`, and :666 calls `AtlasGraph::load_from_disk`. `grep 'create_table|write_csr|fn write_' corpus-engine-atlas-reader/src` finds no store writer (only investigation_graph's JSON and the ports.rs trait methods).
- Trial 2 (reverted): a throwaway test wrote `write_atlas_fixture(EMPTY_ATOMS)` and `write_wiki_fixture` into target/ralph/phase-b/trials/fixture-trial, copied them to a fresh tempdir, and ran `manager_for(copy)`. `graph("atomish")` returned Some, the wiki `walk_provider` returned Some, and Alpha (`entity-53ac3c012920dadb`) had 1 edge. The stores are 19 files and 13,318 bytes.

Options not taken:
- A leaf home for the descriptor JSON. It moves an ingest data file for tests alone; the row does not name it, and the injection seam already exists.
- A `RecipeTester` double for these tests. Their subject is recipe-author running against the real tester, and that crate can hold the real tester.
- (B), a composed test crate, and (C), dropping the memoization coverage. Both are the operator's, and (A) needs neither.
- A second copy of knowledge_view_recipes.json in corpus-engine. It would drift silently, which is principle 8.

What would falsify this:
- The recipe-author dev edge trips a gate that the trial did not run: a studio extraction gate, or size-gate on recipe-author's test key. Then the tests need another home, and that goes to the operator.
- The lance stores are not byte-portable across a copy, or they carry absolute paths. Trial 2 says they are portable on this host; the macOS peer has not been tried.
- The engine parity test cannot compare a fresh store with the checked-in one without byte equality (lance manifests carry uuids). Then it compares what the leaf reads (atoms and edges), as the row says, and not bytes.
- The fixtures grow past ~50 KB, or a third store class appears. Then generate the fixtures in a build step instead, which is an operator question because rule 3a forbids build.rs.

</details>
