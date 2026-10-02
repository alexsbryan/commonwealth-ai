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

/// In-memory `AgentNotes` for logic tests — capturing, never SQL (fp-27;
/// same off-by-default rule as `middleware::fixtures`).
///
/// The real-SQL proofs of the note store live beside their owner
/// (`corpus-engine-notes/tests/real_sql_flows.rs`); this double carries only
/// the fixture for consumers' logic tests. Its contract mirrors the store's
/// where logic tests lean on it: `read_notes` is newest-first and hides
/// retired rows unless `include_retired` — the store half of that contract is
/// pinned in `real_sql_flows.rs`, so a divergence fails somewhere real.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use super::*;
    use crate::recipe::notes::{Note, NoteScope, NoteSource, RecipeNotes, ScopeFilter};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    pub struct RecordingNotes {
        rows: Mutex<Vec<Note>>,
        seq: AtomicU64,
    }

    impl RecordingNotes {
        fn next_id(&self) -> String {
            format!("rec-{}", self.seq.fetch_add(1, Ordering::SeqCst))
        }
    }

    #[async_trait]
    impl RecipeNotes for RecordingNotes {
        async fn write_note_full(
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
            payload_json: Option<&str>,
        ) -> Result<String> {
            let id = self.next_id();
            self.rows.lock().unwrap().push(Note {
                id: id.clone(),
                kind: kind.to_string(),
                content: content.to_string(),
                symbols,
                files,
                session_id: session_id.to_string(),
                created_at: "1970-01-01T00:00:00Z".to_string(),
                tool_name: None,
                retired_at: None,
                retired_by: None,
                scope: scope.as_str().to_string(),
                feature_id: feature_id.map(str::to_string),
                promoted_from: None,
                related_entity: related_entity.map(str::to_string),
                source: source.as_str().to_string(),
                supersedes: supersedes.map(str::to_string),
                payload_json: payload_json.map(str::to_string),
            });
            Ok(id)
        }

        async fn read_notes_scoped(
            &self,
            query: Option<&str>,
            symbols: &[String],
            files: &[String],
            kinds: &[String],
            limit: usize,
            include_retired: bool,
            scope_filter: &ScopeFilter,
        ) -> Result<Vec<Note>> {
            let rows = self.rows.lock().unwrap();
            let out: Vec<Note> = rows
                .iter()
                .rev()
                .filter(|n| include_retired || n.retired_at.is_none())
                .filter(|n| kinds.is_empty() || kinds.iter().any(|k| k == &n.kind))
                .filter(|n| query.is_none_or(|q| n.content.contains(q)))
                .filter(|n| symbols.iter().all(|s| n.symbols.contains(s)))
                .filter(|n| files.iter().all(|f| n.files.contains(f)))
                .filter(|n| {
                    scope_filter.scopes.is_empty()
                        || scope_filter.scopes.iter().any(|s| n.scope == s.as_str())
                })
                .filter(|n| {
                    scope_filter
                        .feature_id
                        .as_deref()
                        .is_none_or(|f| n.feature_id.as_deref() == Some(f))
                })
                .take(limit)
                .cloned()
                .collect();
            Ok(out)
        }
    }

    #[async_trait]
    impl AgentNotes for RecordingNotes {
        async fn read_notes(
            &self,
            query: Option<&str>,
            symbols: &[String],
            files: &[String],
            kinds: &[String],
            limit: usize,
            include_retired: bool,
        ) -> Result<Vec<Note>> {
            self.read_notes_scoped(
                query,
                symbols,
                files,
                kinds,
                limit,
                include_retired,
                &ScopeFilter::default(),
            )
            .await
        }

        async fn read_notes_by_related_entity(
            &self,
            related_entity: &str,
            kinds: &[&str],
        ) -> Result<Vec<Note>> {
            let rows = self.rows.lock().unwrap();
            Ok(rows
                .iter()
                .rev()
                .filter(|n| n.retired_at.is_none())
                .filter(|n| n.related_entity.as_deref() == Some(related_entity))
                .filter(|n| kinds.is_empty() || kinds.contains(&n.kind.as_str()))
                .cloned()
                .collect())
        }

        async fn has_active_note_with_content(
            &self,
            kind: &str,
            content: &str,
            source: NoteSource,
        ) -> Result<bool> {
            let rows = self.rows.lock().unwrap();
            Ok(rows.iter().any(|n| {
                n.retired_at.is_none()
                    && n.kind == kind
                    && n.content == content
                    && n.source == source.as_str()
            }))
        }

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
        ) -> Result<String> {
            self.write_note_full(
                kind,
                content,
                symbols,
                files,
                session_id,
                scope,
                feature_id,
                related_entity,
                source,
                supersedes,
                None,
            )
            .await
        }

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
        ) -> Result<String> {
            self.write_note_full(
                kind,
                content,
                symbols,
                files,
                session_id,
                scope,
                feature_id,
                related_entity,
                NoteSource::Agent,
                None,
                None,
            )
            .await
        }

        async fn update_note_payload(&self, id: &str, payload_json: &str) -> Result<bool> {
            let mut rows = self.rows.lock().unwrap();
            for row in rows.iter_mut() {
                if row.id == id {
                    row.payload_json = Some(payload_json.to_string());
                    return Ok(true);
                }
            }
            Ok(false)
        }

        async fn log_tool_call(
            &self,
            session_id: &str,
            tool_name: &str,
            outcome: &str,
        ) -> Result<()> {
            self.rows.lock().unwrap().push(Note {
                id: self.next_id(),
                kind: "tool_call".to_string(),
                content: tool_name.to_string(),
                symbols: vec![],
                files: vec![],
                session_id: session_id.to_string(),
                created_at: "1970-01-01T00:00:00Z".to_string(),
                tool_name: Some(tool_name.to_string()),
                retired_at: None,
                retired_by: None,
                scope: NoteScope::Session.as_str().to_string(),
                feature_id: None,
                promoted_from: None,
                related_entity: None,
                source: NoteSource::Observed.as_str().to_string(),
                supersedes: None,
                payload_json: Some(outcome.to_string()),
            });
            Ok(())
        }

        async fn tool_call_log_rows(
            &self,
            _since: i64,
            _limit: usize,
        ) -> Result<Vec<ToolCallLogRow>> {
            Ok(vec![])
        }
    }
}
