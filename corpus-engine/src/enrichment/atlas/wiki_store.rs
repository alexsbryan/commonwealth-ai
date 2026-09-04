// SPDX-License-Identifier: AGPL-3.0-or-later
//! WIKIPEDIA_ATLAS_V2 — W1: the columnar wiki store **writer** (`articles.lance`
//! + `edges.lance`).
//!
//! Wikipedia's atlas is structural, not semantic (see
//! `docs/specs/WIKIPEDIA_ATLAS_V2.md`), and its retrieval consumer is the
//! **`WikipediaGraph` neighbors API** — `neighbors` / `neighbors_for_axis` /
//! `co_neighbors` / `has_contested_section` — not `atlas_navigate`. Those are
//! **predicate queries** (axis filtering matches the link's `source_section_path`),
//! so the link graph is stored as a predicate-queryable Lance table, NOT the
//! `edges.csr` adjacency the SEP `atlas_navigate` BFS uses. This store is the
//! columnar replacement for the SQLite `wikipedia_graph.db`: one
//! `ColumnarWikipediaGraph` (W2) serves the same query API over these two tables,
//! and W4 retires the SQLite + the 1.39 GB `edges.json`.
//!
//! - `articles.lance` — row per article, structural columns
//!   (title / qid / revision / in_scope / pov_total / citation_total). Keyed by
//!   title (the link-graph + neighbor-API key).
//! - `edges.lance` — row per `(source_title, source_section_path, target_title)`
//!   link, carrying `relationship_type` + `occurrence_count` + the section path
//!   (for axis filtering) + the denormalised `target_in_scope` (so the neighbor
//!   query needs no per-row join back to `articles`).
//!
//! **W4 (2026-09-04): this module is now the whole build.**
//! [`wiki_rows_from_chunks`] aggregates a corpus's chunks into the two row
//! sets directly, and [`build_wikipedia_columnar_store_from_chunks`] writes
//! them — no SQLite in the middle. The SQLite `wikipedia_graph.db` was only
//! ever a build aggregator that `export_columnar` dumped back out to these
//! same tables; the aggregation is an in-memory pass, so the round-trip
//! (2.4 GB of intermediate at full wiki scale) bought nothing.
//!
//! Wikipedia does NOT use the `atoms.lance` + `edges.csr` atom store the SEP-
//! class corpora use, and this is deliberate: its consumer is the predicate-
//! shaped neighbor API, and `neighbors_for_axis` filters on the per-edge
//! strings `link_text` + `source_section_path`, which a CSR adjacency
//! (`store.rs`'s `LocalEdge = (u32, u32, u8, f32, u8)`) has nowhere to put.
//! See `docs/specs/WIKIPEDIA_ATLAS_V2.md` §"Status + correction".

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{BooleanArray, Int64Array, RecordBatch, StringArray};

use crate::enrichment::atlas::atoms::AtomId;
use crate::enrichment::pipeline::atlas::EntityType;
use crate::extractors::wikipedia_types::{wiki_title_from_url, WikipediaChunkMetadata};
use crate::index::StoredChunkWithMetadata;

/// Wiki columnar store schema version. Bump on any `articles.lance` /
/// `edges.lance` column change (e.g. the Layer-1 cluster/bridge columns or a
/// future article-embedding column).
///
/// **v2 (2026-09-04)** added `atom_id` + `chunk_id` to `articles.lance`. Without
/// them the store cannot answer [`super::provider::AtlasProvider`]: the walk
/// keys on an atom id where the neighbor API keys on a title, and
/// `atom_evidence` has no chunk to hand back. A v1 store has neither column and
/// must be rebuilt (`svrn atlas wikipedia build-graph`).
pub const WIKI_STORE_FORMAT_VERSION: u32 = 2;

/// The columnar article store directory name (a Lance table under `atlas/`).
pub const ARTICLES_LANCE_DIRNAME: &str = "articles.lance";
/// The columnar edge store directory name (a Lance table under `atlas/`).
pub const EDGES_LANCE_DIRNAME: &str = "edges.lance";
pub(crate) const ARTICLES_TABLE: &str = "articles";
pub(crate) const EDGES_TABLE: &str = "edges";

/// One wiki article as v2 columns — the structural fields the `WikipediaGraph`
/// `record` / `has_contested_section` surface reads. Keyed by `title`. Nullable
/// source fields collapse to sentinels (`""` qid, `-1` revision), matching the
/// SEP store convention.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiArticleRow {
    /// Content-hash atom id — [`wiki_atom_id`]. The walk's key, and the seed
    /// table's. Distinct from `title`, which is the link graph's key: the two
    /// faces of this store address the same article by different handles
    /// because their questions differ, and both are stored so neither has to
    /// derive the other's.
    pub atom_id: String,
    /// Canonical article title (the neighbor-API + link-graph key).
    pub title: String,
    /// Wikidata QID (`""` if absent).
    pub wikidata_qid: String,
    /// Revision id for the freshness gate (`-1` if absent).
    pub revision_id: i64,
    /// False for a dangling link target not itself in indexed scope.
    pub in_scope: bool,
    /// Aggregate POV-flag count across the article's sections (contested signal).
    pub pov_total: i64,
    /// Aggregate citation-needed count (sourcing signal).
    pub citation_total: i64,
    /// Any section flagged contested (`pov_count > 0` OR a `controversy`
    /// section) — the `has_contested_section` signal, denormalised to the
    /// article so the check is a single column read.
    pub is_contested: bool,
    /// The chunk this article's atom cites — its evidence anchor, and the
    /// join key to `chunks.lance` for the migrated seed table. The LOWEST
    /// chunk id among the article's chunks: deterministic across rebuilds
    /// (a `HashMap` iteration order is not), and the lead section, since the
    /// chunker emits in document order.
    pub chunk_id: String,
}

/// One link-graph edge as v2 columns — row per
/// `(source_title, source_section_path, target_title)`, mirroring the SQLite
/// `edges` table. `occurrence_count` is per-section (the neighbor query SUMs
/// across sections); `source_section_path` drives `neighbors_for_axis` filtering;
/// `target_in_scope` is denormalised so the neighbor query is a single-table
/// predicate scan.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiEdgeRow {
    pub source_title: String,
    pub target_title: String,
    /// `topical | causal | contested | defines | action | see-also`
    /// (the `classify_relationship` axis).
    pub relationship_type: String,
    /// The link's anchor text — one of the three fields `neighbors_for_axis`
    /// matches axis terms against (with `target_title` + `source_section_path`).
    pub link_text: String,
    pub occurrence_count: i64,
    /// `›`-joined section path the link sits in (axis-filter key).
    pub source_section_path: String,
    /// Whether `target_title` is itself an in-scope article.
    pub target_in_scope: bool,
}

