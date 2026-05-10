# Bench history — findings preserved across raw-result cleanup (2026-05-10)

This file is the durable summary of the bench campaigns whose raw JSON / log artifacts (~247 files, 19.8 MB) were deleted on 2026-05-10 to keep the repo lean. The findings below are what future work should build on; the inputs and harnesses (`run_bench.py`, `synthesize_queries.py`, `label_gt.py`, `sovereign bench atlas`, `sovereign voice eval`, `bench/sep_atlas/run_batch.sh`) are still in-tree, so every result is reproducible if anyone needs to regenerate.

The raw results lived under `sovereign/bench/{atlas_retrieval,atlas_throughput,sep_atlas,voice}/` alongside the now-archived per-campaign markdown reports. Cross-references to memory notes (e.g. `project_atlas_retrieval_decisions.md`) point at user-MEMORY entries that carry the production decisions distilled from these benches.

## atlas_retrieval

Question the campaign answered: does the v2 atlas (atoms / edges / trajectories from `sovereign enrich`) provide a real retrieval signal beyond cosine + BM25 on the same chunks? Run on `brothers_karamazov` (2,426 paragraph chunks, 94 atoms, 118 edges) because it had a complete atlas including `cross_corpus_edges.json`.

### Controls for circularity

Atom-derived ground truth creates two circular pathways into atlas-tier's numbers (query-side via atom-similar synthesis, routing-side via provenance == GT). The campaign used three independent controls:

- `--paraphrase` (Bonsai-8B rewrites templates into 2 research-style phrasings) — breaks query-side bias.
- `atlas-tier-loo` (hide the atom each query was derived from) — breaks routing-side bias.
- `golden-gt` via `label_gt.py` — LLM judge that never sees atoms; breaks both. Expensive (~16-29 s/judge call), reserved for follow-up validation.

### Headline result (the cell that mattered)

The most controlled cell, **paraphrased + LOO** (563 queries):

| variant | r@10 | MRR |
|---|---|---|
| flat-fp32 | 0.334 | 0.153 |
| flat-pq (16-byte) | 0.190 | 0.114 |
| bm25-only | 0.320 | 0.243 |
| atlas-tier (+1-hop) | **0.883** | 0.491 |
| atlas-tier-prune | 0.810 | 0.485 |

Atlas-tier sits ~50 pts r@10 above flat-fp32 in the controlled setting. **This is the load-bearing evidence behind atlas-tier as the primary production retrieval path.** See `project_atlas_retrieval_decisions.md` for the committed config (1024-fp32 raw chunks, atlas-tier primary + dense/BM25 hybrid parallel).

### Storage compression takeaways

- **fp16 is free** — bit-identical recall to fp32 (verified across template, paraphrased, LOO variants). Always prefer fp16 for chunk storage; saves 50% of vector bytes.
- **PQ at 16 bytes/vector** loses ~0.5 pts r@10 on template GT, much more on paraphrased (0.190 vs 0.334 r@10). Usable for chunks with hybrid BM25 fallback; not safe as the sole representation.
- Spend the embedding-quality budget on the **skeleton tier (atoms)**, not chunks. Chunks can ride on lexical + compressed dense.

### Embedding model A/B — qwen-embedding-0.6b vs Jina v5-small-retrieval

Direct head-to-head on the paraphrased + LOO eval. Both 1024-d, both fine-tuned from Qwen3-0.6B. Three configs: Qwen-0.6b (production), v5 no-prefix, v5 with `query: ` / `passage: ` prefixes (v5 was trained asymmetric — direct similarity probe showed prefixes lifted relevance margin +0.47 → +0.59).

Findings:
- v5 wins r@1 / MRR (+7-9 pts r@1 across every dense variant); sharper contrastive head.
- **Qwen wins r@10 on atlas-tier paths by 10-12 pts** (the production path).
- BM25 is identical across embeddings; BM25-rerank within ~3 pts.
- v5 trails because atom display text (`Alyosha | Alexei | a young man...`) is short identifier-style — OOD for v5's MS-MARCO-ish training. Qwen's symmetric training handles short-vs-long without the mismatch.

