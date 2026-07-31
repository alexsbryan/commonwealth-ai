# Enrichment de-risking spikes — SP1–SP6 + D1 (2026-07-30 → 2026-07-31)

**Status: CLOSED 2026-07-31.** All six spikes from
`corpus-engine/ENRICHMENT_ROADMAP_SIZING.md` §1 answered against the
pre-registered gate table (frozen in
`research/enrichment-spikes/README.md` before any run), plus the D1
dead-code/doc-truth sweep (commit `a564c93f`). Full workspace gates
green at close: lint `--full` 0 errors, tests 8,827 pass / 0 fail.
Total spend ≈ 5 engineer-days against the §1 estimate of 5–8.

Working memos, harnesses, and run artifacts live in
`research/enrichment-spikes/` (memos committed; `data/`, `runs/`,
`backup/` gitignored per convention). This document is the frozen
roll-up: verdict table, workstream consequences, then the six memos
verbatim (headings demoted one level). Design doc:
`~/.claude/plans/let-s-plan-out-our-melodic-tome.md` (operator scope:
"Everything").

---

## Consolidated verdict table

| Spike | Question | Answer | Gated workstream → consequence |
|---|---|---|---|
| SP1 | GLiNER2 in the Rust/ONNX stack on pinned ort rc.9? | **YES, 2.8× faster than v1** (7.04 vs 2.45 chunks/s, same harness/fixture); entities + typed slots work; tuple-linked relations PARTIAL (slots, not pairs); no second ORT link needed | P2.1 fundable at `L (8-15d)`, confidence Low→Med (runtime risk retired; relation pairing is post-hoc design or stays LLM-judged). gline-rs not needed for the v2 path |
| SP2 | Extractive-vs-abstractive retrieval parity on our banks? | **PARITY, zero delta** — \|B−A′\| = 0.0000 fact+source on both sep banks at equal coverage (band ≤0.025/≤0.0167 unused) | **T1 kill-point does NOT fire.** P1.1 `M (3-5d) · High` — extractive-first is the default; abstractive is an upgrade behind P1.2's verification, not a prerequisite. Cost datum: abstractive retrofit 1,539 s of 35B for 14 articles vs ~7 min embed-only extractive |
| SP3 | What does judge-scoring one corpus cost? | Fast 4B 7.2 s/node (73 min / 608-node corpus, ~22 h sep-scale); **primary 35B only 1.3× cost** (prefill-bound + MoE) and a decisively better judge (max_support p50 0.99 vs 0.68, 2× claims/node) | P1.2 defaults set: **primary-tier judging; judge-now ≤ ~1.5k nodes; 10-15% stratified sampling above.** Stream B verifier seeds landed (`sovereign/bench/faithfulness/`, 959 + 197 rows) |
| SP4 | Qwen3-family rerank < 20 ms/pair on the merged slot infra? | **Bar MISSED at 22.7 ms/pair** batched (passage-length), but protocol works with zero new code; official Qwen3-Reranker GGUF adopted, harrier retired; title-mode rerank is free (2.6 ms/pair) | P3.3 re-scoped to **A/B-only** with an explicit ~470 ms/query budget (top-20 → top-5); llama-server-external route not worth pricing; title-level prerank stage newly viable |
| SP5 | NP extraction + Leiden in Rust — adopt or write? | **PASS with 57× headroom** — 5.2 s wall for 10k chunks (gate < 300 s), 17/20 communities cohere | P2.2 `L (10-15d)`, confidence Med→High. **Adopt `leiden-rs`** (vendor/pin-audit caveat), **write** the ~250-line NP/co-occurrence layer; entity-co-occurrence-only fallback dead |
| SP6 | Late chunking on the 0.6B embedder — memory + recall? | Binding **works** on vendored llama-cpp-4 0.4.2 (0.2.x null-buffer gone); peak RSS 7.1/12.8/24.4 GB at W=8k/16k/32k; recall hit@5/@10 = 1.000 all arms (article-level golden saturates), MRR 0.953→1.000 at 1.4–2.9× embed cost | **P2.4 late-chunking follow-on DEFERRED** — zero measurable gain at k≥5 doesn't pay for memory + embed-time + offsets plumbing. Re-open: chunk-granularity golden, or the cheaper contextual embed-text sibling A/Bs positive with headroom. P2.4's non-late scope stands `M (3-5d) · Med` |
| D1 | Dead-code + doc-truth sweep | Landed on main (`a564c93f`): ConvTieredProvider deleted, debouncer v1 pass gated for atlas views, FieldModelStats stub deleted, ~18 doc-lies fixed, GLiNER comments corrected against SP1 measurements | Ratchet: dead code deleted, docs truth-restored, store/knob counts unchanged — T1's "demolition permit" bought |

## What this decides — next workstreams

The spike bundle existed to retire the lowest-confidence sizes before
money is committed (sizing doc §1). Outcome, in tranche terms (§4):

1. **Tranche 1 "Trust" proceeds with its kill-point disarmed.** SP2's
   parity result means P1.1/P1.2 keep their planned shape. The
   remaining T1 scope is **P0 core (P0.1–0.4) + P1 + P2.5** — P0
   first, since measurability gates everything behind it. P1.1 enters
   at High confidence with extractive-first as the default; P1.2
   enters with its sampling policy and judge-tier decided (SP3)
   instead of guessed.
2. **Tranche 2's two spike-gated P2 items both survived.** §4's "Med
   (SP1/SP5 gate 2 items)" caveat on P2 is resolved: P2.1 runtime
   risk retired (relations partial — pairing is a design task, not a
   feasibility risk), P2.2 at High with the adopt/write split
   decided. The entity-co-occurrence-only fallback and the
   second-ORT-link fallback are both dead — planned work the spikes
   deleted.
3. **Deferred/cancelled, with prices attached.** P2.4's late-chunking
   variant deferred (SP6, re-open triggers recorded); P3.3 is
   A/B-only at a known latency budget (SP4) — its "build a Qwen3
   protocol branch" line item stays cancelled; jina/harrier retired
   as working defaults.
4. **Wave 4 (P5.1/P5.2a/P5.2b probes, gates G8–G10) remains
   optional** — report-only evidence for re-planning full P5 after
   T2, runnable in T1/T3 slack (§4 Tranche 4). Nothing downstream
   commits on them.

## Cross-cutting operational findings (surfaced by the spikes, kept as notes)

- **Metal-OOM wedge (SP3):** a fast-slot GPU OOM while the 35B is
  resident wedges llama.cpp's backend permanently — every subsequent
  decode 503s until daemon restart. Batch judge runs must pin
  single-slot residency or treat 503-bursts as a restart signal.
