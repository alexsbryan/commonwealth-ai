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
    /// Charter-declared opt-in for automatic red-team after the final
    /// milestone passes. Defaults to `false`; the charter parser sets
    /// it via `**Red team:** auto` in the preamble, and the CLI lifts
    /// that into the DB via [`FeatureStore::set_auto_redteam`].
    pub auto_redteam: bool,
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

/// One row of the `atos_runs` table.
#[derive(Debug, Clone)]
pub struct AtosRunRow {
    pub id: String,
    pub feature_id: String,
    pub milestone_id: String,
    pub driver: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub exit_code: Option<i64>,
    pub stop_passed: Option<bool>,
    /// Run mode: `"normal"` (agent driver) or `"redteam"` (restricted
    /// tool set). Populated on every row from M3.2 forward; pre-M3.2
    /// rows default to `"normal"` via the column DEFAULT.
    pub mode: String,
    /// Captured stdout from the stop_condition run, bounded at 8KB by
    /// the orchestrator. Populated only for the `end-milestone` path.
    pub stop_stdout: Option<String>,
}

/// One row of the `atos_tool_events` table.
#[derive(Debug, Clone)]
pub struct AtosToolEvent {
    pub id: String,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub phase: String,
    pub args_json: Option<String>,
    pub outcome: Option<String>,
    pub duration_ms: Option<i64>,
    pub fired_at: i64,
}

