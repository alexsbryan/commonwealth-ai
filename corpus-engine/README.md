# corpus-engine

A Rust library for building and searching local knowledge indexes from real-world data sources. It handles the full pipeline — acquire, extract, chunk, embed, index, search — with a clean public API and a minimal contract for distributed use.

```rust
let engine = CorpusEngine::new(recipes_dir, index_dir, embed_fn);
engine.ingest(&CorpusSpec::Builtin("wikipedia".into()), None).await?;

let index = engine.open_index(&index_dir.join("wikipedia")).await?;
let results = index.search(&query_embedding, "Ostrom design principles", 10).await?;
```

## What it is

`corpus-engine` turns a source like a Wikipedia dump, an OpenAlex JSONL export, or a directory of HTML files into a searchable local index. The index lives on disk as a LanceDB table and supports hybrid search — vector similarity via IVF-PQ plus keyword matching via Tantivy — in a single query.

It's the storage and retrieval layer for two downstream projects:

- **[Sovereign](../lcol-llm)** — a standalone AI application that indexes knowledge bases locally and reasons over them with a local LLM.
- **[Commonwealth](../commonwealth)** — a distributed inference mesh that shards knowledge indexes across peer nodes.

Both projects share one index directory on disk, and both interact with `corpus-engine` through the same public API. Neither depends on the other. `corpus-engine` knows nothing about either of them.

## Why LanceDB

The first version of this crate used SQLite with `sqlite-vec` and FTS5. At Wikipedia scale (6.8M chunks), `sqlite-vec` does a brute-force linear scan on every query — 200–400ms per search on a laptop. For a sharded fan-out across several peer nodes, that's the dominant cost in the whole pipeline.

LanceDB solves this with proper approximate nearest-neighbor indexing (IVF-PQ), disk-based columnar storage (Lance format), and native hybrid search via Tantivy. Expected query latency at Wikipedia scale drops from hundreds of milliseconds to under 15ms with >0.95 recall, and memory pressure stays low because vectors are memory-mapped from SSD rather than held in RAM.

It's embedded, Rust-native, disk-based by design, and the Lance 1.0 storage format is stable. No server process, no C++ bindings, no per-query RAM blowup.

## Architecture

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐     ┌──────────────┐
│  Acquirer   │ ──▶ │  Extractor   │ ──▶ │   Chunker   │ ──▶ │   Embedder   │
└─────────────┘     └──────────────┘     └─────────────┘     └──────────────┘
     ▲                    ▲                    ▲                    ▲
     │                    │                    │                    │
 bulk_download        mediawiki_xml        paragraph            EmbedFn
 local_file           stackexchange        sentence              (injected
 web_crawl            jsonl (openalex)     fixed                  by caller)
 api_paginated        html                 semantic
                      csv
                      parquet
                      plaintext

                                              │
                                              ▼
                                    ┌──────────────────┐
                                    │   CorpusIndex    │
                                    │                  │
                                    │   LanceDB +      │
                                    │   Tantivy FTS    │
                                    └──────────────────┘
                                              │
                                              ▼
                                    ~/.sovereign/indexes/
                                    ├── wikipedia/
                                    ├── openalex/
                                    └── stackexchange-shard-0-6200000/
```

Each stage is a trait implementation the engine dispatches to based on a **Recipe** — a TOML file describing the pipeline for a specific corpus.

### The `EmbedFn` injection point

`corpus-engine` never computes embeddings itself. It takes a closure:

```rust
pub type EmbedFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>>
        + Send + Sync
>;
```

- **Sovereign** passes a closure wrapping its local `InferenceProvider::embed()`, which runs a GGUF embedding model via llama.cpp.
- **Commonwealth** passes a closure that POSTs to a local `/v1/embeddings` HTTP endpoint.
- **Tests** pass a mock that returns zero vectors.

This keeps the crate free of any specific embedding runtime. You can plug in Candle, llama.cpp, an OpenAI API, or anything else.

## Public API

```rust
use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};

let engine = CorpusEngine::new(
    "~/.sovereign/recipes".into(),
    "~/.sovereign/indexes".into(),
    embed_fn,
);

// ── Ingestion ─────────────────────────────────────
engine.builtin_corpora();                         // list built-in definitions
engine.discover_recipes()?;                       // scan recipes dir for user recipes
engine.ingest(&CorpusSpec::Builtin("wikipedia".into()), progress).await?;

// ── Index management ──────────────────────────────
engine.installed_indexes().await?;                // list all on-disk indexes
let index = engine.open_index(&path).await?;      // open one for searching
engine.remove_index("wikipedia")?;

// ── Search ────────────────────────────────────────
index.search(&query_embedding, query_text, 10).await?;
index.info().await?;                              // metadata, chunk count, size
index.chunk_count().await?;

// ── Shard operations (the Commonwealth contract) ──
engine.index_stats("wikipedia").await?;
engine.extract_shard("wikipedia", chunk_range, &output_dir).await?;
engine.merge_shards(&shard_dirs, &output_dir).await?;
```

### The three-operation sharding contract

Everything a distribution layer needs from `corpus-engine` fits in three operations:

| Operation | What it does | Used for |
|---|---|---|
| `index_stats(corpus_id)` | Reports total chunks, chunk ID range, and size on disk | Planning how to split the corpus across peers |
| `extract_shard(corpus_id, chunk_range, output)` | Creates a new index containing only chunks in the given range | Preparing data for peer transfer |
| `merge_shards(shard_dirs, output)` | Reconstitutes a complete index from multiple shard files | Consolidating received shards on a receiving peer |

A shard is structurally identical to a complete index — same LanceDB schema, same search API. `CorpusIndex::search()` doesn't know or care whether it's searching a full corpus or a slice of one. That's Liskov substitution, and it's the whole reason the shard contract stays at three operations.

Commonwealth uses these to distribute indexes across mesh nodes without knowing anything about LanceDB internals. `corpus-engine` knows nothing about nodes, meshes, or networks — it just extracts and merges byte ranges on request.

## Recipes

A recipe is a TOML file describing how to build one corpus:

```toml
[corpus]
id = "wikipedia"
name = "Wikipedia"
license = "CC BY-SA 4.0"
mesh_sharing = true

