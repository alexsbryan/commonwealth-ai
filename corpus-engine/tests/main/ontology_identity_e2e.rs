// SPDX-License-Identifier: AGPL-3.0-or-later
//! Identity under a DECLARED ontology (ontology-v1 P3): when do two mentions
//! become one atom, and what does the atlas say about it afterwards.
//!
//! Three behaviours, each with the red input named: an external identifier
//! that merges strictly across origins where the default stack is silent, the
//! SAME evidence declared as a fallback instead and therefore gated, and the
//! undeclared case where nothing new can fire at all (I5 — `bench enron`
//! reads `entities` and never looks at `reified`).
//!
//! These drive the public `reconcile` / `reify_merges` surface, so they live
//! here rather than in `multi_origin.rs`, which ARCH §3.1 would otherwise
//! push into the approach band.

use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity, Provenance, SignalKind};
use corpus_engine::enrichment::atlas::edges::EdgeType;
use corpus_engine::enrichment::ontology::IdentityPolicy;
use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
use corpus_engine::enrichment::reconciliation::{
    reconcile, reify_merges, MergeSignal, ReconciliationPolicy, ReifiedMerge,
};

fn ent(name: &str, id: &str, sk: SignalKind, doc: &str) -> Entity {
    Entity {
        id: AtomId::from_raw(id),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec-001", None),
        description: String::new(),
        defining_quote: None,
        salience: 0.5,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Provenance::new("ext", doc, sk),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }
}

// ── Declared identity keys (ontology v1, P3) ─────────────

/// `coin` identified by its accession number — the `identity` half of the
/// two tests below. The `identity_fallback` half builds its own map from
/// the same key, because that contrast IS the test.
fn identity_policy() -> IdentityPolicy {
    let mut identity = std::collections::BTreeMap::new();
    identity.insert("coin".to_string(), vec!["find_id".to_string()]);
    IdentityPolicy {
        identity,
        identity_fallback: std::collections::BTreeMap::new(),
    }
}

fn coin(id: &str, name: &str, find_id: &str, sk: SignalKind, doc: &str) -> Entity {
    let mut e = ent(name, id, sk, doc);
    e.entity_type = EntityType::Other("coin".into());
    e.attributes
        .insert("find_id".into(), serde_json::Value::String(find_id.into()));
    e
}

#[test]
fn external_id_merges_strictly_cross_origin() {
    // Two catalogue entries for one find, from two extractors, with names
    // that share no token at all. Nothing in the default stack can see
    // them: the merge exists only because the recipe said `find_id` IS the
    // identity — so this also proves the blocking pass carries the pair.
    let entities = vec![
        coin(
            "entity-0001",
            "Series Y penny of Aldfrith",
            "SF-2019-114",
            SignalKind::LlmBatch,
            "cat.md",
        ),
        coin(
            "entity-0002",
            "Wessex Down 114",
            "sf-2019-114",
            SignalKind::ColumnHeader,
            "finds.csv",
        ),
    ];
    // The default policy needs TWO signals cross-origin, and the pair has
    // no name signal at all — so a run without the declaration is the red
    // input for this test.
    let undeclared = reconcile(entities.clone(), &ReconciliationPolicy::default());
    assert_eq!(undeclared.entities.len(), 2, "no declaration, no merge");
    assert!(undeclared.reified.is_empty());

    let policy = ReconciliationPolicy {
        identity: identity_policy(),
        ..Default::default()
    };
    let outcome = reconcile(entities, &policy);
    assert_eq!(outcome.entities.len(), 1, "one find, one atom");
    assert_eq!(outcome.reified.len(), 1);
    let merge = &outcome.reified[0];
    assert_eq!(merge.grade, ReifiedMerge::EXTERNAL);
    assert!(merge.signals.contains(&MergeSignal::ExternalId));
    assert_eq!(merge.inputs.len(), 2);
}

