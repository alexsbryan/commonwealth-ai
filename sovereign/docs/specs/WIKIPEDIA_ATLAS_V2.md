# Wikipedia atlas v2 — the columnar-structural path

Status: **design sketch** (2026-06-28). Supersedes the "keep wikipedia on rkyv"
interim decision in `ATLAS_V2_DEPLOYMENT.md` step 3a.

## Thesis

Wikipedia's atlas is **structural, not semantic**. Its atoms are
`enrichment_depth: "structural"` stubs — one shallow Entity per article (title +
lead sentence + URL) — and its real enrichment is the structure Wikipedia
*already carries*: the link graph, section paths, POV/citation signals, redirects,
Wikidata QIDs. None of that is LLM-derived; it's parsed straight from the dump.

That structure is **columnar-native**. The proper v2 path for wikipedia is *not*
to port SEP's semantic atom model (rich JSON payload + embeddings + ANN) onto
Lance — that's the model mismatch that made the "wiki paged reader" hard. It's to
bake the structure **into Lance columns**, where each article is a row and its
enrichment is typed columns + a CSR link adjacency. The hard reader problem
(payload parsing, the `&'a` borrow over on-demand reads) **dissolves**, because
there is no rich payload to parse — only structural columns read columnar-selective,
which is exactly Lance's strength.

## Status + correction (2026-06-28)

