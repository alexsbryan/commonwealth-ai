// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology declaration v1, P5: a DECLARED type reaches an answer.
//!
//! This is the deterministic half of link 5 of the ontology-v1 chain — recipe
//! → validate → install → init → build → **the author's noun comes out** — and
//! it is the half a test can own. It walks the real artefacts: an
//! `atlas/ontology.json` written by the same writer the resolve step calls,
//! read back by the same reader retrieval uses, driving the real classifier,
//! the real traversal and the real brief assembler. What it deliberately does
//! NOT cover is the model call in `atom_enum`'s stage 1; that lane is
//! `SOVEREIGN_ATOM_ENUM=1` against a built corpus, and this file is the
//! statement of what must be true for that lane to have a chance.
//!
//! The probe is `sovereign-recipes/wessex-hoard/truth.json`'s
//! `enumeration_probe`: "Which coins are in this catalogue, and what metal is
//! each?", `expected_coin_count` 7. Seven, not three — four of the catalogue's
//! coins are typed `sceatta`, and `sceatta specializes coin`.

use corpus_engine::atlas_traversal::{
    assemble_brief, classify_query, classify_query_with, engine::AtlasView, traverse, QueryPlan,
};
use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity};
use corpus_engine::enrichment::atlas::{read_atlas_ontology, write_atlas_ontology};
use corpus_engine::enrichment::ontology::OntologyPolicies;
use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

/// The shipped numismatics declaration, read from the template itself so no
/// test here can pass against an ontology nobody ships.
fn numismatics_policies() -> OntologyPolicies {
    corpus_engine::recipe_templates::policies("numismatics")
        .expect("numismatics is a shipped ontology template")
}

/// `sovereign-recipes/wessex-hoard/truth.json`, vendored by `build.rs` beside
/// the recipe it belongs to. THE truth file, not a transcription of it: until
/// 2026-09-03 the seven catalogue rows were re-typed here, so an edit to the
/// manifest the eval bank and the chain proof both read left this test
/// asserting the old hoard.
const TRUTH_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/recipes/wessex-hoard/truth.json"));

/// One catalogue coin, typed under the author's noun.
fn coin(idx: usize, name: &str, subtype: &str, metal: &str, salience: f32) -> Entity {
    let mut attributes = serde_json::Map::new();
    attributes.insert("metal".into(), serde_json::Value::String(metal.into()));
    Entity {
        id: AtomId::entity(idx),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Other(subtype.into()),
        first_appearance: ChunkRef::new(format!("sec_{idx:04}"), None),
        description: format!("Catalogue entry {idx}."),
        defining_quote: None,
        salience,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes,
        concept_kind: None,
    }
}

/// The wessex-hoard catalogue, read from `truth.json`'s `entities.coin` rows
/// in file order. Salience descends so the walk's sort is deterministic; it is
/// not a truth field.
fn wessex_hoard() -> Vec<Entity> {
    let truth: serde_json::Value = serde_json::from_str(TRUTH_JSON).expect("truth.json parses");
    let coins = truth["entities"]["coin"]
        .as_array()
        .expect("truth.json declares entities.coin");
    coins
        .iter()
        .enumerate()
        .map(|(i, c)| {
            coin(
                i + 1,
                c["name"].as_str().expect("every truth coin has a name"),
                c["subtype"]
                    .as_str()
                    .expect("every truth coin has a subtype"),
                c["metal"].as_str().expect("every truth coin has a metal"),
                0.90 - 0.10 * i as f32,
            )
        })
        .collect()
}

/// `truth.json`'s `enumeration_probe`: the question the chain must answer, and
/// how many coins the answer must carry.
fn enumeration_probe() -> (String, usize) {
    let truth: serde_json::Value = serde_json::from_str(TRUTH_JSON).expect("truth.json parses");
    let probe = &truth["enumeration_probe"];
    (
        probe["question"]
            .as_str()
            .expect("the probe declares a question")
            .to_string(),
        probe["expected_coin_count"]
            .as_u64()
            .expect("the probe declares expected_coin_count") as usize,
    )
}

