//! `SqliteAcquirer` — materializes a SQL query over a SQLite database
//! into a newline-delimited JSON file that the existing `Jsonl`
//! extractor can consume.
//!
//! ## Why JSONL
//!
//! The `Acquirer` trait in `corpus-engine` returns a filesystem path.
//! Extractors read from that path. Rather than extend the trait with
//! a streaming document iterator — which would require migrating every
//! existing acquirer — this implementation renders rows to a JSONL file
//! at `<download_dir>/rows.jsonl`. Downstream extraction (`Jsonl`) and
//! chunking (`Passthrough` for memories, `Paragraph` for conversations)
//! is unchanged.
//!
//! ## Params shape
//!
//! Deserialized from the `params` blob on `AcquirerConfig::Custom`:
//!
//! ```toml
//! [acquire]
//! type = "custom"
//! kind = "sqlite"
//! [acquire.params]
//! db_path         = "~/.sovereign/sovereign.db"
//! query           = "SELECT id, content, last_used AS version \
//!                    FROM memories WHERE deleted_at IS NULL"
//! content_column  = "content"
//! id_column       = "id"
//! version_column  = "version"
//! # group_column    = "conversation_id"   # optional
//! # group_separator = "\n\n"              # optional
//! ```
//!
//! Rows are emitted as one JSON object per line with the fields the
//! `Jsonl` extractor reads (`content`, plus an `id` that flows through
//! to `source_doc_id`). When `group_column` is set, rows sharing a
//! group key are concatenated in query order using `group_separator`
//! (defaults to `"\n\n"`) and emitted as a single document.
//!
//! ## Concurrency
//!
//! Opens the SQLite file in `SQLITE_OPEN_READ_ONLY` mode. WAL mode
//! (enabled by `sovereign-store`) allows this second reader to coexist
//! with the live writer without blocking either side.

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::engine::{CorpusEngine, CustomAcquirerFn};
use corpus_engine::error::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

/// Deserialized from `AcquirerConfig::Custom.params`. Public so
/// tests can construct instances directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqliteAcquirerParams {
    /// Absolute or `~`-prefixed path to the SQLite database file.
    pub db_path: String,
    /// The SELECT statement run against the database. Must project
    /// at minimum `content_column` and `id_column`. May project
    /// `version_column` and/or `group_column` when configured.
    pub query: String,
    /// Column on the SELECT projection holding the document text.
    pub content_column: String,
    /// Column on the SELECT projection holding a stable document id.
    /// This becomes `source_doc_id` in the LanceDB index, enabling
    /// deletion-by-disappearance via
    /// `CorpusIndex::delete_chunks_by_source_doc`.
    pub id_column: String,
    /// Optional column whose value is used as a delta-detection
    /// signal. When the version matches the previously-indexed
    /// version for a given id, the row is skipped on re-ingest.
    /// Analogous to `source_version` in `local_file`.
    #[serde(default)]
    pub version_column: Option<String>,
    /// Optional column to group rows into multi-row documents
    /// (e.g. `conversation_id` to assemble a conversation from its
    /// messages). When set, the acquirer concatenates per-group
    /// content using `group_separator`.
    #[serde(default)]
    pub group_column: Option<String>,
    /// Separator inserted between grouped rows. Defaults to two
    /// newlines (`"\n\n"`) if `group_column` is set.
    #[serde(default)]
    pub group_separator: Option<String>,
}

