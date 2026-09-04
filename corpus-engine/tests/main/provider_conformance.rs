// SPDX-License-Identifier: AGPL-3.0-or-later
//! ONE conformance suite for [`AtlasProvider`], run against BOTH backends.
//!
//! The grounding walk holds a provider behind `dyn`, so a backend that answers
//! the trait differently from its sibling is a defect the walk cannot see and
//! no per-backend test can catch: each one passes its own expectations. The
//! failure this module exists to prevent is the quiet one — wikipedia grounding
//! that works and disagrees.
//!
//! So the assertions live ONCE, in [`assert_provider_conformance`], and both
//! implementors are handed the SAME graph to answer about:
//!
//! ```text
//!   A --Involves--> B          A cites chunk "1", B cites chunk "2"
//!   (and one id that is in neither store)
//! ```
//!
//! - **atom-class:** `AtlasGraph` over a real `atoms.lance` + `edges.csr`,
//!   written by `store::write_store_blocking` into a tempdir.
//! - **wiki-class:** `WikiAtlasProvider` over a real `articles.lance` +
//!   `edges.lance`, written by the direct chunks build into a tempdir.
//!
//! Atom IDS differ between the two by construction — one is
//! `AtomId::entity_content_hash` over an extracted Entity, the other over an
//! article — so the suite takes the pair it should ask about rather than
//! hard-coding them. Everything else is identical, and any divergence in
//! behaviour fails on the same line for whichever backend drifted.

use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef};
use corpus_engine::enrichment::atlas::context::AtlasGraph;
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use corpus_engine::enrichment::atlas::provider::AtlasProvider;
use corpus_engine::enrichment::atlas::store;
use corpus_engine::enrichment::atlas::wiki_store::{
    build_wikipedia_columnar_store_from_chunks, wiki_atom_id,
};
use corpus_engine::enrichment::atlas::{AtomEnvelope, Entity};
use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
use corpus_engine::extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
use corpus_engine::index::StoredChunkWithMetadata;
use corpus_engine::WikiAtlasProvider;

/// What the two fixtures agree they hold, minus the ids they cannot share.
struct Subject<'a> {
    backend: &'a str,
    corpus_id: &'a str,
    /// The source atom (`A`), which cites chunk `"1"`.
    a: String,
    /// The target atom (`B`), which cites chunk `"2"`.
    b: String,
    /// A well-formed id belonging to neither store.
    absent: String,
}

