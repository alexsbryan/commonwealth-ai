// SPDX-License-Identifier: AGPL-3.0-or-later
//! The wiki link graph — `articles.lance` + `edges.lance`, and the only backend
//! that serves it.
//!
//! `ColumnarWikipediaGraph` answers the [`WikipediaGraphApi`] surface —
//! `neighbors` / `neighbors_for_axis` / `co_neighbors` / `reverse_neighbors` /
//! `has_contested_section` / `record` — over the v2 columnar store written by
//! [`crate::enrichment::atlas::wiki_store`], via Lance predicate queries plus
//! Rust-side aggregation.
//!
//! The wiki neighbor queries are predicate-shaped — axis filtering matches a
//! link's `source_section_path` / `link_text` / `target_title` — which is
//! Lance's strength: `WHERE source_title = ?` with predicate pushdown over a
//! BTree scalar index, then the `GROUP BY (target, rel) / SUM(occurrence) /
//! ORDER / LIMIT` folded in Rust over the bounded per-article edge set.
//!
//! **W4 (2026-09-04): the SQLite `wikipedia_graph.db` is retired** and this is
//! the sole backend. It was never the model — it was a build aggregator that
//! `export_columnar` dumped back out to these same two tables — and
//! `wiki_store::wiki_rows_from_chunks` now writes them from the chunks
//! directly. `open_wikipedia_graph` has no fallback left to pick.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use arrow_array::{Array, BooleanArray, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::enrichment::atlas::ann_store::AnnSeedTable;
use crate::enrichment::atlas::atoms::{AtomType, ChunkRef};
use crate::enrichment::atlas::context::{AtomView, EdgeView, EvidenceRef};
use crate::enrichment::atlas::edges::{EdgeProvenance, EdgeType};
use crate::enrichment::atlas::evidence_site::EvidenceSite;
use crate::enrichment::atlas::projection::AtomRecord;
use crate::enrichment::atlas::provider::AtlasProvider;
use crate::enrichment::atlas::wiki_store::{WikiArticleRow, ARTICLES_TABLE, EDGES_TABLE};
use crate::enrichment::ontology::OntologyPolicies;

// ─── The wiki link-graph query surface ───────────────────────────────────────
//
// `Neighbor` / `ArticleRecord` / `WikipediaGraphApi` moved here from
// `wikipedia_graph` when the SQLite backend was retired (WIKIPEDIA_ATLAS_V2 W4).
// They live with the reader that serves them: there is one backend now, and a
// type whose only implementor is in this file has no reason to sit in another.

/// A one-hop neighbor in the link graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Neighbor {
    /// Target article title, canonical form (spaces, not underscores).
    pub title: String,
    /// Coarse relationship class derived from section + link text.
    /// One of `topical | causal | contested | defines | action | see-also`.
    /// Open text carried as DATA beside the closed `EdgeType` kind, never as a
    /// new enum arm (ARCH principle 9; EPISTEMIC_INDEX one-store-provider).
    pub relationship_type: String,
    /// How many sections of the source article link here. Higher =
    /// stronger structural signal, and the ranking key: the neighbor
    /// queries order by this summed across sections.
    pub occurrence_count: i64,
    /// True iff the target is itself an in-scope article (it has an
    /// `articles.lance` row). Dangling link targets are `false`.
    pub in_scope: bool,
}

/// A single article record. Exposed so callers can read derived
/// signals (cluster_id, bridge_score, contested totals) without a
/// second round-trip when both are needed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArticleRecord {
    pub title: String,
    pub wikidata_qid: Option<String>,
    pub revision_id: Option<i64>,
    pub in_scope: bool,
    /// Layer-1 slot (HDBSCAN cluster). Not written yet — always `None`.
    pub cluster_id: Option<i64>,
    /// Layer-1 slot (bridge detection). Not written yet — always `None`.
    pub bridge_score: Option<f64>,
    pub pov_total: i64,
    pub citation_total: i64,
}

