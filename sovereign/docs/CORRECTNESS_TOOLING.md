# Correctness tooling

The toolbelt that protects this repo against regressions is wider than `cargo test`. Most of it lives behind CLI subcommands that score a change against a fixed eval bank, write a JSON report, and (for the load-bearing ones) expose flags to gate one axis at a time without rerunning the whole pipeline. This file is the inventory + a field guide for picking the right tool.

If you add a new eval, append it here.

---

## Inventory

Grouped by what each tool actually protects, not by where the code lives.

### 1. Chat semantics — routing & voice

| Tool | Command | Protects | Source |
|---|---|---|---|
| **Wikipedia eval (routing-only)** | `sovereign eval run --bank sovereign/bench/wikipedia/questions.toml --routing-only` | Classifier accuracy: predicted intent vs `expected_intent` per question. Fast iteration — no retrieval, no synthesis. The "did the routing logic regress" gate the user is referring to. | `sovereign/crates/sovereign-cli/src/eval_cmd/mod.rs` |
| **Voice eval — base (centre)** | `sovereign voice eval --all` (or `--scenario <id>`) | Voice-contract adherence over the 12 base scenarios — well-formed witness moves on common shapes. Deterministic checks (length, question density, banned phrases) + LLM-as-judge against 8 principles + 4 avoid-list patterns. `--canned-response "<text>"` scores an arbitrary string without the daemon. Default scenarios dir is `bench/voice/`. | `sovereign/crates/sovereign-cli/src/voice_eval/mod.rs`; bank at `sovereign/bench/voice/*.toml` |
| **Voice eval — hard mode (edges)** | `sovereign voice eval --all --scenarios-dir bench/voice/hard --report bench/voice/baseline/<run>.json` | Adversarial / chaos-monkey companion: 8 scenarios (H01–H08) probing flattery bait, memory gaslight, prompt injection, binary pressure, identity probes, multi-thread, crisis-adjacent, recursive meta. Same scoring harness — point it at a different scenarios dir. Pin `--judge-model` to the 35B across both modes so chat-model variance doesn't get conflated with judge variance. Archived runs at `sovereign/bench/voice/baseline/`. | bank at `sovereign/bench/voice/hard/H0*.toml` (see its `README.md` for the "fair adversarial" rules) |

### 2. Retrieval & synthesis quality

| Tool | Command | Protects | Source |
|---|---|---|---|
| **Wikipedia eval (synth)** | `sovereign eval run --bank … --synth [--no-judge]` | End-to-end recall + synthesis facts on the 52-question Wikipedia bank. Strict scorer + LLM-as-judge for paraphrased coverage. The headline number for retrieval+synthesis. | `sovereign/crates/sovereign-cli/src/eval_cmd/mod.rs`; bank at `sovereign/bench/wikipedia/` |
| **Atlas-grounded retrieval gate** | `sovereign eval run --bank … --with-atlas <id> [--atlas-top-k N]` | Same bank, but fuses atlas Entity embeddings into retrieval. Used to measure source-lift from atlas grounding (e.g. the 50→82.8% jump on 2026-05-02). | same as above |
| **Atlas retrieval eval** | `sovereign enrich atlas-eval --corpus <id>` | Tokenized title-overlap retrieval against a resolved atlas. Per-phase precision/recall/F1 by entity type. | `sovereign/crates/sovereign-cli/src/enrich_cmd/atlas_eval.rs` |
| **Reading-surface diag** | `sovereign reading-diag query "<q>" [--corpus <id>]` | The desktop citation chain: chunk retrieval → neighbor deref → atom-span detection → atom card → cross-corpus links. Validates the reading surface without launching the UI. | `sovereign/crates/sovereign-cli/src/reading_diag_cmd.rs` |

### 3. Enrichment / corpus build correctness

