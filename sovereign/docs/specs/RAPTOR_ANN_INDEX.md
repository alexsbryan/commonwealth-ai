# RAPTOR Summary-Node ANN Index

**Purpose.** Replace the brute-force cosine scan in `apply_raptor_grounding` with an approximate-nearest-neighbour (ANN) index over RAPTOR summary embeddings, so raptor grounding scales past SEP's ~11k nodes to wiki-scale corpora (100k–500k nodes). Hand this to the engineer who picks up the work; it's a contract — keep it in sync with the implementation in the same PR.

**Scope.** Almost everything lands on substrate that already exists: LanceDB (already drives leaf-chunk vector search in corpus-engine), the `conv_raptor_nodes` SQLite table (source of truth, already built by `enrich raptor`), and the `apply_raptor_grounding` wire-in (already env-gated + late-injected, shipped 2026-06-08). The genuinely *new* pieces are: a derived `raptor_summaries.lance` table per corpus, a `build_raptor_index` step, a `search_raptor_summaries` query primitive, and the freshness gate. The [landing map](#landing-map--new-vs-reuse) calls out new vs reuse.

**Status.** Phase 1 **SHIPPED 2026-06-08**. Built per this contract: `corpus-engine/src/index/raptor.rs` (pure-LanceDB `RaptorSummaryRow`/`RaptorHit`/`build_raptor_index`/`search_raptor_summaries` + `raptor_summaries.meta.json` sidecar), `CorpusEngine::search_raptor_summaries`/`raptor_index_meta` accessors, `sovereign-tools/src/raptor_index.rs::build_corpus_raptor_index` (the `conv_raptor_nodes` read→map→build glue), the `apply_raptor_grounding` index-fast-path + scan fallback + `max(created_at)` freshness gate (`corpus_raptor_version` on `ConvTieredReader`), and the `enrich raptor` auto-hook + standalone `enrich raptor-index <corpus>` verb. Parity / unit / freshness tests green. **One deviation** — see [decision 4](#decisions): the injected score is the EXACT cosine recomputed from the stored embedding, not `1 − _distance`. Still a **scaling prerequisite, not a current blocker** — it changes throughput, not answers — see [When is this needed](#when-is-this-needed).

---

## Background — why

RAPTOR collapsed-tree grounding is on by default (`SOVEREIGN_RAPTOR_GROUNDING`, late-injected per the `feat(retrieval): late RAPTOR injection` commit). It injects a corpus's top-M summary nodes as virtual chunks so a query can match a whole-document/section SUMMARY, not just leaf chunks. Measured value on the SEP summarization banks (Darwin-36B): **+8–10pt answer theme-coverage on obscure works**, ~2× context coverage; QA-neutral via late injection.

`apply_raptor_grounding` computes relevance by **brute-force cosine over every summary node** in the queried corpus:

```
list_corpus_raptor_nodes(corpus_id, min_level)   // full SQLite table scan, decode every BLOB
  → for each node: cosine(query_emb, node.summary_embedding)   // O(N · dim)
  → sort, top-M
```

This is `O(N_nodes · 1024)` per query, **no index**. Measured cost (SEP, 11k nodes): ~1.1s in *debug*, est. ~55–110ms in release. At wiki-scale (100k–500k nodes) the release scan is ~0.5–1s+ **per query**, paid on every query because raptor is default-on. That's the wall this spec removes.

### When is this needed

| corpus | nodes | est. release scan | verdict |
|---|---|---|---|
| SEP | ~11k | ~80ms | fine as-is, no index |
| (threshold) | ~30–40k | ~250ms | index starts paying off |
| wiki-scale | 100k–500k | 0.7–3.5s | **index required before enabling** |

> ⚠️ The release scan cost is an *estimate* (debug→release extrapolation, ~10–20×). **First task of the implementing session: measure it.** Release-build, run one timed SEP eval with `SOVEREIGN_RAPTOR_GROUNDING=1`, read the retrieval (`wall − rt`) delta vs `=0`. That pins the real node-count threshold where the scan crosses ~250ms and sets how urgent this is. ~10 min.

**Do not build this speculatively.** Build it when a target corpus's raptor tree approaches the threshold, OR as the explicit prerequisite for a "raptor on wiki" rollout.

---

## Current state (anchors)

| thing | location | shape |
|---|---|---|
| the scan to replace | `sovereign/crates/sovereign-core/src/runtime/retrieval/raptor_grounding.rs:99` (`apply_raptor_grounding`; moved here by the `runtime/retrieval.rs` module split) | reads `self.conv_tiered_reader.list_corpus_raptor_nodes(corpus_id, min_level)`, cosines each, sorts, top-M, dedupe-by-`conv_uuid` (opt-in `SOVEREIGN_RAPTOR_DEDUPE`) |
| SQLite source of truth | `conv_raptor_nodes` table, `sovereign/crates/sovereign-store/src/migrations.rs:569` | cols: `node_id` PK, `corpus_id`, `conv_uuid`, `level`, `summary` TEXT, `summary_embedding` BLOB (LE f32), `centroid_embedding`, `…`. Index `idx_conv_raptor_nodes_conv_level (corpus_id, conv_uuid, level)` |
| row struct | `ConvRaptorNodeRow`, `sovereign/crates/sovereign-core/src/conv_tiered.rs:53` | `summary_embedding: Vec<f32>` (1024-dim, Qwen3-Embedding-0.6B) |
| BLOB codec | `encode_f32_vec` / `decode_f32_vec`, `sovereign/crates/sovereign-store/src/sqlite.rs` | LE f32 bytes |
| build/insert path | `build_raptor_rows`, `sovereign/crates/sovereign-tools/src/conv_tiered_provider.rs:519`; persisted via `save_conv_raptor_nodes` (`sqlite.rs:2766`) called at `conv_tiered_provider.rs:379` | **hook point for index build is right after this save / as a post-ingest batch pass** |
| **reuse: leaf ANN search** | `CorpusIndex::search`, `corpus-engine/src/index/search.rs:98` | `table.query().nearest_to(q_emb).nprobes(50).limit(k)`; **flat-scan fallback for <10k rows** at `:112` |
| **reuse: ANN index build** | `build_vector_index_with_progress`, `corpus-engine/src/index/create.rs:57` | `table.create_index(&["embedding"], Index::IvfPq(IvfPqIndexBuilder::default().num_partitions(p).distance_type(Cosine))).replace(true)` |
| precedent: same problem, unsolved | `apply_atlas_grounding`, `retrieval/atlas_grounding.rs:75` | also brute-force in-memory cosine over `AtlasContext.entries` (`atlas_context.rs:34`), loaded per-corpus at startup by `AtlasContextManager` (`sovereign-tools/src/atlas_context_manager.rs`), disk-cached `atlas/atoms.embeddings.bin`. **See [decision 2](#decisions).** |

**No HNSW/FAISS/Annoy anywhere in the workspace** — LanceDB IVF is the only vector-index tool, and it's already proven for leaf chunks. Reuse it; do not add a dependency.

---

## Design

A per-corpus **`raptor_summaries.lance`** table, derived from `conv_raptor_nodes`, queried by the same LanceDB primitive leaf search uses. Kept **separate** from `chunks.lance` so raptor retrieval stays a distinct top-M (mixing them into the leaf index would defeat the whole point — leaf retrieval would then surface summaries organically and re-introduce the displacement we engineered out).

```
enrich raptor  ──writes──▶  conv_raptor_nodes (SQLite, source of truth)
                                   │
                          build_raptor_index (NEW, batch)
                                   ▼
                          raptor_summaries.lance  ──IVF/Flat index──┐
                                                                     │
query ──embed──▶ apply_raptor_grounding ──▶ search_raptor_summaries(corpus, q_emb, M, min_level)
                          │  (NEW path)            nearest_to().only_if(level>=N).limit(M)
                          └─ fallback: list_corpus_raptor_nodes scan   (when no index built)
```

### Table schema (`raptor_summaries.lance`)

| column | type | source |
|---|---|---|
| `node_id` | string | `ConvRaptorNodeRow.node_id` |
| `conv_uuid` | string | `.conv_uuid` (slug derived at read time, as today) |
| `level` | int32 | `.level` (for `min_level` filter) |
| `summary` | string | `.summary` (the virtual chunk's content) |
| `embedding` | fixed-size-list<float32, 1024> | `.summary_embedding` (name it `embedding` to match the leaf-index convention so `create_index(&["embedding"], …)` is uniform) |

Everything `apply_raptor_grounding` needs to build the virtual `ScoredChunk` (content=summary, title=slug-from-conv_uuid, url/source_doc_id=conv_uuid, `metadata.source="raptor"`, `raptor_level`) is present. No need to copy `centroid_embedding` or the JSON columns — they're not used by grounding.

### Query primitive

```rust
// corpus-engine, e.g. src/index/raptor.rs
pub struct RaptorHit { pub node_id: String, pub conv_uuid: String,
                       pub level: i64, pub summary: String, pub score: f32 }

pub async fn search_raptor_summaries(
    &self, corpus_id: &str, query_emb: &[f32], top_m: usize, min_level: i64,
) -> Result<Vec<RaptorHit>>
//  open raptor_summaries.lance for corpus_id
//  .query().nearest_to(query_emb).only_if(format!("level >= {min_level}"))
//          .nprobes(50).distance_type(Cosine).limit(top_m).execute()
//  map rows → RaptorHit (score = 1 - cosine_distance, to match current cosine semantics)
```

`SOVEREIGN_RAPTOR_DEDUPE` (dedupe-by-`conv_uuid`) stays a **post-query** step in `apply_raptor_grounding` — M is tiny. To keep M distinct works after dedupe, over-fetch (`limit(top_m * K)`) then dedupe-then-truncate. (Same semantics as today's sort→retain→truncate.)

### Wire-in (`apply_raptor_grounding`)

Replace the `list_corpus_raptor_nodes` + manual-cosine block with:

```rust
let hits = match corpus_engine.search_raptor_summaries(corpus_id, embedding, fetch_m, min_level).await {
    Ok(h) if !h.is_empty() => h,
    _ => /* FALLBACK: existing list_corpus_raptor_nodes scan + cosine */,
};
```

**Keep the scan as fallback** so a corpus whose index hasn't been built yet (or failed) still works — degraded throughput, not broken. The Runtime already holds a `CorpusEngine` (leaf search); thread `search_raptor_summaries` through the same handle.

### Build + freshness

- **`build_raptor_index(corpus_id)`** (corpus-engine or sovereign-tools): read all rows via the existing `list_corpus_raptor_nodes(corpus_id, 0)`, write to `raptor_summaries.lance`, `create_index` (see [decision 1](#decisions) for index type), stamp a build-version in table metadata = `max(created_at)` (or row count) of the source rows.
- **Trigger:** auto after `enrich raptor` finishes for a corpus (in `conv_tiered_provider.rs` after the save loop), **plus** a re-runnable `sovereign enrich raptor-index <corpus>` escape hatch. Mirrors the `build_structural_atlas` post-install hook.
- **Freshness gate:** on open, compare table's stamped build-version against `SELECT max(created_at) FROM conv_raptor_nodes WHERE corpus_id=?`; if stale → rebuild (or fall back to scan + log). Reuse the mtime/version-cache discipline from the `installed_indexes` cache (perf commit 2026-06-08).

---

## Landing map — new vs reuse

**New:**
- `raptor_summaries.lance` table + schema.
- `build_raptor_index(corpus_id)` + the `enrich raptor-index` CLI verb + the auto-hook after `enrich raptor`.
- `search_raptor_summaries` query primitive (corpus-engine).
- Freshness gate (build-version stamp + staleness check).
- The fallback branch in `apply_raptor_grounding`.

**Reuse (do not reinvent):**
- LanceDB IVF index build (`create.rs:57` pattern) and vector search (`search.rs:98` pattern).
- `conv_raptor_nodes` (source of truth, unchanged) + `list_corpus_raptor_nodes` (now the fallback + the build's reader).
- Slug-from-`conv_uuid` + virtual-`ScoredChunk` construction (unchanged in `apply_raptor_grounding`).

---

## Decisions

1. **IVF-FLAT vs IVF-PQ — accuracy vs memory.** PQ (product quantization) is *lossy*; for top-8 whole-doc-summary retrieval a quantization miss drops a relevant summary, hurting the exact thing raptor adds. **Default to IVF-FLAT (exact within nprobes partitions, no PQ loss) wherever memory allows** (~100k nodes × 1024 × 4B ≈ 400MB). Switch to **IVF-PQ only at wiki-scale** (memory pressure), gated by a recall-validation check (below). LanceDB also flat-scans <10k automatically, so small corpora need no index at all.

   **SHIPPED (per the implementing user's call): no-index-under-30k (exact flat `nearest_to`) + IVF-PQ above.** `FLAT_SCAN_THRESHOLD = 30_000` in `index/raptor.rs` — raised above the leaf 10k because RAPTOR *summary* nodes are far fewer than leaf chunks, so the ~30–40k pay-off point keeps **every current corpus** (SEP's ~11k summary nodes included) on the exact flat path, with IVF-PQ reserved for genuinely wiki-scale trees. **IVF-FLAT is not used** — its availability in lancedb 0.27 is unverified, and the under-threshold flat scan already delivers the exactness IVF-FLAT would buy. Because the score is recomputed exactly from the stored embedding ([decision 4]), even the IVF-PQ path returns exact scores (only its *candidate selection* is approximate, the recall gate covers that).
2. **Raptor-specific now vs shared raptor+atlas abstraction.** `apply_atlas_grounding` has the *identical* brute-force scan (in-memory cosine over `AtlasContext.entries`). A shared "embedding ANN index" abstraction could retire both scans — cleaner, but pulls atlas (its graph-walk path, its disk-cache, its startup manager) into scope. **Recommendation: ship raptor-specific first**, but shape `search_raptor_summaries` / the table builder generically enough that atlas can adopt it as Phase 2. Do not block raptor on the atlas refactor.
3. **Build trigger:** auto-post-`enrich raptor` (default, always-fresh) + explicit `enrich raptor-index` (re-index escape hatch). Not lazy-on-first-query (first-query latency spike is worse than a build step).
4. **Score semantics — SHIPPED with a correction.** The plan was `score = 1 − cosine_distance` read straight off LanceDB's `_distance`. The parity test caught that this `_distance` (LanceDB's cosine kernel) carries **~5e-3 error on near-parallel vectors** — enough to perturb both the score and the top-K boundary ranking. **Shipped:** `nearest_to` is used only as the candidate generator; `search_raptor_summaries` recomputes the EXACT cosine from the returned `embedding` column (mirroring the leaf path's `cosine_distance_from_fixed_list` in `index/search.rs`) and re-sorts, so the injected score is bit-comparable to `crate::atlas_context::cosine`. The parity test asserts exact top-K set-equality + score match (<1e-5) against the brute-force scan.

---

## Test plan

Per house practice (E2E tests prove correctness):

1. **Parity (the load-bearing test):** on the SEP corpus, `search_raptor_summaries` top-M must return the **same node set** (and same order, modulo ANN approximation) as the brute-force `list_corpus_raptor_nodes` + cosine for a battery of query embeddings. With IVF-FLAT this should be exact; assert ≥ 0.95 set-overlap @ M=8 (tolerate rare ANN reorderings).
2. **Bench parity:** re-run `sovereign/bench/sep/summarize.toml` + `questions.toml` A/B with the index path vs the scan path — sources/judge deltas must be within noise (the index is a *speed* change, not a *quality* change). Baselines: off 85/87, late 86/92 (from the late-injection commit).
3. **Unit:** `build_raptor_index` round-trips N rows; `level >= min_level` filter; dedupe-by-`conv_uuid` over-fetch; empty-corpus + missing-index → fallback path fires.
4. **Recall gate (only if IVF-PQ is used):** measure top-8 recall of IVF-PQ vs exact on a held-out query set; require ≥ 0.9 before allowing PQ for a corpus.
5. **Freshness:** mutate `conv_raptor_nodes` (add a node), assert the staleness check triggers a rebuild (or fallback).

---

## Phasing

- **Phase 0 — done.** SEP always-on on the brute-force scan (fine at 11k).
- **Phase 1 — this spec.** Raptor-specific LanceDB index + fallback + freshness. ~300–450 LOC + tests. Unblocks one large corpus. **Start with the release-latency measurement to confirm urgency + the threshold.**
- **Phase 2 — optional.** Generalize to a shared raptor+atlas ANN abstraction (decision 2).

## Open questions

- Exact LanceDB `only_if` filter syntax for the `level` column on the installed version (0.27) — confirm `int32` filter pushdown works with `nearest_to` (vs post-filtering, which would under-fill `limit`).
- Where `raptor_summaries.lance` lives on disk relative to the corpus's `chunks.lance` (same corpus dir? a `raptor/` subdir?) — pick to match `installed_indexes` discovery so the daemon's index walk finds/ignores it correctly (and so the `CorpusKind::Code` filter from the 2026-05-19 fix doesn't misclassify it).
- Does the freshness stamp belong in LanceDB table metadata or a sidecar `raptor_summaries.meta.json` (mirroring `_corpus_meta.json`)?

## References

- Late-injection + default-on commit: `feat(retrieval): late RAPTOR injection, on by default` (2026-06-08).
- Throughput finding + the brute-force-scan caveat: doc-comment at `apply_raptor_grounding` (`retrieval/raptor_grounding.rs:99`).
- Leaf-chunk ANN reference impl: `corpus-engine/src/index/search.rs:98`, `create.rs:57`.
- Atlas precedent (same unsolved scan): `apply_atlas_grounding` `retrieval/atlas_grounding.rs:75`, `atlas_context.rs:34`.
