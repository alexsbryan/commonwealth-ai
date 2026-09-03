// SPDX-License-Identifier: AGPL-3.0-or-later
//! The regression the unit tests in `evidence_site` cannot catch.
//!
//! `EvidenceSite::derive` is tested on its own, but the SEP defect was not
//! a wrong *decision* — it was the right decision never reaching the
//! request. The producer had to actually stamp the site onto every
//! `ChunkRequest` it emits, from a real atlas loaded off real disk. That
//! is this module, and it costs milliseconds: a two-atom v2 store in a
//! tempdir, no model, no daemon, no corpus index.
//!
//! It works without embeddings because `atlas_navigate_ann` force-seeds
//! every atom whose canonical name appears in the question
//! (`contains_whole_word`), and a graph with no ANN table contributes
//! name-match seeds only. Ranking quality needs the live bench; ADDRESSING
//! does not, and addressing is where the bug was.

use corpus_engine::enrichment::atlas::atoms::AtomId;
use corpus_engine::enrichment::atlas::context::{
    atlas_navigate_ann, AtlasContext, AtlasEntry, AtlasGraph, ChunkRequest,
};
use corpus_engine::enrichment::atlas::evidence_site::ChunkSelector;
use corpus_engine::enrichment::atlas::store;
use corpus_engine::enrichment::atlas::{AtomEnvelope, ChunkRef, Entity};
use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

fn named_atom(n: usize, name: &str) -> Entity {
    Entity {
        id: AtomId::entity(n),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Concept,
        // A section slug, the SEP shape — NOT a numeric row id.
        first_appearance: ChunkRef::new(
            format!("sec_{n:04}"),
            Some("the consequence argument holds that".into()),
        ),
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

/// Build a real on-disk v2 atlas under `atlas_corpus_id` and navigate it
/// with a question naming its atom. Returns the emitted requests.
async fn navigate_fixture(atlas_corpus_id: &str, question: &str) -> Vec<ChunkRequest> {
    let atoms = vec![AtomEnvelope::Entity(named_atom(1, "consequence argument"))];
    let atom_id = atoms[0].id().as_str().to_string();

    let tmp = tempfile::tempdir().unwrap();
    store::write_store_blocking(tmp.path(), atlas_corpus_id, &atoms, &[]).unwrap();
    let graph = AtlasGraph::load_lance_from_disk(atlas_corpus_id, tmp.path()).unwrap();

    let ctx = AtlasContext {
        atlas_corpus_id: atlas_corpus_id.to_string(),
        entries: vec![AtlasEntry {
            atom_id,
            canonical_name: "consequence argument".into(),
            embed_text: "consequence argument desc".into(),
            // Zero vector: the ANN/cosine path contributes nothing and the
            // name-match seed carries the test. Deliberate.
            embedding: vec![0.0; 4],
        }],
        top_k: 4,
    };

    atlas_navigate_ann(question, &[0.0; 4], &[&ctx], &[&graph], 4, 2).await
}

/// THE REGRESSION. A per-article atlas (`sep-freewill`) cites chunks that
/// live in its PARENT (`sep`). Before `EvidenceSite`, the emitted request
/// carried `sep-freewill` and the consumer scoped its fetch to it — an
/// index holding an `atlas/` dir and no chunks — so atlas grounding
/// contributed zero to every SEP answer, silently.
#[tokio::test]
async fn a_per_article_atlas_addresses_its_parent_not_itself() {
    let reqs = navigate_fixture("sep-freewill", "reconstruct the consequence argument").await;
    assert!(
        !reqs.is_empty(),
        "name-match seeding produced no requests; the fixture is not exercising the walk"
    );
    for r in &reqs {
        assert_eq!(
            r.site.chunk_corpus().as_str(),
            "sep",
            "a request from the `sep-freewill` atlas must be fetched from `sep`; \
             got {} — this is the defect fixed in 3ab1fecbc",
            r.site.chunk_corpus()
        );
        assert_eq!(r.site.article(), Some("freewill"));
    }
}

/// The other layout must not regress into the parent rule: a whole-corpus
/// atlas IS its own chunk corpus, and has no article to filter on.
#[tokio::test]
async fn a_self_hosted_atlas_addresses_itself_and_filters_on_no_article() {
    let reqs = navigate_fixture("wikipedia", "reconstruct the consequence argument").await;
    assert!(!reqs.is_empty());
    for r in &reqs {
        assert_eq!(r.site.chunk_corpus().as_str(), "wikipedia");
        assert_eq!(
            r.site.article(),
            None,
            "a self-hosted atlas has no article; filtering on one made the \
             old code compare a chunk title against a CORPUS id"
        );
    }
}

/// The second axis travels too: a `sec_NNNN` id is a Section selector, not
/// something the consumer has to recover with `parse::<u64>()`.
#[tokio::test]
async fn the_selector_is_carried_not_reparsed() {
    let reqs = navigate_fixture("sep-freewill", "reconstruct the consequence argument").await;
    assert!(matches!(reqs[0].selector, ChunkSelector::Section(_)));
}
