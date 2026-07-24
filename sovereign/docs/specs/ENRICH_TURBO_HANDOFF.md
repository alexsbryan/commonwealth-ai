# Enrichment Turbocharge — Handoff Brief

*Written 2026-07-24 at the end of the benchmarking + turbocharge session. Everything
below is UNCOMMITTED working-tree state (see §7). Companion memory:
`project_enrich_resource_benchmark.md` and `project_oicp_fast_slot_hijack.md`.*

## 1. Objective and where it stands

**Objective (Alex, standing):** desktop book ingest — attach → fully-Ready
(skeleton + GLiNER + RAPTOR) — in **≤ 6 minutes**, with **PROBE-6 quality held**
(see §5). **Portability constraint (Alex, 2026-07-24):** tuning must pay off on
the *median* target machine — roughly **32 GB, CPU-only** — not just this Strix
Halo box. That reframes every lever: a change that only harvests idle batch
capacity (concurrency, fan-out, dispatch order) is neutral-to-negative on a
saturated CPU; only changes that cut **total token volume** generalize. Report
the ledger `totals` (prompt/completion tokens) next to wall-clock every
iteration — wall-clock alone hides which kind of win you have.

**State (end of 2026-07-24 turbo session #2):** the 301-chunk subset went
**188s → 89.4s** attach→Ready, and — the number that matters for the portability
constraint — **prompt tokens 88,232 → 26,664 (−70%)**, LLM calls 54 → 31.
Full-book projection ≈ **4.4 min** on this box; the machine-independent claim is
the token cut, which should track far more directly on a CPU-only host.
**The headline win (GLiNER entity swap, §6.5) PASSED its full-book PROBE-6 gate**
(2026-07-24). GLiNER vs LLM path, both full-book, judge pinned 35B: **judge mean
2.80 vs 1.40, mech 76% vs 64%, fabrications 7 vs 11, Ready 234s vs 521s.** Every
question at or above its LLM-path reading; **both canaries held**
(professor_perfect_detonator 5/5, stevie_address_label 57→**71%** — the entity
anchor tracker went UP, ruling out the 2B-style entity-loss failure mode).
HONEST BOUND: two single runs can't prove GLiNER *improves* quality (the +1.4
swing exceeds the noise floor but one-run-each carries variance); the defensible
claim is **no regression + both canaries held + aggregate inside band while
ingest halved.** Ship-blocker is now only the desktop wiring (§6.6), not quality.

### Iteration ledger (301-chunk subset; tokens are the portable metric)

| stack | attach→Ready | RAG-ready | prompt tok | LLM calls | portable? |
|---|---|---|---|---|---|
| baseline (iter 7) | 188s | 78s | 88,232 | 54 | — |
| +LPT +RAPTOR member cap (iter 8–10) | 149.5s | 78s | 77,140 | 57 | cap=yes, LPT=no |
| +client embed_batch (iter 11) | 147.1s | **25.9s** | 77,632 | 57 | yes (bandwidth) |
| +GLiNER entity pass (iter 12) | **89.4s** | **12.3s** | **26,664** | 31 | **yes (−70% tok)** |

Which wins are portable, explicitly:
- **GLiNER swap** and **RAPTOR member cap** cut tokens → pay off everywhere,
  more on slow boxes. These are the real wins.
- **client embed_batch** amortizes HTTP round-trips + engages the embed slot's
  16-seq packed decode → worth *more* on CPU-only (every round-trip is dead
  time there). RAG-ready 78s → 26s is the user-visible half.
- **LPT dispatch / Pass-B fan-out** are scheduling only. On this box they
  harvested idle FastShort batch capacity; on a saturated CPU they are neutral,
  and Pass-B's 64→14 split is a slight token *negative* (+12% prompt from
  repeated preamble). Kept because smaller Pass-B calls also fix a real
  truncation bug, not for the (box-specific) speed.

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

## 6. What got done this session (§6.1–6.5) + what's left (§6.6)

The remaining-path items from the prior handoff were mostly *done* this session;
the instrument that made them findable was a **per-call ledger** (§6.1).