/// Every member of the trait, asked of one provider. Called once per backend.
fn assert_provider_conformance(p: &dyn AtlasProvider, s: &Subject<'_>) {
    let who = s.backend;

    // ── identity + site ──────────────────────────────────────────────────
    assert_eq!(p.atlas_corpus_id(), s.corpus_id, "{who}: atlas_corpus_id");
    // Both fixtures are self-hosted (the atlas id is not `sep-<slug>`), so the
    // chunks live in the atlas's own corpus and no title filter applies.
    assert_eq!(
        p.site().chunk_corpus().as_str(),
        s.corpus_id,
        "{who}: site().chunk_corpus"
    );
    assert_eq!(
        p.site().article(),
        None,
        "{who}: self-hosted has no article"
    );
    assert_eq!(p.article_slug(), s.corpus_id, "{who}: article_slug");

    // ── atom ─────────────────────────────────────────────────────────────
    let a = p
        .atom(&s.a)
        .unwrap_or_else(|| panic!("{who}: atom(A) missing"));
    assert_eq!(a.id(), s.a, "{who}: atom(A).id round-trips");
    assert!(!a.name().is_empty(), "{who}: atom(A) has a name");
    let b = p
        .atom(&s.b)
        .unwrap_or_else(|| panic!("{who}: atom(B) missing"));
    assert_eq!(b.id(), s.b, "{who}: atom(B).id round-trips");
    // The absent id is the failing input every accessor is checked against: a
    // provider that returned something here would be inventing atoms.
    assert!(p.atom(&s.absent).is_none(), "{who}: atom(absent) is None");

    // ── atom_evidence ────────────────────────────────────────────────────
    let ev = p.atom_evidence(&s.a);
    assert_eq!(ev.len(), 1, "{who}: A cites exactly one chunk");
    assert_eq!(ev[0].chunk_id(), "1", "{who}: A's evidence anchor");
    assert_eq!(
        p.atom_evidence(&s.b)[0].chunk_id(),
        "2",
        "{who}: B's evidence anchor"
    );
    assert!(
        p.atom_evidence(&s.absent).is_empty(),
        "{who}: evidence(absent) is empty, not a panic"
    );

    // ── edges ────────────────────────────────────────────────────────────
    let out = p.edges_from(&s.a);
    assert_eq!(out.len(), 1, "{who}: A has one out-edge");
    assert_eq!(out[0].source, s.a, "{who}: out-edge source is A");
    assert_eq!(out[0].target, s.b, "{who}: out-edge target is B");
    assert_eq!(
        out[0].edge_type,
        EdgeType::Involves,
        "{who}: the kind is the closed Involves"
    );
    assert!(
        (0.0..=1.0).contains(&out[0].confidence),
        "{who}: confidence is a [0,1] value, got {}",
        out[0].confidence
    );

    // The walk reads edges backwards too, so the in-view must name the same
    // endpoints in the same order — not the reversed pair.
    let inn = p.edges_to(&s.b);
    assert_eq!(inn.len(), 1, "{who}: B has one in-edge");
    assert_eq!(inn[0].source, s.a, "{who}: in-edge source is still A");
    assert_eq!(inn[0].target, s.b, "{who}: in-edge target is still B");

    assert!(p.edges_from(&s.b).is_empty(), "{who}: B has no out-edges");
    assert!(p.edges_to(&s.a).is_empty(), "{who}: A has no in-edges");
    assert!(
        p.edges_from(&s.absent).is_empty(),
        "{who}: edges_from(absent) is empty"
    );
    assert!(
        p.edges_to(&s.absent).is_empty(),
        "{who}: edges_to(absent) is empty"
    );

    // ── seed table + ontology (the two absences the walk must NAME) ───────
    assert!(
        p.ann_seed_table().is_none(),
        "{who}: no seed table was attached"
    );
    assert!(
        !p.has_ann_seed_table(),
        "{who}: has_ann_seed_table agrees with ann_seed_table"
    );
    assert!(p.ontology().is_none(), "{who}: nothing was declared");
    // Provided member: undeclared means every declared-type path is inert,
    // rather than every subtype matching or the call panicking.
    assert!(
        !p.is_subtype_of("anything", "anything"),
        "{who}: is_subtype_of is false for an undeclared corpus"
    );
}

// ── the two fixtures ────────────────────────────────────────────────────────

fn entity(name: &str, chunk: &str, corpus: &str) -> Entity {
    Entity {
        id: AtomId::entity_content_hash(name, &EntityType::Concept, corpus),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Concept,
        first_appearance: ChunkRef::new(chunk.to_string(), None),
        description: format!("a description of {name} long enough to be real"),
        defining_quote: None,
        salience: 0.9,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }
}

/// `AtlasGraph` over a real `atoms.lance` + `edges.csr`.
fn atom_class(dir: &std::path::Path, corpus: &str) -> (AtlasGraph, String, String) {
    let a = entity("alpha concept", "1", corpus);
    let b = entity("beta concept", "2", corpus);
    let (aid, bid) = (a.id.as_str().to_string(), b.id.as_str().to_string());
    let edges = vec![Edge {
        id: EdgeId::new(1),
        edge_type: EdgeType::Involves,
        source: a.id.clone(),
        target: b.id.clone(),
        evidence: Vec::new(),
        trigger_event: None,
        sub_question: None,
        confidence: 1.0,
        provenance: EdgeProvenance::Derived,
    }];
    let atoms = vec![AtomEnvelope::Entity(a), AtomEnvelope::Entity(b)];
    store::write_store_blocking(dir, corpus, &atoms, &edges).unwrap();
    let g = AtlasGraph::load_lance_from_disk(corpus, dir).unwrap();
    (g, aid, bid)
}

