//! Plan-item store — SQLite-backed index of the implementation-plan
//! entries for a single project.
//!
//! ## Role
//!
//! `sovereign project plan` composes `IMPLEMENTATION_PLAN.md` at repo
//! root (markdown for humans) AND inserts one row per phase checklist
//! here (SQL for queries: `state=open AND phase=current`,
//! `depends_on`, bulk state updates when a phase passes). The two
//! representations are redundant by design — markdown is the user's
//! authored artifact, SQL is the derived index.
//!
//! ## Why a separate DB from `notes.db`
//!
//! Plan items have a different query shape than working notes. Notes
//! are append-only, scoped by feature / session, retrievable by
//! (symbols, files, kind); plan items are stateful (state
//! transitions from `open` → `in-progress` → `done`), graph-shaped
//! (`depends_on` → topological order), and regeneratable (a new
//! design_hash supersedes old items). Stuffing them into `notes`
//! with JSON-in-symbols defeats every query we'd want. See the
//! plan's step 6b trade-off note.
//!
//! Phase-pass events still write a normal `decision`-kind note into
//! `notes.db` pointing back at the plan_items closed by the phase,
//! so the audit trail spans both stores.
//!
//! ## Schema stability
//!
//! The `id` column is a stable slug like `"plan.phase-1.ingest"` so
//! external references survive regeneration. The `design_hash`
//! column records which DESIGN.md revision spawned the row; older
//! rows are retired on regeneration rather than deleted so stale
//! references from notes aren't orphaned.

use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Public types ──────────────────────────────────────────────────

/// The four lifecycle states a plan item can occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanItemState {
    Open,
    InProgress,
    Done,
    Deferred,
}

impl PlanItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in-progress",
            Self::Done => "done",
            Self::Deferred => "deferred",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "in-progress" => Self::InProgress,
            "done" => Self::Done,
            "deferred" => Self::Deferred,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    /// Stable id (e.g. `"plan.phase-1.ingest-service"`). Survives
    /// regeneration — callers use it as a long-lived reference.
    pub id: String,
    pub phase: u32,
    pub title: String,
    pub body: String,
    /// DESIGN.md section this phase realizes (e.g.
    /// `"DESIGN.md §Data & interfaces"`).
    pub realizes: Option<String>,
    /// JSON-encoded array of plan_item ids this depends on.
    pub depends_on: Vec<String>,
    /// Free-form stop-condition text (usually a shell command, but
    /// some phases take manual-review stops — the string is
    /// rendered verbatim into IMPLEMENTATION_PLAN.md).
    pub stop_hint: Option<String>,
    pub state: PlanItemState,
    /// `sha256(DESIGN.md)` when this row was written. Lets callers
    /// distinguish fresh rows from stale ones on regeneration.
    pub design_hash: String,
    pub created_at: i64,
    pub updated_at: i64,
}

// ─── Store ─────────────────────────────────────────────────────────

pub struct PlanStore {
    conn: Arc<Mutex<Connection>>,
}

impl PlanStore {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "PlanStore::open {}: {e}",
                db_path.display()
            )))
        })?;
        conn.execute_batch(SCHEMA).map_err(sqlite_err)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Upsert a plan item. Matches on `id`; replaces all non-id
    /// fields. `created_at` is preserved if the row already exists.
    pub async fn upsert(&self, item: &PlanItem) -> Result<()> {
        let deps_json = serde_json::to_string(&item.depends_on)
            .map_err(|e| Error::Io(std::io::Error::other(format!("plan_items deps: {e}"))))?;
        let conn = self.conn.lock().await;

        // Preserve the original created_at if the row already exists;
        // upsert otherwise.
        let existing_created: Option<i64> = conn
            .query_row(
                "SELECT created_at FROM plan_items WHERE id = ?",
                params![item.id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        let created_at = existing_created.unwrap_or(item.created_at);

        conn.execute(
            "INSERT INTO plan_items
                (id, phase, title, body, realizes, depends_on, stop_hint,
                 state, design_hash, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                phase       = excluded.phase,
                title       = excluded.title,
                body        = excluded.body,
                realizes    = excluded.realizes,
                depends_on  = excluded.depends_on,
                stop_hint   = excluded.stop_hint,
                state       = excluded.state,
                design_hash = excluded.design_hash,
                updated_at  = excluded.updated_at",
            params![
                item.id,
                item.phase,
                item.title,
                item.body,
                item.realizes,
                deps_json,
                item.stop_hint,
                item.state.as_str(),
                item.design_hash,
                created_at,
                item.updated_at,
            ],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Fetch all items, ordered by phase then creation time. The
    /// plan composer uses this when regenerating IMPLEMENTATION_PLAN.md
    /// so the markdown output stays stable across runs.
    pub async fn list_all(&self) -> Result<Vec<PlanItem>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, phase, title, body, realizes, depends_on, stop_hint,
                        state, design_hash, created_at, updated_at
                 FROM plan_items
                 ORDER BY phase ASC, created_at ASC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], row_to_item)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        Ok(rows)
    }

    /// Fetch items for a specific phase, in insertion order.
    pub async fn list_phase(&self, phase: u32) -> Result<Vec<PlanItem>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, phase, title, body, realizes, depends_on, stop_hint,
                        state, design_hash, created_at, updated_at
                 FROM plan_items
                 WHERE phase = ?
                 ORDER BY created_at ASC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![phase], row_to_item)
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        Ok(rows)
    }

    /// Mark rows done by id. Used by `project phase pass N` when a
    /// phase's stop condition succeeds.
    pub async fn set_state(&self, id: &str, state: PlanItemState, now: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE plan_items SET state = ?1, updated_at = ?2 WHERE id = ?3",
            params![state.as_str(), now, id],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Mark items older than `current_hash` as `deferred`. Called on
    /// regeneration so stale items aren't deleted (preserving any
    /// external references) but also don't appear in the fresh plan.
    pub async fn defer_stale(&self, current_hash: &str, now: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE plan_items
                 SET state = 'deferred', updated_at = ?1
                 WHERE design_hash <> ?2 AND state IN ('open', 'in-progress')",
                params![now, current_hash],
            )
            .map_err(sqlite_err)?;
        Ok(affected)
    }
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanItem> {
    let deps_json: String = row.get("depends_on")?;
    let depends_on: Vec<String> = serde_json::from_str(&deps_json).unwrap_or_default();
    let state_str: String = row.get("state")?;
    let state = PlanItemState::parse(&state_str).unwrap_or(PlanItemState::Open);
    Ok(PlanItem {
        id: row.get("id")?,
        phase: row.get::<_, i64>("phase")? as u32,
        title: row.get("title")?,
        body: row.get("body")?,
        realizes: row.get("realizes")?,
        depends_on,
        stop_hint: row.get("stop_hint")?,
        state,
        design_hash: row.get("design_hash")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("plan_items sqlite: {e}")))
}

