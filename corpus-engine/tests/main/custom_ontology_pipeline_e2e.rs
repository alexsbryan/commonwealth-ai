// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end for a declared ontology (`[enrichment.ontology] version = 1`),
//! from the shipped recipe template to the atom record the store writes.
//!
//! Drives the SHIPPED `numismatics` template — the one
//! `svrn recipe new --ontology numismatics` scaffolds — rather than a fixture
//! written to pass: a declaration nobody ships proves nothing about the
//! declaration authors get. No model runs; Phase 1's response is canned, so
//! what this test measures is the pipeline, not an extractor's compliance.
//!
//! The chain: recipe → `CustomAtlasSpec` → `LiteraryAtlasPipeline` →
//! `compose_phase1` (the prompt and the generated schema) → `parse_phase1`
//! (what survives) → `resolve_entities_and_events` (the atoms) →
//! `projection::project` (the record `atoms.lance` holds).

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::{
    AtomEnvelope, AtomId, ChunkRef, Entity, Provenance, SignalKind,
};
use corpus_engine::enrichment::atlas::projection::project;
use corpus_engine::enrichment::atlas::{
    resolve_entities_and_events, resolve_entities_and_events_with, resolve_step_3b_with,
    ResolutionPolicy,
};
use corpus_engine::enrichment::ontology::{OntologyPolicies, TypeIndex};
use corpus_engine::enrichment::pipeline::atlas::{
    DiscourseAct, EnrichmentDepth, EntityType, SectionExtraction,
};
use corpus_engine::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
use corpus_engine::enrichment::pipeline::types::ChapterInput;
use corpus_engine::enrichment::pipeline::Pipeline;
use corpus_engine::enrichment::reconciliation::{reconcile, reify_merges, ReconciliationPolicy};
use corpus_engine::types::EmbedFn;
use corpus_engine::Recipe;

/// Deterministic stand-in for the embedder: same text → same vector. The
/// resolver only uses it for description-similarity merging, and this fixture
/// plants no near-duplicates.
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

/// One canned Phase-1 response, carrying every case the order names: a coin
/// with typed attributes, a specializing `sceatta`, a `mint`, an anchored
/// attribution with a subject, an ANCHORLESS attribution, a declared voice
/// ("the cataloguer") both as an entity and as an attribution, and a question.
const PHASE1_RESPONSE: &str = r#"{
  "section_id": "sec_0001",
  "entities_introduced": [
    {
      "canonical_name": "Series R sceatta",
      "aliases": ["Series R"],
      "entity_type": "coin",
      "description": "A small silver penny of the southern English series.",
      "anchor": "Series R sceatta",
      "attributes": {
        "weight": 1.29,
        "metal": "Silver",
        "denomination": "penny",
        "mint": "Hamwic",
        "struck": "c. 710-760"
      }
    },
    {
      "canonical_name": "Series H sceatta",
      "entity_type": "sceatta",
      "description": "The Hamwic series proper.",
      "anchor": "Series H",
      "attributes": { "weight": 1.11 }
    },
    {
      "canonical_name": "Hamwic",
      "entity_type": "mint",
      "description": "The middle-Saxon trading settlement at Southampton.",
      "anchor": "Hamwic"
    },
    {
      "canonical_name": "The Cataloguer",
      "entity_type": "person",
      "description": "The compiler of this catalogue.",
      "anchor": "the cataloguer"
    }
  ],
  "claims": [
    {
      "content": "Series R was struck at Hamwic between 710 and 760.",
      "discourse_act": "imply",
      "claim_kind": "attribution",
      "subject": "Series R sceatta",
      "attributed_to": "the cataloguer",
      "anchor": "struck at Hamwic",
      "attributes": { "proposed_date": "c. 710-760", "grade": "die-link" }
    },
    {
      "content": "Series H is later than Series R.",
      "claim_kind": "attribution",
      "subject": "Series H sceatta"
    }
  ],
  "questions_raised": [
    { "content": "Which mint struck the Series R sceattas?", "anchor": "Series R" }
  ]
}"#;

