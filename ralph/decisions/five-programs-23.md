<!-- ledger -->

**five-programs-23 · 2026-09-24 · REVIEW-mint-fp-cli-llm-split (neither half prices below 0) · director** — this commit
- Needed: the mint worker measured both split halves and halted at the cap: driving either half below 0 needs at least 18 rows against a cap of 8, and most of them are leaf admissions.
- Chose: close the row by its own exit clause ("a half whose reach cannot fall below 0 stays in cli-llm with its edges named in the appendix"). No rows minted and no code changed. The row's eight appendix edges become NEEDS-OPERATOR lines. The chat-dial residue row is not minted because its premise is already spent. Boundary gate FAILED at 62 violations (reproduced at a8bc46f14, EXIT=1).
- Because: five-programs-11 already rules that a split is minted only when its priced delta is below 0. The census prices bench at +12 and ingest at ≥+5. Every arm that lowers those numbers is the operator's call: a leaf home for `cli_shared::help` / `core::setup_config` (§12 3a), bench dialling the daemon for its whole turn (§9, a drive rewrite), or dialling local GGUF loads (a behaviour change).

<!-- appendix -->

## five-programs-23 · 2026-09-24 — cli-llm split closed with no rows; its edges go to the operator

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fpclillm-20260924.md. Reproduced at a8bc46f14:

- `cargo xtask boundary-gate` (toolbox, corpus-engine/) → FAILED (62), EXIT=1.
- `python3 target/ralph/five-programs/split_reach.py bench`: core 204 refs / 44 files, eval 58, cli_shared 56 / 35, tools 36, corpus_engine 28, inference 21, gliner 10, corpus_index 9, turn_client 5, store 4. Refs leaving the group: crate::chat_cmd 32 / 15 files, enrich_cmd 11. `… ingest`: corpus_engine 265 / 70, cli_shared 125 / 66, core 91 / 41, tools 65, corpus_index 45, enrichment_build 25, workflow_host 20, pods 19, mesh 11. These match the package's figures.
- `grep -rhoE 'sovereign_inference::[a-z_]+' sovereign-cli-llm/src`: remote 25, embedded 2, reranker_standalone 1. sovereign-inference/src/lib.rs:26-28 is `pub mod remote { pub use oicp_client::*; }`. EmbedOnlyProvider::load is called at router_cache_cmd.rs:231 and router_fit_cmd.rs:403, and StandaloneReranker::load at inner_chaos/recall.rs:752. The package said "28 of 31". The unit differs (grep occurrences vs gate refs), but the shape is the same.

Why I did not mint: a repoint row for the 25 `::remote` refs leaves the edge red while the 3 loads stay, so it yields nothing, and the charter rules out adding scope. A split row that nets positive is what five-programs-11 forbids. Why I did not decide the package's questions 2-4: each one either admits a leaf (charter: operator) or changes which model produces vectors or how bench drives a turn (end-user-observable). The questions stand in the package with their options.

Falsified if a census finds an existing leaf that already exports the help/setup_config vocabulary (a repoint, not an admission), or if bench's in-process turn reach turns out smaller than 15 files once chat_cmd::bootstrap is counted, so that one dial row prices the bench half below 0.

</details>
