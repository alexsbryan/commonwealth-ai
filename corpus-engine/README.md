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

- **[Sovereign](../sovereign)** — a standalone AI application that indexes knowledge bases locally and reasons over them with a local LLM.
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
 huggingface          stackexchange        sentence              (injected
 local_file           wikipedia_jsonl      fixed                  by caller)
                      jsonl (openalex)     semantic
                      html
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
                              ┌───────────────┼───────────────┐
                              ▼               ▼               ▼
                    ~/.svrnmesh/indexes/   Enrichment     Delta Updates
                    ├── wikipedia/         (field model,  (version manifests,
                    ├── openalex/           opt-in)        incremental)
                    └── …-shard-0-…/
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
    "~/.svrnmesh/recipes".into(),
    "~/.svrnmesh/indexes".into(),
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
embedding_model = "qwen3-embedding-0.6b"
embedding_dimensions = 1024
```

Built-in recipes ship for Wikipedia, OpenAlex, Stack Exchange, Project Gutenberg, the Stanford Encyclopedia of Philosophy, and CRS Reports. Recipe TOML files live in the [`sovereign-recipes`](../sovereign-recipes) repository and are consumed via `RecipeRegistry`.

### Recipe Registry

`RecipeRegistry` (`src/registry.rs`) manages the catalog of available corpora:

- **Bundled snapshot** — `build.rs` vendors `sovereign-recipes/registry.toml` into `OUT_DIR` and `registry.rs` `include_str!`s it from there, so the engine works fully offline with no checked-in snapshot copy to drift. Updating the snapshot = updating the `sovereign-recipes/` tree and rebuilding.
- **Live refresh** — `RecipeRegistry::refresh()` fetches the latest `registry.toml` from GitHub. Each entry has a `toml_url` pointing to the raw recipe file.
- **Resolution order** — local override on disk → remote (`toml_url`) → bundled fallback (`recipe_builtin.rs::bundled_recipe_toml`).
- **SHA-256 verification** — when the registry entry's `sha256` field is non-empty, the fetched recipe is verified.

Users can drop custom recipe TOML files into the local recipes directory and they get picked up by `engine.discover_recipes()`.

## Field Model Enrichment

An optional LLM-driven post-indexing pass that analyzes a corpus holistically. Enabled per recipe with `[enrichment] enabled = true, domain = "philosophy"`.

Five phases:

1. **Skeleton extraction** — identifies canonical questions and positions from overview chunks using domain-specific LLM prompts
2. **HDBSCAN clustering** — clusters chunk embeddings (no inference required), then labels clusters via LLM
3. **Alignment** — maps clusters to skeleton positions using embedding similarity + LLM verification
4. **Fault line detection** — identifies substantive disagreements between aligned positions
5. **Open question detection** — surfaces questions where the corpus has gaps

The `Domain` trait (`src/enrichment/domain.rs`) is the single extension point. It defines epistemic vocabulary, overview filters, all LLM prompts, and configuration parameters. Five fully-implemented domains live in `src/enrichment/domains/`: `philosophy`, `personal`, `conversational`, `business_email`, and `institutional`. (A domain is registered only when every method has a real body — earlier `todo!()` stubs were removed so a `--domain` selection can't panic mid-enrichment.)

`FieldModelEngine` orchestrates all phases with checkpoint-based resumability. Without an `InferenceFn`, enrichment is skipped with a warning — ingestion still succeeds.

## Delta Updates

`update/delta.rs` supports incremental index updates via version manifests:

- `VersionManifest` tracks per-document revision IDs
- `ManifestDiff` computes additions, updates, and deletions between two manifests
- Updates apply in three phases: deletions → updates (delete-then-re-add) → additions
- Resumability via `_update_progress.json` so interrupted updates continue where they left off

## Supported source formats

| Extractor | Source format | Notes |
|---|---|---|
| `mediawiki_xml` | MediaWiki dump (XML, optionally bz2) | Strips templates, wikilinks, refs, and bold/italic markup; splits by section headers |
| `wikipedia_jsonl` | Wikipedia JSONL (ZIP+JSONL from HuggingFace) | Section-level extraction with Wikidata QIDs, revision IDs, and link metadata |
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
~/.svrnmesh/indexes/
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

All indexes in a shared directory must use the same embedding model. The default is `qwen3-embedding-0.6b` (1024 dimensions).

## Safety

Hardcoded rules enforced by the engine, not configurable from recipes:

- **robots.txt** compliance on all web crawls
- **1 second minimum** delay between requests to the same domain
- **Honest User-Agent**: `CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)`
- **Scope enforcement**: crawl link patterns must match the seed URL's domain
- **Size warnings**: downloads larger than 1.5× the estimated size trigger a warning

These are deliberate choices. A library that scrapes the web should behave well on the web.

## Recipe Test Harness

Community-contributed recipes go through a review process before they're merged. Without a test harness, reviewers have to either trust the author or download hundreds of gigabytes to verify that a recipe works. The harness closes that gap.

Run one command, get a `TEST_REPORT.md` that shows exactly what was extracted, how it chunked, and (optionally) whether search works. The report goes in the PR alongside the recipe file. Reviewers read a Markdown file instead of running a pipeline.

### Usage

```bash
# Test a recipe — downloads a sample, runs extract → chunk, writes TEST_REPORT.md
sovereign recipe test ./recipes/openalex/recipe.toml

