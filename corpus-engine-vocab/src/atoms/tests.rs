//! Tests for [`super`] — carved out of `atoms.rs` unchanged (ARCH §3.1:
//! the file was 2033 lines). Mechanical move only: no assertion, fixture
//! or name differs from the inline module it replaces.

use super::*;

#[test]
fn atom_type_is_a_closed_set_that_round_trips() {
    // `AtomType::ALL` and the enum are two spellings of one closed set;
    // this pins them together. A variant added to the enum and not to ALL
    // fails on the length assert — which is the only reason ALL is safe to
    // iterate as "every kind".
    assert_eq!(AtomType::ALL.len(), 12);
    let mut seen: Vec<String> = Vec::new();
    for t in AtomType::ALL {
        let v = serde_json::to_value(t).unwrap();
        let tag = v
            .as_str()
            .expect("AtomType serialises as a string")
            .to_string();
        assert_eq!(serde_json::from_value::<AtomType>(v).unwrap(), t);
        seen.push(tag);
    }
    seen.sort();
    let n = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), n, "two AtomType variants share an on-disk tag");
}

#[test]
fn atom_type_accessor_agrees_with_the_on_disk_tag() {
    // `AtomEnvelope` serialises with `#[serde(tag = "atom_type")]`, so the
    // JSON tag and `atom_type()` are two renderings of one closed set.
    // Verified red-first: flipping the Entity arm of `atom_type()` to
    // `AtomType::Event` fails this assert before it fails anything else.
    let env = AtomEnvelope::Entity(sample_entity());
    let tag = serde_json::to_value(&env).unwrap()["atom_type"]
        .as_str()
        .expect("every envelope serialises with an atom_type tag")
        .to_string();
    assert_eq!(env.atom_type(), AtomType::Entity);
    assert_eq!(
        serde_json::to_value(env.atom_type()).unwrap().as_str(),
        Some(tag.as_str()),
        "AtomType and the envelope tag serialise differently"
    );
}

#[test]
fn display_name_truncates_prose_kinds_only() {
    // The four canonical accessors replaced eleven hand-copied fan-outs
    // across four crates. This is the behaviour those copies had to agree
    // on and now cannot disagree about: a NAME is never clipped, PROSE is.
    let long = "x".repeat(50);

    let event = AtomEnvelope::Event(Event {
        attributes: Default::default(),
        id: AtomId::event(1),
        description: long.clone(),
        event_type: EventType::Action,
        participants: Vec::new(),
        evidence: Vec::new(),
        section_position: SectionPosition::section("sec_0001"),
        causal_antecedents: Vec::new(),
        enrichment_depth: EnrichmentDepth::Structural,
    });
    assert_eq!(event.display_name(Some(8)).chars().count(), 9); // 8 + ellipsis
    assert_eq!(event.display_name(None), long);
    assert_eq!(event.salience(), None, "Event carries no scalar score");

    let entity = AtomEnvelope::Entity(Entity {
        canonical_name: long.clone(),
        ..sample_entity()
    });
    assert_eq!(
        entity.display_name(Some(8)),
        long,
        "a name is never clipped"
    );
    assert_eq!(entity.salience(), Some(0.1));
}

#[test]
fn atom_id_constructors_produce_zero_padded_ids() {
    assert_eq!(AtomId::entity(1).as_str(), "entity-0001");
    assert_eq!(AtomId::event(42).as_str(), "event-0042");
    assert_eq!(AtomId::state(7).as_str(), "state-0007");
}

// ── Move 6: content-hash atom id stability tests ──────

#[test]
fn entity_content_hash_is_stable_across_calls() {
    let a = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
    let b = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
    assert_eq!(a, b);
    assert_eq!(a.as_str().len(), "entity-".len() + 16);
    assert!(a.is_content_hash());
}

#[test]
fn entity_content_hash_normalises_canonical_name() {
    // Lookup_key normalises case + punctuation; same key → same id.
    let a = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
    let b = AtomId::entity_content_hash("ALBERT-EINSTEIN", &EntityType::Person, "wikipedia");
    assert_eq!(a, b);
}

