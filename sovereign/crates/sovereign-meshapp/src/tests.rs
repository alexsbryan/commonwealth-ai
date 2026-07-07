// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A minimal but real-shaped atlas: two entities, one Relation and one Event
/// edge (the Event references a dangling `entity-ghost` to prove non-entity
/// participants are dropped), a `chapters.json` mapping sections to non-trivial
/// chunk ids, one reconciliation merge, plus `_summary.json` + `edges.json`.
fn write_fixture(dir: &Path) {
    let atlas = dir.join("atlas");
    std::fs::create_dir_all(&atlas).unwrap();
    std::fs::write(
        atlas.join("atoms.json"),
        r#"{
          "schema_version": "2.3",
          "atoms": [
            {"atom_type":"Entity","data":{
              "id":"entity-aaa","canonical_name":"El Paso","entity_type":"institution",
              "first_appearance":{"chunk_id":"sec_00002","passage_preview":"El Paso Corp."},
              "description":"Energy company.","salience":0.5,"enrichment_depth":"extracted",
              "aliases":["El Paso Corp.","PGET"]}},
            {"atom_type":"Entity","data":{
              "id":"entity-bbb","canonical_name":"Kenneth Lay","entity_type":"person",
              "first_appearance":{"chunk_id":"sec_00001","passage_preview":"Ken Lay"},
              "description":"Chairman.","salience":0.9,"enrichment_depth":"extracted"}},
            {"atom_type":"Relation","data":{
              "id":"relation-xyz","label":"counterparty_of",
              "participants":["entity-aaa","entity-bbb"],"relation_type":"association",
              "evidence":[{"chunk_id":"sec_00002","passage_preview":"El Paso and Lay discussed terms"}],
              "section_range":{"start":"sec_00002","end":"sec_00002"},"enrichment_depth":"extracted"}},
            {"atom_type":"Event","data":{
              "id":"event-pqr","description":"Lay emailed El Paso","event_type":"unspecified",
              "participants":["entity-bbb","entity-aaa","entity-ghost"],
              "evidence":[{"chunk_id":"sec_00001","passage_preview":"Date: Thu, 26 Jul 2001"}],
              "section_position":{"section_id":"sec_00001"},"enrichment_depth":"extracted"}}
          ]
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("chapters.json"),
        r#"{"corpus_id":"t","schema_version":"1.0","chapters":[
            {"id":"sec_00001","title":"Email A","chapter":1,"chunk_ids":[100]},
            {"id":"sec_00002","title":"Email B","chapter":2,"chunk_ids":[200,201]}
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        atlas.join("reconciliation.json"),
        r#"{"schema_version":1,"corpus":"t","merged_entities":[
            {"canonical_id":"entity-aaa","canonical_name":"El Paso",
             "surface_forms":[["El Paso",{"signal_kind":"llm_batch"}],
                              ["El Paso Corp.",{"signal_kind":"llm_batch"}]],
             "signals_fired":["name_similarity"],
             "source_atom_ids":["entity-aaa","entity-zzz"]}
        ]}"#,
    )
    .unwrap();
    std::fs::write(
        atlas.join("_summary.json"),
        r#"{"schema_version":2,"atom_count":4,
            "atom_counts":{"Entity":2,"Relation":1,"Event":1,"State":0,"Claim":0,"Question":0}}"#,
    )
    .unwrap();
    std::fs::write(
        atlas.join("edges.json"),
        r#"{"schema_version":1,"edges":[
            {"id":"edge-1","edge_type":"Involves","source":"event-pqr","target":"entity-aaa","confidence":1.0,"provenance":"derived"},
            {"id":"edge-2","edge_type":"Involves","source":"event-pqr","target":"entity-bbb","confidence":1.0,"provenance":"derived"}
        ]}"#,
    )
    .unwrap();
}

#[test]
fn atlas_entities_map_with_type_aliases_and_reconciliation() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());
    let (entities, _rels, findings) = load_atlas_as_investigation(tmp.path()).unwrap();

    assert_eq!(entities.len(), 2);
    assert!(findings.is_empty(), "atlas has no pattern findings");

    let el_paso = entities.iter().find(|e| e.id == "entity-aaa").unwrap();
    assert_eq!(el_paso.entity_type, "institution");
    assert_eq!(el_paso.aliases, vec!["El Paso Corp.", "PGET"]);
    assert_eq!(
        el_paso.attributes.get("description").unwrap().as_str(),
        Some("Energy company.")
    );
    let recon = el_paso.attributes.get("reconciliation").unwrap();
    assert_eq!(recon["surface_forms"].as_array().unwrap().len(), 2);
    assert_eq!(recon["signals_fired"][0], "name_similarity");
    assert_eq!(recon["source_count"], 2);

    let lay = entities.iter().find(|e| e.id == "entity-bbb").unwrap();
    assert!(lay.attributes.get("reconciliation").is_none());
}

