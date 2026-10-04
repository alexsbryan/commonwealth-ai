// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 3b — the resolve step itself, and the token-overlap rules it matches on.
//!
//! Fixtures live in `tests_fixtures.rs`; the name-to-atom resolvers in
//! `tests_matching.rs`; trajectories and typed extensions in their own files.
//! Split at these seams, not by line count, when the test module left
//! `resolution.rs` (ARCH §3.1/§3.2).

use super::tests_fixtures::*;
use super::*;
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntitySketch, EntityType, EventSketch};
use std::sync::Arc;

#[test]
fn levenshtein_matches_expected_values() {
    // Pin the DP against well-known textbook pairs plus a few
    // names that drive resolution rule 2.
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("kitten", "sitting"), 3);
    // ivan → ilya: i stays, v→l, a→y, n→a = 3 edits.
    assert_eq!(levenshtein("ivan", "ilya"), 3);
    // "ivan" vs "iván" (transliteration with accented character)
    // would be 1 — we keep rule 2's cap at 2 to leave headroom.
    assert_eq!(levenshtein("ivan", "ivn"), 1);
}

#[test]
fn step_3b_resolves_state_relation_claim_question_atoms_from_sketches() {
    use super::super::atoms::{AtomId, ChunkRef, Entity};
    use crate::enrichment::pipeline::atlas::{
        ClaimSketch, DiscourseAct, EnrichmentDepth, EntitySketch, EntityStateSketch, EntityType,
        EpistemicStatus, QuestionSketch, RelationSketch, RelationStateSketch,
    };

    // Build two canonical entities from (simulated) Step 3a.
    let entities = vec![
        Entity {
            id: AtomId::entity(1),
            canonical_name: "Alyosha".into(),
            aliases: vec!["Alexei Fyodorovich".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "Youngest Karamazov.".into(),
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
            description: "Monastery elder.".into(),
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

    // Two sections, in order. sec_0001 introduces a state +
    // relation + claim + question; sec_0002 develops the state
    // further (so Transition edges fire).
    let sections = vec![
        SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_introduced: vec![EntitySketch {
                attributes: Default::default(),
                canonical_name: "Alyosha".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                description: "".into(),
                defining_quote: None,
                anchor: String::new(),
            }],
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "Eager attention at the elder's feet".into(),
                anchor: "knelt at Zossima's feet".into(),
                state_type: None,
            }],
            relations_introduced: vec![RelationSketch {
                attributes: Default::default(),
                relation_type: None,
                participants: vec!["Alyosha".into(), "Zossima".into()],
                label: "Novice-elder bond".into(),
                anchor: "laid his hand on Alyosha's head".into(),
            }],
            relations_developed: vec![RelationStateSketch {
                participants: vec!["Alyosha".into(), "Zossima".into()],
                label: "Formation through blessing".into(),
                anchor: "blessed the novice".into(),
                state_type: None,
            }],
            events: vec![],
            claims: vec![ClaimSketch {
                attributes: Default::default(),
                claim_kind: None,
                subject: None,
                scope: None,
                content: "Active love costs more than dreamt love.".into(),
                discourse_act: DiscourseAct::Argue,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: Some("Zossima".into()),
                anchor: "love in dreams is greedy".into(),
                quotable_excerpt: None,
            }],
            questions_raised: vec![QuestionSketch {
                content: "Can a faith formed in the cell survive the world?".into(),
                anchor: "faith in the cell".into(),
            }],
            argument_reconstructions: Vec::new(),
            type_extension: None,
            type_extensions: Vec::new(),
        },
        SectionExtraction {
            section_id: "sec_0002".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "Resolve to leave the monastery".into(),
                anchor: "must go out into the world".into(),
                state_type: None,
            }],
            ..Default::default()
        },
    ];

    let out = resolve_step_3b(&sections, &entities, &[]).unwrap();

    // 2 states from entities_developed (Alyosha × 2) + 1 state
    // from relations_developed (Alyosha↔Zossima) = 3 states.
    assert_eq!(out.states.len(), 3);
    // 1 Relation introduced.
    assert_eq!(out.relations.len(), 1);
    assert_eq!(out.relations[0].participants.len(), 2);
    // 1 Claim, attributed to entity-0002 (Zossima).
    assert_eq!(out.claims.len(), 1);
    assert_eq!(out.claims[0].attributed_to, Some(AtomId::entity(2)));
    // 1 Question, resolution_status defaults to Open.
    assert_eq!(out.questions.len(), 1);
    assert!(matches!(
        out.questions[0].resolution_status,
        super::super::atoms::ResolutionStatus::Open
    ));

    // Trajectory index carries Alyosha (2 states → 1 transition)
    // and the Alyosha↔Zossima relation (1 state → 0 transitions).
    let alyosha_traj = out
        .trajectories
        .get(AtomId::entity(1).as_str())
        .expect("Alyosha trajectory");
    assert_eq!(alyosha_traj.atom_type, "Entity");
    assert_eq!(alyosha_traj.states.len(), 2);
    assert_eq!(alyosha_traj.transitions.len(), 1);
    // States ordered by section (sec_0001 before sec_0002).
    assert_eq!(alyosha_traj.states[0].section_range.start, "sec_0001");
    assert_eq!(alyosha_traj.states[1].section_range.start, "sec_0002");

    let relation_id = &out.relations[0].id;
    let relation_traj = out
        .trajectories
        .get(relation_id.as_str())
        .expect("relation trajectory");
    assert_eq!(relation_traj.atom_type, "Relation");
    assert_eq!(relation_traj.states.len(), 1);

    // Edges: 2 Involves (state→Alyosha) + 2 Grounds (state
    // evidence) + 2 Involves (relation→each participant) + 1
    // Involves (state→relation) + 1 Grounds (relation state
    // evidence) + 1 Involves (claim→Zossima) + 1 Grounds
    // (claim evidence) + 1 Transition (Alyosha state chain).
    // Exact count is 11 for this fixture; pin the categories
    // rather than the total so a future reorder of edge
    // emission doesn't break the test.
    let count = |t: EdgeType| out.edges.iter().filter(|e| e.edge_type == t).count();
    assert!(count(EdgeType::Involves) >= 5); // state→entity×2, rel→participant×2, claim→attributed_to
    assert!(count(EdgeType::Grounds) >= 3); // state evidence + claim evidence
    assert_eq!(count(EdgeType::Transition), 1);
}