/// `WikiAtlasProvider` over a real `articles.lance` + `edges.lance`, built the
/// way the CLI builds it: from chunks.
async fn wiki_class(dir: &std::path::Path, corpus: &str) -> (WikiAtlasProvider, String, String) {
    fn meta(section: &str, links: Vec<(&str, &str)>) -> String {
        let m = WikipediaChunkMetadata {
            section_name: section.into(),
            section_path: vec![section.into()],
            section_depth: 0,
            section_type: "lead".into(),
            citation_needed_count: None,
            pov_count: None,
            clarification_needed_count: None,
            update_count: None,
            is_flagged_stable: None,
            outgoing_links: links
                .into_iter()
                .map(|(t, l)| WikiLink {
                    target_title: t.into(),
                    link_text: l.into(),
                })
                .collect(),
            revision_id: Some(1),
            wikidata_qid: None,
            page_id: None,
        };
        serde_json::to_string(&m).unwrap()
    }
    fn ch(id: u64, title: &str, m: String) -> StoredChunkWithMetadata {
        StoredChunkWithMetadata {
            id,
            title: Some(title.into()),
            url: None,
            metadata_raw: Some(m),
        }
    }
    // Chunk ids 1 and 2 so the evidence anchors match the atom-class fixture.
    build_wikipedia_columnar_store_from_chunks(
        dir,
        corpus,
        vec![
            ch(1, "Alpha", meta("Lead", vec![("Beta", "beta")])),
            ch(2, "Beta", meta("Lead", vec![])),
        ],
    )
    .await
    .unwrap();
    let p = WikiAtlasProvider::open(dir, corpus).await.unwrap();
    (
        p,
        wiki_atom_id("Alpha", corpus),
        wiki_atom_id("Beta", corpus),
    )
}

// ── the two runs ────────────────────────────────────────────────────────────

#[tokio::test]
async fn atom_class_backend_conforms() {
    let tmp = tempfile::tempdir().unwrap();
    let corpus = "conformance-atoms";
    let (g, a, b) = atom_class(tmp.path(), corpus);
    assert_provider_conformance(
        &g,
        &Subject {
            backend: "atom-class (AtlasGraph over atoms.lance + edges.csr)",
            corpus_id: corpus,
            a,
            b,
            absent: "entity-0000000000000000".into(),
        },
    );
}

#[tokio::test]
async fn wiki_class_backend_conforms() {
    let tmp = tempfile::tempdir().unwrap();
    let corpus = "conformance-wiki";
    let (p, a, b) = wiki_class(tmp.path(), corpus).await;
    assert_provider_conformance(
        &p,
        &Subject {
            backend: "wiki-class (WikiAtlasProvider over articles.lance + edges.lance)",
            corpus_id: corpus,
            a,
            b,
            absent: "entity-0000000000000000".into(),
        },
    );
}

/// The suite is only worth anything if it can fail. This drives the SAME
/// assertions with a deliberately wrong expectation and requires a panic —
/// so a future edit that turns `assert_provider_conformance` into a no-op
/// (an early return, a swallowed `Option`) is caught by the suite itself.
#[tokio::test]
async fn the_conformance_suite_can_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let corpus = "conformance-wiki";
    let (p, a, _b) = wiki_class(tmp.path(), corpus).await;
    let wrong = Subject {
        backend: "deliberately wrong",
        corpus_id: corpus,
        a: a.clone(),
        // B is claimed to be an id the store does not hold.
        b: "entity-1111111111111111".into(),
        absent: "entity-0000000000000000".into(),
    };
    let boom = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_provider_conformance(&p, &wrong)
    }));
    assert!(
        boom.is_err(),
        "the conformance assertions must fail on a wrong subject"
    );
}

/// A seed table attached to either backend is reported by both accessors —
/// the one member the fixtures above deliberately leave absent, checked here
/// so `has_ann_seed_table` is not vacuously false everywhere.
#[tokio::test]
async fn a_seed_table_is_reported_by_both_accessors() {
    use corpus_engine::enrichment::atlas::ann_store::AnnSeedTable;
    let tmp = tempfile::tempdir().unwrap();
    let store_dir = tmp.path().join("wiki");
    std::fs::create_dir_all(&store_dir).unwrap();
    let (p, a, _) = wiki_class(&store_dir, "conformance-wiki").await;
    assert!(!p.has_ann_seed_table());

    let ann_dir = tmp.path().join("ann");
    std::fs::create_dir_all(&ann_dir).unwrap();
    let table = AnnSeedTable::build(&ann_dir, &[(a.clone(), vec![0.1f32; 8])])
        .await
        .unwrap();
    let p = p.with_ann_seed_table(Arc::new(table));
    assert!(p.has_ann_seed_table());
    assert!(p.ann_seed_table().is_some());
}
