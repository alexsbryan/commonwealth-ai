// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Configuration.constituent_atoms` → [`EdgeType::Configures`] edges.
//!
//! Its own file rather than another 150 lines of `store.rs`, which is already
//! past ARCH §3.1's ceiling: the derivation answers one question no other part
//! of the store write asks — which field is secretly an edge list — and the
//! store's job is the CSR and the Lance table, not the vocabulary.

use super::super::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use super::super::AtomEnvelope;

/// The [`EdgeType::Configures`] edges a `Configuration` atom's
/// `constituent_atoms` field already IS — made traversable.
///
/// A Configuration names the atoms it is a configuration OF in a field, not in
/// `edges.json`; `schema_validation`'s orphan analysis says so in as many words
/// and excludes the kind from its tally. That is fine for validation and fatal
/// for navigation: the thematic row seeds on `Configuration`, so a walk could
/// land on one and have nowhere to go — a terminus by accident, unlike
/// `Summary`, which is a terminus by rule (`ground`'s R1). On
/// `brothers-karamazov-book-1` that is three of the four seedable themes.
///
/// No new edge kind — spec §3 forbids a private one, and none is needed:
/// `Configures` has carried a CSR type byte (6) and an `edge_weight` of 0.6
/// since ATLAS_STORAGE_V2, minted for this relation (`context.rs`: "Configures
/// / Composes → medium (configuration's constituent …)") and emitted by
/// nothing. This is the writer that was missing, not a new vocabulary.
///
/// Derived HERE, at the one store write, rather than in a pipeline phase,
/// because every atlas that gets a v2 store gets it from this function —
/// `write_atlas_full`'s fresh write and `atlas migrate-all`'s rebuild from
/// `atoms.json` alike — so an atlas cannot have the field and lack the edge.
/// `atoms.json` and `edges.json` are unchanged on disk: this is a projection
/// into the read path, the same standing as the CSR itself.
///
/// Idempotent and additive: an edge already present in `edges` for the same
/// (source, target) pair under this kind is not duplicated, and a constituent
/// id no atom carries is dropped by `write_edges_csr`'s `by_id` join anyway.
/// The id is content-derived (ARCH §7.5 — never a counter), so re-running the
/// derivation over the same atoms produces the same edge ids.
pub(crate) fn derive_configures_edges(atoms: &[AtomEnvelope], existing: &[Edge]) -> Vec<Edge> {
    let already: std::collections::HashSet<(&str, &str)> = existing
        .iter()
        .filter(|e| e.edge_type == EdgeType::Configures)
        .map(|e| (e.source.as_str(), e.target.as_str()))
        .collect();
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for atom in atoms {
        let AtomEnvelope::Configuration(cfg) = atom else {
            continue;
        };
        for target in &cfg.constituent_atoms {
            let (s, t) = (cfg.id.as_str(), target.as_str());
            if s == t || already.contains(&(s, t)) || !seen.insert((s.to_string(), t.to_string())) {
                continue;
            }
            out.push(Edge {
                id: EdgeId::from_raw(format!("configures:{s}:{t}")),
                edge_type: EdgeType::Configures,
                source: cfg.id.clone(),
                target: target.clone(),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                // Deterministic field read, not an inference: the model already
                // committed to the membership when it wrote `constituent_atoms`,
                // and re-deciding it here would be a second opinion about one
                // fact. Same standing as step 3a's `Involves` edges.
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::Configuration;
    use crate::enrichment::atlas::edges::{EdgeProvenance, EdgeType};
    use crate::enrichment::atlas::{AtomId, Edge, EdgeId};
    use crate::enrichment::pipeline::atlas::EnrichmentDepth;

    fn configuration(idx: usize, constituents: &[AtomId]) -> AtomEnvelope {
        AtomEnvelope::Configuration(Configuration {
            id: AtomId::configuration(idx),
            label: format!("theme-{idx}"),
            description: "an interpretive structure".into(),
            constituent_atoms: constituents.to_vec(),
            evidence: Vec::new(),
            confidence: 0.8,
            interpretive_note: "one reading among several".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    /// The field becomes edges of the EXISTING kind. Failing input: delete the
    /// `derive_configures_edges` call from `write_store` — the walk then seeds
    /// on a Configuration it can never leave, which is the
    /// `brothers-karamazov-book-1` theme bar reading could-not-judge.
    #[test]
    fn constituent_atoms_become_configures_edges() {
        let atoms = vec![configuration(1, &[AtomId::entity(1), AtomId::claim(2)])];
        let derived = derive_configures_edges(&atoms, &[]);
        assert_eq!(derived.len(), 2);
        assert!(derived.iter().all(|e| e.edge_type == EdgeType::Configures));
        assert!(derived
            .iter()
            .all(|e| e.provenance == EdgeProvenance::Derived && e.confidence == 1.0));
        assert_eq!(derived[0].source.as_str(), "config-0001");
        let targets: Vec<&str> = derived.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(targets, vec!["entity-0001", "claim-0002"]);
        // Identity from essence, never a counter (ARCH §7.5): the same atoms
        // derive the same ids on every rebuild.
        assert_eq!(derived[0].id.as_str(), "configures:config-0001:entity-0001");
    }

    /// Additive, never duplicating: an atlas whose pipeline already wrote a
    /// `Configures` edge for a pair keeps exactly one. Failing input: drop the
    /// `already` set — `migrate-all` then doubles every such edge on each
    /// rebuild, silently, because the CSR has no uniqueness constraint.
    #[test]
    fn an_edge_the_pipeline_already_wrote_is_not_duplicated() {
        let atoms = vec![configuration(1, &[AtomId::entity(1), AtomId::entity(2)])];
        let existing = vec![Edge {
            id: EdgeId::from_raw("edge-00001"),
            edge_type: EdgeType::Configures,
            source: AtomId::configuration(1),
            target: AtomId::entity(1),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 0.9,
            provenance: EdgeProvenance::LlmConfiguration,
        }];
        let derived = derive_configures_edges(&atoms, &existing);
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].target.as_str(), "entity-0002");

        // And a repeated constituent id in the field itself is one edge.
        let dupe = vec![configuration(2, &[AtomId::entity(9), AtomId::entity(9)])];
        assert_eq!(derive_configures_edges(&dupe, &[]).len(), 1);
    }

    /// A corpus with no Configuration atoms derives nothing — the negative
    /// control, so a passing positive is not just "this function returns rows".
    #[test]
    fn an_atlas_without_configurations_derives_no_edges() {
        let atoms = vec![super::super::tests::entity(1, "Alyosha")];
        assert!(derive_configures_edges(&atoms, &[]).is_empty());
    }
}