#[test]
fn entity_content_hash_differs_across_corpora() {
    let a = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "wikipedia");
    let b = AtomId::entity_content_hash("Albert Einstein", &EntityType::Person, "sep");
    assert_ne!(a, b);
}

#[test]
fn entity_content_hash_differs_across_types() {
    let a = AtomId::entity_content_hash("Mercury", &EntityType::Place, "wikipedia");
    let b = AtomId::entity_content_hash("Mercury", &EntityType::Concept, "wikipedia");
    assert_ne!(a, b);
}

/// The reason [`AtomId::exact_entity_content_hash`] exists, stated as the
/// input that made the folded constructor wrong: two REAL, distinct
/// Wikipedia articles whose titles differ only in the case of one letter.
/// Under `entity_content_hash` they are one atom, which is a silent merge
/// of two encyclopedia pages; under the exact constructor they are two.
#[test]
fn exact_entity_content_hash_keeps_two_wikipedia_titles_that_differ_only_in_case_apart() {
    let ty = EntityType::Other("article".to_string());
    // The pair the full wikipedia rebuild actually died on.
    let lower = AtomId::exact_entity_content_hash("Jigsaw puzzle", &ty, "wikipedia");
    let upper = AtomId::exact_entity_content_hash("Jigsaw Puzzle", &ty, "wikipedia");
    assert_ne!(lower, upper, "two distinct articles must be two atoms");
    // And the folded constructor is the thing that could not tell them
    // apart — asserted, so this test fails loudly if `lookup_key` ever
    // stops folding and the exact constructor becomes redundant.
    assert_eq!(
        AtomId::entity_content_hash("Jigsaw puzzle", &ty, "wikipedia"),
        AtomId::entity_content_hash("Jigsaw Puzzle", &ty, "wikipedia"),
    );
}

/// The two families never collide, and the id SHAPE is unchanged so
/// everything that routes on the `entity-` prefix keeps working.
#[test]
fn exact_and_folded_entity_ids_are_disjoint_but_the_same_shape() {
    let ty = EntityType::Person;
    let exact = AtomId::exact_entity_content_hash("Albert Einstein", &ty, "wikipedia");
    let folded = AtomId::entity_content_hash("Albert Einstein", &ty, "wikipedia");
    // Same name, same type, same corpus, DIFFERENT derivation: the domain
    // tag is what keeps a store built one way from resolving ids minted
    // the other way by accident.
    assert_ne!(exact, folded);
    assert!(exact.as_str().starts_with("entity-"));
    assert_eq!(exact.as_str().len(), "entity-".len() + 16);
}

/// Length framing, not the `|` separator, is what makes the exact
/// constructor unambiguous — the property `lookup_key` supplied for free
/// to the folded one. Both of these hash to the same string under a naive
/// `"{name}|{ty}|{corpus}"` join.
#[test]
fn exact_entity_content_hash_cannot_be_confused_across_field_boundaries() {
    let ty = EntityType::Other("t".to_string());
    assert_ne!(
        AtomId::exact_entity_content_hash("a|t|b", &ty, "c"),
        AtomId::exact_entity_content_hash("a", &ty, "b|c"),
    );
    // Colons frame the lengths; a name that contains one is still safe.
    assert_ne!(
        AtomId::exact_entity_content_hash("1:x", &ty, "c"),
        AtomId::exact_entity_content_hash("x", &ty, "c"),
    );
}

/// Stable and corpus-qualified, exactly as the folded constructor is:
/// these are the properties the wiki store cites when it says a peer can
/// compute an id with no registry.
#[test]
fn exact_entity_content_hash_is_stable_and_corpus_qualified() {
    let ty = EntityType::Other("article".to_string());
    let a = AtomId::exact_entity_content_hash("Roman Empire", &ty, "wikipedia");
    assert_eq!(
        a,
        AtomId::exact_entity_content_hash("Roman Empire", &ty, "wikipedia")
    );
    assert_ne!(
        a,
        AtomId::exact_entity_content_hash("Roman Empire", &ty, "wikipedia-fetched")
    );
    assert_ne!(
        a,
        AtomId::exact_entity_content_hash("Roman Republic", &ty, "wikipedia")
    );
    assert_ne!(
        a,
        AtomId::exact_entity_content_hash("Roman Empire", &EntityType::Place, "wikipedia")
    );
}

