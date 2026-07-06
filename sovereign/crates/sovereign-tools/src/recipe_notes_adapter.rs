// SPDX-License-Identifier: AGPL-3.0-or-later
//! Monolith-side adapter wiring the real [`NoteStore`] to the
//! [`RecipeNotes`](sovereign_contracts::recipe::notes::RecipeNotes) contract the
//! recipe-authoring tools depend on.
//!
//! The recipe-author bundle names only the trait, so it carries no
//! `corpus-engine-notes` dependency. This adapter — which does — converts the
//! contract DTOs to the store's own (identically-shaped) types field-for-field,
//! and folds the store's error into `Error::Storage`, exactly as the old inline
//! `ce_notes_err` bridge did. Behavior is bit-identical.
//!
//! The trait and the store type are both external to this crate, so the impl
//! lives on a local newtype ([`NoteStoreRecipeNotes`]) to satisfy the orphan
//! rule; construction sites wrap their `Arc<NoteStore>` in it before injecting.

use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine_notes::{
    NoteRow as CeNoteRow, NoteScope as CeNoteScope, NoteSource as CeNoteSource, NoteStore,
    ScopeFilter as CeScopeFilter,
};
use sovereign_contracts::recipe::notes::{
    NoteRow, NoteScope, NoteSource, RecipeNotes, ScopeFilter,
};
use sovereign_core::error::{Error, Result};

/// Wraps an `Arc<NoteStore>` so the foreign [`RecipeNotes`] trait can be
/// implemented for it (orphan rule).
pub struct NoteStoreRecipeNotes(pub Arc<NoteStore>);

impl NoteStoreRecipeNotes {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self(store)
    }
}

fn to_ce_scope(s: NoteScope) -> CeNoteScope {
    match s {
        NoteScope::Global => CeNoteScope::Global,
        NoteScope::Feature => CeNoteScope::Feature,
        NoteScope::Session => CeNoteScope::Session,
    }
}

fn to_ce_source(s: NoteSource) -> CeNoteSource {
    match s {
        NoteSource::Agent => CeNoteSource::Agent,
        NoteSource::Committed => CeNoteSource::Committed,
        NoteSource::Extracted => CeNoteSource::Extracted,
        NoteSource::Inferred => CeNoteSource::Inferred,
        NoteSource::Observed => CeNoteSource::Observed,
    }
}

fn to_ce_scope_filter(f: &ScopeFilter) -> CeScopeFilter {
    CeScopeFilter {
        scopes: f.scopes.iter().copied().map(to_ce_scope).collect(),
        feature_id: f.feature_id.clone(),
    }
}

fn from_ce_row(r: CeNoteRow) -> NoteRow {
    NoteRow {
        id: r.id,
        kind: r.kind,
        content: r.content,
        symbols: r.symbols,
        files: r.files,
        session_id: r.session_id,
        created_at: r.created_at,
        tool_name: r.tool_name,
        retired_at: r.retired_at,
        retired_by: r.retired_by,
        scope: r.scope,
        feature_id: r.feature_id,
        promoted_from: r.promoted_from,
        related_entity: r.related_entity,
        source: r.source,
        supersedes: r.supersedes,
        payload_json: r.payload_json,
    }
}

/// Same mapping the recipe-author code used inline: every note-store failure
/// surfaces as `Error::Storage` (the closest matching variant).
fn notes_err(e: corpus_engine_notes::Error) -> Error {
    Error::Storage(e.to_string())
}

#[async_trait]
impl RecipeNotes for NoteStoreRecipeNotes {
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
        self.0
            .write_note_full(
                kind,
                content,
                symbols,
                files,
                session_id,
                to_ce_scope(scope),
                feature_id,
                related_entity,
                to_ce_source(source),
                supersedes,
                payload_json,
            )
            .await
            .map_err(notes_err)
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
    ) -> Result<Vec<NoteRow>> {
        let ce_filter = to_ce_scope_filter(scope_filter);
        let rows = self
            .0
            .read_notes_scoped(
                query,
                symbols,
                files,
                kinds,
                limit,
                include_retired,
                &ce_filter,
            )
            .await
            .map_err(notes_err)?;
        Ok(rows.into_iter().map(from_ce_row).collect())
    }
}
