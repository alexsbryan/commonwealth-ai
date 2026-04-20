//! Feature store — per-feature charters and milestones for the Agent
//! Task Orchestration System (ATOS).
//!
//! A **feature** is a unit of scoped work: a charter (operator-authored
//! brief), an in-SQLite `SOVEREIGN.md` (per-feature invariants), and a
//! stop-condition shell command. Features move through states —
//! `provisioned` → `active` → `completed` | `archived`.
//!
//! A **milestone** is one opencode/claude session's worth of work inside a
//! feature. Milestones record their brief, start/end timestamps, and the
//! compliance report assembled by `sovereign atos end-milestone`.
//!
//! This store is orthogonal to [`NoteStore`](crate::notes::NoteStore): both
//! can share the same SQLite file, but `FeatureStore` owns its tables via
//! `CREATE TABLE IF NOT EXISTS` on open. No cross-store FK; the link from
//! a feature-scoped note to its feature is via `notes.feature_id`.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Types ────────────────────────────────────────────────────────────────────

/// Lifecycle state of an ATOS feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureState {
    Provisioned,
    Active,
    Paused,
    Archived,
    Completed,
}

impl FeatureState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioned => "provisioned",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Archived => "archived",
            Self::Completed => "completed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "provisioned" => Some(Self::Provisioned),
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "archived" => Some(Self::Archived),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// One row of the `features` table.
#[derive(Debug, Clone)]
pub struct FeatureRow {
    pub id: String,
    pub title: String,
    pub charter_md: String,
    pub sovereign_md: String,
    pub state: String,
    pub stop_condition: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

/// One row of the `feature_milestones` table.
#[derive(Debug, Clone)]
pub struct MilestoneRow {
    pub id: String,
    pub feature_id: String,
    pub ordinal: i64,
    pub brief_md: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub compliance_report_json: Option<String>,
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite store for ATOS features and milestones.
pub struct FeatureStore {
    conn: Arc<Mutex<Connection>>,
}

impl FeatureStore {
    /// Open or create the database at `db_path`. Idempotent — safe to call
    /// on an existing file. Tables are created via `CREATE TABLE IF NOT
    /// EXISTS`; no explicit versioning needed while the schema is
    /// append-only.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "FeatureStore::open {}: {e}",
                db_path.display()
            )))
        })?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA).map_err(sqlite_err)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ── Feature writes ─────────────────────────────────────────────────────

    /// Insert a new feature in the `Provisioned` state.
    ///
    /// Returns [`Error::InvalidInput`] if `id` already exists.
    pub async fn provision(
        &self,
        id: &str,
        title: &str,
        charter_md: &str,
        sovereign_md: &str,
        stop_condition: &str,
    ) -> Result<FeatureRow> {
        if id.is_empty() {
            return Err(Error::InvalidInput("feature id cannot be empty".into()));
        }
        let now = unix_now();

        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "INSERT INTO features
                   (id, title, charter_md, sovereign_md, state, stop_condition,
                    created_at, updated_at, archived_at)
                 VALUES (?1, ?2, ?3, ?4, 'provisioned', ?5, ?6, ?6, NULL)
                 ON CONFLICT(id) DO NOTHING",
                params![id, title, charter_md, sovereign_md, stop_condition, now],
            )
            .map_err(sqlite_err)?;
        if affected == 0 {
            return Err(Error::InvalidInput(format!(
                "feature '{id}' already exists"
            )));
        }

        Ok(FeatureRow {
            id: id.into(),
            title: title.into(),
            charter_md: charter_md.into(),
            sovereign_md: sovereign_md.into(),
            state: "provisioned".into(),
            stop_condition: stop_condition.into(),
            created_at: now,
            updated_at: now,
            archived_at: None,
        })
    }

    /// Transition a feature to a new state. No-op if already in that state.
    pub async fn set_state(&self, id: &str, new_state: FeatureState) -> Result<bool> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        let archived_at = if new_state == FeatureState::Archived {
            Some(now)
        } else {
            None
        };
        let affected = conn
            .execute(
                "UPDATE features
                 SET state = ?1, updated_at = ?2,
                     archived_at = COALESCE(?3, archived_at)
                 WHERE id = ?4",
                params![new_state.as_str(), now, archived_at, id],
            )
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    /// Archive a feature (set state=archived and stamp archived_at).
    pub async fn archive(&self, id: &str, _reason: &str) -> Result<bool> {
        self.set_state(id, FeatureState::Archived).await
    }

    // ── Feature reads ──────────────────────────────────────────────────────

    pub async fn get(&self, id: &str) -> Result<Option<FeatureRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, title, charter_md, sovereign_md, state, stop_condition,
                        created_at, updated_at, archived_at
                 FROM features WHERE id = ?",
                params![id],
                map_feature_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(sqlite_err(other)),
            })?;
        Ok(row)
    }

    /// Return every feature, newest first. `include_archived = false`
    /// excludes rows whose `archived_at IS NOT NULL`.
    pub async fn list(&self, include_archived: bool) -> Result<Vec<FeatureRow>> {
        let sql = if include_archived {
            "SELECT id, title, charter_md, sovereign_md, state, stop_condition,
                    created_at, updated_at, archived_at
             FROM features
             ORDER BY created_at DESC"
        } else {
            "SELECT id, title, charter_md, sovereign_md, state, stop_condition,
                    created_at, updated_at, archived_at
             FROM features
             WHERE archived_at IS NULL
             ORDER BY created_at DESC"
        };
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(sql).map_err(sqlite_err)?;
        let mapped = stmt.query_map([], map_feature_row).map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    // ── Milestone writes ───────────────────────────────────────────────────

    /// Append a new milestone to a feature. `ordinal` is the 1-based index
    /// within the feature.
    pub async fn add_milestone(
        &self,
        feature_id: &str,
        ordinal: i64,
        brief_md: &str,
    ) -> Result<MilestoneRow> {
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.conn.lock().await;

        // Fail loudly if the parent feature is missing — keeps milestones
        // from orphaning on a typo'd --feature-id flag.
        let parent_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM features WHERE id = ?",
                params![feature_id],
                |r| r.get(0),
            )
            .map_err(sqlite_err)?;
        if parent_exists == 0 {
            return Err(Error::InvalidInput(format!(
                "add_milestone: feature '{feature_id}' not provisioned"
            )));
        }

        conn.execute(
            "INSERT INTO feature_milestones
                (id, feature_id, ordinal, brief_md, started_at, ended_at, compliance_report_json)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL)",
            params![id, feature_id, ordinal, brief_md],
        )
        .map_err(sqlite_err)?;

        Ok(MilestoneRow {
            id,
            feature_id: feature_id.into(),
            ordinal,
            brief_md: brief_md.into(),
            started_at: None,
            ended_at: None,
            compliance_report_json: None,
        })
    }

    /// Stamp `started_at = now` on the given milestone.
    pub async fn mark_started(&self, milestone_id: &str) -> Result<bool> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE feature_milestones SET started_at = ?1
                 WHERE id = ?2 AND started_at IS NULL",
                params![now, milestone_id],
            )
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    /// Stamp `ended_at = now` and store the compliance report JSON.
    pub async fn mark_ended(
        &self,
        milestone_id: &str,
        compliance_report_json: &str,
    ) -> Result<bool> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE feature_milestones SET ended_at = ?1, compliance_report_json = ?2
                 WHERE id = ?3",
                params![now, compliance_report_json, milestone_id],
            )
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    // ── Milestone reads ────────────────────────────────────────────────────

    pub async fn list_milestones(&self, feature_id: &str) -> Result<Vec<MilestoneRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, feature_id, ordinal, brief_md,
                        started_at, ended_at, compliance_report_json
                 FROM feature_milestones
                 WHERE feature_id = ?
                 ORDER BY ordinal ASC",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![feature_id], map_milestone_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    /// Return the highest ordinal for a feature, or 0 if it has no milestones.
    pub async fn next_ordinal(&self, feature_id: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        let max: Option<i64> = conn
            .query_row(
                "SELECT MAX(ordinal) FROM feature_milestones WHERE feature_id = ?",
                params![feature_id],
                |r| r.get(0),
            )
            .map_err(sqlite_err)?;
        Ok(max.unwrap_or(0) + 1)
    }
}

