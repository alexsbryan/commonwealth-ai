# CorpusIndex::sample_embeddings(n) returns the FIRST n rows in scan order, not a random sample — biased for diffuse corpora

CorpusIndex::sample_embeddings(n) returns the FIRST n rows in scan order, not a random sample — biased for diffuse corpora

`CorpusIndex::sample_embeddings(n)` (corpus-engine `index/enrichment.rs`) is a
plain LanceDB `.limit(n)` with NO `ORDER BY` and NO randomization. It returns the
first `n` rows in table scan order (≈ ingest order), which is deterministic but
NOT a representative sample.

Consequence: any "corpus fingerprint" built from it (mean centroid, max-over-
sample, topic profile) is a biased slice for a large/diffuse corpus. On wikipedia
(1.9M chunks) the first 256 rows are whatever was ingested first, so a
mean/max-over-sample relevance signal misranks the corpus for specific queries.
It's fine for small focused corpora where the whole thing is on-topic.

If you need a true corpus-level relevance signal, do a real nearest-neighbor
query against the index (see `CorpusIndex::nearest_vector_distance`), not a
sample. See [[project_corpus_prefilter_signal_2026_07_13]].

---