| Tool | Command | Protects | Source |
|---|---|---|---|
| **Atlas phase eval** | `sovereign enrich eval --corpus <id>` | Per-phase precision/recall/F1 (Sections, Facts, Tensions, …) against a golden-set TOML. The standard bench for parsing books and enrichment corpora. | `sovereign/crates/sovereign-cli/src/enrich_cmd/eval.rs` |
| **Eval-median (variance)** | `sovereign enrich eval-median --corpus <id> --runs N` | Reset → build → eval N times. Reports min/median/max F1 per phase to separate signal from noise; flags phases that look stochastic. | `sovereign/crates/sovereign-cli/src/enrich_cmd/eval_median.rs` |
| **Awareness eval** | `sovereign awareness eval [--from-template consulting\|startup\|team-lead]` | Person/org/initiative extraction P/R/F1 over personal-knowledge + conversation-history views. | `sovereign/crates/sovereign-cli/src/awareness_cmd/eval.rs` |

### 4. Throughput & model-fit

| Tool | Command | Protects | Source |
|---|---|---|---|
| **Atlas throughput bench** | `sovereign bench atlas [--corpus <id>] [--tasks …] [--no-warmup]` | Tokens/sec **and** parser-validated correctness for Phase 1 + short-call synthesis tasks against the daemon's currently-loaded primary. Use to pick a primary model before a long ingest. Projects 1800-article runtime. | `sovereign/crates/sovereign-cli/src/bench_cmd/atlas.rs` |
| **Embed throughput bench** | `cargo run -p sovereign-inference --example bench_embed -- …` | `EmbedSlot` decode tok/s across Metal/Vulkan/ROCm. Run on backend swap, llama-cpp upgrade, embed-quant change, or "ingest feels slow." Methodology in `sovereign/docs/BENCHMARKING.md`. | `sovereign/crates/sovereign-inference/examples/bench_embed.rs` |
| **FastShort batched-decode bench** | `cargo run --release -p sovereign-inference --example bench_decode_batch -- --model <fast.gguf> --backend gpu --total-ctx 16384 --n-seq 1,2,4,8 --k 32 --prompt-tokens 900 --gen-tokens 128 --iters 2 --n-ubatch 2048` | Multi-seq autoregressive decode throughput at varying `n_seq_max` against the **chat** path. Used to characterize the FastShort speedup ceiling (2.1–2.8× wall-clock for short calls on Strix Halo / ROCm / Qwen3.5-2B Q6_K) and to detect regressions in the continuous-batched dispatch (`embedded.rs::generate_sync_batched` + `FastShortCoalescer`). Re-run on llama-cpp upgrade or backend swap. | `sovereign/crates/sovereign-inference/examples/bench_decode_batch.rs` |

### 5. UI responsiveness — TTFI

The Playwright suite lives in `sovereign/crates/sovereign-desktop/tests/e2e/`. Run via `npm run` from that directory.

| Tool | Command | Protects | Source |
|---|---|---|---|
| **TTFI (time to first intelligence)** | `npm run test:ttfi` then `npm run report:ttfi` (with optional baseline diff via `report:ttfi:save-baseline`) | Six observed tiers + a derived `gap` measured in-page from the Send-click anchor: `generic`, `specific`, `aux`, `visible`, `thinking`, `content`, and `gap = content − specific`. Mocks at the Tauri-event boundary, not HTTP — measures exactly what the user perceives. Methodology in `sovereign/crates/sovereign-desktop/tests/e2e/TTFI.md`. | `tests/e2e/specs/ttfi.spec.ts` + `fixtures/ttfi-probe.js` + `scripts/ttfi-summary.mjs` |
| **Chat E2E suites** | `npm run test:e2e` | Golden path, edge cases, chaos, conversation routing, placeholder, mesh-health, watched-folder, reading-conversation. | `tests/e2e/specs/chat-*.spec.ts`, `mesh-health.spec.ts`, `reading-conversation.spec.ts`, `watched-folder-detail.spec.ts` |
| **Component / state-machine tests** | `npm run test` (vitest) | Approval, routing, skills, setupWizard, chat XState machines + Svelte component contracts. | `src/lib/**/*.test.ts`, `src/lib/machines/**/*.test.ts` |

