// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe-author note seam.
//!
//! The recipe-authoring tools persist their working memory (decisions, research
//! findings, capability requests, checkpoints) as feature-scoped agent notes.
//! They did this by calling `corpus_engine_notes::NoteStore` directly — a
//! runtime dependency the extractable authoring package cannot carry.
//!
//! [`RecipeNotes`] is the contract they depend on instead: the exact slice of
//! the note store they use (`write_note_full` + `read_notes_scoped`), plus the
//! small DTOs those methods speak. A monolith-side adapter implements it over
//! the real `NoteStore`; the package sees only this trait. The DTOs mirror the
//! store's shape so the adapter is a field-for-field pass-through and behavior
//! is bit-identical.

use async_trait::async_trait;

use crate::error::Result;

/// Scope dimension of a note. `Feature` notes belong to one ATOS feature;
/// `Global` are repo-wide; `Session` are ephemeral scratch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteScope {
    Global,
    Feature,
    Session,
}

impl NoteScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Feature => "feature",
            Self::Session => "session",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "global" => Some(Self::Global),
            "feature" => Some(Self::Feature),
            "session" => Some(Self::Session),
            _ => None,
        }
    }
}

/// Provenance dimension for notes. `Agent` is the highest-confidence source —
/// the agent explicitly wrote the note; the others record automated sources the
/// audit assembly ranks lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteSource {
    Agent,
    Committed,
    Extracted,
    Inferred,
    Observed,
}

impl NoteSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Committed => "committed",
            Self::Extracted => "extracted",
            Self::Inferred => "inferred",
            Self::Observed => "observed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "committed" => Some(Self::Committed),
            "extracted" => Some(Self::Extracted),
            "inferred" => Some(Self::Inferred),
            "observed" => Some(Self::Observed),
            _ => None,
        }
    }

    /// Audit-display priority. Higher number = higher priority.
    pub fn priority(self) -> u8 {
        match self {
            Self::Agent => 4,
            Self::Committed => 3,
            Self::Extracted => 2,
            Self::Inferred => 1,
            Self::Observed => 0,
        }
    }
}

/// A single note row, as read back from the store. Mirrors the store row so the
/// adapter copies field-for-field; `scope`/`source` are the string forms.
#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub symbols: Vec<String>,
    pub files: Vec<String>,
    pub session_id: String,
    /// RFC 3339 timestamp string.
    pub created_at: String,
    /// Primary tool this note concerns (reflections only; `None` otherwise).
    pub tool_name: Option<String>,
    /// Unix timestamp when this note was retired; `None` means active.
    pub retired_at: Option<i64>,
    /// Human-readable reason for retirement.
    pub retired_by: Option<String>,
    /// Scope dimension: `"global"` | `"feature"` | `"session"`.
    pub scope: String,
    /// ATOS feature id when `scope == "feature"`. `None` otherwise.
    pub feature_id: Option<String>,
    /// Origin note id when created by `promote_note`. `None` for native writes.
    pub promoted_from: Option<String>,
    /// Free-text entity name this note relates to. `None` when unanchored.
    pub related_entity: Option<String>,
    /// Provenance: `"agent"` | `"committed"` | `"extracted"` | `"inferred"` |
    /// `"observed"`.
    pub source: String,
    /// Note id this note reverses. `None` for first-time decisions.
    pub supersedes: Option<String>,
    /// Structured per-kind payload (JSON). `None` for kinds without one.
    pub payload_json: Option<String>,
}

/// Retrieval filter for scope/feature combinations. `default()` reads all notes
/// regardless of scope.
#[derive(Debug, Clone, Default)]
pub struct ScopeFilter {
    /// When non-empty, restrict results to rows whose scope is in this list.
    pub scopes: Vec<NoteScope>,
    /// When `Some`, apply `feature_id = ?` (only meaningful with `Feature`).
    pub feature_id: Option<String>,
}

/// The slice of the note store the recipe-authoring tools depend on. A
/// monolith-side adapter implements this over `corpus_engine_notes::NoteStore`.
#[async_trait]
pub trait RecipeNotes: Send + Sync {
    /// Persist a note; returns the new note id. Mirrors
    /// `NoteStore::write_note_full`.
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<String>;

    /// Read notes matching the given filters, newest first. Mirrors
    /// `NoteStore::read_notes_scoped`.
    #[allow(clippy::too_many_arguments)]
    async fn read_notes_scoped(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
        scope_filter: &ScopeFilter,
    ) -> Result<Vec<NoteRow>>;
}
