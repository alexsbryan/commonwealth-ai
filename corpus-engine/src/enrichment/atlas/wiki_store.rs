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
//! W1 is the writer + schema only (dormant). The source wiring (SQLite
//! `WikipediaGraph` → these tables) is W1b; the reader is W2.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{BooleanArray, Int64Array, RecordBatch, StringArray};

/// Wiki columnar store schema version. Bump on any `articles.lance` /
/// `edges.lance` column change (e.g. the Layer-1 cluster/bridge columns or a
/// future article-embedding column).
pub const WIKI_STORE_FORMAT_VERSION: u32 = 1;

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
        Field::new("title", DataType::Utf8, false),
        Field::new("wikidata_qid", DataType::Utf8, false),
        Field::new("revision_id", DataType::Int64, false),
        Field::new("in_scope", DataType::Boolean, false),
        Field::new("pov_total", DataType::Int64, false),
        Field::new("citation_total", DataType::Int64, false),
        Field::new("is_contested", DataType::Boolean, false),
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
    let articles_path =
        write_table(atlas_dir, ARTICLES_LANCE_DIRNAME, ARTICLES_TABLE, asch, abatches, None).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;
    use futures::TryStreamExt;
    use lancedb::query::ExecutableQuery;

    fn art(title: &str, pov: i64) -> WikiArticleRow {
        WikiArticleRow {
            title: title.into(),
            wikidata_qid: format!("Q-{title}"),
            revision_id: 100,
            in_scope: true,
            pov_total: pov,
            citation_total: 5,
            is_contested: pov > 0,
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
        let db = lancedb::connect(atlas_dir.to_str().unwrap()).execute().await.unwrap();
        let tbl = db.open_table(ARTICLES_TABLE).execute().await.unwrap();
        let batches: Vec<RecordBatch> =
            tbl.query().execute().await.unwrap().try_collect().await.unwrap();
        let s = |b: &RecordBatch, n| b.column_by_name(n).unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
        let i = |b: &RecordBatch, n| b.column_by_name(n).unwrap().as_any().downcast_ref::<Int64Array>().unwrap().clone();
        let bo = |b: &RecordBatch, n| b.column_by_name(n).unwrap().as_any().downcast_ref::<BooleanArray>().unwrap().clone();
        let mut out = Vec::new();
        for b in &batches {
            let (title, qid) = (s(b, "title"), s(b, "wikidata_qid"));
            let (rev, pov, cit) = (i(b, "revision_id"), i(b, "pov_total"), i(b, "citation_total"));
            let (insc, cont) = (bo(b, "in_scope"), bo(b, "is_contested"));
            for k in 0..b.num_rows() {
                out.push(WikiArticleRow {
                    title: title.value(k).to_string(),
                    wikidata_qid: qid.value(k).to_string(),
                    revision_id: rev.value(k),
                    in_scope: insc.value(k),
                    pov_total: pov.value(k),
                    citation_total: cit.value(k),
                    is_contested: cont.value(k),
                });
            }
        }
        out.sort_by(|a, b| a.title.cmp(&b.title));
        out
    }

    async fn read_edges(atlas_dir: &Path) -> Vec<WikiEdgeRow> {
        let db = lancedb::connect(atlas_dir.to_str().unwrap()).execute().await.unwrap();
        let tbl = db.open_table(EDGES_TABLE).execute().await.unwrap();
        let batches: Vec<RecordBatch> =
            tbl.query().execute().await.unwrap().try_collect().await.unwrap();
        let s = |b: &RecordBatch, n| b.column_by_name(n).unwrap().as_any().downcast_ref::<StringArray>().unwrap().clone();
        let i = |b: &RecordBatch, n| b.column_by_name(n).unwrap().as_any().downcast_ref::<Int64Array>().unwrap().clone();
        let bo = |b: &RecordBatch, n| b.column_by_name(n).unwrap().as_any().downcast_ref::<BooleanArray>().unwrap().clone();
        let mut out = Vec::new();
        for b in &batches {
            let (src, tgt, rel, lt, sect) = (s(b, "source_title"), s(b, "target_title"), s(b, "relationship_type"), s(b, "link_text"), s(b, "source_section_path"));
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
        assert!(alpha.iter().any(|e| e.target_title == "External" && !e.target_in_scope));
        // occurrence_count survives (the neighbor query SUMs it).
        assert_eq!(
            alpha.iter().find(|e| e.target_title == "Beta").unwrap().occurrence_count,
            2
        );
    }
}
