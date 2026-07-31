# P5.2b — Multi-vector MaxSim Lance sibling-table prototype (G10)

**VERDICT: G10 answered (report-only). The pinned lancedb 0.27.2 / lance 4.0
holds a ColBERT-shape multivector column natively and scores MaxSim correctly
(planted-row rank 1 at every scale). Storage at the design shape measures
56.1 KB/row f32 → 10.5 GB f32 / ≈2.6 GB int8 at the 188k-chunk SEP pilot —
consistent with RETRIEVAL_REDESIGN.md:261-266's 3–6 GB sizing. Exact
brute-force MaxSim scales linearly (~2.0 s/query at 188k — offline/bench
viable, not interactive); IVF-PQ works end-to-end but approximates MaxSim
poorly at defaults (top-10 overlap 0.12–0.23 vs brute-force,
nprobes-INSENSITIVE) and is rescued only by `refine_factor` (0.61–0.66 at
rf=10, ~64 ms/query at 60k). No production plumbing was needed beyond the
documented sibling-table pattern.**

Measured 2026-07-31, M2 Max, release build (timing is the measurand — the SP4
exception; `[profile.dev] debug=0` compiles lance unoptimized). Harness:
`corpus-engine/examples/maxsim_probe.rs` (committed). Raw logs:
`runs/p52b/probe-{20k,20k-clustered,20k-rf,60k}.log`.

## Question (gate G10)

Copy the sibling-table pattern (`raptor_index.rs` precedent:
`<corpus>/raptor_summaries.lance` + meta sidecar, invisible to
`installed_indexes()`, brute-force under 30k rows); is LanceDB native
multivector/MaxSim a viable storage mechanism, at what storage + query cost,
vs the recorded ~3–6 GB / 188k chunks sizing?

## Method (exact commands)

```
cargo build --release -p corpus-engine --features treesitter --example maxsim_probe
./target/release/examples/maxsim_probe research/enrichment-spikes/runs/p52b 20000 128 20 10
./target/release/examples/maxsim_probe research/enrichment-spikes/runs/p52b 60000 128 20 10
```

Synthetic vectors at the documented design shape — 128d f32, 60–160 token
vectors/row (mean ~110 ≈ 200-token chunk after 50% pooling), 32-token
queries. Rows draw token vectors around 256 topic centroids (weight 0.7) —
uniform random vectors are pathological for IVF (all centroids equidistant)
and were measured first by mistake: overlap 0.01 (see honesty note below).
Row 0 is planted to contain query 0's exact vectors, verifying ranking
semantics. Column type `List<FixedSizeList<Float32, 128>>`; query via
`.nearest_to(v0)` + `.add_query_vector(v_i)` per token
(lancedb `table/query.rs` concatenates into the multivector plan).

## Numbers

| Metric | 20k rows | 60k rows | 188k (extrapolated) |
|---|---|---|---|
| write | 2.4 s (8.4k rows/s) | 6.5 s (9.3k rows/s) | ~21 s |
| disk (f32) | 1.12 GB | 3.37 GB | **10.5 GB** (int8 ≈ **2.6 GB**) |
| brute-force MaxSim, mean/query | 237 ms | 645 ms | ~2.0 s |
| IVF-PQ build | 3.3 s | 9.1 s | ~30 s |
| indexed query (default nprobes) | 18.5 ms | 31.7 ms | ~100 ms |
| indexed query (nprobes=64, rf=10) | 34.4 ms | 63.5 ms | ~200 ms |
| top-10 overlap vs brute-force (default) | 0.23 | 0.12 | — |
| top-10 overlap (nprobes=64, no rf) | 0.23 | 0.12 | — |
| top-10 overlap (nprobes=64, **rf=10**) | **0.66** | **0.61** | — |

Planted-row check: rank 1 with distance −31.0 (= −Σ max-cos over 32 exact
query tokens) at both scales, indexed and flat — MaxSim semantics are native
and correct. Disk ratio vs raw f32 payload: 1.00 (no format overhead at this
shape).

## Interpretation

- **Storage: viable, and the recorded sizing is confirmed.** The int8 path
  (2.6 GB at pilot scale) is the design point; f32 was what was measured —
  int8/f16 encoding on a multivector column is untested here and is the
  first thing productionization must verify.
- **Exact MaxSim is an offline/bench tool at pilot scale, not interactive.**
  Linear scan cost ~10.6 µs/row-pair-set; the raptor_index stance
  ("brute-force under 30k rows") transfers: a ≤30k-chunk corpus gets exact
  MaxSim in ≲350 ms.
- **IVF-PQ recall is the open risk, and it is knob-shaped, not wall-shaped.**
  Overlap being nprobes-insensitive but refine_factor-sensitive localizes
  the loss to PQ quantization of token vectors (XTR-style partial scoring),
  not partition misses. rf=10 buys 0.6 overlap at ~3.5x the indexed latency
  — still 10x faster than flat at 60k.
- **Honesty note:** overlap numbers are on SYNTHETIC clustered vectors.
  Real-embedding recall (per RETRIEVAL_REDESIGN option (c):
  answerai-colbert-small / GTE-ModernColBERT vectors) must be re-measured
  before any adoption decision; the first uniform-random run scored overlap
  0.01 — vector distribution dominates this measurement, so treat 0.6 as
  "tunable, order-of-magnitude", not a recall promise.

## Consequences (evidence for post-T2 P5/P3 re-planning; no commitment)

- The storage layer for late-interaction rerank (RETRIEVAL_REDESIGN (c)) is
  a solved problem on the pinned stack — sibling table + native multivector
  column, zero new dependencies, ~200-line writer.
- The retrieval-quality question is now the ONLY question: real token
  vectors + refine_factor sweep + int8 encoding, all of which the committed
  harness can measure in minutes once an encoder produces vectors (SP6's
  token-level embedding path is one candidate producer; a dedicated ColBERT
  encoder is the design-intent one).
- Interactive use at 188k-chunk scale requires the index; budget ~200
  ms/query at rf=10 — comparable to SP4's full-chunk rerank budget (~470 ms
  top-20), so a MaxSim stage does not obviously beat the cross-encoder on
  latency; it competes on candidate breadth (whole-corpus vs top-20).

Exit criterion (report-only storage + query numbers vs the recorded sizing):
**met**.