**Decision: stay on `qwen-embedding-0.6b` for v1.** Atlas-tier is the primary path and v5 trails there. Open caveat (Wikipedia-domain check) is documented in `project_embedding_model_v1.md` — v5 may close or invert the gap on factoid corpora; a 1k-chunk / 100-query Wikipedia bench would resolve it in ~20 min.

**Do not redo:** the v5 prefix probe is settled — without prefixes v5 trails by ~9 pts r@10 on flat-fp32. If you ever evaluate v5 again, prefixes are mandatory.

### Brief-quality probes (judge-scored, K=10)

LLM-judge yes/partial/no scoring on the brief input window, Bonsai-8B as judge. Slow (~0.05 calls/s). Three probes were run:

- **Full sweep (n=25 stratified)**: atlas-tier-prune-labeled best at 52% yes+partial; bm25-only and atlas-tier-loo-hop both at 44%; flat-fp32 lowest at 32%.
- **Targeted (n=61, atlas-tier-prune vs labeled)**: labeled wins 54.1% vs 44.3%. Adding atom-type labels to the brief consistently lifts yes+partial by ~10 pts. This is the basis for the committed brief format `(<atom_type>) ...` (see `project_atlas_retrieval_decisions.md`).
- **Regression probes (n=10, three label-format A/Bs)**: confirmed minimal-label and type-only deliver the same lift as descriptive labels. `(<atom_type>)` is the smallest sufficient form.

**Do not redo:** the judge-corroboration smoke from `bench-report-golden.md` (n=7) corroborated atlas-tier-prune ≈ bm25 ≈ flat-fp32 within sample noise on chunk-level GT, and showed 1-hop expansion *hurting* via candidate dilution. Per-class signal needs n≥30 to be trustworthy. LOO gives equivalent assurance at n=190 / n=563 without paying judge tax.

### Open follow-ups noted at campaign end

- Hold-out atom split (train atoms only seen by atlas-tier; test atoms only used to generate queries) — stronger version of LOO, natural next step.
- Wikipedia-domain v5 vs Qwen check — see above; ~20 min compute, low cost, not on v1 critical path.

## atlas_throughput

Cross-model speed + correctness comparison for picking `[models].primary` for long atlas-enrichment batches. The motivating workload is the SEP 1800-article ingest. `sovereign bench atlas` is a `sovereign-cli` subcommand (no Python harness) that hits the running daemon's `/v1/chat/completions` with four tasks (`phase1_short` / `phase1_medium` / `phase1_long` + `cluster_name_synth`) spanning the real input-size range. The reference run (`sep-al-farabi`, 38 min) was 79% Phase 1 / 21% short calls.

The bench captures the daemon-reported `model_id` so archives can't be silently mislabelled. Headline metric is **`phase1 decode tok/s avg`** (cluster_name hides load-tax in proportion; trust Phase 1 for batch projection). `phase1 success rate` < 100% is a hard reject.

### Models tested and verdicts

| model | Phase 1 | tok/s | est. 1800×5 | verdict |
|---|---|---|---|---|
| Qwopus3.5-27B-v3.5-Q6_K | 3/3 | ~15 | ~64 d | production-ready |
| Qwopus-GLM-18B-Healed-Q6_K | 2/3 (long fails) | 22-38 | ~14 d if you fix long | conditional |
| Darwin-9B-Opus.Q8_0 | 0/1 | 42.7 | n/a | rejected (whitespace corruption in 14k strict JSON) |
| gemma-4-31B-it-Q5_K_M | 3/3 | 25-31 | ~20 d | viable; most reliable phase1 yet |
| Bonsai-8B-Q1_0 | 1/1 structural | 399 | 1.2 d | rejected for extraction (shape-conformant filler, ~9 atoms vs 32) |
| FINAL-Bench_Darwin-35B-A3B-Q8_0 (post-fix 2026-05-04) | 3/3 | 24-26 | ~7.0 d | rich extraction (23/15/28 atoms) |
| FINAL-Bench_Darwin-36B-Opus-Q6_K (dense) | 3/3 | 23-27 | ~4.7 d | balanced (20/15/11 atoms); faster than 35B-A3B |
| Nemotron-Cascade-2-30B-A3B.Q6_K (post-fix) | 3/3 | 12-31 | ~2.8 d | A3B early-EOS root-caused + fixed; production-viable |

