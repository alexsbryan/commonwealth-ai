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

use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
use corpus_engine::enrichment::atlas::projection::project;
use corpus_engine::enrichment::atlas::resolve_entities_and_events;
use corpus_engine::enrichment::pipeline::atlas::{DiscourseAct, EntityType, SectionExtraction};
use corpus_engine::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
use corpus_engine::enrichment::pipeline::types::ChapterInput;
use corpus_engine::enrichment::pipeline::Pipeline;
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
    let by_name: HashMap<&str, &_> = e
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
