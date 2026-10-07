// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exact-name symbol lookup. A child module of [`super`] (declared with
//! `#[path]` in `scip_graph.rs`) so it reaches `ScipGraph`'s private
//! connection; it moved here when the corpus scope landed, because
//! `scip_graph.rs` is pinned at its size.

use rusqlite::params;

use super::{ScipGraph, SymbolRow};
use crate::error::{Error, Result};

impl ScipGraph {
    /// Look up symbols by exact name (and optional kind filter)
    /// across every corpus this graph has ingested. The Symbol Lookup
    /// MCP tool uses this as the authoritative source — Lance carried
    /// the same data redundantly until the move to SCIP-as-truth, but
    /// the SQLite path here doesn't depend on the chunk index being
    /// fresh and survives a corrupt Lance corpus.
    ///
    /// `limit` is a hard cap on the row count; pass `8` for the
    /// default tool contract. `kind` is matched verbatim against the
    /// schema's `kind` column when `Some`, and `corpus` against
    /// `corpus_id`; `None` skips either filter. The corpus is a predicate,
    /// not a filter on the result: under the cap, another corpus's rows
    /// would crowd the scoped one's out.
    pub async fn find_symbols_by_name(
        &self,
        name: &str,
        kind: Option<&str>,
        corpus: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SymbolRow>> {
        let limit_clamped: i64 = limit.clamp(1, 256) as i64;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT corpus_id, name, qualified_name, kind, file_path, \
                        line_start, line_end, language \
                 FROM symbols \
                 WHERE name = ?1 AND (?2 IS NULL OR kind = ?2) \
                   AND (?3 IS NULL OR corpus_id = ?3) \
                 ORDER BY corpus_id, file_path, line_start \
                 LIMIT ?4",
            )
            .map_err(|e| Error::Database(format!("find_symbols_by_name prepare: {e}")))?;
        // Collect inside the same scope as `stmt` / `conn`: a MappedRows
        // returned out of a sub-block drops `stmt` before it is consumed.
        let rows = stmt
            .query_map(params![name, kind, corpus, limit_clamped], |row| {
                Ok(SymbolRow {
                    corpus_id: row.get(0)?,
                    name: row.get(1)?,
                    qualified_name: row.get(2)?,
                    kind: row.get(3)?,
                    file_path: row.get(4)?,
                    line_start: row.get(5)?,
                    line_end: row.get(6)?,
                    language: row.get(7)?,
                })
            })
            .map_err(|e| Error::Database(format!("find_symbols_by_name query: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }
}