fn articles_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("atom_id", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("wikidata_qid", DataType::Utf8, false),
        Field::new("revision_id", DataType::Int64, false),
        Field::new("in_scope", DataType::Boolean, false),
        Field::new("pov_total", DataType::Int64, false),
        Field::new("citation_total", DataType::Int64, false),
        Field::new("is_contested", DataType::Boolean, false),
        Field::new("chunk_id", DataType::Utf8, false),
    ]))
}

fn edges_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("source_title", DataType::Utf8, false),
        Field::new("target_title", DataType::Utf8, false),
        Field::new("relationship_type", DataType::Utf8, false),
        Field::new("link_text", DataType::Utf8, false),
        Field::new("occurrence_count", DataType::Int64, false),
        Field::new("source_section_path", DataType::Utf8, false),
        Field::new("target_in_scope", DataType::Boolean, false),
    ]))
}

fn articles_batch(rows: &[WikiArticleRow], sch: &Arc<Schema>) -> Result<RecordBatch, String> {
    let str_col = |f: &dyn Fn(&WikiArticleRow) -> &str| {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>())) as arrow_array::ArrayRef
    };
    let cols: Vec<arrow_array::ArrayRef> = vec![
        str_col(&|r| r.atom_id.as_str()),
        str_col(&|r| r.title.as_str()),
        str_col(&|r| r.wikidata_qid.as_str()),
        Arc::new(Int64Array::from(
            rows.iter().map(|r| r.revision_id).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.in_scope).collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rows.iter().map(|r| r.pov_total).collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rows.iter().map(|r| r.citation_total).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.is_contested).collect::<Vec<_>>(),
        )),
        str_col(&|r| r.chunk_id.as_str()),
    ];
    RecordBatch::try_new(sch.clone(), cols).map_err(|e| format!("articles record batch: {e}"))
}

fn edges_batch(rows: &[WikiEdgeRow], sch: &Arc<Schema>) -> Result<RecordBatch, String> {
    let str_col = |f: &dyn Fn(&WikiEdgeRow) -> &str| {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>())) as arrow_array::ArrayRef
    };
    let cols: Vec<arrow_array::ArrayRef> = vec![
        str_col(&|r| r.source_title.as_str()),
        str_col(&|r| r.target_title.as_str()),
        str_col(&|r| r.relationship_type.as_str()),
        str_col(&|r| r.link_text.as_str()),
        Arc::new(Int64Array::from(
            rows.iter().map(|r| r.occurrence_count).collect::<Vec<_>>(),
        )),
        str_col(&|r| r.source_section_path.as_str()),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.target_in_scope).collect::<Vec<_>>(),
        )),
    ];
    RecordBatch::try_new(sch.clone(), cols).map_err(|e| format!("edges record batch: {e}"))
}

/// Create + batch-write a Lance table under `atlas_dir`, overwriting any prior
/// one. Batched adds bound peak memory on the 1.67M-article / ~33M-edge wiki
/// graph.
async fn write_table(
    atlas_dir: &Path,
    dirname: &str,
    table: &str,
    sch: Arc<Schema>,
    batches: Vec<RecordBatch>,
    scalar_index: Option<&str>,
) -> Result<PathBuf, String> {
    let lance_dir = atlas_dir.join(dirname);
    if lance_dir.exists() {
        std::fs::remove_dir_all(&lance_dir)
            .map_err(|e| format!("remove stale {}: {e}", lance_dir.display()))?;
    }
    let uri = atlas_dir
        .to_str()
        .ok_or_else(|| format!("non-utf8 atlas dir {}", atlas_dir.display()))?;
    let db = lancedb::connect(uri)
        .execute()
        .await
        .map_err(|e| format!("lancedb connect {uri}: {e}"))?;
    let tbl = db
        .create_empty_table(table, sch)
        .execute()
        .await
        .map_err(|e| format!("create {dirname}: {e}"))?;
    for rb in batches {
        tbl.add(vec![rb])
            .execute()
            .await
            .map_err(|e| format!("{dirname} add: {e}"))?;
    }
    // A scalar BTree index turns the neighbor query's `WHERE source_title = ?`
    // (and `IN (...)`) from a full columnar scan into a point lookup — the
    // difference between ~700 ms and ~ms on the 7.85M-edge wiki graph.
    if let Some(col) = scalar_index {
        tbl.create_index(
            &[col],
            lancedb::index::Index::BTree(lancedb::index::scalar::BTreeIndexBuilder::default()),
        )
        .replace(true)
        .execute()
        .await
        .map_err(|e| format!("{dirname} index {col}: {e}"))?;
    }
    Ok(lance_dir)
}

const BATCH: usize = 50_000;

/// Write the wiki columnar store — `articles.lance` + `edges.lance` — into
/// `atlas_dir`. The columnar replacement for the SQLite `wikipedia_graph.db`;
/// the [`super::super::wikipedia_graph`] neighbor API reads it via predicate
/// queries (W2). Returns the `articles.lance` path.
pub async fn write_wikipedia_columnar_store(
    atlas_dir: &Path,
    articles: &[WikiArticleRow],
    edges: &[WikiEdgeRow],
) -> Result<PathBuf, String> {
    let asch = articles_schema();
    let abatches = articles
        .chunks(BATCH)
        .map(|c| articles_batch(c, &asch))
        .collect::<Result<Vec<_>, _>>()?;
    let articles_path = write_table(
        atlas_dir,
        ARTICLES_LANCE_DIRNAME,
        ARTICLES_TABLE,
        asch,
        abatches,
        None,
    )
    .await?;

    let esch = edges_schema();
    let ebatches = edges
        .chunks(BATCH)
        .map(|c| edges_batch(c, &esch))
        .collect::<Result<Vec<_>, _>>()?;
    // Index `source_title` — the neighbor query's predicate column.
    write_table(
        atlas_dir,
        EDGES_LANCE_DIRNAME,
        EDGES_TABLE,
        esch,
        ebatches,
        Some("source_title"),
    )
    .await?;

    Ok(articles_path)
}