Cross-reference `project_sep_atlas_gemma_ab.md` for the SEP-pinned choice: Gemma-31B won Phase 1 reliability + Phase 4 dedup (−58% silent drops) + Phase 7 (3 configurations vs 2). Pin SEP campaign to `gemma-4-31B-it-Q5_K_M`.

### Major bugs uncovered and fixed during the campaign

These are load-bearing fixes triggered by the bench; the bench is the canary, not the test surface:

1. **Chat template fallback for Jinja2-macro models (2026-04-26).** Daemon's `format_prompt` silently fell back to `{system}\n\n{user}` concat when `apply_chat_template` rejected a model's gguf-embedded template (Gemma 3/4 use macro-based templates). Symptom: models role-played multi-turn output, never emitted EOS, decoded to `max_tokens`. Fix: retry via `apply_chat_template_oaicompat` with `use_jinja: true` (llama.cpp minja path) before falling through to a loud-warned plain-text concat. Any pre-fix bench of a macro-template model is bogus.

2. **A3B-MoE early-EOS root cause (2026-05-04).** Was thought to be a family-wide hardware/model limit. Real cause: in `sovereign-inference/src/json_constraint.rs`, `step_object`'s `AwaitCommaOrClose → b','` arm accepted `,` unconditionally; the recursive validator correctly rejected `,` after the last typed property when `additionalProperties: false`. The disagreement triggered the diagnostic latch → forced EOS-only mode → truncated JSON. A3B models tokenize `],\n` into single multi-byte tokens that trip the state-machine boundary; dense models tokenize into smaller tokens that don't. Fix: mirror the validator's `more_pairs_possible` check inside `step_object`. Locked in by three regression tests (`comma_after_last_typed_property_is_invalid_in_both_paths` + variants). Nemotron Phase 1 jumped 66.7% → 100% with no other change. Darwin-A3B atoms went 12 → 28 on long.

3. **Daemon `completion_tokens` bookkeeping bug (2026-04-26).** Daemon reports `prompt_tokens=0` and mislabels `completion_tokens` (carries total = prompt + generated). Bench `decode_tokens_per_sec` is inflated by ~50-75%. Comparative ranking still valid (bug applies equally); wall-clock metrics (`phase1_seconds_per_chapter`, `est_hours_1800_articles_5_chapters`) are accurate. The pre-fix Darwin-35B-A3B "170 tok/s" was ~7× inflated. Real steady-state: ~25 tok/s. Track-and-fix in chat-completions response assembly.

4. **Bench harness `model: "primary"` sentinel broke after daemon mesh-router change (2026-05-04).** Returns 503 ("no node in this mesh advertises model 'primary'"). Fix in `bench_cmd/atlas.rs:651` switches to empty string. Pre-fix runs look like 0/3 wipeouts — that's the harness, not the model.

### Caveats baked into the bench

- Uses `temperature: 0.0` (greedy) for reproducibility; production runs at 0.2. Decode speed essentially identical; correctness may be slightly more conservative at greedy.
- Slot reload tax hidden by warmup; use `--no-warmup` for cold-start measurement.
- Per-task results are single-shot. Re-run any task landing far from a prior median for that model.
- Failed tasks' `response_head` carries the full model output (not 500-char preview) for post-mortem.

## sep_atlas

Driver + logs for `philosophy_atlas` enrichment over the Stanford Encyclopedia of Philosophy in parallel across two mesh peers (LittleMac + RuggedFox). Per-article granularity — each `sep-<slug>` is a self-contained sub-corpus.

