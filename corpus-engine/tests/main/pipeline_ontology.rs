// SPDX-License-Identifier: AGPL-3.0-or-later
//! ei-2-map: every atlas describes itself (`EPISTEMIC_INDEX.md` §1, Map
//! row). For each registered pipeline, the map the resolve step writes —
//! `Pipeline::declared_ontology()` through `write_atlas_ontology`, the same
//! call `atlas_resolve.rs` makes — lands in an atlas dir, reads back through
//! the same reader every consumer uses, and names that pipeline's own kinds.

use std::path::Path;

use corpus_engine::enrichment::atlas::{
    read_atlas_ontology, write_atlas_ontology, AtlasOntologyFile,
};
use corpus_engine::enrichment::ontology::{NavigationPolicy, OntologyPolicies, TypeKind};
use corpus_engine::enrichment::pipeline::pipelines::configurable_atlas::CustomAtlasSpec;
use corpus_engine::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
use corpus_engine::enrichment::pipeline::{Pipeline, PipelineRegistry};
use corpus_engine::recipe::Recipe;
use corpus_engine_vocab::taxonomy::EntityType;

/// Build the atlas dir the way the resolve step does and read it back.
fn build_and_read(dir: &Path, pipeline: &dyn Pipeline, version: u32) -> AtlasOntologyFile {
    let atlas_dir = dir.join("atlas");
    write_atlas_ontology(
        &atlas_dir,
        pipeline.id(),
        version,
        &pipeline.declared_ontology(),
    )
    .unwrap();
    read_atlas_ontology(&atlas_dir).expect("ontology.json parses as the envelope")
}

fn builtin(id: &str) -> std::sync::Arc<dyn Pipeline> {
    PipelineRegistry::builtin()
        .get(id)
        .unwrap_or_else(|| panic!("{id} is registered"))
}

fn entity_names(p: &OntologyPolicies) -> Vec<&str> {
    p.shape.types.iter().map(|t| t.name.as_str()).collect()
}

fn label_of<'a>(p: &'a OntologyPolicies, name: &str) -> Option<&'a str> {
    p.type_decl(name).and_then(|t| t.label.as_deref())
}

/// Literary: `concept` entities are the genre's themes — the declared name is
/// the `entity_type` the atoms carry, the label is the genre's noun. Failing
/// input: a type named `theme` (no literary atom carries it), or the label
/// dropped.
#[test]
fn literary_atlas_describes_itself_naming_theme() {
    let tmp = tempfile::tempdir().unwrap();
    let p = builtin("literary_atlas");
    let file = build_and_read(tmp.path(), &*p, AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION);

    assert_eq!(file.pipeline_id, "literary_atlas");
    assert!(!file.is_author_declared());
    assert_eq!(file.ontology_version, 1);
    let map = &file.policies;
    assert!(map.has_declarations());
    assert_eq!(label_of(map, "concept"), Some("theme"));
    assert_eq!(label_of(map, "person"), Some("character"));
    assert!(map.derivation.configurations, "literary runs Phase 8");
    assert!(
        map.derivation.arguments,
        "the Phase-1 schema carries argument_reconstructions"
    );
    assert_eq!(map.navigation, NavigationPolicy::default());
}

/// Philosophy: the same five entity kinds under its own nouns, and the map
/// says it reconstructs arguments — `ArgumentReconstruction` is a closed atom
/// kind, so it is recorded on the derivation axis, not as a declared type.
#[test]
fn philosophy_atlas_describes_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let p = builtin("philosophy_atlas");
    let file = build_and_read(tmp.path(), &*p, AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION);
    let map = &file.policies;
    assert_eq!(file.pipeline_id, "philosophy_atlas");
    assert!(map.has_declarations());
    assert_eq!(label_of(map, "person"), Some("philosopher"));
    assert!(map.type_decl("concept").is_some());
    assert!(map.derivation.arguments);
    assert!(map.derivation.configurations);
}

/// Conversation: the voices facet carries what the module doc says in prose —
/// the user is the voice and neither the user nor the assistant is ever an
/// entity.
#[test]
fn conversation_atlas_describes_itself_with_its_voices() {
    let tmp = tempfile::tempdir().unwrap();
    let p = builtin("conversation_atlas");
    let file = build_and_read(tmp.path(), &*p, AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION);
    let map = &file.policies;
    assert_eq!(file.pipeline_id, "conversation_atlas");
    assert!(map.has_declarations());
    assert_eq!(map.assertion.voices.self_voice.as_deref(), Some("the user"));
    assert_eq!(
        map.assertion.voices.not_entities,
        vec!["the user".to_string(), "the assistant".to_string()]
    );
}

