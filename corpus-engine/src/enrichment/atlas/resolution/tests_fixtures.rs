// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 3b test fixtures — embeds, section/entity/event builders, the head-noun corpus.
//!
//! Shared by the four `resolution` test files. One set of fixtures, not four:
//! the head-noun sections alone are 400 lines and every file that resolves a
//! name needs them.

use super::*;
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntitySketch, EntityType, EventSketch};
use std::sync::Arc;

/// Deterministic embed: first two letters seed a small 3-vector.
/// Same text → same vector; slightly different text → different
/// direction. Lets us pin cosine-threshold behaviour without
/// touching a model.
pub(super) fn fake_embed() -> EmbedFn {
    Arc::new(move |s: &str| {
        let s = s.to_string();
        Box::pin(async move {
            let bytes = s.as_bytes();
            let a = bytes.first().copied().unwrap_or(0) as f32;
            let b = bytes.get(1).copied().unwrap_or(0) as f32;
            let c = bytes.get(2).copied().unwrap_or(0) as f32;
            Ok(vec![a, b, c])
        })
    })
}

/// Embed that forces a specific cosine similarity — returns the
/// same vector for any input, so all comparisons land at 1.0.
pub(super) fn always_one_embed() -> EmbedFn {
    Arc::new(move |_s: &str| Box::pin(async move { Ok(vec![1.0_f32, 0.0, 0.0]) }))
}

pub(super) fn section(
    id: &str,
    entities: Vec<EntitySketch>,
    events: Vec<EventSketch>,
) -> SectionExtraction {
    SectionExtraction {
        section_id: id.into(),
        enrichment_depth: EnrichmentDepth::Extracted,
        entities_introduced: entities,
        entities_developed: Vec::new(),
        relations_introduced: Vec::new(),
        relations_developed: Vec::new(),
        events,
        claims: Vec::new(),
        questions_raised: Vec::new(),
        argument_reconstructions: Vec::new(),
        type_extension: None,
        type_extensions: Vec::new(),
    }
}

pub(super) fn entity(name: &str, aliases: &[&str], description: &str) -> EntitySketch {
    EntitySketch {
        attributes: Default::default(),
        canonical_name: name.into(),
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        entity_type: EntityType::Person,
        description: description.into(),
        anchor: String::new(),
        defining_quote: None,
    }
}

pub(super) fn event(desc: &str, participants: &[&str]) -> EventSketch {
    EventSketch {
        attributes: Default::default(),
        event_type: None,
        description: desc.into(),
        participants: participants.iter().map(|s| s.to_string()).collect(),
        anchor: String::new(),
    }
}

#[tokio::test]
async fn atlas_resolve_merges_alias_variants_into_single_entity() {
    // Rule 1: alias match. Sketch in sec_0002 has canonical_name
    // "Alyosha" which appears in sec_0001's aliases.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity(
                "Alexei Fyodorovich Karamazov",
                &["Alyosha", "Alexey"],
                "The youngest Karamazov brother.",
            )],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity("Alyosha", &[], "Novice at the monastery.")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        1,
        "alias match should merge into a single entity"
    );
    // Merged entity keeps the original canonical_name, picks up
    // the richer description, and unions aliases.
    let e = &out.entities[0];
    assert_eq!(e.canonical_name, "Alexei Fyodorovich Karamazov");
    assert!(e.aliases.iter().any(|a| a == "Alyosha"));
}

#[tokio::test]
async fn atlas_resolve_keeps_distinct_entities_with_similar_names_and_different_descriptions() {
    // Rule 2 requires BOTH Levenshtein ≤ 2 AND cosine ≥ 0.90.
    // "Ivan" and "Ilya" are Levenshtein 2 apart but the fake
    // embed gives them different directions → cosine ≈ 0.98-ish
    // actually since the vectors are (i,v,a) and (i,l,y)... the
    // first character matches, the rest don't. We force the
    // cosine gap by using descriptions that differ in the first
    // byte.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity(
                "Ivan",
                &[],
                "One distinct description starting with O.",
            )],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity(
                "Ilya",
                &[],
                "A different description starting with A.",
            )],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        2,
        "Ivan and Ilya must stay distinct when descriptions diverge"
    );
}

