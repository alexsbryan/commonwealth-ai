// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;
use crate::enrichment::atlas::resolution_placement::place_index_chunks;
use crate::index::EnrichmentChunkRow;
use crate::recipe_ontology::OntologyBlock;

const HAPPENINGS: &str = r#"
[[types]]
name = "happening"
kind = "event"
description = "Something that happened."
identity_criterion = "Two mentions are one happening when they tell of the same occurrence."

[[types]]
name = "report"
kind = "claim"
force = "assertive"
subject = "happening"
description = "A report of a happening."
"#;

const A: &str = "A fire broke out at the Mill Lane works on Tuesday.";
const B: &str = "Crews fought the Mill Lane fire for six hours.";

/// A statement of the window's one happening, read in `doc` on `line`.
fn report(id: &str, doc: &str, line: &str) -> Claim {
    serde_json::from_value(json!({
        "id": id, "content": line, "discourse_act": "assert",
        "epistemic_status": "attributed", "scope": "universal", "subject": null,
        "evidence": [{"chunk_id": "s1", "passage_preview": line}],
        "claim_kind": "report", "anchor": line, "enrichment_depth": "extracted",
        "attributes": {
            LOCAL_REF_ATTRIBUTE: "p1",
            CHAPTER_SUBJECT_ATTRIBUTE: "s1/p1",
            SOURCE_DOCUMENT_ATTRIBUTE: doc,
            SUBJECT_FIELDS_ATTRIBUTE: {},
        },
    }))
    .unwrap()
}

/// Failing input, named: the news journey (runs/loop/ship/journeys-5b,
/// 2026-10-20): `mill-lane-fire`, stated in report-a and report-b, cited
/// report-a's chunk only, so report-b's quote was under no cited chunk. A
/// happening stated in two documents cites each, in statement order, and
/// each is placed on its own installed chunk.
#[tokio::test]
async fn a_happening_stated_in_two_documents_cites_both_installed_chunks() {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(HAPPENINGS).unwrap(),
    }
    .policies()
    .unwrap();
    let row = |id: u64, doc: &str, content: &str| EnrichmentChunkRow {
        id,
        content: content.into(),
        title: None,
        url: None,
        metadata_raw: Some(json!({ "id": doc }).to_string()),
        source_doc_id: Some(doc.into()),
    };
    let rows = [row(1, "report-a", A), row(2, "report-b", B)];
    let docs = SectionDocuments::from_chunk_rows([("s1", &[1u64, 2][..])], &rows);
    let mut claims = vec![report("a", "report-a", A), report("b", "report-b", B)];
    let (mut entities, mut events, mut states, mut relations) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut ars, mut positions, mut oppositions, mut edges) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut trajectories = BTreeMap::new();
    let mut atoms = BuildAtoms {
        entities: &mut entities,
        events: &mut events,
        states: &mut states,
        relations: &mut relations,
        claims: &mut claims,
        argument_reconstructions: &mut ars,
        positions: &mut positions,
        oppositions: &mut oppositions,
        edges: &mut edges,
        trajectories: &mut trajectories,
    };
    let mut on_document = |_: &str, _: &DocumentResolution| {};
    let (reports, failures) = resolve_declared_types(
        &mut atoms,
        &docs,
        &Participants::default(),
        &p,
        "news",
        None,
        Answerer::Proposed,
        &mut on_document,
    )
    .await;
    assert_eq!(reports[0].records, 1, "{reports:?} {failures:?}");
    place_index_chunks(&mut atoms, &docs);
    let [fire] = events.as_slice() else {
        panic!("one happening: {events:?}")
    };
    let cited: Vec<(Option<&str>, Option<u64>, &str)> = fire
        .evidence
        .iter()
        .map(|c| {
            (
                c.source_doc_id.as_deref(),
                c.index_chunk,
                c.chunk_id.as_str(),
            )
        })
        .collect();
    assert_eq!(
        cited,
        [
            (Some("report-a"), Some(1), "s1"),
            (Some("report-b"), Some(2), "s1")
        ],
        "{:?}",
        fire.evidence
    );
    assert_eq!(fire.evidence[1].passage_preview.as_deref(), Some(B));
}
