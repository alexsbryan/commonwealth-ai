# SP4 — Qwen3-family rerank on the merged RerankSlot infra

**Verdict: G4 latency bar MISSED on passage-length chunks (22.7 ms/pair batched vs the
< 20 ms/pair bar) — but the protocol question is a resounding YES, the official
Qwen3-Reranker GGUF is the model to adopt, and title-style rerank is effectively free
(2.6 ms/pair). P3.3 re-scopes to an A/B that budgets ~470 ms per top-20 passage pass.**

Measured 2026-07-30 on the M2 Max, release build, in-process `StandaloneReranker`
(auto-detected `RerankProtocol::YesNoLogit`), Metal. Model load excluded; one warm batch
before every timed pass.

## Method (exact commands)

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

## Sanity gate (G4 precondition — both pass, timing counts)

| Model | relevant mean | irrelevant mean | separation | max \|score\| |
|---|---|---|---|---|
| harrier-oss-v1-0.6b Q8_0 | +0.204 | −1.360 | **+1.56** | 1.87 |
| Qwen3-Reranker-0.6B Q8_0 (official) | +6.181 | −11.582 | **+17.76** | 12.07 |

No 1e-23 magnitude collapse on either — the official GGUF carries its scoring surface
(Qwen3-0.6B ties `lm_head` to `token_embd`, nothing to drop in conversion).

## Latency (the deliverable)

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

## Quality (decides the model, not the gate)

`rerank_batch_check` correctness oracle: both models pass both scenarios (top-8 overlap
8/8; rank shift ≤1 harrier, 0 qwen3; systematic bias ≤0.0016).

But ranking quality diverges hard on the prerank (title) scenario — query "How did
Heisenberg's uncertainty principle reshape philosophical debate about determinism?":

- qwen3-reranker top-3: *Uncertainty principle, Werner Heisenberg, Copenhagen
  interpretation* — on-topic.
- harrier top-3: *Wave function collapse, Great Barrier Reef, Surrealism* — relevance
  noise on short inputs (its +1.56 sanity separation is 11× weaker than qwen3's).

**Adopt the official Qwen3-Reranker GGUF; retire harrier as the working default.**

## Verdict for P3.3

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

## Artifacts

- `sovereign/crates/sovereign-inference/examples/rerank_pairs_probe.rs` (committed):
  sanity gate + 100-pair timing probe, fixture-driven.
- Fixtures: `data/chunks_{20,100}.jsonl` (gitignored; regenerate with
  `scripts/dump_chunks.py --seed 11|13`).
- Hygiene for D1 (from the plan, updated by this run): `rerank_smoke.rs:6,21` still
  points at nonexistent `jina-...-Q6_K.gguf`; `rerank_batch_check.rs` default path is
  now VALID (official GGUF symlinked); `sovereign-contracts/src/traits.rs:510-511`
  still claims a `[rerank]` models.toml section that doesn't exist.