### 6. Health & data integrity (diagnostics, not benchmarks)

These don't score quality — they detect drift between expected and actual state. Cheap to run; surface a problem before an eval would.

| Tool | Command | Surfaces |
|---|---|---|
| **System doctor** | `sovereign doctor [--json] [--fix]` | Three-layer health: Sovereign (server, indexes), Commonwealth (daemon, mesh, inference), OmO (skills, MCP). Exit 0 = pass. |
| **Corpus diag** | `sovereign corpus diag <id>` | Distinct-article count vs. recipe filter; flags resume-cursor coordinate-space bugs and incomplete shards (this is how the 17/38 Wikipedia shard gap was found). |
| **Corpus status** | `sovereign corpus status` | Per-corpus shard completion %. |
| **Atlas status / budget** | `sovereign atlas status [--corpus <id>]` · `sovereign atlas budget [--corpus <id>] [--set N]` | Tier-2 enrichment progress (atoms, phases, token spend) + top-N budget. |
| **Spec drift** | `sovereign drift <feature-id>` | Diff between on-disk `spec.md` and approved spec. Accepts changes with rationale. |

### 7. Cargo / vitest test suites

Not eval banks — but they are the floor. Run before any of the above lights up.

| Suite | Run with | Notable |
|---|---|---|
| Sovereign unit tests | `cargo test` (workspace) at `sovereign/` | ~300 tests across crates: router, planner, inference, mesh, core. |
| Commonwealth mesh + scheduler integration | `cargo test --test integration` / `--test scheduler_integration` at `commonwealth/crates/commonwealth-test-harness/` | Mesh formation, fault injection, graceful departure, knowledge assignment, portfolio balancing. |
| Commonwealth unit tests | `cargo test` at `commonwealth/` | API, discovery, inference, state. |
| Corpus engine | `cargo test` at `corpus-engine/` | Filter pipeline, parquet ingest, watcher lifecycle, recipe back-compat, HTTP API pagination. |

### 8. Operator-driven eval harness

Separate from the everyday CLIs above; used for longer experiments where you want a manifest + per-axis analyzers.

| Tool | Command | Source |
|---|---|---|
| **sovereign-eval finalize / score / diff / audit** | `sovereign-eval finalize-run <run-id>` · `sovereign-eval score <run-id> --against <baseline-run>` · `sovereign-eval diff <run-a> <run-b>` · `sovereign-eval audit <earlier> <later>` | `sovereign/crates/sovereign-eval/src/bin/main.rs` |

---

## Field guide — pick the right tool

> "I just changed X. What do I run?"

