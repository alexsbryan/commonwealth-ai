# Embedding model comparison — `qwen-embedding-0.6b` vs `v5-small-retrieval`

Decision context: validating the embedding standard for v1 (Wikipedia + other corpora). Both are 1024-dim, both fine-tuned from **Qwen3-0.6B** (Qwen Team's instruction-tuned embedding head vs Jina's `Jina Embeddings v5 Text Small Retrieval` head).

## Test setup

- corpus: `brothers_karamazov` — 2,426 paragraph chunks, 94 atoms, 118 edges
- queries: **563** template + paraphrased (Bonsai-8B rewrites)
- GT: atom-section containment + leave-one-atom-out (LOO) for routing-side circularity control
- bench harness: `run_bench.py` (in-memory cosine + BM25, deterministic)

Three configurations bencheded:

| label | server | query input | passage input |
|---|---|---|---|
| **Qwen-0.6b** | daemon `:9741` (qwen-embedding-0.6b slot) | raw text | raw text |
| **v5 (no prefix)** | `llama-server :8090` (v5-small-retrieval-Q8_0) | raw text | raw text |
| **v5 (query:/passage:)** | same | `"query: " + text` | `"passage: " + text` |

The v5 prefix probe was added after observing v5 beat Qwen on top-1 but trail on recall depth. A direct similarity probe (`Who is Alyosha?` × Alyosha description) showed prefixes lifted the rel-vs-unrelated margin from +0.47 to +0.59, confirming v5 expects asymmetric prefixes.

## Headline (paraphrased + LOO controls)

Recall@10, all 563 queries — the metric that matters for K=10 brief input window.

| variant | Qwen-0.6b | v5 (no prefix) | v5 (prefixed) |
|---|---|---|---|
| flat-fp32 | 0.334 | 0.247 | **0.353** |
| bm25-only | 0.320 | 0.320 | 0.320 |
| bm25-rerank | **0.277** | 0.242 | 0.295 |
| atlas-tier | **0.904** | 0.854 | 0.787 |
| atlas-tier-prune | **0.860** | 0.755 | 0.760 |
| atlas-tier-loo | **0.810** | 0.723 | 0.732 |
| atlas-tier-loo-hop | **0.883** | 0.851 | 0.782 |

## Headline (r@1, MRR)

v5 has consistently sharper top-1 ranking; Qwen consistently wider recall depth.

| variant | metric | Qwen-0.6b | v5 (prefixed) | Δ |
|---|---|---|---|---|
| flat-fp32 | r@1 | 0.103 | **0.181** | +0.078 |
| flat-fp32 | MRR | 0.153 | **0.260** | +0.107 |
| atlas-tier | r@1 | 0.346 | **0.432** | +0.086 |
| atlas-tier | MRR | 0.509 | **0.533** | +0.024 |
| atlas-tier-loo-hop | r@1 | 0.330 | **0.421** | +0.091 |
| atlas-tier-loo-hop | MRR | 0.491 | **0.524** | +0.033 |

## What the numbers say

**v5 wins** (top-1 / sharp ranking):
- +7-9 pts r@1 across every dense variant. Sharper contrastive head; better at putting the single best chunk first.
- +0.02-0.10 MRR everywhere. Same story, expressed continuously.
- Marginal +2 pts r@10 on flat-fp32 — only when prefixes applied. Without prefixes v5 trails by ~9 pts; the prefix matters.

**Qwen wins** (recall depth at K≥5):
- −10 to −12 pts r@10 on the atlas-tier paths (primary production route). v5 narrows the candidate set well via top-atom match, but the within-section dense rerank trails.
- −10 pts r@5 on atlas-tier — the gap opens immediately past rank 1.

**Both equivalent**: BM25 (no embedding); BM25-rerank within ~3 pts.

## Why this happens

1. **v5 is asymmetric retrieval-tuned.** Jina v5 small retrieval was trained with a contrastive objective on query↔passage pairs with explicit task prefixes. It's optimized to land the best result at rank 1, at some cost to the long tail. Qwen-embedding-0.6b is a more general-purpose embedding head and produces flatter, broader similarity distributions.

2. **Atom display text is short identifier-style** (`Alyosha Karamazov | Alexei | a young man of nineteen | ...`), not paragraph prose. Even with the `passage:` prefix this is out-of-distribution for v5's training corpus. Qwen's more symmetric training handles short-vs-long without the mismatch.

3. **Domain.** Brothers Karamazov is literary narrative; v5 retrieval is plausibly tuned on factoid/web-search distributions (MS MARCO, BEIR, etc.). Wikipedia would be closer to v5's sweet spot — this bench may understate v5 on the corpus we care about most.

## Recommendation for v1

**Stay on `qwen-embedding-0.6b` for the v1 standard** — but with one open caveat.

The decisive evidence: atlas-tier is committed as the primary production path, and v5 trails by 10-12 pts r@10 on every atlas variant. The brief input window is K=10; what matters is having relevant chunks anywhere in that window, not just at rank 1. Qwen wins that metric on every dense variant we benched.

The +2 pts flat-fp32 win for v5 (with prefixes) doesn't compensate, especially since flat-fp32 is the *secondary* parallel-merge source, not the primary path.

**Operational risks of switching to v5:**
- Asymmetric prefix discipline must be enforced everywhere: queries get `query: `, chunks get `passage: `, atoms get... unclear. Prefix bugs silently degrade recall by ~10 pts (see "no prefix" column).
- Atom display text is out-of-distribution for v5 — would want to evaluate alternative atom rendering before committing.

**Open caveat — Wikipedia-domain check**: Brothers Karamazov favors Qwen's general-purpose head; v5 may close or invert the gap on Wikipedia-style factoid corpora. If we want to be sure v1 doesn't leave recall on the table for the 400 GB corpus, a small Wikipedia sample bench (1k chunks, 100 queries) would resolve this in ~20 minutes of compute. Not on the critical path for v1, but a low-cost de-risking step.

## Reproducing

```bash
# Qwen-0.6b path (already cached as bench-report-paraphrased-loo.md)
uv run prep_chunks.py --corpus brothers_karamazov \
  --out chunks-brothers_karamazov.jsonl
uv run run_bench.py --corpus brothers_karamazov \
  --chunks chunks-brothers_karamazov.jsonl \
  --queries queries-brothers_karamazov-paraphrased.jsonl \
  --out-md bench-report-paraphrased-loo.md

# v5 path — runs against a separate llama-server, daemon untouched
llama-server -m /Users/alexsbryan/dev/commonwealth-ai/sovereign/models/v5-small-retrieval-Q8_0.gguf \
  --embeddings --port 8090 --host 127.0.0.1 -c 4096 &

uv run prep_chunks.py --corpus brothers_karamazov \
  --daemon http://127.0.0.1:8090 --embed-model v5 \
  --passage-prefix "passage: " \
  --out chunks-brothers_karamazov-v5-prefixed.jsonl

uv run run_bench.py --corpus brothers_karamazov \
  --chunks chunks-brothers_karamazov-v5-prefixed.jsonl \
  --queries queries-brothers_karamazov-paraphrased.jsonl \
  --daemon http://127.0.0.1:8090 --embed-model v5 \
  --query-prefix "query: " --passage-prefix "passage: " \
  --out-md bench-report-v5-prefixed-paraphrased-loo.md
```

Total wall-clock to reproduce both paths from scratch: ~6 min (3 min embed × 2, ~10 s bench × 2).
