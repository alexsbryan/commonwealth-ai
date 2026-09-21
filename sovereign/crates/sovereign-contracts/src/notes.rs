// SPDX-License-Identifier: AGPL-3.0-or-later
//! The agent-notes port — the whole slice of the note store the programs
//! OUTSIDE `code/` read and write.
//!
//! `corpus-engine-notes` belongs to `svrn code` (`docs/FIVE_PROGRAMS.md` §2:
//! "owns the SCIP index and notes on disk"). Every other program reaching
//! `corpus_engine_notes::NoteStore` is one program naming another's store, and
//! §4 rule 1 (one data directory, one owner) is why promoting that crate to a
//! shared leaf is the wrong answer.
//!
//! [`RecipeNotes`](crate::recipe::notes::RecipeNotes) already ported the
//! recipe-authoring slice (`write_note_full` + `read_notes_scoped`) for the
//! extractable studio package. [`AgentNotes`] widens the same seam to the rest:
//! the turn path's dossier and lesson writes, the knowledge front door's note
//! evidence channel, and the tool-pattern reflector. It takes `RecipeNotes` as
//! a supertrait so there is ONE declaration of each method, not two.
//!
//! The DTOs are the ones `recipe::notes` already publishes — one definition,
//! reached from both traits. The single implementation over the real store
//! lives in the owning package (`corpus-engine-notes/src/port.rs`), so a
//! consumer takes `Arc<dyn AgentNotes>` from its host and an `Arc<NoteStore>`
//! coerces into it at the construction site.

use async_trait::async_trait;

use crate::error::Result;
use crate::recipe::notes::{Note, NoteScope, NoteSource, RecipeNotes};

/// One row of the store's tool-call log: what was called, by whom, and how it
/// came out. Mirrors the store row field-for-field.
#[derive(Debug, Clone)]
pub struct ToolCallLogRow {
    /// Log-row id (store primary key).
    pub id: String,
    /// Session that made the call.
    pub session_id: String,
    /// Tool id as registered.
    pub tool_name: String,
    /// `"success"` | `"error"` | `"empty_result"`.
    pub outcome: String,
    /// Unix timestamp of the call.
    pub called_at: i64,
}

/// The agent working-memory port. Implemented once, over the real store, by
/// the package that owns notes on disk.
#[async_trait]
pub trait AgentNotes: RecipeNotes {
    /// Scope-agnostic read, newest first. Mirrors `NoteStore::read_notes`.
    #[allow(clippy::too_many_arguments)]
    async fn read_notes(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
    ) -> Result<Vec<Note>>;

    /// Read active notes anchored to one free-text entity, restricted to
    /// `kinds`. Mirrors `NoteStore::read_notes_by_related_entity`.
    async fn read_notes_by_related_entity(
        &self,
        related_entity: &str,
        kinds: &[&str],
    ) -> Result<Vec<Note>>;

    /// Whether an active note of this kind already carries this exact content
    /// from this source. Mirrors `NoteStore::has_active_note_with_content`.
    async fn has_active_note_with_content(
        &self,
        kind: &str,
        content: &str,
        source: NoteSource,
    ) -> Result<bool>;

    /// Persist a note with provenance but no structured payload; returns the
    /// new id. Mirrors `NoteStore::write_note_with_source`.
    #[allow(clippy::too_many_arguments)]
    async fn write_note_with_source(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: NoteSource,
        supersedes: Option<&str>,
    ) -> Result<String>;

    /// Persist a note anchored to a related entity; returns the new id.
    /// Mirrors `NoteStore::write_note_with_relation`.
    #[allow(clippy::too_many_arguments)]
    async fn write_note_with_relation(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: NoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
    ) -> Result<String>;

    /// Replace one note's structured payload; `false` when no row matched.
    /// Mirrors `NoteStore::update_note_payload`.
    async fn update_note_payload(&self, id: &str, payload_json: &str) -> Result<bool>;

    /// Append one tool-call log row. Mirrors `NoteStore::log_tool_call`.
    async fn log_tool_call(&self, session_id: &str, tool_name: &str, outcome: &str) -> Result<()>;

    /// Read tool-call log rows called at or after `since`, newest first.
    /// Mirrors `NoteStore::tool_call_log_rows`.
    async fn tool_call_log_rows(&self, since: i64, limit: usize) -> Result<Vec<ToolCallLogRow>>;
}