/// Cap on events stored per run. Matches the `tool_call_log`
/// ring-buffer limit to keep `features.db` bounded under a runaway
/// driver. Enforced inside `record_tool_event`.
const ATOS_EVENTS_PER_RUN_LIMIT: i64 = 10_000;

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

        // Pre-schema: detect whether `atos_runs` already exists WITHOUT
        // the M3.2 columns. If so, ALTER them in before the main
        // `CREATE TABLE IF NOT EXISTS` becomes a no-op and hides the
        // old definition. ALTER TABLE ADD COLUMN on an existing row is
        // legitimate SQLite — unlike CHECK changes — so this stays a
        // plain additive migration.
        add_column_if_missing(
            &conn,
            "atos_runs",
            "mode",
            "TEXT NOT NULL DEFAULT 'normal' CHECK(mode IN ('normal','redteam'))",
        )?;
        add_column_if_missing(&conn, "atos_runs", "stop_stdout", "TEXT")?;
        conn.execute_batch(SCHEMA).map_err(sqlite_err)?;
        // M5.7: opt-in auto red-team flag on features. Additive
        // migration — pre-M5.7 features deserialize with `auto_redteam
        // = false`, matching the "absent means off" charter rule.
        add_column_if_missing(
            &conn,
            "features",
            "auto_redteam",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

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
            auto_redteam: false,
        })
    }

    /// Flip the `auto_redteam` opt-in flag for an existing feature.
    /// Called by the charter-driven provision path after `parse` has
    /// decided whether the preamble requested auto-redteam. Returns
    /// `true` when the row was updated, `false` when no feature with
    /// that id exists.
    pub async fn set_auto_redteam(&self, id: &str, enabled: bool) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE features SET auto_redteam = ?1, updated_at = ?2 WHERE id = ?3",
                params![enabled as i64, unix_now(), id],
            )
            .map_err(sqlite_err)?;
        Ok(affected > 0)
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
                        created_at, updated_at, archived_at, auto_redteam
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
                    created_at, updated_at, archived_at, auto_redteam
             FROM features
             ORDER BY created_at DESC"
        } else {
            "SELECT id, title, charter_md, sovereign_md, state, stop_condition,
                    created_at, updated_at, archived_at, auto_redteam
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

    // ── ATOS run ledger ────────────────────────────────────────────────────

    /// Open a new run row. The caller (usually `sovereign atos
    /// start-milestone`) hands the returned id to the driver subprocess
    /// via `$ATOS_RUN_ID` so every `record_tool_event` call can be
    /// attributed.
    ///
    /// Back-compat wrapper — defaults `mode` to `"normal"`. New
    /// callers use [`open_run_with_mode`].
    pub async fn open_run(
        &self,
        feature_id: &str,
        milestone_id: &str,
        driver: &str,
    ) -> Result<AtosRunRow> {
        self.open_run_with_mode(feature_id, milestone_id, driver, "normal")
            .await
    }

    /// Open a new run with an explicit mode. Use `"redteam"` for the
    /// restricted driver session on a milestone's invariants;
    /// `"normal"` otherwise. The mode is persisted on `atos_runs.mode`
    /// so reports can filter by it without round-tripping through
    /// external env state.
    pub async fn open_run_with_mode(
        &self,
        feature_id: &str,
        milestone_id: &str,
        driver: &str,
        mode: &str,
    ) -> Result<AtosRunRow> {
        if !matches!(mode, "normal" | "redteam") {
            return Err(Error::InvalidInput(format!(
                "open_run_with_mode: mode must be 'normal' or 'redteam', got '{mode}'"
            )));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO atos_runs
                (id, feature_id, milestone_id, driver, session_id, started_at,
                 ended_at, exit_code, stop_passed, mode, stop_stdout)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, NULL, NULL, NULL, ?6, NULL)",
            params![id, feature_id, milestone_id, driver, now, mode],
        )
        .map_err(sqlite_err)?;
        Ok(AtosRunRow {
            id,
            feature_id: feature_id.into(),
            milestone_id: milestone_id.into(),
            driver: driver.into(),
            session_id: None,
            started_at: now,
            ended_at: None,
            exit_code: None,
            stop_passed: None,
            mode: mode.into(),
            stop_stdout: None,
        })
    }

    /// Set the driver-reported session id on a run. The opencode plugin
    /// captures this from hook context on the first tool event and pings
    /// back via `record_atos_event` — the run row then carries enough
    /// info to join driver telemetry with `tool_call_log`.
    pub async fn set_run_session(&self, run_id: &str, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE atos_runs SET session_id = ?1
                 WHERE id = ?2 AND session_id IS NULL",
                params![session_id, run_id],
            )
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    /// Close out a run. `stop_passed` is what `atos end-milestone`
    /// computed from the feature's stop_condition; `exit_code` is the
    /// driver subprocess's exit status.
    ///
    /// Back-compat wrapper: does not persist `stop_stdout`. Use
    /// [`close_run_with_stdout`] from the M3.2+ end-milestone path.
    pub async fn close_run(
        &self,
        run_id: &str,
        exit_code: i64,
        stop_passed: bool,
    ) -> Result<bool> {
        self.close_run_with_stdout(run_id, exit_code, stop_passed, None)
            .await
    }

    /// Close out a run and persist the stop_condition stdout so the
    /// milestone-<n>.md renderer can quote it. Orchestrator caps
    /// `stop_stdout` at 8KB before calling this.
    pub async fn close_run_with_stdout(
        &self,
        run_id: &str,
        exit_code: i64,
        stop_passed: bool,
        stop_stdout: Option<&str>,
    ) -> Result<bool> {
        let now = unix_now();
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE atos_runs
                 SET ended_at = ?1, exit_code = ?2, stop_passed = ?3,
                     stop_stdout = ?4
                 WHERE id = ?5",
                params![now, exit_code, stop_passed as i64, stop_stdout, run_id],
            )
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    pub async fn get_run(&self, run_id: &str) -> Result<Option<AtosRunRow>> {
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT id, feature_id, milestone_id, driver, session_id,
                        started_at, ended_at, exit_code, stop_passed,
                        mode, stop_stdout
                 FROM atos_runs WHERE id = ?",
                params![run_id],
                map_run_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(sqlite_err(other)),
            })?;
        Ok(row)
    }

    pub async fn list_runs_for_feature(&self, feature_id: &str) -> Result<Vec<AtosRunRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, feature_id, milestone_id, driver, session_id,
                        started_at, ended_at, exit_code, stop_passed,
                        mode, stop_stdout
                 FROM atos_runs WHERE feature_id = ?
                 ORDER BY started_at ASC",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt.query_map(params![feature_id], map_run_row).map_err(sqlite_err)?;
        let mut out = Vec::new();
        for r in mapped {
            out.push(r.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    // ── Tool event stream ──────────────────────────────────────────────────

    /// Record one tool-execution event. Orphan events (run_id not in
    /// `atos_runs`) are rejected to keep the ledger honest — a plugin
    /// that starts firing before `open_run` is a bug we want to see,
    /// not silently swallow.
    ///
    /// Enforces the per-run 10k-event ring buffer in the same
    /// transaction as the insert, so a runaway driver can't bloat
    /// `features.db`.
    pub async fn record_tool_event(
        &self,
        run_id: &str,
        call_id: &str,
        tool_name: &str,
        phase: &str,
        args_json: Option<&str>,
        outcome: Option<&str>,
        duration_ms: Option<i64>,
    ) -> Result<String> {
        if !matches!(phase, "before" | "after" | "parse_error") {
            return Err(Error::InvalidInput(format!(
                "record_tool_event: phase must be before|after|parse_error, got '{phase}'"
            )));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let conn = self.conn.lock().await;

        let parent_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM atos_runs WHERE id = ?",
                params![run_id],
                |r| r.get(0),
            )
            .map_err(sqlite_err)?;
        if parent_exists == 0 {
            return Err(Error::InvalidInput(format!(
                "record_tool_event: run_id '{run_id}' not found"
            )));
        }

        conn.execute(
            "INSERT INTO atos_tool_events
                (id, run_id, call_id, tool_name, phase, args_json, outcome, duration_ms, fired_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, run_id, call_id, tool_name, phase, args_json, outcome, duration_ms, now],
        )
        .map_err(sqlite_err)?;

        // Ring-buffer trim per run — keep the 10k most recent events.
        conn.execute(
            "DELETE FROM atos_tool_events
             WHERE run_id = ?1 AND id IN (
                SELECT id FROM atos_tool_events
                WHERE run_id = ?1
                ORDER BY fired_at DESC
                LIMIT -1 OFFSET ?2
             )",
            params![run_id, ATOS_EVENTS_PER_RUN_LIMIT],
        )
        .map_err(sqlite_err)?;

        Ok(id)
    }

    pub async fn list_events_for_run(&self, run_id: &str) -> Result<Vec<AtosToolEvent>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, run_id, call_id, tool_name, phase,
                        args_json, outcome, duration_ms, fired_at
                 FROM atos_tool_events
                 WHERE run_id = ?
                 ORDER BY fired_at ASC",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![run_id], map_event_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for r in mapped {
            out.push(r.map_err(sqlite_err)?);
        }
        Ok(out)
    }
}

