// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store's side of the agent-notes port.
//!
//! [`NoteStore`] implements [`RecipeNotes`] and [`AgentNotes`] here, in the
//! package that OWNS notes on disk (`docs/FIVE_PROGRAMS.md` §2, §4 rule 1). Every
//! other program takes `Arc<dyn AgentNotes>` from its host and never names this
//! crate; an `Arc<NoteStore>` coerces into that at the construction site.
//!
//! This is the ONE implementation. It used to be a newtype
//! (`NoteStoreRecipeNotes`, in `sovereign-tools`) in the `svrn`
//! package, which existed only for the orphan rule — foreign trait, foreign
//! type. From inside the owning crate the type is local, so the newtype is gone
//! and the conversions are the same field-for-field pass-through they were.
//!
//! ## Why two `Note`s, and what stops them drifting
//!
//! `sovereign_contracts::recipe::notes::Note` is a deliberate restatement of
//! [`Note`], not an accident. `sovereign-contracts` sits in the layer-0
//! `contract` layer (`quality/ARCH_LAYERS.toml`), BELOW the knowledge layer that
//! owns the real type, so it could not name it even if the extractable studio
//! package's budget allowed. Adjudicated in noun-convergence rung 7 and kept.
//!
//! The cost of keeping it is drift, and this file is the only place that sees
//! both sides. The `match` arms below are exhaustive on the CONTRACT enums, so a
//! variant added there is a compile error here. The other direction — a variant
//! added to the STORE enum — is invisible to the compiler, and that is the
//! direction that actually rots. [`NoteScope::ALL`] / [`NoteSource::ALL`] exist
//! for it, and the tests at the bottom walk them.

use async_trait::async_trait;

use sovereign_contracts::error::{Error as PortError, Result as PortResult};
use sovereign_contracts::notes::{AgentNotes, ToolCallLogRow as PortToolCallLogRow};
use sovereign_contracts::recipe::notes::{
    Note as PortNote, NoteScope as PortNoteScope, NoteSource as PortNoteSource, RecipeNotes,
    ScopeFilter as PortScopeFilter,
};

use crate::note::{Note, NoteScope, NoteSource, ScopeFilter};
use crate::notes::{NoteStore, ToolCallLogRow};

fn to_scope(s: PortNoteScope) -> NoteScope {
    match s {
        PortNoteScope::Global => NoteScope::Global,
        PortNoteScope::Feature => NoteScope::Feature,
        PortNoteScope::Session => NoteScope::Session,
    }
}

fn to_source(s: PortNoteSource) -> NoteSource {
    match s {
        PortNoteSource::Agent => NoteSource::Agent,
        PortNoteSource::Committed => NoteSource::Committed,
        PortNoteSource::Extracted => NoteSource::Extracted,
        PortNoteSource::Inferred => NoteSource::Inferred,
        PortNoteSource::Observed => NoteSource::Observed,
    }
}

fn to_scope_filter(f: &PortScopeFilter) -> ScopeFilter {
    ScopeFilter {
        scopes: f.scopes.iter().copied().map(to_scope).collect(),
        feature_id: f.feature_id.clone(),
    }
}