impl SqliteAcquirerParams {
    fn resolved_db_path(&self) -> PathBuf {
        if let Some(rest) = self.db_path.strip_prefix("~/") {
            if let Some(home) = dirs_home() {
                return home.join(rest);
            }
        }
        PathBuf::from(&self.db_path)
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Register the SQLite acquirer on `engine` under the `"sqlite"`
/// kind. Call once at Runtime startup before any ingest of a
/// `KnowledgeView` recipe. Idempotent: re-registering overwrites.
pub fn register(engine: &CorpusEngine) {
    let acquirer: CustomAcquirerFn = Arc::new(|params_blob, download_dir| {
        Box::pin(async move { acquire(params_blob, download_dir).await })
    });
    engine.register_acquirer("sqlite", acquirer);
}

async fn acquire(
    params_blob: serde_json::Value,
    download_dir: PathBuf,
) -> Result<PathBuf> {
    let params: SqliteAcquirerParams = serde_json::from_value(params_blob)
        .map_err(|e| Error::Recipe(format!("SqliteAcquirer params invalid: {e}")))?;

    // Blocking I/O — run on a dedicated thread so we don't starve
    // the async runtime on a large query. `spawn_blocking` is the
    // documented tokio idiom for rusqlite.
    tokio::task::spawn_blocking(move || write_jsonl_from_sqlite(&params, &download_dir))
        .await
        .map_err(|e| Error::Recipe(format!("SqliteAcquirer task panicked: {e}")))?
}

fn write_jsonl_from_sqlite(
    params: &SqliteAcquirerParams,
    download_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(download_dir).map_err(Error::Io)?;
    let db_path = params.resolved_db_path();
    if !db_path.exists() {
        return Err(Error::Recipe(format!(
            "SqliteAcquirer: db_path '{}' does not exist",
            db_path.display()
        )));
    }

    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Error::Recipe(format!("SqliteAcquirer: open failed: {e}")))?;

    let out_path = download_dir.join("rows.jsonl");
    let file = std::fs::File::create(&out_path).map_err(Error::Io)?;
    let mut writer = BufWriter::new(file);

    // Prepared-statement column metadata drives the row projection.
    let mut stmt = conn
        .prepare(&params.query)
        .map_err(|e| Error::Recipe(format!("SqliteAcquirer: prepare failed: {e}")))?;
    let column_names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    // Validate required columns project.
    require_column(&column_names, &params.content_column)?;
    require_column(&column_names, &params.id_column)?;
    if let Some(v) = &params.version_column {
        require_column(&column_names, v)?;
    }
    if let Some(g) = &params.group_column {
        require_column(&column_names, g)?;
    }

    let mut rows = stmt
        .query([])
        .map_err(|e| Error::Recipe(format!("SqliteAcquirer: query failed: {e}")))?;

    if let Some(group_col) = &params.group_column {
        let separator = params.group_separator.as_deref().unwrap_or("\n\n");
        // Preserve row-arrival order; BTreeMap would sort lexically.
        let mut groups: Vec<(String, GroupAccum)> = Vec::new();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();

        while let Some(row) = rows
            .next()
            .map_err(|e| Error::Recipe(format!("SqliteAcquirer: row failed: {e}")))?
        {
            let group_key = row_value_as_string(row, &column_names, group_col)
                .unwrap_or_else(|| "__null__".to_string());
            let content = row_value_as_string(row, &column_names, &params.content_column)
                .unwrap_or_default();
            let id = row_value_as_string(row, &column_names, &params.id_column)
                .unwrap_or_else(|| group_key.clone());
            let version = params
                .version_column
                .as_deref()
                .and_then(|col| row_value_as_string(row, &column_names, col));

            let idx = if let Some(&idx) = seen.get(&group_key) {
                idx
            } else {
                let idx = groups.len();
                seen.insert(group_key.clone(), idx);
                groups.push((group_key.clone(), GroupAccum::new(id, version)));
                idx
            };
            groups[idx].1.append(&content, separator);
        }

        for (group_key, accum) in groups {
            write_doc(
                &mut writer,
                &accum.id.unwrap_or(group_key),
                &accum.content,
                accum.version.as_deref(),
            )?;
        }
    } else {
        while let Some(row) = rows
            .next()
            .map_err(|e| Error::Recipe(format!("SqliteAcquirer: row failed: {e}")))?
        {
            let content = row_value_as_string(row, &column_names, &params.content_column)
                .unwrap_or_default();
            let id = row_value_as_string(row, &column_names, &params.id_column)
                .ok_or_else(|| {
                    Error::Recipe(format!(
                        "SqliteAcquirer: id column '{}' returned NULL for a row",
                        params.id_column
                    ))
                })?;
            let version = params
                .version_column
                .as_deref()
                .and_then(|col| row_value_as_string(row, &column_names, col));
            write_doc(&mut writer, &id, &content, version.as_deref())?;
        }
    }

    writer.flush().map_err(Error::Io)?;
    Ok(out_path)
}

fn require_column(columns: &[String], name: &str) -> Result<()> {
    if columns.iter().any(|c| c == name) {
        Ok(())
    } else {
        Err(Error::Recipe(format!(
            "SqliteAcquirer: query does not project required column '{name}'. \
             Available columns: {}",
            columns.join(", ")
        )))
    }
}

/// Render a rusqlite value as a plain string for JSONL emission.
/// Conservative: treats all non-text values via `Display` so integer
/// timestamps / floats round-trip as strings. Memories and
/// conversations only carry text content and text ids, so this is
/// adequate for v1.
fn row_value_as_string(
    row: &rusqlite::Row<'_>,
    columns: &[String],
    name: &str,
) -> Option<String> {
    let idx = columns.iter().position(|c| c == name)?;
    let value: rusqlite::types::Value = row.get(idx).ok()?;
    match value {
        rusqlite::types::Value::Null => None,
        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
        rusqlite::types::Value::Real(f) => Some(f.to_string()),
        rusqlite::types::Value::Text(s) => Some(s),
        rusqlite::types::Value::Blob(_) => None,
    }
}

fn write_doc(
    writer: &mut BufWriter<std::fs::File>,
    id: &str,
    content: &str,
    version: Option<&str>,
) -> Result<()> {
    // Shape matches what the `Jsonl` extractor reads: a `content`
    // field. We include `id` and optional `version` for delta
    // detection even if the extractor ignores them by default —
    // the fields are passed through as chunk metadata.
    let mut obj = serde_json::Map::new();
    obj.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    obj.insert(
        "content".to_string(),
        serde_json::Value::String(content.to_string()),
    );
    if let Some(v) = version {
        obj.insert(
            "version".to_string(),
            serde_json::Value::String(v.to_string()),
        );
    }
    let line = serde_json::to_string(&serde_json::Value::Object(obj))
        .map_err(|e| Error::Recipe(format!("JSONL serialize failed: {e}")))?;
    writeln!(writer, "{line}").map_err(Error::Io)?;
    Ok(())
}

struct GroupAccum {
    id: Option<String>,
    content: String,
    version: Option<String>,
}

impl GroupAccum {
    fn new(id: String, version: Option<String>) -> Self {
        Self {
            id: Some(id),
            content: String::new(),
            version,
        }
    }

