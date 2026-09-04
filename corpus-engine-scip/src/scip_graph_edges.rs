// SPDX-License-Identifier: AGPL-3.0-or-later
//! The warm tier's write: call edges ADDED to the graph, never replaced.
//!
//! A child module of [`super`] (declared with `#[path]` in `scip_graph.rs`) so
//! it can reach `ScipGraph`'s private connection, without adding 200 lines to a
//! file that is already far past ARCH §3.1's line.

use rusqlite::params;

use super::{ScipGraph, ScipRefRecord};
use crate::error::{Error, Result};

impl ScipGraph {
    /// INSERT-ONLY merge of call edges. Executes no `DELETE`, ever.
    ///
    /// ## Why this is not `replace_files`
    ///
    /// The obvious shape for "the file was saved, here are its edges" is
    /// delete-the-file's-rows-then-insert. For an LSP-derived edge set that
    /// shape destroys data on every save, for two independent reasons, and
    /// both were measured against this code before this function existed:
    ///
    /// 1. The exporter records EVERY non-definition occurrence inside an
    ///    enclosing definition — type mentions, imports, field accesses, trait
    ///    names — not only calls (`scip_export.rs`, the `doc.occurrences`
    ///    loop). A call-hierarchy query returns calls. Replacing the first set
    ///    with the second silently deletes the majority of an edited file's
    ///    edges.
    /// 2. An LSP query that returns nothing is indistinguishable from a file
    ///    that genuinely calls nothing. A still-loading analyzer, a file
    ///    outside the loaded workspace, or a `didChangeWatchedFiles` the server
    ///    has not applied yet all answer `[]` — and a delete keyed on that
    ///    answer empties the file.
    ///
    /// So this tier may only ADD. A call removed from the source leaves its row
    /// behind until the next full export corrects it, which is precisely the
    /// eventual-consistency contract [`replace_file_symbols`] already documents
    /// for edges — and strictly better than the status quo it replaces, where
    /// no new edge appeared at all between exports.
    ///
    /// Rows already present are skipped rather than duplicated: `refs` carries
    /// no UNIQUE constraint (four plain indexes, no uniqueness), so repeated
    /// saves of an unchanged function would otherwise multiply its edges. The
    /// identity of an edge here is its occurrence — corpus, file, position and
    /// the two endpoints.
    ///
    /// Returns how many rows were actually inserted.
    pub async fn add_call_edges_for(
        &self,
        corpus_id: &str,
        refs: &[ScipRefRecord],
    ) -> Result<usize> {
        if refs.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN TRANSACTION")
            .map_err(|e| Error::Database(format!("add_call_edges begin: {e}")))?;

        let txn: std::result::Result<usize, rusqlite::Error> = (|| {
            let mut inserted = 0usize;
            for r in refs {
                let n = conn.execute(
                    "INSERT INTO refs (corpus_id, caller_symbol, callee_symbol, \
                     caller_qualified, callee_qualified, file_path, line, start_col, \
                     end_line, end_col, ref_kind) \
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11 \
                     WHERE NOT EXISTS ( \
                       SELECT 1 FROM refs WHERE corpus_id = ?1 AND file_path = ?6 \
                         AND line = ?7 AND start_col = ?8 \
                         AND caller_symbol = ?2 AND callee_symbol = ?3)",
                    params![
                        corpus_id,
                        r.caller_symbol,
                        r.callee_symbol,
                        r.caller_qualified,
                        r.callee_qualified,
                        r.file_path,
                        r.line,
                        r.start_col,
                        r.end_line,
                        r.end_col,
                        r.ref_kind
                    ],
                )?;
                inserted += n;
            }
            Ok(inserted)
        })();

        match txn {
            Ok(inserted) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| Error::Database(format!("add_call_edges commit: {e}")))?;
                Ok(inserted)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(Error::Database(format!(
                    "add_call_edges failed (rolled back, graph preserved): {e}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;
    use crate::scip_graph::ScipSymbolRecord;

    fn sym(name: &str, file: &str) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: name.into(),
            qualified_name: String::new(),
            kind: "function".into(),
            file_path: file.into(),
            line_start: 1,
            line_end: 2,
            language: "rust".into(),
        }
    }

    fn edge(caller: &str, callee: &str, file: &str, line: i32, col: i32) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: caller.into(),
            callee_symbol: callee.into(),
            // Empty on purpose: the LSP tier that writes these has no SCIP
            // descriptor to offer (rust-analyzer advertises no monikerProvider),
            // and `find_callers` keys on the bare name anyway.
            caller_qualified: String::new(),
            callee_qualified: String::new(),
            file_path: file.into(),
            line,
            start_col: col,
            end_line: line,
            end_col: col + 4,
            ref_kind: "direct".into(),
        }
    }