Architectural constraint baked in:
- `mesh_sharing=false` on the SEP recipe (Stanford license) → no distributed-shard ingest of the base index.
- Atlas pipeline is per-corpus → same corpus on two nodes collides on atom IDs.
- Per-article enrichment naturally parallelizes via hash(slug) mod 2 disjoint cover.

Cross-reference:
- `project_sep_atlas_phase0.md` — Phase 0 validated end-to-end on `sep-compatibilism`; default `max_output_tokens` bumped 4096 → 16384; per-article parallel ready.
- `project_sep_atlas_gemma_ab.md` — Phase 1 Gemma vs Qwopus A/B → pin SEP campaign to `gemma-4-31B-it-Q5_K_M` (reliability + dedup + configurations).
- `project_phase4_entity_synthesis.md` — Phase 4 entity synthesis at salience 0.1 reduced silent drops 14 → 0 on `sep-compatibilism`; captured Fara/Vihvelin/Jones/Smith/Black, +14 Involves edges.

Operational tooling captured in the README: `--peer-index 0|1`, `--dry-run`, idempotence via `atoms.json` existence check, fedora (RuggedFox) ready-state runbook (node_id stability through toolbx restart is load-bearing — see `project_toolbx_node_id_volatility.md`).

## voice

Tier-B harness for the relational voice contract. Drives scenarios under `bench/voice/<id>.toml` through the daemon-backed `Runtime`, scores deterministic checks + LLM-as-judge axes. CLI: `sovereign voice eval` (lives in `crates/sovereign-cli/src/voice_eval/`).

Contract surfaces (in `crates/sovereign-core/src/runtime.rs`):
- `RELATIONAL_BASE_SYSTEM_PROMPT` (full, chat-default)
- `RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT` (compact, situated-handler default for Relational skills)

### Base + hard mode campaign (2026-05-01 → 2026-05-02)

Baseline → iter19 → iter1-4 hard, three architectural unlocks + prompt iteration. **Production state at campaign end: 9B small fast slot passes 12/12 base and 8/8 hard effective. 4B XS pass rate ~75% (good fallback).** The work isn't 9B-overfit.

The eight changes that landed in priority order of impact:

1. **FTS hyphen-as-NOT bug fix (`sqlite.rs::sanitize_fts5_query`).** The single biggest find. `sanitize_fts5_query` preserved `-` in tokens; FTS5 parses `6-month` as `6 NOT month`, silently zeroing OR-clause recall. Voice scenario 07 was the canonical repro. Fix: drop `c != '-'` from the splitter.
2. **Compact relational contract.** Full base prompt (~1100 tokens) + memories + tensions pushed the 9B into open-ended planning that ran past 9.8 KB without closing `</think>`. Compact form keeps lead posture, 5 named moves, anti-patterns, `RIGHT_EDGE` cue, closing distillation. Planning converges in 600-1200 tokens.
3. **Memory wiring on situated handlers.** `handle_expressive_query` + `handle_simple` were building ad-hoc prompts and ignoring `context.memories` despite `build_context` loading them upstream. Both now route through `build_compact_relational_system_message(context)` when active skill is Relational.
4. **Multi-shot Pass A: structured contradiction detection.** Fast-slot call returns `{contradiction, prior_evidence, current_claim}`. Soft-fails to `None`. Strictly additive.
5. **Conditional dialectical scaffolding (the iter19 unlock).** Always-on dialectic (iter18 trial) was net-neutral on pass count; gating on `Pass A.is_some()` was the unlock. **5/12 → 8/12 on both models.**
6. **`enable_thinking` end-to-end.** Threaded through `chat_template_kwargs`. Empirical: `enable_thinking: false` is the setting that triggers reliable auto-`</think>` close on Qwen3.5-9B-vOP. With `true`, the chat template prepends `<think>` but the fine-tune fails to close.
7. **`strip_thinking_response` helper** in `title.rs`, handles three observed shapes (standard, no-opener/has-closer, no-tags-at-all). Applied at runtime response-assembly + eval-side `drive_turn` so prod chat and eval surface identical text.
8. **Scenario calibrations (register-level only).** Surface variants the contract names ("you've mentioned", "the record", "your messages") — never scenario-pinned seed content.

