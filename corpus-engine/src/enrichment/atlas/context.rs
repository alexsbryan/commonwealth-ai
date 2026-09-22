// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas context CORPUS side — the one write path + the chapters join.
//!
//! The atlas context (graph, views, walk, bag builders) moved to the
//! `corpus-engine-atlas-reader` leaf on 2026-09-21 (FIVE_PROGRAMS §12
//! decision 1) and is re-exported below in full. What stays here is what
//! only the ENGINE side owns:
//!
//! - [`read_section_rows`] — the `chapters.json` read. That manifest is
//!   corpus state in the INDEX ROOT (not the atlas dir), so its one parser
//!   (`pipeline::chapter_manifest`) stays host-side and the leaf graph
//!   receives the join as data: every `AtlasGraph::load_from_disk` caller
//!   passes this fn's result explicitly. A graph opened without it has an
//!   empty join, which the resolver reports — never a silent default.
//! - [`build_persistent_ann_seed_table`] — the ANN backfill WRITE.

pub use corpus_engine_atlas_reader::context::*;

#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use corpus_engine_atlas_reader::ann_store::AnnSeedTable;

/// glassbox number the 3b go/no-go watches: `resolved` of `total` bag entries
/// became ANN rows (the rest had no embedding or didn't resolve to an atom-id,
/// which the v1 cosine path also drops, so the seedable set matches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnBuildStats {
    pub resolved: usize,
    pub total: usize,
}

/// ATLAS_STORAGE_V2 backfill: write the persistent per-corpus ANN seed table
/// (`<atlas_dir>/atoms_ann.lance`) from an already-embedded [`AtlasContext`].
/// Each atom-bearing entry contributes `(atom_id, embedding)` — `atom_id` is
/// first-class on the entry (Phase B), so this is a pure transform with no
/// reverse-resolve. Idempotent: a stale table dir is removed first.
/// Lifecycle-time only (the CLI backfill / migrate-all / enrich completion),
/// never the hot query path.
pub async fn build_persistent_ann_seed_table(
    atlas_dir: &Path,
    ctx: &AtlasContext,
) -> Result<AnnBuildStats, String> {
    let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let total = ctx.entries.len();
    for entry in &ctx.entries {
        // Entries with no backing atom (eval-only Tension virtual chunks) or no
        // embedding can't seed the ANN table; the read-path bag drops them too.
        if entry.atom_id.is_empty() || entry.embedding.is_empty() {
            continue;
        }
        // First-seen wins (deterministic); duplicates are the same atom.
        if !seen.insert(entry.atom_id.clone()) {
            continue;
        }
        rows.push((entry.atom_id.clone(), entry.embedding.clone()));
    }
    let resolved = rows.len();
    if resolved == 0 {
        return Err(format!(
            "no atom-bearing entries for {} (0/{total}) — nothing to index",
            ctx.atlas_corpus_id
        ));
    }
    let dir = crate::enrichment::atlas::ann_store::ann_table_dir(atlas_dir);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("remove stale ANN table {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create ANN table dir: {e}"))?;
    AnnSeedTable::build(&dir, &rows).await?;
    Ok(AnnBuildStats { resolved, total })
}

/// Read `<atlas_dir>/../chapters.json` into the section join.
///
/// The path is derived from the dir the caller already opened — same rule
/// as the graph's `index_root`, so no call site can pass a manifest that
/// disagrees with the store it belongs to. A missing or unparseable
/// manifest yields an empty join, which the resolver reports.
pub fn read_section_rows(atlas_dir: &Path) -> std::collections::HashMap<String, Vec<u64>> {
    let Some(corpus_dir) = atlas_dir.parent() else {
        return std::collections::HashMap::new();
    };
    let path = corpus_dir.join("chapters.json");
    match crate::enrichment::pipeline::chapter_manifest::ChapterManifest::load(&path) {
        Ok(Some(m)) => m
            .chapters
            .into_iter()
            .filter(|c| !c.chunk_ids.is_empty())
            .map(|c| (c.id, c.chunk_ids))
            .collect(),
        Ok(None) => std::collections::HashMap::new(),
        Err(e) => {
            tracing::warn!(
                manifest = %path.display(),
                error = %e,
                "atlas: chapters.json unreadable; section evidence falls back to search"
            );
            std::collections::HashMap::new()
        }
    }
}