#[test]
fn relation_content_hash_ignores_participant_order() {
    let p1 = AtomId::entity_content_hash("Alice", &EntityType::Person, "c");
    let p2 = AtomId::entity_content_hash("Bob", &EntityType::Person, "c");
    let a = AtomId::relation_content_hash(
        &[p1.clone(), p2.clone()],
        &crate::taxonomy::RelationType::Interpersonal,
        "married_to",
        "c",
    );
    let b = AtomId::relation_content_hash(
        &[p2, p1],
        &crate::taxonomy::RelationType::Interpersonal,
        "married_to",
        "c",
    );
    assert_eq!(a, b);
}

#[test]
fn sequential_ids_are_not_content_hash() {
    assert!(!AtomId::entity(1).is_content_hash());
    assert!(!AtomId::event(42).is_content_hash());
}

#[test]
fn content_hash_ids_pass_is_content_hash_check() {
    let id = AtomId::entity_content_hash("Test", &EntityType::Person, "c");
    assert!(id.is_content_hash());
}

#[test]
fn all_atom_variants_have_content_hash_constructors() {
    // Smoke that every variant compiles + emits a content-hash-shaped id.
    use crate::taxonomy::*;
    let parent = AtomId::entity_content_hash("e", &EntityType::Person, "c");
    let ids = vec![
        AtomId::entity_content_hash("e", &EntityType::Person, "c"),
        AtomId::event_content_hash("d", &EventType::Action, "s0", "c"),
        AtomId::state_content_hash(&parent, &StateType::Epistemic, "l", "c"),
        AtomId::relation_content_hash(
            std::slice::from_ref(&parent),
            &RelationType::Interpersonal,
            "l",
            "c",
        ),
        AtomId::claim_content_hash("c", &DiscourseAct::Assert, &EpistemicStatus::Confident, "c"),
        AtomId::question_content_hash("q", &QuestionType::Thematic, "c"),
        AtomId::configuration_content_hash("cfg", "c"),
        AtomId::argument_reconstruction_content_hash("arg", "c"),
        AtomId::position_content_hash("pos", "endorse", "c"),
        AtomId::opposition_content_hash("X vs Y", "c"),
    ];
    for id in &ids {
        assert!(
            id.is_content_hash(),
            "expected content-hash shape: {}",
            id.as_str()
        );
    }
    // All distinct (different prefixes + different inputs).
    let mut sorted: Vec<&str> = ids.iter().map(|a| a.as_str()).collect();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "ids must be distinct");
}

#[test]
fn entity_content_hash_handles_unicode() {
    let a = AtomId::entity_content_hash("Søren Kierkegaard", &EntityType::Person, "sep");
    assert!(a.is_content_hash());
}

#[test]
fn entity_atom_roundtrips_through_envelope() {
    let entity = Entity {
        id: AtomId::entity(1),
        canonical_name: "Alyosha".into(),
        aliases: vec!["Alexei Fyodorovich".into()],
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0004", Some("the third son".into())),
        description: "Youngest Karamazov brother; novice at the monastery.".into(),
        defining_quote: None,
        salience: 0.92,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    };
    let env = AtomEnvelope::Entity(entity.clone());
    let json = serde_json::to_string(&env).unwrap();
    // Pin the on-disk shape — atom_type as PascalCase, data nested.
    assert!(json.contains("\"atom_type\":\"Entity\""));
    assert!(json.contains("\"canonical_name\":\"Alyosha\""));
    let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
    match back {
        AtomEnvelope::Entity(e) => {
            assert_eq!(e.canonical_name, entity.canonical_name);
            assert_eq!(e.salience, entity.salience);
            assert_eq!(e.enrichment_depth, EnrichmentDepth::Extracted);
        }
        _ => panic!("expected Entity variant"),
    }
}

