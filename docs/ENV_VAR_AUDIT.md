# Environment-variable audit — dead-codepath survey

**Date:** 2026-07-13
**Scope:** all app-specific `SOVEREIGN_*` / `LLAMA_*` env vars across the monorepo (238 distinct names, 908 `env::var` read sites in 245 files).
**Goal:** identify large dead codepaths behind env vars unlikely to be used again, as removal candidates.
**Outcome:** survey only. No code was removed — see "Why nothing was removed" below.

Standard build/system vars (`HOME`, `OUT_DIR`, `CARGO_*`, `RUST_*`, `PATH`, `USER`, etc.) were out of scope.

---

## Headline finding

**A 238-var count overstates the sprawl.** Across all five subsystem clusters, the large default-OFF blocks are overwhelmingly *active experiments* (tracked in a flags SSOT with coverage tests), *safety guards*, or *shipped features that concluded positive* — not accidental cruft. The mesh/RPC and inference clusters are essentially all-KEEP.

Genuinely dead code is small and concentrated. And three of the most promising "confirmed dead" candidates turned out to have **live in-tree consumers** that a naive removal would have silently broken (see Traps).

---

## Why nothing was removed

The first-pass plan was to remove four "confirmed net-negative" vars. On close inspection each was more entangled than the survey suggested, and the net clean win did not justify the regression risk right now:

| Candidate | Why it was held |
|---|---|
| `RERANK_DEDUP_ONLY` / `_CORPORA` / `_PICKER` | **Live bench actuator.** `bench_cmd/scaffolding_param.rs::RerankSettings::set_env` sets these vars, and `bench_cmd/promote.rs:389` calls it in the live param-loop. Removing the bootstrap reads makes `set_env` a silent no-op — the loop would build sessions with reranking always off. Production path *is* superseded by recipe `dedup_by_source`, but the bench channel still drives the vars. Clean removal requires rewiring the bench channel to recipe config first. |
| `RUN_PHASE1B` | **Bigger + deliberately retained.** Sole caller (`run_phase1b_coverage`) is the only user of 3 `Pipeline` trait methods implemented across 5 pipelines + a shared parse helper (~250–400 LOC across 6 files, not the ~63 first estimated). Its retention comment is forward-looking: "re-enable for entity-dense reference works where coverage recall matters." That is a var you *might* use again — fails the "unlikely to reuse" bar. It's a dormant-but-working feature, not dead cruft. |
| `ATOM_ENUM` (+ sub-knobs) | **Genuinely dead, but entangled surgery.** The `enumerate_typed_atom_chunks` entity body (~400 net LOC) is concluded net-negative (`attempt_enron_atom_enum_rag_enumeration`: exec_cast 6→4, "don't enable without redesign"). BUT it shares the function with the **default-ON, shipped** overview path (`enumerate_overview_claim_chunks`) and the shared `ATOM_ENUM_TOPK` / `ATOM_ENUM_SCORE` knobs. Removal also must update the pipeline flags SSOT (`retrieval_pipeline.rs`) and its coverage doc-test (`retrieval_pipeline_doc.rs`). Doable, but a careful multi-site change deferred to avoid regression risk. |
| `DOC_CHUNK_NEIGHBOURS` | The one clean, self-contained win (~8 LOC, net-negative, default-OFF). Prototyped and reverted with the rest; safe to redo anytime. |

---

## Traps — "looks dead, has a live consumer"

Record these so a future sweep doesn't repeat the investigation:

1. **`RERANK_DEDUP_*` is driven by the bench param-loop**, not just the archived production reranker. Grep `set_env` in `bench_cmd/` before touching. Removing the env read silently breaks `promote.rs`.
2. **`ATOM_ENUM_TOPK` / `ATOM_ENUM_SCORE` are shared** between the dead entity path and the active overview path. They *look* like atom-enum knobs but are load-bearing for the shipped overview feature.
3. **The rerank cross-encoder cluster (`RERANK_MODEL_PATH` etc.) is default-inert but wired into the production daemon** (`daemon/inference.rs`). The archive doc (`docs/archive/RERANK_EXPERIMENT.md`) explicitly says "keep the code around" pending a slot-commit decision. Treat as one owner decision, not per-var.
4. **`FRONTDOOR` is a backwards-compat alias for `HARNESS`** — the one legacy duplicate, but tested and documented.

---

## Genuine removal candidates (for a future, dedicated cleanup)

Ranked by dead-codepath size. None removed yet; each needs the noted follow-up.

| Var(s) | Dead LOC | Status | Follow-up before removal |
|---|---|---|---|
| `ATOM_ENUM` (+ `_RELATIONS` `_RANK` `_POOL` `_NOFILTER`) | ~400 net | Net-negative, "don't re-enable" | Excise entity branch only; keep overview path + `_TOPK`/`_SCORE`; update flags SSOT + doc-test |
| `META_BRIDGE` | ~150 | Archived experiment, still registered as a retrieval step | Product call; `bridge_boost` (retrieval.rs) + pipeline step registration |
| `GRAPH_NEIGHBOR_EXPAND` | ~139 | Wikipedia-specific, never promoted | Owner confirm — no explicit net-negative note |
| `RUN_PHASE1B` | ~250–400 | Net-negative for bench, but deliberately retained | Owner call — it's a working dormant feature, not cruft |
| `RERANK_DEDUP_ONLY`/`_CORPORA`/`_PICKER` | ~42 | Prod path superseded by recipe config | Rewire bench param-loop channel to recipe config first |
| `KQ_PER_CORPUS_CAP` + `KQ_XCORPUS_FLOOR` (pair) | ~55 | Env-only, no config wiring/setter | Confirm no external bench/replay sets them; remove the pair + helper together |
| `DOC_CLUSTER_WEIGHT` / `_POOL` | ~70 | Default-OFF opt-in experiment | No recorded conclusion — owner call |
| `DOC_CHUNK_NEIGHBOURS` | ~8 | Net-negative, default flipped OFF 2026-05-22 | Clean; safe to remove anytime |
| `KQ_RETRY_FLOOR`, `CONTENT_TEMPERATURE`, `RERANK_PROMPT_VARIANT`/`_INSTRUCT`, `RPC_ASSUME_WARMED` | ~30 total | Small debug/A-B/superseded hatches | Low-value; confirm no bench setter |

