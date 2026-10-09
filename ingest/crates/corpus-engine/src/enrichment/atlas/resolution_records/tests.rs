// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;
use crate::enrichment::ontology::{DocumentFieldsDecl, MetadataSourceDecl, SourceDecl};
use crate::index::EnrichmentChunkRow;

const CORPUS: &str = "c";

fn row(id: u64, key: &str, date: &str, thread: &str, body: &str) -> EnrichmentChunkRow {
    EnrichmentChunkRow {
        id,
        content: body.into(),
        title: Some(format!("about {key}")),
        url: None,
        metadata_raw: Some(json!({"date": date, "thread": thread}).to_string()),
        source_doc_id: Some(key.into()),
    }
}

/// m1 and m2 share a thread; m3 is another. Section 1 holds two documents, so
/// a claim there is placed by its anchor.
fn documents() -> SectionDocuments {
    let rows = [
        row(
            1,
            "m1",
            "2001-01-01T10:00:00Z",
            "t1",
            "We can offer gas at Malin for April.\nPrice is 4.12.",
        ),
        row(
            2,
            "m2",
            "2001-01-02T10:00:00Z",
            "t1",
            "Customer accepted the Malin offer for April.",
        ),
        row(
            3,
            "m3",
            "2001-01-03T10:00:00Z",
            "t2",
            "Separately, a new request for power in Q3.",
        ),
    ];
    SectionDocuments::from_chunk_rows([("sec_1", &[1u64, 2][..]), ("sec_2", &[3u64][..])], &rows)
}

fn policies() -> OntologyPolicies {
    let mut p = OntologyPolicies::default();
    p.shape.types = vec![
        OntologyTypeDecl {
            name: "deal".into(),
            kind: TypeKind::Entity,
            identity_criterion: Some("the same transaction".into()),
            identity_bar: Some(0.5),
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "stage_update".into(),
            kind: TypeKind::Claim,
            subject: Some("deal".into()),
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "commitment".into(),
            kind: TypeKind::Claim,
            subject: Some("person".into()),
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "person".into(),
            kind: TypeKind::Entity,
            ..Default::default()
        },
    ];
    p.change.document = Some(DocumentFieldsDecl {
        date: Some("date".into()),
        thread: Some("thread".into()),
        id: None,
    });
    p
}

fn claim(id: &str, kind: &str, section: &str, anchor: &str, subject: Option<&str>) -> Claim {
    serde_json::from_value(json!({
        "id": id, "content": anchor, "discourse_act": "assert", "epistemic_status": "confident",
        "scope": "universal", "evidence": [{"chunk_id": section, "passage_preview": anchor}],
        "subject": subject, "anchor": anchor, "claim_kind": kind, "enrichment_depth": "extracted",
        "attributed_to": if kind == "commitment" { Some("entity-0001") } else { None },
    }))
    .unwrap()
}

/// What 3a and 3b leave: a Phase-1 `deal` atom merged by name, and every kind
/// of reference to it.
struct Build {
    entities: Vec<Entity>,
    events: Vec<Event>,
    states: Vec<State>,
    relations: Vec<Relation>,
    claims: Vec<Claim>,
    edges: Vec<Edge>,
    trajectories: BTreeMap<String, Trajectory>,
}