#[test]
fn event_atom_roundtrips_with_participants() {
    let event = Event {
        attributes: Default::default(),
        id: AtomId::event(1),
        description: "Zosima instructs Alyosha to leave the monastery.".into(),
        event_type: EventType::Decision,
        participants: vec![AtomId::entity(1), AtomId::entity(2)],
        evidence: vec![ChunkRef::new(
            "sec_0013",
            Some("go out into the world".into()),
        )],
        section_position: SectionPosition::section("sec_0013"),
        causal_antecedents: Vec::new(),
        enrichment_depth: EnrichmentDepth::Extracted,
    };
    let env = AtomEnvelope::Event(event);
    let json = serde_json::to_string(&env).unwrap();
    assert!(json.contains("\"atom_type\":\"Event\""));
    let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
    match back {
        AtomEnvelope::Event(e) => {
            assert_eq!(e.participants.len(), 2);
            assert_eq!(e.section_position.section_id, "sec_0013");
        }
        _ => panic!("expected Event variant"),
    }
}

#[test]
fn state_atom_roundtrips_with_entity_id_and_section_range() {
    use crate::taxonomy::StateType;
    let state = State {
        id: AtomId::state(17),
        entity_id: AtomId::entity(1),
        label: "Reluctant attraction — Jane watches Rochester with increasing intensity".into(),
        state_type: StateType::Psychological,
        evidence: vec![ChunkRef::new("ch015", None), ChunkRef::new("ch017", None)],
        section_range: SectionRange {
            start: "ch014".into(),
            end: "ch018".into(),
        },
        confidence: Some(0.82),
        enrichment_depth: EnrichmentDepth::Extracted,
    };
    let env = AtomEnvelope::State(state.clone());
    let json = serde_json::to_string(&env).unwrap();
    assert!(json.contains("\"atom_type\":\"State\""));
    assert!(json.contains("\"state_type\":\"psychological\""));
    let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
    match back {
        AtomEnvelope::State(s) => {
            assert_eq!(s.entity_id, state.entity_id);
            assert_eq!(s.confidence, Some(0.82));
        }
        _ => panic!("expected State variant"),
    }
}

#[test]
fn relation_atom_carries_participants_in_order() {
    use crate::taxonomy::RelationType;
    let relation = Relation {
        attributes: Default::default(),
        id: AtomId::relation(3),
        label: "Jane–Rochester: employer/dependent bond becoming mutual transformation".into(),
        participants: vec![AtomId::entity(1), AtomId::entity(2)],
        relation_type: RelationType::Interpersonal,
        evidence: Vec::new(),
        section_range: SectionRange {
            start: "ch012".into(),
            end: "ch038".into(),
        },
        enrichment_depth: EnrichmentDepth::Extracted,
    };
    let json = serde_json::to_string(&AtomEnvelope::Relation(relation.clone())).unwrap();
    assert!(json.contains("\"atom_type\":\"Relation\""));
    let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
    match back {
        AtomEnvelope::Relation(r) => {
            assert_eq!(r.participants, relation.participants);
            assert_eq!(r.relation_type, relation.relation_type);
        }
        _ => panic!("expected Relation"),
    }
}