Hard-mode iter1-4 added three more architectural lifts:
- **H02 routing miss** → `looks_like_memory_reference` heuristic + `force_expressive_memref` pre-check in `router.rs` for "Remember when …" / "You mentioned X" framings.
- **H05 FTS retrieval gap** → `memory::recall_relevant_memories_embed` cosine recall on Relational skills, FTS fallback on error.
- **Brevity discipline (iter2-4)** → K=3 memory render cap, universal brevity anchor with explicit "cut the wisdom-voice paragraph" wording, tightened dialectic on Pass A path, gated edge-of-competence clause on medical/legal/financial keyword match.

Known remaining issues:
- **9B base bench has ±2-4 scenario run-to-run variance.** Single-run numbers should be treated as suggestive; 3-run median is the proper signal.
- **04/06 question density** habitually pairs anchor + refinement; contract says "usually one real question" not "exactly one" — cap lifted to 2.
- **right_disagreement axis variance** — contradiction scenarios pass deterministically but judge gives `dis=0` on textbook responses. Judge-prompt audit pending.

### Inner-work tuning campaign (2026-05-04)

Six-iteration prompt-tuning pass on `RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT` filtered to 11 inner-work scenarios via `--skill inner-work`. Triggered by a desktop incident where a heartfelt journal entry rendered as third-person retrieval reasoning with code-corpus chunks leaking into the witness reply (the canonical "corpus pollution" failure).

Architectural fixes pre-tuning (each its own gate):
- Drop knowledge tool from inner-work skill — witness has no business retrieving from external corpora.
- Skill exclusivity on `InnerWorkSurface.svelte` mount — snapshot/deactivate non-witness skills, restore on exit.
- Force witness path when register=Relational (`runtime::override_intent_for_relational_register`) — router was misclassifying paragraph-shape personal prose as MetalingualQuery at confidence 1.00.
- Streaming witness path (`handle_expressive_query_stream` + `title::strip_thinking_stream`) — was `NotImplemented`, falling back to non-streaming.
- `code_identifier_check` regression gate against corpus pollution recurrence.
- Three new bench scenarios (`13-journal-heartfelt.toml`, `H09-journal-think-leak.toml`, `H10-journal-corpus-pollution.toml`) — paragraph-shape entries are the actual failure surface.
- `--skill <id>` filter on voice eval — earlier bench was implicitly mixed.
- `--diff <baseline.json>` axis-level diff — per-scenario flips have run-to-run variance; axis means pool across scenarios.

Five generalizable findings from the six iterations:

1. **Per-directive brakes recover calibration.** Pure substance push (iter2: "quote prior detail; don't paraphrase") moves specificity +0.55 but tanks calibration −0.82 by inducing fabricated continuity on thin records. Adding conditional brakes per directive ("if you can't quote it, say so plainly") recovers half the calibration drop while keeping most specificity. The brakes are the load-bearing mechanism, not the prose framing.
2. **A single cross-cutting mantra is insufficient — abstraction failure.** Iter4 mantra ("Specific from the record, silent on the gap. Don't bridge with wisdom.") produced the **worst** avoid-list penalty in the campaign (2.91, +0.55 from baseline) — the mantra is itself wisdom-voice; the model copies the register of the prompt regardless of semantic content. Mantras teach by example, not by content.
3. **Form-consistency moves a different axis bundle.** Iter5 rewrote everything as `When X, do Y. When not-X, do other-Y.` Pure conditional improved avoid-list −0.45 (biggest avoid-list win) but cost questions −0.55 and didn't recover calibration. Mixed form (iter3: declaratives for engagement, conditionals for brakes) held both registers.
4. **Conflicting norms can reinforce, not contradict.** "Simplify floor" iteration removed a redundant length norm; silence regressed −0.27. The two norms weren't conflicting — the model was averaging between them, and that average was the discipline. Redundancy that reinforces by averaging is not the same as redundancy that produces contradiction.
5. **No single architecture wins all axes.** Pareto frontier: iter3 (pass count + balance), iter5 (avoid wisdom-voice), iter4 (specificity peak but pass count crashed), baseline (calibration peak), iter1 (silence peak), iter2 (question density peak). For witness skills, pass count + balance is the objective → iter3 is production.