impl Build {
    fn new() -> Self {
        let entity = |id: &str, name: &str, ty: &str, attrs: Value| -> Entity {
            serde_json::from_value(json!({
                "id": id, "canonical_name": name, "entity_type": ty,
                "first_appearance": {"chunk_id": "sec_1"}, "description": "", "salience": 0.5,
                "enrichment_depth": "extracted", "attributes": attrs,
            }))
            .unwrap()
        };
        let range = json!({"start": "sec_1", "end": "sec_1"});
        Self {
            entities: vec![
                entity("entity-0001", "Malin deal", "deal", json!({})),
                entity("entity-0002", "Ann", "person", json!({"works_on": "entity-0001"})),
            ],
            events: vec![serde_json::from_value(json!({
                "id": "event-0001", "description": "offer", "event_type": "unspecified",
                "participants": ["entity-0001", "entity-0002"], "evidence": [],
                "section_position": {"section_id": "sec_1"}, "enrichment_depth": "extracted",
            }))
            .unwrap()],
            states: vec![
                serde_json::from_value(json!({
                    "id": "state-0001", "entity_id": "entity-0001", "label": "open",
                    "state_type": "unclassified", "evidence": [], "section_range": range,
                    "enrichment_depth": "extracted",
                }))
                .unwrap(),
                serde_json::from_value(json!({
                    "id": "state-0002", "entity_id": "relation-0001", "label": "active",
                    "state_type": "unclassified", "evidence": [], "section_range": range,
                    "enrichment_depth": "extracted",
                }))
                .unwrap(),
            ],
            relations: vec![serde_json::from_value(json!({
                "id": "relation-0001", "label": "negotiates", "participants": ["entity-0002", "entity-0001"],
                "relation_type": "unclassified", "evidence": [], "section_range": range,
                "enrichment_depth": "extracted",
            }))
            .unwrap()],
            claims: vec![
                claim("claim-0001", "stage_update", "sec_1", "offer gas at Malin for April. Price", Some("entity-0001")),
                claim("claim-0002", "stage_update", "sec_1", "accepted the Malin offer", None),
                claim("claim-0003", "stage_update", "sec_2", "a new request for power", None),
                claim("claim-0004", "stage_update", "sec_2", "words in no document", None),
                claim("claim-0005", "commitment", "sec_2", "a new request", None),
            ],
            edges: vec![
                serde_json::from_value(json!({"id": "edge-00001", "edge_type": "Involves",
                    "source": "event-0001", "target": "entity-0001", "confidence": 1.0, "provenance": "derived"}))
                .unwrap(),
                serde_json::from_value(json!({"id": "edge-00002", "edge_type": "Involves",
                    "source": "event-0001", "target": "entity-0002", "confidence": 1.0, "provenance": "derived"}))
                .unwrap(),
            ],
            trajectories: serde_json::from_value(json!({
                "entity-0001": {"canonical_name": "Malin deal", "atom_type": "Entity", "transitions": [],
                    "states": [{"state_id": "state-0001", "label": "open", "section_range": range}]},
                "relation-0001": {"canonical_name": "negotiates", "atom_type": "Relation", "transitions": [],
                    "states": [{"state_id": "state-0002", "label": "active", "section_range": range}]},
            }))
            .unwrap(),
        }
    }

    async fn resolve(&mut self) -> (Vec<RecordsReport>, Vec<PhaseFailure>, Vec<String>) {
        self.resolve_with(&policies()).await
    }

    async fn resolve_with(
        &mut self,
        policies: &OntologyPolicies,
    ) -> (Vec<RecordsReport>, Vec<PhaseFailure>, Vec<String>) {
        let (mut ars, mut positions, mut oppositions) = (Vec::new(), Vec::new(), Vec::new());
        let mut atoms = BuildAtoms {
            entities: &mut self.entities,
            events: &mut self.events,
            states: &mut self.states,
            relations: &mut self.relations,
            claims: &mut self.claims,
            argument_reconstructions: &mut ars,
            positions: &mut positions,
            oppositions: &mut oppositions,
            edges: &mut self.edges,
            trajectories: &mut self.trajectories,
        };
        let mut seen = Vec::new();
        let mut sink = |ty: &str, r: &DocumentResolution| seen.push(format!("{ty}:{}", r.document));
        let (reports, failures) = resolve_declared_types(
            &mut atoms,
            &documents(),
            policies,
            CORPUS,
            Answerer::Proposed,
            &mut sink,
        )
        .await;
        (reports, failures, seen)
    }

    fn subject(&self, claim: &str) -> Option<String> {
        let c = self.claims.iter().find(|c| c.id.as_str() == claim).unwrap();
        c.subject.as_ref().map(|s| s.as_str().to_string())
    }