#[test]
fn claim_atom_carries_discourse_act_and_epistemic_status() {
    use crate::taxonomy::{ClaimScope, DiscourseAct, EpistemicStatus};
    let claim = Claim {
        attributes: Default::default(),
        subject: None,
        id: AtomId::claim(42),
        content: "Active love costs more than dreamt love.".into(),
        discourse_act: DiscourseAct::Argue,
        epistemic_status: EpistemicStatus::Confident,
        scope: ClaimScope::Universal,
        evidence: vec![ChunkRef::new(
            "ch_5_p3",
            Some("love in dreams is greedy".into()),
        )],
        quotable_excerpt: None,
        attributed_to: Some(AtomId::entity(7)),
        confidence: Some(0.91),
        anchor: None,
        enrichment_depth: EnrichmentDepth::Extracted,
        claim_kind: None,
        concession_outcome: None,
        evidence_kind: None,
    };
    let json = serde_json::to_string(&AtomEnvelope::Claim(claim.clone())).unwrap();
    assert!(json.contains("\"discourse_act\":\"argue\""));
    assert!(json.contains("\"epistemic_status\":\"confident\""));
    assert!(json.contains("\"scope\":\"universal\""));
    let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
    match back {
        AtomEnvelope::Claim(c) => {
            assert_eq!(c.discourse_act, DiscourseAct::Argue);
            assert_eq!(c.attributed_to, claim.attributed_to);
        }
        _ => panic!("expected Claim"),
    }
}

#[test]
fn claim_anchor_round_trips_through_json() {
    // Pin the contract that `Claim.anchor` survives serialise →
    // deserialise. The drift-report renderer feeds `anchor` to
    // the cross-corpus fuzzy matcher; before this field existed
    // every normative claim landed in the critical "(no anchor)"
    // bucket because the matcher consulted the prose content
    // instead of the code symbol. This test exists so a future
    // refactor (e.g. switching the serde representation, dropping
    // the field "because it's optional") fails loudly here
    // rather than silently re-introducing the bug.
    use crate::taxonomy::{ClaimScope, DiscourseAct, EpistemicStatus};
    let claim = Claim {
        attributes: Default::default(),
        subject: None,
        id: AtomId::claim(7),
        content: "`open_index_for_corpus` always opens `<index_dir>/<corpus_id>`.".into(),
        discourse_act: DiscourseAct::Assert,
        epistemic_status: EpistemicStatus::Confident,
        scope: ClaimScope::Universal,
        evidence: vec![],
        quotable_excerpt: None,
        attributed_to: None,
        confidence: None,
        anchor: Some("open_index_for_corpus".into()),
        enrichment_depth: EnrichmentDepth::Extracted,
        claim_kind: None,
        concession_outcome: None,
        evidence_kind: None,
    };
    let json = serde_json::to_string(&AtomEnvelope::Claim(claim.clone())).unwrap();
    assert!(
        json.contains("\"anchor\":\"open_index_for_corpus\""),
        "anchor field must serialise into the JSON envelope, got: {json}"
    );

    let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
    match back {
        AtomEnvelope::Claim(c) => {
            assert_eq!(c.anchor.as_deref(), Some("open_index_for_corpus"));
        }
        _ => panic!("expected Claim"),
    }

    // Forward-compat: an atoms.json file written before this
    // field existed deserialises cleanly with `anchor = None`,
    // thanks to `#[serde(default)]`. Synthesise that legacy
    // shape and round-trip it.
    let legacy = r#"{"atom_type":"Claim","data":{
        "id":"claim-0001",
        "content":"legacy claim with no anchor field",
        "discourse_act":"assert",
        "epistemic_status":"confident",
        "scope":"fictional",
        "enrichment_depth":"extracted"
    }}"#;
    let parsed: AtomEnvelope = serde_json::from_str(legacy).unwrap();
    match parsed {
        AtomEnvelope::Claim(c) => assert!(c.anchor.is_none()),
        _ => panic!("expected Claim from legacy atoms.json shape"),
    }

    // And: when anchor is None we DO NOT emit the key (so the
    // file size doesn't grow for pre-engineering-atlas pipelines
    // that never set an anchor).
    let no_anchor = Claim {
        attributes: Default::default(),
        subject: None,
        id: AtomId::claim(8),
        content: "no-anchor claim".into(),
        discourse_act: DiscourseAct::Assert,
        epistemic_status: EpistemicStatus::Confident,
        scope: ClaimScope::Fictional,
        evidence: vec![],
        quotable_excerpt: None,
        attributed_to: None,
        confidence: None,
        anchor: None,
        enrichment_depth: EnrichmentDepth::Extracted,
        claim_kind: None,
        concession_outcome: None,
        evidence_kind: None,
    };
    let json = serde_json::to_string(&AtomEnvelope::Claim(no_anchor)).unwrap();
    assert!(
        !json.contains("\"anchor\""),
        "anchor=None must skip serialisation, got: {json}"
    );
}

