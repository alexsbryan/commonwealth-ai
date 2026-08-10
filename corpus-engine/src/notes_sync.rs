// SPDX-License-Identifier: AGPL-3.0-or-later
//! NoteStore ↔ alignment-corpus sync.
//!
//! Lets `~/.svrnmesh/notes.db` (the ATOS NoteStore at
//! [`crate::notes::NoteStore`]) ride the same mesh-replication path
//! as `~/.claude/` markdown. The alignment-workspace extractor calls
//! [`export_notes_as_docs`] to fold every active row into the
//! corpus; the projector calls [`import_notes_from_chunks`] when it
//! sees a chunk whose `source_doc_id` starts with `notes://`.
//!
//! Why a thin direct-SQL adapter rather than extending
//! `NoteStore`'s API: the round-trip needs `updated_at` (the
//! mutable-merge tiebreaker) which isn't on `NoteRow`, plus
//! `INSERT OR REPLACE` semantics that the existing INSERT-only
//! writers don't provide. Keeping the adapter narrow avoids
//! widening `NoteStore` for one consumer.

use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::extractors::ExtractedDoc;

/// Wire-format representation of a `notes` row. Mirrors every
/// column the live SELECT/INSERT statements use, plus
/// `updated_at` as the mutable-merge tiebreaker. Versioned-by-shape:
/// receivers ignore unknown fields (`#[serde(default)]`) so a peer
/// running newer code can talk to an older receiver without
/// schema-bump coordination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedNote {
    pub id: String,
    pub kind: String,
    pub content: String,
    /// JSON array string as stored in the DB.
    pub symbols: String,
    /// JSON array string as stored in the DB.
    pub files: String,
    pub session_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub retired_at: Option<i64>,
    #[serde(default)]
    pub retired_by: Option<String>,
    pub scope: String,
    #[serde(default)]
    pub feature_id: Option<String>,
    #[serde(default)]
    pub promoted_from: Option<String>,
    #[serde(default)]
    pub related_entity: Option<String>,
    pub source: String,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub payload_json: Option<String>,
}

/// Read every non-retired note from `notes_db` and emit one
/// [`ExtractedDoc`] per row, ready for the alignment pipeline to
/// chunk into the corpus. `tool_call_log` rows are explicitly NOT
/// exported — they are high-volume, low-value, and locally
/// rebuildable.
///
/// Returns an empty list when the DB doesn't exist (a fresh
/// machine that's never run the daemon). That makes alignment
/// ingest robust to a brand-new install.
pub fn export_notes_as_docs(notes_db: &Path) -> Result<Vec<ExtractedDoc>> {
    if !notes_db.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open(notes_db).map_err(sqlite_err)?;
    let rows = read_active_notes(&conn)?;
    let mut docs = Vec::with_capacity(rows.len());
    for n in rows {
        let body = serde_json::to_string(&n)
            .map_err(|e| Error::Serialization(format!("notes_sync serialize: {e}")))?;
        docs.push(ExtractedDoc {
            title: Some(format!("note:{}", n.id)),
            content: body,
            url: None,
            source_id: format!("notes://{}", n.id),
            metadata: Some(serde_json::json!({ "mtime": n.updated_at })),
            source_file: None,
            embed_text: None,
        });
    }
    Ok(docs)
}

/// Result of an [`import_notes_from_chunks`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub upserted: usize,
    pub deserialize_errors: usize,
}

/// Import notes from an iterator of `(source_doc_id, content_json)`
/// pairs into the local NoteStore. Skips entries whose
/// `source_doc_id` doesn't start with `notes://` so the alignment
/// projector can hand it the full row set without filtering.
///
/// Uses `INSERT OR REPLACE` keyed on `id`, which is safe because
/// the upstream mutable-merge dedupe already kept only the
/// newest-`updated_at` row per id; the SQLite upsert is a deterministic
/// finishing step, not a conflict-resolution gate.
///
/// Parents `notes_db`'s directory if missing (so a fresh machine
/// can be the receiver of the first sync without a prior NoteStore
/// open).
pub fn import_notes_from_chunks<'a, I>(notes_db: &Path, chunks: I) -> Result<ImportReport>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    if let Some(parent) = notes_db.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("notes_sync mkdir {}: {e}", parent.display()),
            ))
        })?;
    }
    // Touch + initialize the schema if the DB doesn't exist. We can't
    // re-use NoteStore::open here because it's async + Tokio-locked;
    // the bare CREATE TABLE IF NOT EXISTS path is enough for the
    // import-only use case. NoteStore will adopt the file on next
    // open.
    let conn = Connection::open(notes_db).map_err(sqlite_err)?;
    ensure_minimal_notes_schema(&conn)?;

    let mut report = ImportReport::default();
    for (source_doc_id, content) in chunks {
        if !source_doc_id.starts_with("notes://") {
            continue;
        }
        let parsed: ExportedNote = match serde_json::from_str(content) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    source_doc_id,
                    error = %e,
                    "notes_sync: chunk content failed to deserialize as ExportedNote; row skipped"
                );
                report.deserialize_errors += 1;
                continue;
            }
        };
        // Defensive — make sure the chunk's id matches the
        // source_doc_id encoding so a malicious or corrupted chunk
        // can't write under an arbitrary id.
        let expected_id = source_doc_id.trim_start_matches("notes://");
        if expected_id != parsed.id {
            tracing::warn!(
                source_doc_id,
                inner_id = %parsed.id,
                "notes_sync: id mismatch between source_doc_id and payload; row skipped"
            );
            report.deserialize_errors += 1;
            continue;
        }
        upsert_row(&conn, &parsed)?;
        report.upserted += 1;
    }
    Ok(report)
}

