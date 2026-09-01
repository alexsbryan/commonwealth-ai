// SPDX-License-Identifier: AGPL-3.0-or-later
//! `FactStore` — a SQLite home for the deterministic code fact base, keyed by
//! `(corpus_id, file_path)` so it can be patched one file at a time.
//!
//! ## Why this exists
//!
//! The fact base (`facts::Facts` — fn defs, `Type { field: value }` config
//! construction-fields, string literals) used to serialize to a single
//! `facts.json` rewritten WHOLE on any change (43 MB / ~280k rows for this
//! repo). Updating one edited file meant rewriting all of it. That is the exact
//! problem this codebase has solved four times over (SCIP graph, LanceDB
//! chunks, atlas atoms-delta, corpus updater): a canonical store keyed by a
//! stable identity, mutated by **delete-by-key-then-insert scoped to the changed
//! unit**, never a whole-artifact rewrite.
//!
//! Facts are flat per-file records with no cross-file edges — SCIP-shaped, not
//! atlas-shaped — so this deliberately mirrors [`corpus_engine_scip::ScipGraph`]:
//! rows carry `(corpus_id, file_path)`, [`replace_files`] does a per-file
//! `DELETE … WHERE corpus_id=? AND file_path=?` then `INSERT` in one
//! transaction (the `stamp_doc_identity` DELETE-FIRST discipline), and
//! [`replace_all`] is the atomic full-corpus build. Multi-corpus by design (the
//! daemon holds one merged store the `facts` tool queries), so every operation
//! takes an explicit `corpus_id` rather than binding one at open time.
//!
//! Reads are indexed SQL, so the `facts` MCP tool and `check-spec` no longer
//! load + scan 43 MB per query.

use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::facts::{CtorField, Facts, FnDef, StrLit};

/// Bump when the schema changes so a stale DB is rebuilt rather than queried.
const SCHEMA_VERSION: i64 = 1;

fn db_err(e: rusqlite::Error) -> Error {
    Error::Database(e.to_string())
}

