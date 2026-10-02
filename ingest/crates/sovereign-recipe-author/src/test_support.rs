// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test-only [`RecipeNotes`] implementation.
//!
//! The authoring-tool unit tests exercise the tools against a note store. The
//! real store (`corpus_engine_notes::NoteStore`) lives monolith-side and drags
//! corpus-engine — exactly the dependency this package exists to shed. So the
//! tests inject this in-memory stand-in instead.
//!
//! It is a *faithful-enough* stand-in: it stores rows in insertion order and
//! applies the same scope / feature / kind / query / retired filters that
//! `read_notes_scoped` documents, newest-first. That is all the tool tests
//! observe. Fidelity of the REAL adapter (`NoteStoreRecipeNotes` over the SQLite
//! store) is covered separately by the `recipe_author_loop` integration test in
//! `sovereign-tools`, which runs the tools against the actual store — so moving
//! the unit tests onto this stub loses no real-store coverage.

use std::sync::Mutex;

use async_trait::async_trait;

use sovereign_contracts::error::Result;
use sovereign_contracts::recipe::notes::{Note, NoteScope, NoteSource, RecipeNotes, ScopeFilter};

/// In-memory [`RecipeNotes`]: append-only rows behind a mutex.
#[derive(Default)]
pub(crate) struct InMemoryRecipeNotes {
    rows: Mutex<Vec<Note>>,
}

impl InMemoryRecipeNotes {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RecipeNotes for InMemoryRecipeNotes {
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
    ) -> Result<String> {
        let mut rows = self.rows.lock().unwrap();
        let seq = rows.len();
        let id = format!("note-{}", seq + 1);
        rows.push(Note {
            id: id.clone(),
            kind: kind.to_string(),
            content: content.to_string(),
            symbols,
            files,
            session_id: session_id.to_string(),
            // Zero-padded insertion index: a monotonic, clock-free `created_at`
            // that sorts newest-last so the reverse iteration below is stable.
            created_at: format!("{seq:020}"),
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
    ) -> Result<Vec<Note>> {
        let rows = self.rows.lock().unwrap();
        let scope_strs: Vec<&str> = scope_filter.scopes.iter().map(|s| s.as_str()).collect();
        let out: Vec<Note> = rows
            .iter()
            .rev() // newest first (rows are pushed oldest → newest)
            .filter(|r| include_retired || r.retired_at.is_none())
            .filter(|r| scope_strs.is_empty() || scope_strs.contains(&r.scope.as_str()))
            .filter(|r| match &scope_filter.feature_id {
                Some(fid) => r.feature_id.as_deref() == Some(fid.as_str()),
                None => true,
            })
            .filter(|r| kinds.is_empty() || kinds.contains(&r.kind))
            .filter(|r| query.is_none_or(|q| r.content.contains(q)))
            .filter(|r| symbols.is_empty() || symbols.iter().any(|s| r.symbols.contains(s)))
            .filter(|r| files.is_empty() || files.iter().any(|f| r.files.contains(f)))
            .take(limit)
            .cloned()
            .collect();
        Ok(out)
    }
}