#[test]
fn question_atom_resolution_status_variants_roundtrip() {
    use crate::taxonomy::QuestionType;
    for status in [
        ResolutionStatus::Resolved {
            claim_id: AtomId::claim(1),
        },
        ResolutionStatus::Contested {
            claim_ids: vec![AtomId::claim(1), AtomId::claim(2)],
        },
        ResolutionStatus::Open,
        ResolutionStatus::Dissolved,
    ] {
        let q = Question {
            id: AtomId::question(1),
            content: "Can authentic feeling survive contact with social reality?".into(),
            question_type: QuestionType::Thematic,
            addressed_by: Vec::new(),
            raised_at: Vec::new(),
            resolution_status: status,
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let json = serde_json::to_string(&AtomEnvelope::Question(q.clone())).unwrap();
        let back: AtomEnvelope = serde_json::from_str(&json).unwrap();
        match back {
            AtomEnvelope::Question(r) => {
                assert_eq!(r.content, q.content);
            }
            _ => panic!("expected Question"),
        }
    }
}

#[test]
fn configuration_atom_requires_interpretive_note() {
    let cfg = Configuration {
        id: AtomId::configuration(1),
        label: "Anna's descent mirrored against Levin's ascent, arguing authentic \
               life requires participation in something beyond individual desire."
            .into(),
        description: "Two trajectories with no shared characters after Part 1, \
                     structurally mirrored."
            .into(),
        constituent_atoms: vec![AtomId::entity(1), AtomId::entity(2)],
        evidence: Vec::new(),
        confidence: 0.71,
        interpretive_note: "Alternative reading: the parallel is ironic rather than \
                           argumentative. We extract the parallel-as-argument reading \
                           as primary but flag the ironic reading as live."
            .into(),
        enrichment_depth: EnrichmentDepth::Extracted,
    };
    let env = AtomEnvelope::Configuration(cfg.clone());
    // Every atom type exposes id() and enrichment_depth() without match.
    assert_eq!(env.id().as_str(), "config-0001");
    assert_eq!(env.enrichment_depth(), EnrichmentDepth::Extracted);
    let json = serde_json::to_string(&env).unwrap();
    assert!(json.contains("\"atom_type\":\"Configuration\""));
    assert!(json.contains("interpretive_note"));
}

#[test]
fn atoms_file_serialises_with_schema_version() {
    let file = AtomsFile::new(vec![]);
    let json = serde_json::to_string(&file).unwrap();
    // SCHEMA_VERSION is the source of truth — the test pins
    // whatever the current value is so a future bump updates this
    // assertion automatically. The shape of `atoms` is what we
    // actually want to assert.
    let expected_ver = format!("\"schema_version\":\"{}\"", AtomsFile::SCHEMA_VERSION);
    assert!(
        json.contains(&expected_ver),
        "{json} should contain {expected_ver}"
    );
    assert!(json.contains("\"atoms\":[]"));
}

/// One Entity atom, shared by the accessor tests below.
fn sample_entity() -> Entity {
    Entity {
        id: AtomId::entity(5),
        canonical_name: "X".into(),
        aliases: Vec::new(),
        entity_type: EntityType::Concept,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: "x".into(),
        defining_quote: None,
        salience: 0.1,
        enrichment_depth: EnrichmentDepth::Structural,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }
}

#[test]
fn atom_envelope_exposes_id_and_depth_without_matching() {
    let env = AtomEnvelope::Entity(sample_entity());
    assert_eq!(env.id().as_str(), "entity-0005");
    assert_eq!(env.enrichment_depth(), EnrichmentDepth::Structural);
}