#[tokio::test]
async fn atlas_resolve_merges_russian_patronymic_variants_via_shared_tokens() {
    // Rule 3: ≥ 2 shared tokens of length ≥ 3 after case-folding.
    // "Alexei Fyodorovich Karamazov" and "Alexei Fyodorovich"
    // share "alexei" and "fyodorovich" — 2 tokens, qualifies.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity(
                "Alexei Fyodorovich Karamazov",
                &[],
                "Youngest brother.",
            )],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity("Alexei Fyodorovich", &[], "The novice.")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        1,
        "shared-token overlap should merge patronymic variants"
    );
    assert!(out.entities[0]
        .aliases
        .iter()
        .any(|a| a == "Alexei Fyodorovich"));
}

#[tokio::test]
async fn atlas_resolve_rule_3_bounds_cross_section_matching_to_window() {
    // Rule 3 (shared-token overlap) requires ≥ 2 shared tokens
    // AND stays within the 5-section lookback. Pick a name pair
    // that (a) doesn't substring-match — so rule 4 stays out —
    // and (b) would match via rule 3 if the window allowed.
    // "Fyodorovich Alexei" and "Alexei Fyodorovich Karamazov"
    // share "alexei" + "fyodorovich" (2 tokens, both len ≥ 5)
    // but neither is a substring of the other.
    let mut sections = Vec::new();
    sections.push(section(
        "sec_0001",
        vec![entity("Fyodorovich Alexei", &[], "Youngest brother.")],
        vec![],
    ));
    for i in 2..=9 {
        sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
    }
    sections.push(section(
        "sec_0010",
        vec![entity("Alexei Fyodorovich Karamazov", &[], "The novice.")],
        vec![],
    ));
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        2,
        "rule 3 shouldn't cross the 5-section window without alias evidence"
    );
}

#[tokio::test]
async fn atlas_resolve_rule_4_substring_crosses_any_section_distance() {
    // Rule 4 (substring match, len ≥ 5) is a stronger signal
    // than shared-token overlap and is allowed to cross the
    // lookback window. Covers the common fragmentation of
    // `Alyosha Karamazov` (earlier section) ↔ `Alyosha` (much
    // later) that blocked Step 3b trajectory construction on
    // real Brothers Karamazov data.
    let mut sections = Vec::new();
    sections.push(section(
        "sec_0001",
        vec![entity("Alyosha Karamazov", &[], "Youngest brother.")],
        vec![],
    ));
    for i in 2..=19 {
        sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
    }
    sections.push(section(
        "sec_0020",
        vec![entity("Alyosha", &[], "")],
        vec![],
    ));
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        1,
        "substring match with shorter ≥ 5 chars must cross any section distance"
    );
    assert!(out.entities[0]
        .aliases
        .iter()
        .any(|a| a.eq_ignore_ascii_case("Alyosha")));
}

#[tokio::test]
async fn atlas_resolve_rule_4_respects_whole_word_boundary() {
    // `Ivan` is a substring of `Ivanovich` byte-wise but not a
    // whole-word match. Rule 4 must honour word boundaries so
    // it doesn't false-merge a short-named character into a
    // longer patronymic.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity("Ivanovich", &[], "Distinct character A.")],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity("Ivan", &[], "Distinct character B.")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    // Though "Ivan" is 4 chars (below the 5-char floor anyway),
    // this test also pins the behavior should the floor ever
    // drop: "Ivan" would still not substring-merge into
    // "Ivanovich" because of whole-word guard.
    assert_eq!(out.entities.len(), 2);
}

#[tokio::test]
async fn atlas_resolve_rule_4_merges_title_prefix_names() {
    // "Zossima" vs "Father Zossima" — zossima (7 chars, len ≥ 5)
    // is a whole-word substring of "father zossima". Real
    // smoke-test case: Phase 1 extracted both forms from
    // different chapters and they fragmented before rule 4.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity("Zossima", &[], "Monastery elder.")],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity("Father Zossima", &[], "The elder blesses Alyosha.")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(out.entities.len(), 1);
}

