# Prioritized cleanup list

**Status 2026-07-12: P0 #1-5 and P1 #7-11 executed** (see the Execution
record at the bottom). Remaining live: P1 #6 (fresh census — needs a
committed tree for the SCIP re-index), P1 #12 first slice, api snapshots
for the 4 hub crates (parked on a rustc ICE — see record), and all of P2.

Generated 2026-07-11 from the quality-program instrumentation:
`sovereign code arch-report` (SCIP census + declared↔observed deltas + git
temporal coupling), `quality/baselines/clippy_counts.tsv` (lint-gate),
`quality/baselines/fan_in.tsv`, and the §10 deferral ledger. Refresh the
inputs with `sovereign code arch-report` + `cargo xtask quality`; each item
names the metric that proves it done. Ordering inside a tier is the
suggested attack order.

**Caveat on observed metrics:** the SCIP index used for the census predates
the most recent carve-outs (it still shows `sovereign-core/src/traits.rs`),
so some coupling attributed to `sovereign-core` belongs to
`sovereign-contracts` on a fresh index. Items marked ⟳ should be re-read
after the next re-index (commit + clean tree → daemon reindexes).

## P0 — hours each; mechanical; do first

| # | Item | Evidence | Done when |
|---|---|---|---|
| 1 | **Cut the 13 dead Cargo edges** (declared-never-observed). Highest value: `sovereign-cli-shared → corpus-engine` (a tiny shared lib dragging the whole knowledge layer into every CLI sibling's dep chain), `sovereign-cli-llm → oicp-types`, `sovereign-cli-daemon → {commonwealth-tdd, sovereign-pipeline, sovereign-workflow}`, 5 commonwealth-internal edges. Verify each against macros/dyn-dispatch before cutting (SCIP misses those) — one `cargo check -p <crate>` per removal. | arch_report deltas | `DeclaredNeverObserved` count 13 → 0 (or annotated why kept); layer-gate green |
| 2 | **Fix `unexpected_cfgs` (6 sites)** — `corpus-engine-archaeology/src/rough_edges.rs` gates code on `feature = "treesitter"` which that crate does not declare: those blocks NEVER compile. Either declare the feature or delete the dead paths. Possible latent behavior gap, not cosmetics. | lint-gate | `unexpected_cfgs` count → 0 |
| 3 | **Audit `clippy::await_holding_lock` (23 sites)** — a std-mutex guard held across `.await` is a deadlock/starvation hazard, not style. Triage each: drop the guard before awaiting or switch to tokio::sync. | lint-gate | count → 0 or each site commented why safe |
| 4 | **Mechanical warning sweep** — dead_code (69), unused_imports (11, the `runtime/handlers` `use crate::traits::*` blocks ci.yml already names), plus the auto-fixable clippy tail (`unnecessary_sort_by` 44, `collapsible_match` 22, `useless_conversion` 15, ~40 more). Most apply via `cargo clippy --fix`. This is the gating work for the `-D warnings` flip (Stage L3): after it, default-warn count ≈ 0. | lint-gate | non-ratcheted lint counts → 0; flip CI clippy to `-D warnings -A` the ratcheted four |
| 5 | **Bootstrap api-gate snapshots** (quiet moment; heavy nightly compile): `rustup toolchain install $(cat quality/nightly-pin.txt) && cargo install cargo-public-api --locked && cargo run -p xtask -- api-gate --update-baseline`. Until this runs, the weekly api-surface lane fails soft. | api-gate | 7 snapshots committed under `quality/baselines/api/` |

## P1 — days each; structural; receipt each with a before/after arch-report

| # | Item | Evidence | Done when |
|---|---|---|---|
| 6 | **Re-index + re-census, then freeze R3's carve lines** ⟳ — commit the current work, let the daemon rebuild SCIP, re-run `sovereign code arch-report`. The `sovereign-tools → sovereign-core` 7,169-ref edge is carried mostly by `Error`/`Result` symbols that likely re-attribute to sovereign-contracts (a stable leaf — fine) on a fresh index. Do NOT carve sovereign-tools on stale attribution. | arch_report cross_edges | fresh census committed as the R3 input |
| 7 | **`sovereign-store/src/sqlite.rs` split** — 43 commits/6mo, ~3,930 lines, ~70 from its own §10-declared 4,000-line trigger; the split shape (one file per StateStore sub-trait) is already designed. Cheapest big-file win. | §10 ledger + churn | file leaves `quality/baselines/oversized.txt` via `--tighten` |
| 8 | **`sovereign-core/runtime/retrieval.rs` split** — 5,008 lines × 51 commits/6mo = the hottest churn×size in the workspace; every dev collides here. Use per-file symbol co-change + the file fan-in table to pick seams. | churn×size + arch_report files | entry leaves oversized baseline; its co-change partners drop |
| 9 | **Handlers hidden-coupling cluster** — `attached_doc.rs ⇄ simple.rs ⇄ complex_task.rs` (11-12 joint commits, r≈0.8, NO structural edge) and `executor.rs ⇄ planner.rs` (25 joint, r=0.64): logic these files keep co-editing lives in neither — extract the shared convention (likely prompt/synthesis contract) into a named module. | temporal coupling | pairs leave the hidden-coupling list on the next census |
| 10 | **commonwealth-api middleware cluster** — `approval_gate ⇄ {decision_extractor, context_injector, tool_injector, session_briefing}` (12-15 joint commits each, no structural edge): the middlewares share an implicit context contract; make it a type. | temporal coupling | same |
| 11 | **`sovereign-contracts` missing_docs burn-down (796)** — the contract crate IS the product for a 10-dev team; its public surface should read like documentation. Chunk by module (types/routing → traits → setup_config …), banked weekly by `--tighten`. | lint-gate | sovereign-contracts/missing_docs trend ↓ (target 0); doc-coverage weekly artifact |
| 12 | **corpus-engine 217-file SCC, first slice** — the whole crate is one reference cycle, anchored by `lib.rs` (file fan-in 171) and `error.rs` (134). First slice: split the monolithic error enum so modules stop all meeting in one file, and stop intra-crate imports going through the crate root. Full fix is R4 (below). | arch_report cycles + file fan-in | SCC size shrinks on next census |

## P2 — the roadmap (weeks; sequence after P1's fresh census)

| # | Item | Notes |
|---|---|---|
| 13 | **R3: carve `sovereign-tools`** — the only two-way hub (fan-in 9 / fan-out 10). Carve the `code/` (SCIP-backed) tool family first — precedent: corpus-engine-scip. Input: fresh census carrier symbols (#6). | fan_in.tsv cap drops; hub flag clears in arch_posture |
| 14 | **R4: corpus-engine decomposition** per `corpus-engine/DECOMPOSITION.md` — Step 1 (watchers; plan already drafted) → 5 (wikipedia) → 6 (types) → 7 (enrich, last). Kills most of #12 structurally. | observed fan-in 18 → ~8; CrateBoundaryFiction stays 0 across new seams |
| 15 | **R6: retire the 3 cross-family exceptions** (`commonwealth-api → sovereign-{core,tools,atos}`) — goal state is the OICP seam. Delete each `[[exception]]` in ARCH_LAYERS.toml with its edge; stale-exception failures enforce the cleanup. | exception list empty |
| 16 | **Panic-ratchet burn-down** — unwrap_used 565 + expect_used 362, crate-by-crate starting with daemon/server-facing crates (corpus-engine 201, sovereign-mesh 95+, sovereign-tools 126). | clippy_counts trend ↓ |
| 17 | **Complexity budget burn-down** — cognitive_complexity 240 + too_many_lines 128, opportunistic (only when touching the function anyway); never a dedicated sweep. | trend ↓ |
| 18 | **Month-later blocking flips** (~2026-08, after 4 clean weeks): delete `continue-on-error` on gates/deny/clippy lanes; commit `.github/rulesets/main.json`; update CONTRIBUTING. | every red on main is a genuine regression |

---

## Execution record — 2026-07-12 (P0 + P1 batch)

| Item | Outcome |
|---|---|
| P0 #1 dead edges | ✅ 10 of 13 removed (incl. `sovereign-cli-shared → corpus-engine` + its vestigial feature); internal edges 205 → 196. 3 KEPT — SCIP missed function-scoped `use`-only imports (`sovereign-cli-daemon → {commonwealth-tdd, sovereign-workflow}`, `sovereign-cli-llm → oicp-types`): treat `DeclaredNeverObserved` on single-handler crates with extra suspicion. |
| P0 #2 unexpected_cfgs | ✅ rough_edges.rs treesitter blocks were dead-on-arrival since the carve-out — deleted (−322 lines incl. 4 orphaned helpers); `FindingKind::DocDrift` kept for serde/consumer compat. Bonus: urls.rs test un-gated (cfg on a feature the crate never declared — the test silently never ran). |
| P0 #3 await_holding_lock | ✅ All 23 sites were TEST code. 6 test modules: documented `#![allow]` (intentional whole-test serialization locks — HOME/e2e/INGEST/SOVEREIGN_HOME; each `#[tokio::test]` owns its runtime → contention parks a thread, never deadlocks). 2 real fixes: block-scoped assertion guards in functional.rs + solve_http.rs. Zero production sites. |
| P0 #4 mechanical sweep | ✅ `cargo clippy --fix` workspace pass + agent deletions; `-D warnings` flip with explicit `-A` for the ratcheted lints (see ci.yml). dead_code NOT mass-deleted — several flagged items sit in active test scaffolding (harness.rs scripts); left to the ratchet. |
| P1 #7 sqlite.rs | ✅ 4,097 → 582-line parent + 14 concern modules (largest 1,098); pure move proven byte-for-byte; 72/72 tests. FOUND: `--features postgres` was ALREADY broken (13 contract-drift errors in postgres.rs: missing version/deleted_at, no DocumentAssetStore impl) — predates the split; the weekly cargo-hack lane will keep flagging it until fixed or the feature is retired. |
| P1 #8 retrieval.rs | ✅ 5,032 → 11 modules under retrieval/ (largest 1,008); full sovereign-core suite green; tracing prefix-filters still match. |
| P1 #9 hidden coupling (core) | ✅ plan_grammar.rs now owns the `{N.key}` placeholder grammar (emit+parse+prompt-sync test); synthesis_common.rs owns the handler trio's mirrored plumbing (CompletionRequest tails, ResponseProvenance telemetry, transcript EvidenceContext). FOUND: SYSTEM_OVERVIEW's `[sample:N:method]`/`[eval:name]` grammar NEVER existed in code — doc fiction corrected in SYSTEM_OVERVIEW + DEVELOPMENT.md. |
| P1 #10 middleware cluster | ✅ middleware/shared.rs: single-sourced notes/features db paths + prepend_to_system + the shared test fixture (the coupling was mostly FIXTURE LOCKSTEP — every new ChatCompletionRequest field used to touch 6 files). −289 lines; 349 tests green. |
| P1 #11 contracts docs | ✅ 798 → 0 missing_docs in sovereign-contracts (+971 researched doc lines; low-confidence items flagged in the agent report). oicp-types' 114 remain (separate crate, ratcheted). |
| P1 #6 fresh census | ⏳ Blocked on a committed tree (SCIP reindexer gates on clean tree); run `sovereign code arch-report` after landing this batch, then freeze R3 carve lines. |
| P1 #12 SCC first slice | ⏳ Deferred to the fresh census (error.rs out-edge analysis). |
| P0 #5 api snapshots | ◐ 3 of 7 banked (oicp-types, oicp-client, sovereign-contracts — 7,217 surface lines committed; api-gate green). The 4 hub crates are PARKED: rustc nightly-2026-07-01 ICEs compiling `lance-index-4.0.0` (opaque-type trait-selection panic). Un-park: bump quality/nightly-pin.txt to a nightly that builds lance-index, uncomment the crates in xtask api_gate.rs, `api-gate --update-baseline`. |
| Post-batch reconciliation | lint-gate: 60 pairs cleared + 3 lowered by the batch; final baseline 1,637 warnings across 153 pairs (was 2,555/211). Growth in sovereign-core/-inference (+17) was YOUR new code landed since the 07-11 snapshot (state_cartridge_spike, lessons.rs, core_tests/harness, cpu_compat) — accepted explicitly. fan-in caps: 3 lowered (dead-edge cuts). oversized: retrieval.rs + sqlite.rs + oicp-types lib.rs cleared; 6 entries grown by your recent commits re-accepted (notes.rs 6363, streaming.rs 3275, router.rs 2710, model_slot.rs 3712, core_tests.rs 2378, lessons.rs 1318 NEW — candidates for §10 rows). All 5 gates + api-gate green at batch close; fmt clean. |