    fn serialized(&self) -> String {
        json!([
            self.entities,
            self.events,
            self.states,
            self.relations,
            self.claims,
            self.edges,
            self.trajectories
        ])
        .to_string()
    }
}

#[tokio::test]
async fn records_decide_the_type_and_the_name_merged_atom_is_retired() {
    let mut b = Build::new();
    let (reports, failures, seen) = b.resolve().await;

    // Documents in clock order; m3's only placed statement is claim 3.
    assert_eq!(seen, ["deal:m1", "deal:m2", "deal:m3"]);
    let r = &reports[0];
    assert_eq!(
        (
            r.claims,
            r.statements,
            r.unplaced,
            r.documents,
            r.records,
            r.retired
        ),
        (4, 3, 1, 3, 2, 1),
        "{r:?}"
    );

    // The thread puts claims 1 and 2 in one deal; claim 3 opens another.
    let (one, two, three) = (
        b.subject("claim-0001"),
        b.subject("claim-0002"),
        b.subject("claim-0003"),
    );
    assert!(one.is_some() && one == two, "{one:?} {two:?}");
    assert!(three.is_some() && three != one);
    // The record's id is its opening statement's, hashed: the anchor's span in
    // m1's folded body, not a counter.
    let body = "We can offer gas at Malin for April. Price is 4.12.";
    let start = body.find("offer gas at Malin for April. Price").unwrap();
    let opener = format!(
        "m1@{start}..{}",
        start + "offer gas at Malin for April. Price".len()
    );
    let want =
        AtomId::exact_entity_content_hash(&opener, &EntityType::from_str_repr("deal"), CORPUS);
    assert_eq!(one.as_deref(), Some(want.as_str()));
    let deals: Vec<&Entity> = b
        .entities
        .iter()
        .filter(|e| e.entity_type.as_str_repr() == "deal")
        .collect();
    assert_eq!(deals.len(), 2);
    assert!(deals
        .iter()
        .all(|d| d.provenance.extractor_id == EXTRACTOR_ID));

    // A claim with no anchor in its document is no statement: no subject, recorded.
    assert_eq!(b.subject("claim-0004"), None);
    assert!(failures
        .iter()
        .any(|f| f.subject == "atom:claim-0004"
            && f.kind == PhaseFailureKind::UnresolvedClaimSubject));

    // Nothing names a retired atom, and what fell with it is counted.
    let out = b.serialized();
    for gone in ["entity-0001", "relation-0001", "state-0001", "state-0002"] {
        assert!(
            !out.contains(&format!("\"{gone}\"")),
            "{gone} survives: {out}"
        );
    }
    let dropped: Vec<(&str, usize)> = r.dropped.iter().map(|(k, n)| (*k, *n)).collect();
    assert_eq!(
        dropped,
        [
            ("claim_attribution", 1),
            ("edge", 1),
            ("entity_attribute", 1),
            ("event_participant", 1),
            ("relation", 1),
            ("state", 2),
            ("trajectory", 2),
        ]
    );
    // Claim 1's Phase-1 subject was the retired atom; RESOLVE gave it its own.
    assert_eq!(b.edges.len(), 1);
    assert_eq!(
        b.entities
            .iter()
            .find(|e| e.id.as_str() == "entity-0002")
            .unwrap()
            .attributes
            .len(),
        0
    );
}

