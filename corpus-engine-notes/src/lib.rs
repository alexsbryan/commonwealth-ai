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
//! - [`notes`] — `NoteStore`, the SQLite-backed working memory store.
//!   Tracks kind/scope/source provenance for every note, plus tool-call
//!   logs and re-rank fingerprints.
//! - [`project_docs`] — `ProjectDocsStore` indexing DESIGN.md / RFC
//!   markdown files for the project-status surface.
//! - `notes_sync` lives back in `corpus-engine` (the bridge between
//!   corpus-engine's `ExtractedDoc` and this crate's `NoteStore`).
//!   Avoids a cyclic workspace dep — see Cargo.toml comment.
//!
//! ## Public surface
//!
//! Re-exported at the crate root to match what was at
//! `corpus_engine::*` pre-carve-out:
//!
//! - [`NoteStore`], [`NoteRow`], [`NoteScope`], [`NoteSource`],
//!   [`ScopeFilter`], [`ToolCallLogRow`]
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

pub mod error;
pub mod notes;
mod notes_schema;
pub mod project_docs;

pub use error::{Error, Result};
pub use notes::{
    BackfillReport, EmbedFn, ExportedNoteEmbedding, ExportedNoteEntity, ExportedNoteRow,
    GlinerFn, IngestRemoteReport, NotePropagationEvent, NoteRow, NoteScope, NoteSource,
    NoteStore, PropagationSinkFn, ScopeFilter, ToolCallLogRow,
};
pub use project_docs::{find_markdown_files, DocResult, ProjectDocsStore};