- **Chunk title prefix (SP6):** production chunks carry a
  `"{slug}\n\n"` prefix absent from source docs — any
  chunk-text-to-source alignment needs a body fallback. Chunk offsets
  exist nowhere in the pipeline (`TextChunk{content,index}`).
- **Freshness-gate blind spots (SP2 + Wave 0):** `source_version`
  derives from row timestamps, so a same-timestamp content swap is
  invisible to version-gated caches; and the sep RAPTOR lance was
  live-while-orphaned (zero SQL rows, 11,181-row sidecar) —
  contributing ≈ +1 fact on 1/8 summarize questions.
- **Daemon stop/start race (SP2):** a late SIGTERM can land on the
  new daemon; it exits 0 and launchd doesn't respawn. Bound health
  waits and check process liveness, not just the port.
- **Judge harness validity (SP3):** the `x_forced_choice` envelope
  only reaches the daemon via `SplitInferenceProvider`
  (`response_format: json_schema`) — a naive /v1/chat/completions
  judge run looks plausible and is invalid. P0.3 should carry this.

---

# The six memos, verbatim


---

## SP1 — GLiNER2 in the Rust/ONNX stack (bare ort, pinned rc.9)

**Verdict: YES — the exit criterion is met with room. The pre-exported GLiNER2 monolithic
ONNX graph loads and runs on the PINNED `ort =2.0.0-rc.9`, driven bare (no gline-rs), and
is 2.8× FASTER than v1 in the same harness on the same 50 real sep chunks. The
relation-style field-group schema also fills typed slots through the same export —
with one structural caveat (slots, not linked tuples). No second ORT link needed.
P2.1 confidence: Low → High.**

Measured 2026-07-30, M2 Max, release build. Harness:
`sovereign-gliner/examples/gliner2_probe.rs` (committed) — both stacks run in ONE binary
over the same fixture, so the throughput comparison is apples-to-apples. Machine was
concurrently running the SP2 Arm A′ enrich (daemon/GPU busy); both passes shared that
load, so absolute numbers are conservative but the RATIO is fair.

### Method (exact commands)

```
cargo build --release -p sovereign-gliner --example gliner2_probe
/usr/bin/time -l ./target/release/examples/gliner2_probe \
  ~/.cache/huggingface/hub/models--lion-ai--gliner2-base-v1-onnx/snapshots/5551729ccc76b30395bc9600f2348ec52a87cead \
  research/enrichment-spikes/data/chunks_50.jsonl
```

Model: `lion-ai/gliner2-base-v1-onnx` (pre-exported monolithic encoder+span-head, 795MB,
DeBERTa-v3-base backbone). Input contract: `input_ids` / `attention_mask` /
`text_positions` / `schema_positions` / `span_idx` → `span_scores (1, fields, words, 8)`;
schema `( [P] task ( [E] field … ) ) [SEP_TEXT] words`, pre-tokenized encoding.
Fixture: 50 seeded-random sep chunks (`scripts/dump_chunks.py --seed 7`, p50 810 chars,
8,485 words total). v1 = installed `gliner_small-v2.1` via the production
`GlinerExtractor` (gline-rs stack), `extract_batch`, threshold 0.6 (its default).
GLiNER2 threshold 0.5 (export README default).

### rc.9 compatibility (the actual spike question)

Zero issues. Port deltas from the artifact's rc.12 example were purely mechanical:
`ort::inputs![]` is fallible in rc.9 (`?`), `try_extract_tensor` returns an ndarray view
(index `view[[0,fi,start,w]]`) rather than a `(shape, slice)` tuple. Session build,
tensor construction, and the run call are otherwise identical to the in-house PaddleOCR
bare-ort pattern (`local_corpus/ocr/paddle/detect.rs`). Load time 716ms.

### Numbers

| Metric | v1 (gliner_small-v2.1) | GLiNER2 base (bare rc.9) |
|---|---|---|
| total wall, 50 chunks | 20.4 s | **7.1 s** |
| chunks/s | 2.45 | **7.04** (2.8×) |
| words/s | 416 | **1,195** |
| mentions/chunk | 3.2 | 8.5 (threshold 0.5, no span-NMS) |
| model on disk | 591 MB | 795 MB |
| process max RSS | 1.63 GB (solo, gliner_smoke) | ~6.7 GB incremental (8.3 GB combined run minus v1 solo) |

Entity quality eyeball (same chunks): v1 and g2 agree on the clear people
(Dewey, Bealer, Jackson, Peacocke, Boghossian); g2 adds correct mentions v1 missed
(Alain Locke, Barnes, Paul Boghossian) and denser Work/date coverage. g2 artifacts:
duplicate overlapping spans (no NMS in the probe — production needs the standard
max-score-span dedup) and it tags the leading article slug baked into sep chunk text
(fixture artifact, not a model fault).

### Relation trial

Schema `( [P] authorship ( [E] author [E] work title ) )` over the same 50 chunks:
227 slot fills in 7.3s, and the fills are genuinely slot-typed — e.g. author "Aquinas" +
work title "ST II-II q. 64 a. 7"; work titles "Z.1, 1028a20–31" / "Catechism of the
Catholic Church"; authors Aristotle/Sennett/Horty. **Caveat:** the export's `span_scores`
head yields typed spans per field, NOT linked (author, work) tuples — pairing would be a
post-hoc step (proximity/syntax heuristic) or need the full GLiNER2 structured head,
which this export does not expose. So: entities YES, typed slots YES, tuple-linked
relations PARTIAL.

### Consequences for P2.1 (GLiNER2 upgrade, sizing doc §2)

- Confidence Low → **High**; size stays M but the "second onnxruntime link" fallback and
  its bundle-cost pricing are DEAD — rc.9 runs the graph as-is.
- The gline-rs dependency is not needed for GLiNER2: the bare-ort path is ~200 lines
  (probe) and follows the PaddleOCR precedent. `SemplificaAI/gliner2-rs` was not needed
  and was not evaluated.
- RSS: budget ~6-7 GB resident for base under default ort arena settings on long doc
  chunks; session/arena tuning is the named follow-up before production residency.
- Tuple-linked relation extraction should be scoped as post-hoc pairing over slot fills
  (or stay LLM-judged), per the partial result above.
- D1 rides resolved: gliner_ner.rs:19 size comment corrected (591MB measured, was
  "~150MB"); the ~10 (conv) vs ~24 mentions/chunk comments are corpus-specific claims —
  measured 3.2 (v1, threshold 0.6) on SEP doc chunks, left as-is with this datum recorded.

