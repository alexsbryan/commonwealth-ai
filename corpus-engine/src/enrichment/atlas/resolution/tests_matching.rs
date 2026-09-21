// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 3b name matching — fuzzy participant snap, salience attribution, relation_key.
//!
//! Sibling of `tests.rs`, which was over ARCH §3.2's 1200-line ceiling when the
//! test module moved out of `resolution.rs`. Split at its own seam, not by line
//! count: this file is the name-to-atom resolvers, that one is the step itself.

use super::tests_fixtures::*;
use super::*;
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntitySketch, EntityType, EventSketch};
use std::sync::Arc;

// ── Landing 2.A — fuzzy participant snap fallbacks ────────

/// Build a minimal resolved-entity set for fuzzy-lookup tests.
/// Mirrors what Step 3a would produce in the real pipeline.
fn fuzzy_fixture_entities() -> Vec<super::super::atoms::Entity> {
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    vec![
        Entity {
            id: AtomId::entity(1),
            canonical_name: "Fyodor Fyodorovitch Karamazoff".into(),
            aliases: vec!["Fyodor".into(), "Karamazoff".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "Patriarch.".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
        Entity {
            id: AtomId::entity(2),
            canonical_name: "Alexei Fyedorovitch Kramzof".into(),
            aliases: vec!["Alyosha".into(), "Alexei".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "Youngest son.".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
        Entity {
            id: AtomId::entity(3),
            canonical_name: "Sofya Ivanovna Karamzova".into(),
            aliases: vec!["Sofya".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0003", None),
            description: "Second wife.".into(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
    ]
}

#[test]
fn resolve_entity_id_fuzzy_snaps_cyrillic_mangled_participant_to_canonical() {
    // Observed in the Landing 1 smoke test: relation participant
    // strings leak mid-word Cyrillic chars (`Sofya Ivаnovna
    // Karаmzova` — а is Cyrillic) even when the entity atom was
    // resolved from the clean Latin form. After `fold` applies
    // `transliterate_cyrillic` both sides collapse to the same
    // folded key and the exact-match path takes it — no
    // Levenshtein needed. Lock this behaviour.
    let entities = fuzzy_fixture_entities();
    let name_index = build_name_index(&entities);
    let token_index = build_token_index(&entities);
    let mangled = "Sofya Ivаnovna Karаmzova"; // Cyrillic а in two places
    let id = resolve_entity_id_fuzzy(mangled, &name_index, &token_index)
        .expect("cyrillic-mangled form should snap to Sofya entity");
    assert_eq!(id.as_str(), "entity-0003");
}

#[test]
fn resolve_entity_id_fuzzy_snaps_levenshtein_within_two_after_translit() {
    // Isolate the vote fallback: `Karazoff` is not a canonical,
    // alias, or token-index key (the real forms are `Karamazoff`
    // with an extra `m-a`). Exact-match fallbacks (1) and (2)
    // both miss. Lev(`karazoff`, `karamazoff`) = 2 and
    // `karamazoff` appears in exactly one entity's tokens, so
    // the vote fallback lands on that entity.
    let entities = fuzzy_fixture_entities();
    let name_index = build_name_index(&entities);
    let token_index = build_token_index(&entities);
    let drifted = "Karazoff";
    let id = resolve_entity_id_fuzzy(drifted, &name_index, &token_index)
        .expect("Karazoff ↔ Karamazoff within Levenshtein-2 should snap via vote fallback");
    assert_eq!(id.as_str(), "entity-0001");
}

#[test]
fn resolve_entity_id_fuzzy_snap_is_robust_to_mixed_drift_with_one_clean_token() {
    // Realistic smoke-test form: `Fyodor Pvlvitch Karazoff`.
    // The `Fyodor` token short-circuits via fallback 2 (appears
    // in exactly one entity's tokens). The point of this test
    // is to assert the call resolves regardless of which
    // fallback path takes it — so if we later tighten the
    // long-token fallback, the vote path would catch it.
    let entities = fuzzy_fixture_entities();
    let name_index = build_name_index(&entities);
    let token_index = build_token_index(&entities);
    let id = resolve_entity_id_fuzzy("Fyodor Pvlvitch Karazoff", &name_index, &token_index)
        .expect("Fyodor + Karazoff should resolve to entity-0001");
    assert_eq!(id.as_str(), "entity-0001");
}

#[test]
fn resolve_entity_id_fuzzy_refuses_to_snap_on_multi_match_ambiguity() {
    // When a query token sits within Levenshtein 2 of tokens
    // belonging to two distinct entities, the vote for that
    // token is ambiguous and must not count. If no query token
    // produces a single-entity vote, the lookup returns None.
    // Construct two entities whose long tokens collide under
    // Lev 2 so the fallback cannot disambiguate.
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    let entities = vec![
        Entity {
            id: AtomId::entity(1),
            canonical_name: "Marina".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
        Entity {
            id: AtomId::entity(2),
            canonical_name: "Marika".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
    ];
    let name_index = build_name_index(&entities);
    let token_index = build_token_index(&entities);
    // `Marnka` is Lev-1 from `Marina` (replace i→n wait no, marna
    // vs marina... let me recompute). Take `Marinka` — Lev-1
    // from `Marina` (insert k) AND Lev-1 from `Marika` (insert n).
    let ambiguous = "Marinka";
    assert_eq!(levenshtein("marinka", "marina"), 1);
    assert_eq!(levenshtein("marinka", "marika"), 1);
    assert!(
        resolve_entity_id_fuzzy(ambiguous, &name_index, &token_index).is_none(),
        "ambiguous Lev-1 match against 2 entities must not snap"
    );
}

#[test]
fn resolve_entity_id_fuzzy_respects_min_token_length_guard() {
    // `Ivan` (4 chars) is below FUZZY_TOKEN_MIN_LEN=5 so the
    // vote fallback cannot consider it — this is the guard
    // that prevents `Ivan` ↔ `Ilya` (Lev 3, would be rejected
    // anyway) or `Anna` ↔ `Anka` style near-misses from
    // collapsing short ambiguous names.
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    let entities = vec![Entity {
        id: AtomId::entity(1),
        canonical_name: "Anna".into(),
        aliases: vec![],
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: "".into(),
        salience: 1.0,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        defining_quote: None,
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }];
    let name_index = build_name_index(&entities);
    let token_index = build_token_index(&entities);
    // `Anka` is Lev-1 from `Anna` but both are 4 chars. Guard
    // must keep them distinct.
    assert!(
        resolve_entity_id_fuzzy("Anka", &name_index, &token_index).is_none(),
        "4-char tokens are below the fuzzy floor and must not snap"
    );
}

// ── Landing 4.B — salience-aware attribution resolver ─────

fn high_salience_fyodor() -> super::super::atoms::Entity {
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    // Aliases are the richer full-name forms Phase 3a merges
    // together — NOT the bare first name. Real-world smoke
    // data from brothers_karamazov showed no entity had
    // "Fyodor" as a standalone alias; every alias was a full
    // patronymic form. Mirror that here so the strict fuzzy
    // resolver actually has to bail on "Fyodor" alone.
    Entity {
        id: AtomId::entity(1),
        canonical_name: "Fyodor Pavlovich Karamazov".into(),
        aliases: vec!["Fyodor Pavlovitch Karamazov".into()],
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: "Patriarch.".into(),
        salience: 1.0,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        defining_quote: None,
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }
}

fn low_salience_fyodor_drift() -> super::super::atoms::Entity {
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    Entity {
        id: AtomId::entity(2),
        canonical_name: "Fyodor Karazov".into(),
        aliases: vec![],
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0004", None),
        description: "Variant.".into(),
        salience: 0.2,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        defining_quote: None,
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }
}

fn two_contested_ivans() -> (super::super::atoms::Entity, super::super::atoms::Entity) {
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    let a = Entity {
        id: AtomId::entity(1),
        canonical_name: "Ivan Karamazov".into(),
        aliases: vec![],
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: "Brother.".into(),
        defining_quote: None,
        salience: 0.8,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    };
    let b = Entity {
        id: AtomId::entity(2),
        canonical_name: "Ivan Petrovich Sidorov".into(),
        aliases: vec![],
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0002", None),
        description: "Different Ivan.".into(),
        defining_quote: None,
        salience: 0.7,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    };
    (a, b)
}

#[test]
fn salience_resolver_snaps_to_dominant_when_strict_bails_on_ambiguity() {
    // Two entities share the token "fyodor" — the strict
    // resolver refuses to guess (single-match-wins fails). The
    // salience fallback picks the father (salience 1.0) over
    // the drift variant (salience 0.2) because 1.0 > 2.0 × 0.2.
    let entities = vec![high_salience_fyodor(), low_salience_fyodor_drift()];
    let name_index = build_name_index(&entities);
    let token_index = build_token_index(&entities);
    // "Fyodor" alone is ambiguous — both entities' first tokens
    // fold to "fyodor" and both have "fyodor" as a long token.
    assert!(resolve_entity_id_fuzzy("Fyodor", &name_index, &token_index).is_none());
    let id = resolve_entity_id_with_salience("Fyodor", &entities, &name_index, &token_index)
        .expect("salience-aware fallback should snap to dominant");
    assert_eq!(id.as_str(), "entity-0001");
}

#[test]
fn salience_resolver_bails_when_candidates_are_comparable() {
    // Two Ivans with comparable salience (0.8 vs 0.7 → ratio
    // 1.14, below SALIENCE_DOMINANCE_FACTOR=2.0). Build
    // canonicals that DON'T exact-match the query so the
    // strict resolver is forced to bail and the salience
    // fallback is actually exercised. Both canonicals share
    // "ivan" and "karamazov" as tokens.
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    let contested = vec![
        Entity {
            id: AtomId::entity(1),
            canonical_name: "Ivan Fyodorovich Karamazov".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            defining_quote: None,
            salience: 0.8,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
        Entity {
            id: AtomId::entity(2),
            canonical_name: "Ivan Petrovich Karamazov".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            defining_quote: None,
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
    ];
    let name_index = build_name_index(&contested);
    let token_index = build_token_index(&contested);
    // Query shares "karamazov" token with both entities AND
    // first token "ivan" with both. Strict resolver bails
    // (karamazov appears in 2 entities → ambiguous). Salience
    // 0.8 vs 0.7 → 0.8 < 2.0 × 0.7 → fallback also bails.
    assert!(
        resolve_entity_id_with_salience(
            "Ivan Unknownovich Karamazov",
            &contested,
            &name_index,
            &token_index,
        )
        .is_none(),
        "comparable salience must not snap — the resolver stays silent"
    );
}

#[test]
fn salience_resolver_respects_first_token_guard() {
    // A dominant-salience entity does NOT snap if its first
    // token differs from the query. A claim attributed to
    // "Ivan" must not land on Fyodor even though Fyodor is
    // the most-salient Karamazov.
    let fyodor = high_salience_fyodor();
    let (ivan, _) = two_contested_ivans();
    let entities = vec![fyodor, ivan];
    let name_index = build_name_index(&entities);
    let token_index = build_token_index(&entities);
    // "Ivan" (4 chars) is below min-token-length → None.
    assert!(
        resolve_entity_id_with_salience("Ivan", &entities, &name_index, &token_index,).is_none()
    );
    // "Ivan Karamazov" shares "karamazov" with Fyodor but the
    // first_token_matches guard rejects Fyodor (fyodor ≠ ivan).
    // Ivan's canonical "Ivan Karamazov" matches perfectly via
    // the strict path — salience fallback isn't needed.
    let id =
        resolve_entity_id_with_salience("Ivan Karamazov", &entities, &name_index, &token_index)
            .expect("should snap to Ivan");
    assert_eq!(id.as_str(), "entity-0001"); // Ivan is entity-0001 in this fixture because he was listed first in two_contested_ivans
}

// ── Landing 2.C — relation_key dedup invariant ────────────

#[test]
fn relation_key_invariant_same_participants_different_labels_collapse_today() {
    // Pin the contract around the `relation_key`-based dedup at
    // line ~746: two RelationSketch inputs with the SAME resolved
    // participant set but DIFFERENT labels collapse to a single
    // Relation atom with whichever label arrived first. This
    // behaviour is deliberate — a fuzzy-snapped participant
    // list can collide two distinct-looking sketches onto the
    // same logical relation, and we want one atom, not two
    // duplicates with conflicting labels. When the fuzzy snap
    // becomes more permissive (Landing 2.A), this invariant
    // must still hold; if it ever changes the caller must
    // decide the policy deliberately rather than drifting.
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, RelationSketch};

    let entities = vec![
        Entity {
            id: AtomId::entity(1),
            canonical_name: "Alyosha".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
        Entity {
            id: AtomId::entity(2),
            canonical_name: "Zossima".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        },
    ];

    // Two sections each introducing a relation between the
    // same two participants with different prose labels.
    let sections = vec![
        SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            relations_introduced: vec![RelationSketch {
                attributes: Default::default(),
                relation_type: None,
                participants: vec!["Alyosha".into(), "Zossima".into()],
                label: "Novice-elder bond".into(),
                anchor: "knelt at the elder's feet".into(),
            }],
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0002".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            relations_introduced: vec![RelationSketch {
                attributes: Default::default(),
                relation_type: None,
                participants: vec!["Alyosha".into(), "Zossima".into()],
                label: "Spiritual father-son".into(),
                anchor: "blessed the novice".into(),
            }],
            ..Default::default()
        },
    ];

    let out = resolve_step_3b(&sections, &entities, &[]).unwrap();
    // Contract: one Relation atom, first label wins. Changing
    // this policy requires updating this test AND deciding the
    // merge strategy explicitly (e.g. keep both, concatenate
    // labels, promote to Interpreted depth with richer metadata).
    assert_eq!(
        out.relations.len(),
        1,
        "same-participants-different-labels must collapse to one atom \
         under the current dedup policy"
    );
    assert_eq!(out.relations[0].label, "Novice-elder bond");
}
