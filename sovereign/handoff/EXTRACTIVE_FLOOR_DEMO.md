# Extractive Floor Demo — summaries that cannot make things up

The user-facing win (T1 plan §Demonstrating value, P1.1 seam): the
knowledge tier can now build its summary trees from **verbatim source
sentences** — no LLM prose anywhere in the tree — and every abstractive
build now **falls back to extractive per cluster** instead of silently
thinning the tree when a summary call fails (2026-07-31).

SP2 measured the license to do this: extractive and abstractive trees
scored **identically** on both retrieval benches (|B−A′| = 0.0000 at
equal coverage). Fluent prose is not what retrieval quality comes
from; the sentences themselves are. That makes verbatim extraction a
floor we can stand on, not a downgrade.

---

## The run

```
$ sovereign enrich raptor chaos-secret-agent --doc-type narrative \
    --summary-mode extractive --force

  summaries:  Extractive
  [1/1] the-secret-agent-a-simple-tale  316 chunks · 19 nodes · 91.3s
```

91 seconds for a whole novel, embed-only — no chat model needs to be
resident at all. Every node's summary is source sentences chosen by
cosine-to-centroid, re-joined in document order, and stamped:

```
$ sqlite3 sovereign.db "select distinct prompt_version, summarizer_model
    from conv_raptor_nodes where corpus_id='chaos-secret-agent'"
rex-2026-07-31.1|extractive
```

The provenance stamps (P1.3) understand modes. An extractive tree is
fresh under extractive config and stale under abstractive config —
switching a corpus between modes is one `--refresh-stale` run in
either direction, and only the affected trees rebuild:

```
$ ... --refresh-stale --summary-mode extractive
  documents fresh (stamps match, skipped): 1

$ ... --refresh-stale        # abstractive default
  stale: the-secret-agent-a-simple-tale — prompt rex-2026-07-31.1 != rpv-2026-07-31.1; rebuilding
```

## The fallback: no more silently thinner trees

Before today, a cluster whose summary LLM call failed (daemon hiccup,
grammar-parse failure) was **dropped** — the tree shipped with fewer
nodes and nobody was told. Retrieval coverage shrank invisibly. Now
that cluster gets an extractive node instead: verbatim sentences,
stamped `extractive` so `--refresh-stale` can retry the abstractive
summary later. A tree now always has all its nodes; the stamps say
which ones are floor-grade. Covered by unit test
(`abstractive_llm_failure_falls_back_to_extractive`).

## Honest caveats a product person should hear

- **Extraction inherits the corpus's furniture.** On the chaos corpus
  (built without `--strip-furniture`) one cluster's extractive summary
  is Project Gutenberg license text — because that cluster genuinely
  is license text. Extractive mode makes corpus hygiene visible
  instead of letting an LLM paper over it.
- **The default has not flipped.** Abstractive remains the default
  everywhere. The plan's default policy (extractive for memory
  corpora, abstractive for attached docs) flips only after the A/B on
  the summarize banks — that harness run is the next step of P1.1.
- **No speed win on this host today** — the resident 4B summarizes a
  novel in similar wall time. The wins are trust (a verbatim summary
  cannot hallucinate), independence (builds with no chat model
  loaded), and coverage (the fallback).

Cost: two constants, one enum, one field on the builder config;
extraction itself is ~100 lines reusing the existing embed + cosine
machinery. No new stores, no new knobs beyond `--summary-mode`.