---

## Explicit KEEP — do not remove

**Load-bearing guards & invariants:** `GROUNDING_GATE`, `GV_THRESHOLD`, `GATE_EXCLUDE_RAPTOR` (anti-fabrication), `CITATION_GROUNDING`, `CITATION_BROAD`, `EXACTVAL_FIX`, `SPECIFICS_SCAN`, `ALTERNATION_GRAMMAR` (breaks tool-calling if wrong), `FORCE_TOOL_CALLS`, `MTP_DISABLE`, `MTP_QUARANTINE_DISABLE`, `FORCE_CPU_CHAT` (Gemma4+Metal), `SKIP_VRAM_CHECK`, `BLOCK_NON_LATIN` (unicode-crash denylist).

**Shipped features (default-ON) + their knobs:** all `RAPTOR_*`, `DOC_PPR`/`_BOOST`, `CONV_PPR_WEIGHT`, `ATOM_ENUM_OVERVIEW` (+ shared `_TOPK`/`_SCORE`), `MEM_TIER_ALPHA`, `KQ_EFFORT_TIER`, `HISTORY_RETRIEVAL`, `SYNTHESIS_OUTPUT_FLOOR`, `KQ_FANOUT_CONCURRENCY`, `NOTES_EMBED_WEIGHT`, `NOTES_EPHEMERAL_TTL_DAYS`.

**Recent / positive spikes (not dead):** `PREFIX_STATE`/`_MIN` (cartridge, RESOLVED faithful + 116–145× faster, 2026-07-12), `CVEC*` (J-lens, Phase-3 passed chaos red lines), `DRAFT_STREAM` (wired to desktop DraftPreview).

**Active experiments (owner sign-off, not a dead-code sweep):** `AGENTIC_KQ` (+`_THRESHOLD`, gates the 1113-LOC evidence loop), `QUERY_DECOMP`/`DECOMP_DECAY`/`TITLE_EXPAND` (enumerated in `retrieval_pipeline_flags()` SSOT with a coverage test), `MEM_PICK`/`_POOL`/`_MARGIN` (2026-07-09 shipped seam), `MEM_LLM_PICK`/`MEM_RERANK*`/`SHORT_SPECIFICS_SCAN`/`NOTE_AS_METADATA` (dormant "kept for bench" seams).

**Distributed-inference plumbing (all live):** the whole `RPC_*` set (config-wired via `bootstrap.rs::apply_shared_model_role_to_env` + env-override), all mesh kill-switches (`IROH`, `JOIN_HOST`, `DISABLE_MDNS`, `DISABLE_PEER_INFERENCE`, `DISABLE_AUTO_COLLAB`, `ADVERTISE_ADDR`, `PRIMARY_SIBLINGS`, `USE_SUPERVISOR`, `WORKER_RUNNER`), `VAST_IMAGE` (live SEP vast.ai runs).

**Infra paths & plumbing (all live):** `DATA_DIR`, `HOME`, `CORPORA_DIR`, `RECIPES_DIR`, `DB_PATH`, `MODELS_DIR`, `WORKSPACE_DIR`, `DAEMON_URL`, `BIND`, `CLI_*_BIN`, `SERVER_PATH`, `COMMAND_BRIDGE*`, `CLIENT_TOKEN`, `GIT_SHA`, OCR prod paths (`TESSERACT_BIN`, `TESSDATA_DIR`, `PADDLE_OCR_MODEL_DIR`, `PDFIUM_LIB`, `GLINER_MODEL_DIR`), `ATLAS_*`, `ENRICH_*`, `BASELINE_*`, `RSS_*_LIMIT_MB`.

---

## Methodology

Five parallel subsystem audits, each classifying its vars KEEP / REMOVE / UNCERTAIN with codepath-size estimates, cross-referencing `docs/archive/` and the sovereign memory index:

- **A** retrieval / rerank / raptor / atom-enum / doc-graph
- **B** memory / KQ / query-decomp / citation / sufficiency / synthesis
- **C** inference / embedded (MTP, CVEC, prefix-state, sampling, llama)
- **D** mesh / RPC / distributed inference
- **E** infra / daemon / CLI / desktop / OCR / atlas / enrichment / eval / bench

There is **no central env registry** — reads are scattered `std::env::var` calls. A follow-up worth considering: a typed config surface (or at least a documented registry) so this audit doesn't have to be redone by grep next time. The closest existing structure is `retrieval_pipeline_flags()` in `retrieval_pipeline.rs`, which is an SSOT + coverage-tested table for the *retrieval* flags only — a good model to extend.
