// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wikipedia link graph — a SQLite-backed adjacency store built from
//! the structural metadata that Wikipedia extractors already populate
//! (`outgoing_links`, `section_path`, `pov_count`, …) but that the
//! retrieval path currently throws away.
//!
//! Layer 0 of the Atlas-style enrichment plan: zero-LLM, structural
//! signals only. Layer 1 fills in `articles.cluster_id` and
//! `articles.bridge_score` (HDBSCAN over the in-scope subgraph); Layer
//! 2 targets LLM enrichment at high-signal articles. Both layers are
//! schema-compatible with this file as written — no migration required
//! to bolt them on later.
//!
//! ## Storage
//!
//! Three tables, scoped by `corpus_id` so multiple Wikipedia-class
//! corpora can share one DB if a future build merges them:
//!
//! - `articles` — one row per distinct title encountered (in-scope
//!   sources, plus dangling targets with `in_scope = 0`).
//! - `edges` — one row per `(source_article, source_section, target_title)`.
//!   `occurrence_count` collapses duplicates introduced by the
//!   chunker emitting N chunks per section.
//! - `section_signals` — per-section pov / citation / link counts,
//!   plus a derived `is_contested` flag.
//!
//! ## Threading
//!
//! Mirrors `ScipGraph`: synchronous `rusqlite::Connection` wrapped in
//! a `tokio::sync::Mutex`. All hot-path queries are index-covered
//! and complete in microseconds; the async mutex is negligible
//! overhead. Bulk insert uses `spawn_blocking`-friendly synchronous
//! transactions.
//!
//! ## Section-path delimiter
//!
//! `section_path` arrays are joined with U+203A (›). Wikipedia titles
//! never contain this character, so the delimiter is unambiguous on
//! split. Do not change without a schema migration — existing rows
//! depend on this exact byte sequence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::extractors::wikipedia_types::{wiki_title_from_url, WikipediaChunkMetadata};
use crate::index::StoredChunkWithMetadata;

/// Section-path delimiter. U+203A (›) — never appears in Wikipedia
/// titles, so split/join round-trips cleanly.
pub const SECTION_PATH_DELIMITER: char = '\u{203a}';

/// On-disk schema version. Bumped when the table layout changes in
/// a way that prior data can no longer be read correctly. Cf.
/// `ScipGraph::SCHEMA_VERSION`.
pub const SCHEMA_VERSION: u32 = 1;

// ─── Public types ─────────────────────────────────────────────

/// A one-hop neighbor in the link graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Neighbor {
    /// Target article title, canonical form (spaces, not underscores).
    pub title: String,
    /// Coarse relationship class derived from section + link text.
    /// One of `topical | causal | contested | defines | action | see-also`.
    pub relationship_type: String,
    /// How many sections of the source article link here. Higher =
    /// stronger structural signal.
    pub occurrence_count: i64,
    /// True iff a row exists in `articles` for this target — i.e.
    /// the target is also in the indexed scope (Vital L5, etc.).
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
    pub cluster_id: Option<i64>,
    pub bridge_score: Option<f64>,
    pub pov_total: i64,
    pub citation_total: i64,
}

/// Aggregated counts returned by `ingest_from_lance` — useful for
/// the CLI to log a sanity-check summary after a build.
#[derive(Debug, Clone, Copy, Default)]
pub struct IngestSummary {
    pub articles_inserted: usize,
    pub edges_inserted: usize,
    pub sections_inserted: usize,
    pub dangling_targets: usize,
    pub chunks_with_metadata: usize,
    pub chunks_without_metadata: usize,
}

/// Staleness signal for a Wikipedia graph relative to the underlying
/// LanceDB index. Mirrors `ScipGraph::StalenessCaution` semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StalenessCaution {
    /// Graph fresh — every chunk's revision_id was seen at last build,
    /// or `last_built_at` is recent enough to be irrelevant.
    None,
    /// Some indexed chunks have a higher revision_id than the graph
    /// recorded — Wikipedia editors have updated the corpus since
    /// the last build. Surface to callers; do not penalize scores.
    GraphIsAging { age_hours: u64 },
    /// Graph is older than the staleness threshold AND chunk revisions
    /// have moved past it. Caller should rebuild the graph.
    GraphIsStale { age_hours: u64, corpus_id: String },
}

// ─── WikipediaGraph ──────────────────────────────────────────

pub struct WikipediaGraph {
    conn: Arc<Mutex<Connection>>,
    corpus_id: String,
}