/// One fn-def query hit, carrying the corpus it came from (the merged store
/// spans corpora; the `facts` tool labels each result with its corpus).
#[derive(Debug, Clone)]
pub struct FnDefHit {
    pub corpus_id: String,
    pub name: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct CtorFieldHit {
    pub corpus_id: String,
    pub struct_type: String,
    pub field: String,
    pub value: String,
    pub enclosing_fn: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct StrLitHit {
    pub corpus_id: String,
    pub content: String,
    pub enclosing_fn: String,
    pub file: String,
    pub line: usize,
}

/// SQLite-backed fact base. `Arc<Mutex<Connection>>` mirrors the other
/// in-crate rusqlite stores (`wikipedia_graph`, `ScipGraph`): one writer at a
/// time, cheap concurrent access through the tokio mutex.
pub struct FactStore {
    conn: Arc<Mutex<Connection>>,
}

impl FactStore {
    /// Open (creating if absent) the fact DB at `path`. A schema-version
    /// mismatch wipes and re-inits — facts are a pure derivation of source, so
    /// there is nothing to preserve; the next `code facts` / overlay refills it.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(db_err)?;
        Self::from_conn(conn)
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        Self::from_conn(conn)
    }

    /// Open the fact store for a corpus index directory, lazily migrating a
    /// legacy `facts.json` monolith into `facts.db` on first use. Returns `None`
    /// when neither exists (no fact base built yet). This keeps `facts` / a
    /// `check-spec` working across the JSON→SQLite cutover: the first read after
    /// deploy imports the old file once, every read after hits the store.
    pub async fn open_for_dir(dir: &Path, corpus_id: &str) -> Result<Option<Self>> {
        let db = dir.join("facts.db");
        if db.exists() {
            return Ok(Some(Self::open(&db)?));
        }
        let json = dir.join("facts.json");
        if json.exists() {
            let facts = crate::facts::Facts::load(&json)?; // io::Error → Error::Io
            let store = Self::open(&db)?;
            store.replace_all(corpus_id, &facts).await?;
            return Ok(Some(store));
        }
        Ok(None)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        Self::ensure_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn ensure_schema(conn: &Connection) -> Result<()> {
        let found: i64 = conn
            .query_row(
                "SELECT value FROM fact_meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if found != 0 && found != SCHEMA_VERSION {
            // Stale schema — drop and rebuild. Facts are re-derivable.
            conn.execute_batch(
                "DROP TABLE IF EXISTS fact_fn_defs;
                 DROP TABLE IF EXISTS fact_ctor_fields;
                 DROP TABLE IF EXISTS fact_str_lits;
                 DROP TABLE IF EXISTS fact_meta;",
            )
            .map_err(db_err)?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fact_fn_defs (
                id INTEGER PRIMARY KEY,
                corpus_id TEXT NOT NULL DEFAULT '',
                name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_fn_name ON fact_fn_defs(name);
             CREATE INDEX IF NOT EXISTS idx_fn_file ON fact_fn_defs(corpus_id, file_path);

             CREATE TABLE IF NOT EXISTS fact_ctor_fields (
                id INTEGER PRIMARY KEY,
                corpus_id TEXT NOT NULL DEFAULT '',
                struct_type TEXT NOT NULL,
                field TEXT NOT NULL,
                value TEXT NOT NULL,
                enclosing_fn TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_ctor_struct ON fact_ctor_fields(struct_type);
             CREATE INDEX IF NOT EXISTS idx_ctor_field ON fact_ctor_fields(field);
             CREATE INDEX IF NOT EXISTS idx_ctor_file ON fact_ctor_fields(corpus_id, file_path);

             CREATE TABLE IF NOT EXISTS fact_str_lits (
                id INTEGER PRIMARY KEY,
                corpus_id TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL,
                enclosing_fn TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_str_file ON fact_str_lits(corpus_id, file_path);

             CREATE TABLE IF NOT EXISTS fact_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );",
        )
        .map_err(db_err)?;

        conn.execute(
            "INSERT OR REPLACE INTO fact_meta (key, value) VALUES ('schema_version', ?)",
            params![SCHEMA_VERSION.to_string()],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Insert every record in `facts` under `corpus_id`. Shared by
    /// `replace_all` and `replace_files` (both delete first, then call this).
    /// Runs inside the caller's open transaction on `conn`.
    fn insert_facts(conn: &Connection, corpus_id: &str, facts: &Facts) -> rusqlite::Result<()> {
        for d in &facts.fn_defs {
            conn.execute(
                "INSERT INTO fact_fn_defs (corpus_id, name, file_path, line) VALUES (?, ?, ?, ?)",
                params![corpus_id, d.name, d.file, d.line as i64],
            )?;
        }
        for c in &facts.ctor_fields {
            conn.execute(
                "INSERT INTO fact_ctor_fields (corpus_id, struct_type, field, value, enclosing_fn, file_path, line)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![corpus_id, c.struct_type, c.field, c.value, c.enclosing_fn, c.file, c.line as i64],
            )?;
        }
        for s in &facts.str_lits {
            conn.execute(
                "INSERT INTO fact_str_lits (corpus_id, content, enclosing_fn, file_path, line)
                 VALUES (?, ?, ?, ?, ?)",
                params![corpus_id, s.content, s.enclosing_fn, s.file, s.line as i64],
            )?;
        }
        Ok(())
    }

    /// Atomic full-corpus (re)build: replace ALL of `corpus_id`'s facts. The
    /// `code facts` batch builder writes here. Never observably empty mid-swap
    /// (single transaction); rolls back on error, preserving the prior facts.
    pub async fn replace_all(&self, corpus_id: &str, facts: &Facts) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN TRANSACTION").map_err(db_err)?;
        let txn: rusqlite::Result<()> = (|| {
            for tbl in ["fact_fn_defs", "fact_ctor_fields", "fact_str_lits"] {
                conn.execute(
                    &format!("DELETE FROM {tbl} WHERE corpus_id = ?"),
                    params![corpus_id],
                )?;
            }
            Self::insert_facts(&conn, corpus_id, facts)?;
            conn.execute(
                "INSERT OR REPLACE INTO fact_meta (key, value) VALUES (?, ?)",
                params![
                    format!("built_at:{corpus_id}"),
                    corpus_engine_yield::time::unix_now().to_string()
                ],
            )?;
            Ok(())
        })();
        finish_txn(&conn, txn, "replace_all")
    }

    /// Per-file incremental merge — the overlay/watcher hot path. `facts` MUST
    /// contain only the records extracted from `files`. Deletes each file's
    /// prior rows across all three tables, inserts the fresh ones, all in one
    /// transaction; every OTHER file is untouched and a failure rolls back.
    /// A deleted source file (in `files`, no records in `facts`) simply drops
    /// its rows.
    pub async fn replace_files(
        &self,
        corpus_id: &str,
        files: &[String],
        facts: &Facts,
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN TRANSACTION").map_err(db_err)?;
        let txn: rusqlite::Result<()> = (|| {
            for f in files {
                for tbl in ["fact_fn_defs", "fact_ctor_fields", "fact_str_lits"] {
                    conn.execute(
                        &format!("DELETE FROM {tbl} WHERE corpus_id = ? AND file_path = ?"),
                        params![corpus_id, f],
                    )?;
                }
            }
            Self::insert_facts(&conn, corpus_id, facts)?;
            Ok(())
        })();
        finish_txn(&conn, txn, "replace_files")
    }

    /// Reconstruct the in-memory [`Facts`] for one corpus — the compatibility
    /// path for `check-spec`, whose deterministic checks iterate `Facts` in
    /// memory (call-graph neighborhood analysis etc.). Cheaper than the old
    /// `facts.json` load for a single corpus and it never touches other corpora.
    pub async fn load_all(&self, corpus_id: &str) -> Result<Facts> {
        let conn = self.conn.lock().await;
        let mut facts = Facts::default();

        let mut stmt = conn
            .prepare("SELECT name, file_path, line FROM fact_fn_defs WHERE corpus_id = ?")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![corpus_id], |r| {
                Ok(FnDef {
                    name: r.get(0)?,
                    file: r.get(1)?,
                    line: r.get::<_, i64>(2)? as usize,
                })
            })
            .map_err(db_err)?;
        for r in rows {
            facts.fn_defs.push(r.map_err(db_err)?);
        }

        let mut stmt = conn
            .prepare("SELECT struct_type, field, value, enclosing_fn, file_path, line FROM fact_ctor_fields WHERE corpus_id = ?")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![corpus_id], |r| {
                Ok(CtorField {
                    struct_type: r.get(0)?,
                    field: r.get(1)?,
                    value: r.get(2)?,
                    enclosing_fn: r.get(3)?,
                    file: r.get(4)?,
                    line: r.get::<_, i64>(5)? as usize,
                })
            })
            .map_err(db_err)?;
        for r in rows {
            facts.ctor_fields.push(r.map_err(db_err)?);
        }

        let mut stmt = conn
            .prepare("SELECT content, enclosing_fn, file_path, line FROM fact_str_lits WHERE corpus_id = ?")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![corpus_id], |r| {
                Ok(StrLit {
                    content: r.get(0)?,
                    enclosing_fn: r.get(1)?,
                    file: r.get(2)?,
                    line: r.get::<_, i64>(3)? as usize,
                })
            })
            .map_err(db_err)?;
        for r in rows {
            facts.str_lits.push(r.map_err(db_err)?);
        }

        Ok(facts)
    }

    /// Distinct corpus ids present — the `facts` tool's "search every corpus"
    /// default. Cheap: reads the small per-table corpus index.
    pub async fn corpora(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT corpus_id FROM (
                    SELECT corpus_id FROM fact_fn_defs
                    UNION SELECT corpus_id FROM fact_ctor_fields
                    UNION SELECT corpus_id FROM fact_str_lits
                 ) ORDER BY corpus_id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(db_err)?);
        }
        Ok(out)
    }

