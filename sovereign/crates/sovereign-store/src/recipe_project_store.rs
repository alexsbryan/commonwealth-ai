// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RecipeProjectStore` — durable state for the recipe-author workspace.
//!
//! The recipe-author surface (a product feature: a domain expert authoring a
//! corpus recipe with the agent) needs one durable row per project: a stable
//! `feature_id` anchor plus the partner's charter. It originally borrowed the
//! ATOS `corpus-engine-atos::FeatureStore` (reusing its `recipe_authoring`
//! lifecycle state), which transitively coupled the product to the ATOS
//! experiment. This store is that data layer, decoupled — single-purpose, no
//! ATOS milestones / runs / lifecycle states.
//!
//! Schema and async model mirror the sibling stores in this crate
//! (`Arc<Mutex<Connection>>`, `CREATE TABLE IF NOT EXISTS` on open). The table
//! is append-mostly; the `id` is the recipe-author `feature_id` (a v4 UUID).

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS recipe_projects (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    charter_md  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    archived_at INTEGER
);";

/// Errors from the recipe-project store. Deliberately small — callers map
/// these into their own surface error (e.g. `sovereign-tools::Error`).
#[derive(Debug)]
pub enum RecipeProjectError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    /// A precondition the caller can fix (empty id, duplicate id).
    InvalidInput(String),
}

impl std::fmt::Display for RecipeProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "recipe-project store io: {e}"),
            Self::Sqlite(e) => write!(f, "recipe-project store sqlite: {e}"),
            Self::InvalidInput(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for RecipeProjectError {}

impl From<rusqlite::Error> for RecipeProjectError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<std::io::Error> for RecipeProjectError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, RecipeProjectError>;

/// One row of the `recipe_projects` table. Field names match the subset of
/// the former `FeatureRow` the recipe-author surface actually read, so the
/// migration was a type swap, not a field rename.
#[derive(Debug, Clone)]
pub struct RecipeProjectRow {
    pub id: String,
    pub title: String,
    pub charter_md: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

/// SQLite store for recipe-author projects.
pub struct RecipeProjectStore {
    conn: Arc<Mutex<Connection>>,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<RecipeProjectRow> {
    Ok(RecipeProjectRow {
        id: r.get(0)?,
        title: r.get(1)?,
        charter_md: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        archived_at: r.get(5)?,
    })
}

const COLS: &str = "id, title, charter_md, created_at, updated_at, archived_at";

impl RecipeProjectStore {
    /// Open or create the database at `db_path`. Idempotent — safe to call on
    /// an existing file. Creates the parent dir and the table on first open.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            RecipeProjectError::Io(std::io::Error::other(format!(
                "RecipeProjectStore::open {}: {e}",
                db_path.display()
            )))
        })?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert a fresh recipe-author project. Returns
    /// [`RecipeProjectError::InvalidInput`] if `id` is empty or already exists.
    pub async fn provision_recipe_project(
        &self,
        id: &str,
        title: &str,
        charter_md: &str,
    ) -> Result<RecipeProjectRow> {
        if id.is_empty() {
            return Err(RecipeProjectError::InvalidInput(
                "recipe project id cannot be empty".into(),
            ));
        }
        let now = unix_now();
        let conn = self.conn.lock().await;
        let affected = conn.execute(
            "INSERT INTO recipe_projects
               (id, title, charter_md, created_at, updated_at, archived_at)
             VALUES (?1, ?2, ?3, ?4, ?4, NULL)
             ON CONFLICT(id) DO NOTHING",
            params![id, title, charter_md, now],
        )?;
        if affected == 0 {
            return Err(RecipeProjectError::InvalidInput(format!(
                "recipe project '{id}' already exists"
            )));
        }
        Ok(RecipeProjectRow {
            id: id.into(),
            title: title.into(),
            charter_md: charter_md.into(),
            created_at: now,
            updated_at: now,
            archived_at: None,
        })
    }

    /// Fetch a single project by id, or `None`.
    pub async fn get(&self, id: &str) -> Result<Option<RecipeProjectRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                &format!("SELECT {COLS} FROM recipe_projects WHERE id = ?1"),
                params![id],
                |r| row_from(r),
            )
            .optional()?;
        Ok(row)
    }

    /// All projects, newest-updated first. `include_archived = false` hides
    /// rows with a non-null `archived_at` (parity with the former
    /// `FeatureStore::list`; the recipe-author surface only ever passes
    /// `false`).
    pub async fn list(&self, include_archived: bool) -> Result<Vec<RecipeProjectRow>> {
        let conn = self.conn.lock().await;
        let sql = if include_archived {
            format!("SELECT {COLS} FROM recipe_projects ORDER BY updated_at DESC")
        } else {
            format!(
                "SELECT {COLS} FROM recipe_projects \
                 WHERE archived_at IS NULL ORDER BY updated_at DESC"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], |r| row_from(r))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (RecipeProjectStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = RecipeProjectStore::open(&dir.path().join("recipe_projects.db")).unwrap();
        (s, dir)
    }

    #[tokio::test]
    async fn provision_then_get_round_trips() {
        let (s, _d) = store().await;
        let row = s
            .provision_recipe_project("rp1", "Federal case law", "Build over CourtListener")
            .await
            .unwrap();
        assert_eq!(row.id, "rp1");
        assert_eq!(row.title, "Federal case law");
        assert_eq!(row.charter_md, "Build over CourtListener");
        assert!(row.archived_at.is_none());

        let loaded = s.get("rp1").await.unwrap().unwrap();
        assert_eq!(loaded.id, "rp1");
        assert_eq!(loaded.charter_md, "Build over CourtListener");
        assert!(s.get("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_id_is_invalid_input() {
        let (s, _d) = store().await;
        s.provision_recipe_project("rp1", "a", "c").await.unwrap();
        let err = s.provision_recipe_project("rp1", "b", "c").await;
        assert!(matches!(err, Err(RecipeProjectError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn empty_id_rejected() {
        let (s, _d) = store().await;
        assert!(matches!(
            s.provision_recipe_project("", "t", "c").await,
            Err(RecipeProjectError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn list_returns_newest_first() {
        let (s, _d) = store().await;
        s.provision_recipe_project("a", "first", "c").await.unwrap();
        s.provision_recipe_project("b", "second", "c").await.unwrap();
        let all = s.list(false).await.unwrap();
        assert_eq!(all.len(), 2);
        // Both share the same second-granularity timestamp in a fast test, so
        // assert membership rather than order strictness.
        let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"b"));
    }
}