/// The spike-3 fine-print declaration, reduced to the one type the
/// head-noun merge destroyed. `identity` is the only axis these two
/// tests differ on — the shape at
/// `research/ontology-retrieval/spikes/extraction-census/recipe.toml:98`
/// declares `recipient` with NO identity key.
pub(super) fn recipient_policies(
    identity: &[&str],
) -> crate::enrichment::ontology::OntologyPolicies {
    use crate::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl, ShapePolicy, TypeKind};
    OntologyPolicies {
        shape: ShapePolicy {
            types: vec![OntologyTypeDecl {
                name: "recipient".into(),
                kind: TypeKind::Entity,
                identity: identity.iter().map(|k| (*k).to_string()).collect(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(super) fn recipient(name: &str, description: &str) -> EntitySketch {
    let mut s = entity(name, &[], description);
    s.entity_type = EntityType::Other("recipient".into());
    s
}

/// The bare head noun first, its three qualified forms in a later
/// section — the order the Spotify policy presents them in.
pub(super) fn head_noun_sections() -> Vec<SectionExtraction> {
    vec![
        section(
            "sec_0001",
            vec![recipient(
                "partners",
                "Third parties Spotify shares data with.",
            )],
            vec![],
        ),
        section(
            "sec_0002",
            vec![
                recipient("Authentication partners", "Verify a user's identity."),
                recipient("Payment partners", "Process subscription payments."),
                recipient("Advertising partners", "Serve and measure ads."),
            ],
            vec![],
        ),
    ]
}

async fn resolved_recipient_names(identity: &[&str]) -> Vec<String> {
    let p = recipient_policies(identity);
    let out = resolve_entities_and_events_with(
        &head_noun_sections(),
        &fake_embed(),
        &ResolutionPolicy::new(&p),
    )
    .await
    .unwrap();
    out.entities
        .iter()
        .map(|e| e.canonical_name.clone())
        .collect()
}

/// Rule 4 merges on whole-word CONTAINMENT, which is fuzzy evidence:
/// "partners" is inside "Payment partners" and is a different recipient.
/// Passing `MergeEvidence::Exact` skipped `merge_permitted` rule 3
/// entirely, so a type the author identified by an external key merged
/// anyway. Revert the call at rule 4 to `Exact` and this goes red with
/// one atom named "partners".
#[tokio::test]
async fn rule_4_containment_obeys_a_declared_identity_key() {
    let names = resolved_recipient_names(&["find_id"]).await;
    assert_eq!(names.len(), 4, "got {names:?}");
}

/// OPEN DEFECT, ignored rather than deleted so it cannot rot: a DECLARED
/// type with no identity key has nothing to refuse on — `merge_permitted`
/// rule 3 skips a keyless type by design (`resolution_identity.rs`, the
/// `keys.is_empty()` arm), so the bare head noun still absorbs all three
/// qualified forms. Spike 3 (2026-09-19, the 108-section fine-print
/// census): Spotify's recipient recall by canonical name was 4 of 13.
/// Which criterion should refuse this is a merge-policy fork for the
/// operator — `ralph/DECISIONS.md`, 2026-09-19. Run with
/// `--ignored` to watch it.
#[tokio::test]
#[ignore = "open defect: a keyless declared type has no head-noun refusal (ralph/DECISIONS.md 2026-09-19)"]
async fn a_bare_head_noun_does_not_absorb_its_qualified_forms() {
    let names = resolved_recipient_names(&[]).await;
    assert_eq!(
        names.len(),
        4,
        "a bare head noun must not absorb its qualified forms; got {names:?}"
    );
}

#[tokio::test]
async fn rule_3_5_merges_fyodor_drift_variants_via_single_long_token_and_high_cosine() {
    // The Landing 4 smoke-test residual: "Fyodor Pavlovich
    // Karamazov" and "Fyodor Karazov" describe the same
    // patriarch but only share one long token (`fyodor`) after
    // fold — `karazov` ↔ `karamazov` is Lev 2, above rule 3's
    // fuzzy cap. Rule 3.5 catches this when description cosine
    // ≥ 0.92. We use `always_one_embed` to force cosine = 1.0.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity(
                "Fyodor Pavlovich Karamazov",
                &[],
                "The Karamazov patriarch.",
            )],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity("Fyodor Karazov", &[], "The Karamazov patriarch.")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &always_one_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        1,
        "Rule 3.5 should merge Fyodor drift variants on single-token + cosine"
    );
}

#[tokio::test]
async fn rule_3_5_blocks_sibling_collapse_on_single_shared_surname() {
    // Alexei and Dmitri share only `karamazov` (surname) as a
    // long token; first_token_matches fails so rule 3.5 does
    // NOT fire even with identical descriptions. Protects the
    // sibling-distinction invariant.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity("Alexei Karamazov", &[], "A Karamazov brother.")],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity("Dmitri Karamazov", &[], "A Karamazov brother.")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &always_one_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        2,
        "Rule 3.5 must not collapse siblings sharing only surname + cosine"
    );
}

#[tokio::test]
async fn rule_3_5_sparse_path_merges_drift_variant_with_empty_description() {
    // The real-world residual from the Landing 4 smoke: the
    // sparse Fyodor drift has an empty description. Rule 3.5's
    // sparse path fires when first_token_matches + ≥ 1 shared
    // long token + exactly one side has no description.
    // first_token_matches still guards against sibling
    // collapse.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity(
                "Fyodor Pavlovich Karamazov",
                &[],
                "The Karamazov patriarch, wealthy landowner and provocateur.",
            )],
            vec![],
        ),
        section(
            "sec_0002",
            // Empty description — the actual condition that
            // tripped up rule 3.5's strict path in Landing 4.
            vec![entity("Fyodor Karazov", &[], "")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &always_one_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        1,
        "sparse drift variant must merge via rule 3.5 sparse path"
    );
}