### Artifacts

- `sovereign/crates/sovereign-gliner/examples/gliner2_probe.rs` (committed; dev-deps
  `ort =2.0.0-rc.9` + `ndarray 0.16` + `tokenizers 0.21` added to sovereign-gliner —
  versions unify with the existing lockfile, no second ORT).
- Fixture `data/chunks_50.jsonl` (gitignored, regenerate with seed 7).
- Model cached at HF snapshot `5551729ccc76b30395bc9600f2348ec52a87cead`.

---

## SP2 — Extractive-vs-abstractive RAPTOR parity (G2)

**VERDICT: PARITY — GATE PASSES WITH ZERO DELTA. |Arm B − Arm A′| = 0.0000 on
aggregate fact_score and source_score for both banks, against a band of
fact ≤ 0.025 (summarize) / ≤ 0.0167 (obscure), source exact-equal. The T1
kill-point does NOT fire; P1.1's extractive SummaryMode is viable at equal
coverage on retrieval-quality grounds alone.**

Answered 2026-07-31. All artifacts under `research/enrichment-spikes/runs/`
(`arm0/`, `armA/`, `armB/`, `armAprime-enrich.log`, `armB-compute.log`).

### Results

Prod-pipeline surface (`--prod-pipeline --isolate --limit 30`), aggregate per
run = mean over questions of |fact matched|/(matched+missing) and
source_score.ratio; scorer `scripts/score_runs.py`. All three replicates
per arm per bank were IDENTICAL (the surface is deterministic; same as Arm 0).

| Bank | Arm 0 (orphan, 11,181 nodes) | Arm A′ (abstractive, 271 nodes) | Arm B (extractive, 271 nodes) | \|B−A′\| | Band | Gate |
|---|---|---|---|---|---|---|
| summarize (n=8) | fact 0.6250 · src 1.0 | fact 0.6125 · src 1.0 | fact 0.6125 · src 1.0 | 0.0000 · 0.0000 | ≤0.025 · exact | **PASS** |
| summarize_obscure (n=6) | fact 0.7833 · src 1.0 | fact 0.7833 · src 1.0 | fact 0.7833 · src 1.0 | 0.0000 · 0.0000 | ≤0.0167 · exact | **PASS** |

Raw-index no-regression guards (one per arm per bank, `--limit 30`, no
`--prod-pipeline`): A′ and B both scored summarize 0.6625 / obscure 0.7500,
source 1.0 — identical to each other and within the original dated band
(README G2 original fixtures). As pre-registered, a raptor swap cannot move
raw-index scores; the guard confirms the rebuilds broke nothing.

### Validity checks (why zero delta is a real result, not a stale cache)

- The daemon serving the Arm B benches booted AFTER the swap (see incident
  note below), so no in-memory pre-swap state could survive. SQL rows and the
  rebuilt lance were verified extractive on disk before the runs
  (`sqlite3 … substr(summary,…)`; meta row_count 271).
- **The swap demonstrably changed retrieval:** per-question retrieved pools
  differ between A′ and B on 1/8 summarize + 1/6 obscure questions (r1 vs
  r1), i.e. Arm B's re-embedded extractive summaries shifted node selection —
  yet all 14/14 matched-fact sets are identical. The parity is in outcomes,
  not in frozen inputs.
- Mechanism: RAPTOR grounding's contribution to these banks flows dominantly
  through `evidence_chunk_ids` (identical across arms by construction — only
  `summary` + `summary_embedding` were swapped). Summary text affects node
  *selection*; the scored evidence is chunk-level. At equal coverage the
  selection differences were too small to move any fact.

### What Arm B was

Per node (plan step 4): member texts via `direct_member_chunk_ids` (level 0)
/ `evidence_chunk_ids` (levels 1-2, where direct is NULL) from
`chunks.lance`; sentence-split (≥40 chars); sentences embedded
(`qwen-embedding-0.6b` via daemon `/v1/embeddings`); ranked by cosine to the
node's untouched `centroid_embedding`; top sentences to the node's own
abstractive summary length (avg 420 chars → extractive avg 520, stop-at-≥
overshoot); source order restored; re-embedded → `summary_embedding`.
Compute: `scripts/armb_extractive.py` → `runs/armB/armb_nodes.clean.jsonl`
(271 nodes). Write path: `sovereign-store/examples/armb_write_nodes.rs` via
the public `SqliteStateStore::save_conv_raptor_nodes` (atomic per-conv
delete+insert, correct f32-BLOB encoding) — no raw rusqlite. Then
`sovereign enrich raptor-index sep` (destructive rebuild from SQL rows).

### Coverage note (Arm A′ vs Arm 0)

A′'s scoped 14-article tree (271 nodes) scored −0.0125 on summarize vs the
11,181-node orphan (0.6125 vs 0.6250), identical on obscure. Informational
only (Arm 0 is not a gate input; coverage differs). Consistent with Wave 0's
ablation finding: the orphan lance contributes ≈ +1 fact on 1/8 summarize
questions.

### Exact commands

```
## Arm A′ build (predecessor session):
sovereign enrich raptor sep --titles-file data/sp2_bank_articles.txt --group-by-article --force
## (raptor-index ran automatically at the end of the retrofit; log runs/armAprime-enrich.log)

## Benches, per arm (runs/arm{A,B}/run-benches.sh):
sovereign eval run --bank sovereign/bench/sep/<bank>.toml --prod-pipeline --isolate \
  --limit 30 --format json --output runs/<arm>/<bank>-r{1..3}.json
sovereign eval run --bank sovereign/bench/sep/<bank>.toml --limit 30 --format json \
  --output runs/<arm>/<bank>-rawindex.json   # raw-index guard

## Arm B swap:
.venv/bin/python -u scripts/armb_extractive.py --db ~/.svrnmesh/sovereign.db \
  --chunks ~/.svrnmesh/indexes/sep/chunks.lance --corpus sep --out runs/armB/armb_nodes.jsonl
cargo build -p sovereign-store --example armb_write_nodes
target/debug/examples/armb_write_nodes ~/.svrnmesh/sovereign.db runs/armB/armb_nodes.clean.jsonl sep
sovereign enrich raptor-index sep
```

### Restore (obligation satisfied)

sep returned byte-identical from `backup/` on 2026-07-31: daemon stopped →
lance + meta copied back (`diff -r` clean on both) → `DELETE FROM
conv_raptor_nodes WHERE corpus_id='sep'` (baseline zero rows restored) →
daemon restarted. Meta back at source_version 1780886017 / 11,181 rows.

