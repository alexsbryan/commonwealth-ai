// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase-3 resolution under a DECLARED ontology, end to end from Phase-1
//! sketches to the atoms and edges the atlas writes.
//!
//! Four behaviours P3 adds, each with the red input named: a relation whose
//! ends contradict the declaration, a claim whose `subject` names nothing, a
//! `ref` attribute whose value names nothing, and a role mentioned twice. The
//! fifth test is the I1 guard at resolution level — the shim and an empty
//! policy are the same call, so a version-0 corpus cannot drift.
//!
//! These drive the PUBLIC resolver surface
//! (`resolve_entities_and_events_with` / `resolve_step_3b_with`), so they live
//! here rather than inside `resolution.rs`, which is 4.5x ARCH §3.1's ceiling
//! and does not need another 340 lines.

use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::AtomId;
use corpus_engine::enrichment::atlas::edges::EdgeType;
use corpus_engine::enrichment::atlas::{
    resolve_entities_and_events, resolve_entities_and_events_with, resolve_step_3b,
    resolve_step_3b_with, ResolutionOutput, ResolutionPolicy, Step3bOutput,
};
use corpus_engine::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl, TypeKind};
use corpus_engine::enrichment::pipeline::atlas::{
    ClaimSketch, DiscourseAct, EnrichmentDepth, EntitySketch, EntityStateSketch, EntityType,
    EpistemicStatus, EventSketch, RelationSketch, SectionExtraction,
};
use corpus_engine::enrichment::pipeline::types::PhaseFailureKind;
use corpus_engine::types::EmbedFn;

/// Deterministic stand-in for the embedder: same text, same vector.
fn fake_embed() -> EmbedFn {
    Arc::new(move |s: &str| {
        let s = s.to_string();
        Box::pin(async move {
            let b = s.as_bytes();
            Ok(vec![
                b.first().copied().unwrap_or(0) as f32,
                b.get(1).copied().unwrap_or(0) as f32,
                b.get(2).copied().unwrap_or(0) as f32,
            ])
        })
    })
}