/// The numismatics pipeline exactly as `enrich init` builds it, plus the
/// policies with the template's voice declaration added — the shipped
/// template declares no voices, and the voice rule is half of what P2 buys.
fn numismatics_pipeline() -> LiteraryAtlasPipeline {
    let toml = corpus_engine::recipe_templates::load_builtin("numismatics")
        .expect("numismatics is a shipped ontology template");
    let recipe = Recipe::from_toml(toml).expect("the shipped template parses");
    let mut spec = recipe
        .custom_atlas_spec()
        .expect("the template declares an [enrichment.ontology] block");
    let mut policies = spec.policies();
    assert!(
        policies.has_declarations(),
        "the shipped template must declare types or this test proves nothing"
    );
    policies.assertion.voices.not_entities = vec!["the cataloguer".into()];
    spec.policies = Some(policies);
    LiteraryAtlasPipeline::with_custom_ontology(&spec)
}

fn chapter() -> ChapterInput {
    let text = "The Series R sceatta, at 1.29 g, was struck at Hamwic c. 710-760.".to_string();
    ChapterInput {
        chapter_id: "sec_0001".into(),
        title: "Series R".into(),
        approx_tokens: text.len() / 4,
        text,
        metadata: HashMap::from([("ordinal".to_string(), "1".to_string())]),
    }
}

fn extraction() -> SectionExtraction {
    numismatics_pipeline()
        .parse_phase1(PHASE1_RESPONSE)
        .expect("the canned response parses")
        .section_extraction
        .expect("the atlas parser attaches a section extraction")
}

#[test]
fn phase1_prompt_carries_the_declared_types_and_a_generated_schema() {
    let prompt = numismatics_pipeline().compose_phase1(&chapter(), &[]);
    assert!(prompt.system.contains("## Domain focus"), "guidance kept");
    assert!(prompt.system.contains("## Declared types"));
    assert!(prompt.system.contains("**coin**") && prompt.system.contains("**attribution**"));
    assert!(prompt.system.contains("weight (number in g)"));

    let schema = prompt
        .response_schema
        .expect("a declared ontology attaches its generated schema");
    let entity_types = schema["$defs"]["entity_sketch"]["properties"]["entity_type"]["enum"]
        .as_array()
        .expect("entity_type is an enum");
    for declared in ["coin", "sceatta", "ruler", "mint"] {
        assert!(
            entity_types.contains(&serde_json::Value::String(declared.into())),
            "{declared} reachable in the grammar"
        );
    }
    assert!(
        entity_types.contains(&serde_json::Value::String("person".into())),
        "the generic six stay reachable"
    );
    assert_eq!(
        schema["$defs"]["entity_sketch"]["properties"]["attributes"]["properties"]["weight"]
            ["type"],
        "number"
    );
}

#[test]
fn declared_entities_survive_extraction_with_typed_attributes() {
    let e = extraction();
    let by_name: HashMap<&str, _> = e
        .entities_introduced
        .iter()
        .map(|x| (x.canonical_name.as_str(), x))
        .collect();

    let coin = by_name["Series R sceatta"];
    assert_eq!(coin.entity_type, EntityType::Other("coin".into()));
    assert_eq!(coin.attributes["weight"].as_f64(), Some(1.29));
    assert_eq!(coin.attributes["metal"].as_str(), Some("silver"));
    assert_eq!(coin.attributes["mint"].as_str(), Some("Hamwic"));
    assert_eq!(coin.attributes["struck"].as_str(), Some("c. 710-760"));

    // `sceatta specializes coin`, so it accepts coin's `weight` without
    // re-declaring it.
    let sceatta = by_name["Series H sceatta"];
    assert_eq!(sceatta.entity_type, EntityType::Other("sceatta".into()));
    assert_eq!(sceatta.attributes["weight"].as_f64(), Some(1.11));

    assert_eq!(
        by_name["Hamwic"].entity_type,
        EntityType::Other("mint".into())
    );
    assert!(
        !by_name.contains_key("The Cataloguer"),
        "a declared voice is a speaker, not subject matter"
    );
}