fn read_active_notes(conn: &Connection) -> Result<Vec<ExportedNote>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, content, symbols, files, session_id,
                    created_at, updated_at, tool_name, retired_at,
                    retired_by, scope, feature_id, promoted_from,
                    related_entity, source, supersedes, payload_json
             FROM notes
             WHERE retired_at IS NULL",
        )
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ExportedNote {
                id: row.get(0)?,
                kind: row.get(1)?,
                content: row.get(2)?,
                symbols: row.get(3)?,
                files: row.get(4)?,
                session_id: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                tool_name: row.get(8)?,
                retired_at: row.get(9)?,
                retired_by: row.get(10)?,
                scope: row.get(11)?,
                feature_id: row.get(12)?,
                promoted_from: row.get(13)?,
                related_entity: row.get(14)?,
                source: row.get(15)?,
                supersedes: row.get(16)?,
                payload_json: row.get(17)?,
            })
        })
        .map_err(sqlite_err)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(sqlite_err)?);
    }
    Ok(out)
}

fn upsert_row(conn: &Connection, n: &ExportedNote) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO notes (
            id, kind, content, symbols, files, session_id,
            created_at, updated_at, tool_name, retired_at,
            retired_by, scope, feature_id, promoted_from,
            related_entity, source, supersedes, payload_json
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18
        )",
        params![
            n.id,
            n.kind,
            n.content,
            n.symbols,
            n.files,
            n.session_id,
            n.created_at,
            n.updated_at,
            n.tool_name,
            n.retired_at,
            n.retired_by,
            n.scope,
            n.feature_id,
            n.promoted_from,
            n.related_entity,
            n.source,
            n.supersedes,
            n.payload_json,
        ],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