impl WikipediaGraph {
    /// Open or create the SQLite database at the given path.
    /// Creates parent directories if needed; runs `init_schema`.
    pub fn open(db_path: &Path, corpus_id: &str) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("WikipediaGraph open: {e}")))?;
        Self::apply_pragmas(&conn);
        Self::init_schema(&conn)?;
        Self::stamp_schema_version(&conn);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            corpus_id: corpus_id.to_string(),
        })
    }

    /// In-memory DB for tests.
    pub fn open_in_memory(corpus_id: &str) -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("WikipediaGraph in-memory: {e}")))?;
        Self::apply_pragmas(&conn);
        Self::init_schema(&conn)?;
        Self::stamp_schema_version(&conn);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            corpus_id: corpus_id.to_string(),
        })
    }

    /// Default on-disk path for a corpus's wikipedia graph DB. The
    /// CLI uses this when `--db-path` isn't supplied.
    pub fn default_db_path(indexes_dir: &Path, corpus_id: &str) -> PathBuf {
        indexes_dir.join(corpus_id).join("wikipedia_graph.db")
    }

    fn apply_pragmas(conn: &Connection) {
        // WAL + NORMAL: same trade-off ScipGraph uses. Reads never
        // block writes, durability is fsync-on-checkpoint.
        let _ = conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;",
        );
        // mmap window + page cache sized for the file shape we
        // actually see in production. The previous 8 GiB mmap +
        // 2 GiB cache reservation (rationale: "harmless on smaller
        // hosts — mmap is lazy") was wrong on hosts where the
        // daemon and a CLI process both open the same DB: each
        // process reserves its own address space and the page-
        // cache pressure compounds. On Strix Halo (125 GiB unified
        // memory shared with GPU), two opens of the wiki graph
        // were claiming 20 GiB of usage between them — enough to
        // OOM-kill the bench process during bootstrap when the
        // GPU pinned memory + daemon model weights were already in
        // residence.
        //
        // Empirical file size: ~2.3 GiB for the full Wikipedia
        // graph (50k articles, 7M edges). 1.5 GiB mmap is generous
        // headroom for current shape + growth and still slack
        // against the file. 64 MiB page cache is a standard sane
        // default — graph queries are small-result reads, not
        // table scans; cranking the cache to 2 GiB never helped a
        // measurable workload.
        let _ = conn.execute_batch("PRAGMA mmap_size = 1610612736;");
        let _ = conn.execute_batch("PRAGMA cache_size = -65536;");
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS articles (
                id INTEGER PRIMARY KEY,
                corpus_id      TEXT NOT NULL,
                title          TEXT NOT NULL,
                page_id        INTEGER,
                wikidata_qid   TEXT,
                revision_id    INTEGER,
                in_scope       INTEGER NOT NULL DEFAULT 1,
                cluster_id     INTEGER,
                bridge_score   REAL,
                pov_total      INTEGER NOT NULL DEFAULT 0,
                citation_total INTEGER NOT NULL DEFAULT 0,
                UNIQUE(corpus_id, title)
            );
            CREATE INDEX IF NOT EXISTS idx_articles_corpus
                ON articles(corpus_id);
            CREATE INDEX IF NOT EXISTS idx_articles_qid
                ON articles(wikidata_qid) WHERE wikidata_qid IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_articles_cluster
                ON articles(corpus_id, cluster_id);

            CREATE TABLE IF NOT EXISTS edges (
                id INTEGER PRIMARY KEY,
                corpus_id           TEXT NOT NULL,
                source_article_id   INTEGER NOT NULL,
                source_section_path TEXT NOT NULL,
                section_type        TEXT NOT NULL,
                target_title        TEXT NOT NULL,
                target_article_id   INTEGER,
                link_text           TEXT NOT NULL,
                relationship_type   TEXT NOT NULL DEFAULT 'topical',
                occurrence_count    INTEGER NOT NULL DEFAULT 1
            );
            CREATE UNIQUE INDEX IF NOT EXISTS uq_edges_triple
                ON edges(corpus_id, source_article_id, source_section_path, target_title);
            CREATE INDEX IF NOT EXISTS idx_edges_source
                ON edges(corpus_id, source_article_id);
            CREATE INDEX IF NOT EXISTS idx_edges_target
                ON edges(corpus_id, target_title);
            CREATE INDEX IF NOT EXISTS idx_edges_target_id
                ON edges(corpus_id, target_article_id) WHERE target_article_id IS NOT NULL;

            CREATE TABLE IF NOT EXISTS section_signals (
                id INTEGER PRIMARY KEY,
                corpus_id                  TEXT NOT NULL,
                article_id                 INTEGER NOT NULL,
                section_path               TEXT NOT NULL,
                section_type               TEXT NOT NULL,
                pov_count                  INTEGER NOT NULL DEFAULT 0,
                citation_needed_count      INTEGER NOT NULL DEFAULT 0,
                clarification_needed_count INTEGER NOT NULL DEFAULT 0,
                update_count               INTEGER NOT NULL DEFAULT 0,
                outgoing_link_count        INTEGER NOT NULL DEFAULT 0,
                is_contested               INTEGER NOT NULL DEFAULT 0,
                UNIQUE(corpus_id, article_id, section_path)
            );
            CREATE INDEX IF NOT EXISTS idx_sigs_contested
                ON section_signals(corpus_id, is_contested) WHERE is_contested = 1;
            CREATE INDEX IF NOT EXISTS idx_sigs_link_count
                ON section_signals(corpus_id, outgoing_link_count);

            CREATE TABLE IF NOT EXISTS wiki_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )
        .map_err(|e| Error::Database(format!("WikipediaGraph schema: {e}")))?;
        Ok(())
    }

    fn stamp_schema_version(conn: &Connection) {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO wiki_meta (key, value) VALUES ('schema_version', ?)",
            params![SCHEMA_VERSION.to_string()],
        );
    }

    /// Wipe all rows for this corpus. Used by `--rebuild`. Schema is
    /// preserved.
    pub async fn clear_corpus(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        let corpus = self.corpus_id.clone();
        conn.execute(
            "DELETE FROM section_signals WHERE corpus_id = ?",
            params![corpus],
        )
        .map_err(|e| Error::Database(format!("clear sigs: {e}")))?;
        conn.execute("DELETE FROM edges WHERE corpus_id = ?", params![corpus])
            .map_err(|e| Error::Database(format!("clear edges: {e}")))?;
        conn.execute("DELETE FROM articles WHERE corpus_id = ?", params![corpus])
            .map_err(|e| Error::Database(format!("clear articles: {e}")))?;
        Ok(())
    }

    // ─── Hot-path queries ────────────────────────────────────

    /// Resolve a title to its article_id within this graph's corpus.
    /// `None` if the title isn't in scope (or doesn't appear at all).
    /// Internal helper — public callers use `neighbors` / `record`.
    async fn article_id_for(&self, title: &str) -> Option<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id FROM articles WHERE corpus_id = ? AND title = ? AND in_scope = 1",
            params![self.corpus_id, title],
            |r| r.get::<_, i64>(0),
        )
        .ok()
    }

    /// One-hop forward neighbors of `title`, ordered by
    /// `occurrence_count DESC` (strongest structural signal first).
    /// Capped by `limit`. Returns an empty Vec when the title isn't
    /// in scope — callers should treat that the same as "no graph
    /// info" and fall back to pure hybrid search.
    pub async fn neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        let Some(article_id) = self.article_id_for(title).await else {
            return Vec::new();
        };
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT e.target_title,
                    e.relationship_type,
                    SUM(e.occurrence_count) AS total_occ,
                    COALESCE(MAX(a.in_scope), 0) AS target_in_scope
             FROM edges e
             LEFT JOIN articles a
               ON a.corpus_id = e.corpus_id AND a.id = e.target_article_id
             WHERE e.corpus_id = ? AND e.source_article_id = ?
             GROUP BY e.target_title, e.relationship_type
             ORDER BY total_occ DESC
             LIMIT ?",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![self.corpus_id, article_id, limit as i64], |row| {
            Ok(Neighbor {
                title: row.get(0)?,
                relationship_type: row.get(1)?,
                occurrence_count: row.get(2)?,
                in_scope: row.get::<_, i64>(3)? == 1,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Outbound neighbors whose `target_title`, `link_text`, or
    /// `source_section_path` lexically contains at least one of
    /// `axis_terms` (case-insensitive substring). Each axis term
    /// short-circuits OR, so the union of matches is returned.
    /// Ranked by `SUM(occurrence_count) DESC` among matched edges,
    /// not by raw graph occurrence — useful when callers want
    /// query-aligned neighbors instead of "this article's most-
    /// cited internal vocabulary".
    pub async fn neighbors_for_axis(
        &self,
        title: &str,
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        let Some(article_id) = self.article_id_for(title).await else {
            return Vec::new();
        };
        let terms: Vec<String> = axis_terms
            .iter()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let axis_clause = std::iter::repeat_n(
            "(LOWER(e.target_title) LIKE ?1 \
              OR LOWER(e.link_text) LIKE ?1 \
              OR LOWER(e.source_section_path) LIKE ?1)",
            terms.len(),
        )
        .enumerate()
        .map(|(i, _)| {
            format!(
                "(LOWER(e.target_title) LIKE ?{n} \
                  OR LOWER(e.link_text) LIKE ?{n} \
                  OR LOWER(e.source_section_path) LIKE ?{n})",
                n = i + 3
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ");

        let sql = format!(
            "SELECT e.target_title,
                    e.relationship_type,
                    SUM(e.occurrence_count) AS total_occ,
                    COALESCE(MAX(a.in_scope), 0) AS target_in_scope
             FROM edges e
             LEFT JOIN articles a
               ON a.corpus_id = e.corpus_id AND a.id = e.target_article_id
             WHERE e.corpus_id = ?1 AND e.source_article_id = ?2
               AND ({axis_clause})
             GROUP BY e.target_title, e.relationship_type
             ORDER BY total_occ DESC
             LIMIT {}",
            limit as i64
        );
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        params.push(Box::new(self.corpus_id.clone()));
        params.push(Box::new(article_id));
        for t in &terms {
            params.push(Box::new(format!("%{t}%")));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(Neighbor {
                title: row.get(0)?,
                relationship_type: row.get(1)?,
                occurrence_count: row.get(2)?,
                in_scope: row.get::<_, i64>(3)? == 1,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Articles linked-to by every input title (intersection of
    /// outbound edge sets). Optionally filtered by axis-term match
    /// against `target_title` / `link_text` / `source_section_path`.
    /// Ranked by total occurrence_count summed across the input
    /// titles. Empty `titles` → empty result.
    ///
    /// This is the comparison-friendly primitive: shared concepts
    /// that both A and B reference are exactly the bridge articles
    /// a comparative answer would draw on.
    pub async fn co_neighbors(
        &self,
        titles: &[String],
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        if titles.len() < 2 {
            return Vec::new();
        }
        // Resolve all input titles to ids; drop any that aren't in
        // scope (no edges to intersect).
        let mut ids: Vec<i64> = Vec::new();
        for t in titles {
            if let Some(id) = self.article_id_for(t).await {
                ids.push(id);
            }
        }
        if ids.len() < 2 {
            return Vec::new();
        }

        let in_clause = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");

        let axis_filter = if axis_terms.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = (0..axis_terms.len())
                .map(|i| {
                    format!(
                        "(LOWER(e.target_title) LIKE ?{n} \
                         OR LOWER(e.link_text) LIKE ?{n} \
                         OR LOWER(e.source_section_path) LIKE ?{n})",
                        n = i + 2
                    )
                })
                .collect();
            format!(" AND ({})", parts.join(" OR "))
        };

        let n_required = ids.len() as i64;
        let sql = format!(
            "SELECT e.target_title,
                    MIN(e.relationship_type) AS rel,
                    SUM(e.occurrence_count) AS total_occ,
                    COALESCE(MAX(a.in_scope), 0) AS target_in_scope,
                    COUNT(DISTINCT e.source_article_id) AS distinct_sources
             FROM edges e
             LEFT JOIN articles a
               ON a.corpus_id = e.corpus_id AND a.id = e.target_article_id
             WHERE e.corpus_id = ?1
               AND e.source_article_id IN ({in_clause})
               {axis_filter}
             GROUP BY e.target_title
             HAVING distinct_sources = {n_required}
             ORDER BY total_occ DESC
             LIMIT {}",
            limit as i64
        );
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let terms: Vec<String> = axis_terms
            .iter()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        params.push(Box::new(self.corpus_id.clone()));
        for t in &terms {
            params.push(Box::new(format!("%{t}%")));
        }
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(Neighbor {
                title: row.get(0)?,
                relationship_type: row.get(1)?,
                occurrence_count: row.get(2)?,
                in_scope: row.get::<_, i64>(3)? == 1,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Reverse neighbors (articles that link TO `title`). Capped at
    /// `limit` and ordered by `occurrence_count DESC` so the densest
    /// structural in-edges win — never `SELECT *` here, popular
    /// hubs ("United States") have hundreds of thousands of incoming
    /// edges at full Wikipedia scale.
    pub async fn reverse_neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT a.title,
                    e.relationship_type,
                    SUM(e.occurrence_count) AS total_occ
             FROM edges e
             JOIN articles a
               ON a.id = e.source_article_id AND a.corpus_id = e.corpus_id
             WHERE e.corpus_id = ? AND e.target_title = ?
             GROUP BY a.title, e.relationship_type
             ORDER BY total_occ DESC
             LIMIT ?",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![self.corpus_id, title, limit as i64], |row| {
            Ok(Neighbor {
                title: row.get(0)?,
                relationship_type: row.get(1)?,
                occurrence_count: row.get(2)?,
                in_scope: true,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Whether any section of this article was flagged contested
    /// (Wikipedia editors set pov_count > 0 OR section_type =
    /// "controversy"). Used by the prompt assembler to surface the
    /// `(contested)` source marker; cheap because the index covers
    /// `is_contested = 1`.
    pub async fn has_contested_section(&self, title: &str) -> bool {
        let Some(article_id) = self.article_id_for(title).await else {
            return false;
        };
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT 1 FROM section_signals
             WHERE corpus_id = ? AND article_id = ? AND is_contested = 1
             LIMIT 1",
            params![self.corpus_id, article_id],
            |_r| Ok(1_i64),
        )
        .map(|_| true)
        .unwrap_or(false)
    }

    /// Full article record — null fields mean Layer 1 hasn't filled
    /// them in yet. Useful for diagnostics; the hot path uses
    /// `neighbors` / `has_contested_section` directly.
    pub async fn record(&self, title: &str) -> Option<ArticleRecord> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT title, wikidata_qid, revision_id, in_scope,
                    cluster_id, bridge_score, pov_total, citation_total
             FROM articles
             WHERE corpus_id = ? AND title = ?",
            params![self.corpus_id, title],
            |row| {
                Ok(ArticleRecord {
                    title: row.get(0)?,
                    wikidata_qid: row.get(1)?,
                    revision_id: row.get(2)?,
                    in_scope: row.get::<_, i64>(3)? == 1,
                    cluster_id: row.get(4)?,
                    bridge_score: row.get(5)?,
                    pov_total: row.get(6)?,
                    citation_total: row.get(7)?,
                })
            },
        )
        .ok()
    }

    // ─── Columnar export (WIKIPEDIA_ATLAS_V2 W1b) ────────────

    /// Export this SQLite graph to the columnar v2 store (`articles.lance` +
    /// `edges.lance`) under `atlas_dir` — the format the
    /// [`crate::wikipedia_columnar::ColumnarWikipediaGraph`] reader serves. A
    /// faithful 1:1 dump: articles + the per-article `is_contested` flag
    /// (`EXISTS` over `section_signals`), and edges joined to `articles` for the
    /// `source_title` + the denormalised `target_in_scope` the columnar neighbor
    /// query reads without a join. The SQLite stays the build aggregator here;
    /// W4 makes the columnar store the build output directly + retires this DB.
    ///
    /// The SQLite read is scoped to drop the connection guard before the async
    /// Lance write, so no `!Send` rusqlite handle crosses the `.await`.
    pub async fn export_columnar(&self, atlas_dir: &Path) -> Result<()> {
        use crate::enrichment::atlas::wiki_store::{WikiArticleRow, WikiEdgeRow};
        let (articles, edges): (Vec<WikiArticleRow>, Vec<WikiEdgeRow>) = {
            let conn = self.conn.lock().await;
            // Flush the WAL into the main db ONCE up front. The ingest commits
            // one giant transaction, leaving a multi-GB WAL; reading back through
            // it merges WAL+db pages per read — brutal at full-wiki scale. Best
            // effort (a read-only / busy db just keeps the WAL).
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            // Only **in-scope** articles need a row in `articles.lance` — at
            // full wiki, in-scope is ~52k of 1.67M (the rest are dangling link
            // targets, represented via `edges.target_in_scope`, not their own
            // rows). The neighbor API never reads a dangling target's record.
            let mut astmt = conn
                .prepare(
                    "SELECT a.title, a.wikidata_qid, a.revision_id, a.in_scope, \
                            a.pov_total, a.citation_total, \
                            EXISTS(SELECT 1 FROM section_signals s \
                                   WHERE s.corpus_id = a.corpus_id AND s.article_id = a.id \
                                     AND s.is_contested = 1) \
                     FROM articles a WHERE a.corpus_id = ?1 AND a.in_scope = 1",
                )
                .map_err(|e| Error::Database(format!("export articles prepare: {e}")))?;
            let articles: Vec<WikiArticleRow> = astmt
                .query_map(params![self.corpus_id], |row| {
                    Ok(WikiArticleRow {
                        title: row.get(0)?,
                        wikidata_qid: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        revision_id: row.get::<_, Option<i64>>(2)?.unwrap_or(-1),
                        in_scope: row.get::<_, i64>(3)? == 1,
                        pov_total: row.get(4)?,
                        citation_total: row.get(5)?,
                        is_contested: row.get::<_, i64>(6)? == 1,
                    })
                })
                .map_err(|e| Error::Database(format!("export articles query: {e}")))?
                .filter_map(|r| r.ok())
                .collect();
            drop(astmt);
            let mut estmt = conn
                .prepare(
                    "SELECT src.title, e.target_title, e.relationship_type, e.link_text, \
                            e.occurrence_count, e.source_section_path, \
                            COALESCE(tgt.in_scope, 0) \
                     FROM edges e \
                     JOIN articles src \
                       ON src.id = e.source_article_id AND src.corpus_id = e.corpus_id \
                     LEFT JOIN articles tgt \
                       ON tgt.id = e.target_article_id AND tgt.corpus_id = e.corpus_id \
                     WHERE e.corpus_id = ?1",
                )
                .map_err(|e| Error::Database(format!("export edges prepare: {e}")))?;
            let edges: Vec<WikiEdgeRow> = estmt
                .query_map(params![self.corpus_id], |row| {
                    Ok(WikiEdgeRow {
                        source_title: row.get(0)?,
                        target_title: row.get(1)?,
                        relationship_type: row.get(2)?,
                        link_text: row.get(3)?,
                        occurrence_count: row.get(4)?,
                        source_section_path: row.get(5)?,
                        target_in_scope: row.get::<_, i64>(6)? == 1,
                    })
                })
                .map_err(|e| Error::Database(format!("export edges query: {e}")))?
                .filter_map(|r| r.ok())
                .collect();
            (articles, edges)
        };
        crate::enrichment::atlas::wiki_store::write_wikipedia_columnar_store(
            atlas_dir, &articles, &edges,
        )
        .await
        .map_err(|e| Error::Database(format!("export columnar write: {e}")))?;
        Ok(())
    }

    // ─── Stats / staleness ───────────────────────────────────

    /// Total articles in scope for this corpus.
    pub async fn article_count(&self) -> usize {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE corpus_id = ? AND in_scope = 1",
            params![self.corpus_id],
            |r| r.get::<_, usize>(0),
        )
        .unwrap_or(0)
    }

    /// Total edges (not deduped — a `(source, section, target)` row
    /// counts once regardless of `occurrence_count`).
    pub async fn edge_count(&self) -> usize {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE corpus_id = ?",
            params![self.corpus_id],
            |r| r.get::<_, usize>(0),
        )
        .unwrap_or(0)
    }

    /// Compute a staleness verdict by comparing the graph's recorded
    /// `revision_id_max` against the `current_max_revision_id` the
    /// caller has just observed in LanceDB. The caller pulls the
    /// observed max with a single `SELECT MAX(json_extract(metadata,
    /// '$.revision_id'))` so this method stays cheap (no LanceDB
    /// dependency in corpus-engine's own DB code).
    pub async fn staleness_for(&self, current_max_revision_id: Option<i64>) -> StalenessCaution {
        let conn = self.conn.lock().await;
        let stored: Option<i64> = conn
            .query_row(
                "SELECT value FROM wiki_meta WHERE key = 'revision_id_max'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok());
        let last_built_at: Option<String> = conn
            .query_row(
                "SELECT value FROM wiki_meta WHERE key = 'last_built_at'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok();

        let age_hours = last_built_at
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| {
                let age = chrono::Utc::now() - t.with_timezone(&chrono::Utc);
                age.num_hours().max(0) as u64
            })
            .unwrap_or(0);

        match (stored, current_max_revision_id) {
            (Some(stored_max), Some(current_max)) if current_max > stored_max => {
                if age_hours >= 24 {
                    StalenessCaution::GraphIsStale {
                        age_hours,
                        corpus_id: self.corpus_id.clone(),
                    }
                } else {
                    StalenessCaution::GraphIsAging { age_hours }
                }
            }
            _ => StalenessCaution::None,
        }
    }

    // ─── Bulk ingest ─────────────────────────────────────────

    /// Bulk-load the graph from a vector of LanceDB chunks with raw
    /// metadata (typically the output of
    /// `CorpusIndex::all_chunks_with_raw_metadata`). Aggregates
    /// duplicate edges per `(article, section)` in memory before
    /// insert; uses `INSERT OR IGNORE` so a re-run is a no-op (it
    /// will not reset `occurrence_count`).
    ///
    /// This is the cheap structural path — no LLM, no embedding.
    /// At Wikipedia Vital L5 scope (~51K articles) it completes in
    /// <5 minutes on a modern laptop.
    pub async fn ingest_from_chunks(
        &self,
        chunks: Vec<StoredChunkWithMetadata>,
    ) -> Result<IngestSummary> {
        // 1. Aggregate per (title, section_path) in memory.
        //    The chunker emits N chunks per section; without this
        //    aggregation we'd insert N duplicate edges per section.
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

            // Resolve canonical article title — prefer chunk.title if
            // present, fall back to URL-derived title. Articles without
            // either are skipped (they're typically extraction
            // artifacts).
            let article_title = chunk
                .title
                .clone()
                .or_else(|| chunk.url.as_deref().and_then(wiki_title_from_url));
            let Some(article_title) = article_title else {
                continue;
            };

            let entry = articles
                .entry(article_title.clone())
                .or_insert_with(|| AggregatedArticle::new(article_title));

            // Carry per-article fields from the first metadata seen.
            // Wikipedia revisions identify the same article across
            // its sections; later chunks overwrite only when the
            // stored value is None.
            if entry.wikidata_qid.is_none() {
                entry.wikidata_qid = meta.wikidata_qid.clone();
            }
            if entry.page_id.is_none() {
                entry.page_id = meta.page_id;
            }
            if entry.revision_id.is_none() {
                entry.revision_id = meta.revision_id;
            }

            // Per-section aggregation.
            let section_path_joined = join_section_path(&meta.section_path);
            let section = entry
                .sections
                .entry(section_path_joined.clone())
                .or_insert_with(|| AggregatedSection::new(meta.section_type.clone()));

            // First time seeing this section: capture its counts.
            if !section.counts_seen {
                section.pov_count = meta.pov_count.unwrap_or(0);
                section.citation_needed_count = meta.citation_needed_count.unwrap_or(0);
                section.clarification_needed_count = meta.clarification_needed_count.unwrap_or(0);
                section.update_count = meta.update_count.unwrap_or(0);
                section.section_type = meta.section_type.clone();
                section.counts_seen = true;
            }

            // Outgoing-link aggregation. The chunker repeats the same
            // outgoing_links across every chunk in a section, so the
            // structural truth — "this section links to target X
            // once" — is encoded as occurrence_count=1 per
            // (section, target) row. Cross-section aggregation is
            // done by SUM at query time. First-seen link_text wins.
            for link in &meta.outgoing_links {
                section
                    .outgoing
                    .entry(link.target_title.clone())
                    .or_insert_with(|| AggregatedEdge {
                        link_text: link.link_text.clone(),
                        relationship_type: classify_relationship(
                            &meta.section_path,
                            &link.link_text,
                        ),
                        occurrence_count: 1,
                    });
            }

            // Pull article-level totals out of section counts, in case
            // a Layer 1 query wants pov_total without a section join.
            entry.pov_total += meta.pov_count.unwrap_or(0);
            entry.citation_total += meta.citation_needed_count.unwrap_or(0);
        }

        // 2. Bulk insert in a single transaction.
        let mut summary = IngestSummary {
            chunks_with_metadata,
            chunks_without_metadata,
            ..Default::default()
        };
        let conn = self.conn.lock().await;
        let corpus = self.corpus_id.clone();

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| Error::Database(format!("begin: {e}")))?;

        // 2a. Articles. Each in-scope source article gets a row with
        //     in_scope=1; later we'll ALSO insert dangling target
        //     titles with in_scope=0 (so the resolution pass can
        //     cheaply update target_article_id).
        for art in articles.values() {
            let inserted = conn
                .execute(
                    "INSERT OR IGNORE INTO articles
                     (corpus_id, title, page_id, wikidata_qid, revision_id,
                      in_scope, pov_total, citation_total)
                     VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
                    params![
                        corpus,
                        art.title,
                        art.page_id,
                        art.wikidata_qid,
                        art.revision_id,
                        art.pov_total,
                        art.citation_total,
                    ],
                )
                .map_err(|e| Error::Database(format!("insert article: {e}")))?;
            if inserted > 0 {
                summary.articles_inserted += 1;
            } else {
                // Already exists (rebuild path). Update the typed
                // fields anyway so a re-ingest picks up new revisions.
                let _ = conn.execute(
                    "UPDATE articles
                     SET page_id = COALESCE(?, page_id),
                         wikidata_qid = COALESCE(?, wikidata_qid),
                         revision_id = COALESCE(?, revision_id),
                         in_scope = 1,
                         pov_total = ?,
                         citation_total = ?
                     WHERE corpus_id = ? AND title = ?",
                    params![
                        art.page_id,
                        art.wikidata_qid,
                        art.revision_id,
                        art.pov_total,
                        art.citation_total,
                        corpus,
                        art.title,
                    ],
                );
            }
        }

        // 2b. Build a title → article_id map for FK lookups during
        //     edge insertion.
        let mut title_to_id: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT id, title FROM articles WHERE corpus_id = ?")
                .map_err(|e| Error::Database(format!("prep title map: {e}")))?;
            let rows = stmt
                .query_map(params![corpus], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| Error::Database(format!("query title map: {e}")))?;
            for row in rows.filter_map(|r| r.ok()) {
                title_to_id.insert(row.1, row.0);
            }
        }

        // 2c. Edges + section_signals.
        for art in articles.values() {
            let Some(&article_id) = title_to_id.get(&art.title) else {
                continue;
            };
            for (section_path, section) in &art.sections {
                let outgoing_link_count = section.outgoing.len() as i64;
                let is_contested =
                    (section.pov_count > 0 || section.section_type == "controversy") as i64;

                let inserted = conn
                    .execute(
                        "INSERT OR IGNORE INTO section_signals
                         (corpus_id, article_id, section_path, section_type,
                          pov_count, citation_needed_count,
                          clarification_needed_count, update_count,
                          outgoing_link_count, is_contested)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        params![
                            corpus,
                            article_id,
                            section_path,
                            section.section_type,
                            section.pov_count,
                            section.citation_needed_count,
                            section.clarification_needed_count,
                            section.update_count,
                            outgoing_link_count,
                            is_contested,
                        ],
                    )
                    .map_err(|e| Error::Database(format!("insert sig: {e}")))?;
                if inserted > 0 {
                    summary.sections_inserted += 1;
                } else {
                    // Rebuild path — refresh per-section counts.
                    let _ = conn.execute(
                        "UPDATE section_signals
                         SET section_type = ?, pov_count = ?,
                             citation_needed_count = ?,
                             clarification_needed_count = ?,
                             update_count = ?, outgoing_link_count = ?,
                             is_contested = ?
                         WHERE corpus_id = ? AND article_id = ? AND section_path = ?",
                        params![
                            section.section_type,
                            section.pov_count,
                            section.citation_needed_count,
                            section.clarification_needed_count,
                            section.update_count,
                            outgoing_link_count,
                            is_contested,
                            corpus,
                            article_id,
                            section_path,
                        ],
                    );
                }

                for (target_title, edge) in &section.outgoing {
                    let inserted = conn
                        .execute(
                            "INSERT OR IGNORE INTO edges
                             (corpus_id, source_article_id, source_section_path,
                              section_type, target_title, link_text,
                              relationship_type, occurrence_count)
                             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                            params![
                                corpus,
                                article_id,
                                section_path,
                                section.section_type,
                                target_title,
                                edge.link_text,
                                edge.relationship_type,
                                edge.occurrence_count,
                            ],
                        )
                        .map_err(|e| Error::Database(format!("insert edge: {e}")))?;
                    if inserted > 0 {
                        summary.edges_inserted += 1;
                    } else {
                        // Rebuild path: refresh occurrence_count,
                        // never reset.
                        let _ = conn.execute(
                            "UPDATE edges
                             SET occurrence_count = ?,
                                 relationship_type = ?,
                                 link_text = ?,
                                 section_type = ?
                             WHERE corpus_id = ? AND source_article_id = ?
                               AND source_section_path = ? AND target_title = ?",
                            params![
                                edge.occurrence_count,
                                edge.relationship_type,
                                edge.link_text,
                                section.section_type,
                                corpus,
                                article_id,
                                section_path,
                                target_title,
                            ],
                        );
                    }
                }
            }
        }

        // 2d. Insert dangling targets (in_scope=0). A dangling target
        //     is a target_title on an edge that has no in-scope
        //     article row. We add a stub row so cluster/bridge
        //     analysis at Layer 1 can decide whether to include or
        //     exclude these — without the row, every JOIN would have
        //     to LEFT-JOIN on title which is needlessly slow at full
        //     Wikipedia scale.
        let _ = conn.execute(
            "INSERT OR IGNORE INTO articles (corpus_id, title, in_scope)
             SELECT DISTINCT e.corpus_id, e.target_title, 0
             FROM edges e
             LEFT JOIN articles a
               ON a.corpus_id = e.corpus_id AND a.title = e.target_title
             WHERE e.corpus_id = ? AND a.id IS NULL",
            params![corpus],
        );

        // 2e. Resolve target_article_id on every edge in a single
        //     UPDATE. This is what makes one-hop-by-id lookups cheap.
        let _ = conn.execute(
            "UPDATE edges
             SET target_article_id = (
                 SELECT a.id FROM articles a
                 WHERE a.corpus_id = edges.corpus_id
                   AND a.title = edges.target_title
             )
             WHERE corpus_id = ?",
            params![corpus],
        );

        // 2f. Track dangling target count for the build summary.
        summary.dangling_targets = conn
            .query_row(
                "SELECT COUNT(*) FROM articles WHERE corpus_id = ? AND in_scope = 0",
                params![corpus],
                |r| r.get::<_, usize>(0),
            )
            .unwrap_or(0);

        // 2g. Stamp build metadata. revision_id_max is read by the
        //     staleness probe.
        let max_rev: Option<i64> = articles.values().filter_map(|a| a.revision_id).max();
        if let Some(max_rev) = max_rev {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO wiki_meta (key, value) VALUES ('revision_id_max', ?)",
                params![max_rev.to_string()],
            );
        }
        let _ = conn.execute(
            "INSERT OR REPLACE INTO wiki_meta (key, value) VALUES ('last_built_at', ?)",
            params![chrono::Utc::now().to_rfc3339()],
        );

        conn.execute_batch("COMMIT")
            .map_err(|e| Error::Database(format!("commit: {e}")))?;

        Ok(summary)
    }
}

// ─── Aggregation helpers (private) ───────────────────────────

struct AggregatedArticle {
    title: String,
    page_id: Option<i64>,
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
            page_id: None,
            wikidata_qid: None,
            revision_id: None,
            pov_total: 0,
            citation_total: 0,
            sections: HashMap::new(),
        }
    }
}

struct AggregatedSection {
    section_type: String,
    pov_count: i64,
    citation_needed_count: i64,
    clarification_needed_count: i64,
    update_count: i64,
    counts_seen: bool,
    outgoing: HashMap<String, AggregatedEdge>,
}

impl AggregatedSection {
    fn new(section_type: String) -> Self {
        Self {
            section_type,
            pov_count: 0,
            citation_needed_count: 0,
            clarification_needed_count: 0,
            update_count: 0,
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

fn join_section_path(parts: &[String]) -> String {
    parts.join(&SECTION_PATH_DELIMITER.to_string())
}

/// Rule-based relationship-type classifier. Runs at insert time on
/// every edge; spends zero LLM tokens. ~80% accuracy is the bar — the
/// label is one of several signals the re-ranker mixes via RRF, not
/// a load-bearing decision.
///
/// Order matters: section-path patterns dominate (causal section beats
/// "is" link-text), then link-text verb prefixes, then default to
/// `topical`.
fn classify_relationship(section_path: &[String], link_text: &str) -> String {
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

// ─── WikipediaGraphApi: backend-agnostic neighbor surface (W3) ────────────────

/// The query surface the runtime consumes from a Wikipedia link graph —
/// implemented by both the SQLite [`WikipediaGraph`] and the columnar
/// [`crate::wikipedia_columnar::ColumnarWikipediaGraph`], so the runtime can hold
/// `Arc<dyn WikipediaGraphApi>` and stay backend-agnostic. `#[async_trait]` keeps
/// it `dyn`-safe (the methods are async). WIKIPEDIA_ATLAS_V2 W3 — the seam the
/// columnar store is swapped in behind, before W4 retires the SQLite.
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

#[async_trait::async_trait]
impl WikipediaGraphApi for WikipediaGraph {
    async fn neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        WikipediaGraph::neighbors(self, title, limit).await
    }
    async fn neighbors_for_axis(
        &self,
        title: &str,
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        WikipediaGraph::neighbors_for_axis(self, title, axis_terms, limit).await
    }
    async fn co_neighbors(
        &self,
        titles: &[String],
        axis_terms: &[String],
        limit: usize,
    ) -> Vec<Neighbor> {
        WikipediaGraph::co_neighbors(self, titles, axis_terms, limit).await
    }
    async fn reverse_neighbors(&self, title: &str, limit: usize) -> Vec<Neighbor> {
        WikipediaGraph::reverse_neighbors(self, title, limit).await
    }
    async fn has_contested_section(&self, title: &str) -> bool {
        WikipediaGraph::has_contested_section(self, title).await
    }
    async fn record(&self, title: &str) -> Option<ArticleRecord> {
        WikipediaGraph::record(self, title).await
    }
    async fn article_count(&self) -> usize {
        WikipediaGraph::article_count(self).await
    }
    async fn edge_count(&self) -> usize {
        WikipediaGraph::edge_count(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::wikipedia_types::WikiLink;

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

    #[tokio::test]
    async fn ingests_simple_article_and_resolves_neighbors() {
        let g = WikipediaGraph::open_in_memory("test").unwrap();
        let chunks = vec![chunk(
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
        )];
        let summary = g.ingest_from_chunks(chunks).await.unwrap();
        assert_eq!(summary.articles_inserted, 1);
        assert_eq!(summary.edges_inserted, 2);
        assert_eq!(summary.dangling_targets, 2);

        let neighbors = g.neighbors("Albert Einstein", 5).await;
        let titles: Vec<&str> = neighbors.iter().map(|n| n.title.as_str()).collect();
        assert!(titles.contains(&"Special relativity"));
        assert!(titles.contains(&"Photoelectric effect"));
        // Both targets are out-of-scope at Vital L5 minimum scale,
        // so in_scope should be false on the neighbor records.
        assert!(neighbors.iter().all(|n| !n.in_scope));
    }

    /// WIKIPEDIA_ATLAS_V2 W1b: the columnar export served by
    /// `ColumnarWikipediaGraph` answers `neighbors` / `neighbors_for_axis` /
    /// `has_contested_section` identically to this SQLite graph — the
    /// gold-standard parity for the SQLite → Lance migration.
    #[tokio::test]
    async fn columnar_export_matches_sqlite_neighbors() {
        use crate::wikipedia_columnar::ColumnarWikipediaGraph;
        let g = WikipediaGraph::open_in_memory("test").unwrap();
        // Cross-linked so some targets are themselves in-scope sources.
        let chunks = vec![
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
                    vec!["Lead"],
                    "lead",
                    None,
                    vec![
                        ("Albert Einstein", "Einstein"),
                        ("Photoelectric effect", "photoelectric effect"),
                    ],
                ),
            ),
        ];
        g.ingest_from_chunks(chunks).await.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        g.export_columnar(tmp.path()).await.unwrap();
        let c = ColumnarWikipediaGraph::open(tmp.path()).await.unwrap();

        // Compare neighbor sets as sorted (title, rel, occ, in_scope) tuples.
        let key = |ns: Vec<Neighbor>| {
            let mut v: Vec<(String, String, i64, bool)> = ns
                .into_iter()
                .map(|n| (n.title, n.relationship_type, n.occurrence_count, n.in_scope))
                .collect();
            v.sort();
            v
        };
        for title in ["Albert Einstein", "Special relativity"] {
            assert_eq!(
                key(g.neighbors(title, 50).await),
                key(c.neighbors(title, 50).await),
                "neighbors parity for {title}",
            );
        }
        // Axis-filtered parity on a real section term.
        let axis = vec!["criticism".to_string()];
        assert_eq!(
            key(g.neighbors_for_axis("Albert Einstein", &axis, 50).await),
            key(c.neighbors_for_axis("Albert Einstein", &axis, 50).await),
            "neighbors_for_axis parity",
        );
        // Contested-section parity (the Criticism/controversy section).
        assert_eq!(
            g.has_contested_section("Albert Einstein").await,
            c.has_contested_section("Albert Einstein").await,
        );
        assert!(c.has_contested_section("Albert Einstein").await);
    }

    #[tokio::test]
    async fn dedupes_chunked_section() {
        // Two chunks of the same section repeat the same outgoing
        // links — must collapse into a single edge with
        // occurrence_count = 1, not 2.
        let g = WikipediaGraph::open_in_memory("test").unwrap();
        let m1 = meta_with(
            vec!["Lead"],
            "lead",
            None,
            vec![("Black hole", "black hole")],
        );
        let m2 = m1.clone();
        let chunks = vec![
            chunk(1, "Albert Einstein", m1),
            chunk(2, "Albert Einstein", m2),
        ];
        let summary = g.ingest_from_chunks(chunks).await.unwrap();
        // One article, one edge — the chunker emitted two chunks but
        // the section's link set is the same.
        assert_eq!(summary.edges_inserted, 1);
        let n = g.neighbors("Albert Einstein", 5).await;
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].occurrence_count, 1);
    }

    #[tokio::test]
    async fn aggregates_across_sections() {
        // Same target appears from two different sections — both
        // edges land separately, occurrence_count totals 2.
        let g = WikipediaGraph::open_in_memory("test").unwrap();
        let chunks = vec![
            chunk(
                1,
                "Albert Einstein",
                meta_with(
                    vec!["Lead"],
                    "lead",
                    None,
                    vec![("Niels Bohr", "Niels Bohr")],
                ),
            ),
            chunk(
                2,
                "Albert Einstein",
                meta_with(
                    vec!["Quantum mechanics"],
                    "factual",
                    None,
                    vec![("Niels Bohr", "Bohr")],
                ),
            ),
        ];
        let summary = g.ingest_from_chunks(chunks).await.unwrap();
        assert_eq!(summary.edges_inserted, 2);
        let n = g.neighbors("Albert Einstein", 5).await;
        assert_eq!(n.iter().filter(|n| n.title == "Niels Bohr").count(), 1);
        let bohr = n.iter().find(|n| n.title == "Niels Bohr").unwrap();
        // SUM(occurrence_count) across the two section-rows.
        assert_eq!(bohr.occurrence_count, 2);
    }

    #[tokio::test]
    async fn flags_contested_sections() {
        let g = WikipediaGraph::open_in_memory("test").unwrap();
        let chunks = vec![
            chunk(
                1,
                "Atomic bombings of Hiroshima",
                meta_with(
                    vec!["Debate over bombings"],
                    "controversy",
                    Some(2),
                    vec![("Truman", "Truman")],
                ),
            ),
            chunk(
                2,
                "Atomic bombings of Hiroshima",
                meta_with(vec!["Lead"], "lead", None, vec![]),
            ),
        ];
        g.ingest_from_chunks(chunks).await.unwrap();
        assert!(
            g.has_contested_section("Atomic bombings of Hiroshima")
                .await
        );
    }

    #[tokio::test]
    async fn relationship_classifier_picks_contested_via_section() {
        assert_eq!(
            classify_relationship(&["Criticism".to_string()], "John Smith",),
            "contested",
        );
        assert_eq!(
            classify_relationship(&["Origins".to_string()], "Industrial Revolution",),
            "causal",
        );
        assert_eq!(
            classify_relationship(&["See also".to_string()], "anything"),
            "see-also",
        );
        assert_eq!(
            classify_relationship(&[], "led to widespread famine"),
            "causal",
        );
        assert_eq!(
            classify_relationship(&[], "is a type of physicist"),
            "defines",
        );
        assert_eq!(
            classify_relationship(&["History".to_string()], "Albert Einstein"),
            "topical",
        );
    }

    #[tokio::test]
    async fn rebuild_updates_in_place() {
        let g = WikipediaGraph::open_in_memory("test").unwrap();
        let m1 = meta_with(
            vec!["Lead"],
            "lead",
            None,
            vec![("Black hole", "black hole")],
        );
        g.ingest_from_chunks(vec![chunk(1, "X", m1)]).await.unwrap();
        // Run again without clear — should be idempotent.
        let m2 = meta_with(vec!["Lead"], "lead", None, vec![("Black hole", "BH")]);
        g.ingest_from_chunks(vec![chunk(1, "X", m2)]).await.unwrap();
        assert_eq!(g.article_count().await, 1);
        // Edge count — single triple, link_text refreshed.
        assert_eq!(g.edge_count().await, 1);
    }

    #[tokio::test]
    async fn clear_corpus_wipes_rows() {
        let g = WikipediaGraph::open_in_memory("test").unwrap();
        let m = meta_with(vec!["Lead"], "lead", None, vec![("X", "x")]);
        g.ingest_from_chunks(vec![chunk(1, "Source", m)])
            .await
            .unwrap();
        assert!(g.article_count().await > 0);
        g.clear_corpus().await.unwrap();
        assert_eq!(g.article_count().await, 0);
        assert_eq!(g.edge_count().await, 0);
    }
}