#[test]
fn anchorless_declared_claims_drop_and_the_anchored_one_keeps_its_facets() {
    let e = extraction();
    assert_eq!(e.claims.len(), 1, "the anchorless attribution dropped");
    let c = &e.claims[0];
    assert_eq!(c.claim_kind.as_deref(), Some("attribution"));
    assert_eq!(c.subject.as_deref(), Some("Series R sceatta"));
    // `force = "assertive"` on the type decides; the model said `imply`.
    assert_eq!(c.discourse_act, DiscourseAct::Assert);
    assert_eq!(c.attributes["grade"].as_str(), Some("die-link"));
    assert_eq!(c.attributes["proposed_date"].as_str(), Some("c. 710-760"));
    assert_eq!(
        c.attributed_to, None,
        "attributed to a voice, so there is no atom to point at"
    );
}

#[tokio::test]
async fn resolved_atoms_carry_the_declared_type_and_its_attributes() {
    let sections = vec![extraction()];
    let resolved = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .expect("resolution succeeds");

    let coin = resolved
        .entities
        .iter()
        .find(|e| e.canonical_name == "Series R sceatta")
        .expect("the coin atom exists");
    assert_eq!(coin.entity_type, EntityType::Other("coin".into()));
    assert_eq!(coin.attributes["weight"].as_f64(), Some(1.29));
    assert_eq!(coin.attributes["metal"].as_str(), Some("silver"));

    // The record the columnar store and the resident cache both derive from.
    let record = project(&AtomEnvelope::Entity(coin.clone()));
    assert_eq!(record.subtype, "coin", "the declared name IS the subtype");
    assert_eq!(record.name, "Series R sceatta");
}

// ── P3: resolution and identity ─────────────────────────────
//
// Two things a declared ontology has to do that P2 could not: make a ROLE
// resolve to the thing that plays it, and make an author's identity criterion
// decide when two mentions are one thing. Both drive SHIPPED templates.

/// The contracts template's canned Phase 1: Acme appears twice, once as the
/// `organization` it is and once as the `party` it is acting as, plus an
/// obligation about it.
const CONTRACTS_SECTION_1: &str = r#"{
  "section_id": "sec_0001",
  "entities_introduced": [
    {
      "canonical_name": "Acme Holdings Ltd",
      "entity_type": "organization",
      "description": "A holding company incorporated in Delaware.",
      "anchor": "Acme Holdings Ltd"
    }
  ],
  "questions_raised": [
    { "content": "Which entity is the contracting party?", "anchor": "Acme Holdings Ltd" }
  ]
}"#;

const CONTRACTS_SECTION_2: &str = r#"{
  "section_id": "sec_0002",
  "entities_introduced": [
    {
      "canonical_name": "Acme Holdings Ltd",
      "entity_type": "party",
      "description": "The counterparty under the master agreement.",
      "anchor": "the Party"
    }
  ],
  "claims": [
    {
      "content": "Acme shall deliver the audited accounts by 31 March.",
      "discourse_act": "assert",
      "claim_kind": "obligation",
      "subject": "Acme Holdings Ltd",
      "anchor": "shall deliver the audited accounts",
      "attributes": { "deontic": "require", "deadline": "31 March" }
    }
  ],
  "questions_raised": [
    { "content": "What happens if the accounts are late?", "anchor": "shall deliver" }
  ]
}"#;

/// The contracts template's pipeline and the policies it composed from — the
/// same `OntologyPolicies` both halves of the chain read.
fn contracts() -> (LiteraryAtlasPipeline, OntologyPolicies) {
    let toml = corpus_engine::recipe_templates::load_builtin("contracts")
        .expect("contracts is a shipped ontology template");
    let recipe = Recipe::from_toml(toml).expect("the shipped template parses");
    let spec = recipe
        .custom_atlas_spec()
        .expect("the template declares an [enrichment.ontology] block");
    (
        LiteraryAtlasPipeline::with_custom_ontology(&spec),
        spec.policies(),
    )
}