fn map_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AtosRunRow> {
    let stop_passed_int: Option<i64> = row.get(8)?;
    Ok(AtosRunRow {
        id: row.get(0)?,
        feature_id: row.get(1)?,
        milestone_id: row.get(2)?,
        driver: row.get(3)?,
        session_id: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        exit_code: row.get(7)?,
        stop_passed: stop_passed_int.map(|n| n != 0),
        mode: row.get(9)?,
        stop_stdout: row.get(10)?,
    })
}

/// Add a column to a table if the table exists and the column does
/// not. Used by [`FeatureStore::open`] to land additive schema
/// changes (M3.2: `atos_runs.mode`, `atos_runs.stop_stdout`) on
/// pre-existing databases without disturbing fresh ones.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    defn: &str,
) -> Result<()> {
    // Table absent → CREATE TABLE IF NOT EXISTS below will handle it.
    let table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = ?",
            params![table],
            |r| r.get(0),
        )
        .map_err(sqlite_err)?;
    if table_exists == 0 {
        return Ok(());
    }
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma).map_err(sqlite_err)?;
    let mut rows = stmt.query([]).map_err(sqlite_err)?;
    let mut seen = false;
    while let Some(r) = rows.next().map_err(sqlite_err)? {
        let name: String = r.get(1).map_err(sqlite_err)?;
        if name == column {
            seen = true;
            break;
        }
    }
    if !seen {
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {defn}");
        conn.execute(&sql, []).map_err(sqlite_err)?;
    }
    Ok(())
}