#[tokio::test]
async fn rule_3_5_sparse_path_respects_first_token_guard() {
    // Same empty-description case but different first names —
    // rule 3.5 sparse path must NOT fire. Protects against
    // "Alexei Karamazov" vs bare "Dmitri" (or anyone else with
    // a sparse reference sharing only the surname).
    let sections = vec![
        section(
            "sec_0001",
            vec![entity("Alexei Karamazov", &[], "The youngest brother.")],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity("Dmitri Karamazov", &[], "")],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &always_one_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        2,
        "sparse path must not bypass the first_token_matches guard"
    );
}

#[tokio::test]
async fn rule_3_5_requires_high_cosine_even_with_shared_first_token() {
    // Same first token, same single long token, but DIFFERENT
    // descriptions → cosine below 0.92 → no merge. Protects
    // "two distinct Fyodors" who happen to share a surname-like
    // token.
    // `fake_embed` derives vectors from first bytes; crafting
    // descriptions that start with different characters makes
    // the cosine land well below 0.92.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity(
                "Fyodor Pavlovich Karamazov",
                &[],
                "A drunkard patriarch known for his vice and cunning.",
            )],
            vec![],
        ),
        section(
            "sec_0002",
            vec![entity(
                "Fyodor Karazov",
                &[],
                "Different saintly healer, helps pilgrims find their way.",
            )],
            vec![],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        2,
        "different descriptions must keep the two Fyodors distinct"
    );
}

#[tokio::test]
async fn atlas_resolve_cross_window_merges_when_alias_evidence_fires() {
    // Same setup as the boundary test but the later sketch
    // carries the earlier entity's name in its aliases — rule 1
    // crosses any distance.
    let mut sections = Vec::new();
    sections.push(section(
        "sec_0001",
        vec![entity(
            "Alexei Fyodorovich Karamazov",
            &[],
            "Youngest brother.",
        )],
        vec![],
    ));
    for i in 2..=9 {
        sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
    }
    sections.push(section(
        "sec_0010",
        vec![entity(
            "Alyosha",
            &["Alexei Fyodorovich Karamazov"],
            "The novice.",
        )],
        vec![],
    ));
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        1,
        "alias match should merge across any section distance"
    );
}