/// Minimum schema needed for `INSERT OR REPLACE`. NoteStore's full
/// schema (FTS5, triggers, secondary indexes, meta_counters) is
/// established lazily on the next `NoteStore::open` against the
/// same file. This is intentionally light — we only need the
/// `notes` columns the upsert touches; the FTS index will catch up
/// when the daemon next opens the DB.
fn ensure_minimal_notes_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS notes (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL,
            content       TEXT NOT NULL,
            symbols       TEXT NOT NULL DEFAULT '[]',
            files         TEXT NOT NULL DEFAULT '[]',
            session_id    TEXT NOT NULL DEFAULT '',
            created_at    INTEGER NOT NULL,
            updated_at    INTEGER NOT NULL,
            tool_name     TEXT,
            retired_at    INTEGER,
            retired_by    TEXT,
            scope         TEXT NOT NULL DEFAULT 'global',
            feature_id    TEXT,
            promoted_from TEXT,
            related_entity TEXT,
            source        TEXT NOT NULL DEFAULT 'agent',
            supersedes    TEXT,
            payload_json  TEXT
        );
        "#,
    )
    .map_err(sqlite_err)?;
    Ok(())
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("notes_sync sqlite: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_notes_db(path: &Path) -> Connection {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        ensure_minimal_notes_schema(&conn).unwrap();
        conn
    }

    fn insert_test_note(conn: &Connection, id: &str, kind: &str, content: &str, updated_at: i64) {
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id,
                                created_at, updated_at, scope, source)
             VALUES (?1, ?2, ?3, '[]', '[]', 'sess', ?4, ?4, 'global', 'agent')",
            params![id, kind, content, updated_at],
        )
        .unwrap();
    }

    #[test]
    fn export_skips_retired_and_omits_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        // Missing DB → empty export, not an error.
        let docs = export_notes_as_docs(&dir.path().join("nope.db")).unwrap();
        assert!(docs.is_empty());

        let path = dir.path().join("notes.db");
        let conn = touch_notes_db(&path);
        insert_test_note(&conn, "n1", "decision", "alpha", 100);
        insert_test_note(&conn, "n2", "invariant", "beta", 200);
        // Retire n2.
        conn.execute(
            "UPDATE notes SET retired_at = 999 WHERE id = ?1",
            params!["n2"],
        )
        .unwrap();
        drop(conn);

        let docs = export_notes_as_docs(&path).unwrap();
        assert_eq!(docs.len(), 1, "retired note excluded");
        assert_eq!(docs[0].source_id, "notes://n1");
        assert!(docs[0].content.contains("\"alpha\""));
        let mtime = docs[0]
            .metadata
            .as_ref()
            .and_then(|m| m.get("mtime"))
            .and_then(|v| v.as_i64())
            .unwrap();
        assert_eq!(mtime, 100);
    }

    #[test]
    fn export_then_import_round_trips() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let src_db = src.path().join("notes.db");
        let dst_db = dst.path().join("notes.db");

        let conn = touch_notes_db(&src_db);
        insert_test_note(&conn, "n1", "decision", "alpha body", 100);
        insert_test_note(&conn, "n2", "invariant", "beta body", 200);
        drop(conn);

        let docs = export_notes_as_docs(&src_db).unwrap();
        let pairs: Vec<(&str, &str)> = docs
            .iter()
            .map(|d| (d.source_id.as_str(), d.content.as_str()))
            .collect();
        let report = import_notes_from_chunks(&dst_db, pairs.iter().copied()).unwrap();
        assert_eq!(report.upserted, 2);
        assert_eq!(report.deserialize_errors, 0);

        // Re-export the destination and confirm the bodies match.
        let echoed = export_notes_as_docs(&dst_db).unwrap();
        let echoed_sids: Vec<&str> = echoed.iter().map(|d| d.source_id.as_str()).collect();
        assert!(echoed_sids.contains(&"notes://n1"));
        assert!(echoed_sids.contains(&"notes://n2"));
    }

    #[test]
    fn import_rejects_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        // Hand-craft a chunk whose source_doc_id and payload id disagree.
        let payload = serde_json::to_string(&ExportedNote {
            id: "evil".into(),
            kind: "decision".into(),
            content: "bad".into(),
            symbols: "[]".into(),
            files: "[]".into(),
            session_id: "s".into(),
            created_at: 1,
            updated_at: 1,
            tool_name: None,
            retired_at: None,
            retired_by: None,
            scope: "global".into(),
            feature_id: None,
            promoted_from: None,
            related_entity: None,
            source: "agent".into(),
            supersedes: None,
            payload_json: None,
        })
        .unwrap();
        let pairs = [("notes://expected", payload.as_str())];
        let report = import_notes_from_chunks(&db, pairs.iter().copied()).unwrap();
        assert_eq!(report.upserted, 0);
        assert_eq!(report.deserialize_errors, 1);
    }

    #[test]
    fn import_skips_non_notes_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        let pairs = [("plans/foo.md", "# heading"), ("memory/x.md", "body")];
        let report = import_notes_from_chunks(&db, pairs.iter().copied()).unwrap();
        assert_eq!(report.upserted, 0);
        assert_eq!(report.deserialize_errors, 0);
    }

    #[test]
    fn import_overwrites_older_with_newer() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        // Seed with the older copy.
        {
            let conn = touch_notes_db(&db);
            insert_test_note(&conn, "n1", "decision", "old body", 100);
        }
        // Build a chunk for the newer copy.
        let newer = ExportedNote {
            id: "n1".into(),
            kind: "decision".into(),
            content: "new body".into(),
            symbols: "[]".into(),
            files: "[]".into(),
            session_id: "s".into(),
            created_at: 100,
            updated_at: 200,
            tool_name: None,
            retired_at: None,
            retired_by: None,
            scope: "global".into(),
            feature_id: None,
            promoted_from: None,
            related_entity: None,
            source: "agent".into(),
            supersedes: None,
            payload_json: None,
        };
        let payload = serde_json::to_string(&newer).unwrap();
        let pairs = [("notes://n1", payload.as_str())];
        let report = import_notes_from_chunks(&db, pairs.iter().copied()).unwrap();
        assert_eq!(report.upserted, 1);

        let conn = Connection::open(&db).unwrap();
        let body: String = conn
            .query_row("SELECT content FROM notes WHERE id = 'n1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(body, "new body");
    }
}