// ─── Schema ───────────────────────────────────────────────────────────────────

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS features (
    id             TEXT PRIMARY KEY,
    title          TEXT NOT NULL,
    charter_md     TEXT NOT NULL,
    sovereign_md   TEXT NOT NULL DEFAULT '',
    state          TEXT NOT NULL CHECK(state IN
                     ('provisioned','active','paused','archived','completed')),
    stop_condition TEXT NOT NULL DEFAULT '',
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    archived_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_features_state ON features(state);

CREATE TABLE IF NOT EXISTS feature_milestones (
    id                     TEXT PRIMARY KEY,
    feature_id             TEXT NOT NULL REFERENCES features(id) ON DELETE CASCADE,
    ordinal                INTEGER NOT NULL,
    brief_md               TEXT NOT NULL,
    started_at             INTEGER,
    ended_at               INTEGER,
    compliance_report_json TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_milestones_feat_ord
    ON feature_milestones(feature_id, ordinal);
";

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("FeatureStore sqlite: {e}")))
}

fn map_feature_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FeatureRow> {
    Ok(FeatureRow {
        id: row.get(0)?,
        title: row.get(1)?,
        charter_md: row.get(2)?,
        sovereign_md: row.get(3)?,
        state: row.get(4)?,
        stop_condition: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        archived_at: row.get(8)?,
    })
}