/// Referential: the one built-in whose prompt admits `event` as an entity
/// type, so its map lists it. Registered and atlas-producing — the order's
/// premise that it "produces no atlas of its own" does not hold.
#[test]
fn referential_atlas_describes_itself_including_event_entities() {
    let tmp = tempfile::tempdir().unwrap();
    let p = builtin("referential_atlas");
    let file = build_and_read(tmp.path(), &*p, AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION);
    let map = &file.policies;
    assert_eq!(file.pipeline_id, "referential_atlas");
    assert!(map.has_declarations());
    assert!(entity_names(map).contains(&"event"));
    assert!(!map.derivation.configurations, "referential skips Phase 8");
}

/// Engineering: Phase 1 emits only claims with no subtype, so there is no
/// type to declare — the file is still written, with the pipeline's terms
/// and flags, so the atlas describes itself as "claims only".
#[test]
fn engineering_atlas_writes_a_map_with_no_types() {
    let tmp = tempfile::tempdir().unwrap();
    let p = builtin("engineering_atlas");
    let file = build_and_read(tmp.path(), &*p, AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION);
    assert_eq!(file.pipeline_id, "engineering_atlas");
    assert!(!file.policies.has_declarations());
    assert!(!file.policies.derivation.arguments);
    assert_eq!(file.policies.vocabulary(), *p.vocabulary());
}

/// Custom (configurable) atlas: the map is the recipe's own declaration,
/// type for type, and the envelope says an author declared it.
#[test]
fn configurable_atlas_describes_itself_with_the_recipes_declaration() {
    let tmp = tempfile::tempdir().unwrap();
    let toml = corpus_engine::recipe_templates::load_builtin("numismatics")
        .expect("numismatics template ships");
    let recipe = Recipe::from_toml(toml).expect("template loads");
    let spec: CustomAtlasSpec = recipe
        .custom_atlas_spec()
        .expect("numismatics declares an ontology");
    let declared = spec.policies();
    let p = LiteraryAtlasPipeline::with_custom_ontology(&spec);

    let file = build_and_read(tmp.path(), &p, spec.ontology_version);
    assert_eq!(file.pipeline_id, "custom_atlas");
    assert!(file.is_author_declared());
    assert_eq!(file.ontology_version, spec.ontology_version);
    let map = &file.policies;
    assert!(map.has_declarations());
    assert_eq!(map.shape, declared.shape);
    assert_eq!(map.identity, declared.identity);
    assert_eq!(map.change, declared.change);
    assert_eq!(map.navigation, declared.navigation);
    assert!(entity_names(map).contains(&"coin"));
}

/// One decider each (ARCH §10.6): for every registered pipeline the map's
/// terms are `vocabulary()` term for term, and its configuration flag is
/// `runs_configuration_phase()`. Failing input: a built-in TOML that writes
/// `[vocabulary]` or `derive.configurations`, or an override of
/// `declared_ontology` that forgets to fill them.
#[test]
fn builtin_maps_use_the_pipelines_own_deciders() {
    let registry = PipelineRegistry::builtin();
    let mut checked = 0;
    for id in registry.pipeline_ids() {
        let p = registry.get(id).unwrap();
        if !id.ends_with("_atlas") {
            continue; // `literary` (v1) produces no atlas dir
        }
        let map = p.declared_ontology();
        assert_eq!(map.vocabulary(), *p.vocabulary(), "{id}: terms");
        assert_eq!(
            map.derivation.configurations,
            p.runs_configuration_phase(),
            "{id}: configuration flag"
        );
        assert!(
            map.prose.guidance.is_empty(),
            "{id}: guidance is the custom-path hinge"
        );
        checked += 1;
    }
    assert!(checked >= 5, "checked {checked} atlas pipelines");
}

/// I5, kept structural: a built-in map names only entity types the taxonomy
/// already knows (`EntityType::NAMED`), so `enumerable_types` in retrieval —
/// which dedups declared names against the generic six — renders the same
/// bytes for a rebuilt literary or SEP atlas as it does today. Referential's
/// `event` is the one documented exception (its prompt admits it; the atoms
/// carry `Other("event")`), and this test pins it as exactly one.
#[test]
fn builtin_maps_name_only_kinds_their_atoms_carry() {
    let registry = PipelineRegistry::builtin();
    let mut exceptions = Vec::new();
    for id in registry.pipeline_ids() {
        let p = registry.get(id).unwrap();
        for t in &p.declared_ontology().shape.types {
            assert_eq!(t.kind, TypeKind::Entity, "{id}: {}", t.name);
            if !EntityType::NAMED.contains(&t.name.as_str()) {
                exceptions.push(format!("{id}:{}", t.name));
            }
        }
    }
    assert_eq!(exceptions, vec!["referential_atlas:event".to_string()]);
}
