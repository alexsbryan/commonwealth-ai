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

use crate::enrichment::atlas::wiki_store::{ARTICLES_TABLE, EDGES_TABLE};

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
        Ok(Self { articles, edges })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::wiki_store::{
        write_wikipedia_columnar_store, WikiArticleRow, WikiEdgeRow,
    };

    fn art(title: &str, contested: bool) -> WikiArticleRow {
        WikiArticleRow {
            title: title.into(),
            wikidata_qid: format!("Q-{title}"),
            revision_id: 10,
            in_scope: true,
            pov_total: if contested { 2 } else { 0 },
            citation_total: 3,
            is_contested: contested,
        }
    }

    fn edge(src: &str, tgt: &str, rel: &str, sect: &str, occ: i64) -> WikiEdgeRow {
        WikiEdgeRow {
            source_title: src.into(),
            target_title: tgt.into(),
            relationship_type: rel.into(),
            link_text: tgt.to_lowercase(),
            occurrence_count: occ,
            source_section_path: sect.into(),
            target_in_scope: true,
        }
    }

    #[tokio::test]
    async fn columnar_graph_serves_the_neighbor_api() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // A,B,D in scope; C contested + in scope. Links:
        //   A→B (topical/Intro), A→C (contested/Criticism), A→D (topical/See also)
        //   B→C (topical/Body)  — so C is co-cited by A and B.
        let articles = vec![
            art("A", false),
            art("B", false),
            art("C", true),
            art("D", false),
        ];
        let edges = vec![
            edge("A", "B", "topical", "Intro", 2),
            edge("A", "C", "contested", "Criticism", 1),
            edge("A", "D", "topical", "See also", 1),
            edge("B", "C", "topical", "Body", 3),
        ];
        write_wikipedia_columnar_store(dir, &articles, &edges)
            .await
            .unwrap();
        let g = ColumnarWikipediaGraph::open(dir).await.unwrap();

        // neighbors(A): B, C, D — ranked by occurrence (B=2 first).
        let n = g.neighbors("A", 10).await;
        assert_eq!(n.len(), 3);
        assert_eq!(n[0].title, "B");
        assert_eq!(n[0].occurrence_count, 2);
        let titles: HashSet<&str> = n.iter().map(|x| x.title.as_str()).collect();
        assert!(titles.contains("C") && titles.contains("D"));

        // neighbors_for_axis(A, ["criticism"]): only C (Criticism section).
        let ax = g.neighbors_for_axis("A", &["criticism".into()], 10).await;
        assert_eq!(ax.len(), 1);
        assert_eq!(ax[0].title, "C");
        assert_eq!(ax[0].relationship_type, "contested");

        // co_neighbors(A, B): C, linked from both.
        let co = g.co_neighbors(&["A".into(), "B".into()], &[], 10).await;
        assert_eq!(co.len(), 1);
        assert_eq!(co[0].title, "C");
        assert_eq!(co[0].occurrence_count, 4); // 1 (A→C) + 3 (B→C)

        // reverse_neighbors(C): A and B link to it.
        let rev = g.reverse_neighbors("C", 10).await;
        let rtitles: HashSet<&str> = rev.iter().map(|x| x.title.as_str()).collect();
        assert!(rtitles.contains("A") && rtitles.contains("B"));

        // has_contested_section + record.
        assert!(g.has_contested_section("C").await);
        assert!(!g.has_contested_section("A").await);
        let rec = g.record("A").await.expect("record A");
        assert_eq!(rec.title, "A");
        assert_eq!(rec.wikidata_qid.as_deref(), Some("Q-A"));
        assert!(rec.in_scope);
        assert_eq!(rec.pov_total, 0);
        assert!(g.record("nope").await.is_none());
    }

    /// The gate: `open_wikipedia_graph` returns `None` when no store is present,
    /// and selects the columnar backend once `atlas/articles.lance` +
    /// `atlas/edges.lance` exist for the corpus.
    #[tokio::test]
    async fn open_wikipedia_graph_serves_the_columnar_store_and_nothing_else() {
        use crate::enrichment::atlas::wiki_store::{
            write_wikipedia_columnar_store, WikiArticleRow, WikiEdgeRow,
        };
        let tmp = tempfile::tempdir().unwrap();
        let indexes_dir = tmp.path();
        let corpus_id = "wiki-test";

        // Nothing present yet → None. Since W4 there is no SQLite to fall to,
        // so this is the whole of "no link graph for this corpus".
        assert!(open_wikipedia_graph(indexes_dir, corpus_id).await.is_none());

        // Write a columnar store at <indexes>/<corpus>/atlas/.
        let atlas_dir = indexes_dir
            .join(corpus_id)
            .join(crate::enrichment::atlas::ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas_dir).unwrap();
        let articles = vec![
            WikiArticleRow {
                title: "A".into(),
                wikidata_qid: String::new(),
                revision_id: -1,
                in_scope: true,
                pov_total: 0,
                citation_total: 0,
                is_contested: false,
            },
            WikiArticleRow {
                title: "B".into(),
                wikidata_qid: String::new(),
                revision_id: -1,
                in_scope: true,
                pov_total: 0,
                citation_total: 0,
                is_contested: false,
            },
        ];
        let edges = vec![WikiEdgeRow {
            source_title: "A".into(),
            target_title: "B".into(),
            relationship_type: "topical".into(),
            link_text: "b".into(),
            occurrence_count: 1,
            source_section_path: "Lead".into(),
            target_in_scope: true,
        }];
        write_wikipedia_columnar_store(&atlas_dir, &articles, &edges)
            .await
            .unwrap();

        // With the store present the gate serves neighbors from it.
        let g = open_wikipedia_graph(indexes_dir, corpus_id)
            .await
            .expect("columnar graph selected");
        let n = g.neighbors("A", 10).await;
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "B");
    }
}
