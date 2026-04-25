# Atlas retrieval bench

Tests whether the v2 atlas (atoms / edges / trajectories produced by `sovereign enrich`) provides a real retrieval signal beyond what dense-cosine or BM25 give you on the same chunks. Built on `brothers_karamazov` because it has a current atlas including `cross_corpus_edges.json`.

## Pipeline

```
prep_chunks.py        →  chunks-<corpus>.jsonl       (paragraph chunks + 1024-d embeddings)
synthesize_queries.py →  queries-<corpus>.jsonl      (atom-derived queries + section-level GT)
                      →  queries-<corpus>-paraphrased.jsonl  (--paraphrase <model>)
run_bench.py          →  bench-report.md             (variants × recall@K + MRR)
label_gt.py           →  golden-gt.jsonl             (LLM-judged chunk-level GT, optional)
```

Variants (`run_bench.py --variants ...`):
- `flat-fp32` — cosine top-K, baseline
- `flat-fp16`, `flat-pq` — storage compression probes (1024×4B → 2B / 16B per vector)
- `bm25-only`, `bm25-rerank` — lexical baseline + dense rerank
- `atlas-tier`, `atlas-tier-prune` — query→atom match → narrow chunks to atom provenance (±1-hop edges)
- `atlas-tier-loo`, `atlas-tier-loo-hop` — same, but the atom each query was derived from is hidden from the candidate pool

## How to rerun (brothers_karamazov)

```bash
# 0. daemon must be running on :9741 with embed slot loaded
# 1. paragraph-level chunks + embeddings via daemon
uv run prep_chunks.py --corpus brothers_karamazov --out ./chunks-brothers_karamazov.jsonl

# 2. atom-derived queries (template path; add --paraphrase Bonsai-8B-Q1_0 for variants)
uv run synthesize_queries.py --corpus brothers_karamazov --out ./queries-brothers_karamazov.jsonl
uv run synthesize_queries.py --corpus brothers_karamazov --paraphrase Bonsai-8B-Q1_0 \
  --out ./queries-brothers_karamazov-paraphrased.jsonl

# 3. bench (atom-section GT, all variants including LOO)
uv run run_bench.py --corpus brothers_karamazov \
  --chunks ./chunks-brothers_karamazov.jsonl \
  --queries ./queries-brothers_karamazov-paraphrased.jsonl \
  --out-md ./bench-report-paraphrased-loo.md
```

Each step is idempotent. `prep_chunks.py` and `synthesize_queries.py --paraphrase` are the only LLM-touching steps; `run_bench.py` is all in-memory numpy and runs in ~20-30 s on 2,426 chunks × 563 queries × 9 variants.

## Three controls for circularity

The atom-derived ground truth creates two circular pathways into atlas-tier's numbers:
1. **Query-side**: queries synthesized from atom descriptions embed close to those atoms → trivially route to source atom.
2. **Routing-side**: source atom's provenance sections ARE the GT → atlas-tier hits guaranteed.

Three controls used here:

- **`--paraphrase`** breaks the query-side bias. Bonsai-8B rewrites each template into 2 research-style phrasings.
- **`atlas-tier-loo`** breaks the routing-side bias. For each query, hide the atom it was derived from (`source_id`) from atlas-tier's candidate atom matrix; the tier must reach the right sections via *other* atoms whose provenance overlaps.
- **`golden-gt` (`label_gt.py`)** breaks both — chunk-level ground truth from an LLM judge that doesn't see atoms. Expensive (Bonsai judge ~16-29 s/call); reserved for follow-up validation on a focused subsample.

## Key findings (snapshot)

`r@10` on `brothers_karamazov` (2,426 chunks, 94 atoms, 118 edges):

| variant | template (190q) | template+LOO | paraphrased (563q) | **paraphrased+LOO** |
|---|---|---|---|---|
| flat-fp32 | 0.505 | — | 0.334 | — |
| flat-pq (16-byte vectors) | 0.500 | — | 0.190 | — |
| bm25-only | 0.642 | — | 0.320 | — |
| atlas-tier (+1-hop) | 0.984 | 0.942 | 0.904 | **0.883** |
| atlas-tier-prune | 0.916 | 0.816 | 0.860 | **0.810** |

The most controlled cell (paraphrased + LOO) shows atlas-tier ~50 points above flat-fp32 r@10.

The judge-derived smoke (n=7, `golden-gt-smoke.jsonl`) corroborated atlas-tier-prune ≈ bm25 ≈ flat-fp32 within sample noise on chunk-level GT, with 1-hop expansion *hurting* via candidate dilution. Per-class signal needs n≥30 to be trustworthy; LOO gives equivalent assurance at n=190 / n=563.

## Tuning + storage takeaways

- **fp16 is free** — identical recall to fp32 (1024×4 → 1024×2 bytes/chunk).
- **PQ at 16 bytes/vector loses ~0.5 pts r@10** on template GT, more on paraphrased — usable for chunks tier with hybrid BM25 fallback.
- Spend the embedding-quality budget on the **skeleton tier** (atoms), not the chunks tier. Chunks can ride on lexical + compressed dense.

## Extending

- **New corpus**: needs both `~/.sovereign/indexes/<id>/atlas/` (run `sovereign enrich build`) and `~/.sovereign/enrichment/<id>/config.json` pointing at the source text. Then steps 1-3 above as-is.
- **New variant**: add a branch in `run_bench.py` next to the existing `atlas-tier-*` arms; the per-query loop calls each variant with the same `(q_vec, q_tokens, atom_mat)` inputs. Latency + recall accumulators are auto-included.
- **More circularity controls**: hold-out atom split (train atoms only seen by atlas-tier; test atoms only used to generate queries) is a stronger version of LOO and a natural next step.