// ── chunks → columnar rows (WIKIPEDIA_ATLAS_V2 W4: the direct build) ─────────
//
// The aggregation that used to feed the SQLite `wikipedia_graph.db` before
// `export_columnar` dumped it back out to Lance. The SQLite was never the
// model — it was a build aggregator, and the aggregation is this in-memory
// pass. Emitting the columnar rows straight from it drops the round-trip
// (2.4 GB of intermediate on the full wiki) and leaves ONE writer for the
// store this crate reads.

/// Section-path delimiter. U+203A (›) — never appears in Wikipedia titles, so
/// the joined `source_section_path` splits back cleanly. Changing it
/// invalidates every stored `edges.lance` path, so bump
/// [`WIKI_STORE_FORMAT_VERSION`] if it ever moves.
pub const SECTION_PATH_DELIMITER: char = '\u{203a}';

/// The entity type every wiki article atom carries — the value the live
/// `atoms.json` has always written, and part of the id's essence.
pub const WIKI_ENTITY_TYPE: &str = "article";

/// The atom id for a wiki article.
///
/// ONE DERIVATION (ARCH §10.6). This delegates to
/// [`AtomId::entity_content_hash`], the function `newsworthy_events`,
/// `code_walk`, `tabular_atoms` and `atoms_delta` already use — including the
/// `wikipedia-fetched` layer, whose 413 atoms are all `entity-<16 hex>` today.
/// A wiki article's identity in `wikipedia` and in its fetched/newsworthy twin
/// must come off the same function or the two derivations drift; the ids stay
/// DISTINCT because `corpus_id` is part of the essence, which is by design.
/// (An earlier draft hashed its own `(title, corpus_id)` framing here — a
/// second hasher for one essence, which is exactly the smell §10.6 names.)
///
/// Identity from ESSENCE, not from a counter (ARCH §7.5). Wikipedia's atom ids
/// were `entity-0001`, `entity-0002`, … assigned in sorted-title order, so
/// inserting one article shifted the id of every article after it: stable only
/// for a corpus that never changed, which is not a property anything can safely
/// cite. The hash is stable under insertion, deletion and rebuild — which is
/// what makes it CORRECT for the progressive path: a re-fetched or re-watched
/// title keeps its id instead of being renumbered by its neighbours.
///
/// The name is normalised by `lookup_key` before hashing, so the `|` field
/// separator inside `entity_content_hash` cannot appear in it and the framing
/// is unambiguous by construction. Normalisation does mean two titles differing
/// only in punctuation or case share an id; [`wiki_rows_from_chunks`] REFUSES
/// the build on that rather than silently merging two articles (§18.3).
pub fn wiki_atom_id(title: &str, corpus_id: &str) -> String {
    AtomId::entity_content_hash(
        title,
        &EntityType::from_str_repr(WIKI_ENTITY_TYPE),
        corpus_id,
    )
    .as_str()
    .to_string()
}

/// Counters from a direct chunks → columnar build — what the CLI prints as a
/// sanity check, and what the build ledger records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WikiIngestSummary {
    /// In-scope articles (one `articles.lance` row each).
    pub articles: usize,
    /// Unique `(source, section, target)` links (one `edges.lance` row each).
    pub edges: usize,
    /// Distinct `(article, section)` pairs seen — the old `section_signals`
    /// row count, kept as a build signal (the columnar store denormalises
    /// section signals onto the article + the edge).
    pub sections: usize,
    /// Link targets that are not themselves in-scope articles. They get no
    /// `articles.lance` row; `edges.target_in_scope = false` carries them.
    pub dangling_targets: usize,
    pub chunks_with_metadata: usize,
    pub chunks_without_metadata: usize,
    /// Highest `revision_id` seen — the freshness stamp.
    pub revision_id_max: Option<i64>,
}

struct AggregatedArticle {
    title: String,
    /// Lowest chunk id seen for this article — the evidence anchor. Lowest
    /// rather than first-seen because chunk arrival order is not stable.
    min_chunk_id: u64,
    wikidata_qid: Option<String>,
    revision_id: Option<i64>,
    pov_total: i64,
    citation_total: i64,
    sections: HashMap<String, AggregatedSection>,
}

impl AggregatedArticle {
    fn new(title: String) -> Self {
        Self {
            title,
            min_chunk_id: u64::MAX,
            wikidata_qid: None,
            revision_id: None,
            pov_total: 0,
            citation_total: 0,
            sections: HashMap::new(),
        }
    }

    /// The `articles.is_contested` signal: any section flagged POV, or a
    /// section typed `controversy`. Denormalised onto the article so
    /// `has_contested_section` is a single column read.
    fn is_contested(&self) -> bool {
        self.sections
            .values()
            .any(|s| s.pov_count > 0 || s.section_type == "controversy")
    }
}

struct AggregatedSection {
    section_type: String,
    pov_count: i64,
    counts_seen: bool,
    outgoing: HashMap<String, AggregatedEdge>,
}

impl AggregatedSection {
    fn new(section_type: String) -> Self {
        Self {
            section_type,
            pov_count: 0,
            counts_seen: false,
            outgoing: HashMap::new(),
        }
    }
}

struct AggregatedEdge {
    link_text: String,
    relationship_type: String,
    occurrence_count: i64,
}

/// Join a `section_path` array with [`SECTION_PATH_DELIMITER`].
pub fn join_section_path(parts: &[String]) -> String {
    parts.join(&SECTION_PATH_DELIMITER.to_string())
}

/// Rule-based relationship-type classifier. Runs at build time on every edge;
/// spends zero LLM tokens. ~80% accuracy is the bar — the label is one of
/// several signals the re-ranker mixes via RRF, not a load-bearing decision.
///
/// Order matters: section-path patterns dominate (a causal section beats an
/// "is" link-text), then link-text verb prefixes, then default to `topical`.
///
/// **One decider (ARCH §10.6).** This is the only implementation of the wiki
/// relationship axis; the `relationship_type` column of `edges.lance` is its
/// only output.
pub fn classify_relationship(section_path: &[String], link_text: &str) -> String {
    let path_lower: Vec<String> = section_path.iter().map(|p| p.to_lowercase()).collect();
    let last_path = path_lower.last().map(String::as_str).unwrap_or("");
    let any_path_contains = |needles: &[&str]| -> bool {
        path_lower
            .iter()
            .any(|p| needles.iter().any(|n| p.contains(n)))
    };

    if any_path_contains(&["criticism", "controversy", "debate", "dispute"]) {
        return "contested".to_string();
    }
    if any_path_contains(&["causes", "origins", "background"]) {
        return "causal".to_string();
    }
    if last_path.ends_with("see also") || last_path == "see also" {
        return "see-also".to_string();
    }

    let lt = link_text.trim().to_lowercase();
    let starts_with_any = |prefixes: &[&str]| -> bool {
        prefixes
            .iter()
            .any(|p| lt.starts_with(&format!("{p} ")) || lt == *p)
    };
    if starts_with_any(&["led", "caused", "resulted", "prompted", "triggered"]) {
        return "causal".to_string();
    }
    if starts_with_any(&["is", "are", "was", "were", "defined", "known"]) {
        return "defines".to_string();
    }

    "topical".to_string()
}

