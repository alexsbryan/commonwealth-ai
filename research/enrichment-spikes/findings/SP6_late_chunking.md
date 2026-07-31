# SP6 — Late chunking on the 0.6B embedder: memory + recall?

**VERDICT: G6 answered on all three deliverables. (1) Binding: token-level reads
WORK on llama-cpp-4 0.4.2 — the 0.2.x null-buffer failure is gone. (2) Memory:
peak process RSS 7.1 / 12.8 / 24.4 GB at W = 8k / 16k / 32k. (3) Recall:
hit@5 = hit@10 = 1.000 for every arm (golden saturates at article granularity);
MRR 0.953 status quo vs 0.961–1.000 late — late chunking fixed the ~2 of 17
queries whose top-1 chunk was off-article, at 1.4–2.9x embed wall-clock.
Recommendation: DEFER the P2.4 late-chunking follow-on — no demonstrated recall
gain that pays for the memory, embed-time, and offsets-plumbing costs; re-open
trigger below.**

## Question (sizing doc §1, gate G6)

Can `qwen-embedding-0.6b` produce token-level (unpooled) embeddings over long
windows on our vendored binding, at what memory, and does post-pooled per-chunk
embedding beat status quo on a small recall golden? Not pass/fail — binding
verdict + ceiling + hit@k delta recorded. Gates only the P2.4 late-chunking
follow-on (go/defer).

Honesty rule (pre-registered): the status-quo baseline is LAST-token-pooled
chunks — the GGUF's `qwen3.pooling_type = 3` — embedded through the same
gguf-native pooled path production uses. The late arm is compared against that,
with a last-token-per-span variant alongside mean-per-span.

## Binding verdict — the headline

**Token-level reads WORK on the vendored llama-cpp-4 0.4.2** (llama.cpp b9982).
`with_pooling_type(LlamaPoolingType::None)` + all-logits batch +
`embeddings_ith(i)` returns a distinct, non-null 1024-dim vector per token
(probe: 11 tokens, norms 103.1–123.1, all distinct). The prior failure —
`embeddings_ith` returning a null buffer under pooled layout on the bundled
0.2.x binding (embed_slot.rs:178-187 history comment) — does not reproduce on
0.4.2. The RE-TEST the plan called for is answered: the binding is not the
blocker anymore.

## Method actually run

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

## Span location — the offsets-plumbing finding

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

## Numbers

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

## Exit-criterion verdict

**Met.** Gate G6 asked for binding verdict + memory ceiling per window + hit@k
delta recorded, explicitly not pass/fail. All three are recorded above with the
exact commands. The plan's RE-TEST instruction on the 0.4.2 binding is
answered: works.

## Size/confidence update + go/defer recommendation

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