#[tokio::test]
async fn acme_is_one_organization_atom_with_a_party_state() {
    let (pipeline, policies) = contracts();
    let sections: Vec<SectionExtraction> = [CONTRACTS_SECTION_1, CONTRACTS_SECTION_2]
        .iter()
        .map(|raw| {
            pipeline
                .parse_phase1(raw)
                .expect("the canned response parses")
                .section_extraction
                .expect("the atlas parser attaches a section extraction")
        })
        .collect();

    let policy = ResolutionPolicy::new(&policies);
    let step_3a = resolve_entities_and_events_with(&sections, &fake_embed(), &policy)
        .await
        .expect("resolution succeeds");

    // ONE atom. `party role_of organization`, so the mention that called Acme
    // a party is a mention of the same organization — not a second thing.
    let acme: Vec<_> = step_3a
        .entities
        .iter()
        .filter(|e| e.canonical_name == "Acme Holdings Ltd")
        .collect();
    assert_eq!(acme.len(), 1, "one organization, not one per role");
    assert_eq!(
        acme[0].entity_type,
        EntityType::Other("organization".into()),
        "the atom is what Acme IS, not what it is acting as"
    );
    let acme_id = acme[0].id.clone();

    let step_3b = resolve_step_3b_with(&sections, &step_3a.entities, &step_3a.events, &policy)
        .expect("3b resolves");

    // …and the role it plays is a State on it, which is where the contract's
    // "who is a party" question is answered from.
    let party: Vec<_> = step_3b
        .states
        .iter()
        .filter(|s| s.entity_id == acme_id && s.label == "party")
        .collect();
    assert_eq!(party.len(), 1, "the party mention became a State");

    // The obligation's `subject` is a party — and resolves to that same
    // organization atom, because there is only one.
    let obligation = step_3b
        .claims
        .iter()
        .find(|c| c.claim_kind.as_deref() == Some("obligation"))
        .expect("the anchored obligation survived");
    assert_eq!(obligation.subject.as_ref(), Some(&acme_id));
}

#[test]
fn two_coins_sharing_an_external_id_merge_into_one_same_as_claim() {
    // The numismatics template ships no `identity` — a catalogue whose finds
    // carry an accession number declares one. This is that recipe's edit, and
    // the merge below is impossible without it: the two entries share no name
    // token, so every default signal is silent on them.
    let toml = corpus_engine::recipe_templates::load_builtin("numismatics").unwrap();
    let recipe = Recipe::from_toml(toml).unwrap();
    let mut policies = recipe.custom_atlas_spec().unwrap().policies();
    let coin = policies
        .shape
        .types
        .iter_mut()
        .find(|t| t.name == "coin")
        .expect("the template declares `coin`");
    coin.identity = vec!["find_id".into()];

    let entry = |id: &str, name: &str, find_id: &str, kind: SignalKind, doc: &str| {
        let mut e = Entity {
            id: AtomId::from_raw(id),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: EntityType::Other("coin".into()),
            first_appearance: ChunkRef::new("sec_0001", None),
            description: String::new(),
            defining_quote: None,
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Provenance::new("ext", doc, kind),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        };
        e.attributes
            .insert("find_id".into(), serde_json::Value::String(find_id.into()));
        e
    };

    // Exactly the chain `svrn enrich reconcile` runs: the atlas's declared
    // identity, flattened through `specializes`, into the merge policy.
    let policy = ReconciliationPolicy {
        identity: TypeIndex::from_policies(&policies).effective_identity_policy(),
        ..Default::default()
    };
    let outcome = reconcile(
        vec![
            entry(
                "entity-0001",
                "Series Y penny of Aldfrith",
                "SF-2019-114",
                SignalKind::LlmBatch,
                "catalogue.md",
            ),
            entry(
                "entity-0002",
                "Wessex Down 114",
                "sf-2019-114",
                SignalKind::ColumnHeader,
                "finds.csv",
            ),
        ],
        &policy,
    );
    assert_eq!(outcome.entities.len(), 1, "one find, one coin");

    let (claims, edges) = reify_merges(&outcome.reified, 1, 1);
    assert_eq!(
        claims.len(),
        1,
        "and the merge is IN the atlas, not only in the oplog"
    );
    assert_eq!(claims[0].claim_kind.as_deref(), Some("same_as"));
    assert_eq!(claims[0].attributes["grade"].as_str(), Some("external"));
    assert_eq!(
        claims[0].attributes["same_as"].as_array().map(|a| a.len()),
        Some(2)
    );
    // Reachable from either side.
    let targets: Vec<&str> = edges.iter().map(|e| e.target.as_str()).collect();
    assert!(targets.contains(&"entity-0001") && targets.contains(&"entity-0002"));
}