#[test]
fn descriptive_key_alone_is_gated() {
    // The SAME evidence as the test above, with the key moved from
    // `identity` to `identity_fallback`. That is the whole difference
    // between a criterion of identity and a description of one, and it is
    // the only difference between these two fixtures.
    //
    // A fallback key on the NAME would prove nothing here: agreeing on
    // `name` makes `name_similarity` fire too, so the pair would clear the
    // gate on two signals and the gate would never be tested. `find_id` on
    // two entries with unrelated names leaves the descriptive key alone.
    let mut identity_fallback = std::collections::BTreeMap::new();
    identity_fallback.insert("coin".to_string(), vec!["find_id".to_string()]);
    let policy = ReconciliationPolicy {
        identity: IdentityPolicy {
            identity: std::collections::BTreeMap::new(),
            identity_fallback,
        },
        ..Default::default()
    };
    let left = coin(
        "entity-0001",
        "Series Y penny of Aldfrith",
        "SF-2019-114",
        SignalKind::LlmBatch,
        "cat.md",
    );
    let right_cross = coin(
        "entity-0002",
        "Wessex Down 114",
        "sf-2019-114",
        SignalKind::ColumnHeader,
        "finds.csv",
    );
    let gated = reconcile(vec![left.clone(), right_cross], &policy);
    assert_eq!(
        gated.entities.len(),
        2,
        "one descriptive signal does not clear the cross-origin gate of 2"
    );
    assert!(gated.reified.is_empty());

    // Same evidence, same origin: one signal is the whole gate there, and
    // it merges — so the refusal above is the CROSS-ORIGIN rule doing its
    // job, not the signal failing to fire.
    let right_same = coin(
        "entity-0002",
        "Wessex Down 114",
        "sf-2019-114",
        SignalKind::LlmBatch,
        "cat.md",
    );
    let merged = reconcile(vec![left, right_same], &policy);
    assert_eq!(merged.entities.len(), 1);
    assert_eq!(merged.reified.len(), 1);
    assert!(merged.reified[0]
        .signals
        .contains(&MergeSignal::DescriptiveKey));
    assert_eq!(
        merged.reified[0].grade,
        ReifiedMerge::SIGNAL_GATED,
        "no external identifier agreed, and no judge ran"
    );
}

#[test]
fn default_policy_reified_is_empty() {
    // I5 in one assertion: with no declaration the signal stack, the
    // blocking keys and the gate are what they were, so `bench enron` —
    // which reads `entities` and never looks at `reified` — cannot move.
    let entities = vec![
        ent("Kenneth Lay", "entity-0001", SignalKind::LlmBatch, "a.eml"),
        ent("Kenneth Lay", "entity-0002", SignalKind::LlmBatch, "b.eml"),
    ];
    let outcome = reconcile(entities, &ReconciliationPolicy::default());
    assert_eq!(
        outcome.entities.len(),
        1,
        "the same-origin merge still runs"
    );
    assert_eq!(outcome.oplog_entries.len(), 1);
    assert_eq!(
        outcome.reified.len(),
        1,
        "the merge IS reified — the primitive always fills it"
    );
    assert!(
        !outcome.reified[0]
            .signals
            .contains(&MergeSignal::ExternalId),
        "but nothing declared can have fired"
    );

    // And the caller is what decides: nothing writes these unless the
    // corpus declares an ontology (`enrich reconcile`'s gate).
    let (claims, edges) = reify_merges(&outcome.reified, 1, 1);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].claim_kind.as_deref(), Some("same_as"));
    assert_eq!(
        claims[0].subject.as_ref().map(|s| s.as_str()),
        Some("entity-0001")
    );
    assert_eq!(claims[0].attributes["same_as"].as_array().unwrap().len(), 2);
    assert!(
        edges.iter().all(|e| e.source == claims[0].id),
        "every edge hangs off the claim"
    );
    assert!(
        edges.iter().any(|e| e.edge_type == EdgeType::Involves),
        "Involves, so a reader seeded on either side finds the merge"
    );
    assert!(
        !edges.iter().any(|e| e.edge_type == EdgeType::Grounding),
        "Grounding is the cross-corpus family; a merge inside one corpus is not one"
    );
}
