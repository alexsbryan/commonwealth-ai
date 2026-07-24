# Enrichment Turbocharge — Handoff Brief

*Written 2026-07-24 at the end of the benchmarking + turbocharge session. Everything
below is UNCOMMITTED working-tree state (see §7). Companion memory:
`project_enrich_resource_benchmark.md` and `project_oicp_fast_slot_hijack.md`.*

## 1. Objective and where it stands

**Objective (Alex, standing):** desktop book ingest — attach → fully-Ready
(skeleton + GLiNER + RAPTOR) — in **≤ 6 minutes** on this box, with **PROBE-6
quality held** (see §5). Tight iterations, subset builds for speed probes.

**State:** full book was 16.6 min (4B enrich) at arc start; the current stack
measures **185s on the 300-chunk subset ≈ 9.1 min full-book projection**
(10.2 min measured at the iteration-5 stack). Quality gate **passed** at the
iteration-5 stack. Target not yet reached; the remaining path is profiled, not
speculative (§6).

## 2. The measurement system (use this, don't rebuild it)

- **Harness:** `svrn bench book-report` (bench binary = `target/debug/sovereign-cli`,
  verb dispatches to `sovereign-cli-llm`). It drives the REAL desktop attach path
  in-process (`DocumentAssetManager::ingest` → skeleton → RAPTOR).
- **Resource ledger:** `bench_cmd/resource_meter.rs` (`MeteredInference` decorator) —
  per-phase LLM calls / prompt+completion tokens / embed counts / `models_seen`,
  printed after each run and persisted in `timings.json`. `models_seen` is the
  routing truth-teller; trust it over any "chat model:" label.
- **Model-role flags:** `--enrich-model <id>` / `--answer-model <id>` /
  `--judge-model <id>`. **Always pin `--judge-model Qwen3.6-35B-A3B-MTP-UD-Q6_K`**
  when varying anything else, or the judge moves with the experiment.
- **Subset speed probe (~3–5 min/iteration):** truncated book in a cache dir —
  the bench accepts a pre-existing `974.txt` without sha checks:
  ```
  head -c 168000 ~/.sovereign/bench-cache/book-report/974.txt > <dir>/974.txt
  target/debug/sovereign-cli bench book-report --cache-dir <dir> \
    --enrich-model Qwen3.5-4B-UD-MTP-Q6_K_XL --questions __none__
  ```
  (`--questions __none__` matches nothing → build only.) Subset↔full scaling
  ≈ ×2.95–3.15, validated twice.
- **Phase walls:** parse `transitions[]` in the run's `timings.json` (gaps > a few
  seconds are the story). `[t3-profile]` stderr line (temporary `eprintln` in
  `build_atlas_artifacts_with_checkpoint`) splits RAPTOR tree vs motifs.
- **Run cadence:** foreground Bash runs (Alex's preference; background tasks were
  swept/killed twice). Foreground cap is 10 min — only >10-min full-book gates go
  to background.

## 3. Iteration log (subset attach→Ready)

| # | Lever | Result | Verdict |
|---|---|---|---|
| 0 | baseline, 4B-MTP enrich | 331s | — |
| 1 | skeleton `ExtractDurable`→`EnrichBulk` (Fast lane) + fan-out 6→12 | 307s | keep (enables FastShort) |
| 2 | fast slot GGUF → `Qwen3.5-4B-UD-MTP-Q6_K_XL` (config.toml swap) | 296s | keep |
| 3 | `SOVEREIGN_N_UBATCH=2048` env knob (systemd drop-in `20-ubatch.conf`) | 290s | dud for this path (FastShort already 2048); knob kept, harmless |
| 4 | **window skeleton**: 1 deduped entity list / 12-chunk window, grammar 1-line, deterministic code-side chunk attribution (`parse_window_skeleton_batch`) | 285s (savings hide under embed overlap) | keep — load-bearing |
| 5 | **batched Pass-B**: one call names ALL segments (`idx\|title\|func` lines, grammar-forced count); `summary`/`key_entities` now empty (briefing reads only title+range) | **209s** (killed the 135s silent block) | keep |
| 6 | Pass-B chunked ≤64 segs/call (fixes >85-segment truncation found by the gate) + raptor summaries → `EnrichBulk` | 221s (full titling costs a few s) | keep — correctness |
| 7 | **reuse T1 stored embeddings for TextTiling** (`extract_segments` takes `stored_embeddings`; call site fetches + sorts by `chunk_index`) | **185s** | keep |