#[tokio::test]
async fn atlas_events_dedupe_within_adjacent_sections() {
    // Same event described near-identically in adjacent sections
    // merges into a single event atom with both evidence refs.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity("Alyosha", &[], "")],
            vec![event("Alyosha arrives at the monastery.", &["Alyosha"])],
        ),
        section(
            "sec_0002",
            vec![],
            vec![event("Alyosha arrives at the monastery.", &["Alyosha"])],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &always_one_embed())
        .await
        .unwrap();
    assert_eq!(
        out.events.len(),
        1,
        "high-similarity events in adjacent sections should dedupe"
    );
    assert_eq!(out.events[0].evidence.len(), 2);
}

#[tokio::test]
async fn atlas_events_stay_distinct_across_wide_section_gaps() {
    // The same event narrated again 10 sections later is worth
    // preserving — it's narrative repetition, not noise.
    let mut sections = Vec::new();
    sections.push(section(
        "sec_0001",
        vec![entity("Alyosha", &[], "")],
        vec![event("Alyosha arrives at the monastery.", &["Alyosha"])],
    ));
    for i in 2..=10 {
        sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
    }
    sections.push(section(
        "sec_0011",
        vec![],
        vec![event("Alyosha arrives at the monastery.", &["Alyosha"])],
    ));
    let out = resolve_entities_and_events(&sections, &always_one_embed())
        .await
        .unwrap();
    assert_eq!(
        out.events.len(),
        2,
        "events beyond the ±2-section dedupe window stay distinct"
    );
}

#[tokio::test]
async fn atlas_resolve_emits_involves_edges_for_event_participants() {
    let sections = vec![section(
        "sec_0001",
        vec![entity("Alyosha", &[], ""), entity("Zosima", &[], "")],
        vec![event("Zosima instructs Alyosha.", &["Zosima", "Alyosha"])],
    )];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(out.events.len(), 1);
    // One Involves edge per participant, source = event id.
    let involves: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Involves)
        .collect();
    assert_eq!(involves.len(), 2);
    let src = &out.events[0].id;
    assert!(involves.iter().all(|e| e.source == *src));
    assert_eq!(involves[0].provenance, EdgeProvenance::Derived);
}

#[tokio::test]
async fn atlas_resolve_salience_is_frequency_normalised() {
    // Alyosha appears in 3 sections, Zosima in 1. Salience is
    // a monotonic function of reference count; Alyosha > Zosima.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity("Alyosha", &[], ""), entity("Zosima", &[], "")],
            vec![],
        ),
        section("sec_0002", vec![entity("Alyosha", &[], "")], vec![]),
        section("sec_0003", vec![entity("Alyosha", &[], "")], vec![]),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    let a = out
        .entities
        .iter()
        .find(|e| e.canonical_name == "Alyosha")
        .unwrap();
    let z = out
        .entities
        .iter()
        .find(|e| e.canonical_name == "Zosima")
        .unwrap();
    assert!(a.salience > z.salience);
    assert!((a.salience - 1.0).abs() < 0.001);
}

#[tokio::test]
async fn atlas_resolve_synthesizes_entity_atoms_for_orphan_event_participants() {
    // Participant name not matching any introduced entity is
    // synthesized into a minimal Entity atom rather than dropped.
    // The event keeps both participants and both Involves edges
    // are emitted. (Replaces the historical "orphans drop"
    // contract — see the SYNTHESIZED_ENTITY_SALIENCE rationale.)
    let sections = vec![section(
        "sec_0001",
        vec![entity("Alyosha", &[], "")],
        vec![event(
            "Alyosha and some Stranger meet.",
            &["Alyosha", "Stranger"],
        )],
    )];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(out.events.len(), 1);
    assert_eq!(out.events[0].participants.len(), 2);
    assert_eq!(out.entities.len(), 2);
    let stranger = out
        .entities
        .iter()
        .find(|e| e.canonical_name == "Stranger")
        .expect("Stranger should be synthesized from event participant");
    assert!(
        (stranger.salience - SYNTHESIZED_ENTITY_SALIENCE).abs() < 1e-6,
        "synthesized atoms should carry the indirect-evidence salience tier"
    );
    // Two Involves edges, one per participant.
    assert_eq!(
        out.edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Involves)
            .count(),
        2
    );
    // Synthesis must clear the failure buffer — no
    // unresolved-participant signals to surface.
    assert!(out.failures.is_empty());
}
