<!-- ledger -->

**five-programs-11 · 2026-09-24 · fw-4 (the cli-llm split) · director** — this commit
- Needed: fw-4 halted pre-edit (ctl/NEEDS_HUMAN.md). Priced as spec'd, the split raises the gate. It closes at most 5 edges and opens at least 10.
- Chose: the package's option 1. STRIKE fw-4 (`[x]`, no code, delta 0) and rescope REVIEW-mint-fp-cli-llm-split so it mints the dials that remove each half's [svrn] reach first. It mints a split row only once that row's priced delta is below 0. fp-55's `depends [fw-4]` is satisfied by the strike, since its leaf-budget mechanism does not need the split.
- Because: PROMPT.md's loop takes net-decreasing rows only, and fp-29 set the precedent of refusing a net-positive row. The charter prefers the smaller reversible step. Option 2 (land the split as a placement move at about +17) contradicts that rule and needs the operator's word. Option 3 has no sub-cut below 0. The worker found none, and the census confirms it: every moving module also names svrn members.

<!-- appendix -->

## five-programs-11 · 2026-09-24 — the cli-llm split waits on its dials

<details><summary>reasoning, evidence, package</summary>

Reproduced before deciding (director, toolbox, tree clean at ed462c224):

- `cargo xtask boundary-gate` from corpus-engine/ → `boundary-gate FAILED (62 violation(s))`, EXIT=1. It shows 14 red `sovereign-cli-llm →` lines: enrichment-catalog, enrichment-build (normal + dev), inference, gliner, mesh, pods, pipeline, authoring-harness, eval, cli-mesh, commonwealth-state, corpus-engine, corpus-engine-notes.
- `python3 target/ralph/five-programs/split_scan.py` (groups exactly as §11 lists them) matches the package. bench 60,523 lines: core 204, cli_shared 56, tools 36, store 4, plus crate::chat_cmd 32. ingest 47,114 lines: core 91, cli_shared 125, tools 65, workflow_host 20, daemon 3, store 4, plus chat_cmd 14. svrn remainder 16,983 lines: it names corpus_engine 56, inference 4, mesh 1, pipeline 1, cli_mesh 1, commonwealth_state 2, notes 2, and reaches enrich_cmd (the enrichment crates) 7 times.
- quality/ARCH_LAYERS.toml [svrn] `crates` lists sovereign-core, -tools, -store, -cli-shared, -daemon, -workflow-host and -cli-llm as members, not [[package_leaf]]s. A new [ingest] or [bench] crate naming any of them is a red line.
- The "six zero-ref deps first" premise was spent at 51cd76669 ("drop 7 dead dependency lines — 115 -> 108"). The free delta is 0.

Pricing: the edges that close on cli-llm are those whose refs sit only in the moving halves: gliner, pods, authoring-harness, eval, and enrichment-build dev. That is 5 at most. The opened edges have a floor of 6 from ingest (core, cli-shared, tools, store, workflow-host, daemon) plus 4 from bench (core, cli-shared, tools, store), 10 in all, before counting both halves' reach into cli-llm itself for chat_cmd. chat_cmd::bootstrap builds a Runtime, so it cannot become a leaf.

Refused alternatives: option 2, the placement move at +17, needs the operator. It is available to them if they want the halves' true reach on the scoreboard. Option 3, a partial cut: every closable edge's callers live in modules that also name svrn members.

Falsified if: a dial row lands that removes a half's core/cli_shared/tools reach and the split then prices below 0 (the rescoped mint row exists to find that). A second way: the operator promotes sovereign-cli-shared (or a carved help/setup_config piece) to a [[package_leaf]], which would change the opened-edge floor.

</details>