#[test]
fn step_3b_drops_sketches_with_unknown_entity_names() {
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityStateSketch};

    let entities = vec![super::super::atoms::Entity {
        id: super::super::atoms::AtomId::entity(1),
        canonical_name: "Alyosha".into(),
        aliases: Vec::new(),
        entity_type: crate::enrichment::pipeline::atlas::EntityType::Person,
        first_appearance: super::super::atoms::ChunkRef::new("sec_0001", None),
        description: "x".into(),
        defining_quote: None,
        salience: 1.0,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }];

    let sections = vec![SectionExtraction {
        section_id: "sec_0001".into(),
        enrichment_depth: EnrichmentDepth::Extracted,
        entities_developed: vec![
            EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "real state".into(),
                anchor: String::new(),
                state_type: None,
            },
            EntityStateSketch {
                entity_name: "Mystery Character".into(),
                label: "orphan state".into(),
                anchor: String::new(),
                state_type: None,
            },
        ],
        ..Default::default()
    }];

    let out = resolve_step_3b(&sections, &entities, &[]).unwrap();
    // Only the resolvable state lands; the orphan drops.
    assert_eq!(out.states.len(), 1);
    assert_eq!(out.states[0].label, "real state");
}

#[test]
fn shared_token_overlap_merges_mixed_encoding_tokens() {
    // The real-world bug Phase 3a hit: `Karamазов` with a few
    // Cyrillic chars mid-word vs `Karamazov` pure Latin. After
    // transliteration both fold to `karamazov` and count as 1
    // shared token, so with a matching first name ("Fyodor")
    // rule 3 fires (≥2 shared tokens).
    assert_eq!(
        shared_token_overlap("Fyodor Karamазов", "Fyodor Pavlovich Karamazov"),
        2
    );
}

#[test]
fn first_token_matches_allows_exact_and_fuzzy_firstname_drift() {
    assert!(first_token_matches("Fyodor Karamazov", "Fyodor Pavlovitch"));
    assert!(first_token_matches(
        "Alyosha Karamazov",
        "Alyoshá Karámázov"
    )); // diacritic strip folds both first tokens
    assert!(first_token_matches("Alexey Karamazov", "Alexei Karamazov")); // Lev 1
    assert!(!first_token_matches(
        "Alexei Fyodorovic Karamazov",
        "Dmitri Fyodorovic Karamazov"
    )); // different first names must not match
    assert!(!first_token_matches("Ivan", "Ilya")); // 4-char guard — below fuzzy floor
}

