# SP5 — Noun-phrase extraction + Leiden in Rust: adopt or write?

**VERDICT: G5 PASS — 5.2 s wall for 10k chunks (gate < 300 s, 57x headroom); 17/20
sampled communities eyeball-cohere. Adopt `leiden-rs` for community detection;
write the noun-phrase/co-occurrence layer ourselves (it is ~250 lines and the
probe IS the first draft). P2.2 confidence Med → High.**

## Question (sizing doc §1)

Noun-phrase extraction + Leiden/Louvain in Rust — adopt or write? Exit: concept
graph for 10k chunks < 5 min CPU; communities eyeball-cohere.

## Crate survey (2026-07-31)

| Crate | Version | License | Verdict |
|---|---|---|---|
| `leiden-rs` | 0.8.1 (2026-05-15) | MIT/Apache-2.0 | **ADOPT.** Core is dependency-tiny (rand, rustc-hash, thiserror), takes a CSR edge list directly (`GraphDataBuilder`), 4 quality functions, seedable. Its optional petgraph adapter wants petgraph ^0.8 (we pin 0.6) — skip the adapter, feed edges directly. Caveat for P2.2 productionization: repo hosted on gitcode.com (mirror), 9.2k downloads — vendor or pin-audit before production use. |
| `graphrs` | 0.11.16 (2025-12) | MIT | Louvain + Leiden but drags its own graph type + quick-xml/rayon/serde tree. More surface than needed. |
| `single-clustering` | 0.6.1 | non-standard license | Excluded on license alone. |

Hand-rolled Louvain (~200 lines) remains a viable fallback if the provenance
caveat ever bites; the probe's clean seam (edge list in, partition out) makes the
swap trivial.

## Method actually run

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

## Numbers (debug build, M2 Max, single-threaded)

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

## Eyeball verdict (top-20 communities by size, runs/sp5/communities_r2.txt)

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

## Consequences for P2.2

- **Adopt-or-write answer: both, split by layer.** Communities: adopt leiden-rs
  (with the vendoring caveat above). NP extraction + graph build: write —
  it is small, the motif machinery precedent covers the hard parts, and no
  surveyed crate provides it.
- P2.2 size unchanged (L 10-15d), confidence Med → **High**. The
  "entity-co-occurrence-only" fallback (G5 on-failure branch) is NOT needed.
- Real remaining work for production P2.2: incremental updates (the probe is
  batch), concept labeling, and cross-corpus df calibration — none of which the
  spike question covered.
