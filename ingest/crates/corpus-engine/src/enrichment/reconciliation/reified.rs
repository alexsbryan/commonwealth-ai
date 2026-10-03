// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merges as atoms — the `same_as` Claim a DECLARED corpus gets.
//!
//! The oplog already records every merge, but an oplog is an operator
//! artefact: nothing that answers a question reads it. A corpus that declared
//! its identity criteria asked for the merge to be part of the knowledge, so
//! each one is also written as a Claim the inspector shows and either side
//! reaches.
//!
//! Only `svrn enrich reconcile` on a declared corpus calls this — see the
//! DEFAULTS_LEDGER row for the condition that would make it always-on.

use super::multi_origin::ReifiedMerge;
use crate::enrichment::atlas::atoms::{AtomId, Claim};
use crate::enrichment::atlas::edges::Edge;

/// Turn merges into `same_as` Claims plus the edges that reach them.
///
/// `next_claim_index` and `next_edge_index` are the caller's next free ids —
/// this primitive owns no counter, so appending to an existing atlas cannot
/// collide with what is already there.
///
/// The edges are `Involves` (claim → each merged atom, so a reader seeded on
/// either side finds the merge) and `Grounds` (claim → each evidence chunk).
/// NOT `Grounding`: that is the cross-corpus family (`edges.rs`), and a merge
/// inside one corpus is not a cross-corpus link.
pub fn reify_merges(
    merges: &[ReifiedMerge],
    next_claim_index: usize,
    next_edge_index: usize,
) -> (Vec<Claim>, Vec<Edge>) {
    use crate::enrichment::atlas::edges::{EdgeId, EdgeProvenance, EdgeType};
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
    };

    let mut claims = Vec::with_capacity(merges.len());
    let mut edges = Vec::new();
    let mut claim_index = next_claim_index;
    let mut edge_index = next_edge_index;

    for merge in merges {
        let claim_id = AtomId::claim(claim_index);
        claim_index += 1;
        let mut attributes = serde_json::Map::new();
        attributes.insert(
            "same_as".to_string(),
            serde_json::Value::Array(
                merge
                    .inputs
                    .iter()
                    .map(|id| serde_json::Value::String(id.as_str().to_string()))
                    .collect(),
            ),
        );
        attributes.insert(
            "grade".to_string(),
            serde_json::Value::String(merge.grade.clone()),
        );
        attributes.insert(
            "signals".to_string(),
            serde_json::Value::Array(
                merge
                    .signals
                    .iter()
                    .map(|s| serde_json::Value::String(s.as_str().to_string()))
                    .collect(),
            ),
        );
        claims.push(Claim {
            attributes,
            // The merge is ABOUT the atom it produced.
            subject: Some(merge.output.clone()),
            id: claim_id.clone(),
            content: format!(
                "{} are the same thing ({} identity: {})",
                merge
                    .inputs
                    .iter()
                    .map(|i| i.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                merge.grade,
                merge
                    .signals
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: merge.evidence.clone(),
            quotable_excerpt: None,
            attributed_to: None,
            // Derived by the reconciler, not scored by a model — the same
            // rule every deterministic atom in Phase 3 follows.
            confidence: None,
            anchor: None,
            claim_kind: Some("same_as".to_string()),
            concession_outcome: None,
            evidence_kind: None,
            enrichment_depth: EnrichmentDepth::Structural,
        });

        for target in &merge.inputs {
            edges.push(Edge {
                id: EdgeId::new(edge_index),
                edge_type: EdgeType::Involves,
                source: claim_id.clone(),
                target: target.clone(),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            });
            edge_index += 1;
        }
        for e in &merge.evidence {
            edges.push(Edge {
                id: EdgeId::new(edge_index),
                edge_type: EdgeType::Grounds,
                source: claim_id.clone(),
                target: AtomId::from_raw(e.chunk_id.clone()),
                evidence: vec![e.clone()],
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            });
            edge_index += 1;
        }
    }
    (claims, edges)
}
