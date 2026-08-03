# Enrichment spike bundle — SP1–SP6 + D1 sweep + P5 probes

**CLOSED 2026-07-31 (SP1–SP6 + D1 + P5 probes).** All six spikes AND the three Wave-4
P5 probes (G8–G10, funded in T1 slack) answered; frozen roll-up with the consolidated
verdict table lives at `sovereign/docs/archive/ENRICHMENT_SPIKES_2026_07.md`. P5
headlines: descent loses to one-shot at equal budget + the RAPTOR output is a forest
(P5.1); page-score separation real, ONNX export blocked upstream (P5.2a); MaxSim
sibling-table viable, IVF-PQ recall is knob-shaped (P5.2b). sep restored byte-identical
from `backup/` per the §"Backups" procedure (2026-07-31). Memos below stay as working
copies; harnesses are committed and re-runnable.

Executable initiative for the ENRICHMENT_ROADMAP_SIZING.md §1 spike table plus the D1
dead-code/doc-truth sweep and the P5 evidence probes. Design doc (file:line-grounded,
design-reviewed): `~/.claude/plans/let-s-plan-out-our-melodic-tome.md`. Scope confirmed
by operator: "Everything" (~12–20 engineer-days).

House conventions honored: gates + fixtures frozen in this README **before any run**;
findings land in `findings/SPn_*.md` (bold verdict up top, tables, exact producing
commands, artifact paths); `data/`, `runs/`, `backup/`, `.venv/` gitignored; close-out
rolls up to `sovereign/docs/archive/` + one NoteStore `decision` note per verdict.

## Pre-registered decision gates (frozen 2026-07-30, before any run)

| # | Gate | Threshold | On failure |
|---|---|---|---|
| G1 (SP1) | GLiNER2 ONNX on the PINNED `ort =2.0.0-rc.9`, bare `ort::Session`, entities + one relation schema over 50 real chunks | throughput ≥ v1 (`gliner_small-v2.1`) measured fresh in the same harness, same chunks, same thread count | Document failure mode (rc.9 op support / needs second ORT link, with bundle cost priced); P2.1 falls back to v1-family multi-task checkpoints, L → S-M. Partial (entities yes, relations no) → adopt for entities, relations stay LLM-judged |
| G2 (SP2) | Extractive-vs-abstractive parity at EQUAL coverage: \|Arm B − Arm A′\| per bank on aggregate fact_score and source_score | Within the frozen run-to-run band (below). Band = max−min across the three dated baselines per bank: `summarize` fact ≤ **0.0125**, source ≤ **0.0000** (1.0000 in all 3 runs); `summarize_obscure` fact ≤ **0.0167**, source ≤ **0.0000** | Quantified miss → T1 kill-point armed: P1.1/P1.2 re-plan around verified-abstractive-only; faithfulness contract stands |
| G3 (SP3) | Judge cost known: min/corpus at 608 / 1,262 / 11,181-node scales × {fast 4B, primary 35B} | Not pass/fail — numbers recorded + P1.2 default sampling rate proposed + judge-now vs wait-for-verifier per corpus size | — (informational; harness validity gate: provider MUST be SplitInferenceProvider — a naive /v1/chat/completions run is invalid and must be discarded) |
| G4 (SP4) | Qwen3-family rerank ms/pair on M2 Max, `rerank_batch_check`, 100 (query,chunk) pairs, `--release` timing | < 20 ms/pair (batched number is the deliverable; sequential recorded too). Sanity pre-gate before timing counts: relevant−irrelevant separation ≥ 0.5 logits AND scores not ~1e-23 | Documented "no"; P3.3 declined or re-scoped (llama-server-external `/v1/rerank` priced as alternate) |
| G5 (SP5) | Concept graph (noun-phrase candidates → co-occurrence → communities) over 10k wikipedia chunks | < 5 min single-machine CPU AND 20 sampled communities eyeball-cohere against article titles | P2.2 ships entity-co-occurrence-only concept graph (halves) |
| G6 (SP6) | Late chunking on `qwen-embedding-0.6b` via vendored llama-cpp-4 0.4.2: token-level reads work; memory ceiling per window W ∈ {8k, 16k, 32k}; recall delta on a small golden | Not pass/fail — binding verdict + ceiling + hit@k delta recorded. Honesty rule: status-quo baseline is LAST-token-pooled chunks (GGUF `qwen3.pooling_type = 3`), compare accordingly | Gates only the P2.4 late-chunking follow-on (go/defer) |
| G7 (D1) | Dead-code + doc-truth sweep lands on main | `./scripts/sovereign-lint.sh --human --full` AND `./scripts/sovereign-test.sh --human` both exit 0 (macOS host, no toolbox); debouncer gate covered by existing knowledge-view tests | Fix before merge — D1 is the only track that edits production code |
| G8 (P5.1) | Budgeted tree-descent answerer vs one-shot top-K at EQUAL token budget on the summarize banks | Report-only: score delta + complete hop logs (JSONL) | Evidence for re-planning P5 after T2; no downstream commitment |
| G9 (P5.2a) | ColModernVBERT page-score separation on a small scanned-PDF fixture + ONNX-export feasibility on ort rc.9 | Report-only: NDCG-ish separation + rc.9 verdict | Same — evidence only |
| G10 (P5.2b) | MaxSim multi-vector Lance sibling-table prototype | Report-only: storage + query numbers vs RETRIEVAL_REDESIGN.md:261-266 sizing (~3–6 GB / 188k chunks) | Same — evidence only |