[acquire]
type = "bulk_download"
url = "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-pages-articles.xml.bz2"
resume = true

[extract]
type = "mediawiki_xml"
namespace_filter = [0]
skip_redirects = true
decompress = "bzip2"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 256

[index]
embedding_model = "nomic-embed-text-v2"
embedding_dimensions = 768
```

Built-in recipes ship for Wikipedia, OpenAlex, Stack Exchange, Project Gutenberg, the Stanford Encyclopedia of Philosophy, and CRS Reports. Users can drop custom recipe TOML files into the recipes directory and they get picked up by `engine.discover_recipes()`.

## Supported source formats

| Extractor | Source format | Notes |
|---|---|---|
| `mediawiki_xml` | MediaWiki dump (XML, optionally bz2) | Strips templates, wikilinks, refs, and bold/italic markup; splits by section headers |
| `stackexchange_xml` | Stack Exchange Posts.xml | Pairs questions with answers above a configurable score threshold |
| `jsonl` | Newline-delimited JSON | Supports OpenAlex inverted-index abstract reconstruction; optional gzip |
| `html` | Directory of .html/.htm files | Extracts `<title>`, strips tags, decodes entities, collapses whitespace |
| `csv` | CSV files | Configurable content/title columns and delimiter |
| `parquet` | Parquet files | Arrow-based column extraction |
| `plaintext` | Directory of .txt files | Optional Project Gutenberg boilerplate stripping |

## Chunking strategies

| Chunker | Behavior |
|---|---|
| `paragraph` | Splits on double-newlines, then single newlines, then sentence boundaries, then word boundaries. Configurable max size and overlap. Handles documents from one sentence to a full book. |
| `sentence` | Accumulates at sentence boundaries up to a max character limit. |
| `fixed` | Fixed-size windows with overlap, split at word boundaries. |
| `semantic` | Splits at heading boundaries (Markdown `#`, MediaWiki `==`, Setext `===`). |

## Index layout on disk

Each corpus is a LanceDB directory containing:

```
~/.sovereign/indexes/
├── wikipedia/
│   ├── _corpus_meta.json          # corpus_id, embedding_model, mesh_sharing, etc.
│   ├── chunks.lance/              # Lance table directory
│   │   ├── _versions/
│   │   ├── data/
│   │   │   ├── 0000.lance
│   │   │   └── 0001.lance
│   │   ├── _indices/              # IVF-PQ vector index + Tantivy FTS indexes
│   │   └── _latest.manifest
│
├── openalex/
│
└── stackexchange-shard-0-6200000/  # Shard — structurally identical to a complete index
    ├── _corpus_meta.json
    └── chunks.lance/
```

The `_corpus_meta.json` file is the authoritative source for corpus metadata. Filenames are for humans.

## Embedding model compatibility

Every index records its embedding model and dimensions in `_corpus_meta.json`. `CorpusEngine::open_index()` validates that an index was built with the same model as the engine is configured for, and returns `Error::IncompatibleEmbedding` on mismatch. This prevents you from searching an index built with one model using a query vector from a different model.

All indexes in a shared directory must use the same embedding model. The default is `nomic-embed-text-v2` (768 dimensions).

## Safety

Hardcoded rules enforced by the engine, not configurable from recipes:

- **robots.txt** compliance on all web crawls
- **1 second minimum** delay between requests to the same domain
- **Honest User-Agent**: `CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)`
- **Scope enforcement**: crawl link patterns must match the seed URL's domain
- **Size warnings**: downloads larger than 1.5× the estimated size trigger a warning

These are deliberate choices. A library that scrapes the web should behave well on the web.

## Testing

```
cargo test
```

95 tests cover:
- Index create/insert/search round-trip (FTS only, vector only, hybrid)
- Shard extract/merge round-trip (3 shards → merge → results match original)
- Embedding model compatibility check
- Every extractor with representative fixtures (MediaWiki XML, Stack Exchange XML, JSONL, Parquet, HTML, plaintext)
- Every chunker at small, medium, and book-sized inputs
- Recipe TOML parsing and round-trip serialization
- Safety: robots.txt scope validation, rate limiter, download size checks
- Deep-link parsing and round-trip

## Dependencies

Notable dependencies, all pinned via workspace versions in consumers:

- `lancedb` — embedded vector database
- `arrow`, `parquet` — columnar data and Parquet reading
- `quick-xml` — streaming XML parsing
- `scraper` — HTML parsing
- `bzip2`, `flate2` — decompression
- `reqwest` — HTTP with resumable downloads
- `tokio` — async runtime
- `serde`, `toml` — recipe parsing and serialization

**Build requirement:** LanceDB pulls in `lance-table` which requires the protobuf compiler (`protoc`). Install it with `brew install protobuf` on macOS or `apt install protobuf-compiler` on Debian/Ubuntu before building.

## License

Apache-2.0