// ─── Schema ────────────────────────────────────────────────────────

const SCHEMA: &str = include_str!("../assets/atos_plan_items_schema.sql");

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(id: &str, phase: u32, now: i64) -> PlanItem {
        PlanItem {
            id: id.to_string(),
            phase,
            title: format!("title-{id}"),
            body: format!("body-{id}"),
            realizes: Some(format!("DESIGN.md §section-{id}")),
            depends_on: vec![],
            stop_hint: Some("cargo test".to_string()),
            state: PlanItemState::Open,
            design_hash: "hash-abc".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn upsert_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("plan.db");
        let store = PlanStore::open(&db_path).unwrap();

        let item = make_item("plan.phase-0.skeleton", 0, 1000);
        store.upsert(&item).await.unwrap();

        let rows = store.list_all().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "plan.phase-0.skeleton");
        assert_eq!(rows[0].phase, 0);
        assert_eq!(rows[0].state, PlanItemState::Open);
        assert_eq!(rows[0].design_hash, "hash-abc");
    }

    #[tokio::test]
    async fn upsert_preserves_created_at_on_update() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PlanStore::open(&tmp.path().join("plan.db")).unwrap();

        let mut item = make_item("plan.phase-1.a", 1, 1000);
        store.upsert(&item).await.unwrap();

        // Re-upsert with a new updated_at; created_at should NOT
        // drift (it's a stable "when was this row first born" field).
        item.updated_at = 2000;
        item.created_at = 2000; // caller passes this but store should ignore
        item.title = "new title".into();
        store.upsert(&item).await.unwrap();

        let rows = store.list_all().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].created_at, 1000, "created_at must be immutable across upserts");
        assert_eq!(rows[0].updated_at, 2000);
        assert_eq!(rows[0].title, "new title");
    }

    #[tokio::test]
    async fn list_phase_filters_and_sorts() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PlanStore::open(&tmp.path().join("plan.db")).unwrap();
        store.upsert(&make_item("a", 0, 100)).await.unwrap();
        store.upsert(&make_item("b", 1, 200)).await.unwrap();
        store.upsert(&make_item("c", 1, 300)).await.unwrap();

        let phase_1 = store.list_phase(1).await.unwrap();
        assert_eq!(phase_1.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["b", "c"]);
    }

    #[tokio::test]
    async fn set_state_transitions_work() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PlanStore::open(&tmp.path().join("plan.db")).unwrap();
        store.upsert(&make_item("x", 0, 100)).await.unwrap();
        store.set_state("x", PlanItemState::Done, 500).await.unwrap();
        let rows = store.list_all().await.unwrap();
        assert_eq!(rows[0].state, PlanItemState::Done);
        assert_eq!(rows[0].updated_at, 500);
    }

    #[tokio::test]
    async fn defer_stale_leaves_done_items_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PlanStore::open(&tmp.path().join("plan.db")).unwrap();
        let mut old_open = make_item("old-open", 0, 100);
        let mut old_done = make_item("old-done", 0, 100);
        old_done.state = PlanItemState::Done;
        let new_open = make_item("new-open", 0, 100);
        // `new_open` has hash-abc (current), olds do too but we'll
        // rewrite them to simulate a prior generation.
        store.upsert(&old_open).await.unwrap();
        store.upsert(&old_done).await.unwrap();
        // Simulate a prior hash on the two old ones.
        old_open.design_hash = "hash-stale".into();
        store.upsert(&old_open).await.unwrap();
        old_done.design_hash = "hash-stale".into();
        store.upsert(&old_done).await.unwrap();
        store.upsert(&new_open).await.unwrap();

        let deferred = store.defer_stale("hash-abc", 999).await.unwrap();
        assert_eq!(deferred, 1, "only the stale open item should defer; done stays done");

        let rows = store.list_all().await.unwrap();
        let by_id: std::collections::HashMap<_, _> =
            rows.iter().map(|r| (r.id.as_str(), r)).collect();
        assert_eq!(by_id["old-open"].state, PlanItemState::Deferred);
        assert_eq!(by_id["old-done"].state, PlanItemState::Done, "done stays done");
        assert_eq!(by_id["new-open"].state, PlanItemState::Open);
    }

    #[tokio::test]
    async fn depends_on_round_trips_json() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PlanStore::open(&tmp.path().join("plan.db")).unwrap();
        let mut item = make_item("b", 1, 100);
        item.depends_on = vec!["a.1".into(), "a.2".into()];
        store.upsert(&item).await.unwrap();
        let rows = store.list_all().await.unwrap();
        assert_eq!(rows[0].depends_on, vec!["a.1", "a.2"]);
    }
}
