// SPDX-License-Identifier: AGPL-3.0-or-later
//! # corpus-engine-notes
//!
//! The NoteStore — agent working memory — plus project_docs (DESIGN.md
//! / RFC index) and notes_sync (alignment-corpus shard bridge).
//!
//! Carved out of `corpus-engine` (2026-05-23, step 3 of the
//! decomposition plan). 11 workspace crates depended on corpus-engine
//! solely for these types pre-carve-out; now they depend here
//! directly and skip the rest of corpus-engine's surface.
//!
//! Modules:
//! - [`note`] — `Note` and its value types (`NoteScope`, `NoteSource`,
//!   `ScopeFilter`). Plain data, no storage engine; this is what the
//!   knowledge layer publishes. Split out of `notes.rs` in
//!   noun-convergence rung 7, where `Note` was also renamed `Note`.
//! - [`notes`] — `NoteStore`, the SQLite-backed working memory store.
//!   Tracks kind/scope/source provenance for every note, plus tool-call
//!   logs and re-rank fingerprints.
//! - [`project_docs`] — `ProjectDocsStore` indexing DESIGN.md / RFC
//!   markdown files for the project-status surface.
//! - [`response_mine`] — the regex response miner (Phase 7.2); scans an
//!   assistant transcript for decision-shaped sentences.
//! - [`decision_extractor`] — the per-turn `Middleware` that mines the
//!   assistant response and persists a `source='extracted'` note on the
//!   next turn unless the user corrects it. Moved down from
//!   `sovereign-api::middleware` by domains `dm-decision-extractor-move`
//!   (the note store's question, not routing's).
//! - `notes_sync` lives back in `corpus-engine` (the bridge between
//!   corpus-engine's `ExtractedDoc` and this crate's `NoteStore`).
//!   Avoids a cyclic workspace dep — see Cargo.toml comment.
//!
//! ## Public surface
//!
//! Re-exported at the crate root to match what was at
//! `corpus_engine::*` pre-carve-out:
//!
//! - [`Note`], [`NoteScope`], [`NoteSource`], [`ScopeFilter`]
//! - [`NoteStore`], [`ToolCallLogRow`]
//! - [`ProjectDocsStore`], [`DocResult`], [`find_markdown_files`]
//!
//! ## Error type
//!
//! Narrow local `Error` (Io + Database + InvalidInput) mirroring the
//! atos carve-out. `From<corpus_engine_notes::Error> for
//! corpus_engine::Error` is intentionally NOT added — the three
//! corpus-engine-internal consumers
//! (`alignment_projector`, `extractors::alignment_workspace`,
//! `update::project_index_watcher`) `map_err` explicitly. Avoids
//! adding a `From` impl that creates a non-obvious flow.

pub mod decision_extractor;
pub mod error;
pub mod note;
pub mod notes;
mod notes_schema;
pub mod project_docs;
pub mod response_mine;

pub use error::{Error, Result};
pub use note::{is_ephemeral_kind, Note, NoteScope, NoteSource, ScopeFilter, EPHEMERAL_KINDS};
pub use notes::{
    BackfillReport, EmbedFn, ExportedNoteEmbedding, ExportedNoteEntity, ExportedNoteRow, GlinerFn,
    IngestRemoteReport, NodeAttribution, NodeRoster, NotePropagationEvent, NoteReadOutcome,
    NoteStore, PropagationSinkFn, RosterEntry, ToolCallLogRow, NOTES_APP_ID,
};
pub use project_docs::{find_markdown_files, DocResult, ProjectDocsStore};