#[cfg(test)]
mod store_io_tests {
    //! L5 — the v2 store read path end to end: projection fidelity through
    //! [`AtomView`] and the `atoms.lance` + `edges.csr` load.
    use super::*;
    use crate::enrichment::atlas::store;
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    use understanding_vocab::atoms::AtomId;
    use understanding_vocab::atoms::{AtomEnvelope, AtomType, ChunkRef, Entity};
    use understanding_vocab::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};

    fn sample_entity(n: usize, name: &str, salience: f32) -> Entity {
        Entity {
            id: AtomId::entity(n),
            canonical_name: name.into(),
            aliases: vec![format!("{name}-alias")],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(format!("sec_{n:04}"), Some("preview text".into())),
            description: format!("desc of {name}"),
            defining_quote: None,
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn sample_edge(n: usize, source: AtomId, target: AtomId) -> Edge {
        Edge {
            id: EdgeId::new(n),
            edge_type: EdgeType::Involves,
            source,
            target,
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    /// The v2 store, read back through the public `AtlasGraph` API: projected
    /// fields via `AtomView`, edge adjacency + degree, typed enumeration, and
    /// the deep `atom_envelope` parse.
    #[test]
    fn v2_store_projects_fields_and_edges() {
        let atoms = vec![
            AtomEnvelope::Entity(sample_entity(1, "Alice", 0.9)),
            AtomEnvelope::Entity(sample_entity(2, "Bob", 0.4)),
        ];
        let id1 = atoms[0].id().as_str().to_string();
        let id2 = atoms[1].id().as_str().to_string();
        let edge = sample_edge(1, AtomId::entity(1), AtomId::entity(2));

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path();
        store::write_store_blocking(atlas_dir, "c1", &atoms, std::slice::from_ref(&edge)).unwrap();
        let graph = AtlasGraph::load_lance_from_disk("c1", atlas_dir, read_section_rows(atlas_dir))
            .unwrap();

        assert_eq!(graph.atom_count(), 2);
        assert_eq!(graph.edge_count(), 1);

        let a = graph.atom(&id1).expect("lookup id1");
        assert_eq!(a.kind(), AtomType::Entity);
        assert_eq!(a.name(), "Alice");
        assert_eq!(a.subtype(), EntityType::Person.as_str_repr());
        assert_eq!(a.description(), "desc of Alice");
        assert!((a.salience() - 0.9).abs() < 1e-6);
        assert_eq!(a.alias_count(), 1);
        let ev: Vec<_> = a.evidence().collect();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].chunk_id(), "sec_0001");
        assert_eq!(ev[0].passage_preview(), "preview text");

        // Typed enumeration touches only the projected kind tag.
        assert_eq!(graph.atoms_of_kind(AtomType::Entity).count(), 2);
        assert_eq!(graph.atoms_of_kind(AtomType::Claim).count(), 0);

        // Edge adjacency + degree.
        assert_eq!(graph.edge_degree(&id1), 1);
        assert_eq!(graph.edge_degree(&id2), 1);
        let from1 = graph.edges_from(&id1);
        assert_eq!(from1.len(), 1);
        assert_eq!(from1[0].target, id2.as_str());
        assert_eq!(graph.edges_to(&id2).len(), 1);
        assert_eq!(graph.edges_from(&id2).len(), 0);

        // Deep parse round-trips the full atom from its payload blob.
        match a.atom_envelope().expect("payload parses") {
            AtomEnvelope::Entity(e) => assert_eq!(e.canonical_name, "Alice"),
            _ => panic!("expected entity payload"),
        }

        assert!(graph.atom("no-such-id").is_none());
    }

    /// The DARK attribute suffix (`SOVEREIGN_ATLAS_EMBED_ATTRIBUTES`).
    ///
    /// Two facts, separately checked: the rendering is deterministic and
    /// key-sorted, and the knob is OFF by default so nothing renders today.
    #[test]
    fn attribute_suffix_is_dark_and_key_sorted() {
        let mut attrs = serde_json::Map::new();
        attrs.insert("metal".into(), serde_json::Value::String("silver".into()));
        attrs.insert("weight".into(), serde_json::json!(1.21));
        attrs.insert("mint".into(), serde_json::Value::String("Eoforwic".into()));

        // Inserted metal, weight, mint — deliberately NOT alphabetical, so
        // this asserts the renderer sorts rather than that the map happens
        // to. It does not: something in this workspace enables
        // `serde_json/preserve_order`, so `Map` is insertion-ordered here and
        // this same assertion passed under `-p corpus-engine` while failing
        // in the full build until `render_attributes` sorted for itself.
        assert_eq!(
            super::render_attributes(&attrs),
            "\nattr: metal=silver; mint=Eoforwic; weight=1.21"
        );
        assert_eq!(super::render_attributes(&serde_json::Map::new()), "");

        // Default OFF. An explicit override in the environment makes this
        // assertion measure the override rather than the default, so say so.
        assert!(
            std::env::var("SOVEREIGN_ATLAS_EMBED_ATTRIBUTES").is_err(),
            "this test asserts the DEFAULT; unset SOVEREIGN_ATLAS_EMBED_ATTRIBUTES to run it"
        );
        assert_eq!(atom_attributes_suffix(&attrs), "");

        // And an atom with no attributes renders identically either way —
        // which is every atom of every undeclared corpus (I5).
        assert_eq!(atom_attributes_suffix(&serde_json::Map::new()), "");
    }

    /// The vocabulary carrier (ontology-v1 P5). A graph loaded from an atlas
    /// dir with an `ontology.json` carries the declared policies; one without
    /// carries `None`, and `is_subtype_of` is inert there — that inertness is
    /// what makes every `a == b || graph.is_subtype_of(a, b)` compare
    /// byte-identical for SEP / Wikipedia / Enron (I5).
    #[test]
    fn graph_carries_the_declared_ontology_and_walks_specializes() {
        use understanding_vocab::ontology::decl::{OntologyTypeDecl, TypeKind};
        use understanding_vocab::read::ATLAS_DIRNAME;

        fn decl(name: &str, specializes: Option<&str>) -> OntologyTypeDecl {
            OntologyTypeDecl {
                name: name.to_string(),
                kind: TypeKind::Entity,
                specializes: specializes.map(str::to_string),
                ..Default::default()
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("wessex-hoard").join(ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas_dir).unwrap();
        let atoms = vec![AtomEnvelope::Entity(sample_entity(
            1,
            "Aldfrith penny",
            0.9,
        ))];
        store::write_store_blocking(&atlas_dir, "wessex-hoard", &atoms, &[]).unwrap();

        // No ontology.json → declared nothing, and the subtype walk is inert.
        let undeclared =
            AtlasGraph::load_from_disk("wessex-hoard", &atlas_dir, HashMap::new()).unwrap();
        assert!(undeclared.ontology().is_none());
        assert!(!undeclared.is_subtype_of("sceatta", "coin"));
        assert!(!undeclared.is_subtype_of("coin", "coin"));

        // A policy set that declares NO types still reads as undeclared — the
        // filter lives in `with_ontology`, so no consumer re-checks it.
        let empty = crate::enrichment::ontology::OntologyPolicies::from_prose(
            "some prose guidance",
            Default::default(),
        );
        crate::enrichment::atlas::writer::write_atlas_ontology(
            &atlas_dir,
            "custom_atlas",
            1,
            &empty,
        )
        .unwrap();
        let prose_only =
            AtlasGraph::load_from_disk("wessex-hoard", &atlas_dir, HashMap::new()).unwrap();
        assert!(prose_only.ontology().is_none());

        // Declared types → the walk answers.
        let mut declared = crate::enrichment::ontology::OntologyPolicies::default();
        declared.shape.types = vec![decl("coin", None), decl("sceatta", Some("coin"))];
        crate::enrichment::atlas::writer::write_atlas_ontology(
            &atlas_dir,
            "custom_atlas",
            1,
            &declared,
        )
        .unwrap();
        let g = AtlasGraph::load_from_disk("wessex-hoard", &atlas_dir, HashMap::new()).unwrap();
        assert_eq!(g.ontology().unwrap().shape.types.len(), 2);
        assert!(g.is_subtype_of("sceatta", "coin"));
        assert!(g.is_subtype_of("coin", "coin"));
        assert!(!g.is_subtype_of("coin", "sceatta"));
        assert!(!g.is_subtype_of("ruler", "coin"));
    }

    /// `load_from_disk` loads the v2 store when present and errors (no
    /// fallback) when absent — the ATLAS_STORAGE_V2 "no v2 store ⇒ Err"
    /// invariant that lets wikipedia (no atom store) be skipped by the caller
    /// rather than stranded.
    #[test]
    fn load_from_disk_requires_a_v2_store() {
        use understanding_vocab::read::ATLAS_DIRNAME;

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("c1").join(ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas_dir).unwrap();
        let id1 = AtomId::entity(1).as_str().to_string();

        // No v2 store yet → Err, never a panic or a silent empty graph.
        assert!(AtlasGraph::load_from_disk("c1", &atlas_dir, HashMap::new()).is_err());

        // Write the v2 store → load_from_disk serves it.
        let atoms = vec![
            AtomEnvelope::Entity(sample_entity(1, "Alice", 0.9)),
            AtomEnvelope::Entity(sample_entity(2, "Bob", 0.4)),
        ];
        let edge = sample_edge(1, AtomId::entity(1), AtomId::entity(2));
        store::write_store_blocking(&atlas_dir, "c1", &atoms, std::slice::from_ref(&edge)).unwrap();
        let g = AtlasGraph::load_from_disk("c1", &atlas_dir, HashMap::new()).unwrap();
        assert_eq!(g.atom_count(), 2);
        assert_eq!(g.atom(&id1).unwrap().name(), "Alice");

        // Remove the store → Err again (the no-fallback invariant).
        std::fs::remove_dir_all(atlas_dir.join(store::ATOMS_LANCE_DIRNAME)).unwrap();
        assert!(AtlasGraph::load_from_disk("c1", &atlas_dir, HashMap::new()).is_err());
    }

    /// map-conversion rung 3: the rows reach the walk two ways short of the
    /// defaults, and neither depends on declared types. (1) A loader attaches
    /// a pipeline's map to a mapless graph and the source says so; a declared
    /// map is never displaced by it. (2) `ontology.json` with rows and NO
    /// types — engineering's map — still hands its rows to the walk, though
    /// `ontology()` stays `None` for the declared-type paths. Failing input:
    /// read the rows through `ontology()` and case (2) reads as mapless.
    #[test]
    fn a_typeless_map_and_a_pipeline_default_both_reach_the_walk() {
        use crate::enrichment::atlas::context::read_section_rows;
        use crate::enrichment::atlas::provider::AtlasProvider;
        use crate::enrichment::atlas::writer::write_atlas_ontology;
        use corpus_engine_atlas_reader::ground::{navigation_policy_for, PolicySource};
        use corpus_engine_atlas_reader::store::ATOMS_LANCE_DIRNAME as _;
        use understanding_vocab::ontology::AtlasOntologyFile;
        use understanding_vocab::ontology::OntologyPolicies;
        use understanding_vocab::ontology::{NavigationPolicy, QuestionKind};
        use understanding_vocab::read::ATLAS_DIRNAME;

        let tmp = tempfile::tempdir().unwrap();
        let atlas_dir = tmp.path().join("c1").join(ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas_dir).unwrap();
        let atoms = vec![AtomEnvelope::Entity(sample_entity(1, "Alice", 0.9))];
        store::write_store_blocking(&atlas_dir, "c1", &atoms, &[]).unwrap();

        // (1) no file: mapless, then the loader's fallback, named.
        let g = AtlasGraph::load_from_disk("c1", &atlas_dir, HashMap::new()).unwrap();
        assert!(g.navigation().is_none());
        let mut rows = NavigationPolicy::default();
        rows.tension.hops = 7;
        let g = g.with_pipeline_map("philosophy_atlas", rows.clone());
        let (policy, source) = navigation_policy_for(&[&g as &dyn AtlasProvider]);
        assert_eq!(policy.walk(QuestionKind::Tension).hops, 7);
        assert_eq!(
            source,
            PolicySource::PipelineDefault {
                atlas: "c1".into(),
                pipeline: "philosophy_atlas".into()
            }
        );
        assert_eq!(
            source.label(),
            "pipeline default `philosophy_atlas` for c1 (no atlas/ontology.json yet)"
        );

        // (2) a typeless file with rows: declared to the walk, invisible to
        // the declared-type paths, and the pipeline fallback cannot displace it.
        let mut typeless = OntologyPolicies::default();
        typeless.navigation.tension.hops = 3;
        assert!(!typeless.has_declarations());
        write_atlas_ontology(
            &atlas_dir,
            "engineering_atlas",
            AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION,
            &typeless,
        )
        .unwrap();
        let g = AtlasGraph::load_from_disk("c1", &atlas_dir, HashMap::new())
            .unwrap()
            .with_pipeline_map("philosophy_atlas", rows);
        assert!(g.ontology().is_none(), "no types declared");
        let (policy, source) = navigation_policy_for(&[&g as &dyn AtlasProvider]);
        assert_eq!(policy.walk(QuestionKind::Tension).hops, 3);
        assert_eq!(source, PolicySource::Declared("c1".into()));
    }

    /// Inc 5: `call_chain` BFSs only `ScipStructural` (call) edges, skips
    /// `ContainmentStructural` parents, is cycle-safe + depth-bounded, marks
    /// reciprocal trait-pair edges `[dyn-dispatch]`, and `resolve_symbol_seed`
    /// snaps a `::`-qualified code symbol from natural language. Runs over the
    /// v2 Lance backend — the only backend that carries edge provenance.
    #[test]
    fn call_chain_walks_scip_edges_over_the_v2_store() {
        use crate::enrichment::atlas::store;
        use crate::enrichment::pipeline::atlas::EntityType;

        let code = |n: usize, name: &str, ty: &str| -> AtomEnvelope {
            AtomEnvelope::Entity(Entity {
                id: AtomId::entity(n),
                canonical_name: name.into(),
                aliases: vec![],
                entity_type: EntityType::Other(ty.into()),
                first_appearance: ChunkRef::new("m", Some("src".into())),
                description: format!("does {name}"),
                defining_quote: None,
                salience: 0.0,
                enrichment_depth: EnrichmentDepth::Structural,
                affiliation: None,
                role: None,
                participants: vec![],
                provenance: Default::default(),
                attributes: serde_json::Map::new(),
                concept_kind: None,
            })
        };
        // module `m` contains alpha/beta/gamma/delta (containment); scip calls:
        // alpha→beta→gamma→alpha (cycle) and alpha↔delta (reciprocal trait pair).
        let atoms = vec![
            code(1, "m", "module"),
            code(2, "m::alpha", "function"),
            code(3, "m::beta", "function"),
            code(4, "m::gamma", "function"),
            code(5, "m::delta", "function"),
        ];
        let edge = |n: usize, s: usize, t: usize, prov: EdgeProvenance| Edge {
            id: EdgeId::new(n),
            edge_type: EdgeType::Involves,
            source: AtomId::entity(s),
            target: AtomId::entity(t),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: prov,
        };
        let edges = vec![
            edge(1, 1, 2, EdgeProvenance::ContainmentStructural),
            edge(2, 1, 3, EdgeProvenance::ContainmentStructural),
            edge(3, 1, 4, EdgeProvenance::ContainmentStructural),
            edge(4, 1, 5, EdgeProvenance::ContainmentStructural),
            edge(5, 2, 3, EdgeProvenance::ScipStructural), // alpha → beta
            edge(6, 3, 4, EdgeProvenance::ScipStructural), // beta → gamma
            edge(7, 4, 2, EdgeProvenance::ScipStructural), // gamma → alpha (cycle)
            edge(8, 2, 5, EdgeProvenance::ScipStructural), // alpha → delta
            edge(9, 5, 2, EdgeProvenance::ScipStructural), // delta → alpha (reciprocal)
        ];

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        store::write_store_blocking(dir, "code1", &atoms, &edges).unwrap();
        let graph = AtlasGraph::load_lance_from_disk("code1", dir, read_section_rows(dir)).unwrap();

        let alpha = AtomId::entity(2).as_str().to_string();

        // CALLEES from alpha, depth 3. Containment parent `m` is never followed.
        let chain = graph.call_chain(&alpha, CallDirection::Callees, 3, 16);
        assert!(chain.hit());
        let names: Vec<&str> = chain.nodes.iter().map(|n| n.name.as_str()).collect();
        // alpha(0) → beta,delta(1) → gamma(2); cycle back to alpha is cut.
        assert_eq!(names, vec!["m::alpha", "m::beta", "m::delta", "m::gamma"]);
        assert!(!names.contains(&"m"), "containment parent must not appear");
        assert_eq!(chain.nodes[0].depth, 0);
        assert_eq!(chain.nodes[1].depth, 1); // beta
        assert_eq!(chain.nodes[3].depth, 2); // gamma

        // delta is reached over a reciprocal scip pair → dyn-dispatch flagged;
        // beta is a one-way call → not flagged.
        let delta = chain.nodes.iter().find(|n| n.name == "m::delta").unwrap();
        let beta = chain.nodes.iter().find(|n| n.name == "m::beta").unwrap();
        assert!(
            delta.via_dyn_dispatch,
            "alpha↔delta reciprocal = dyn-dispatch"
        );
        assert!(!beta.via_dyn_dispatch);

        // CALLERS of gamma, 1 hop: only beta (scip). The containment parent `m`
        // is NOT a caller (wrong provenance).
        let gamma = AtomId::entity(4).as_str().to_string();
        let callers = graph.call_chain(&gamma, CallDirection::Callers, 1, 16);
        let caller_names: Vec<&str> = callers.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(caller_names, vec!["m::gamma", "m::beta"]);

        // Depth bound: depth=1 stops after one hop and flags truncation.
        let shallow = graph.call_chain(&alpha, CallDirection::Callees, 1, 16);
        assert_eq!(shallow.nodes.len(), 3); // alpha + beta + delta
        assert!(shallow.truncated);

        // Named seed resolution from natural language.
        assert_eq!(
            graph
                .resolve_symbol_seed("what does the beta function call")
                .as_deref(),
            Some(AtomId::entity(3).as_str()),
            "last-segment token `beta` resolves to m::beta",
        );
        assert_eq!(
            graph
                .resolve_symbol_seed("trace m::alpha please")
                .as_deref(),
            Some(alpha.as_str()),
            "whole qualified-name mention resolves",
        );
        assert_eq!(
            graph.resolve_symbol_seed("how does the parser work"),
            None,
            "no symbol mentioned → no named seed (conceptual path takes over)",
        );
    }

    /// ei-7a. `seed_population` puts `Summary` in the `thematic` row's seed
    /// kinds, and `render_atom_entry` is the fan-out that decides whether a
    /// kind can enter the bag at all. A `None` here makes the kind UNSEEDABLE
    /// however the map names it — the writer derives a population and the
    /// renderer silently drops half of it, which is the substitution ARCH
    /// §18.3 forbids and which the Position/State arms were added to prevent.
    ///
    /// Both directions: the arm renders, AND it renders the level, which is
    /// the only thing distinguishing two summaries of one article.
    #[test]
    fn a_summary_atom_renders_into_the_seed_bag() {
        use understanding_vocab::atoms::{AtomId, Summary};
        use understanding_vocab::taxonomy::EnrichmentDepth;
        let atom = AtomEnvelope::Summary(Summary {
            id: AtomId::summary_content_hash("node-1", "sep"),
            node_id: "node-1".into(),
            level: 2,
            text: "polynomial time is the standard for feasible computation".into(),
            evidence: Vec::new(),
            children: Vec::new(),
            enrichment_depth: EnrichmentDepth::extracted_default(),
        });
        let (name, text) =
            render_atom_entry(&atom, "computational-complexity").expect("Summary must render");
        assert_eq!(
            name, "computational-complexity",
            "article-scoped, like every other non-Entity kind"
        );
        assert!(text.contains("[Summary L2]"), "got {text}");
        assert!(text.contains("polynomial time"));
    }
}
