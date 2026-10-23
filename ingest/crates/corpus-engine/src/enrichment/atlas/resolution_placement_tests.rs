use std::collections::BTreeMap;

use corpus_index::index::EnrichmentChunkRow;
use serde_json::json;

use super::*;
use crate::enrichment::atlas::atoms::{Claim, Entity};
use crate::enrichment::atlas::edges::Edge;
use crate::enrichment::atlas::resolution::Trajectory;

fn chunk(id: u64, doc: &str, content: &str) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: content.to_string(),
        title: None,
        url: None,
        metadata_raw: None,
        source_doc_id: Some(doc.to_string()),
    }
}

fn claim(id: &str, section: &str, anchor: &str) -> Claim {
    serde_json::from_value(json!({
        "id": id, "content": anchor, "discourse_act": "assert", "epistemic_status": "confident",
        "scope": "universal", "evidence": [{"chunk_id": section, "passage_preview": anchor}],
        "anchor": anchor, "claim_kind": "observation", "enrichment_depth": "extracted",
    }))
    .unwrap()
}

fn entity(id: &str, section: &str, anchor: &str) -> Entity {
    serde_json::from_value(json!({
        "id": id, "canonical_name": id, "entity_type": "issue",
        "first_appearance": {"chunk_id": section, "passage_preview": anchor},
        "description": "", "salience": 0.0, "enrichment_depth": "extracted",
    }))
    .unwrap()
}

/// Window `sec_00001` holds one document of two chunks (6, 7); window
/// `sec_00002` holds two one-chunk documents (8, 9).
fn documents() -> SectionDocuments {
    let rows = [
        chunk(
            6,
            "issue-101",
            "The reporter opened the ticket. The build fails on arm64.",
        ),
        chunk(
            7,
            "issue-101",
            "The fix landed in release 2.4. Closed by the maintainer.",
        ),
        chunk(8, "issue-102", "Docs are missing a page on proxies."),
        chunk(9, "issue-103", "Login times out after ten seconds."),
    ];
    SectionDocuments::from_chunk_rows(
        [("sec_00001", &[6u64, 7][..]), ("sec_00002", &[8u64, 9][..])],
        &rows,
    )
}

fn place(entities: &mut Vec<Entity>, claims: &mut Vec<Claim>) -> PlacementReport {
    let (mut events, mut states, mut relations) = (Vec::new(), Vec::new(), Vec::new());
    let (mut ars, mut positions, mut oppositions) = (Vec::new(), Vec::new(), Vec::new());
    let mut edges: Vec<Edge> = Vec::new();
    let mut trajectories: BTreeMap<String, Trajectory> = BTreeMap::new();
    let mut atoms = BuildAtoms {
        entities,
        events: &mut events,
        states: &mut states,
        relations: &mut relations,
        claims,
        argument_reconstructions: &mut ars,
        positions: &mut positions,
        oppositions: &mut oppositions,
        edges: &mut edges,
        trajectories: &mut trajectories,
    };
    place_index_chunks(&mut atoms, &documents())
}

/// The layer journeys cited `sec_00001`, which the daemon's chunk endpoint
/// refuses ("Cannot parse value `sec_00001` to a `u64`", 2026-10-20): no
/// evidence named its installed chunk. A quote in chunk 7 is placed in 7, an
/// entity's first appearance in 6, and each claim of a two-document window
/// in its own document's chunk; a quote across the 6|7 boundary stays
/// unplaced and is counted. On the old code `index_chunk` did not exist and
/// nothing set it, so every `Some(..)` assertion below failed as `None`.
#[test]
fn evidence_is_placed_in_the_installed_chunk_holding_its_quote() {
    let mut entities = vec![entity(
        "entity-0001",
        "sec_00001",
        "The build fails on arm64",
    )];
    let mut claims = vec![
        claim("claim-0001", "sec_00001", "The fix landed in release 2.4."),
        claim("claim-0002", "sec_00001", "fails on arm64. The fix landed"),
        claim("claim-0003", "sec_00002", "missing a page on proxies"),
        claim("claim-0004", "sec_00002", "Login times out"),
    ];
    let report = place(&mut entities, &mut claims);

    assert_eq!(entities[0].first_appearance.index_chunk, Some(6));
    assert_eq!(claims[0].evidence[0].index_chunk, Some(7));
    assert_eq!(
        claims[1].evidence[0].index_chunk, None,
        "a quote across a chunk boundary is placed in no chunk"
    );
    assert_eq!(claims[2].evidence[0].index_chunk, Some(8));
    assert_eq!(claims[3].evidence[0].index_chunk, Some(9));
    assert_eq!(
        claims[0].evidence[0].chunk_id, "sec_00001",
        "the section stays in chunk_id"
    );
    assert_eq!(report.evidence, 5);
    assert_eq!(report.placed, 4);
    assert_eq!(report.count(Unplaced::AcrossChunks), 1);
    assert_eq!(
        report.summary(),
        "index chunks: 4 of 5 evidence placed by quote, 0 by document metadata (anchor across chunks ×1)"
    );
}