Meta-lesson with receipts: micro-levers (routing/MTP/ubatch) bought ~12%;
profile-driven WORK CUTS bought the rest. Decode volume is the wall on this
box (Vulkan/LPDDR5 — batched decode doesn't amortize; FastShort ≈ ties MTP).

## 4. Current profile (iteration-7 subset run)

```
0–78s    embedding+indexing (embed slot)  ∥  window skeleton (fast slot, done ~70s)
78.7s    rag_available
78–92s   T2 tail (~13s — action atoms / persist)
92–155s  segments naming (chunked Pass-B) ∥ overview          (~30s post-reuse)
155–185s RAPTOR: tree=57.8s TOTAL in block, motifs=1.6s, persistence/ANN small
```
The RAPTOR tree (18 nodes) is **prefill-bound on full cluster text**
(~3k tok/summary; `SUMMARIZE_BUFFER=6` already coalesces into FastShort).

## 5. Quality protocol (non-negotiable before shipping any lever)

- **PROBE-6** (~18 min, vs ~65 for the full bank): `--questions
  stevie_address_label,professor_perfect_detonator,mother_almshouse_move,winnie_realises_verloc_role,winnie_incurious_motif,professor_menace_vs_impact`
  with judge pinned. **Noise floor: mean per-question |Δmech| = 15.3 pts
  (measured same-condition repeat); judge ±1.** Aggregate moves <15 pts = noise.
- Semantics: `professor_perfect_detonator` = saturated canary (a drop is damage);
  `stevie_address_label` = entity-anchor tracker (broke under 2B enrichment —
  skeleton entity loss signature); `winnie_realises_verloc_role` = rock-stable.
  **Any scary single reading: re-fire that question on the same asset before
  concluding** (the iter-5 gate's "T2 collapse" was variance; re-fire 90/4.5).
- Full 20-question bank only at plateaus. Full-bank references (this runtime,
  post-fix): 35B-enrich 59%/2.24 · 4B-enrich 55%/2.06 · 4B/4B 56%/2.12 — all
  statistically even. **Iterations 6–7 have NOT yet had a full-book PROBE-6
  gate** (they're correctness + waste-removal, low risk, but gate before ship).
- May's 3.69 judge record: half framing error (best-of-six), most of the rest
  was the quote-verifier defacement (fixed this session). Don't chase it.

## 6. Remaining path to ≤6 min (sized, in order)

1. **RAPTOR leaf summarization redesign** — the gating piece (~58s → target
   ~20-25s). Options: clip per-chunk contribution in the summary prompt, or
   single-pass leaves. **Constraint:** `quote_spans` must stay verbatim-true
   (they anchor retrieval grounding); clip the prompt, not the quote extraction.
   Code: `raptor_atlas.rs` `summarize_clusters_buffered*` + prompt at ~line 770.
2. **Embed floor (78s for 301 chunks ≈ 3.8 chunks/s)** — outer `EMBED_BATCH=64`
   in `document_asset.rs` is NOT the limit; look at the embed slot internals
   (n_seq/ubatch in `sovereign-inference/embedded`). May's rate was ~16/s.
3. **T2 tail 13s** (post-skeleton, pre-multi_hop_ready — action atoms/persist).
4. **T2∥T3 overlap** — RAPTOR needs only embeddings (ready at ~78s); segments
   naming needs `main_entities`. Same 4B slot serializes LLM work, so overlap
   only buys the non-LLM parts unless work is split across fast+primary slots.

Sized honestly: (1)–(3) ≈ subset 110–130s ≈ **5.8–6.8 min full-book**.

## 7. Working-tree map (ALL UNCOMMITTED; per Alex: no commits without explicit ask)

| File | What changed |
|---|---|
| `oicp-client/src/lib.rs` | **Fast-slot hijack fix**: Medium/Slow pin the provider's chat model; auto OICP envelope only when model empty. Tests updated. Pre-fix HTTP bench numbers measured the 4B — re-baseline anything older than 2026-07-23. |
| `sovereign-core/src/quote_verification.rs` | `normalise_for_match` (typographic fold + markdown-marker strip + edge-ellipsis trim); composites still fail. 5 new tests. |
| `sovereign-core/src/runtime/attached_doc_render.rs` | Lenient `<tool_call>` parse (balanced-brace, missing close tag), `strip_dangling_tool_calls`. |
| `sovereign-core/src/runtime/handlers/attached_doc.rs` | Malformed-tool-call retry gate; scaffolding strip on accept + cap-hit paths. |
| `sovereign-core/src/runtime.rs` | re-export of `strip_dangling_tool_calls`. |
| `sovereign-tools/src/attached_document_search.rs` | **Wrong-asset fix**: resolve by conversation via `DocumentSession.source`; most-recent-Ready stub is fallback only. |
| `sovereign-tools/src/document_asset.rs` | Window skeleton + `parse_window_skeleton_batch`; chunked batched Pass-B + `parse_segment_title_lines`; `extract_segments(stored_embeddings)`; `[t3-profile]` eprintln; `T2_BATCH_CONCURRENCY=12`. |
| `sovereign-tools/src/raptor_atlas.rs` | summaries `ExtractDurable`→`EnrichBulk`. |
| `sovereign-inference/embedded/{model_slot,prompt_helpers}.rs` | `chat_slot_n_ubatch()` env knob (`SOVEREIGN_N_UBATCH`, default 512). |
| `sovereign-cli-llm/bench_cmd/{book_report,mod}.rs` + `resource_meter.rs` (new) | Ledger + model flags + judge think-off fix (`enable_thinking=false`, 512 tok). |
| `sovereign-mesh/src/fim_adapter.rs` | pre-session change (FIM arc), not this work. |

**Machine state changed:** `~/.sovereign/config.toml` — `context_size=32768`
(was 8192; backup `config.toml.bak-pre-ctx32k-*`) and fast slot → MTP 4B GGUF
(plain-4B line kept commented). Systemd drop-in
`~/.config/systemd/user/sovereign.service.d/20-ubatch.conf` sets
`SOVEREIGN_N_UBATCH=2048`. Daemon restarts via `systemctl --user restart
sovereign` (release binary via `scripts/dev-release.sh` on THIS box).
Daemon extras loaded this session (`bench-2b`, `bench-27b`) vanish on restart.

## 8. Gotchas that cost time (don't repay them)

- Dev toolchain is DEBUG (`cargo build -p sovereign-cli-llm --features
  corpus-engine/treesitter`); release-required = error, not signal.
- The deployed daemon on THIS box IS release via systemd — `dev-release.sh` to
  rebuild it; a debug-only build is invisible to the daemon.
- Bench answering/judging runs IN the bench process; enrichment inference goes
  over HTTP to the daemon. Daemon-side edits need daemon rebuild+restart;
  sovereign-tools/core edits only need the bench binary rebuild.
- Grammar (llguidance) works on the FastShort lane; grammar-forced line counts
  are what make batched extraction safe.
- Full-workspace gates: `scripts/sovereign-lint.sh --human` /
  `sovereign-test.sh --human` (watcher deliberately off; don't re-enable during
  bench runs — cargo contention corrupts timing).
- Desktop/daemon must be rebuilt to ship the sovereign-core/tools fixes to the
  product; the bench carries them in-process already.