# Options
sovereign recipe test ./recipes/openalex/recipe.toml \
  --sample-size 50       \  # how many records to sample (default: 100)
  --no-embed             \  # skip the embed + search phase (default: always off here)
  --output ./report.md   \  # write report to a custom path
  --offline              \  # skip the source URL HEAD-check
  --verbose                 # print per-record extraction outcome

# Validate fields without downloading anything
sovereign recipe validate ./recipes/openalex/recipe.toml --offline

# Same commands work with the Commonwealth CLI
commonwealth recipe test ./recipes/openalex/recipe.toml --sample-size 50 --no-embed
commonwealth recipe validate ./recipes/openalex/recipe.toml --offline
```

Exit code: `0` = PASS, `1` = FAIL. Suitable for CI.

### What the report covers

| Section | What it shows |
|---|---|
| Summary | Status (PASS/FAIL), recipe ID, sample size, timestamp |
| Warnings | Non-fatal issues (low extraction rate, missing license, etc.) |
| Validation | Per-field checks: `corpus.id`, `corpus.name`, `license`, source configured, source reachable |
| Acquisition | Source URL, bytes downloaded, duration |
| Extraction | Records attempted / succeeded, extraction rate, up to 5 failed-record examples |
| Chunking | Total chunks, avg/min/max char counts, over-limit count, 5 sample chunks |
| Embedding & Search | Test query results (disabled when `--no-embed`) |
| Full-corpus estimate | Projected chunks at full scale (shows the arithmetic) |

### Pass criteria

`passed()` returns `true` when:
- No validation errors
- Extraction rate > 80% (if extraction ran)
- No chunks exceed `max_chars` (if chunking ran)
- All test queries returned ≥ 1 hit (if embedding ran)

An extraction rate in [80%, 90%) generates a warning but still passes.

### Recipe directory convention

```
recipes/
├── openalex/
│   ├── recipe.toml        # The recipe definition
│   ├── README.md          # What it is, who it's for, licensing notes
│   └── TEST_REPORT.md     # Generated by the author; committed to the PR
├── courtlistener/
│   ├── recipe.toml
│   ├── README.md
│   └── TEST_REPORT.md
└── ...
```

`TEST_REPORT.md` is committed to the repository — it is **not** gitignored. It is the artifact that makes async PR review possible.

### CI configuration (community recipe repository)

```yaml
# .github/workflows/test-recipe.yml
name: Test Recipe
on:
  pull_request:
    paths: ['recipes/**']

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 2 }

      - uses: dtolnay/rust-toolchain@stable

      - name: Install protoc
        run: sudo apt-get install -y protobuf-compiler

      - name: Install corpus-engine CLI
        run: cargo install corpus-engine-cli --features cli

      - name: Find changed recipes
        id: recipes
        run: |
          echo "files=$(git diff --name-only HEAD~1 HEAD \
            | grep 'recipe\.toml$' | tr '\n' ' ')" >> $GITHUB_OUTPUT

      - name: Validate each recipe
        run: |
          for f in ${{ steps.recipes.outputs.files }}; do
            corpus-engine recipe validate "$f" --offline
          done

      - name: Test each recipe
        run: |
          for f in ${{ steps.recipes.outputs.files }}; do
            corpus-engine recipe test "$f" --sample-size 50 --no-embed
          done

      - name: Verify TEST_REPORT.md is present
        run: |
          for f in ${{ steps.recipes.outputs.files }}; do
            dir=$(dirname "$f")
            report="$dir/TEST_REPORT.md"
            if [ ! -f "$report" ]; then
              echo "Missing: $report"
              echo "Run 'sovereign recipe test $f' and include the report in your PR."
              exit 1
            fi
            echo "✓ $report present"
          done
```

> **Note on `--no-embed`:** CI uses `--no-embed` for speed and because a GPU may not be available. The embed phase is expected to be run locally by the contributor before submitting the PR.

### Notes on large BulkDownload sources

For recipes that use `acquire.type = "bulk_download"` pointing to a large file (e.g., a Wikipedia dump at 13 GB), the harness downloads the full file before extracting a sample. For these recipes, it's practical to:

1. Run `recipe validate --offline` in CI (no download, field checks only).
2. Run `recipe test` locally against a smaller local mirror or the real source.

For HuggingFace datasets (`acquire.type = "huggingface_dataset"`), the harness automatically downloads **only the first parquet shard**, making CI feasible even for large multi-shard datasets.

## Testing

```
cargo test
```

222 tests cover:
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
- `hdbscan` — clustering for field model enrichment
- `quick-xml` — streaming XML parsing
- `scraper` — HTML parsing
- `bzip2`, `flate2` — decompression
- `reqwest` — HTTP with resumable downloads
- `tokio` — async runtime
- `serde`, `toml` — recipe parsing and serialization

**Build requirement:** LanceDB pulls in `lance-table` which requires the protobuf compiler (`protoc`). Install it with `brew install protobuf` on macOS or `apt install protobuf-compiler` on Debian/Ubuntu before building.

## Roadmap

The v2 enrichment pipeline (typed-graph atlas — seven atom types, seven edge types, stage-by-stage map-reduce with seed-threaded extraction) has its own plan of record:

- **[ENRICHMENT_V2.md](ENRICHMENT_V2.md)** — status table, landing-by-landing scope, verification targets.

## License

Apache-2.0
