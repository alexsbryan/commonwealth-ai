//! Multi-origin entity reconciliation primitive (Phase 4 of the
//! architecture-over-Enron push).
//!
//! The substrate work the whole push is named after. Operates on a
//! `Vec<Entity>` whose atoms carry [`Provenance`] (AD-4) and produces
//! [`ReconciledEntity`]s — same canonical id across multiple
//! surface-form mentions, with the merge signals that fired recorded
//! so the audit log can reproduce the reasoning.
//!
//! Three sub-modules:
//!   - [`signals`] — the pluggable merge signals (name similarity,
//!     email-header match, organisation+role match, judge-confirmed).
//!     Each signal is a pure function over `(left, right) -> bool` +
//!     a name. The reconciler folds the set of signals that fire per
//!     candidate pair.
//!   - [`multi_origin`] — the actual merger. Produces
//!     [`ReconciledEntity`]s + the op log.
//!   - [`oplog`] — append-only JSONL audit log
//!     (`atlas/reconciliation_oplog.jsonl`). Supports `Merge` *and*
//!     `Split` from day one (the legacy `entity_extraction::merge_responses`
//!     is destructive — this primitive is not).

pub mod multi_origin;
pub mod oplog;
pub mod signals;

pub use multi_origin::{reconcile, ReconciledEntity, ReconciliationPolicy};
pub use oplog::{OpKind, OplogEntry, OplogReader, OplogWriter};
pub use signals::{MergeSignal, MergeSignalCheck};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity, Provenance, SignalKind};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn ent(id: &str, name: &str, et: EntityType, signal_kind: SignalKind, doc: &str) -> Entity {
        Entity {
            id: AtomId::from_raw(id),
            canonical_name: name.to_string(),
            aliases: Vec::new(),
            entity_type: et,
            first_appearance: ChunkRef::new("sec-001", None),
            description: String::new(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Provenance::new(signal_kind_extractor_name(&signal_kind), doc, signal_kind),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn signal_kind_extractor_name(sk: &SignalKind) -> &'static str {
        match sk {
            SignalKind::GlinerSpan => "gliner_chunk_ner",
            SignalKind::LlmBatch => "llm_batch",
            SignalKind::ColumnHeader => "column_aware",
            SignalKind::EmailHeader => "email_rfc5322",
            SignalKind::AttachmentDescription => "described_asset",
            SignalKind::OperatorAction => "operator",
            SignalKind::Other(_) => "other",
        }
    }

    #[test]
    fn name_match_collapses_surface_forms() {
        let entities = vec![
            ent(
                "entity-001",
                "Ken Lay",
                EntityType::Person,
                SignalKind::EmailHeader,
                "msg-1",
            ),
            ent(
                "entity-002",
                "Kenneth L. Lay",
                EntityType::Person,
                SignalKind::EmailHeader,
                "msg-1",
            ),
            ent(
                "entity-003",
                "Jeff Skilling",
                EntityType::Person,
                SignalKind::EmailHeader,
                "msg-2",
            ),
        ];
        let policy = ReconciliationPolicy::default();
        let outcome = reconcile(entities, &policy);
        // Two reconciled entities — Lay's two surface forms (same
        // origin, single signal sufficient) + Skilling alone.
        assert_eq!(outcome.entities.len(), 2);
        let lay = outcome
            .entities
            .iter()
            .find(|e| e.canonical_name.contains("Lay"))
            .expect("lay reconciled entity");
        assert_eq!(lay.surface_forms.len(), 2);
        let signals = lay
            .signals_fired
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>();
        assert!(signals.contains(&"name_similarity"));
    }

    #[test]
    fn cross_origin_requires_dual_signals_when_configured() {
        // Two name-similar mentions from different origins — default
        // policy demands two signals.
        let entities = vec![
            ent(
                "entity-001",
                "Williams",
                EntityType::Institution,
                SignalKind::ColumnHeader,
                "spread.xlsx",
            ),
            ent(
                "entity-002",
                "Williams",
                EntityType::Institution,
                SignalKind::LlmBatch,
                "msg-1",
            ),
        ];
        let mut policy = ReconciliationPolicy::default();
        policy.cross_origin_required_signals = 2;
        let outcome = reconcile(entities, &policy);
        // Single signal (name_similarity) — must reject the merge.
        assert_eq!(outcome.entities.len(), 2);
    }
}