**Downstream outcome, appended 2026-08-03 (the table above stays frozen
as written).** **G1 passed and the work it licensed was still rejected.**
P2.1 (GLiNER2 adoption) was built and measured against v1 on our own
corpora, and neither half of the case survived: no throughput win at the
obsidian vault's chunk length (881.9 s vs 893.2 s over 3,175 chunks) and
worse per-mention typing (81.8% vs 96.9% Person accuracy). See
`findings/SP1_gliner2.md` corrections 3–4, `DEFAULTS_LEDGER.md`
(REJECTED), notes `dc2e4b5d` / `f42cf7ec`.

The gate itself is where the gap is, and it is worth reading before the
next spike register is frozen: G1's threshold was *"throughput ≥ v1
measured fresh in the same harness, same chunks"* — **50 sep chunks**.
It said nothing about the target corpus's chunk-length distribution and
nothing about extraction *quality*, so a model that is faster on short
chunks and worse at typing passed cleanly. A feasibility gate is not an
adoption gate; when the two are conflated, the funded tranche is the
place it gets discovered.

Interpretation notes, pre-registered:

- **G2 AMENDMENT (2026-07-30, before any arm ran — surface-validity fix).** The original
  band below was computed from the `summarize{,_obscure}/` dated baselines, which are
  RAW-INDEX mode runs — a surface that never invokes `apply_raptor_grounding` (help text:
  "Measures the pipeline chat surfaces actually run, unlike the default raw-index mode";
  CI lane 2b exists because the raw lanes are blind to pipeline effects). A raptor swap
  cannot move raw-index scores, so gating on that surface would measure noise. Amended
  gate: **arms are scored on `--prod-pipeline --isolate`** (the raptor-aware,
  deterministic production surface; existing corroboration: prod-isolated 07-16/07-17
  fact spread 0.025 summarize / 0.0167 obscure, source 1.0 stable). **Band = max−min
  across three same-day Arm-0 replicates per bank**, recorded in `runs/arm0/` BEFORE
  A′/B exist. The synth-mode dated files are unusable as a band (fact 0.825 → 0.325
  across 06-10 → 07-06 straddles the Jun-27 sep reingest — corpus drift, not run
  variance). One raw-index run per bank is kept per arm as a no-regression guard
  against the ORIGINAL band below. Arm 0 must also verify (via `--inspect`) whether
  raptor-derived chunks actually appear in the scored evidence pool — a "raptor
  contributes nothing to these banks" result is itself a datum that re-frames the gate.