    #[tokio::test]
    async fn add_call_edges_cannot_delete_an_edge_it_did_not_derive() {
        // THE invariant. The exporter records every non-definition occurrence —
        // type mentions and imports included — while an LSP call-hierarchy query
        // returns calls only. If this write ever became a per-file replace, the
        // type mention below would vanish on the next save of a.rs, one file at
        // a time, looking exactly like the feature working.
        let g = ScipGraph::open_in_memory("alpha").unwrap();
        // The callees are seeded as symbols because `find_callers` resolves
        // its argument through the `symbols` table before it ever touches
        // `refs` — an edge to a name the graph does not know is stored but
        // unreachable. That is not a limitation to work around, it is the
        // reason this tier reuses the exporter's symbol table instead of
        // minting ids of its own: the callee of a call to an EXISTING function
        // is already there.
        g.ingest_symbols_and_refs(
            vec![
                sym("a_one", "a.rs"),
                sym("SomeType", "types.rs"),
                sym("helper", "b.rs"),
            ],
            vec![
                edge("a_one", "SomeType", "a.rs", 5, 8),
                edge("a_one", "helper", "a.rs", 6, 8),
            ],
        )
        .await
        .unwrap();
        assert_eq!(g.ref_count().await, 2);

        // The empty answer — a still-loading analyzer, a file outside the
        // loaded workspace, and a file that truly calls nothing all look like
        // this, and none of them may cost an edge.
        let inserted = g.add_call_edges_for("alpha", &[]).await.unwrap();
        assert_eq!(inserted, 0);
        assert_eq!(g.ref_count().await, 2, "an empty answer removed an edge");

        // A partial answer — only ONE of the two known edges — is still additive.
        let inserted = g
            .add_call_edges_for("alpha", &[edge("a_one", "helper", "a.rs", 6, 8)])
            .await
            .unwrap();
        assert_eq!(inserted, 0, "already present");
        assert_eq!(
            g.ref_count().await,
            2,
            "a partial answer replaced the file's edges"
        );
        let (callers, _) = g.find_callers("SomeType", 1).await.unwrap();
        assert_eq!(
            callers.len(),
            1,
            "the occurrence no LSP query can return survived"
        );
    }

    #[tokio::test]
    async fn add_call_edges_inserts_what_is_new_and_repeats_nothing() {
        let g = ScipGraph::open_in_memory("alpha").unwrap();
        g.ingest_symbols_and_refs(
            vec![
                sym("a_one", "a.rs"),
                sym("helper", "b.rs"),
                sym("other", "c.rs"),
            ],
            vec![],
        )
        .await
        .unwrap();

        let derived = vec![
            edge("a_one", "helper", "a.rs", 6, 8),
            edge("a_one", "other", "a.rs", 7, 8),
        ];
        assert_eq!(g.add_call_edges_for("alpha", &derived).await.unwrap(), 2);
        assert_eq!(g.ref_count().await, 2);

        // `refs` carries no UNIQUE constraint, so idempotence is this
        // function's job. Saving an unchanged file must not multiply its edges.
        assert_eq!(g.add_call_edges_for("alpha", &derived).await.unwrap(), 0);
        assert_eq!(g.ref_count().await, 2, "a second save duplicated edges");

        // A genuinely new call site at a different position IS a new edge.
        let moved = vec![edge("a_one", "helper", "a.rs", 9, 8)];
        assert_eq!(g.add_call_edges_for("alpha", &moved).await.unwrap(), 1);
        assert_eq!(g.ref_count().await, 3);

        let (callers, _) = g.find_callers("other", 1).await.unwrap();
        assert_eq!(
            callers.first().map(|c| c.symbol_name.as_str()),
            Some("a_one"),
            "an LSP-derived edge with no qualified name is still visible to callers()"
        );
    }
}
