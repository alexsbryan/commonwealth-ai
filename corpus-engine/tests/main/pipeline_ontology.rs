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
use corpus_engine::enrichment::atlas::{AtomType, EdgeType};
use corpus_engine::enrichment::ontology::{NavigationPolicy, OntologyPolicies, TypeKind};
use corpus_engine::enrichment::pipeline::pipelines::configurable_atlas::CustomAtlasSpec;
use corpus_engine::enrichment::pipeline::pipelines::literary_atlas::LiteraryAtlasPipeline;
use corpus_engine::enrichment::pipeline::{Pipeline, PipelineRegistry};
use corpus_engine::recipe::Recipe;
use corpus_engine_vocab::ontology::{QuestionKind, SummarySource};
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
    // Since map-conversion rung 2 the genre declares its own rows, written
    // against what it emits — not the pre-registered table, which seeds the
    // tension row on a `Position` no built-in build produces.
    assert_ne!(map.navigation, NavigationPolicy::default());
    assert!(!map
        .navigation
        .tension
        .seed
        .kinds
        .contains(&AtomType::Position));
}

/// map-conversion rung 2 (2026-09-08): every ON row of every built-in map
/// names only kinds its pipeline can emit (`KindSet::covers`, the universal
/// rule) — and the ratchet BITES: the pre-registered table fails it on every
/// built-in, because `Position`, `Causes`, `OpposesIn` and `Grounds` seat in
/// no built-in atlas. Failing input: put `Grounds` back in any walk list, or
/// `Position` in any seed list.
#[test]
fn builtin_navigation_rows_name_only_kinds_the_pipeline_emits() {
    let registry = PipelineRegistry::builtin();
    let mut checked = 0;
    let mut rows_on = 0;
    for id in registry.pipeline_ids() {
        if !id.ends_with("_atlas") {
            continue;
        }
        let p = registry.get(id).unwrap();
        let emits = p.emits();
        let map = p.declared_ontology();
        for (kind, row) in map.navigation.rows() {
            if row.exemplars.is_empty() {
                continue; // switched off by name
            }
            rows_on += 1;
            assert_eq!(
                emits.covers(row),
                None,
                "{id}: the {} row names a kind this pipeline never emits",
                kind.as_str()
            );
        }
        let uncovered = NavigationPolicy::default()
            .rows()
            .filter(|(_, r)| emits.covers(r).is_some())
            .count();
        assert!(
            uncovered > 0,
            "{id}: the pre-registered table is fully covered, so this ratchet could not bite"
        );
        checked += 1;
    }
    assert!(checked >= 5, "checked {checked} atlas pipelines");
    // Four genres × five rows on, plus engineering's one.
    assert_eq!(rows_on, 21);
}