**Production state: iter3 (axis-aligned directives + per-directive conditional brakes, mixed declarative/conditional form).** Pass 9/11. Documented in the campaign comment block in `RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT`.

**Empirical-certainty caveats noted at campaign end:** every claim above rests on a single 11-scenario run per architecture (axis-noise floor ~±0.18). Small axis movements (silence ±0.18, edge ±0.09, honesty ±0.09) are at-or-below noise. Proper next move: 3 runs of iter3 + 3 runs of iter5 for variance floor + form-hypothesis replication, ~1.5 hours of compute. Hard mode and 9B fast slot weren't re-run on this campaign — iter3 generalization to 9B is unverified. Judge model was same as chat model (Darwin-35B both); future runs should pin judge to a separate slot.

### Team-pipeline architecture: experimental rejection (2026-05-03)

**Verdict: REJECT.** The "Situated Team → Presenter" five-stage chat pipeline (Curator/Drafter/Presenter on Fast+Primary slots) was implemented end-to-end through Phases 1-4 and tuned across 10 Presenter iterations. Full A/B against legacy single-pass showed net regression on every named success criterion:

- Base voice 9/12 → 4/12 (**−5**, plan's pre-merge rule was Δ ≤ −1, so 5× the threshold).
- Hard voice 4/8 → 3/8.
- Latency base p95 52s → 184s (+253%); synthesis 38s → 71-155s (2-4×).

The kill-switch (`SOVEREIGN_TEAM_PIPELINE`, default-off) is the gate. Underlying modules kept as research scaffolding. See `project_team_pipeline_rejected.md` in memory for the production-state summary.

**The decisive datapoint:** the original motivating failure ("free will vs determinism" tangling) is **no longer reproducible on legacy**. Legacy handles it cleanly in 38s with structured 3920-char exposition. Whatever broke when the plan was written got fixed in the same window (better Drafter prompts, sane `max_tokens` defaults, atlas/SEP retrieval tuning). The team pipeline was solving a problem that no longer existed while regressing the regression bar.

The one place the team pipeline won: **length on hard scenarios (+3)** via Curator per-section budgets. If anyone wants that, extract `pipeline/curator.rs` budget logic into a standalone helper rather than reviving the full pipeline.

Five cross-iteration prompt-engineering lessons worth keeping even though the architecture didn't:

1. **Listing avoid-list strings in a rewrite prompt** causes small open-weight models to copy them verbatim into the output (in-context examples leak).
2. **Numbered procedural steps in a prompt** cause the model to emit numbered analysis as visible output ("Let me analyze: 1. ... 2. ...").
3. **The witness contract works as a generation constraint** (legacy single-pass) but **fails as a rewrite constraint** (Presenter on Drafter's draft).
4. **Mechanical artifact stripping must live in code**, not prompt — listing strip targets in the prompt teaches the model to narrate the cleanup task.
5. **Composing two LLM passes on the same Primary model** doubles latency without doubling quality — the Presenter rewrite loses anchors more than it adds value.

What stays behind the kill-switch: `pipeline/curator.rs`, `pipeline/stages.rs`, `pipeline/presenter.rs` (the `strip_presenter_artifacts` post-processor is genuinely useful as a standalone helper), `pipeline/judge.rs`, `NarrationPhase` stage frames. **Do not flip the default to on.** If deleting, preserve `strip_presenter_artifacts` and consider extracting Curator section-budget logic first.