    fn append(&mut self, chunk: &str, separator: &str) {
        if !self.content.is_empty() {
            self.content.push_str(separator);
        }
        self.content.push_str(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn seed_memories(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                last_used INTEGER NOT NULL,
                deleted_at INTEGER
            );
            INSERT INTO memories VALUES ('m1', 'first memory', 100, NULL);
            INSERT INTO memories VALUES ('m2', 'second memory', 200, NULL);
            INSERT INTO memories VALUES ('m3', 'deleted memory', 150, 1);
        ",
        )
        .unwrap();
    }

    #[test]
    fn materializes_memories_to_jsonl() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("state.db");
        seed_memories(&db);

        let params = SqliteAcquirerParams {
            db_path: db.display().to_string(),
            query: "SELECT id, content, last_used AS version \
                    FROM memories WHERE deleted_at IS NULL ORDER BY id"
                .into(),
            content_column: "content".into(),
            id_column: "id".into(),
            version_column: Some("version".into()),
            group_column: None,
            group_separator: None,
        };

        let out = write_jsonl_from_sqlite(&params, tmp.path()).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "deleted row excluded, two rows expected");
        assert!(lines[0].contains(r#""id":"m1""#));
        assert!(lines[0].contains(r#""content":"first memory""#));
        assert!(lines[0].contains(r#""version":"100""#));
        assert!(lines[1].contains(r#""id":"m2""#));
    }

    fn seed_conversations(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                updated_at INTEGER NOT NULL,
                skill_id TEXT
            );
            CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            INSERT INTO conversations VALUES ('c1', 500, 'research-analyst');
            INSERT INTO conversations VALUES ('c2', 600, 'inner-work');
            INSERT INTO messages VALUES ('m1','c1','user','hello there', 1);
            INSERT INTO messages VALUES ('m2','c1','assistant','hi',          2);
            INSERT INTO messages VALUES ('m3','c1','user','another',       3);
            INSERT INTO messages VALUES ('m4','c2','user','private',       4);
        ",
        )
        .unwrap();
    }

    #[test]
    fn group_column_assembles_one_doc_per_conversation() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("state.db");
        seed_conversations(&db);

        let params = SqliteAcquirerParams {
            db_path: db.display().to_string(),
            query: "SELECT c.id AS conversation_id, \
                           m.content AS content, \
                           c.updated_at AS version \
                    FROM conversations c \
                    JOIN messages m ON m.conversation_id = c.id \
                    WHERE c.skill_id IS NOT 'inner-work' \
                    ORDER BY c.id, m.created_at"
                .into(),
            content_column: "content".into(),
            id_column: "conversation_id".into(),
            version_column: Some("version".into()),
            group_column: Some("conversation_id".into()),
            group_separator: Some("\n\n".into()),
        };

        let out = write_jsonl_from_sqlite(&params, tmp.path()).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 1, "inner-work filtered; one conversation remains");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["id"], "c1");
        let content = parsed["content"].as_str().unwrap();
        assert!(content.contains("hello there"));
        assert!(content.contains("\n\n"));
        assert!(content.contains("another"));
        assert!(!content.contains("private"));
    }

    #[test]
    fn missing_db_returns_recipe_error() {
        let tmp = TempDir::new().unwrap();
        let params = SqliteAcquirerParams {
            db_path: tmp.path().join("nope.db").display().to_string(),
            query: "SELECT 1".into(),
            content_column: "content".into(),
            id_column: "id".into(),
            version_column: None,
            group_column: None,
            group_separator: None,
        };
        let err = write_jsonl_from_sqlite(&params, tmp.path()).unwrap_err();
        match err {
            Error::Recipe(m) => assert!(m.contains("does not exist")),
            other => panic!("expected Recipe error, got {other:?}"),
        }
    }

    #[test]
    fn empty_result_set_produces_empty_jsonl() {
        // Covers the plan's §11 "Handles empty result set without
        // error" invariant: the acquirer must produce a zero-byte
        // (or near-zero) JSONL file and return its path cleanly,
        // not blow up or skip creating the file (which would make
        // downstream `Jsonl` extraction error out with "file not
        // found").
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("state.db");
        // Create schema but insert no rows.
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                last_used INTEGER NOT NULL
            );",
        )
        .unwrap();

        let params = SqliteAcquirerParams {
            db_path: db.display().to_string(),
            query: "SELECT id, content, last_used AS version FROM memories".into(),
            content_column: "content".into(),
            id_column: "id".into(),
            version_column: Some("version".into()),
            group_column: None,
            group_separator: None,
        };

        let out = write_jsonl_from_sqlite(&params, tmp.path())
            .expect("empty result must not error");
        assert!(out.exists(), "JSONL file created even when empty");
        let body = std::fs::read_to_string(&out).unwrap();
        assert_eq!(body, "", "empty result produces empty file");
    }