/// The CONVERSE of the ratchet above, and the one that was missing: every atom
/// kind a pipeline EMITS must be named as a seed by some ON row, or be listed
/// here as deliberately unreachable.
///
/// The forward rule (rows ⊆ emits) stops a map promising a walk its atlases
/// cannot serve. Nothing stopped the opposite, and the opposite is what
/// happened: `section_extraction_kinds()` has emitted `Event`, `Relation` and
/// `Question` since it existed, no built-in row named any of the three, and
/// `seed_population` derives the ANN table from the union of the rows' seed
/// kinds — so a third of every section-extracted atlas had no vector and no
/// walk could reach it. Measured on `chaos-secret-agent` 2026-09-09:
/// `atlas status --json` reports `ann.embedded_atoms` 151 against 226 atoms,
/// the missing 75 being exactly Event 33 + Relation 20 + Question 22. The
/// bench probe `present-killer-weapon` is answered by one of them (event-0033,
/// "…with a carving knife"), which is why the lane read a false abstention on
/// an answerable question.
///
/// Failing input: drop `Event` from any genre's `lookup` seed list.
#[test]
fn every_kind_a_pipeline_emits_is_reachable_by_some_row() {
    // Deliberately unreachable, with the reason. A kind belongs here only when
    // no measured question needs it — adding a kind to a seed list with no
    // failing input is a widening nobody asked for (ARCH §18.1).
    //
    // `Question`: the work's OWN open questions ("Why did Stevie set off
    // fireworks?"), raised by the text rather than asserted by it. No probe on
    // any bank is answered by one, and seeding them would put the corpus's
    // questions into a cosine race against the reader's.
    const UNREACHABLE_BY_DESIGN: &[AtomType] = &[AtomType::Question];

    let registry = PipelineRegistry::builtin();
    let mut checked = 0;
    let mut orphans: Vec<String> = Vec::new();
    for id in registry.pipeline_ids() {
        if !id.ends_with("_atlas") {
            continue;
        }
        let p = registry.get(id).unwrap();
        let emits = p.emits();
        let map = p.declared_ontology();
        for kind in &emits.atoms {
            if UNREACHABLE_BY_DESIGN.contains(kind) {
                continue;
            }
            let reachable = map.navigation.rows().any(|(_, row)| {
                if row.exemplars.is_empty() {
                    return false; // switched off by name
                }
                row.seed.kinds.contains(kind)
                    // The enumeration row seeds the DECLARED types, which are
                    // entity subtypes — so it reaches Entity and nothing else.
                    || (row.seed.declared && *kind == AtomType::Entity)
                    // A Summary reaches the reader by COMPOSITION as well as by
                    // seeding: `summary_sources` is the late append, and a row
                    // that lists one serves summaries whether or not it seeds
                    // them (engineering's tension row is the case in point).
                    || (*kind == AtomType::Summary && !row.summary_sources.is_empty())
            });
            if !reachable {
                orphans.push(format!("{id}: {kind:?}"));
            }
        }
        checked += 1;
    }
    assert!(checked >= 5, "checked {checked} atlas pipelines");
    assert!(
        orphans.is_empty(),
        "these kinds are built and then unreachable — every one is atoms written \
         to disk that no walk can seed and no reader can ever see: {orphans:?}"
    );
}

/// The rows say what the build emits, genre by genre: philosophy seeds
/// tension on reconstructed arguments and composes summaries on it (the
/// order's spec); referential, which skips Phase 8, seeds no Configuration;
/// engineering, claims only, keeps one row on.
#[test]
fn builtin_navigation_rows_follow_each_genres_emit_set() {
    let philosophy = builtin("philosophy_atlas").declared_ontology().navigation;
    assert_eq!(
        philosophy.tension.seed.kinds,
        vec![AtomType::Claim, AtomType::ArgumentReconstruction]
    );
    assert_eq!(
        philosophy.tension.summary_sources,
        SummarySource::ALL.to_vec()
    );
    assert!(philosophy
        .thematic
        .seed
        .kinds
        .contains(&AtomType::Configuration));

    let referential = builtin("referential_atlas").declared_ontology().navigation;
    assert!(!referential
        .thematic
        .seed
        .kinds
        .contains(&AtomType::Configuration));
    assert!(!referential.thematic.walk.contains(&EdgeType::Configures));

    let engineering = builtin("engineering_atlas").declared_ontology().navigation;
    let on: Vec<_> = engineering
        .classifiable()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(on, vec![QuestionKind::Tension]);
    assert_eq!(engineering.tension.seed.kinds, vec![AtomType::Claim]);
    // Its emit set is the genre's deciders, not a hand list: claims from
    // Phase 1 and Summary from the seed table. `Configuration` was here until
    // 2026-09-09, on the genre-default Phase-8 flag, and the comment that sat
    // in its place already recorded the tell — "the two installed atlases
    // carry none". The reachability ratchet turned that observation into a
    // failure: this genre switches off all four rows that could seed a
    // Configuration, so the phase spent a model call per build on atoms no
    // walk could reach. `EngineeringGenre::runs_configuration_phase` is now
    // `false`, and `Configures` leaves the edge set with it.
    let emits = builtin("engineering_atlas").emits();
    assert_eq!(
        emits.atoms,
        [AtomType::Claim, AtomType::Summary].into_iter().collect()
    );
    assert_eq!(emits.edges, [EdgeType::Tension].into_iter().collect());
    assert!(emits.entity_types.is_empty());
    assert!(!emits.declares_types);
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