// ── Landing 5 — Rule 3.5 single-long-token + cosine ─────

#[test]
fn shared_long_token_count_only_counts_tokens_of_minimum_length() {
    // `the`, `and` — below FUZZY_TOKEN_MIN_LEN=5, don't count.
    // Long tokens do.
    assert_eq!(
        shared_long_token_count("The Fyodor Karamazov", "The Fyodor Bank"),
        1 // only "fyodor" is long enough AND shared
    );
    // Two long shared tokens: "alexei" (6) + "karamazov" (9).
    assert_eq!(
        shared_long_token_count("Alexei Fyodorovich Karamazov", "Alexei Petrovich Karamazov"),
        2
    );
    // Single shared long token: the Fyodor drift case.
    assert_eq!(
        shared_long_token_count("Fyodor Pavlovich Karamazov", "Fyodor Karazov"),
        1
    );
    // No long tokens shared — all short tokens would be
    // filtered out.
    assert_eq!(shared_long_token_count("the end", "the top"), 0);
}

#[test]
fn rule_3_requires_first_token_match_to_prevent_sibling_collapse() {
    // Two siblings share patronymic + surname exactly (2 shared
    // tokens, above the rule 3 threshold) but differ in first
    // name. Pre-guard, shared_token_overlap alone would have
    // merged them — the first-token-matches guard blocks this.
    // Inputs are real Brothers Karamazov names so future
    // regressions on this specific case surface loudly.
    assert_eq!(
        shared_token_overlap(
            "Alexei Fyodorovich Karamazov",
            "Dmitri Fyodorovich Karamazov"
        ),
        2
    );
    assert!(!first_token_matches(
        "Alexei Fyodorovich Karamazov",
        "Dmitri Fyodorovich Karamazov"
    ));
}

#[test]
fn shared_token_overlap_counts_lev1_fuzzy_matches_on_long_tokens() {
    // Landing 2 observation: Phase 3a rule 3 was failing to
    // merge `Zossima` ↔ `Elder Zósima` (one-char drop after
    // diacritic strip) because exact-token overlap = 0 even
    // though every shared token is one edit away. Fuzzy pass
    // at Lev ≤ 1 for tokens ≥ 5 chars catches this while
    // staying conservative enough to keep distinct entities
    // distinct (below).
    assert_eq!(
        shared_token_overlap("Zossima", "Elder Zósima"),
        1,
        "zossima ↔ zosima (Lev 1) should count as a fuzzy match"
    );
    assert_eq!(
        shared_token_overlap("Ivan Fyodoroič Kárámazov", "Iván Fyódorič Kárazòv"),
        // After fold + Lev-1 fuzzy:
        //   ivan (4 chars) — filtered by min-len 3 but below
        //   fuzzy guard 5, so must match exactly → does match.
        //   fyodoroic ↔ fyodoric (Lev 1) → fuzzy match.
        //   karamazov ↔ karazov (Lev 2) → NOT fuzzy at Lev 1.
        // Total: ivan + fyodoric = 2. Rule 3 (≥ 2) would fire.
        2,
    );
    // Distinct entities must stay apart — only one shared
    // long-token, no fuzzy headroom.
    assert_eq!(shared_token_overlap("Ivan Karamazov", "Ilya Karamazov"), 1,);
}

#[test]
fn shared_token_overlap_ignores_short_tokens() {
    // "of the house" vs "the house of" — tokens of length ≥ 3
    // are "the" and "house"; both appear in both strings.
    // Overlap = 2. Tokens of length < 3 ("of") are filtered.
    assert_eq!(shared_token_overlap("of the house", "the house of"), 2);
    // Russian patronymic: 2 long-enough tokens share.
    assert_eq!(
        shared_token_overlap("alexei fyodorovich karamazov", "alexei fyodorovich"),
        2
    );
    // Disjoint tokens share none.
    assert_eq!(
        shared_token_overlap("ivan karamazov", "alexei smerdyakov"),
        0
    );
}