fn to_port_note(r: Note) -> PortNote {
    PortNote {
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

fn to_port_log_row(r: ToolCallLogRow) -> PortToolCallLogRow {
    PortToolCallLogRow {
        id: r.id,
        session_id: r.session_id,
        tool_name: r.tool_name,
        outcome: r.outcome,
        called_at: r.called_at,
    }
}

/// Every store failure surfaces as `Error::Storage` — the mapping the inline
/// recipe-author bridge used, unchanged.
fn port_err(e: crate::Error) -> PortError {
    PortError::Storage(e.to_string())
}

#[async_trait]
impl RecipeNotes for NoteStore {
    async fn write_note_full(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: PortNoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: PortNoteSource,
        supersedes: Option<&str>,
        payload_json: Option<&str>,
    ) -> PortResult<String> {
        NoteStore::write_note_full(
            self,
            kind,
            content,
            symbols,
            files,
            session_id,
            to_scope(scope),
            feature_id,
            related_entity,
            to_source(source),
            supersedes,
            payload_json,
        )
        .await
        .map_err(port_err)
    }

    async fn read_notes_scoped(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
        scope_filter: &PortScopeFilter,
    ) -> PortResult<Vec<PortNote>> {
        let filter = to_scope_filter(scope_filter);
        let rows = NoteStore::read_notes_scoped(
            self,
            query,
            symbols,
            files,
            kinds,
            limit,
            include_retired,
            &filter,
        )
        .await
        .map_err(port_err)?;
        Ok(rows.into_iter().map(to_port_note).collect())
    }
}

#[async_trait]
impl AgentNotes for NoteStore {
    async fn read_notes(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
        include_retired: bool,
    ) -> PortResult<Vec<PortNote>> {
        let rows =
            NoteStore::read_notes(self, query, symbols, files, kinds, limit, include_retired)
                .await
                .map_err(port_err)?;
        Ok(rows.into_iter().map(to_port_note).collect())
    }

    async fn read_notes_by_related_entity(
        &self,
        related_entity: &str,
        kinds: &[&str],
    ) -> PortResult<Vec<PortNote>> {
        let rows = NoteStore::read_notes_by_related_entity(self, related_entity, kinds)
            .await
            .map_err(port_err)?;
        Ok(rows.into_iter().map(to_port_note).collect())
    }

    async fn has_active_note_with_content(
        &self,
        kind: &str,
        content: &str,
        source: PortNoteSource,
    ) -> PortResult<bool> {
        NoteStore::has_active_note_with_content(self, kind, content, to_source(source))
            .await
            .map_err(port_err)
    }

    async fn write_note_with_source(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: PortNoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
        source: PortNoteSource,
        supersedes: Option<&str>,
    ) -> PortResult<String> {
        NoteStore::write_note_with_source(
            self,
            kind,
            content,
            symbols,
            files,
            session_id,
            to_scope(scope),
            feature_id,
            related_entity,
            to_source(source),
            supersedes,
        )
        .await
        .map_err(port_err)
    }

    async fn write_note_with_relation(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
        scope: PortNoteScope,
        feature_id: Option<&str>,
        related_entity: Option<&str>,
    ) -> PortResult<String> {
        NoteStore::write_note_with_relation(
            self,
            kind,
            content,
            symbols,
            files,
            session_id,
            to_scope(scope),
            feature_id,
            related_entity,
        )
        .await
        .map_err(port_err)
    }

    async fn update_note_payload(&self, id: &str, payload_json: &str) -> PortResult<bool> {
        NoteStore::update_note_payload(self, id, payload_json)
            .await
            .map_err(port_err)
    }

    async fn log_tool_call(
        &self,
        session_id: &str,
        tool_name: &str,
        outcome: &str,
    ) -> PortResult<()> {
        NoteStore::log_tool_call(self, session_id, tool_name, outcome)
            .await
            .map_err(port_err)
    }

    async fn tool_call_log_rows(
        &self,
        since: i64,
        limit: usize,
    ) -> PortResult<Vec<PortToolCallLogRow>> {
        let rows = NoteStore::tool_call_log_rows(self, since, limit)
            .await
            .map_err(port_err)?;
        Ok(rows.into_iter().map(to_port_log_row).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The direction the exhaustive `match` above cannot check. `to_scope` is
    // exhaustive on the CONTRACT enum, so a variant added there is a build
    // error. A variant added to the STORE enum compiles fine and simply becomes
    // inexpressible over the seam — silently, at runtime, as a note the
    // programs outside `code/` can never write or recognise.
    //
    // Watched failing before it was kept: adding a sixth `NoteSource` here and
    // to its `ALL` fails `contract_mirrors_every_store_source` with "store
    // source `archived` has no mirror on the contract side", which is the
    // message that names the fix.

    #[test]
    fn contract_mirrors_every_store_scope() {
        for st in NoteScope::ALL {
            let mirrored = PortNoteScope::parse(st.as_str()).unwrap_or_else(|| {
                panic!(
                    "store scope `{}` has no mirror on the contract side",
                    st.as_str()
                )
            });
            assert_eq!(to_scope(mirrored).as_str(), st.as_str());
        }
    }

    #[test]
    fn contract_mirrors_every_store_source() {
        for st in NoteSource::ALL {
            let mirrored = PortNoteSource::parse(st.as_str()).unwrap_or_else(|| {
                panic!(
                    "store source `{}` has no mirror on the contract side",
                    st.as_str()
                )
            });
            assert_eq!(to_source(mirrored).as_str(), st.as_str());
            assert_eq!(
                mirrored.priority(),
                to_source(mirrored).priority(),
                "audit ranking must be identical on both sides of the seam"
            );
        }
    }

    #[test]
    fn scope_filter_crosses_the_seam_unchanged() {
        let f = PortScopeFilter {
            scopes: vec![PortNoteScope::Global, PortNoteScope::Feature],
            feature_id: Some("nc-7-note".to_string()),
        };
        let st = to_scope_filter(&f);
        assert_eq!(
            st.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            f.scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(st.feature_id, f.feature_id);
    }

    #[test]
    fn every_note_field_survives_the_crossing() {
        let st = Note {
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
            // Spelled in full, deliberately: a new field on `Note` must be
            // adjudicated here (mirror it, or state why the port cannot see it)
            // rather than silently defaulted past the seam.
            origin_node_id: Some("node-abc".into()),
            sent_at: Some(11),
            received_at: Some(12),
        };
        let n = to_port_note(st.clone());
        assert_eq!(n.id, st.id);
        assert_eq!(n.kind, st.kind);
        assert_eq!(n.content, st.content);
        assert_eq!(n.symbols, st.symbols);
        assert_eq!(n.files, st.files);
        assert_eq!(n.session_id, st.session_id);
        assert_eq!(n.created_at, st.created_at);
        assert_eq!(n.tool_name, st.tool_name);
        assert_eq!(n.retired_at, st.retired_at);
        assert_eq!(n.retired_by, st.retired_by);
        assert_eq!(n.scope, st.scope);
        assert_eq!(n.feature_id, st.feature_id);
        assert_eq!(n.promoted_from, st.promoted_from);
        assert_eq!(n.related_entity, st.related_entity);
        assert_eq!(n.source, st.source);
        assert_eq!(n.supersedes, st.supersedes);
        assert_eq!(n.payload_json, st.payload_json);
    }

    #[test]
    fn tool_call_log_row_crosses_the_seam_unchanged() {
        let st = ToolCallLogRow {
            id: "t1".into(),
            session_id: "s1".into(),
            tool_name: "blast".into(),
            outcome: "success".into(),
            called_at: 42,
        };
        let r = to_port_log_row(st.clone());
        assert_eq!(r.id, st.id);
        assert_eq!(r.session_id, st.session_id);
        assert_eq!(r.tool_name, st.tool_name);
        assert_eq!(r.outcome, st.outcome);
        assert_eq!(r.called_at, st.called_at);
    }
}