fn map_milestone_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MilestoneRow> {
    Ok(MilestoneRow {
        id: row.get(0)?,
        feature_id: row.get(1)?,
        ordinal: row.get(2)?,
        brief_md: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        compliance_report_json: row.get(6)?,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> FeatureStore {
        let dir = tempfile::tempdir().unwrap();
        let store = FeatureStore::open(&dir.path().join("features.db")).unwrap();
        // leak the tempdir so the file survives for the duration of the test
        std::mem::forget(dir);
        store
    }

    #[tokio::test]
    async fn provision_round_trip() {
        let store = make_store().await;
        let f = store
            .provision(
                "atos-version-flag",
                "Add --version flag to `sovereign atos`",
                "# charter",
                "# sovereign",
                "cargo run -- atos --version",
            )
            .await
            .unwrap();
        assert_eq!(f.id, "atos-version-flag");
        assert_eq!(f.state, "provisioned");

        let loaded = store.get("atos-version-flag").await.unwrap().unwrap();
        assert_eq!(loaded.title, "Add --version flag to `sovereign atos`");
        assert_eq!(loaded.charter_md, "# charter");
        assert_eq!(loaded.sovereign_md, "# sovereign");
        assert_eq!(loaded.stop_condition, "cargo run -- atos --version");
        assert!(loaded.archived_at.is_none());
    }

    #[tokio::test]
    async fn provision_duplicate_fails() {
        let store = make_store().await;
        store.provision("dup", "t", "c", "", "").await.unwrap();
        let err = store.provision("dup", "t2", "c2", "", "").await.unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn state_transitions_and_archive_stamps_timestamp() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "").await.unwrap();
        assert!(store.set_state("f1", FeatureState::Active).await.unwrap());

        let loaded = store.get("f1").await.unwrap().unwrap();
        assert_eq!(loaded.state, "active");
        assert!(loaded.archived_at.is_none());

        assert!(store.archive("f1", "done").await.unwrap());
        let archived = store.get("f1").await.unwrap().unwrap();
        assert_eq!(archived.state, "archived");
        assert!(archived.archived_at.is_some());
    }

    #[tokio::test]
    async fn list_excludes_archived_by_default() {
        let store = make_store().await;
        store.provision("a", "t", "c", "", "").await.unwrap();
        store.provision("b", "t", "c", "", "").await.unwrap();
        store.archive("b", "done").await.unwrap();

        let active = store.list(false).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "a");

        let all = store.list(true).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn milestone_lifecycle() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "stop-cmd").await.unwrap();

        let next = store.next_ordinal("f1").await.unwrap();
        assert_eq!(next, 1);

        let m = store.add_milestone("f1", next, "brief text").await.unwrap();
        assert_eq!(m.ordinal, 1);
        assert!(m.started_at.is_none());

        assert!(store.mark_started(&m.id).await.unwrap());
        assert!(store
            .mark_ended(&m.id, r#"{"summary":"passed"}"#)
            .await
            .unwrap());

        let rows = store.list_milestones("f1").await.unwrap();
        assert_eq!(rows.len(), 1);
        let final_row = &rows[0];
        assert!(final_row.started_at.is_some());
        assert!(final_row.ended_at.is_some());
        assert_eq!(
            final_row.compliance_report_json.as_deref(),
            Some(r#"{"summary":"passed"}"#)
        );

        let next2 = store.next_ordinal("f1").await.unwrap();
        assert_eq!(next2, 2);
    }

    #[tokio::test]
    async fn milestone_orphan_rejected() {
        let store = make_store().await;
        let err = store
            .add_milestone("does-not-exist", 1, "brief")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
