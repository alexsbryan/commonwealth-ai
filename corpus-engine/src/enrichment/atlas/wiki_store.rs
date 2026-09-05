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
/// [`AtomId::exact_entity_content_hash`] — it does not hash anything itself.
/// A wiki article's identity in `wikipedia` and in its fetched/newsworthy twin
/// must come off the same function or the two derivations drift; the ids stay
/// DISTINCT because `corpus_id` is part of the essence, which is by design.
/// (An earlier draft hashed its own `(title, corpus_id)` framing here — a
/// second hasher for one essence, which is exactly the smell §10.6 names.)
///
/// It delegates to the EXACT constructor, not to [`AtomId::entity_content_hash`],
/// and that is the whole of the 2026-09-04 fix. The folded constructor keys on
/// `canonical::lookup_key(title)`, which is the right equivalence for an
/// LLM-extracted name and the wrong one for a Wikipedia title: MediaWiki
/// serves `Jigsaw puzzle` and `Jigsaw Puzzle` as two pages, and folding makes
/// them one atom. The first full rebuild died on exactly that pair. Measured
/// over the whole live title namespace (1,562,311 titles in
/// `wikipedia_graph.db`): 38,259 folded keys carry more than one title and
/// 40,869 titles — 2.62% — would have been silently merged. The collision is
/// in the key, before the hash, so widening `short_hash` fixes nothing.
///
/// WHAT A PEER MUST CALL (the property this id exists to give). Anyone holding
/// an exact title and a corpus id can compute a wikipedia atom id with no
/// registry — that is what lets a `CrossCorpusEdge` from `wikipedia-fetched`
/// name its twin in `wikipedia`. The property survives the fix; the function
/// a peer has to agree on is now `exact_entity_content_hash`. Note what did
/// NOT change: `structure_first` and `newsworthy_events` still mint their OWN
/// corpora's atoms through the folded `entity_content_hash`, because those
/// paths also serve non-wiki corpora (`update::delta`, `newsworthy_host`) and
/// re-keying them would re-key every structural atlas in the fleet. Inside a
/// child layer's own corpus those ids stay folded, and a child layer that
/// wants its parent's id must route through THIS function.
///
/// Identity from ESSENCE, not from a counter (ARCH §7.5). Wikipedia's atom ids
/// were `entity-0001`, `entity-0002`, … assigned in sorted-title order, so
/// inserting one article shifted the id of every article after it: stable only
/// for a corpus that never changed, which is not a property anything can safely
/// cite. The hash is stable under insertion, deletion and rebuild — which is
/// what makes it CORRECT for the progressive path: a re-fetched or re-watched
/// title keeps its id instead of being renumbered by its neighbours.
///
/// Framing is unambiguous by length-prefix inside the constructor, so a title
/// containing the field separator cannot be read as two fields.
/// [`wiki_rows_from_chunks`] still REFUSES a build on a duplicate id — now as
/// a backstop against a genuine 64-bit hash collision rather than against the
/// normalisation, which no longer folds anything (§18.1).
pub fn wiki_atom_id(title: &str, corpus_id: &str) -> String {
    AtomId::exact_entity_content_hash(
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
                     {corpus_id} — two distinct titles hashed to one 64-bit id; widen \
                     short_hash before rebuilding",
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

/// What the borrowed seed-table build actually did. Every number is a
/// SEPARATE fact: an article with no chunk anchor and an article whose chunk
/// carries no vector are different failures, and collapsing them into one
/// "resolved" would hide which (ARCH §18.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowedSeedStats {
    /// Rows in `articles.lance`.
    pub articles: usize,
    /// …of which carry a parseable `chunk_id` (the join key).
    pub with_chunk_anchor: usize,
    /// …of which resolved to a vector in `chunks.lance`.
    pub resolved: usize,
    /// Rows written to `atoms_ann.lance`.
    pub written: usize,
}

impl BorrowedSeedStats {
    /// One line for an operator surface; names each drop rather than
    /// reporting a single ratio.
    pub fn describe(&self) -> String {
        format!(
            "{} rows from {} articles — {} carried a chunk anchor, {} of those \
             resolved to a chunk vector",
            self.written, self.articles, self.with_chunk_anchor, self.resolved
        )
    }
}

/// Build a wiki-class atlas's ANN seed table by BORROWING each article's
/// chunk vector — zero embed calls.
///
/// **Why borrowed and not embedded — the reason is the TEXT, not the clock.**
/// A wiki atom's whole embed text is its bare title: 214 of 221 sampled
/// articles have an empty `description`
/// (`sovereign/bench/wikipedia/seed_migration`). Embedding those fresh spends
/// an hour indexing the least informative string the store holds. (The 33.1 h
/// figure in the probe is the v1 atom set's — 1.67M atoms including dangling
/// link targets. This store holds the 51,781 IN-SCOPE articles, so the fresh
/// path here is ~1 h at the probe's measured rate. Cost is why the question
/// was asked; it is not why the answer is `borrow`.)
///
/// The chunk the atom already cites — its lead passage — is embedded, resident
/// in `chunks.lance`, and in the same vector space the query slot runs. The
/// cosine between the two is 0.323 median, so this is a SUBSTITUTION and not
/// an equivalence: the walk seeds on the article's lead passage rather than on
/// its title. That is named here, named in the CLI's output, and it is what
/// the wikipedia lane A/B measures.
///
/// **The join.** `articles.lance` already carries both handles — `atom_id`
/// (the walk's key) and `chunk_id` (the evidence anchor, the LOWEST chunk id
/// of the article, so it is the lead section). So there is no derivation
/// here, only a lookup: atom_id ← article → chunk_id → `chunks.lance`
/// vector. The chunk-side dedupe rule lives with the reader that needs it
/// ([`crate::index::CorpusIndex::embeddings_for_chunk_ids`]).
///
/// **The writer is the existing one.** [`build_persistent_ann_seed_table`] is
/// the ONE `atoms_ann.lance` writer (ARCH §10.6) and it takes an
/// [`AtlasContext`], so this hands it one built from borrowed vectors instead
/// of embedded ones. `AtlasSeeding` gains no arm: that enum is about which
/// LIFECYCLE POINT seeds an atlas write, and this is not an atlas write.
pub async fn build_borrowed_ann_seed_table(
    atlas_dir: &Path,
    index: &crate::index::CorpusIndex,
    atlas_corpus_id: &str,
) -> Result<BorrowedSeedStats, String> {
    use crate::enrichment::atlas::context::{
        build_persistent_ann_seed_table, AtlasContext, AtlasEntry,
    };

    let graph = crate::wikipedia_columnar::ColumnarWikipediaGraph::open(atlas_dir).await?;
    if !graph.has_v2_columns() {
        return Err(format!(
            "wiki store at {} is format v1 (no atom_id/chunk_id): there is no join key to \
             borrow a vector by — rebuild with `svrn atlas wikipedia build-graph`",
            atlas_dir.display()
        ));
    }
    let rows = graph.article_rows().await?;
    let articles = rows.len();

    let mut wanted: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut anchored: Vec<(String, String, u64)> = Vec::new(); // (atom_id, title, chunk_id)
    for r in &rows {
        let Ok(chunk_id) = r.chunk_id.parse::<u64>() else {
            continue;
        };
        if r.atom_id.is_empty() {
            continue;
        }
        wanted.insert(chunk_id);
        anchored.push((r.atom_id.clone(), r.title.clone(), chunk_id));
    }
    let with_chunk_anchor = anchored.len();
    drop(rows);

    let vectors = index
        .embeddings_for_chunk_ids(&wanted)
        .await
        .map_err(|e| format!("borrowed seed table: chunk-vector join: {e}"))?;

    let mut entries: Vec<AtlasEntry> = Vec::with_capacity(anchored.len());
    for (atom_id, title, chunk_id) in anchored {
        let Some(embedding) = vectors.get(&chunk_id) else {
            continue;
        };
        entries.push(AtlasEntry {
            atom_id,
            canonical_name: title.clone(),
            // The atom's own text, not the chunk's. `embed_text` is what a
            // reader sees when asking WHAT this seed stands for, and the
            // answer is the article — the vector is borrowed, the identity
            // is not.
            embed_text: title,
            embedding: embedding.clone(),
        });
    }
    let resolved = entries.len();
    if resolved == 0 {
        return Err(format!(
            "borrowed seed table: 0 of {with_chunk_anchor} anchored articles resolved to a \
             chunk vector in the index — nothing to write"
        ));
    }

    let stats = build_persistent_ann_seed_table(
        atlas_dir,
        &AtlasContext {
            atlas_corpus_id: atlas_corpus_id.to_string(),
            entries,
            top_k: 12,
        },
    )
    .await?;

    let out = BorrowedSeedStats {
        articles,
        with_chunk_anchor,
        resolved,
        written: stats.resolved,
    };
    tracing::info!(
        corpus = atlas_corpus_id,
        atlas = %atlas_dir.display(),
        articles = out.articles,
        with_chunk_anchor = out.with_chunk_anchor,
        resolved = out.resolved,
        written = out.written,
        "wiki seed table: built from borrowed chunk vectors (0 embed calls)"
    );
    Ok(out)
}

#[cfg(test)]
mod tests;