### Secondary corpora (report-only) — DEFERRED

`conversations-anthropic` (1,262 nodes) + `obsidian-vault-959ee8a8f330`
(608) exercise conversation-bench + briefing signposts. Deferred past the
primary gate: sep answered the G2 question with zero delta, these are
pre-registered as report-only (no gate), and each costs another full
swap/bench/restore cycle on live (non-orphan) rows where a member-chunk-id
drift check must pass first. If P1.1 planning wants the conversation-surface
datum, the harness is reusable as-is (`armb_extractive.py --corpus <id>` +
the same writer + `enrich raptor-index`). Book-report stays dropped (attaches
fresh per run; extractive variant arrives with P1.1's real SummaryMode).

### Incidents (process notes)

- **Daemon stop/start race cost ~1h:** after the swap's index rebuild, a
  `daemon stop && daemon start` race let a late SIGTERM land on the NEW
  daemon at 02:03:01; it exited 0 and launchd did not respawn a
  clean-exiting job. An unbounded health-wait loop masked the death. Lesson
  applied: bound daemon waits (300s) and check process liveness, not just
  the port; confirm full stop before starting.
- **Concurrent-writer torn line:** the first armb compute run was wrongly
  presumed dead (pgrep false negative) and a resume run was started
  alongside it; both finished and interleaved one torn JSONL line. All 271
  nodes validated intact from the remaining lines
  (`armb_nodes.clean.jsonl`); torn line discarded. Determinism of the
  compute makes either writer's row equivalent.
- `source_version` does not change on a swap that preserves `created_at`
  (it derives from row timestamps), so any version-gated index cache would
  not see a same-timestamp content change. The daemon restart made this moot
  here, but it's a freshness-gate blind spot worth knowing about
  (consistent with Wave 0's orphan-liveness finding).

### P1.1 / P1.2 implications

- Extractive summaries at equal coverage are score-parity on the sep
  summarize surfaces — P1.1 may choose extractive-first (cheap, faithful by
  construction, no LLM summarization cost: A′'s abstractive retrofit cost
  1,539s of 35B time for 14 articles vs ~7 min of embed-only time for the
  extractive swap) with abstractive as an upgrade, not a prerequisite.
- The faithfulness contract stands regardless (parity here is about
  retrieval quality, not about summary truthfulness — extractive is
  trivially faithful; abstractive still needs P1.2's verification lane).
- Sizing-doc §1 SP2 row: confidence Low → High for the "extractive is not
  worse at equal coverage" premise on document corpora; conversation-surface
  premise remains at its prior confidence (secondary corpora deferred).

---

## SP3 — Judge throughput for the faithfulness lane

**VERDICT: G3 numbers recorded (informational gate). Fast 4B: 7.2 s/node →
73 min/obsidian-corpus, ~22.4 h at sep scale. Primary 35B: 9.4 s/node → 95 min /
~29.2 h — only 1.3x the fast tier, because the workload is prefill-bound and the
MoE's active params keep decode cheap. AND the 35B is a materially better judge:
decisive support (max_support p50 0.99 vs the 4B's diffuse 0.68) and 2x the
claims extracted per node. Recommendation: primary-tier judging by default;
judge-now for corpora ≤ ~1.5k nodes (≤ ~3.3 h), sample 10-15% above.**

### Question (sizing doc §1)

What does judge-scoring one corpus's summaries cost? $/corpus in minutes known →
judge-now vs wait-for-verifier decision; P1.2 sampling-rate default.

### Method actually run

Harness: `sovereign-inference/examples/sp3_judge_probe.rs` (committed).
Validity gate honored: provider is `SplitInferenceProvider` (README G3 — the
`x_forced_choice` structured-output envelope reaches the daemon only via its
`response_format: json_schema` path; the harness PANICS if a reply is not a
calibrated `{"A":p,"B":p}` distribution, so a silently-invalid run cannot
complete). Protocol replicates production `runtime/grounding/judge.rs`
standalone: `extract_claim_list` template (max 4 claims, temp 0, no thinking)
then per-claim forced-choice support over member chunk passages (2,400-char cap,
12-chunk cap, early exit at support ≥ 0.95).

Corpus: `obsidian-vault-959ee8a8f330` — 608 RAPTOR nodes (606 L0), member texts
resolved from chunks.lance via `scripts/sp3_dump_nodes.py` (6,193/6,193 chunk
ids resolved; the reingest-drift check SP2 flagged is moot here — zero missing).

```
.venv/bin/python scripts/sp3_dump_nodes.py --db ~/.svrnmesh/sovereign.db \
  --corpus obsidian-vault-959ee8a8f330 \
  --chunks ~/.svrnmesh/indexes/obsidian-vault-959ee8a8f330/chunks.lance \
  --out data/sp3_nodes_obsidian.jsonl
cargo run -p sovereign-inference --example sp3_judge_probe -- \
  data/sp3_nodes_obsidian.jsonl <model_id> runs/sp3/<tier>/results.jsonl [limit]
```

Fast tier ran the corpus END-TO-END (608/608 nodes). Primary tier ran a 60-node
sample and extrapolates (the plan's sample-extrapolation clause; a full 608-node
35B pass is hours of machine time that buys no additional information).

### Cost table

| Metric | fast Qwopus3.5-4B (608 nodes, end-to-end) | primary Qwen3.6-35B (60-node sample) |
|---|---|---|
| claims/node (raw / excl. failed extractions) | 1.58 / 1.72 | 3.28 (0 failures) |
| calls/node | 12.86 | 12.40 |
| s/node (mean / p50) | 7.23 / 6.18 | 9.40 / 7.04 |
| claim-extract ms (mean) | 1,100 | 2,399 |
| forced-choice ms/call (mean) | 507 | 624 |
| chunks checked/claim (mean) | 7.5 | 3.5 |
| **min/corpus @ obsidian 608** | **73** | **95** |
| **min/corpus @ conv-anthropic 1,262** | **152** | **198** |
| **min/corpus @ sep-scale 11,181** | **1,347 (~22.4 h)** | **1,752 (~29.2 h)** |

The near-parity is structural: both tiers pay mostly prompt prefill (600-token
passages, 1-token forced-choice replies), and the 35B is an A3B MoE. The 35B
checks HALF the chunks per claim (early exit at ≥ 0.95 fires constantly) while
extracting 2x the claims — more verdicts per node at similar call count.

Reliability during the fast run: 7,818 calls, 107 retried (1.4%), 53 hard-failed
after 3 attempts (0.7%) — residue of the daemon fast-slot Metal-OOM incident
(below). 40 nodes (6.6%) lost their claim extraction to those windows; the raw
claims/node under-counts accordingly (conditional value alongside).

### Verdict quality snapshot (fast tier)

Fast 4B: 959 claims scored; 85.3% supported at the 0.5 threshold; `max_support`
p10/p50/p90 = 0.44 / 0.68 / 0.80 — DIFFUSE. Early-exit rarely engaged; verdicts
sit in the noise-accumulation zone the grounding gate's rescue floor exists for
(judge.rs:302-311).

Primary 35B (60-node sample): 197 claims; 89.3% supported; `max_support`
p10/p50/p90 = 0.46 / 0.99 / 1.00 — DECISIVE. This matches the production
expectation that genuine support measures ~0.99. The tier choice is therefore a
verdict-quality knob first and a cost knob second: at 1.3x cost the 35B produces
calibrated-confident verdicts and twice the claim coverage.

### Stream B seed

Every scored tuple appended as `(member_chunks, claim, verdict, max_support)`
JSONL — `sovereign/bench/faithfulness/obsidian_fast_seed.jsonl` (959 rows,
converter `scripts/sp3_streamb.py`). This is the faithfulness lane's seed format
per the sizing-doc decision.

### Operational finding (rides the memo, mirrors a note)

Mid-smoke the daemon's fast slot hit a Metal GPU OOM
(`kIOGPUCommandBufferCallbackErrorOutOfMemory`) when the 35B primary became
resident alongside fast+embed; llama.cpp then wedges the backend permanently
("backend is in error state ... recreate the backend to recover") and EVERY
subsequent fast-slot decode 503s until daemon restart. Judge batch runs at
P1.2 scale must either pin single-slot residency or treat 503-bursts as a
restart signal, not a retry case.

### Consequences

- **Judge-now vs wait-for-verifier:** judge-now (primary tier) for corpora up to
  conversation scale (≤ ~1.5k nodes ≈ ≤ 3.3 h). At sep scale either tier is an
  overnight batch (22-29 h) — viable once, not per-reindex; wait-for-verifier
  (or sampling) is the steady-state answer there.
- **Judge-model default: primary 35B, not fast.** The 1.3x cost premium buys
  decisive support scores (p50 0.99 vs 0.68) and 2x claim coverage. Use the fast
  tier only when the primary slot is contended.
- **P1.2 default sampling rate:** 100% at ≤ 1.5k nodes; 10-15% stratified above
  (sep at 12.5% ≈ 3.7 h primary ≈ 2.3x the full-obsidian cost).
- **Stream B seeds:** `sovereign/bench/faithfulness/obsidian_fast_seed.jsonl`
  (959 rows) + `obsidian_primary_sample_seed.jsonl` (197 rows). The two tiers'
  verdicts on the SAME 60 nodes also give a free inter-judge agreement probe for
  the verifier lane.
- **P0.3 correction confirmed in practice:** the real judge seams are
  `extract_claim_list` + `forced_choice_ab` (runtime/grounding/judge.rs), and
  the `SplitInferenceProvider` envelope requirement is LOAD-BEARING — a naive
  /v1/chat/completions client would have produced a plausible-looking but
  invalid run. P0.3's visibility-promotion line item should carry that.

---

## SP4 — Qwen3-family rerank on the merged RerankSlot infra

**Verdict: G4 latency bar MISSED on passage-length chunks (22.7 ms/pair batched vs the
< 20 ms/pair bar) — but the protocol question is a resounding YES, the official
Qwen3-Reranker GGUF is the model to adopt, and title-style rerank is effectively free
(2.6 ms/pair). P3.3 re-scopes to an A/B that budgets ~470 ms per top-20 passage pass.**

Measured 2026-07-30 on the M2 Max, release build, in-process `StandaloneReranker`
(auto-detected `RerankProtocol::YesNoLogit`), Metal. Model load excluded; one warm batch
before every timed pass.

### Method (exact commands)

```
cargo build --release -p sovereign-inference --example rerank_batch_check
cargo build --release -p sovereign-inference --example rerank_pairs_probe   # new, committed

./target/release/examples/rerank_batch_check  sovereign/models/harrier-oss-v1-0.6b.Q8_0.gguf
./target/release/examples/rerank_batch_check  sovereign/models/qwen3-reranker-0.6b-q8_0.gguf
./target/release/examples/rerank_pairs_probe  sovereign/models/harrier-oss-v1-0.6b.Q8_0.gguf  research/enrichment-spikes/data/chunks_100.jsonl
./target/release/examples/rerank_pairs_probe  sovereign/models/qwen3-reranker-0.6b-q8_0.gguf  research/enrichment-spikes/data/chunks_100.jsonl
./target/release/examples/rerank_pairs_probe  sovereign/models/qwen3-reranker-0.6b-q8_0.gguf  research/enrichment-spikes/data/chunks_20.jsonl
```

Models: `harrier-oss-v1-0.6b.Q8_0.gguf` (on disk since 2026-07-09) and the OFFICIAL
`ggml-org/Qwen3-Reranker-0.6B-Q8_0-GGUF` (fetched 2026-07-30, symlinked to
`sovereign/models/qwen3-reranker-0.6b-q8_0.gguf` — which is `rerank_batch_check`'s
default path, previously dangling). Fixtures: seeded random sep chunks
(`scripts/dump_chunks.py`, seeds 11/13; p50 ≈ 810/756 chars ≈ 200 tokens).

### Sanity gate (G4 precondition — both pass, timing counts)

| Model | relevant mean | irrelevant mean | separation | max \|score\| |
|---|---|---|---|---|
| harrier-oss-v1-0.6b Q8_0 | +0.204 | −1.360 | **+1.56** | 1.87 |
| Qwen3-Reranker-0.6B Q8_0 (official) | +6.181 | −11.582 | **+17.76** | 12.07 |

No 1e-23 magnitude collapse on either — the official GGUF carries its scoring surface
(Qwen3-0.6B ties `lm_head` to `token_embd`, nothing to drop in conversion).

### Latency (the deliverable)

| Fixture | Model | Batched | Sequential | Speedup |
|---|---|---|---|---|
| 100 sep chunks (~200 tok) | harrier | **22.77 ms/pair** (2277 ms total) | 46.18 ms/pair | 2.03× |
| 100 sep chunks | qwen3-reranker | **22.70 ms/pair** (2270 ms total) | 46.22 ms/pair | 2.04× |
| 20 sep chunks (top-20 shape) | qwen3-reranker | **23.34 ms/pair** (467 ms total) | 46.18 ms/pair | 1.98× |
| 48 short titles | harrier | **2.57 ms/pair** (123 ms total) | 25.30 ms/pair | 9.86× |
| 48 short titles | qwen3-reranker | **2.57 ms/pair** (123 ms total) | 25.33 ms/pair | 9.85× |
| 16 curated passages | qwen3-reranker | 7.33 ms/pair (117 ms total) | 28.46 ms/pair | 3.89× |

Latency is model-independent (same 0.6B backbone): per-pair cost is dominated by doc
prefill, so ms/pair is flat in N for passages (22.7 → 23.3 at N=100 → 20) and batching
amortizes only the per-decode-call overhead (2× on passages, ~10× on short titles).
Prior to beat was jina-v3 Q6_K at ~34–40 ms/pair (RERANK_EXPERIMENT.md): beaten ~1.7–2×,
and the jina protocol itself is flagged broken in-code (rerank_slot.rs:87-94).

### Quality (decides the model, not the gate)

`rerank_batch_check` correctness oracle: both models pass both scenarios (top-8 overlap
8/8; rank shift ≤1 harrier, 0 qwen3; systematic bias ≤0.0016).

But ranking quality diverges hard on the prerank (title) scenario — query "How did
Heisenberg's uncertainty principle reshape philosophical debate about determinism?":

- qwen3-reranker top-3: *Uncertainty principle, Werner Heisenberg, Copenhagen
  interpretation* — on-topic.
- harrier top-3: *Wave function collapse, Great Barrier Reef, Surrealism* — relevance
  noise on short inputs (its +1.56 sanity separation is 11× weaker than qwen3's).

**Adopt the official Qwen3-Reranker GGUF; retire harrier as the working default.**

### Verdict for P3.3

- The sizing doc's "build a Qwen3 protocol branch" line item stays CANCELLED — the
  YesNoLogit branch existed and works end-to-end on the official artifact with zero new
  code (this spike wrote only measurement harnesses).
- G4's exit criterion "< 20 ms/pair on M2 Max" is **not met for passage-length chunks**
  (22.7 ms/pair batched). Per the pre-registered on-failure action, P3.3 does not
  proceed as "cheap enough to be free"; it re-scopes to an A/B with an explicit budget:
  **top-20 → top-5 over full chunks costs ~470 ms/query batched** (vs ~925 ms
  sequential). The A/B decides whether that buys retrieval lift worth the latency
  (decision context: per-article dedup + atlas blend already captured most of the SEP
  lift; reranker residual was +1 SEP source / +5 wiki sources +12 facts).
- **Title-mode prerank is free** (2.6 ms/pair batched, 123 ms for 48 titles) and
  qwen3-reranker's title ranking is clean — a title-level rerank stage is viable at
  essentially no cost even where full-chunk rerank is not.
- llama-server-external `/v1/rerank` need not be priced: the in-process slot already
  beats the jina prior and the constraint is model prefill, not our harness.

### Artifacts

- `sovereign/crates/sovereign-inference/examples/rerank_pairs_probe.rs` (committed):
  sanity gate + 100-pair timing probe, fixture-driven.
- Fixtures: `data/chunks_{20,100}.jsonl` (gitignored; regenerate with
  `scripts/dump_chunks.py --seed 11|13`).
- Hygiene for D1 (from the plan, updated by this run): `rerank_smoke.rs:6,21` still
  points at nonexistent `jina-...-Q6_K.gguf`; `rerank_batch_check.rs` default path is
  now VALID (official GGUF symlinked); `sovereign-contracts/src/traits.rs:510-511`
  still claims a `[rerank]` models.toml section that doesn't exist.

---

## SP5 — Noun-phrase extraction + Leiden in Rust: adopt or write?

**VERDICT: G5 PASS — 5.2 s wall for 10k chunks (gate < 300 s, 57x headroom); 17/20
sampled communities eyeball-cohere. Adopt `leiden-rs` for community detection;
write the noun-phrase/co-occurrence layer ourselves (it is ~250 lines and the
probe IS the first draft). P2.2 confidence Med → High.**

### Question (sizing doc §1)

Noun-phrase extraction + Leiden/Louvain in Rust — adopt or write? Exit: concept
graph for 10k chunks < 5 min CPU; communities eyeball-cohere.

### Crate survey (2026-07-31)

| Crate | Version | License | Verdict |
|---|---|---|---|
| `leiden-rs` | 0.8.1 (2026-05-15) | MIT/Apache-2.0 | **ADOPT.** Core is dependency-tiny (rand, rustc-hash, thiserror), takes a CSR edge list directly (`GraphDataBuilder`), 4 quality functions, seedable. Its optional petgraph adapter wants petgraph ^0.8 (we pin 0.6) — skip the adapter, feed edges directly. Caveat for P2.2 productionization: repo hosted on gitcode.com (mirror), 9.2k downloads — vendor or pin-audit before production use. |
| `graphrs` | 0.11.16 (2025-12) | MIT | Louvain + Leiden but drags its own graph type + quick-xml/rayon/serde tree. More surface than needed. |
| `single-clustering` | 0.6.1 | non-standard license | Excluded on license alone. |

Hand-rolled Louvain (~200 lines) remains a viable fallback if the provenance
caveat ever bites; the probe's clean seam (edge list in, partition out) makes the
swap trivial.

### Method actually run

Harness: `corpus-engine/examples/concept_graph_probe.rs` (committed; leiden-rs
added to corpus-engine dev-dependencies only — no production code touched).
Fixture: 10,000 CONTIGUOUS chunks (337 whole articles) from
`~/.svrnmesh/indexes/wikipedia/chunks.lance`, offset 500000
(`scripts/sp5_dump_wiki.py`; contiguous because chunks.lance is article-ordered —
a random sample gives ~1 chunk/article and an artificially thin graph).

```
.venv/bin/python scripts/sp5_dump_wiki.py --corpus ~/.svrnmesh/indexes/wikipedia/chunks.lance \
  --offset 500000 --n 10000 --out data/sp5_wiki_10k.jsonl
cargo run -p corpus-engine --features treesitter --example concept_graph_probe -- \
  research/enrichment-spikes/data/sp5_wiki_10k.jsonl \
  research/enrichment-spikes/runs/sp5/communities_r2.txt 2.0
```

Pipeline (POS-free, patterned on `extract_motif_candidates`'s
tokenization/stoplist/df machinery — sovereign-tools/src/document_asset.rs:2574):

1. **Candidates:** RAKE-style phrases (token runs between stopwords, 1-4 tokens)
   + capitalization runs (catches stopword-bridged NPs like "Bank of England"),
   lowercased. 145,795 distinct candidates from 10k chunks.
2. **Vocabulary:** df band 3 ≤ df ≤ 0.05·N, tf·idf rank, top 5,000 concepts.
3. **Edges:** chunk-window co-occurrence, raw count ≥ 2, then df-normalized
   (cosine-style `cooc/sqrt(df_a·df_b)`). 146,905 edges.
4. **Communities:** leiden-rs, modularity, resolution 2.0, seed 7 → 68
   communities.

### Numbers (debug build, M2 Max, single-threaded)

| Stage | ms |
|---|---|
| load JSONL | 55 |
| phrase extraction + df | 1,605 |
| vocabulary (prune + rank) | 12 |
| co-occurrence edges | 586 |
| Leiden | 368 |
| **total** | **~5,200** (vs 300,000 gate) |

Debug build and one core — a release parallel build has another order of
magnitude available. Extrapolating linearly, even the 1.94M-chunk full wikipedia
corpus is ~17 min CPU at this rate (phrase extraction dominates and is
embarrassingly parallel per chunk).

### Eyeball verdict (top-20 communities by size, runs/sp5/communities_r2.txt)

**17/20 cohere** against article titles: Pleistocene megafauna, battles,
mathematical/economic theory texts, philosophy of life/God, Catholic sacraments,
film actors, materials engineering, Papua/Indonesia, Pacific exploration, stage
actors (split cleanly FROM film actors), US disfranchisement politics, visual
artists, family policy, colonial Mexico/Inca, intellectual history, Near East
archaeology (Ebla/Jericho/Prehistory), Roman Curia governance, fish anatomy.
**3 mixed:** #10 (Mount Unzen + news broadcasting + Triangulum Galaxy), #16
(loose intellectual-history grab bag), #17 (longbow + Black Dahlia + Van Gogh).

Tuning that mattered (both fixes are pre-registered in the probe's comments):
- The motif single-doc df band (≤0.3) admits corpus-generic vocabulary at
  10k-chunk scale — run 1's largest "community" was a hub mush of
  time/years/according. Fix: df ≤ 0.05·N + calendar-term stoplist.
- Raw co-occurrence counts let hub concepts dominate modularity; df-normalizing
  edge weights sharpened 13 coarse communities into 68 mostly-clean ones.

### Consequences for P2.2

- **Adopt-or-write answer: both, split by layer.** Communities: adopt leiden-rs
  (with the vendoring caveat above). NP extraction + graph build: write —
  it is small, the motif machinery precedent covers the hard parts, and no
  surveyed crate provides it.
- P2.2 size unchanged (L 10-15d), confidence Med → **High**. The
  "entity-co-occurrence-only" fallback (G5 on-failure branch) is NOT needed.
- Real remaining work for production P2.2: incremental updates (the probe is
  batch), concept labeling, and cross-corpus df calibration — none of which the
  spike question covered.

---

## SP6 — Late chunking on the 0.6B embedder: memory + recall?

**VERDICT: G6 answered on all three deliverables. (1) Binding: token-level reads
WORK on llama-cpp-4 0.4.2 — the 0.2.x null-buffer failure is gone. (2) Memory:
peak process RSS 7.1 / 12.8 / 24.4 GB at W = 8k / 16k / 32k. (3) Recall:
hit@5 = hit@10 = 1.000 for every arm (golden saturates at article granularity);
MRR 0.953 status quo vs 0.961–1.000 late — late chunking fixed the ~2 of 17
queries whose top-1 chunk was off-article, at 1.4–2.9x embed wall-clock.
Recommendation: DEFER the P2.4 late-chunking follow-on — no demonstrated recall
gain that pays for the memory, embed-time, and offsets-plumbing costs; re-open
trigger below.**

### Question (sizing doc §1, gate G6)

Can `qwen-embedding-0.6b` produce token-level (unpooled) embeddings over long
windows on our vendored binding, at what memory, and does post-pooled per-chunk
embedding beat status quo on a small recall golden? Not pass/fail — binding
verdict + ceiling + hit@k delta recorded. Gates only the P2.4 late-chunking
follow-on (go/defer).

Honesty rule (pre-registered): the status-quo baseline is LAST-token-pooled
chunks — the GGUF's `qwen3.pooling_type = 3` — embedded through the same
gguf-native pooled path production uses. The late arm is compared against that,
with a last-token-per-span variant alongside mean-per-span.

### Binding verdict — the headline

**Token-level reads WORK on the vendored llama-cpp-4 0.4.2** (llama.cpp b9982).
`with_pooling_type(LlamaPoolingType::None)` + all-logits batch +
`embeddings_ith(i)` returns a distinct, non-null 1024-dim vector per token
(probe: 11 tokens, norms 103.1–123.1, all distinct). The prior failure —
`embeddings_ith` returning a null buffer under pooled layout on the bundled
0.2.x binding (embed_slot.rs:178-187 history comment) — does not reproduce on
0.4.2. The RE-TEST the plan called for is answered: the binding is not the
blocker anymore.

### Method actually run

Harness: `sovereign/crates/sovereign-inference/examples/sp6_late_chunk.rs`
(committed). Fixtures: `scripts/sp6_prep.py`.

```
.venv/bin/python scripts/sp6_prep.py \
  --questions ../../sovereign/bench/sep/questions.toml \
  --parquet ~/.svrnmesh/indexes/_downloads/sep.parquet \
  --chunks ~/.svrnmesh/indexes/sep/chunks.lance \
  --n-docs 30 --out-dir data
cargo run -p sovereign-inference --example sp6_late_chunk -- \
  --model sovereign/models/qwen-embedding-0.6b.gguf \
  --data-dir research/enrichment-spikes/data \
  --out research/enrichment-spikes/runs/sp6 --windows 8192,16384,32768
```

- **Docs:** 30 SEP articles = union of `expected_sources` from
  `sovereign/bench/sep/questions.toml` (21 questions, 57 unique slugs), ranked
  by how many questions expect them then by length; 60k–187k chars each
  (~15k–47k tokens — several exceed the 32k window, exercising the
  multi-window path). Article text reconstructed from the source parquet
  (`~/.svrnmesh/indexes/_downloads/sep.parquet`, rows in file order per slug,
  joined with `\n`).
- **Chunks:** the 4,472 production chunks for those articles from
  `~/.svrnmesh/indexes/sep/chunks.lance` — real production chunking, not
  re-chunked for the spike.
- **Golden:** the 17 (of 21) sep bench questions with ≥1 expected article in
  the pool. Hit@k = top-k chunks contain any chunk from an expected article;
  MRR on the first expected-article chunk. Both arms share identical query
  embeddings (gguf-native pooled path, Qwen3-Embedding query instruction
  prefix + `<|endoftext|>`, L2 app-side — exactly the production quirks).
- **Status-quo arm:** per-chunk embed through the gguf-native pooled path (no
  explicit `with_pooling_type` — libllama reads `qwen3.pooling_type=3` → Last),
  production geometry: `AddBos::Always`, `<|endoftext|>` appended, 1024-token
  truncation, 16-seq packed batches.
- **Late arm:** per W ∈ {8k, 16k, 32k}: fresh context (`n_seq_max=1`,
  `n_ctx=n_batch=W`, `n_ubatch=2048`, pooling None), docs decoded in
  consecutive W-token windows (KV cleared between windows), per-token
  embeddings read via `embeddings_ith`, then per chunk span: mean-pool
  (`late_mean_wN`) and last-token (`late_last_wN`), L2-normalized.

### Span location — the offsets-plumbing finding

Chunk offsets do not exist anywhere in the pipeline (`TextChunk{content,index}`
is offset-free), so the harness re-locates each chunk's text in the
reconstructed article. Two real-world lossage sources surfaced:

1. **Production chunks carry a `"{slug}\n\n"` title prefix** the source doc
   does not contain (prepended at ingest). Exact match fails on 100% of chunks
   until the harness falls back to locating the body after the first `\n\n`.
2. Whitespace drift (parquet rows lead with a space; chunk bodies are
   stripped) — handled by a whitespace-collapsed match with an offset map back
   to raw bytes.

With both fallbacks: **4,472/4,472 chunks located, 0 unlocatable, 0 docs with
detokenization drift** (token byte offsets from `token_to_bytes` reconstruct
every article byte-exactly). Production late chunking still wants offsets
threaded through `Chunker`/`TextChunk` — reconstruction worked here because
SEP's paragraph chunker is content-preserving modulo the title prefix, which
is NOT guaranteed for other chunkers (fixed.rs overlaps, sectioned.rs headers).

### Numbers

Debug build, M2-class host, Metal (999 layers offloaded), daemon resident
alongside (35B primary + 4B fast + embed slot — the realistic worst case for
Metal headroom). Raw artifacts: `runs/sp6/{results.json,run.log,stderr.log}`.

**Memory + throughput per window** (659,122 doc tokens per arm; peak RSS is
process-cumulative/monotone, so each arm's "after run" is the ceiling with that
window; the model itself is 1.2 GB of it):

| W | wall s | tok/s | peak RSS after arm |
|---|---|---|---|
| status quo (per-chunk, 16-seq packed) | 130.5 | ~5,050 effective | 5.8 GB |
| late 8,192 | 182.7 | 3,607 | 7.1 GB |
| late 16,384 | 265.1 | 2,486 | 12.8 GB |
| late 32,768 | 378.4 | 1,742 | 24.4 GB |

Embed-time cost delta: **1.4x / 2.0x / 2.9x** over status quo at the same token
volume. Metal allocation is lazy — RSS grows during the first window decode,
not at context creation, so the ceiling only shows under load. 32k's 24.4 GB
peak is marginal on a host already holding the 35B stack resident (the SP3
Metal-OOM wedge is the failure mode to respect); 8k is comfortably cheap.

**Recall** (17 queries, 4,472-chunk pool, hit = any top-k chunk from an
expected article):

| arm | hit@5 | hit@10 | MRR |
|---|---|---|---|
| status_quo | 1.000 | 1.000 | 0.953 |
| late_mean_w8192 | 1.000 | 1.000 | 0.961 |
| late_last_w8192 | 1.000 | 1.000 | 0.971 |
| late_mean_w16384 | 1.000 | 1.000 | 1.000 |
| late_last_w16384 | 1.000 | 1.000 | 0.971 |
| late_mean_w32768 | 1.000 | 1.000 | 1.000 |
| late_last_w32768 | 1.000 | 1.000 | 1.000 |

Read honestly: the golden **saturates at article granularity** — the sep bank's
`expected_sources` labels are per-article, and with ~150 chunks per expected
article in the pool, every arm puts a right-article chunk in the top 5 for all
17 queries. The only discriminative signal left is MRR (top-1): status quo
mis-ranks the top chunk for ~2 of 17 queries; the 16k/32k late arms fix both.
That is +0.047 MRR max, i.e. one-to-two queries on n=17 — directionally
consistent (bigger window → better, mean ≥ last at 16k+), but within noise and
invisible at every k ≥ 5. No chunk-granularity relevance labels exist to
sharpen this without authoring a new golden.

### Exit-criterion verdict

**Met.** Gate G6 asked for binding verdict + memory ceiling per window + hit@k
delta recorded, explicitly not pass/fail. All three are recorded above with the
exact commands. The plan's RE-TEST instruction on the 0.4.2 binding is
answered: works.

### Size/confidence update + go/defer recommendation

**DEFER the P2.4 late-chunking follow-on** (it was "separately funded M-L,
behind SP6" — do not fund it now):

- The measurable recall gain at production-relevant k (≥5) is exactly zero on
  this golden; the top-1 improvement is 1–2 queries of 17.
- The costs are real and now priced: 1.4–2.9x embed wall-clock, 7–24 GB peak
  RSS per ingest worker, plus the production plumbing this spike measured
  around — offsets threaded through `Chunker`/`TextChunk`, the `"{slug}\n\n"`
  title-prefix mismatch between stored chunk text and source doc, per-corpus
  embed stamps (vectors change → same compatibility discipline as
  `EmbedModelInfo`), and content-preserving reconstruction is NOT guaranteed
  for overlapping/sectioned chunkers.
- If ever adopted, W=8k is not the sweet spot despite being cheapest — the
  MRR wins only appear at 16k+, which is where memory bites.

Re-open triggers: (a) a chunk-granularity golden (or notes_tiered failure
class) showing status-quo losses attributable to missing cross-chunk context —
this article-level golden structurally cannot see them; (b) P2.4's cheaper
sibling (contextual embed-text assembly, no vector-layout change) A/Bs positive
and still shows headroom. The harness (`sp6_late_chunk.rs`) is committed and
re-runnable against any future golden in minutes.

SP6 row in `ENRICHMENT_ROADMAP_SIZING.md` §1 updated; P2.4 stays `M (3-5d) ·
Med` for its non-late-chunking scope.