    #[test]
    fn version_column_round_trips_to_jsonl() {
        // Delta detection hook: the per-row version value must
        // survive to the emitted JSONL so the indexer can pick it
        // up. Doesn't test re-ingest skipping (which needs a
        // two-pass integration test); just asserts the wiring.
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("state.db");
        seed_memories(&db);

        let params = SqliteAcquirerParams {
            db_path: db.display().to_string(),
            query: "SELECT id, content, last_used AS version FROM memories WHERE deleted_at IS NULL ORDER BY id".into(),
            content_column: "content".into(),
            id_column: "id".into(),
            version_column: Some("version".into()),
            group_column: None,
            group_separator: None,
        };

        let out = write_jsonl_from_sqlite(&params, tmp.path()).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        let first: serde_json::Value =
            serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(first["version"], "100", "version from last_used=100");
    }

    #[test]
    fn missing_required_column_rejected() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("state.db");
        seed_memories(&db);

        let params = SqliteAcquirerParams {
            db_path: db.display().to_string(),
            query: "SELECT id FROM memories".into(),
            content_column: "content".into(),
            id_column: "id".into(),
            version_column: None,
            group_column: None,
            group_separator: None,
        };
        let err = write_jsonl_from_sqlite(&params, tmp.path()).unwrap_err();
        match err {
            Error::Recipe(m) => assert!(m.contains("does not project required column 'content'")),
            other => panic!("expected Recipe error, got {other:?}"),
        }
    }
}