**Implementation corrected the consumer + edge model** (the sections below this block
are the original sketch; where they say `atlas_navigate` / `edges.csr`, read the
corrected design here). The wiki retrieval consumer is **not** `atlas_navigate` — it's
the `WikipediaGraph` **neighbors API** (`expand_via_wikipedia_graph` →
`neighbors_for_axis` / `co_neighbors` / `neighbors`, gated by
`SOVEREIGN_GRAPH_NEIGHBOR_EXPAND`). Those are **predicate** queries (axis filtering
matches the link's `source_section_path` / `link_text` / `target_title`), so the link
graph is a **predicate-queryable `edges.lance`**, not the `atlas_navigate` `edges.csr`
adjacency. The columnar store is therefore the **drop-in replacement for the SQLite
`wikipedia_graph.db`**, serving the same query API.

**Done + green (corpus-engine columnar layer):**
- **W1** — `articles.lance` + `edges.lance` writer + schema (`enrichment/atlas/wiki_store.rs`).
  Articles: title/qid/revision/in_scope/pov_total/citation_total/is_contested. Edges:
  source_title/target_title/relationship_type/link_text/occurrence_count/source_section_path/target_in_scope.
- **W2** — `ColumnarWikipediaGraph` (`wikipedia_columnar.rs`) serving the full neighbor
  API (`neighbors` / `neighbors_for_axis` / `co_neighbors` / `reverse_neighbors` /
  `has_contested_section` / `record`) via Lance predicate queries + Rust-side
  `GROUP BY/SUM/ORDER/LIMIT` folds. Reuses `wikipedia_graph`'s `Neighbor`/`ArticleRecord`.
- **W1b** — `WikipediaGraph::export_columnar(atlas_dir)` (SQLite → Lance dump) + a
  **gold-standard parity test**: the columnar reader answers `neighbors` /
  `neighbors_for_axis` / `has_contested_section` **identically** to the SQLite graph.
- **W3** — `WikipediaGraphApi` trait (`#[async_trait]`, `dyn`-safe) implemented by both
  backends; the runtime holds `Option<Arc<dyn WikipediaGraphApi>>`
  (`runtime.rs::with_wikipedia_graph`); a shared `corpus_engine::open_wikipedia_graph`
  per-corpus gate (columnar-store-present → columnar, else SQLite) routes **all three**
  loaders (chat/bootstrap, server, desktop — previously duplicated). Gate unit test +
  lint clean across 24 crates. **Live verify (chaos QA over wiki-grounded questions)
  pending** — needs a backfilled wiki columnar store, so it rides with W4.

**Remaining:**
- **W4** — make the columnar store the build output directly + retire the SQLite +
  `atoms.json`/`edges.json`/`atoms.rkyv` for wiki (the ~3.4 GB → few-hundred-MB
  unification). Then the live W3 chaos-QA verify. Open question still: whether wiki's
  `atoms.rkyv`/`AtlasGraph` is used elsewhere (typed-enumeration) and must also move
  before the rkyv delete.

## Current state — the same structure stored three times (~3.4 GB)

| Store | Size | What it holds |
|---|---|---|
| `atlas/atoms.json` | 758 MB | shallow article Entity atoms (title, description, URL) |
| `atlas/edges.json` | **1.39 GB** | the link graph as atom→atom edges |
| `atlas/atoms.rkyv` | 1.26 GB | the v1 read archive (projection of the above) |
| `wikipedia_graph.db` | (SQLite) | `articles` / `edges` / `section_signals` — the *real* structural model |

`wikipedia_graph.rs` (Layer 0, `sovereign atlas wikipedia build-graph`) already
computes the structural model from `WikipediaChunkMetadata` (`section_path`,
`pov_count`, `citation_needed_count`, `outgoing_links`, `wikidata_qid`,
`revision_id`) and exposes the retrieval API wiki actually uses: `neighbors`,
`neighbors_for_axis` (topical/causal/contested/defines), `co_neighbors`,
`reverse_neighbors`, `has_contested_section`, `record`. **No embeddings, no cosine
seeding** — link navigation + structural signals.

So today the same structural reality is materialised three+ ways. v2 unifies it.

## Target — one columnar store

```
atlas/
  articles.lance      # row per article; structure = typed columns
  edges.csr           # link adjacency over interned local ids (reuse the v2 CSR format)
```

**`articles.lance` columns** (all read columnar-selective; no payload blob):

| Column | Type | Source |
|---|---|---|
| `local_id` | u32 | interned (CSR endpoint id) |
| `title` | Utf8 | `articles.title` / canonical_name |
| `url` | Utf8 | `first_appearance.source_doc_id` |
| `description` | Utf8 | atom lead sentence (embed_text / chunk-request preview) |
| `wikidata_qid` | Utf8 (nullable) | `WikipediaChunkMetadata.wikidata_qid` |
| `revision_id` | i64 (nullable) | freshness / staleness gate |
| `in_scope` | bool | dangling-target marker |
| `pov_total` | i64 | contested signal (was `section_signals`) |
| `citation_total` | i64 | sourcing signal |
| `chunk_id` | Utf8 | evidence ChunkRef → the article's source chunk |
| `cluster_id` | i64 (nullable) | **Layer 1 slot** (HDBSCAN) — write later, no migration |
| `bridge_score` | f64 (nullable) | **Layer 1 slot** — bridge detection |
| `embedding` | FixedSizeList<f32> (nullable) | **W5 slot** — article/description vector for ANN seeding |

**`edges.csr`** reuses the existing v2 CSR binary (out + symmetric in, offsets/
neighbors/types/conf). The `type` byte carries the wiki relationship axis
(topical/causal/contested/defines/action/see-also) instead of the SEP `EdgeType`;
`conf` carries `occurrence_count` (normalised). Section context (which section of
the source links out) is dropped at the article-graph level — or kept as a parallel
`section_path` edge column if axis-by-section navigation proves load-bearing.

The 1.39 GB `edges.json` → a compact mmap'd CSR; the 758 MB atoms.json + the
SQLite graph → typed columns. **Est. footprint: ~3.4 GB → a few hundred MB.**

## How it maps onto the `AtlasGraph` API

The columnar article model satisfies the *same* `AtlasGraph` method surface, so
`atlas_navigate` and retrieval are unchanged:

| `AtlasGraph` method | Columnar implementation |
|---|---|
| `atom(title)` | article-row lookup (title→local_id index) |
| `atoms()` / `atoms_of_kind(Entity)` | column scan (every wiki atom is an article Entity) |
| `edges_from/to(title)` | `edges.csr` out/in adjacency (= `neighbors` / `reverse_neighbors`) |
| `edge_degree` | CSR degree (link prominence) |
| `atom_evidence(title)` | the `chunk_id` column → one ChunkRef |
| `AtomView` fields | columns (title→name, description, `pov_total`→a salience-like signal) |

`aliases`/`participants`/`evidence`/`atom_envelope` — the payload-derived fields
that forced the hard reader — are **empty/trivial for wiki** (structural atoms
have no aliases/participants; evidence is the single `chunk_id`). So the
`AtomView::WikiColumnar` variant serves them from columns or returns empty, with
**no payload parse**.

## The reader — why the borrow problem dissolves

Two viable residency strategies; pick by measurement at build (W2):

1. **Resident compact columns** (preload-style, sync). Read `title` + `description`
   + signals at open into resident Arrow arrays (~250 MB for 1.67M rows — the
   bulk *payload* that made preload-sync fatal at wiki scale is **gone**), `edges.csr`
   mmap'd. `AtomView` borrows `&'a str` from the resident column arrays exactly like
   the rkyv backend. Sync query API, no `atlas_navigate` ripple. Simpler; ~250 MB RSS.
2. **Lance-paged selective** (async open → sync bridge). Hold only the title→local_id
   index resident; read touched articles' columns on demand via predicate pushdown
   (the spike's 13–32 MB pattern). Lower RSS, but reintroduces the `&'a` cache
   question — solved with a stable-ref cache (`elsa::FrozenMap`) over the bounded
   BFS neighborhood (tens of articles/query under structural seeding).

Either works *because there is no rich payload*. Strategy 1 is the recommended
starting point (simple, sync, and 250 MB is far under the 1.26 GB rkyv it
replaces); revisit with strategy 2 only if inference co-residency RSS demands it.

## Retrieval — structural seeding (no embeddings, initially)

Wiki `atlas_navigate` keeps its shape (seed → typed-edge BFS → ChunkRequests) but
sources seeds + edge weights structurally:

- **Seed**: name-match (query title/entity → article via the title index) +
  contested/POV signal boosts. No cosine bag (wiki has none). The existing
  name-match seeding in `atlas_navigate` already works title-first.
- **Navigate**: `edges.csr` BFS weighted by the relationship axis — reuse
  `edge_weight` with a wiki axis→weight map (contested/causal high, see-also low),
  mirroring `WikipediaGraph::neighbors_for_axis`.
- **W5 (future, optional)**: an `embedding` column over article descriptions →
  IVF-PQ → ANN seeding. This is the path that makes wiki seeding *semantic* +
  O(log N), and the only reason to ever prefer Lance-paged reads. Deferred until
  structural seeding is proven and the embed cost (~1.67M calls) is justified.

## Increments

- **W1 — columnar builder (dormant).** New wiki builder writes `articles.lance` +
  `edges.csr` directly from `WikipediaChunkMetadata` (reuse `ingest_from_chunks`'s
  parse; emit columns instead of SQLite rows / atom JSON). Gated; rkyv reader
  unchanged. **Verify:** row/edge counts == `wikipedia_graph.db`; spot-check a
  known article's neighbors round-trip.
- **W2 — wiki reader backend.** `AtlasGraph` gains a wiki-columnar backend (resident
  compact columns, strategy 1) behind the existing method API + the `read_v2` gate.
  **Verify:** parity of `neighbors`/`atom`/`atoms_of_kind` vs the rkyv+SQLite path;
  RSS + open-time re-measure vs rkyv's 13–32 MB / 2 ms.
- **W3 — structural seeding.** `atlas_navigate` over the wiki backend: name-match
  seed + axis-weighted CSR BFS. **Verify:** chaos QA on wiki-grounded questions ==
  the rkyv-served answers (retrieval-neutral or better).
- **W4 — flip + retire.** Write wiki's `.read_v2` marker; retire `atoms.json` /
  `edges.json` / `atoms.rkyv` / `wikipedia_graph.db` for wiki. Removes the rkyv
  carve-out entirely (so `ATLAS_V2_DEPLOYMENT.md` step 4 can delete the rkyv reader
  too, once SEP is also off it).
- **W5 — (optional, future) article embeddings → ANN.** The semantic-seeding upgrade.

## Open questions

- **Categories** are *not* in `WikipediaChunkMetadata` today (it has links/sections/
  POV/citation/QID). Category columns are a future extractor enrichment, out of scope
  for W1–W4.
- **Backend shape**: a third `AtlasGraph` backend variant (`Rkyv | Lance | WikiColumnar`)
  vs a separate `WikiAtlas` type behind a shared trait. Lean: a third variant keeps
  one `AtlasGraph` for all consumers; the variant just sources fields from article
  columns. Decide at W2.
- **Section granularity**: article-level rows + `section_path`-derived signals vs
  section-level rows. Start article-level; the SQLite `section_signals` collapses to
  per-article `pov_total`/`citation_total` already.

## Why this is the right v2 architecture (not a carve-out)

SEP/authored corpora: **semantic** enrichment → atom model + payload + embeddings +
ANN (the step-2 Lance reader, done). Wikipedia: **structural** enrichment → article
columns + typed CSR + (future) embeddings. **Both on Lance**, two schemas matched to
the *kind* of enrichment — not one model forced onto a corpus it doesn't fit. This
unifies wiki's three-store split, reclaims ~3 GB, makes wiki's structure first-class
queryable, and removes the rkyv carve-out. The earlier "wiki is a bad Lance fit" was
"wiki is a bad fit for the *semantic atom* model" — which is true, and is exactly why
wiki gets its own columnar-structural schema.