fn view<'a>(entities: &'a [Entity], vocab: Option<&'a OntologyPolicies>) -> AtlasView<'a> {
    AtlasView {
        entities,
        events: &[],
        states: &[],
        relations: &[],
        claims: &[],
        questions: &[],
        configurations: &[],
        edges: &[],
        positions: &[],
        oppositions: &[],
        vocab,
    }
}

/// The whole deterministic chain, over an atlas dir on disk.
#[test]
fn the_enumeration_probe_answers_with_the_authors_noun() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");

    // The resolve step's writer, and the reader retrieval uses. Not a
    // hand-built struct — if the round trip breaks, this test breaks.
    write_atlas_ontology(&atlas_dir, "custom_atlas", 1, &numismatics_policies()).unwrap();
    let policies = read_atlas_ontology(&atlas_dir)
        .map(|f| f.policies)
        .filter(|p| p.has_declarations())
        .expect("a declared ontology round-trips through atlas/ontology.json");

    let entities = wessex_hoard();
    let (question, expected_coins) = enumeration_probe();

    // 1. The classifier reaches the author's noun.
    let plan = classify_query_with(&question, &entities, Some(&policies));
    assert_eq!(
        plan,
        QueryPlan::Enumerate {
            entity_type: "coin".into()
        },
        "the probe must classify as an enumeration over the DECLARED type"
    );

    // 2. The walk returns all seven — the four sceattas included.
    let result = traverse(&plan, view(&entities, Some(&policies)));
    assert!(result.hit);
    assert_eq!(result.kind, "enumerate");
    assert_eq!(
        result.entities.len(),
        expected_coins,
        "truth.json expected_coin_count: `sceatta specializes coin`"
    );
    assert_eq!(
        expected_coins,
        entities.len(),
        "the probe's count and the catalogue it is asked over come from the \
         same manifest, so a coin added to truth.json cannot pass unnoticed"
    );

    // 3. The brief names every coin and its metal — the second half of the
    //    question ("and what metal is each") is answered from `attributes`.
    let text = assemble_brief(&result).to_text();
    for e in &entities {
        assert!(
            text.contains(&e.canonical_name),
            "brief omits {}: {text}",
            e.canonical_name
        );
    }
    for metal in ["metal=silver", "metal=gold", "metal=billon"] {
        assert!(text.contains(metal), "brief omits {metal}: {text}");
    }
}

/// I5, end to end. The same atlas with NO `ontology.json` classifies and
/// walks exactly as it did before ontology v1 — no declared-type plan, and
/// the enumeration cannot be reached at all.
#[test]
fn an_undeclared_atlas_is_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();

    assert!(
        read_atlas_ontology(&atlas_dir).is_none(),
        "an atlas dir with no ontology.json declares nothing"
    );

    let entities = wessex_hoard();
    let (question, _) = enumeration_probe();
    let with_none = classify_query_with(&question, &entities, None);
    assert_eq!(with_none, classify_query(&question, &entities));
    assert!(
        !matches!(
            with_none,
            QueryPlan::Enumerate { .. } | QueryPlan::Aggregate { .. }
        ),
        "no declaration, no declared-type plan: {with_none:?}"
    );
}

/// A prose-only ontology (guidance + vocabulary, the version-0 shape every
/// pre-ontology recipe writes) is not a vocabulary. maple-house is one, which
/// is why the governance lane sees no change from P5.
#[test]
fn a_prose_only_ontology_is_not_a_vocabulary() {
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = tmp.path().join("atlas");
    let prose = OntologyPolicies::from_prose(
        "Extract, as claims, every NORMATIVE STATEMENT.",
        Default::default(),
    );
    write_atlas_ontology(&atlas_dir, "custom_atlas", 1, &prose).unwrap();

    let read_back = read_atlas_ontology(&atlas_dir).expect("the file exists and parses");
    assert!(!read_back.policies.has_declarations());
    assert!(
        read_back.policies.shape.types.is_empty(),
        "prose declares no types, so every declared-type path stays shut"
    );
}