- **G2 AMENDED GATE, FINAL (Arm 0 measured 2026-07-30; frozen before A′/B):**
  invocation is `eval run --bank <bank> --prod-pipeline --isolate --limit 30`
  (`--limit 30` matches the `bench all` lane default the dated baselines used;
  `eval run`'s own default of 10 truncates the pool and halves fact scores —
  invocation mismatch, not regression; runner.rs:1196 truncate is as old as the lane).
  Arm 0 datum: summarize fact **0.6250**, obscure fact **0.7833**, source 1.0 both;
  three same-day replicates were IDENTICAL (run-to-run band 0.0000 — the surface is
  deterministic), so the gate band is the observed day-over-day drift on the dated
  prod-isolated baselines: **fact ≤ 0.025 (summarize) / ≤ 0.0167 (obscure), source
  exact-equal**. Raptor-awareness verified by ablation: `SOVEREIGN_RAPTOR_GROUNDING=false`
  changes the descartes-epistemology pool and drops 1 fact (7→6) — the ORPHAN lance is
  live on this surface (freshness-gate blindness confirmed on the bench), net
  contribution today ≈ +1 fact on 1/8 summarize questions.
- **G2 original fixtures (raw-index; retained as no-regression guard):** band computed from `sovereign/bench/sep/baselines/{summarize,summarize_obscure}/{2026-06-10,2026-07-06,2026-07-16}.json`; aggregate per run = mean over questions of `source_score.ratio` and of `|fact matched| / (|matched| + |missing|)` (n=8 summarize, n=6 obscure). Frozen observed values: summarize fact {0.6500, 0.6625, 0.6500} source {1.0, 1.0, 1.0}; obscure fact {0.7667, 0.7667, 0.7500} source {1.0, 1.0, 1.0}. Since observed source variance is zero, the source gate reads: Arm B's source_score must equal Arm A′'s exactly; any drop is a miss. Arms are only comparable at identical coverage (same rebuilt tree set); Arm 0 (orphan-lance production datum) is recorded but is NOT a gate input.
- **G2 secondary corpora** (conversations-anthropic 1,262 nodes; obsidian-vault-959ee8a8f330 608): swap-in-place under the same backup, but ONLY after a sampled member-chunk-id drift check passes; they exercise conversation-bench + briefing signposts, report-only.
- **G4 prior to beat:** jina-v3 Q6_K measured ~34–40 ms/pair (RERANK_EXPERIMENT.md); jina GGUF path is flagged broken in-code (rerank_slot.rs:87-94) — YesNoLogit family only.
- **SP2 restore obligation:** sep is returned byte-identical from `backup/` after the spike (or kept only by explicit operator decision recorded in the memo).
- One model-bound track at a time; CPU tracks overlap freely. Timing runs (SP4) use `--release` — the sole exception to the debug-builds rule, because optimized latency is the measurand.

## Backups (Wave 0, before any mutation)

- `backup/sovereign.db` — `sqlite3 ~/.svrnmesh/sovereign.db ".backup <abs path>"`
- `backup/sep-raptor_summaries.lance/` + `backup/sep-raptor_summaries.meta.json` — file copies
- `backup/chaos-raptor_summaries.lance/` + meta — preserved before decontamination removal

**Restore procedure (sep):** `sovereign daemon stop` → copy `backup/sep-raptor_summaries.lance` over `~/.svrnmesh/indexes/sep/raptor_summaries.lance` (delete target dir first), same for `.meta.json` → restore `conv_raptor_nodes` sep rows state (baseline: ZERO sep rows — delete any the spike inserted: `DELETE FROM conv_raptor_nodes WHERE corpus_id='sep'`) → `sovereign daemon start`. Full-db fallback: stop daemon → replace `~/.svrnmesh/sovereign.db` with `backup/sovereign.db` → start (loses any unrelated writes after the backup timestamp — file copies above are the surgical path).

## Wave-0 environment findings (2026-07-30, pre-run)

- sep RAPTOR orphan CONFIRMED live: `conv_raptor_nodes` has zero sep rows; sep's lance sidecar claims 11,181. (Note 91025679.)
- chaos-secret-agent RE-CONTAMINATED: clean install Jun 17 08:25, raptor lance re-appeared 10:44 same day; 19 conv_raptor_nodes rows in SQL despite `[enrichment] enabled = false` recipe. Decontamination = remove lance+meta+SQL rows, verify chunks-only (note 1ab68562's validated end state).
- sep recipe is `[enrichment] enabled = true, pipeline = "philosophy_atlas"` — sep is deliberately enriched, NOT in the re-install set.
- chaos-saltgrass: not installed, nothing to clean.