### 6.1 Per-call resource ledger — the instrument
`bench_cmd/resource_meter.rs` now logs every LLM/embed call individually
(`CallRecord`: phase, `start_ms`, `wall_ms`, prompt/completion tokens, a 64-char
prompt fingerprint) and rolls them up by **call family** in
`render_call_families()`. This was load-bearing: the phase buckets CANNOT
separate the window skeleton from the RAPTOR tree (both run under
`building_skeleton`), and a phase's `llm_wall_ms` sums concurrent calls so it is
NOT a duration. The family table's `window` column IS a real elapsed duration
(first-start → last-end). Printed to stderr after each run and persisted in
`timings.json` under `resources.calls[]`. **Use it — don't add eprintlns.**

### 6.2 RAPTOR leaf redesign (DONE — the prior §6 item #1)
`raptor_atlas.rs`: leaf prompts capped to the 13 most-central members
(`MAX_MEMBERS_IN_SUMMARY_PROMPT` + `descriptors_for_prompt`), sized against
`FAST_SHORT_MAX_INPUT_CHARS=6000` (a >6k-char prompt is refused by the batched
FastShort lane and serializes on the single-seq Fast slot). Tree 63s → 35s.
`quote_spans` invariant HELD — the cap bounds the *prompt*; every member still
lands in `evidence_chunk_ids` and quote extraction still runs over all members.
`SUMMARIZE_BUFFER` 6 → 8 (matches the lane's `n_seq_max=8`; it was briefly 12,
which overshot — corrected). LPT (longest-processing-time-first) dispatch so a
big cluster doesn't start behind a full buffer.

### 6.3 Client embed batching (DONE — the prior §6 item #2, root cause was elsewhere)
The embed floor was NOT the slot internals. `InferenceProvider::embed_batch` had
a **trait default that loops `embed()` one text per HTTP round-trip**; neither
`RemoteApiProvider` nor `SplitInferenceProvider` in `oicp-client` overrode it, so
the daemon had served 8959 consecutive `sequences=1` embeds. The whole server
path was already batch-capable (`routes_inference::embeddings` array input →
`embed_batch` → 16-seq packed decode). Fix: `RemoteApiProvider::embed_batch`
posts array input in chunks of 64, sorts by response `index`, verifies row count,
falls back to sequential on rejection. **Also fixes the desktop** (same
provider). embed 84s → 31s, RAG-ready 78s → 26s.

### 6.4 Pass-B segment naming fan-out (DONE — the prior §6 item #3)
`document_asset.rs`: 64→14 segments/call, `buffered(8)`. Portability caveat in
the code — this is scheduling, +12% prompt on CPU-only; kept for the truncation
fix, not the speed.

### 6.5 GLiNER entity swap (DONE mechanically, **GATE PENDING** — the big win)
The T2 window pass asked a 4B LLM to "list the named entities" — literally NER,
and 66% of ingest tokens. Swapped to `gliner_small-v2.1` (ONNX, already
installed) via `DocumentAssetManager::with_entity_extractor(dyn EntityExtractor)`.
`build_skeleton` runs it on `spawn_blocking` and treats **empty ⇒ fall back to
the LLM** (a not-yet-warm `LazyGlinerExtractor` returns empty, must not silently
empty the skeleton). `parse_window_skeleton_batch` split into
`parse_entity_name_list` (LLM) + `attribute_entity_names` (shared) — the latter
recovers document casing by splicing the matched span from source (the
`EntityExtractor` contract returns lowercased). Bench: `--no-gliner` A/B flag,
eager load (not lazy — a lazy loader races the measurement). **prompt tokens
−70%, subset attach→Ready 188→89s; full-book 521→234s.** Machine-independent
(token cut). **GATE PASSED** (see §1): judge 2.80 vs 1.40, both canaries held,
`stevie_address_label` 57→71% (the under-recall risk is affirmatively ruled
out). No regression; some of the gain is variance (two single runs).

### 6.6 What's still open
- **Ship GLiNER to the desktop product — WIRED (2026-07-24), not yet
  runtime-verified.** Desktop: `AppState.entity_extractor` populated in
  `bootstrap_with_progress` (same site as the retrieval `LazyGlinerExtractor`),
  read by all three skeleton-building manager sites in
  `commands/document_asset.rs` (upload, ask-self-heal rebuild,
  rebuild_document_skeleton). Server (mobile host): `manager_from_runtime` reads
  `runtime.gliner` directly. All use `.with_entity_extractor()`, which no-ops to
  the LLM path when absent/not-warm. Builds clean across desktop/server/tools;
  workspace lint pass. **VERIFIED END-TO-END IN THE REAL APP** (2026-07-24):
  launched `target/debug/sovereign-desktop` with `SOVEREIGN_COMMAND_BRIDGE=1`
  (loopback :9745 → real Tauri invoke path), attach-mode; `POST /invoke
  upload_document_asset` on a 71-chunk doc → app log `entity_path="ner"`, **zero**
  "List the named entities" LLM calls to the daemon, asset Ready with 30
  document-cased entities (`Mr Verloc`, not `mr verloc`), 11 action atoms, RAPTOR
  built. A glassbox `build_skeleton — T2 entity extraction path` info log was
  added (records ner-vs-llm per ingest) — a keeper. **Remaining:** `dev-release.sh`
  rebuild to ship to the systemd daemon — a debug build is invisible to it.