fn map_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AtosToolEvent> {
    Ok(AtosToolEvent {
        id: row.get(0)?,
        run_id: row.get(1)?,
        call_id: row.get(2)?,
        tool_name: row.get(3)?,
        phase: row.get(4)?,
        args_json: row.get(5)?,
        outcome: row.get(6)?,
        duration_ms: row.get(7)?,
        fired_at: row.get(8)?,
    })
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

-- ATOS run ledger. One row per driver invocation of a milestone. The
-- orchestrator creates this row on `start-milestone`, exports its id
-- as $ATOS_RUN_ID to the driver subprocess, and closes it out on
-- `end-milestone` with stop_condition exit + duration.
--
-- `mode` added in M3.2: 'normal' runs are agent drivers; 'redteam'
-- runs use a restricted tool surface + narrowed brief and land in the
-- report renderer's Red Team Findings section. The column has a
-- DEFAULT so V1/V2 rows created before M3.2 naturally become 'normal'.
--
-- `stop_stdout` added in M3.2: captures the shell output from
-- `end-milestone`'s stop_condition run so the milestone-<n>.md renderer
-- can quote test results inline without re-running anything.
-- Bounded at 8KB by the orchestrator before insert.
CREATE TABLE IF NOT EXISTS atos_runs (
    id            TEXT PRIMARY KEY,
    feature_id    TEXT NOT NULL,
    milestone_id  TEXT NOT NULL,
    driver        TEXT NOT NULL CHECK(driver IN ('claude','opencode')),
    session_id    TEXT,
    started_at    INTEGER NOT NULL,
    ended_at      INTEGER,
    exit_code     INTEGER,
    stop_passed   INTEGER,
    mode          TEXT NOT NULL DEFAULT 'normal'
                  CHECK(mode IN ('normal','redteam')),
    stop_stdout   TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_feature   ON atos_runs(feature_id);
CREATE INDEX IF NOT EXISTS idx_runs_milestone ON atos_runs(milestone_id);
CREATE INDEX IF NOT EXISTS idx_runs_mode      ON atos_runs(mode);

-- Per-tool event log. Populated by the opencode plugin via
-- `record_atos_event`, and (eventually) by a Claude-side wrapper that
-- mirrors MCP tool_call_log rows into this table. Ring-buffered at
-- 10k rows per run so a runaway loop can't bloat features.db.
CREATE TABLE IF NOT EXISTS atos_tool_events (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES atos_runs(id) ON DELETE CASCADE,
    call_id     TEXT NOT NULL,
    tool_name   TEXT NOT NULL,
    phase       TEXT NOT NULL CHECK(phase IN ('before','after','parse_error')),
    args_json   TEXT,
    outcome     TEXT,
    duration_ms INTEGER,
    fired_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_run  ON atos_tool_events(run_id);
CREATE INDEX IF NOT EXISTS idx_events_tool ON atos_tool_events(tool_name);
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
        auto_redteam: {
            let v: i64 = row.get(9)?;
            v != 0
        },
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

    // ── ATOS run ledger tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn run_lifecycle_and_event_attribution() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "true").await.unwrap();
        let m = store.add_milestone("f1", 1, "brief").await.unwrap();

        let run = store.open_run("f1", &m.id, "claude").await.unwrap();
        assert!(run.ended_at.is_none());
        assert_eq!(run.driver, "claude");

        // Session id filled in after the first event carries it.
        assert!(store.set_run_session(&run.id, "opencode-sess-abc").await.unwrap());
        let loaded = store.get_run(&run.id).await.unwrap().unwrap();
        assert_eq!(loaded.session_id.as_deref(), Some("opencode-sess-abc"));

        // Record a pair of events for one call_id (before + after).
        let e1 = store
            .record_tool_event(
                &run.id,
                "call_xyz",
                "read_notes",
                "before",
                Some(r#"{"query":"hi"}"#),
                None,
                None,
            )
            .await
            .unwrap();
        let e2 = store
            .record_tool_event(
                &run.id,
                "call_xyz",
                "read_notes",
                "after",
                None,
                Some("success"),
                Some(42),
            )
            .await
            .unwrap();
        assert_ne!(e1, e2);

        let events = store.list_events_for_run(&run.id).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].phase, "before");
        assert_eq!(events[1].phase, "after");
        assert_eq!(events[1].outcome.as_deref(), Some("success"));
        assert_eq!(events[1].duration_ms, Some(42));

        // Close the run.
        assert!(store.close_run(&run.id, 0, true).await.unwrap());
        let closed = store.get_run(&run.id).await.unwrap().unwrap();
        assert_eq!(closed.exit_code, Some(0));
        assert_eq!(closed.stop_passed, Some(true));
        assert!(closed.ended_at.is_some());

        // Listing by feature returns our run.
        let runs = store.list_runs_for_feature("f1").await.unwrap();
        assert_eq!(runs.len(), 1);
    }

    #[tokio::test]
    async fn orphan_events_rejected() {
        let store = make_store().await;
        let err = store
            .record_tool_event(
                "no-such-run",
                "call_1",
                "read_notes",
                "before",
                None,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn open_run_defaults_to_normal_mode() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "").await.unwrap();
        let m = store.add_milestone("f1", 1, "brief").await.unwrap();
        let run = store.open_run("f1", &m.id, "claude").await.unwrap();
        assert_eq!(run.mode, "normal");
        let loaded = store.get_run(&run.id).await.unwrap().unwrap();
        assert_eq!(loaded.mode, "normal");
    }

    #[tokio::test]
    async fn open_run_with_redteam_mode_persists() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "").await.unwrap();
        let m = store.add_milestone("f1", 1, "brief").await.unwrap();
        let run = store
            .open_run_with_mode("f1", &m.id, "claude", "redteam")
            .await
            .unwrap();
        let loaded = store.get_run(&run.id).await.unwrap().unwrap();
        assert_eq!(loaded.mode, "redteam");
    }

    #[tokio::test]
    async fn invalid_mode_rejected() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "").await.unwrap();
        let m = store.add_milestone("f1", 1, "brief").await.unwrap();
        let err = store
            .open_run_with_mode("f1", &m.id, "claude", "adversarial")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn close_run_with_stdout_persists() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "").await.unwrap();
        let m = store.add_milestone("f1", 1, "brief").await.unwrap();
        let run = store.open_run("f1", &m.id, "claude").await.unwrap();
        let stdout = "test result: ok. 17 passed; 0 failed";
        assert!(
            store
                .close_run_with_stdout(&run.id, 0, true, Some(stdout))
                .await
                .unwrap()
        );
        let loaded = store.get_run(&run.id).await.unwrap().unwrap();
        assert_eq!(loaded.stop_stdout.as_deref(), Some(stdout));
        assert_eq!(loaded.stop_passed, Some(true));
    }

    #[tokio::test]
    async fn m32_additive_columns_on_prior_db() {
        // Simulate a pre-M3.2 FeatureStore where atos_runs lacks the
        // new `mode` and `stop_stdout` columns. The M3.2 `open()` must
        // detect and ALTER them in rather than silently leaving the
        // schema behind.
        use rusqlite::Connection;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("features.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE features (
                     id TEXT PRIMARY KEY, title TEXT NOT NULL, charter_md TEXT NOT NULL,
                     sovereign_md TEXT NOT NULL DEFAULT '',
                     state TEXT NOT NULL CHECK(state IN ('provisioned','active','paused','archived','completed')),
                     stop_condition TEXT NOT NULL DEFAULT '',
                     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                     archived_at INTEGER);
                 CREATE TABLE feature_milestones (
                     id TEXT PRIMARY KEY, feature_id TEXT NOT NULL REFERENCES features(id) ON DELETE CASCADE,
                     ordinal INTEGER NOT NULL, brief_md TEXT NOT NULL,
                     started_at INTEGER, ended_at INTEGER, compliance_report_json TEXT);
                 CREATE TABLE atos_runs (
                     id TEXT PRIMARY KEY, feature_id TEXT NOT NULL, milestone_id TEXT NOT NULL,
                     driver TEXT NOT NULL CHECK(driver IN ('claude','opencode')),
                     session_id TEXT, started_at INTEGER NOT NULL,
                     ended_at INTEGER, exit_code INTEGER, stop_passed INTEGER);
                 INSERT INTO features VALUES ('prior','t','c','','provisioned','true',1000,1000,NULL);
                 INSERT INTO feature_milestones VALUES ('mm','prior',1,'brief',NULL,NULL,NULL);
                 INSERT INTO atos_runs (id, feature_id, milestone_id, driver, started_at)
                   VALUES ('r0','prior','mm','claude',1000);",
            )
            .unwrap();
        }

        // Reopen — the additive ALTERs should land without data loss.
        let store = FeatureStore::open(&path).unwrap();
        let loaded = store.get_run("r0").await.unwrap().unwrap();
        assert_eq!(loaded.mode, "normal", "pre-M3.2 row defaults to normal mode");
        assert!(loaded.stop_stdout.is_none(), "pre-M3.2 rows have NULL stdout");

        // Future runs pick up the new column properly.
        let fresh = store
            .open_run_with_mode("prior", "mm", "claude", "redteam")
            .await
            .unwrap();
        assert_eq!(fresh.mode, "redteam");
    }

    #[tokio::test]
    async fn auto_redteam_round_trip() {
        let store = make_store().await;
        let f = store.provision("f1", "t", "c", "", "").await.unwrap();
        assert!(!f.auto_redteam, "provision defaults to false");

        let loaded = store.get("f1").await.unwrap().unwrap();
        assert!(!loaded.auto_redteam);

        assert!(store.set_auto_redteam("f1", true).await.unwrap());
        let after = store.get("f1").await.unwrap().unwrap();
        assert!(after.auto_redteam, "flag persisted across reads");

        // Toggle back off.
        assert!(store.set_auto_redteam("f1", false).await.unwrap());
        let cleared = store.get("f1").await.unwrap().unwrap();
        assert!(!cleared.auto_redteam);

        // Unknown feature → no rows updated.
        assert!(!store.set_auto_redteam("ghost", true).await.unwrap());
    }

    #[tokio::test]
    async fn auto_redteam_column_is_additive_on_pre_m5_db() {
        // Build a features.db without the auto_redteam column, then
        // reopen via FeatureStore and confirm the migration lands
        // and existing rows default to false.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("features.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE features (
                     id TEXT PRIMARY KEY, title TEXT NOT NULL, charter_md TEXT NOT NULL,
                     sovereign_md TEXT NOT NULL, state TEXT NOT NULL,
                     stop_condition TEXT NOT NULL DEFAULT '',
                     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                     archived_at INTEGER);
                 CREATE TABLE feature_milestones (
                     id TEXT PRIMARY KEY, feature_id TEXT NOT NULL, ordinal INTEGER NOT NULL,
                     brief_md TEXT NOT NULL, started_at INTEGER, ended_at INTEGER,
                     compliance_report_json TEXT);
                 CREATE TABLE atos_runs (
                     id TEXT PRIMARY KEY, feature_id TEXT NOT NULL, milestone_id TEXT NOT NULL,
                     driver TEXT NOT NULL, session_id TEXT, started_at INTEGER NOT NULL,
                     ended_at INTEGER, exit_code INTEGER, stop_passed INTEGER);
                 INSERT INTO features VALUES ('legacy','t','c','','provisioned','true',1000,1000,NULL);",
            )
            .unwrap();
        }

        let store = FeatureStore::open(&path).unwrap();
        let loaded = store.get("legacy").await.unwrap().unwrap();
        assert!(!loaded.auto_redteam, "pre-M5 row defaults to false");
    }

    #[tokio::test]
    async fn invalid_phase_rejected() {
        let store = make_store().await;
        store.provision("f1", "t", "c", "", "").await.unwrap();
        let m = store.add_milestone("f1", 1, "brief").await.unwrap();
        let run = store.open_run("f1", &m.id, "claude").await.unwrap();

        let err = store
            .record_tool_event(&run.id, "c1", "t", "during", None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
