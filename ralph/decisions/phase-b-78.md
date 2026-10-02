<!-- ledger -->

**phase-b-78 · 2026-09-30 · pb-ingest-rehome (split) · director** — this commit
- Needed: pb-ingest-rehome's shim half rested on a false premise. It said cli-llm's recipe-agent modules left with cli-llm's ingest half, but phase-b-70 kept recipe_agent_cmd and recipe_agent_live_trial svrn-side because both open svrn's store. Dropping the shim would leave them unresolved or add a red `sovereign-cli-llm → sovereign-recipe-author` edge. The re-priced total, ~1,500-1,800 lines against the row's 900, was at the scope guard's 2x line.
- Chose:
  - The modules stay svrn-side (phase-b-70 holds). They reach the recipe project and the tool bundle through the `HostedIngest` cli-llm already composes (chat_cmd/ingest.rs:72).
  - The row splits. `pb-ingest-rehome-daemon` mints the recipe-project port in sovereign-contracts, moves `RecipeAuthoringTools` into sovereign-recipe-author as a `ToolBundle`, and repoints the daemon (BOUNDARY unchanged). `pb-ingest-rehome` keeps the parent name: cli-llm repoints, then the shim and lib.rs:105-109 re-exports drop (BOUNDARY −1). The parent is reset from `[~]` to `[ ]` so the split runs first.
  - pb-cli-llm-ingest-move's line on the riders is corrected in place.
- Because:
  - Placement is phase-b-70's rule (principle 12), and FIVE_PROGRAMS §11 already records it.
  - Reusing the composed port extends the existing owner instead of adding one (principle 11, §2c).
  - The two halves have different proofs: stock-binary behaviour with the edge up, then the edge closing.
  - This commit changes no Rust. BOUNDARY stays at 10.

<!-- appendix -->

## phase-b-78 · 2026-09-30 — pb-ingest-rehome splits; the recipe-agent CLI stays svrn-side and reaches ingest through HostedIngest

<details><summary>reasoning, evidence, package</summary>

Package: ralph/next/phase-b/ctl/NEEDS_HUMAN.md at 783170954, archived at target/ralph/phase-b/pb-ingest-rehome-needs-human.phase-b-78.md.

Evidence, reproduced at 783170954. `git grep sovereign_tools::recipe_author` finds cli-llm riders at recipe_agent_cmd.rs:32-33 and recipe_agent_live_trial.rs:70-71. The daemon riders are at daemon.rs:786, boot.rs:704-705, daemon_services.rs:247, features_http.rs:25,192 and recipe_project_http.rs:38-39. The daemon tests name it at common/mod.rs:243,378,400, d6_surface_e2e.rs:47 and d8_surface_e2e.rs:69, and sovereign-tools at tests/recipe_author_loop.rs:32-33. The shim is sovereign-tools lib.rs:68 `pub use sovereign_recipe_author as recipe_author`, with re-exports at :105-109. recipe_agent_cmd.rs:281-290 opens `SqliteStateStore` at svrn's sovereign.db. sovereign-cli-llm is listed under svrn in quality/ARCH_LAYERS.toml. `ToolBundle` is a contracts trait (tool_bundle.rs:67). cli-llm holds a `HostedIngest` (chat_cmd/ingest.rs:14,72). FIVE_PROGRAMS.md:785-796 already keeps the two modules in cli-llm under phase-b-70 (2), so the design doc needs no edit.

Declined: moving the two modules into `svrn-ingest` with notes as `Arc<dyn RecipeNotes>`. It is fewer lines, but it reverses phase-b-70 for a module that opens svrn's store.

What would falsify this: the recipe-project port cannot carry live_trial's project ops (new/load/read_summary/write_summary/list_checkpoints/project_dir) without exporting the store's row types to a leaf (the store-to-leaf stop). Or `HostedIngest` is absent on the recipe-agent verb's path, so the verb has to compose ingest itself. Either sends pb-ingest-rehome-daemon back with a package.

</details>