#[test]
fn atlas_edges_resolve_sec_to_chunk_and_drop_dangling_participants() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());
    let (_entities, rels, _findings) = load_atlas_as_investigation(tmp.path()).unwrap();

    assert_eq!(rels.len(), 2);
    let rel = rels
        .iter()
        .find(|r| r.relationship_type == "counterparty_of")
        .unwrap();
    let pair: HashSet<&str> = [rel.from_entity_id.as_str(), rel.to_entity_id.as_str()]
        .into_iter()
        .collect();
    assert_eq!(pair, HashSet::from(["entity-aaa", "entity-bbb"]));
    assert_eq!(rel.evidence.chunk_id, "200"); // sec_00002 → first chunk id 200
    assert_eq!(rel.evidence.excerpt, "El Paso and Lay discussed terms");

    let ev = rels
        .iter()
        .find(|r| r.relationship_type == "unspecified")
        .unwrap();
    assert_eq!(
        ev.attributes.get("description").unwrap().as_str(),
        Some("Lay emailed El Paso")
    );
    assert_eq!(ev.evidence.chunk_id, "100"); // sec_00001 → 100
    let ev_pair: HashSet<&str> = [ev.from_entity_id.as_str(), ev.to_entity_id.as_str()]
        .into_iter()
        .collect();
    assert_eq!(ev_pair, HashSet::from(["entity-aaa", "entity-bbb"]));
}

#[test]
fn claims_and_questions_read_atoms_and_handle_empty_or_non_atlas() {
    // The fixture carries entities/relations/events but NO Claim/Question
    // atoms — load_claims/load_questions must read atoms.json, find none, and
    // return Ok([]) (no error, no false positives). The positive path (real
    // claims with resolved attribution + cited evidence) is covered by the
    // live smoke over the Federalist atlas.
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());
    assert!(load_claims(tmp.path(), 100).unwrap().is_empty());
    assert!(load_questions(tmp.path(), 100).unwrap().is_empty());

    // A directory with no `atlas/` → empty, not an error (the guard).
    let bare = tempfile::tempdir().unwrap();
    assert!(load_claims(bare.path(), 100).unwrap().is_empty());
    assert!(load_questions(bare.path(), 100).unwrap().is_empty());
}

#[test]
fn reconciliation_merges_read_sorted_with_reasons() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());
    let merges = reconciliation(tmp.path());
    assert_eq!(merges.len(), 1);
    let m = &merges[0];
    assert_eq!(m.canonical_name, "El Paso");
    assert_eq!(m.surface_forms, vec!["El Paso", "El Paso Corp."]);
    assert_eq!(m.signals_fired, vec!["name_similarity"]);
    assert_eq!(m.source_count, 2);
}

#[test]
fn missing_sidecars_degrade_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());
    std::fs::remove_file(tmp.path().join("chapters.json")).unwrap();
    std::fs::remove_file(tmp.path().join("atlas").join("reconciliation.json")).unwrap();

    let (entities, rels, _f) = load_atlas_as_investigation(tmp.path()).unwrap();
    assert_eq!(entities.len(), 2);
    assert!(entities
        .iter()
        .all(|e| e.attributes.get("reconciliation").is_none()));
    let rel = rels
        .iter()
        .find(|r| r.relationship_type == "counterparty_of")
        .unwrap();
    assert_eq!(rel.evidence.chunk_id, "sec_00002"); // falls back to the raw section id
    assert!(reconciliation(tmp.path()).is_empty());
}

#[test]
fn corpus_stats_reads_summary_edges_recon_and_documents() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());
    let s = corpus_stats(tmp.path());
    assert_eq!(s.atoms, 4);
    assert_eq!(s.entities, 2);
    assert_eq!(s.relations, 1);
    assert_eq!(s.events, 1);
    assert_eq!(s.edges, 2);
    assert_eq!(s.reconciled_merges, 1);
    assert_eq!(s.documents, 2);
}

