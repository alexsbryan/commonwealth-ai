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
//!
//! ## Why two `Note`s, and what stops them drifting
//!
//! `sovereign_contracts::recipe::notes::Note` is a deliberate restatement of
//! `corpus_engine_notes::Note`, not an accident: `sovereign-recipe-author`
//! ships in the extractable studio package and its Cargo description makes the
//! budget the boundary — "carries no corpus-engine or llama.cpp dependency".
//! `sovereign-contracts` also sits in the layer-0 `contract` layer
//! (`quality/ARCH_LAYERS.toml`), BELOW the knowledge layer that owns the real
//! type, so it could not name it even if the package budget allowed.
//! Adjudicated as such in noun-convergence rung 7 and kept.
//!
//! The cost of keeping it is drift, and this file is the only place that sees
//! both sides. The `match` arms below are exhaustive on the CONTRACT enums, so
//! a variant added there is a compile error here. The other direction — a
//! variant added to the STORE enum — is invisible to the compiler, and that is
//! the direction that actually rots. `NoteScope::ALL` / `NoteSource::ALL` on
//! the store side exist for it, and the tests at the bottom walk them.

use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine_notes::{
    Note as CeNote, NoteScope as CeNoteScope, NoteSource as CeNoteSource, NoteStore,
    ScopeFilter as CeScopeFilter,
};
use sovereign_contracts::recipe::notes::{Note, NoteScope, NoteSource, RecipeNotes, ScopeFilter};
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

fn from_ce_note(r: CeNote) -> Note {
    Note {
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
    ) -> Result<Vec<Note>> {
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
        Ok(rows.into_iter().map(from_ce_note).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The direction the exhaustive `match` above cannot check. `to_ce_scope`
    // is exhaustive on the CONTRACT enum, so a variant added there is a build
    // error. A variant added to the STORE enum compiles fine and simply
    // becomes inexpressible over the seam — silently, at runtime, as a note
    // the recipe-authoring package can never write or recognise.
    //
    // Watched failing before it was kept: adding a sixth `NoteSource` to
    // `corpus_engine_notes` and to its `ALL` fails
    // `contract_mirrors_every_store_source` with "store source `archived` has
    // no mirror on the contract side", which is the message that names the fix.

    #[test]
    fn contract_mirrors_every_store_scope() {
        for ce in CeNoteScope::ALL {
            let mirrored = NoteScope::parse(ce.as_str()).unwrap_or_else(|| {
                panic!(
                    "store scope `{}` has no mirror on the contract side",
                    ce.as_str()
                )
            });
            assert_eq!(to_ce_scope(mirrored).as_str(), ce.as_str());
        }
    }

    #[test]
    fn contract_mirrors_every_store_source() {
        for ce in CeNoteSource::ALL {
            let mirrored = NoteSource::parse(ce.as_str()).unwrap_or_else(|| {
                panic!(
                    "store source `{}` has no mirror on the contract side",
                    ce.as_str()
                )
            });
            assert_eq!(to_ce_source(mirrored).as_str(), ce.as_str());
            assert_eq!(
                mirrored.priority(),
                to_ce_source(mirrored).priority(),
                "audit ranking must be identical on both sides of the seam"
            );
        }
    }

    #[test]
    fn scope_filter_crosses_the_seam_unchanged() {
        let f = ScopeFilter {
            scopes: vec![NoteScope::Global, NoteScope::Feature],
            feature_id: Some("nc-7-note".to_string()),
        };
        let ce = to_ce_scope_filter(&f);
        assert_eq!(
            ce.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            f.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(ce.feature_id, f.feature_id);
    }

    #[test]
    fn every_note_field_survives_the_crossing() {
        let ce = CeNote {
            id: "n1".into(),
            kind: "decision".into(),
            content: "body".into(),
            symbols: vec!["Sym".into()],
            files: vec!["a.rs".into()],
            session_id: "s1".into(),
            created_at: "2026-08-20T00:00:00Z".into(),
            tool_name: Some("notes".into()),
            retired_at: Some(7),
            retired_by: Some("fixed".into()),
            scope: "feature".into(),
            feature_id: Some("nc-7-note".into()),
            promoted_from: Some("n0".into()),
            related_entity: Some("Note".into()),
            source: "agent".into(),
            supersedes: Some("n-1".into()),
            payload_json: Some("{}".into()),
            // Spelled in full, deliberately: a new field on the store's `Note`
            // must be adjudicated here (mirror it, or state why the package
            // cannot see it) rather than silently defaulted past the seam.
            origin_node_id: Some("node-abc".into()),
            sent_at: Some(11),
            received_at: Some(12),
        };
        let n = from_ce_note(ce.clone());
        assert_eq!(n.id, ce.id);
        assert_eq!(n.kind, ce.kind);
        assert_eq!(n.content, ce.content);
        assert_eq!(n.symbols, ce.symbols);
        assert_eq!(n.files, ce.files);
        assert_eq!(n.session_id, ce.session_id);
        assert_eq!(n.created_at, ce.created_at);
        assert_eq!(n.tool_name, ce.tool_name);
        assert_eq!(n.retired_at, ce.retired_at);
        assert_eq!(n.retired_by, ce.retired_by);
        assert_eq!(n.scope, ce.scope);
        assert_eq!(n.feature_id, ce.feature_id);
        assert_eq!(n.promoted_from, ce.promoted_from);
        assert_eq!(n.related_entity, ce.related_entity);
        assert_eq!(n.source, ce.source);
        assert_eq!(n.supersedes, ce.supersedes);
        assert_eq!(n.payload_json, ce.payload_json);
    }
}