/// Aggregate a corpus's chunks into the two columnar row sets.
///
/// The chunker emits N chunks per section and repeats each section's
/// `outgoing_links` across all of them, so the structural truth — "this
/// section links to X" — is one row per `(article, section, target)` with
/// `occurrence_count = 1`; the neighbor query SUMs across sections. First-seen
/// `link_text` wins, matching the old `INSERT OR IGNORE`.
///
/// Only in-scope articles (those that appear as a link SOURCE) get an
/// `articles.lance` row — at full wiki that is ~52k of the 1.67M titles the
/// link graph names. Dangling targets ride on `edges.target_in_scope = false`,
/// which is why the neighbor query needs no join back to `articles`.
pub fn wiki_rows_from_chunks(
    corpus_id: &str,
    chunks: Vec<StoredChunkWithMetadata>,
) -> Result<(Vec<WikiArticleRow>, Vec<WikiEdgeRow>, WikiIngestSummary), String> {
    let mut articles: HashMap<String, AggregatedArticle> = HashMap::new();
    let mut chunks_with_metadata = 0usize;
    let mut chunks_without_metadata = 0usize;

    for chunk in chunks {
        let Some(metadata_raw) = chunk.metadata_raw.as_deref() else {
            chunks_without_metadata += 1;
            continue;
        };
        let meta: WikipediaChunkMetadata = match serde_json::from_str(metadata_raw) {
            Ok(m) => m,
            Err(_) => {
                chunks_without_metadata += 1;
                continue;
            }
        };
        chunks_with_metadata += 1;

        // Canonical article title — prefer the chunk title, fall back to the
        // URL-derived one. Neither present means an extraction artifact.
        let Some(article_title) = chunk
            .title
            .clone()
            .or_else(|| chunk.url.as_deref().and_then(wiki_title_from_url))
        else {
            continue;
        };

        let entry = articles
            .entry(article_title.clone())
            .or_insert_with(|| AggregatedArticle::new(article_title));
        entry.min_chunk_id = entry.min_chunk_id.min(chunk.id);

        // Per-article fields come from the first metadata that carries them —
        // Wikipedia revisions identify the same article across its sections.
        if entry.wikidata_qid.is_none() {
            entry.wikidata_qid = meta.wikidata_qid.clone();
        }
        if entry.revision_id.is_none() {
            entry.revision_id = meta.revision_id;
        }

        let section_path_joined = join_section_path(&meta.section_path);
        let section = entry
            .sections
            .entry(section_path_joined)
            .or_insert_with(|| AggregatedSection::new(meta.section_type.clone()));

        if !section.counts_seen {
            section.pov_count = meta.pov_count.unwrap_or(0);
            section.section_type = meta.section_type.clone();
            section.counts_seen = true;
        }

        for link in &meta.outgoing_links {
            section
                .outgoing
                .entry(link.target_title.clone())
                .or_insert_with(|| AggregatedEdge {
                    link_text: link.link_text.clone(),
                    relationship_type: classify_relationship(&meta.section_path, &link.link_text),
                    occurrence_count: 1,
                });
        }

        entry.pov_total += meta.pov_count.unwrap_or(0);
        entry.citation_total += meta.citation_needed_count.unwrap_or(0);
    }

    let revision_id_max = articles.values().filter_map(|a| a.revision_id).max();
    let mut sections = 0usize;
    let mut dangling: HashSet<&str> = HashSet::new();

    let mut edge_rows: Vec<WikiEdgeRow> = Vec::new();
    for art in articles.values() {
        sections += art.sections.len();
        for (section_path, section) in &art.sections {
            for (target_title, edge) in &section.outgoing {
                let target_in_scope = articles.contains_key(target_title.as_str());
                if !target_in_scope {
                    dangling.insert(target_title.as_str());
                }
                edge_rows.push(WikiEdgeRow {
                    source_title: art.title.clone(),
                    target_title: target_title.clone(),
                    relationship_type: edge.relationship_type.clone(),
                    link_text: edge.link_text.clone(),
                    occurrence_count: edge.occurrence_count,
                    source_section_path: section_path.clone(),
                    target_in_scope,
                });
            }
        }
    }
    let dangling_targets = dangling.len();

    // Mint the content-hash ids, and REFUSE on a collision rather than let two
    // articles become one atom. `by_id` is the check, not a convenience: an id
    // that two titles share is a silent merge, which is the failure §18.3 names.
    let mut by_id: HashMap<String, &str> = HashMap::with_capacity(articles.len());
    let mut article_rows: Vec<WikiArticleRow> = Vec::with_capacity(articles.len());
    for a in articles.values() {
        let atom_id = wiki_atom_id(&a.title, corpus_id);
        if let Some(other) = by_id.insert(atom_id.clone(), a.title.as_str()) {
            if other != a.title {
                return Err(format!(
                    "wiki atom id collision: {atom_id} is both {other:?} and {:?} in corpus \
                     {corpus_id} — widen wiki_atom_id before rebuilding",
                    a.title
                ));
            }
        }
        article_rows.push(WikiArticleRow {
            atom_id,
            title: a.title.clone(),
            wikidata_qid: a.wikidata_qid.clone().unwrap_or_default(),
            revision_id: a.revision_id.unwrap_or(-1),
            in_scope: true,
            pov_total: a.pov_total,
            citation_total: a.citation_total,
            is_contested: a.is_contested(),
            // Every in-scope article was reached THROUGH a chunk, so the
            // sentinel is unreachable; it would mean an article aggregated
            // from no chunk at all.
            chunk_id: if a.min_chunk_id == u64::MAX {
                String::new()
            } else {
                a.min_chunk_id.to_string()
            },
        });
    }

    let summary = WikiIngestSummary {
        articles: article_rows.len(),
        edges: edge_rows.len(),
        sections,
        dangling_targets,
        chunks_with_metadata,
        chunks_without_metadata,
        revision_id_max,
    };
    tracing::info!(
        articles = summary.articles,
        edges = summary.edges,
        sections = summary.sections,
        dangling_targets = summary.dangling_targets,
        chunks_with_metadata = summary.chunks_with_metadata,
        chunks_without_metadata = summary.chunks_without_metadata,
        revision_id_max = ?summary.revision_id_max,
        "wiki columnar: aggregated chunks into store rows"
    );
    Ok((article_rows, edge_rows, summary))
}