#[test]
fn subgraph_keeps_top_nodes_and_induced_deduped_edges() {
    let ent = |id: &str, t: &str| InvEntity {
        id: id.into(),
        canonical_name: id.into(),
        entity_type: t.into(),
        attributes: serde_json::Map::new(),
        aliases: Vec::new(),
    };
    let edge = |from: &str, to: &str| InvRelationship {
        id: format!("{from}-{to}"),
        from_entity_id: from.into(),
        to_entity_id: to.into(),
        relationship_type: "rel".into(),
        attributes: serde_json::Map::new(),
        evidence: InvEvidence {
            chunk_id: "1".into(),
            excerpt: String::new(),
        },
        confidence: 1.0,
    };
    // A: A-B,A-C,A-B(dup)=3 · B: A-B,B-C,A-B=3 · C: A-C,B-C=2
    let g = Graph {
        entities: vec![
            ent("A", "institution"),
            ent("B", "institution"),
            ent("C", "person"),
        ],
        rels: vec![
            edge("A", "B"),
            edge("A", "C"),
            edge("B", "C"),
            edge("A", "B"),
        ],
        findings: Vec::new(),
    };

    let sg = subgraph(&g, None, 2);
    assert_eq!(sg.nodes.len(), 2);
    let kept: HashSet<&str> = sg.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(kept, HashSet::from(["A", "B"]));
    assert_eq!(sg.edges.len(), 1);

    let sg2 = subgraph(&g, Some("institution"), 10);
    assert!(sg2.nodes.iter().all(|n| n.entity_type == "institution"));
    assert_eq!(sg2.nodes.len(), 2);
    assert_eq!(sg2.edges.len(), 1);
}

#[test]
fn email_date_parsing_handles_rfc5322_and_us_long_form() {
    assert_eq!(month_num("Jul"), Some(7));
    assert_eq!(month_num("November"), Some(11));
    assert_eq!(month_num("Thu"), None);

    assert_eq!(
        year_month_from_rfc5322("Thu, 26 Jul 2001 09:34:00 -0700").as_deref(),
        Some("2001-07")
    );
    assert_eq!(
        year_month_from_rfc5322(" Friday, November 09, 2001").as_deref(),
        Some("2001-11")
    );
    assert_eq!(
        year_month_from_rfc5322("Mon, 30 Apr 2001").as_deref(),
        Some("2001-04")
    );
    assert_eq!(year_month_from_rfc5322("no date here"), None);

    let email = "From: a@x.com\nTo: b@y.com\nDate: Thu, 26 Jul 2001 09:34:00 -0700\n\
                 Subject: hi\n\nbody text mentioning 1999 should not override the header";
    assert_eq!(parse_email_year_month(email).as_deref(), Some("2001-07"));
    assert_eq!(parse_email_year_month("just a body, no headers"), None);
}

#[tokio::test]
async fn document_feed_orders_docs_desc_and_parses_links() {
    use corpus_engine::index::{EmbeddedChunk, InsertChunk};

    let dir = tempfile::tempdir().unwrap();
    let index = CorpusIndex::create_with_sharing(
        dir.path(),
        "feed-test",
        "Feed Test",
        "test-embed",
        4,
        false,
        Some(false),
        "test",
    )
    .await
    .unwrap();

    let mk = |content: &str, doc: &str, links_json: &str| EmbeddedChunk {
        insert: InsertChunk {
            content: content.to_string(),
            title: Some(doc.to_string()),
            url: None,
            metadata: Some(format!(r#"{{"outbound_links":{links_json}}}"#)),
            content_hash: None,
            source_doc_id: Some(doc.to_string()),
            source_file: None,
            code: Default::default(),
            unit_id: None,
        },
        embedding: vec![0.1, 0.2, 0.3, 0.4],
    };
    index
        .insert_chunks(&[
            mk("older day bullet", "2026-07-05", r#"["Kyiv"]"#),
            mk("newer day bullet one", "2026-07-06", r#"["Gaza war","Benjamin Netanyahu"]"#),
            mk("newer day bullet two", "2026-07-06", r#"[]"#),
        ])
        .await
        .unwrap();

    let feed = document_feed(dir.path(), 10).await.expect("feed");
    assert_eq!(feed.docs.len(), 2);
    // Newest day first.
    assert_eq!(feed.docs[0].source_doc_id, "2026-07-06");
    assert_eq!(feed.docs[0].chunks.len(), 2);
    assert_eq!(
        feed.docs[0].chunks[0].outbound_links,
        vec!["Gaza war".to_string(), "Benjamin Netanyahu".to_string()]
    );
    assert!(feed.docs[0].chunks[1].outbound_links.is_empty());
    assert_eq!(feed.docs[1].source_doc_id, "2026-07-05");
    assert_eq!(feed.docs[1].chunks[0].outbound_links, vec!["Kyiv".to_string()]);

    // limit_docs truncates from the newest end.
    let latest_only = document_feed(dir.path(), 1).await.expect("feed");
    assert_eq!(latest_only.docs.len(), 1);
    assert_eq!(latest_only.docs[0].source_doc_id, "2026-07-06");
}