| If you changed… | Run this | Why |
|---|---|---|
| Routing prompts, classifier, framework cells | `sovereign eval run --bank …/wikipedia_questions.toml --routing-only` | Isolates classifier accuracy from retrieval/synthesis variance. Fast loop. |
| Retrieval, chunking, atlas-grounded fusion | `sovereign eval run --synth` (and `--with-atlas <id>` if testing the atlas path) | The headline sources/facts numbers. Add `--no-judge` for fast iteration; do a final pass with the judge on. |
| Voice / chat-tone prompts, refusal patterns, "voice contract" | `sovereign voice eval --all` (centre); follow with `sovereign voice eval --all --scenarios-dir bench/voice/hard` (edges) before claiming a win | Base = well-formed witness moves; hard = adversarial framing, memory gaslight, prompt injection, binary pressure. A change that holds the centre but breaks the edges is the canonical voice-contract regression. Use `--canned-response` for offline iteration. |
| Primary model swap or quant change | `sovereign bench atlas` first (does it parse? what's the throughput?), then `sovereign eval run --synth` and `sovereign voice eval --all` | Bench gates "can this model produce structured output at all"; the evals gate quality. |
| Enrichment phase prompts (Phase 1/2/3 atlas, books, SEP, Wikipedia ingest) | `sovereign enrich eval --corpus <id>`; `sovereign enrich eval-median --runs 5` if you suspect variance | Per-phase P/R/F1 against the golden TOML. Eval-median separates signal from stochasticity. |
| Atlas resolution / cross-corpus linkers | `sovereign enrich atlas-eval --corpus <id>` | Tokenized title-overlap on a resolved atlas. |
| Personal-knowledge / awareness extractors | `sovereign awareness eval --from-template <name>` | Entity P/R/F1 on builtin templates or your own JSONL. |
| Reading surface (citations, atom cards, cross-corpus refs) | `sovereign reading-diag query "<q>" --corpus <id>` | End-to-end chain without the desktop UI. |
| Embed model / GPU backend / `n_seq_max` / quant | `cargo run -p sovereign-inference --example bench_embed` (see `BENCHMARKING.md`) | Apples-to-apples decode tok/s across Metal/Vulkan/ROCm. |
| FastShort coalescer, `generate_sync_batched`, multi-seq chat decode | `cargo run --release -p sovereign-inference --example bench_decode_batch` | Constant-total-ctx sweep across `n_seq_max ∈ {1,2,4,8}` proves whether a chat-path change preserves the 2.1–2.8× short-call speedup. Run on Phase 1b enrichment regressions or after touching `embedded.rs::generate_sync_batched`. |
| Chat UI (chunk rendering, indicators, slot positions) | `npm run test:ttfi` then `npm run report:ttfi`; `npm run test:e2e` for golden path | TTFI is the perceived-performance gate; the e2e specs cover correctness. |
| Mesh, discovery, scheduler, knowledge assignment | `cargo test --test integration` and `--test scheduler_integration` in `commonwealth-test-harness` | Multi-node scenarios (formation, fault injection, graceful departure). |
| Corpus ingest, recipes, filter pipeline | `cargo test` in `corpus-engine/`; then `sovereign corpus diag <id>` after a real ingest | Unit tests for the filter pipeline; diag catches resume-cursor / dedup bugs that unit tests miss. |
| Specs / approved feature contracts | `sovereign drift <feature-id>` | Detects on-disk drift from approved spec. |
| "Did I break the daemon at all?" | `sovereign doctor` | Three-layer health. Cheap. Run before reporting a failure as a regression. |

### A few rules of thumb

- **Routing changes are cheap to validate; synthesis changes are expensive.** Iterate with `--routing-only` and `--no-judge` until the shape is right; only spend the full judged `--synth` run when you think you're done.
- **Variance is real.** If a phase eval moves by a few F1 points, run `enrich eval-median --runs 5` before declaring a regression. The user has been burned by this — see `feedback_atlas_phase_tuning.md`.
- **Bench before you ingest.** `sovereign bench atlas` is the cheapest way to find out a new primary model can't produce parseable JSON for Phase 1 — much cheaper than discovering it 1800 articles in.
- **Diagnostics first.** If an eval number suddenly tanks, run `sovereign corpus diag` and `sovereign doctor` before you start tuning. Often the cause is data-shape (missing shards, dead peers in `/v1/models`), not the change you just made.
- **TTFI mocks Tauri events, not HTTP.** That's deliberate — it measures what the user perceives, including DOM-level decisions about *where* a signal appears, not just whether one was emitted.

---

## Where things land

- Eval banks: `sovereign/bench/<corpus>/*.toml` (e.g. `sovereign/bench/voice/*.toml`, `sovereign/bench/wikipedia/questions.toml`).
- Eval reports: pass `--output <path>` to anything that supports it. Run outputs are not committed — write them under `target/` or a `sovereign/bench/<corpus>/baselines/` dir as needed.
- TTFI reports: `sovereign/crates/sovereign-desktop/tests/e2e/.ttfi-report.json`; baseline at `.ttfi-baseline.json`.
- Bench harness internals: `sovereign/docs/BENCHMARKING.md` (embed) and `sovereign/crates/sovereign-desktop/tests/e2e/TTFI.md` (UI).