- **T2∥T3 overlap** — RAPTOR needs only embeddings (ready ~12s now); Pass-B
  needs `main_entities`. Still serialized on one slot; real overlap needs work
  split across fast+primary slots.
- **CPU-only validation** — every number here is Strix Halo Vulkan. The token
  cuts *should* dominate on a 32 GB CPU box, but that is a projection, not a
  measurement. Set `SOVEREIGN_FORCE_CPU_CHAT=1` (and a CPU embed path) to probe.
- **The RAPTOR leaf level is now the largest single block** (~35s of 89s) — if
  more is needed, `SUMMARIZE_BUFFER` can't help past `n_seq_max`; the lever is
  fewer/cheaper summaries, not more concurrency.

## 7. Working-tree map (ALL UNCOMMITTED; per Alex: no commits without explicit ask)

| File | What changed |
|---|---|
| `oicp-client/src/lib.rs` | **Fast-slot hijack fix**: Medium/Slow pin the provider's chat model; auto OICP envelope only when model empty. Tests updated. Pre-fix HTTP bench numbers measured the 4B — re-baseline anything older than 2026-07-23. |
| `sovereign-core/src/quote_verification.rs` | `normalise_for_match` (typographic fold + markdown-marker strip + edge-ellipsis trim); composites still fail. 5 new tests. |
| `sovereign-core/src/runtime/attached_doc_render.rs` | Lenient `<tool_call>` parse (balanced-brace, missing close tag), `strip_dangling_tool_calls`. |
| `sovereign-core/src/runtime/handlers/attached_doc.rs` | Malformed-tool-call retry gate; scaffolding strip on accept + cap-hit paths. |
| `sovereign-core/src/runtime.rs` | re-export of `strip_dangling_tool_calls`. |
| `sovereign-tools/src/attached_document_search.rs` | **Wrong-asset fix**: resolve by conversation via `DocumentSession.source`; most-recent-Ready stub is fallback only. |
| `sovereign-tools/src/document_asset.rs` | **(session #2)** GLiNER entity swap: `with_entity_extractor(dyn EntityExtractor)`, NER fast-path in `build_skeleton` with empty⇒LLM fallback; `parse_window_skeleton_batch` split into `parse_entity_name_list` + `attribute_entity_names` (document-casing recovery); Pass-B 64→14 segs `buffered(8)`; action-atom fan-out. 4 new attribution tests. Plus session #1: window skeleton, `extract_segments(stored_embeddings)`, `[t3-profile]` eprintln. |
| `sovereign-tools/src/raptor_atlas.rs` | **(session #2)** leaf member cap (`MAX_MEMBERS_IN_SUMMARY_PROMPT=13` + `descriptors_for_prompt`, 2 tests); LPT dispatch; `SUMMARIZE_BUFFER` 6→8. Plus session #1: summaries `ExtractDurable`→`EnrichBulk`. |
| `oicp-client/src/lib.rs` | **(session #2)** `RemoteApiProvider::embed_batch` + `embed_many_one_request` + `EMBED_BATCH_INPUTS=64`; `SplitInferenceProvider::embed_batch` forwards. (Plus session #1 fast-slot hijack fix, above.) |
| `sovereign-cli-llm/bench_cmd/{book_report,mod}.rs` + `resource_meter.rs` | **(session #2)** per-call `CallRecord` log + `render_call_families()` (2 tests); `--no-gliner` flag; eager GLiNER wiring. Plus session #1 ledger + model flags + judge think-off. |
| `sovereign-inference/embedded/{model_slot,prompt_helpers}.rs` | `chat_slot_n_ubatch()` env knob (`SOVEREIGN_N_UBATCH`, default 512). |
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