fn section(id: &str, entities: Vec<EntitySketch>, events: Vec<EventSketch>) -> SectionExtraction {
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

fn entity(name: &str, aliases: &[&str], description: &str) -> EntitySketch {
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

fn event(desc: &str, participants: &[&str]) -> EventSketch {
    EventSketch {
        attributes: Default::default(),
        event_type: None,
        description: desc.into(),
        participants: participants.iter().map(|s| s.to_string()).collect(),
        anchor: String::new(),
    }
}

/// The shipped numismatics declaration, plus the one relation type the
/// template leaves to the author (there is nothing to check endpoints
/// against otherwise).
fn numismatics() -> OntologyPolicies {
    let mut p = corpus_engine::recipe_templates::policies("numismatics")
        .expect("numismatics is a shipped ontology template");
    p.shape.types.push(OntologyTypeDecl {
        name: "struck_at".into(),
        kind: TypeKind::Relation,
        from: Some("coin".into()),
        to: Some("mint".into()),
        ..Default::default()
    });
    p
}

fn typed(name: &str, ty: &str) -> EntitySketch {
    let mut e = entity(name, &[], name);
    e.entity_type = EntityType::Other(ty.into());
    e.anchor = name.to_string();
    e
}

fn relation(participants: &[&str], rel_type: &str) -> RelationSketch {
    RelationSketch {
        participants: participants.iter().map(|p| p.to_string()).collect(),
        label: "struck at".into(),
        anchor: "struck at".into(),
        relation_type: Some(rel_type.into()),
        attributes: Default::default(),
    }
}

fn claim(content: &str, subject: Option<&str>, attributed_to: Option<&str>) -> ClaimSketch {
    ClaimSketch {
        content: content.into(),
        discourse_act: DiscourseAct::Assert,
        epistemic_status: EpistemicStatus::Confident,
        attributed_to: attributed_to.map(str::to_string),
        quotable_excerpt: None,
        anchor: "struck at".into(),
        claim_kind: Some("attribution".into()),
        subject: subject.map(str::to_string),
        scope: None,
        attributes: Default::default(),
    }
}

/// Resolve a corpus end to end under `policies`, the way
/// `atlas_resolve.rs` does.
async fn resolve(
    policies: &OntologyPolicies,
    sections: Vec<SectionExtraction>,
) -> (ResolutionOutput, Step3bOutput) {
    let policy = ResolutionPolicy::new(policies);
    let step_3a = resolve_entities_and_events_with(&sections, &fake_embed(), &policy)
        .await
        .expect("3a resolves");
    let step_3b = resolve_step_3b_with(&sections, &step_3a.entities, &step_3a.events, &policy)
        .expect("3b resolves");
    (step_3a, step_3b)
}

fn id_of(out: &ResolutionOutput, name: &str) -> AtomId {
    out.entities
        .iter()
        .find(|e| e.canonical_name == name)
        .unwrap_or_else(|| panic!("no atom named {name}"))
        .id
        .clone()
}

#[tokio::test]
async fn endpoint_mismatch_dropped_and_recorded() {
    let policies = numismatics();
    // Names deliberately share no token: Step 3a merges two entities
    // that overlap on two tokens, and "Series R sceatta" / "Series H
    // sceatta" would collapse into one atom before the endpoint check
    // could see the pair.
    let mut section = section(
        "sec_0001",
        vec![
            typed("Series R sceatta", "coin"),
            typed("Hamwic", "mint"),
            typed("Aldfrith", "ruler"),
        ],
        vec![],
    );
    section.relations_introduced = vec![
        // `to` is a mint: the declaration is satisfied.
        relation(&["Series R sceatta", "Hamwic"], "struck_at"),
        // `to` is the person who ruled: it is not a mint.
        relation(&["Series R sceatta", "Aldfrith"], "struck_at"),
    ];
    let (_, step_3b) = resolve(&policies, vec![section]).await;

    assert_eq!(
        step_3b.relations.len(),
        1,
        "the mismatched relation dropped"
    );
    assert_eq!(
        step_3b.relations[0].relation_type,
        corpus_engine::enrichment::pipeline::atlas::RelationType::Other("struck_at".into()),
        "the surviving relation keeps the author's noun"
    );
    let mismatch: Vec<_> = step_3b
        .failures
        .iter()
        .filter(|f| f.kind == PhaseFailureKind::EndpointTypeMismatch)
        .collect();
    assert_eq!(mismatch.len(), 1, "and the drop is on the record");
    assert!(
        mismatch[0].reason.contains("to = `mint`") && mismatch[0].reason.contains("Aldfrith"),
        "{}",
        mismatch[0].reason
    );
}

/// A name the extractor also minted under another type resolves, at a
/// declared end, to the atom of the declared type — not to whichever atom of
/// that name was indexed last. On ft-ans-dev-b (2026-10-02) every hoard and
/// mint had a description-less `person` twin ("Demanhur hoard" the person),
/// and 58 of 63 `holds_coins_of` relations were dropped as mismatches
/// against the twin.
#[tokio::test]
async fn a_declared_end_resolves_to_the_atom_of_its_declared_type() {
    let policies = numismatics();
    let first = section(
        "sec_0001",
        vec![typed("Series R sceatta", "coin"), typed("Hamwic", "mint")],
        vec![],
    );
    let mut later = section("sec_0002", vec![typed("Hamwic", "person")], vec![]);
    later.relations_introduced = vec![relation(&["Series R sceatta", "Hamwic"], "struck_at")];
    let (step_3a, step_3b) = resolve(&policies, vec![first, later]).await;

    let hamwics: Vec<_> = step_3a
        .entities
        .iter()
        .filter(|e| e.canonical_name == "Hamwic")
        .collect();
    assert_eq!(
        hamwics.len(),
        2,
        "the twin survives 3a, as on the ANS corpus"
    );
    let mint = hamwics
        .iter()
        .find(|e| e.entity_type.as_str_repr() == "mint")
        .expect("the mint atom")
        .id
        .clone();
    assert_eq!(
        step_3b.relations.len(),
        1,
        "kept, not refused: {:?}",
        step_3b.failures
    );
    assert_eq!(
        step_3b.relations[0].participants[1], mint,
        "the `to` end is the mint"
    );
}

/// A state recorded on a hoard folds into the hoard. The state sketch names an
/// entity and asserts no type; its fallback `Person` met the declared veto and
/// every hoard or mint a state was recorded on gained a description-less
/// `person` twin (19 names on ft-ans-dev-b, 12 on ft-ans-dev-a).
#[tokio::test]
async fn a_state_reference_folds_into_the_declared_atom_of_its_name() {
    let policies = numismatics();
    let mut s = section("sec_0001", vec![typed("Hamwic", "mint")], vec![]);
    s.entities_developed = vec![EntityStateSketch {
        entity_name: "Hamwic".into(),
        label: "closed by the Danes".into(),
        anchor: "Hamwic".into(),
        state_type: None,
    }];
    let (step_3a, step_3b) = resolve(&policies, vec![s]).await;

    let hamwics: Vec<_> = step_3a
        .entities
        .iter()
        .filter(|e| e.canonical_name == "Hamwic")
        .collect();
    assert_eq!(
        hamwics.len(),
        1,
        "no twin: {:?}",
        hamwics
            .iter()
            .map(|e| e.entity_type.as_str_repr())
            .collect::<Vec<_>>()
    );
    assert_eq!(hamwics[0].entity_type.as_str_repr(), "mint");
    assert_eq!(step_3b.states.len(), 1);
    assert_eq!(
        step_3b.states[0].entity_id, hamwics[0].id,
        "the state is the mint's"
    );
}

/// The schema's escape value for a relation the recipe did not declare
/// resolves exactly as an absent type always has: kept, unchecked, typed
/// `unclassified` — never refused for ends a declared type would reject.
#[tokio::test]
async fn an_unclassified_relation_is_kept_and_its_ends_are_unchecked() {
    let policies = numismatics();
    let mut section = section(
        "sec_0001",
        vec![
            typed("Series R sceatta", "coin"),
            typed("Aldfrith", "ruler"),
        ],
        vec![],
    );
    section.relations_introduced = vec![relation(
        &["Series R sceatta", "Aldfrith"],
        corpus_engine::enrichment::pipeline::pipelines::ontology_schema::UNCLASSIFIED_RELATION,
    )];
    let (_, step_3b) = resolve(&policies, vec![section]).await;

    assert_eq!(step_3b.relations.len(), 1, "an undeclared relation is kept");
    assert_eq!(
        step_3b.relations[0].relation_type,
        corpus_engine::enrichment::pipeline::atlas::RelationType::Other("unclassified".into()),
    );
    assert!(
        !step_3b
            .failures
            .iter()
            .any(|f| f.kind == PhaseFailureKind::EndpointTypeMismatch),
        "no declared ends, so nothing to mismatch"
    );
}

#[tokio::test]
async fn subject_resolves_like_attribution() {
    let policies = numismatics();
    let mut section = section(
        "sec_0001",
        vec![
            typed("Series R sceatta", "coin"),
            entity("Halstead", &[], "A numismatist."),
        ],
        vec![],
    );
    section.claims = vec![
        claim(
            "Series R was struck at Hamwic.",
            Some("Series R sceatta"),
            Some("Halstead"),
        ),
        claim("An orphan attribution.", Some("Nobody At All"), None),
    ];
    let (step_3a, step_3b) = resolve(&policies, vec![section]).await;

    let coin = id_of(&step_3a, "Series R sceatta");
    let scholar = id_of(&step_3a, "Halstead");
    let resolved = &step_3b.claims[0];
    assert_eq!(resolved.subject.as_ref(), Some(&coin), "the referent");
    assert_eq!(resolved.attributed_to.as_ref(), Some(&scholar), "the voice");
    assert_eq!(resolved.claim_kind.as_deref(), Some("attribution"));

    // Both links are Involves edges, so a reader seeded on either the
    // coin or the scholar finds the claim.
    let involves: Vec<&AtomId> = step_3b
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Involves && e.source == resolved.id)
        .map(|e| &e.target)
        .collect();
    assert!(involves.contains(&&coin) && involves.contains(&&scholar));

    // The unresolvable subject: the claim keeps its content and type,
    // and the loss is recorded rather than silent.
    assert_eq!(step_3b.claims[1].subject, None);
    assert_eq!(step_3b.claims[1].claim_kind.as_deref(), Some("attribution"));
    let dropped: Vec<_> = step_3b
        .failures
        .iter()
        .filter(|f| f.kind == PhaseFailureKind::UnresolvedClaimSubject)
        .collect();
    assert_eq!(dropped.len(), 1);
    assert!(dropped[0].reason.contains("Nobody At All"));
}

#[tokio::test]
async fn ref_attribute_snaps_to_id() {
    let policies = numismatics();
    let mut good = typed("Series R sceatta", "coin");
    good.attributes.insert("mint".into(), "Hamwic".into());
    // Shares no token with the coin above — see
    // `endpoint_mismatch_dropped_and_recorded` for why that matters.
    let mut orphan = typed("Beonna penny", "coin");
    orphan
        .attributes
        .insert("mint".into(), "Nowhere At All".into());
    let section = section(
        "sec_0001",
        vec![good, orphan, typed("Hamwic", "mint")],
        vec![],
    );
    let (step_3a, step_3b) = resolve(&policies, vec![section]).await;

    let coin = id_of(&step_3a, "Series R sceatta");
    let mint = id_of(&step_3a, "Hamwic");
    let update = step_3b
        .entity_attribute_updates
        .get(coin.as_str())
        .expect("the coin's attributes were rewritten");
    assert_eq!(
        update["mint"].as_str(),
        Some(mint.as_str()),
        "the ref now points at the atom, not at a string"
    );

    // The unresolvable one keeps the name — a reader still learns what
    // the catalogue said — and the missing edge is on the record.
    let orphan_id = id_of(&step_3a, "Beonna penny");
    assert!(
        !step_3b
            .entity_attribute_updates
            .contains_key(orphan_id.as_str()),
        "nothing to apply, so the name stays"
    );
    let unresolved: Vec<_> = step_3b
        .failures
        .iter()
        .filter(|f| f.kind == PhaseFailureKind::UnresolvedAttributeRef)
        .collect();
    assert_eq!(unresolved.len(), 1);
    assert!(unresolved[0].reason.contains("Nowhere At All"));
}

/// A ref that resolves onto a DIFFERENT declared type is refused; one that
/// resolves onto a generic type still snaps. On the ei7-ans atlas 33 refs
/// had landed on the wrong declared type (a coin's `mint` on a hoard, a
/// `ruler` on a mint) and the guard costs no K1 recall (feature-fidelity
/// O0, 2026-10-02); the generic case is the one the module comment protects.
#[tokio::test]
async fn a_ref_onto_another_declared_type_is_refused_and_a_generic_one_snaps() {
    let policies = numismatics();
    let mut wrong = typed("Series R sceatta", "coin");
    wrong
        .attributes
        .insert("mint".into(), "Beonna penny".into());
    let mut generic = typed("Offa denier", "coin");
    generic.attributes.insert("mint".into(), "Hamwic".into());
    let section = section(
        "sec_0001",
        vec![
            wrong,
            generic,
            typed("Beonna penny", "coin"),
            typed("Hamwic", "place"),
        ],
        vec![],
    );
    let (step_3a, step_3b) = resolve(&policies, vec![section]).await;

    let wrong_id = id_of(&step_3a, "Series R sceatta");
    assert!(
        !step_3b
            .entity_attribute_updates
            .contains_key(wrong_id.as_str()),
        "a coin is not a mint: the ref keeps its name instead of pointing at the coin"
    );
    let refused: Vec<_> = step_3b
        .failures
        .iter()
        .filter(|f| {
            f.kind == PhaseFailureKind::UnresolvedAttributeRef && f.reason.contains("Beonna penny")
        })
        .collect();
    assert_eq!(refused.len(), 1, "and the refusal is on the record");
    assert!(
        refused[0].reason.contains("`coin`"),
        "{}",
        refused[0].reason
    );

    let generic_id = id_of(&step_3a, "Offa denier");
    let hamwic = id_of(&step_3a, "Hamwic");
    assert_eq!(
        step_3b.entity_attribute_updates[generic_id.as_str()]["mint"].as_str(),
        Some(hamwic.as_str()),
        "a generic `place` is not another declared type, so it snaps"
    );
}

#[tokio::test]
async fn role_mention_becomes_state_on_rigid_atom_with_transition() {
    let policies = numismatics();
    let sections = vec![
        section("sec_0001", vec![typed("Aldfrith", "ruler")], vec![]),
        section("sec_0002", vec![typed("Aldfrith", "ruler")], vec![]),
    ];
    let (step_3a, step_3b) = resolve(&policies, sections).await;

    // ONE atom, and it is a person — `ruler role_of person`, so the
    // role is not the kind of thing Aldfrith is.
    let people: Vec<_> = step_3a
        .entities
        .iter()
        .filter(|e| e.canonical_name == "Aldfrith")
        .collect();
    assert_eq!(people.len(), 1, "one person, not one person per role");
    assert_eq!(people[0].entity_type, EntityType::Person);

    let owner = people[0].id.clone();
    let role_states: Vec<_> = step_3b
        .states
        .iter()
        .filter(|s| s.entity_id == owner && s.label == "ruler")
        .collect();
    assert_eq!(role_states.len(), 2, "one State per mention");
    assert_eq!(
        role_states[0].state_type,
        corpus_engine::enrichment::pipeline::atlas::StateType::Other("ruler".into()),
        "the author's noun, not a Phase-5 guess"
    );

    // And the trajectory pass chains them without knowing anything
    // about ontologies — which is the whole reason roles are States.
    let transitions: Vec<_> = step_3b
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Transition)
        .collect();
    assert_eq!(transitions.len(), 1);
    assert_eq!(
        step_3b.trajectories[owner.as_str()].transitions.len(),
        1,
        "and it lands in trajectories.json"
    );
}

#[tokio::test]
async fn an_undeclared_corpus_resolves_exactly_as_the_shim_does() {
    // The I1 guard at resolution level: the shim and an empty policy
    // are the same call, so a version-0 corpus cannot drift.
    let sections = vec![section(
        "sec_0001",
        vec![entity("Jane", &["Miss Eyre"], "A governess.")],
        vec![event("Jane arrives at Thornfield", &["Jane"])],
    )];
    let shim = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .expect("3a resolves");
    let empty = OntologyPolicies::default();
    let (with, _) = resolve(&empty, sections.clone()).await;
    assert_eq!(
        serde_json::to_value(&shim.entities).unwrap(),
        serde_json::to_value(&with.entities).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&shim.events).unwrap(),
        serde_json::to_value(&with.events).unwrap()
    );

    let b_shim = resolve_step_3b(&sections, &shim.entities, &shim.events).unwrap();
    assert!(b_shim.entity_attribute_updates.is_empty());
    assert!(b_shim.states.is_empty() && b_shim.relations.is_empty());
}