/// Claim ids are counters (`AtomId::claim(len + 1)`), so a re-extraction
/// renumbers and reorders them; the records it mints must not move with them.
#[tokio::test]
async fn a_rebuild_with_renumbered_claims_mints_the_same_records() {
    let (mut a, mut b) = (Build::new(), Build::new());
    b.claims.reverse();
    for (i, c) in b.claims.iter_mut().enumerate() {
        c.id = AtomId::claim(100 + i);
    }
    a.resolve().await;
    b.resolve().await;
    let by_anchor = |x: &Build| {
        let mut v: Vec<(Option<String>, Option<String>)> = x
            .claims
            .iter()
            .map(|c| {
                (
                    c.anchor.clone(),
                    c.subject.as_ref().map(|s| s.as_str().to_string()),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(by_anchor(&a), by_anchor(&b));
}

/// PRIMITIVES §0: an event is a full subject of claims, identified again by
/// its criterion like an entity. The same statements decide the same
/// partition; the records are events, and the Phase-1 events of the type go.
#[tokio::test]
async fn an_event_type_is_decided_and_its_records_are_events() {
    let mut as_entity = Build::new();
    as_entity.resolve().await;
    let mut b = Build::new();
    b.events[0].event_type = EventType::from_str_repr("deal");
    let mut p = policies();
    p.shape.types[0].kind = TypeKind::Event;
    assert!(decides(&TypeIndex::from_policies(&p), "deal"));
    let (reports, _, seen) = b.resolve_with(&p).await;
    assert_eq!(seen, ["deal:m1", "deal:m2", "deal:m3"]);
    assert_eq!((reports[0].records, reports[0].retired), (2, 1));
    let deals: Vec<&Event> = b
        .events
        .iter()
        .filter(|e| e.event_type.as_str_repr() == "deal")
        .collect();
    assert_eq!(deals.len(), 2, "{:?}", b.events);
    assert!(deals.iter().all(|e| e.id.as_str() != "event-0001"));
    // The Phase-1 entity named `deal` is no atom of an event type: it stays.
    assert!(b.entities.iter().any(|e| e.id.as_str() == "entity-0001"));
    // The partition is the entity type's: claims 1 and 2 one record, 3 another.
    let (one, two, three) = (
        b.subject("claim-0001"),
        b.subject("claim-0002"),
        b.subject("claim-0003"),
    );
    assert!(one.is_some() && one == two && three.is_some() && three != one);
    assert!(deals.iter().any(|e| Some(e.id.as_str()) == one.as_deref()));
    let same = |x: &Build, a: &str, b: &str| x.subject(a) == x.subject(b);
    for (a, c) in [("claim-0001", "claim-0002"), ("claim-0001", "claim-0003")] {
        assert_eq!(same(&b, a, c), same(&as_entity, a, c), "{a} {c}");
    }
}

#[test]
fn resolve_decides_entity_types_with_a_criterion_and_no_source() {
    let mut p = policies();
    let index = TypeIndex::from_policies(&p);
    assert!(decides(&index, "deal"));
    assert!(!decides(&index, "person"), "no criterion");
    let mut as_event = p.clone();
    as_event.shape.types[0].kind = TypeKind::Event;
    assert!(
        decides(&TypeIndex::from_policies(&as_event), "deal"),
        "an event type"
    );
    as_event.shape.types[0].kind = TypeKind::State;
    assert!(
        !decides(&TypeIndex::from_policies(&as_event), "deal"),
        "a state type"
    );
    assert!(!decides(&index, "stage_update"), "a claim type");
    assert!(!decides(&index, "nothing"), "undeclared");
    p.shape.types[0].source = Some(SourceDecl::Metadata(MetadataSourceDecl::default()));
    assert!(
        !decides(&TypeIndex::from_policies(&p), "deal"),
        "a sourced type"
    );
}

/// The trap order ontology-layer-2-one-path step 6 names: a type only RESOLVE
/// decides, with no claim kind about it, has no statements, so a build would
/// leave it to 3a's merge. It is named so the build can refuse it.
#[test]
fn a_decided_type_with_no_claim_kind_about_it_is_named() {
    let p = policies();
    assert!(types_without_statements(&p).is_empty());
    let mut orphaned = p.clone();
    orphaned
        .shape
        .types
        .retain(|t| t.subject.as_deref() != Some("deal"));
    assert_eq!(types_without_statements(&orphaned), ["deal"]);
    orphaned.shape.types[0].kind = TypeKind::Event;
    assert_eq!(types_without_statements(&orphaned), ["deal"]);
}