/// The query surface the runtime consumes from a Wikipedia link graph.
/// `#[async_trait]` keeps it `dyn`-safe (the methods are async) so the runtime
/// holds `Arc<dyn WikipediaGraphApi>` (`LaneSources::wikipedia_graph`).
///
/// One implementor since W4: [`ColumnarWikipediaGraph`]. The trait stays
/// because the runtime, the bridge builder and the CLI all hold the graph
/// behind it, and because the one-store-provider work gives wiki-class a
/// second face (`AtlasProvider`) over the same two tables.
#[async_trait::async_trait]
pub trait WikipediaGraphApi: Send + Sync {
    async fn neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor>;
    async fn neighbors_for_axis(
        &self,
        title: &str,
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor>;
    async fn co_neighbors(
        &self,
        titles: &[String],
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor>;
    async fn reverse_neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor>;
    async fn has_contested_section(&self, title: &str) -> bool;
    async fn record(&self, title: &str) -> Option<ArticleRecord>;
    async fn article_count(&self) -> usize;
    async fn edge_count(&self) -> usize;
}

/// Columnar (`articles.lance` + `edges.lance`) reader for the wiki link graph —
/// the v2 replacement for the SQLite `WikipediaGraph`, same query API.
pub struct ColumnarWikipediaGraph {
    articles: lancedb::Table,
    edges: lancedb::Table,
    /// Whether `articles.lance` carries the v2 columns (`atom_id`, `chunk_id`).
    /// Read once at open, from the SCHEMA rather than from a version file, so a
    /// store cannot claim a shape it does not have.
    ///
    /// The neighbor API never touches those columns, so a v1 store keeps
    /// serving `neighbors` / `record` / the axis filters exactly as before —
    /// this flag exists so the OTHER face of the store, the grounding-walk
    /// provider, can refuse a v1 store by name instead of returning atoms with
    /// no id and no evidence (ARCH §18.3).
    has_v2_columns: bool,
}

/// One edge row's queried fields (the columns the neighbor API reads).
struct EdgeLite {
    source_title: String,
    target_title: String,
    relationship_type: String,
    link_text: String,
    occurrence_count: i64,
    source_section_path: String,
    target_in_scope: bool,
}

/// A SQL string literal with single quotes escaped, for a Lance `only_if` filter.
fn sql_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

impl ColumnarWikipediaGraph {
    /// Open the columnar store under `atlas_dir` (`articles.lance` +
    /// `edges.lance`). Async (Lance open); the query methods are async too,
    /// matching the SQLite graph's signatures so it slots behind the same trait.
    pub async fn open(atlas_dir: &Path) -> Result<Self, String> {
        let uri = atlas_dir
            .to_str()
            .ok_or_else(|| format!("non-utf8 {}", atlas_dir.display()))?;
        let db = lancedb::connect(uri)
            .execute()
            .await
            .map_err(|e| format!("connect {uri}: {e}"))?;
        let articles = db
            .open_table(ARTICLES_TABLE)
            .execute()
            .await
            .map_err(|e| format!("open articles.lance: {e}"))?;
        let edges = db
            .open_table(EDGES_TABLE)
            .execute()
            .await
            .map_err(|e| format!("open edges.lance: {e}"))?;
        let schema = articles
            .schema()
            .await
            .map_err(|e| format!("read articles.lance schema: {e}"))?;
        let has = |c: &str| schema.field_with_name(c).is_ok();
        let has_v2_columns = has("atom_id") && has("chunk_id");
        if !has_v2_columns {
            tracing::info!(
                dir = %atlas_dir.display(),
                "wiki columnar store is format v1 (no atom_id/chunk_id): neighbor API serves, \
                 grounding-walk provider will refuse; rebuild with `atlas wikipedia build-graph`"
            );
        }
        Ok(Self {
            articles,
            edges,
            has_v2_columns,
        })
    }

    /// Whether this store carries the v2 article columns — see
    /// [`WIKI_STORE_FORMAT_VERSION`](crate::enrichment::atlas::wiki_store::WIKI_STORE_FORMAT_VERSION).
    /// The grounding-walk provider requires them; the neighbor API does not.
    pub fn has_v2_columns(&self) -> bool {
        self.has_v2_columns
    }

    /// Every article row, for a consumer that needs the whole table resident
    /// (the walk provider). A full columnar scan — lifecycle-time only, never
    /// the query path.
    pub async fn article_rows(&self) -> Result<Vec<WikiArticleRow>, String> {
        let stream = self
            .articles
            .query()
            .execute()
            .await
            .map_err(|e| format!("scan articles.lance: {e}"))?;
        let batches: Vec<RecordBatch> = stream
            .try_collect()
            .await
            .map_err(|e| format!("collect articles.lance: {e}"))?;
        let mut out = Vec::new();
        for b in &batches {
            let s = |n: &str| {
                b.column_by_name(n)
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>().cloned())
                    .ok_or_else(|| format!("articles.lance: missing string column {n}"))
            };
            let i = |n: &str| {
                b.column_by_name(n)
                    .and_then(|c| c.as_any().downcast_ref::<Int64Array>().cloned())
                    .ok_or_else(|| format!("articles.lance: missing i64 column {n}"))
            };
            let bo = |n: &str| {
                b.column_by_name(n)
                    .and_then(|c| c.as_any().downcast_ref::<BooleanArray>().cloned())
                    .ok_or_else(|| format!("articles.lance: missing bool column {n}"))
            };
            let (aid, title, qid, cid) = (
                s("atom_id")?,
                s("title")?,
                s("wikidata_qid")?,
                s("chunk_id")?,
            );
            let (rev, pov, cit) = (i("revision_id")?, i("pov_total")?, i("citation_total")?);
            let (insc, cont) = (bo("in_scope")?, bo("is_contested")?);
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
        Ok(out)
    }

    /// Call `f(source_title, target_title)` once per edge row.
    ///
    /// A callback rather than a `Vec<(String, String)>` on purpose: the full
    /// wiki graph is ~7.3M rows, and materialising two owned `String`s per row
    /// costs several hundred MB of transient allocation for data the caller
    /// interns to a `u32` immediately. This hands out `&str` off the Arrow
    /// batch and lets the caller keep only what it needs.
    pub async fn for_each_edge_pair(&self, mut f: impl FnMut(&str, &str)) -> Result<usize, String> {
        let stream = self
            .edges
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "source_title".into(),
                "target_title".into(),
            ]))
            .execute()
            .await
            .map_err(|e| format!("scan edges.lance: {e}"))?;
        let batches: Vec<RecordBatch> = stream
            .try_collect()
            .await
            .map_err(|e| format!("collect edges.lance: {e}"))?;
        let mut n = 0usize;
        for b in &batches {
            let col = |name: &str| {
                b.column_by_name(name)
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>().cloned())
                    .ok_or_else(|| format!("edges.lance: missing string column {name}"))
            };
            let (src, tgt) = (col("source_title")?, col("target_title")?);
            for k in 0..b.num_rows() {
                f(src.value(k), tgt.value(k));
                n += 1;
            }
        }
        Ok(n)
    }

    /// Edge rows matching a Lance `only_if` filter. Errors degrade to an empty
    /// set (mirrors the SQLite graph's error-swallowing query helpers).
    async fn edge_rows(&self, filter: String) -> Vec<EdgeLite> {
        let Ok(stream) = self.edges.query().only_if(filter).execute().await else {
            return Vec::new();
        };
        let batches: Vec<RecordBatch> = match stream.try_collect().await {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for b in &batches {
            let s = |n: &str| {
                b.column_by_name(n)
                    .and_then(|c| c.as_any().downcast_ref::<StringArray>().cloned())
            };
            let i = |n: &str| {
                b.column_by_name(n)
                    .and_then(|c| c.as_any().downcast_ref::<Int64Array>().cloned())
            };
            let bo = |n: &str| {
                b.column_by_name(n)
                    .and_then(|c| c.as_any().downcast_ref::<BooleanArray>().cloned())
            };
            let (Some(src), Some(tgt), Some(rel), Some(lt), Some(sect)) = (
                s("source_title"),
                s("target_title"),
                s("relationship_type"),
                s("link_text"),
                s("source_section_path"),
            ) else {
                continue;
            };
            let (Some(occ), Some(tin)) = (i("occurrence_count"), bo("target_in_scope")) else {
                continue;
            };
            for k in 0..b.num_rows() {
                out.push(EdgeLite {
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

    /// Outbound neighbors of `title`, grouped by (target, relationship_type),
    /// ranked by summed occurrence.
    pub async fn neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        let rows = self
            .edge_rows(format!("source_title = {}", sql_lit(title)))
            .await;
        fold_by_target_rel(rows, limit)
    }

    /// Axis-filtered outbound neighbors — keep edges whose `target_title`,
    /// `link_text`, or `source_section_path` contains any axis term
    /// (case-insensitive).
    pub async fn neighbors_for_axis(
        &self,
        title: &str,
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        let terms = lower_terms(axis_terms);
        if terms.is_empty() {
            return Vec::new();
        }
        let rows: Vec<EdgeLite> = self
            .edge_rows(format!("source_title = {}", sql_lit(title)))
            .await
            .into_iter()
            .filter(|e| edge_matches_axis(e, &terms))
            .collect();
        fold_by_target_rel(rows, limit)
    }

    /// Co-citation: targets linked from EVERY input title (intersection),
    /// axis-filtered, ranked by summed occurrence. Mirrors
    pub async fn co_neighbors(
        &self,
        titles: &[String],
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        if titles.len() < 2 {
            return Vec::new();
        }
        let in_clause = titles
            .iter()
            .map(|t| sql_lit(t))
            .collect::<Vec<_>>()
            .join(", ");
        let terms = lower_terms(axis_terms);
        let mut rows = self
            .edge_rows(format!("source_title IN ({in_clause})"))
            .await;
        if !terms.is_empty() {
            rows.retain(|e| edge_matches_axis(e, &terms));
        }
        struct Acc {
            rel: String,
            occ: i64,
            in_scope: bool,
            sources: HashSet<String>,
        }
        let mut by_target: HashMap<String, Acc> = HashMap::new();
        for e in rows {
            let a = by_target
                .entry(e.target_title.clone())
                .or_insert_with(|| Acc {
                    rel: e.relationship_type.clone(),
                    occ: 0,
                    in_scope: false,
                    sources: HashSet::new(),
                });
            a.occ += e.occurrence_count;
            a.in_scope |= e.target_in_scope;
            if e.relationship_type < a.rel {
                a.rel = e.relationship_type.clone(); // MIN(relationship_type)
            }
            a.sources.insert(e.source_title);
        }
        // Distinct source titles required to reach a target for it to count as
        // co-cited (HAVING distinct_sources = n_required in the SQLite).
        let required = titles.iter().collect::<HashSet<_>>().len();
        let mut out: Vec<Neighbor> = by_target
            .into_iter()
            .filter(|(_, a)| a.sources.len() == required)
            .map(|(target, a)| Neighbor {
                title: target,
                relationship_type: a.rel,
                occurrence_count: a.occ,
                in_scope: a.in_scope,
            })
            .collect();
        out.sort_by(|x, y| {
            y.occurrence_count
                .cmp(&x.occurrence_count)
                .then_with(|| x.title.cmp(&y.title))
        });
        out.truncate(limit);
        out
    }

    /// Inbound neighbors (articles linking TO `title`), grouped by
    /// (source, relationship_type).
    /// (`in_scope = true` — the source is an in-scope article by construction).
    pub async fn reverse_neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        let rows = self
            .edge_rows(format!("target_title = {}", sql_lit(title)))
            .await;
        let mut by: HashMap<(String, String), i64> = HashMap::new();
        for e in rows {
            *by.entry((e.source_title, e.relationship_type)).or_insert(0) += e.occurrence_count;
        }
        let mut out: Vec<Neighbor> = by
            .into_iter()
            .map(|((src, rel), occ)| Neighbor {
                title: src,
                relationship_type: rel,
                occurrence_count: occ,
                in_scope: true,
            })
            .collect();
        out.sort_by(|x, y| {
            y.occurrence_count
                .cmp(&x.occurrence_count)
                .then_with(|| x.title.cmp(&y.title))
        });
        out.truncate(limit);
        out
    }

    /// The 1-row articles batch for `title`, or `None`.
    async fn article_row(&self, title: &str) -> Option<RecordBatch> {
        let stream = self
            .articles
            .query()
            .only_if(format!("title = {}", sql_lit(title)))
            .limit(1)
            .execute()
            .await
            .ok()?;
        let batches: Vec<RecordBatch> = stream.try_collect().await.ok()?;
        batches.into_iter().find(|b| b.num_rows() > 0)
    }

    /// Whether the article has any contested section. Mirrors
    pub async fn has_contested_section(&self, title: &str) -> bool {
        let Some(b) = self.article_row(title).await else {
            return false;
        };
        b.column_by_name("is_contested")
            .and_then(|c| {
                c.as_any()
                    .downcast_ref::<BooleanArray>()
                    .map(|a| a.value(0))
            })
            .unwrap_or(false)
    }

    /// Full article record. `cluster_id` / `bridge_score` are Layer-1 slots not
    /// yet in the columnar store → `None`.
    pub async fn record(&self, title: &str) -> Option<ArticleRecord> {
        let b = self.article_row(title).await?;
        let s = |n: &str| {
            b.column_by_name(n).and_then(|c| {
                c.as_any()
                    .downcast_ref::<StringArray>()
                    .map(|a| a.value(0).to_string())
            })
        };
        let i = |n: &str| {
            b.column_by_name(n)
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>().map(|a| a.value(0)))
        };
        let bo = |n: &str| {
            b.column_by_name(n).and_then(|c| {
                c.as_any()
                    .downcast_ref::<BooleanArray>()
                    .map(|a| a.value(0))
            })
        };
        Some(ArticleRecord {
            title: s("title")?,
            wikidata_qid: s("wikidata_qid").filter(|q| !q.is_empty()),
            revision_id: i("revision_id").filter(|r| *r >= 0),
            in_scope: bo("in_scope").unwrap_or(false),
            cluster_id: None,
            bridge_score: None,
            pov_total: i("pov_total").unwrap_or(0),
            citation_total: i("citation_total").unwrap_or(0),
        })
    }

    /// Articles in scope.
    pub async fn article_count(&self) -> usize {
        self.articles
            .count_rows(Some("in_scope = true".to_string()))
            .await
            .unwrap_or(0)
    }

    /// Total edge rows (per `(source, section, target)`). Mirrors
    pub async fn edge_count(&self) -> usize {
        self.edges.count_rows(None).await.unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl WikipediaGraphApi for ColumnarWikipediaGraph {
    async fn neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        ColumnarWikipediaGraph::neighbors(self, title, limit).await
    }
    async fn neighbors_for_axis(
        &self,
        title: &str,
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        ColumnarWikipediaGraph::neighbors_for_axis(self, title, axis_terms, limit).await
    }
    async fn co_neighbors(
        &self,
        titles: &[String],
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        ColumnarWikipediaGraph::co_neighbors(self, titles, axis_terms, limit).await
    }
    async fn reverse_neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        ColumnarWikipediaGraph::reverse_neighbors(self, title, limit).await
    }
    async fn has_contested_section(&self, title: &str) -> bool {
        ColumnarWikipediaGraph::has_contested_section(self, title).await
    }
    async fn record(&self, title: &str) -> Option<ArticleRecord> {
        ColumnarWikipediaGraph::record(self, title).await
    }
    async fn article_count(&self) -> usize {
        ColumnarWikipediaGraph::article_count(self).await
    }
    async fn edge_count(&self) -> usize {
        ColumnarWikipediaGraph::edge_count(self).await
    }
}

fn lower_terms(axis_terms: &[String]) -> Vec<String> {
    axis_terms
        .iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

fn edge_matches_axis(e: &EdgeLite, terms: &[String]) -> bool {
    let t = e.target_title.to_lowercase();
    let l = e.link_text.to_lowercase();
    let s = e.source_section_path.to_lowercase();
    terms.iter().any(|term| {
        t.contains(term.as_str()) || l.contains(term.as_str()) || s.contains(term.as_str())
    })
}

/// Group edges by (target_title, relationship_type), SUM occurrence, OR
/// target_in_scope; sort by summed occurrence DESC; take `limit`. The shared
/// `neighbors` / `neighbors_for_axis` fold (the SQLite's GROUP BY + SUM + ORDER).
fn fold_by_target_rel(rows: Vec<EdgeLite>, limit: usize) -> Vec<Neighbor> {
    let mut by: HashMap<(String, String), (i64, bool)> = HashMap::new();
    for e in rows {
        let v = by
            .entry((e.target_title, e.relationship_type))
            .or_insert((0, false));
        v.0 += e.occurrence_count;
        v.1 |= e.target_in_scope;
    }
    let mut out: Vec<Neighbor> = by
        .into_iter()
        .map(|((title, rel), (occ, in_scope))| Neighbor {
            title,
            relationship_type: rel,
            occurrence_count: occ,
            in_scope,
        })
        .collect();
    out.sort_by(|x, y| {
        y.occurrence_count
            .cmp(&x.occurrence_count)
            .then_with(|| x.title.cmp(&y.title))
    });
    out.truncate(limit);
    out
}

/// Open the wiki link graph for `corpus_id` — the v2 columnar store
/// (`atlas/articles.lance` + `atlas/edges.lance`). The per-corpus gate the
/// runtime loaders (chat / server / desktop) share.
///
/// `None` means this corpus has no link graph, which is the ordinary answer for
/// every non-wiki corpus. Since W4 there is no second backend to fall through
/// to, so a store that is PRESENT but fails to open returns `None` with a
/// `warn` naming the error — an absence reported, never a silent substitution
/// (ARCH §18.3).
/// Whether `corpus_id` has a wiki link graph on disk — both tables of the v2
/// columnar store present.
///
/// ONE decider for the question (ARCH §10.6). It was asked in three places by
/// two different means: `open_wikipedia_graph` inlined the two `exists()`
/// checks, while `meta-atlas align` and `atlas migrate-all` each stat'd the
/// SQLite `wikipedia_graph.db` — a path that no longer exists, so those two
/// would have silently answered "no link graph" for every corpus forever.
pub fn wikipedia_graph_present(indexes_dir: &Path, corpus_id: &str) -> bool {
    let atlas_dir = indexes_dir
        .join(corpus_id)
        .join(crate::enrichment::atlas::ATLAS_DIRNAME);
    atlas_dir
        .join(crate::enrichment::atlas::wiki_store::ARTICLES_LANCE_DIRNAME)
        .exists()
        && atlas_dir
            .join(crate::enrichment::atlas::wiki_store::EDGES_LANCE_DIRNAME)
            .exists()
}

pub async fn open_wikipedia_graph(
    indexes_dir: &Path,
    corpus_id: &str,
) -> Option<Arc<dyn WikipediaGraphApi>> {
    let atlas_dir = indexes_dir
        .join(corpus_id)
        .join(crate::enrichment::atlas::ATLAS_DIRNAME);
    if wikipedia_graph_present(indexes_dir, corpus_id) {
        match ColumnarWikipediaGraph::open(&atlas_dir).await {
            Ok(g) => {
                tracing::info!(corpus = %corpus_id, backend = "columnar", "wikipedia graph loaded (v2)");
                return Some(Arc::new(g) as Arc<dyn WikipediaGraphApi>);
            }
            Err(e) => {
                tracing::warn!(corpus = %corpus_id, error = %e, "columnar wiki graph present but failed to open; this corpus has no link graph this boot");
            }
        }
    }
    None
}

// ─── The grounding walk's face on the same two tables ────────────────────────

/// [`AtlasProvider`] over the wiki columnar store — the second implementor of
/// the walk's trait, beside `AtlasGraph`'s `atoms.lance` + `edges.csr`.
///
/// **Why a second backend rather than a migration.** Wikipedia's link graph is
/// predicate-shaped: `neighbors_for_axis` filters on the per-edge strings
/// `link_text` and `source_section_path`, which have nowhere to live in the
/// 10-byte CSR record (`store::LocalEdge`). Folding wikipedia into the atom
/// store would delete that capability to gain a layout. The walk does not need
/// the layout; it needs answers to eight questions. So both faces read ONE
/// store: this type answers the walk, [`ColumnarWikipediaGraph`] answers the
/// neighbor API, and neither has to become the other.
///
/// **Resident, because the trait borrows.** `AtomView<'_>` is a view over a
/// resident `AtomRecord`, so a provider cannot serve it from an async Lance
/// query. The articles are projected into records at open and the link graph
/// into a sorted adjacency; both are built once, at lifecycle time, never on
/// the query path. See `WIKIPEDIA_ATLAS_V2.md` "the reader" for why resident is
/// affordable here and was not for the atom model: there is no payload. The
/// `payload` blob is left EMPTY, so `AtomView::atom_envelope` returns `None` —
/// a wiki atom has no deep fields to re-parse.
pub struct WikiAtlasProvider {
    atlas_corpus_id: String,
    site: EvidenceSite,
    atoms: Vec<AtomRecord>,
    by_id: HashMap<String, usize>,
    /// Out-adjacency and in-adjacency as `(from, to)` index pairs, each sorted
    /// by its key so a lookup is a binary search over a contiguous range. Edges
    /// are deduplicated to one per (source, target): the per-section
    /// multiplicity that `occurrence_count` carries is prominence, and lives on
    /// the neighbor face.
    out: Vec<(u32, u32)>,
    inn: Vec<(u32, u32)>,
    ann: Option<Arc<AnnSeedTable>>,
    ontology: Option<OntologyPolicies>,
}

impl WikiAtlasProvider {
    /// Open the wiki columnar store as a walk provider.
    ///
    /// REFUSES a v1 store by name. Without `atom_id` and `chunk_id` every atom
    /// would have no identity and no evidence, and the walk would report 1.6M
    /// atoms that can never become a citation — a defect wearing the shape of a
    /// result (ARCH §18.3). Rebuild with `svrn atlas wikipedia build-graph`.
    pub async fn open(atlas_dir: &Path, atlas_corpus_id: &str) -> Result<Self, String> {
        let graph = ColumnarWikipediaGraph::open(atlas_dir).await?;
        if !graph.has_v2_columns() {
            return Err(format!(
                "wiki store at {} is format v1 (no atom_id/chunk_id): it can serve the neighbor \
                 API but not the grounding walk — rebuild with `svrn atlas wikipedia build-graph`",
                atlas_dir.display()
            ));
        }

        // 1. Articles → resident atom records, in title order so the local ids
        //    are a function of the store's content and not of scan order.
        let mut rows = graph.article_rows().await?;
        rows.sort_by(|a, b| a.title.cmp(&b.title));
        let mut atoms = Vec::with_capacity(rows.len());
        let mut by_id = HashMap::with_capacity(rows.len());
        let mut by_title: HashMap<String, u32> = HashMap::with_capacity(rows.len());
        for (i, r) in rows.iter().enumerate() {
            by_title.insert(r.title.clone(), i as u32);
            by_id.insert(r.atom_id.clone(), i);
            atoms.push(AtomRecord {
                id: r.atom_id.clone(),
                kind: AtomType::Entity,
                name: r.title.clone(),
                label: String::new(),
                content: String::new(),
                subtype: "article".to_string(),
                // Wikipedia's structural atoms have no description — 214 of 221
                // sampled were already empty (bench/wikipedia/seed_migration),
                // and the build has no source for one. Empty is the truth here,
                // not a gap.
                description: String::new(),
                excerpt: String::new(),
                confidence: 0.0,
                // Flat, as `atoms.json` already carries it. A structural
                // prominence score (degree, POV totals) would be a new scorer
                // with no measurement behind it (§18.6); the signals stay in
                // the store's columns until something asks for them.
                salience: 0.5,
                aliases: Vec::new(),
                participants: Vec::new(),
                evidence: vec![ChunkRef {
                    chunk_id: r.chunk_id.clone(),
                    passage_preview: None,
                    source_doc_id: None,
                }],
                payload: Vec::new(),
            });
        }

        // 2. Edges → a sorted index-pair adjacency. Only edges whose BOTH
        //    endpoints are in-scope articles survive, because an `EdgeView`
        //    names its endpoints by atom id and a dangling target has none.
        //    The same rule `write_edges_csr` applies, for the same reason. The
        //    dropped edges are not lost: `ColumnarWikipediaGraph::neighbors`
        //    still serves them with `in_scope: false`.
        let mut out: Vec<(u32, u32)> = Vec::new();
        let mut dangling = 0usize;
        let scanned = graph
            .for_each_edge_pair(|src, tgt| match (by_title.get(src), by_title.get(tgt)) {
                (Some(&s), Some(&t)) => out.push((s, t)),
                _ => dangling += 1,
            })
            .await?;
        out.sort_unstable();
        out.dedup();
        let mut inn: Vec<(u32, u32)> = out.iter().map(|&(s, t)| (t, s)).collect();
        inn.sort_unstable();

        let ontology =
            crate::enrichment::atlas::writer::read_atlas_ontology(atlas_dir).map(|f| f.policies);
        tracing::info!(
            corpus = atlas_corpus_id,
            atoms = atoms.len(),
            edge_rows_scanned = scanned,
            edges = out.len(),
            dangling_edges_dropped = dangling,
            declared_ontology = ontology.is_some(),
            "wiki atlas provider: resident store built"
        );

        Ok(Self {
            atlas_corpus_id: atlas_corpus_id.to_string(),
            site: EvidenceSite::derive(atlas_corpus_id),
            atoms,
            by_id,
            out,
            inn,
            ann: None,
            ontology,
        })
    }

    /// [`Self::open`] from sync code — the daemon's `AtlasContextManager`
    /// resolves a provider on a `Provider` trait method, which is sync.
    ///
    /// Bridges through `store::run_blocking`, the atlas module's ONE
    /// async-from-sync bridge (ARCH §10.6), for the same reason
    /// `LancePreload::open_blocking` does: Lance needs a tokio reactor, and a
    /// fresh dedicated-thread runtime avoids both the nested-runtime panic and
    /// `block_in_place`'s flavour constraints. Lifecycle-time only — corpus
    /// load, never the hot query path.
    pub fn open_blocking(atlas_dir: &Path, atlas_corpus_id: &str) -> Result<Self, String> {
        crate::enrichment::atlas::store::run_blocking(Self::open(atlas_dir, atlas_corpus_id))
    }

    /// Attach the migrated seed table. Separate from `open` because the table
    /// is built by its own lifecycle step and a provider without one is a
    /// legitimate state the walk names (`has_ann_seed_table`).
    pub fn with_ann_seed_table(mut self, ann: Arc<AnnSeedTable>) -> Self {
        self.ann = Some(ann);
        self
    }

    pub fn atom_count(&self) -> usize {
        self.atoms.len()
    }

    pub fn edge_count(&self) -> usize {
        self.out.len()
    }

    /// The contiguous run of `adj` keyed by `k`.
    fn range(adj: &[(u32, u32)], k: u32) -> &[(u32, u32)] {
        let lo = adj.partition_point(|&(a, _)| a < k);
        let hi = adj.partition_point(|&(a, _)| a <= k);
        &adj[lo..hi]
    }

    fn views<'a>(&'a self, adj: &'a [(u32, u32)], k: u32, forward: bool) -> Vec<EdgeView<'a>> {
        Self::range(adj, k)
            .iter()
            .map(|&(a, b)| {
                let (s, t) = if forward { (a, b) } else { (b, a) };
                EdgeView {
                    source: self.atoms[s as usize].id.as_str(),
                    target: self.atoms[t as usize].id.as_str(),
                    // A wikilink is an `Involves` and nothing else. The six
                    // relationship labels (topical / causal / contested /
                    // defines / action / see-also) are OPEN text and ride as
                    // data on `edges.lance`, read by the neighbor face; they
                    // are not arms on the closed `EdgeType` (ARCH principle 9,
                    // EPISTEMIC_INDEX spec §3 "no private kinds").
                    edge_type: EdgeType::Involves,
                    // A wikilink carries no confidence — it either exists or it
                    // does not. `occurrence_count` is PROMINENCE, not
                    // confidence, and putting it in this slot would label it
                    // something it is not; it stays on the neighbor face.
                    confidence: 1.0,
                    provenance: EdgeProvenance::WikilinkStructural,
                }
            })
            .collect()
    }
}

impl AtlasProvider for WikiAtlasProvider {
    fn provider_class(&self) -> &'static str {
        "wiki-class"
    }

    fn atlas_corpus_id(&self) -> &str {
        &self.atlas_corpus_id
    }

    fn site(&self) -> &EvidenceSite {
        &self.site
    }

    fn atom(&self, atom_id: &str) -> Option<AtomView<'_>> {
        self.by_id
            .get(atom_id)
            .map(|&i| AtomView::new(&self.atoms[i]))
    }

    fn atom_evidence(&self, atom_id: &str) -> Vec<EvidenceRef<'_>> {
        match self.by_id.get(atom_id) {
            Some(&i) => self.atoms[i]
                .evidence
                .iter()
                .map(EvidenceRef::new)
                .collect(),
            None => Vec::new(),
        }
    }

    fn edges_from(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        match self.by_id.get(atom_id) {
            Some(&i) => self.views(&self.out, i as u32, true),
            None => Vec::new(),
        }
    }

    fn edges_to(&self, atom_id: &str) -> Vec<EdgeView<'_>> {
        match self.by_id.get(atom_id) {
            Some(&i) => self.views(&self.inn, i as u32, false),
            None => Vec::new(),
        }
    }

    fn ann_seed_table(&self) -> Option<&Arc<AnnSeedTable>> {
        self.ann.as_ref()
    }

    fn ontology(&self) -> Option<&OntologyPolicies> {
        self.ontology.as_ref()
    }
}

#[cfg(test)]
mod tests;