    /// Newest `built_at` across the given corpora (unix secs), for the tool's
    /// freshness stamp. `None` if never built.
    pub async fn built_at(&self, corpora: &[String]) -> Result<Option<i64>> {
        let conn = self.conn.lock().await;
        let mut newest: Option<i64> = None;
        for c in corpora {
            if let Ok(v) = conn.query_row(
                "SELECT value FROM fact_meta WHERE key = ?",
                params![format!("built_at:{c}")],
                |r| r.get::<_, String>(0),
            ) {
                if let Ok(t) = v.parse::<i64>() {
                    newest = Some(newest.map_or(t, |n| n.max(t)));
                }
            }
        }
        Ok(newest)
    }

    // ── Query surface (the `facts` MCP tool) ──
    // `corpus` = None searches every corpus; Some(id) scopes to one. Substring
    // match mirrors the tool's prior in-memory `.contains()` behaviour. `line`
    // is stored as i64; callers get usize.

    pub async fn find_fn_defs(
        &self,
        corpus: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FnDefHit>> {
        let like = format!("%{}%", escape_like(query));
        let conn = self.conn.lock().await;
        let (sql, scoped) = scoped_sql(
            "SELECT corpus_id, name, file_path, line FROM fact_fn_defs \
             WHERE name LIKE ?1 ESCAPE '\\'",
            corpus,
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let map = |r: &rusqlite::Row| {
            Ok(FnDefHit {
                corpus_id: r.get(0)?,
                name: r.get(1)?,
                file: r.get(2)?,
                line: r.get::<_, i64>(3)? as usize,
            })
        };
        let rows = if scoped {
            stmt.query_map(params![like, corpus.unwrap(), limit as i64], map)
        } else {
            stmt.query_map(params![like, limit as i64], map)
        }
        .map_err(db_err)?;
        collect(rows)
    }

    pub async fn find_ctor_fields(
        &self,
        corpus: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CtorFieldHit>> {
        let like = format!("%{}%", escape_like(query));
        let conn = self.conn.lock().await;
        // Matches struct_type OR field OR value (the tool's prior behaviour).
        let (sql, scoped) = scoped_sql(
            "SELECT corpus_id, struct_type, field, value, enclosing_fn, file_path, line \
             FROM fact_ctor_fields \
             WHERE (struct_type LIKE ?1 ESCAPE '\\' OR field LIKE ?1 ESCAPE '\\' OR value LIKE ?1 ESCAPE '\\')",
            corpus,
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let map = |r: &rusqlite::Row| {
            Ok(CtorFieldHit {
                corpus_id: r.get(0)?,
                struct_type: r.get(1)?,
                field: r.get(2)?,
                value: r.get(3)?,
                enclosing_fn: r.get(4)?,
                file: r.get(5)?,
                line: r.get::<_, i64>(6)? as usize,
            })
        };
        let rows = if scoped {
            stmt.query_map(params![like, corpus.unwrap(), limit as i64], map)
        } else {
            stmt.query_map(params![like, limit as i64], map)
        }
        .map_err(db_err)?;
        collect(rows)
    }

    pub async fn find_str_lits(
        &self,
        corpus: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StrLitHit>> {
        let like = format!("%{}%", escape_like(query));
        let conn = self.conn.lock().await;
        let (sql, scoped) = scoped_sql(
            "SELECT corpus_id, content, enclosing_fn, file_path, line FROM fact_str_lits \
             WHERE content LIKE ?1 ESCAPE '\\'",
            corpus,
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let map = |r: &rusqlite::Row| {
            Ok(StrLitHit {
                corpus_id: r.get(0)?,
                content: r.get(1)?,
                enclosing_fn: r.get(2)?,
                file: r.get(3)?,
                line: r.get::<_, i64>(4)? as usize,
            })
        };
        let rows = if scoped {
            stmt.query_map(params![like, corpus.unwrap(), limit as i64], map)
        } else {
            stmt.query_map(params![like, limit as i64], map)
        }
        .map_err(db_err)?;
        collect(rows)
    }
}

/// Append the corpus scope + LIMIT to a base query. Keeps `?1` as the LIKE
/// pattern; when scoped, the corpus id is the next positional param, then LIMIT.
fn scoped_sql(base: &str, corpus: Option<&str>) -> (String, bool) {
    match corpus {
        Some(_) => (format!("{base} AND corpus_id = ?2 LIMIT ?3"), true),
        None => (format!("{base} LIMIT ?2"), false),
    }
}

fn collect<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(db_err)?);
    }
    Ok(out)
}

/// Escape LIKE metacharacters so a literal `%`/`_`/`\` in a query matches
/// literally (paired with `ESCAPE '\'` in the SQL).
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn finish_txn(conn: &Connection, txn: rusqlite::Result<()>, what: &str) -> Result<()> {
    match txn {
        Ok(()) => {
            conn.execute_batch("COMMIT").map_err(db_err)?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(Error::Database(format!(
                "FactStore::{what} failed (rolled back, prior facts preserved): {e}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnd(name: &str, file: &str, line: usize) -> FnDef {
        FnDef {
            name: name.into(),
            file: file.into(),
            line,
        }
    }
    fn ctor(struct_type: &str, field: &str, value: &str, file: &str) -> CtorField {
        CtorField {
            struct_type: struct_type.into(),
            field: field.into(),
            value: value.into(),
            enclosing_fn: "f".into(),
            file: file.into(),
            line: 1,
        }
    }
    fn strl(content: &str, file: &str) -> StrLit {
        StrLit {
            content: content.into(),
            enclosing_fn: "f".into(),
            file: file.into(),
            line: 1,
        }
    }

    #[tokio::test]
    async fn replace_all_then_query() {
        let s = FactStore::open_in_memory().unwrap();
        let facts = Facts {
            fn_defs: vec![
                fnd("export_changed", "scip.rs", 300),
                fnd("replace_files", "g.rs", 10),
            ],
            ctor_fields: vec![ctor("CodeWatcher", "debounce", "800", "watch.rs")],
            str_lits: vec![strl("never_run", "lint.rs")],
        };
        s.replace_all("demo", &facts).await.unwrap();

        assert_eq!(
            s.find_fn_defs(Some("demo"), "export_changed", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            s.find_ctor_fields(Some("demo"), "debounce", 10)
                .await
                .unwrap()[0]
                .value,
            "800"
        );
        assert_eq!(
            s.find_str_lits(None, "never_run", 10).await.unwrap().len(),
            1
        );
        assert_eq!(s.corpora().await.unwrap(), vec!["demo".to_string()]);
    }

    #[tokio::test]
    async fn replace_all_is_idempotent_not_appending() {
        let s = FactStore::open_in_memory().unwrap();
        let facts = Facts {
            fn_defs: vec![fnd("a", "a.rs", 1)],
            ..Default::default()
        };
        s.replace_all("c", &facts).await.unwrap();
        s.replace_all("c", &facts).await.unwrap();
        // Rebuilt, not doubled.
        assert_eq!(s.find_fn_defs(Some("c"), "a", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replace_files_merges_only_named_files() {
        let s = FactStore::open_in_memory().unwrap();
        s.replace_all(
            "c",
            &Facts {
                fn_defs: vec![fnd("a1", "a.rs", 1), fnd("b1", "b.rs", 1)],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Re-index only a.rs (now with a different fn). b.rs must be untouched.
        s.replace_files(
            "c",
            &["a.rs".to_string()],
            &Facts {
                fn_defs: vec![fnd("a2", "a.rs", 5)],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert!(s
            .find_fn_defs(Some("c"), "a1", 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(s.find_fn_defs(Some("c"), "a2", 10).await.unwrap().len(), 1);
        assert_eq!(s.find_fn_defs(Some("c"), "b1", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replace_files_deletes_all_kinds_for_a_file() {
        let s = FactStore::open_in_memory().unwrap();
        s.replace_all(
            "c",
            &Facts {
                fn_defs: vec![fnd("f", "x.rs", 1)],
                ctor_fields: vec![ctor("S", "fld", "v", "x.rs")],
                str_lits: vec![strl("lit", "x.rs")],
            },
        )
        .await
        .unwrap();
        // Empty re-index of x.rs (file deleted) drops every kind.
        s.replace_files("c", &["x.rs".to_string()], &Facts::default())
            .await
            .unwrap();
        assert!(s.find_fn_defs(Some("c"), "f", 10).await.unwrap().is_empty());
        assert!(s
            .find_ctor_fields(Some("c"), "fld", 10)
            .await
            .unwrap()
            .is_empty());
        assert!(s
            .find_str_lits(Some("c"), "lit", 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn corpus_scoping_isolates() {
        let s = FactStore::open_in_memory().unwrap();
        s.replace_all(
            "A",
            &Facts {
                fn_defs: vec![fnd("shared", "a.rs", 1)],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        s.replace_all(
            "B",
            &Facts {
                fn_defs: vec![fnd("shared", "a.rs", 1)],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Rebuilding A leaves B intact.
        s.replace_all("A", &Facts::default()).await.unwrap();
        assert!(s
            .find_fn_defs(Some("A"), "shared", 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            s.find_fn_defs(Some("B"), "shared", 10).await.unwrap().len(),
            1
        );
        // Cross-corpus search still finds B's.
        assert_eq!(s.find_fn_defs(None, "shared", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn load_all_roundtrips() {
        let s = FactStore::open_in_memory().unwrap();
        let facts = Facts {
            fn_defs: vec![fnd("f", "a.rs", 3)],
            ctor_fields: vec![ctor("S", "fld", "val", "a.rs")],
            str_lits: vec![strl("hello", "a.rs")],
        };
        s.replace_all("c", &facts).await.unwrap();
        let got = s.load_all("c").await.unwrap();
        assert_eq!(got.fn_defs.len(), 1);
        assert_eq!(got.fn_defs[0].line, 3);
        assert_eq!(got.ctor_fields[0].value, "val");
        assert_eq!(got.str_lits[0].content, "hello");
    }

    #[tokio::test]
    async fn like_metachars_match_literally() {
        let s = FactStore::open_in_memory().unwrap();
        s.replace_all(
            "c",
            &Facts {
                str_lits: vec![strl("100%_done", "a.rs"), strl("nope", "b.rs")],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // The `%` and `_` must be treated literally, not as wildcards.
        assert_eq!(
            s.find_str_lits(Some("c"), "100%_done", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(s
            .find_str_lits(Some("c"), "100Xdone", 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn built_at_recorded() {
        let s = FactStore::open_in_memory().unwrap();
        assert!(s.built_at(&["c".into()]).await.unwrap().is_none());
        s.replace_all("c", &Facts::default()).await.unwrap();
        assert!(s.built_at(&["c".into()]).await.unwrap().is_some());
    }
}