/// Chunks the index cut with overlap both hold a quote in the overlap: the
/// first in manifest order is its chunk. Evidence with no quote, or naming a
/// document its window does not hold, is counted by why and left unplaced.
#[test]
fn overlap_places_the_first_chunk_and_unplaced_evidence_is_counted_by_why() {
    let rows = [
        chunk(3, "issue-104", "Steps to reproduce: run the tests twice."),
        chunk(4, "issue-104", "run the tests twice. The second run hangs."),
    ];
    let docs = SectionDocuments::from_chunk_rows([("sec_00003", &[4u64, 3][..])], &rows);
    let doc = &docs.documents_for_section("sec_00003")[0];
    assert_eq!(doc.chunk_holding("run the tests twice."), Some(4));
    assert_eq!(doc.chunk_holding("The second run hangs."), Some(4));
    assert_eq!(doc.chunk_holding("Steps to reproduce"), Some(3));

    let mut entities = Vec::new();
    let mut named = claim("claim-0005", "sec_00001", "Login times out");
    named.evidence[0].source_doc_id = Some("issue-103".to_string());
    let mut bare = claim("claim-0006", "sec_00001", "x");
    bare.evidence[0].passage_preview = None;
    let mut claims = vec![named, bare];
    let report = place(&mut entities, &mut claims);
    assert_eq!(report.placed, 0);
    assert_eq!(report.count(Unplaced::NoDocument), 1);
    assert_eq!(report.count(Unplaced::NoAnchor), 1);
}

const PEOPLE: &str = r#"
[[types]]
name = "person"
kind = "entity"
attributes = [{ name = "login", type = "text" }]
identity = ["login"]
source = { metadata = ["author"], attributes = { login = "value" } }
"#;

fn authored(id: u64, doc: &str, author: &str, content: &str) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        metadata_raw: Some(json!({ "author": author }).to_string()),
        ..chunk(id, doc, content)
    }
}

/// The layer journeys' `person` records, projected from each issue's
/// `author`, still cited `sec_00001` after quote placement (journeys-3,
/// 2026-10-20: chunk endpoint HTTP 400, "no evidence chunk proves source
/// 'issue-101'"): they carry no quote to find. Their evidence is the document's
/// metadata, which every chunk of it carries, so the projection cites the
/// document's first chunk in manifest order (12 before 11 here) and names the
/// document. Placement leaves that as it is and counts it apart from quote
/// placements; a person whose document is unknown stays unplaced and counted.
/// On the old code the projection set neither field, so `Some(12)` failed as
/// `None` and `projected` counted 0.
#[test]
fn a_person_projected_from_author_cites_its_documents_first_chunk() {
    let policies = crate::recipe_ontology::OntologyBlock {
        version: 1,
        body: toml::from_str(PEOPLE).unwrap(),
    }
    .policies()
    .unwrap();
    let rows = [
        authored(11, "issue-201", "mira", "Second part of the report."),
        authored(12, "issue-201", "mira", "The report opens here."),
        authored(13, "issue-202", "olu", "Another report."),
    ];
    let docs = SectionDocuments::from_chunk_rows([("sec_00004", &[12u64, 11, 13][..])], &rows);
    let projection = crate::enrichment::atlas::project_source_atoms(&docs, &policies, "c").unwrap();
    let mut entities = projection.atoms;
    let by_name = |es: &[Entity], n: &str| {
        es.iter()
            .find(|e| e.canonical_name == n)
            .unwrap()
            .first_appearance
            .clone()
    };
    let mira = by_name(&entities, "mira");
    assert_eq!(mira.index_chunk, Some(12), "{mira:?}");
    assert_eq!(mira.source_doc_id.as_deref(), Some("issue-201"));
    assert_eq!(mira.chunk_id, "sec_00004", "the section stays in chunk_id");
    assert_eq!(by_name(&entities, "olu").index_chunk, Some(13));

    // A person of an atlas whose document nothing names: no quote, no document.
    entities.push(entity("entity-unknown", "sec_00004", "x"));
    entities
        .last_mut()
        .unwrap()
        .first_appearance
        .passage_preview = None;
    let mut claims = Vec::new();
    let report = place(&mut entities, &mut claims);
    assert_eq!(
        by_name(&entities, "mira"),
        mira,
        "placement leaves it untouched"
    );
    assert_eq!(entities[2].first_appearance.index_chunk, None);
    assert_eq!(report.projected, 2);
    assert_eq!(report.placed, 0);
    assert_eq!(report.count(Unplaced::NoAnchor), 1);
    assert_eq!(
        report.summary(),
        "index chunks: 0 of 3 evidence placed by quote, 2 by document metadata (no anchor ×1)"
    );
}