/// The whole direct build: chunks → rows → `articles.lance` + `edges.lance`.
/// The single entry point the CLI drives; replaces
/// `WikipediaGraph::ingest_from_chunks` + `export_columnar`.
pub async fn build_wikipedia_columnar_store_from_chunks(
    atlas_dir: &Path,
    corpus_id: &str,
    chunks: Vec<StoredChunkWithMetadata>,
) -> Result<WikiIngestSummary, String> {
    let (articles, edges, summary) = wiki_rows_from_chunks(corpus_id, chunks)?;
    write_wikipedia_columnar_store(atlas_dir, &articles, &edges).await?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;

    fn art(title: &str, pov: i64) -> WikiArticleRow {
        WikiArticleRow {
            atom_id: wiki_atom_id(title, "wiki-test"),
            title: title.into(),
            wikidata_qid: format!("Q-{title}"),
            revision_id: 100,
            in_scope: true,
            pov_total: pov,
            citation_total: 5,
            is_contested: pov > 0,
            chunk_id: format!("chunk-{title}"),
        }
    }

    fn edge(src: &str, tgt: &str, rel: &str, sect: &str, occ: i64, tgt_in: bool) -> WikiEdgeRow {
        WikiEdgeRow {
            source_title: src.into(),
            target_title: tgt.into(),
            relationship_type: rel.into(),
            link_text: tgt.to_lowercase(), // anchor text; the title for this fixture
            occurrence_count: occ,
            source_section_path: sect.into(),
            target_in_scope: tgt_in,
        }
    }

    async fn read_articles(atlas_dir: &Path) -> Vec<WikiArticleRow> {
        let db = lancedb::connect(atlas_dir.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let tbl = db.open_table(ARTICLES_TABLE).execute().await.unwrap();
        let batches: Vec<RecordBatch> = tbl
            .query()
            .execute()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let s = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .clone()
        };
        let i = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .clone()
        };
        let bo = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .clone()
        };
        let mut out = Vec::new();
        for b in &batches {
            let (aid, title, qid) = (s(b, "atom_id"), s(b, "title"), s(b, "wikidata_qid"));
            let cid = s(b, "chunk_id");
            let (rev, pov, cit) = (
                i(b, "revision_id"),
                i(b, "pov_total"),
                i(b, "citation_total"),
            );
            let (insc, cont) = (bo(b, "in_scope"), bo(b, "is_contested"));
            for k in 0..b.num_rows() {
                out.push(WikiArticleRow {
                    atom_id: aid.value(k).to_string(),
                    title: title.value(k).to_string(),
                    wikidata_qid: qid.value(k).to_string(),
                    revision_id: rev.value(k),
                    in_scope: insc.value(k),
                    pov_total: pov.value(k),
                    citation_total: cit.value(k),
                    is_contested: cont.value(k),
                    chunk_id: cid.value(k).to_string(),
                });
            }
        }
        out.sort_by(|a, b| a.title.cmp(&b.title));
        out
    }

    async fn read_edges(atlas_dir: &Path) -> Vec<WikiEdgeRow> {
        let db = lancedb::connect(atlas_dir.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let tbl = db.open_table(EDGES_TABLE).execute().await.unwrap();
        let batches: Vec<RecordBatch> = tbl
            .query()
            .execute()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let s = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .clone()
        };
        let i = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .clone()
        };
        let bo = |b: &RecordBatch, n| {
            b.column_by_name(n)
                .unwrap()
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .clone()
        };
        let mut out = Vec::new();
        for b in &batches {
            let (src, tgt, rel, lt, sect) = (
                s(b, "source_title"),
                s(b, "target_title"),
                s(b, "relationship_type"),
                s(b, "link_text"),
                s(b, "source_section_path"),
            );
            let occ = i(b, "occurrence_count");
            let tin = bo(b, "target_in_scope");
            for k in 0..b.num_rows() {
                out.push(WikiEdgeRow {
                    source_title: src.value(k).to_string(),
                    target_title: tgt.value(k).to_string(),
                    relationship_type: rel.value(k).to_string(),
                    link_text: lt.value(k).to_string(),
                    occurrence_count: occ.value(k),
                    source_section_path: sect.value(k).to_string(),
                    target_in_scope: tin.value(k),
                });
            }
        }
        out
    }

    #[tokio::test]
    async fn wikipedia_columnar_store_roundtrips_articles_and_edges() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let articles = vec![art("Alpha", 0), art("Beta", 0), art("Gamma", 3)];
        // Alpha → Beta (topical, Intro), Alpha → Gamma (contested, Criticism),
        // Alpha → External (topical, See also; out-of-scope target).
        let edges = vec![
            edge("Alpha", "Beta", "topical", "Intro", 2, true),
            edge("Alpha", "Gamma", "contested", "Criticism", 1, true),
            edge("Alpha", "External", "topical", "See also", 1, false),
        ];
        write_wikipedia_columnar_store(dir, &articles, &edges)
            .await
            .unwrap();

        // articles.lance round-trips every structural column (sorted by title).
        let mut want = articles.clone();
        want.sort_by(|a, b| a.title.cmp(&b.title));
        assert_eq!(read_articles(dir).await, want);

        // edges.lance round-trips, and carries the fields the neighbor API needs.
        let re = read_edges(dir).await;
        assert_eq!(re.len(), 3);
        let alpha: Vec<&WikiEdgeRow> = re.iter().filter(|e| e.source_title == "Alpha").collect();
        assert_eq!(alpha.len(), 3);
        // axis filtering needs the section path + relationship_type on the edge.
        assert!(alpha.iter().any(|e| e.target_title == "Gamma"
            && e.relationship_type == "contested"
            && e.source_section_path == "Criticism"));
        // dangling/out-of-scope target preserved (the neighbor query filters on it).
        assert!(alpha
            .iter()
            .any(|e| e.target_title == "External" && !e.target_in_scope));
        // occurrence_count survives (the neighbor query SUMs it).
        assert_eq!(
            alpha
                .iter()
                .find(|e| e.target_title == "Beta")
                .unwrap()
                .occurrence_count,
            2
        );
    }

    // ── the direct build (W4) ────────────────────────────────────────────────
    //
    // These carried over from `wikipedia_graph::tests` when the SQLite was
    // retired: they exercise the AGGREGATION, so they belong beside it. The
    // one they replace, `direct_build_matches_sqlite_two_step`, asserted the
    // direct build against the two-step it supersedes and is cited in the
    // retirement commit; it could not survive the backend it compared against.

    use crate::extractors::wikipedia_types::WikiLink;
    use crate::wikipedia_columnar::{ColumnarWikipediaGraph, Neighbor};

    fn meta_with(
        section_path: Vec<&str>,
        section_type: &str,
        pov_count: Option<i64>,
        outgoing: Vec<(&str, &str)>,
    ) -> String {
        let m = WikipediaChunkMetadata {
            section_name: section_path.last().unwrap_or(&"").to_string(),
            section_path: section_path.iter().map(|s| s.to_string()).collect(),
            section_depth: 0,
            section_type: section_type.to_string(),
            citation_needed_count: None,
            pov_count,
            clarification_needed_count: None,
            update_count: None,
            is_flagged_stable: None,
            outgoing_links: outgoing
                .into_iter()
                .map(|(t, l)| WikiLink {
                    target_title: t.to_string(),
                    link_text: l.to_string(),
                })
                .collect(),
            revision_id: Some(42),
            wikidata_qid: None,
            page_id: None,
        };
        serde_json::to_string(&m).unwrap()
    }

    fn chunk(id: u64, title: &str, metadata_raw: String) -> StoredChunkWithMetadata {
        StoredChunkWithMetadata {
            id,
            title: Some(title.to_string()),
            url: Some(format!(
                "https://en.wikipedia.org/wiki/{}",
                title.replace(' ', "_")
            )),
            metadata_raw: Some(metadata_raw),
        }
    }

    /// Einstein links out from a Lead and a Criticism section; Special
    /// relativity links back from Origins, twice (the chunker repeating a
    /// section). Photoelectric effect is linked but never a source, so it is
    /// the dangling target.
    fn fixture() -> Vec<StoredChunkWithMetadata> {
        vec![
            chunk(
                1,
                "Albert Einstein",
                meta_with(
                    vec!["Lead"],
                    "lead",
                    None,
                    vec![
                        ("Special relativity", "special relativity"),
                        ("Photoelectric effect", "photoelectric effect"),
                    ],
                ),
            ),
            chunk(
                2,
                "Albert Einstein",
                meta_with(
                    vec!["Criticism"],
                    "controversy",
                    Some(2),
                    vec![("Special relativity", "criticism of relativity")],
                ),
            ),
            chunk(
                3,
                "Special relativity",
                meta_with(
                    vec!["Origins"],
                    "history",
                    None,
                    vec![
                        ("Albert Einstein", "Einstein"),
                        ("Photoelectric effect", "photoelectric effect"),
                    ],
                ),
            ),
            chunk(
                4,
                "Special relativity",
                meta_with(
                    vec!["Origins"],
                    "history",
                    None,
                    vec![("Albert Einstein", "Einstein")],
                ),
            ),
        ]
    }

    /// The whole `WikipediaGraphApi` surface, from chunks through the direct
    /// build to the reader, against VALUES rather than against another
    /// implementation's output. Every assertion here has a nameable failing
    /// input: drop `target_in_scope` and the dangling assertion goes red; lose
    /// the section split and Einstein's Special-relativity occurrence falls
    /// from 2 to 1; break `classify_relationship` and the `contested` label
    /// goes; drop `source_section_path` from the edge row and the
    /// axis-by-section case returns empty.
    /// A neighbor set as sorted `(title, relationship_type, occurrence, in_scope)`
    /// tuples — the whole answer, so an assertion pins what the API returns
    /// rather than one field of one row.
    fn rows(ns: Vec<Neighbor>) -> Vec<(String, String, i64, bool)> {
        let mut v: Vec<_> = ns
            .into_iter()
            .map(|n| (n.title, n.relationship_type, n.occurrence_count, n.in_scope))
            .collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn direct_build_serves_the_whole_neighbor_api() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let summary = build_wikipedia_columnar_store_from_chunks(dir, "wiki-test", fixture())
            .await
            .unwrap();

        // Two in-scope sources; Photoelectric effect is a target only.
        assert_eq!(summary.articles, 2);
        assert_eq!(summary.dangling_targets, 1);
        assert_eq!(summary.chunks_with_metadata, 4);
        assert_eq!(summary.chunks_without_metadata, 0);
        // Einstein: Lead + Criticism; Special relativity: Origins (the two
        // chunks of it collapse to one section).
        assert_eq!(summary.sections, 3);
        assert_eq!(summary.revision_id_max, Some(42));

        let g = ColumnarWikipediaGraph::open(dir).await.unwrap();
        assert_eq!(g.article_count().await, 2);

        // The whole neighbor set, exactly. Grouping is by (target,
        // relationship_type) — NOT by target — so Einstein's two links to
        // Special relativity stay two rows: the Lead one classifies `topical`,
        // the Criticism one `contested`. Asserting the set rather than a field
        // is what makes that visible; an earlier draft of this test asserted
        // `occurrence_count == 2` for a summed target and was simply wrong
        // about the API.
        assert_eq!(
            rows(g.neighbors("Albert Einstein", 50).await),
            vec![
                ("Photoelectric effect".into(), "topical".into(), 1, false),
                ("Special relativity".into(), "contested".into(), 1, true),
                ("Special relativity".into(), "topical".into(), 1, true),
            ],
        );
        // The dedupe: Special relativity's Origins section is two chunks
        // repeating the same links, and collapses to one edge each. The
        // `Origins` path classifies both `causal`.
        assert_eq!(
            rows(g.neighbors("Special relativity", 50).await),
            vec![
                ("Albert Einstein".into(), "causal".into(), 1, true),
                ("Photoelectric effect".into(), "causal".into(), 1, false),
            ],
        );
        // reverse_neighbors is the in-edge view of the same edges. Note the
        // `in_scope` flag flips meaning with the direction: here the returned
        // article is the SOURCE, and both sources are in-scope, so both are
        // true even though the article being asked about is dangling.
        assert_eq!(
            rows(g.reverse_neighbors("Photoelectric effect", 50).await),
            vec![
                ("Albert Einstein".into(), "topical".into(), 1, true),
                ("Special relativity".into(), "causal".into(), 1, true),
            ],
        );

        // The contested signal rides on the article, from the Criticism section.
        assert!(g.has_contested_section("Albert Einstein").await);
        assert!(!g.has_contested_section("Special relativity").await);

        let rec = g.record("Albert Einstein").await.expect("record");
        assert_eq!(rec.title, "Albert Einstein");
        assert!(rec.in_scope);
        assert_eq!(rec.pov_total, 2);
        assert_eq!(rec.revision_id, Some(42));
    }

    /// The axis filter, one term per column it matches. These three columns are
    /// the reason the wiki store is `edges.lance` and not the `edges.csr`
    /// adjacency — a 10-byte CSR record has nowhere to put a section path or a
    /// link text.
    #[tokio::test]
    async fn axis_filter_matches_section_path_link_text_and_target_title() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        build_wikipedia_columnar_store_from_chunks(dir, "wiki-test", fixture())
            .await
            .unwrap();
        let g = ColumnarWikipediaGraph::open(dir).await.unwrap();

        let hits = |ns: &[Neighbor]| {
            let mut v: Vec<String> = ns.iter().map(|n| n.title.clone()).collect();
            v.sort();
            v
        };

        // (a) matches `source_section_path` — only the Criticism-section link.
        let by_section = g
            .neighbors_for_axis("Albert Einstein", &["criticism".to_string()], 50)
            .await;
        assert_eq!(hits(&by_section), vec!["Special relativity".to_string()]);
        // That link was classified from its section, so it carries the label.
        assert_eq!(by_section[0].relationship_type, "contested");

        // (b) matches `link_text` — "Einstein" is anchor text on Special
        //     relativity's out-edge, and appears in no section path here.
        let by_text = g
            .neighbors_for_axis("Special relativity", &["einstein".to_string()], 50)
            .await;
        assert_eq!(hits(&by_text), vec!["Albert Einstein".to_string()]);

        // (c) matches `target_title`.
        let by_title = g
            .neighbors_for_axis("Albert Einstein", &["photoelectric".to_string()], 50)
            .await;
        assert_eq!(hits(&by_title), vec!["Photoelectric effect".to_string()]);

        // A term matching nothing returns nothing — the failing input that
        // makes the three above mean something.
        let miss = g
            .neighbors_for_axis("Albert Einstein", &["zeppelin".to_string()], 50)
            .await;
        assert!(miss.is_empty());

        // co_neighbors: the concept both articles reach.
        let both = vec![
            "Albert Einstein".to_string(),
            "Special relativity".to_string(),
        ];
        let shared = g.co_neighbors(&both, &[], 50).await;
        assert_eq!(hits(&shared), vec!["Photoelectric effect".to_string()]);
    }

    /// The classifier's section-path rules beat its link-text rules, and the
    /// default is `topical`. Kept from `wikipedia_graph::tests` — the function
    /// moved here, so its test did too.
    #[test]
    fn relationship_classifier_orders_section_over_link_text() {
        // Section path wins even when the link text says "is".
        assert_eq!(
            classify_relationship(&["Criticism".to_string()], "is a physicist"),
            "contested"
        );
        assert_eq!(
            classify_relationship(&["Origins".to_string()], "Industrial Revolution"),
            "causal"
        );
        assert_eq!(
            classify_relationship(&["See also".to_string()], "anything"),
            "see-also"
        );
        // No section signal → link-text verb prefixes.
        assert_eq!(
            classify_relationship(&[], "led to widespread famine"),
            "causal"
        );
        assert_eq!(classify_relationship(&[], "is a mammal"), "defines");
        // Neither → topical.
        assert_eq!(
            classify_relationship(&["Lead".to_string()], "Vienna"),
            "topical"
        );
    }

    /// A chunk with no metadata, and one with metadata but no resolvable
    /// title, are both counted and neither crashes the build.
    #[tokio::test]
    async fn chunks_without_usable_metadata_are_counted_not_dropped_silently() {
        let mut chunks = fixture();
        chunks.push(StoredChunkWithMetadata {
            id: 99,
            title: Some("No Metadata".into()),
            url: None,
            metadata_raw: None,
        });
        chunks.push(StoredChunkWithMetadata {
            id: 100,
            title: Some("Bad Metadata".into()),
            url: None,
            metadata_raw: Some("{not json".into()),
        });
        let (_, _, summary) = wiki_rows_from_chunks("wiki-test", chunks).unwrap();
        assert_eq!(summary.chunks_with_metadata, 4);
        assert_eq!(summary.chunks_without_metadata, 2);
        assert_eq!(summary.articles, 2);
    }

    /// The atom id is a function of the article and its corpus, and of nothing
    /// else — not of position, not of insertion order, not of the rest of the
    /// corpus. That is the whole difference from the counter ids it replaces
    /// (`entity-0001`…, assigned in sorted-title order, so inserting one
    /// article shifted every id after it).
    #[test]
    fn wiki_atom_id_depends_on_the_article_and_the_corpus_and_nothing_else() {
        let a = wiki_atom_id("Roman Empire", "wikipedia");
        assert_eq!(a, wiki_atom_id("Roman Empire", "wikipedia"));
        assert!(a.starts_with("entity-"));
        assert_eq!(a.len(), "entity-".len() + 16);
        // Same title, different corpus → different atom. This is what makes an
        // id unique across the federation without a registry.
        assert_ne!(a, wiki_atom_id("Roman Empire", "wikipedia-newsworthy"));
        // Different title, same corpus → different atom.
        assert_ne!(a, wiki_atom_id("Roman Republic", "wikipedia"));
        // ONE DERIVATION: this must BE `entity_content_hash`, not merely agree
        // with it today. A second hasher for one essence is the §10.6 smell,
        // and it is what an earlier draft of this function was.
        assert_eq!(
            a,
            AtomId::entity_content_hash(
                "Roman Empire",
                &EntityType::from_str_repr(WIKI_ENTITY_TYPE),
                "wikipedia"
            )
            .as_str()
        );
        // The `|` separator inside that hash cannot be smuggled in through a
        // title, because the name is `lookup_key`-normalised first. These two
        // would collide under a naive `"{title}|{corpus}"` join.
        assert_ne!(wiki_atom_id("a|b", "c"), wiki_atom_id("a", "b|c"));
        // Normalisation is real and deliberate: punctuation and case fold away,
        // which is why `wiki_rows_from_chunks` refuses a collision rather than
        // trusting the id to be injective over raw titles.
        assert_eq!(
            wiki_atom_id("Roman Empire", "w"),
            wiki_atom_id("roman  empire!", "w")
        );
    }

    /// The progressive path keeps working across the re-key.
    ///
    /// `wikipedia-fetched` and `wikipedia-newsworthy` write their OWN
    /// atom-class atlases (413 and 14 atoms on this host, all content-hash
    /// ids already) and reach their `wikipedia` twin by atom id — a
    /// `CrossCorpusEdge` carries `peer.corpus_id` + `peer.atom_id`. So the id a
    /// layer mints for "Roman Empire in wikipedia", using the shared
    /// `entity_content_hash` and knowing nothing about this module, must be the
    /// id the rebuilt wiki store holds. That is what makes the edge resolve,
    /// and it is why `wiki_atom_id` delegates rather than agreeing.
    ///
    /// The failing input is the previous scheme: a counter id assigned in
    /// sorted-title order can never be computed by a peer, so no cross-corpus
    /// edge into wikipedia could have resolved by construction.
    #[tokio::test]
    async fn an_id_minted_by_a_peer_layer_resolves_in_the_rebuilt_wiki_store() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        build_wikipedia_columnar_store_from_chunks(
            dir,
            "wikipedia",
            vec![chunk(
                3,
                "Roman Empire",
                meta_with(vec!["Lead"], "lead", None, vec![("Augustus", "Augustus")]),
            )],
        )
        .await
        .unwrap();

        // What a peer layer computes for the SAME article in the parent
        // corpus, through the shared function, with no reference to this file.
        let peer_minted = AtomId::entity_content_hash(
            "Roman Empire",
            &EntityType::from_str_repr("article"),
            "wikipedia",
        );

        use crate::enrichment::atlas::provider::AtlasProvider;
        let p = crate::wikipedia_columnar::WikiAtlasProvider::open(dir, "wikipedia")
            .await
            .unwrap();
        let hit = p
            .atom(peer_minted.as_str())
            .expect("a peer-minted id must resolve in the wiki store");
        assert_eq!(hit.name(), "Roman Empire");
        assert_eq!(p.atom_evidence(peer_minted.as_str())[0].chunk_id(), "3");

        // And the layer's own corpus gives a DIFFERENT id for the same title,
        // which is the point of corpus-qualifying: the twin is a distinct atom
        // in a distinct corpus, reached by an edge rather than confused with it.
        assert_ne!(
            peer_minted.as_str(),
            wiki_atom_id("Roman Empire", "wikipedia-fetched")
        );
    }

    /// The evidence anchor is the LOWEST chunk id of the article, so a rebuild
    /// picks the same chunk whatever order the index streams rows in. The
    /// fixture feeds the higher id first for exactly that reason.
    #[tokio::test]
    async fn chunk_anchor_is_the_lowest_chunk_not_the_first_seen() {
        let chunks = vec![
            chunk(
                900,
                "Albert Einstein",
                meta_with(vec!["Later"], "body", None, vec![("X", "x")]),
            ),
            chunk(
                7,
                "Albert Einstein",
                meta_with(vec!["Lead"], "lead", None, vec![("Y", "y")]),
            ),
        ];
        let (articles, _, _) = wiki_rows_from_chunks("wikipedia", chunks).unwrap();
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].chunk_id, "7");
        assert_eq!(
            articles[0].atom_id,
            wiki_atom_id("Albert Einstein", "wikipedia")
        );
    }

    /// A v1 store — no `atom_id`, no `chunk_id` — still serves the neighbor API
    /// and says so about the walk. The failing input is a real one: it is the
    /// shape of every `articles.lance` written before 2026-09-04, including the
    /// installed wikipedia index at the time of the change.
    #[tokio::test]
    async fn a_v1_store_serves_neighbors_and_reports_that_it_cannot_serve_the_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // The v1 articles schema, verbatim: seven columns, no atom_id/chunk_id.
        let v1 = Arc::new(Schema::new(vec![
            Field::new("title", DataType::Utf8, false),
            Field::new("wikidata_qid", DataType::Utf8, false),
            Field::new("revision_id", DataType::Int64, false),
            Field::new("in_scope", DataType::Boolean, false),
            Field::new("pov_total", DataType::Int64, false),
            Field::new("citation_total", DataType::Int64, false),
            Field::new("is_contested", DataType::Boolean, false),
        ]));
        let batch = RecordBatch::try_new(
            v1.clone(),
            vec![
                Arc::new(StringArray::from(vec!["Alpha", "Beta"])) as arrow_array::ArrayRef,
                Arc::new(StringArray::from(vec!["", ""])),
                Arc::new(Int64Array::from(vec![-1i64, -1])),
                Arc::new(BooleanArray::from(vec![true, true])),
                Arc::new(Int64Array::from(vec![0i64, 0])),
                Arc::new(Int64Array::from(vec![0i64, 0])),
                Arc::new(BooleanArray::from(vec![false, false])),
            ],
        )
        .unwrap();
        write_table(
            dir,
            ARTICLES_LANCE_DIRNAME,
            ARTICLES_TABLE,
            v1,
            vec![batch],
            None,
        )
        .await
        .unwrap();
        // Edges are unchanged between v1 and v2, so the real writer serves.
        let esch = edges_schema();
        let erows = vec![edge("Alpha", "Beta", "topical", "Lead", 1, true)];
        write_table(
            dir,
            EDGES_LANCE_DIRNAME,
            EDGES_TABLE,
            esch.clone(),
            vec![edges_batch(&erows, &esch).unwrap()],
            Some("source_title"),
        )
        .await
        .unwrap();

        let g = ColumnarWikipediaGraph::open(dir).await.unwrap();
        // It knows what it is …
        assert!(!g.has_v2_columns());
        // … and the neighbor API is entirely unaffected by the missing columns.
        let n = g.neighbors("Alpha", 10).await;
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "Beta");
        assert!(g.record("Alpha").await.is_some());

        // A store the current writer produces answers the other way.
        let tmp2 = tempfile::tempdir().unwrap();
        build_wikipedia_columnar_store_from_chunks(tmp2.path(), "wiki-test", fixture())
            .await
            .unwrap();
        assert!(ColumnarWikipediaGraph::open(tmp2.path())
            .await
            .unwrap()
            .has_v2_columns());
    }
}
